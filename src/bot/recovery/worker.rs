//! Workers own state and I/O. They cannot publish an executable decision.

use super::journal::SharedJournal;
use super::standby::LocalStandby;
use crate::bot::control::RunnerHealth;
use crate::bot::{BotResponse, BotRunner};
use crate::monitor::DecisionContext;
use crate::schema::MjaiEvent;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Observation {
    pub context: DecisionContext,
    pub event: Option<MjaiEvent>,
    pub through: u64,
    pub decide: bool,
}

pub enum Report {
    Primary {
        context: DecisionContext,
        health: RunnerHealth,
        response: Option<BotResponse>,
    },
    Standby {
        context: DecisionContext,
        health: RunnerHealth,
        response: Option<BotResponse>,
        legal: Vec<MjaiEvent>,
    },
}

pub struct PrimaryWorker {
    pub input: mpsc::UnboundedSender<Observation>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PrimaryWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum PrimaryState {
    Ready,
    Recovering {
        retry_at: Instant,
        attempts: u32,
        reason: String,
    },
    Blocked {
        reason: String,
    },
}

impl PrimaryState {
    fn health(&self, through: u64) -> RunnerHealth {
        match self {
            Self::Ready => RunnerHealth::Ready { through },
            Self::Recovering { reason, .. } => RunnerHealth::Recovering {
                through,
                reason: reason.clone(),
            },
            Self::Blocked { reason } => RunnerHealth::Blocked {
                through,
                reason: reason.clone(),
            },
        }
    }
}

impl PrimaryWorker {
    pub fn spawn(
        mut runner: Box<dyn BotRunner>,
        journal: SharedJournal,
        output: mpsc::UnboundedSender<Report>,
    ) -> Self {
        let (input, mut observations) = mpsc::unbounded_channel::<Observation>();
        let task = tokio::spawn(async move {
            let mut through = 0;
            let mut state = PrimaryState::Ready;
            while let Some(work) = observations.recv().await {
                let replay_attempt = match &state {
                    PrimaryState::Recovering {
                        attempts, retry_at, ..
                    } if work.decide && Instant::now() >= *retry_at => {
                        Some(attempts.saturating_add(1))
                    }
                    _ => None,
                };
                let can_run = match &state {
                    PrimaryState::Ready => work.event.is_some() || work.decide,
                    PrimaryState::Recovering { .. } => replay_attempt.is_some(),
                    PrimaryState::Blocked { .. } => false,
                };
                let mut response = None;
                if can_run {
                    let attempts = replay_attempt.unwrap_or(1);
                    let result = if replay_attempt.is_some() {
                        // Restart always replays the complete confirmed prefix,
                        // including actual moves made while on standby.
                        through = 0;
                        replay(&mut runner, &journal, work.through).await
                    } else {
                        let result = runner
                            .react(work.event.as_slice())
                            .await
                            .map_err(|e| format!("{e:#}"))
                            .and_then(|reply| validate_delivery(reply, work.through));
                        let needs_restart = match &result {
                            Err(_) => true,
                            Ok(reply) => {
                                reply.unavailable_reason().is_some()
                                    && !semantic_failure(reply)
                                    && !inference_failure(reply)
                            }
                        };
                        if needs_restart {
                            tracing::warn!(context = ?work.context, reason = ?result.as_ref().err(),
                                "primary transport failed; replaying confirmed game prefix");
                            through = 0;
                            replay(&mut runner, &journal, work.through).await
                        } else {
                            result
                        }
                    };
                    match result {
                        Ok(reply) if reply.unavailable_reason().is_none() => {
                            through = work.through;
                            state = PrimaryState::Ready;
                            response = work.decide.then_some(reply);
                        }
                        Ok(reply) => {
                            // validate_delivery already proved this receipt.
                            through = reply.meta.as_ref().unwrap()["delivery"]["through"]
                                .as_u64()
                                .unwrap();
                            let reason = reply.unavailable_reason().unwrap().to_owned();
                            tracing::warn!(context = ?work.context, committed_through = through, %reason,
                                "primary transaction not committed");
                            state = if semantic_failure(&reply) {
                                PrimaryState::Blocked { reason }
                            } else {
                                // The service already retried inference once.
                                // No immediate second replay or per-event retry.
                                PrimaryState::Recovering {
                                    retry_at: Instant::now() + backoff(attempts),
                                    attempts,
                                    reason,
                                }
                            };
                        }
                        Err(reason) => {
                            tracing::warn!(context = ?work.context, attempts, %reason,
                                "primary replay failed; waiting for the next eligible decision");
                            state = PrimaryState::Recovering {
                                retry_at: Instant::now() + backoff(attempts),
                                attempts,
                                reason,
                            };
                        }
                    }
                }
                if output
                    .send(Report::Primary {
                        context: work.context,
                        health: state.health(through),
                        response,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { input, task }
    }
}

fn semantic_failure(reply: &BotResponse) -> bool {
    reply
        .meta
        .as_ref()
        .and_then(|m| m.get("failure"))
        .and_then(|f| f.get("kind"))
        .and_then(|v| v.as_str())
        .is_some_and(|kind| kind == "semantic")
}

fn inference_failure(reply: &BotResponse) -> bool {
    reply
        .meta
        .as_ref()
        .and_then(|m| m.get("failure"))
        .and_then(|f| f.get("kind"))
        .and_then(|v| v.as_str())
        == Some("inference")
}

fn backoff(attempts: u32) -> Duration {
    Duration::from_secs((1u64 << attempts.min(5)).min(30))
}

async fn replay(
    runner: &mut Box<dyn BotRunner>,
    journal: &SharedJournal,
    through: u64,
) -> Result<BotResponse, String> {
    runner
        .reset()
        .await
        .map_err(|e| format!("restart primary transport: {e:#}"))?;
    let prefix = journal.lock().expect("journal poisoned")[..through as usize].to_vec();
    runner
        .react(&prefix)
        .await
        .map_err(|e| format!("replay real game prefix: {e:#}"))
        .and_then(|reply| validate_delivery(reply, through))
}

fn validate_delivery(reply: BotResponse, expected: u64) -> Result<BotResponse, String> {
    let through = reply
        .meta
        .as_ref()
        .and_then(|m| m.get("delivery"))
        .and_then(|r| r.get("through"))
        .and_then(|n| n.as_u64())
        .ok_or_else(|| "Positioned primary omitted its delivery receipt.".to_string())?;
    if through > expected || (reply.unavailable_reason().is_none() && through != expected) {
        return Err(format!(
            "Primary reply acknowledged {through}, expected real prefix {expected}."
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_the_requested_committed_prefix_can_supply_a_primary_action() {
        let reply = |through| BotResponse {
            action: MjaiEvent::None,
            meta: Some(json!({"delivery":{"through":through}})),
        };
        assert!(validate_delivery(reply(4), 4).is_ok());
        assert!(
            validate_delivery(reply(3), 4).is_err(),
            "duplicate stdout line"
        );
        assert!(
            validate_delivery(reply(5), 4).is_err(),
            "out-of-order stdout line"
        );
        let mut unavailable = reply(3);
        unavailable.meta.as_mut().unwrap()["unavailable"] = json!("uncommitted transaction");
        assert!(validate_delivery(unavailable, 4).is_ok());
    }
}

pub struct StandbyWorker {
    pub input: mpsc::UnboundedSender<Observation>,
}

impl StandbyWorker {
    pub fn spawn(seat: u8, players: u8, output: mpsc::UnboundedSender<Report>) -> Self {
        let (input, mut observations) = mpsc::unbounded_channel::<Observation>();
        // The embedded CPU policy never blocks the event/coordinator executor.
        // Dropping input retires the worker after the current finite forward.
        tokio::task::spawn_blocking(move || {
            let mut engine = LocalStandby::new(seat, players).map_err(|e| format!("{e:#}"));
            let mut through = 0;
            let mut policy_error: Option<String> = None;
            while let Some(work) = observations.blocking_recv() {
                let mut legal = vec![];
                let mut response = None;
                let result = match &mut engine {
                    Ok(bot) => work
                        .event
                        .as_ref()
                        .map_or(Ok(()), |event| bot.observe(event))
                        .map(|_| {
                            through = work.through;
                            if work.decide {
                                legal = bot.legal();
                                match bot.decide() {
                                    Ok(reply) => {
                                        response = Some(reply);
                                        policy_error = None;
                                    }
                                    Err(error) => {
                                        policy_error =
                                            Some(format!("Local inference failed: {error:#}"));
                                    }
                                }
                            }
                        })
                        .map_err(|e| format!("{e:#}")),
                    Err(reason) => Err(reason.clone()),
                };
                let health = match result {
                    Ok(()) => match &policy_error {
                        Some(reason) => RunnerHealth::Blocked {
                            through,
                            reason: reason.clone(),
                        },
                        None => RunnerHealth::Ready { through },
                    },
                    Err(reason) => {
                        engine = Err(reason.clone());
                        RunnerHealth::Blocked { through, reason }
                    }
                };
                if output
                    .send(Report::Standby {
                        context: work.context,
                        health,
                        response,
                        legal,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self { input }
    }
}
