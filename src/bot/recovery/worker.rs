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
    /// Restricted actions proved directly by the provider's live window
    /// while native replay context is incomplete; never a guessed full mask.
    pub interrupted_legal: Option<Vec<MjaiEvent>>,
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
    AwaitingHand {
        reason: String,
    },
    Ready,
    Recovering {
        retry_at: Instant,
        attempts: u32,
        reason: String,
    },
    Blocked {
        reason: String,
        retry_at: Instant,
        attempts: u32,
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
            Self::Blocked { reason, .. } | Self::AwaitingHand { reason } => RunnerHealth::Blocked {
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
        forecast: Option<crate::bot::forecast::ForecastInput>,
    ) -> Self {
        let (input, mut observations) = mpsc::unbounded_channel::<Observation>();
        let task = tokio::spawn(async move {
            let mut through = 0;
            let mut state = PrimaryState::Ready;
            while let Some(work) = observations.recv().await {
                if let Some(MjaiEvent::InterruptedKyoku { reason }) = &work.event {
                    state = PrimaryState::AwaitingHand {
                        reason: reason.clone(),
                    };
                    if let Some(forecast) = &forecast {
                        forecast.revoke();
                    }
                }
                let replay_attempt = match &state {
                    PrimaryState::Recovering {
                        attempts, retry_at, ..
                    }
                    | PrimaryState::Blocked {
                        attempts, retry_at, ..
                    } if work.decide && Instant::now() >= *retry_at => {
                        Some(attempts.saturating_add(1))
                    }
                    _ => None,
                };
                let can_run = match &state {
                    PrimaryState::Ready => work.event.is_some() || work.decide,
                    PrimaryState::Recovering { .. } | PrimaryState::Blocked { .. } => {
                        replay_attempt.is_some()
                    }
                    PrimaryState::AwaitingHand { .. } => false,
                };
                let mut response = None;
                if can_run {
                    let attempts = replay_attempt.unwrap_or(1);
                    let result = if replay_attempt.is_some() {
                        // Restart always replays the complete confirmed prefix,
                        // including actual moves made while on standby.
                        through = 0;
                        replay(&mut runner, &journal, work.through, forecast.as_ref()).await
                    } else {
                        let result = runner
                            .react(work.event.as_slice())
                            .await
                            .map_err(|e| format!("{e:#}"))
                            .and_then(|reply| validate_delivery(reply, work.through))
                            .and_then(|reply| validate_restoration(reply, work.event.as_slice()));
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
                            replay(&mut runner, &journal, work.through, forecast.as_ref()).await
                        } else {
                            result
                        }
                    };
                    match result {
                        Ok(mut reply) if reply.unavailable_reason().is_none() => {
                            through = work.through;
                            state = PrimaryState::Ready;
                            if let Some(forecast) = &forecast {
                                forecast.observe(
                                    work.context,
                                    &mut reply,
                                    work.through,
                                    work.decide,
                                );
                            } else if let Some(meta) =
                                reply.meta.as_mut().and_then(|meta| meta.as_object_mut())
                            {
                                meta.remove("forecast_error");
                            }
                            response = work.decide.then_some(reply);
                        }
                        Ok(reply) => {
                            if let Some(forecast) = &forecast {
                                forecast.revoke();
                            }
                            // validate_delivery already proved this receipt.
                            through = reply.meta.as_ref().unwrap()["delivery"]["through"]
                                .as_u64()
                                .unwrap();
                            let reason = reply.unavailable_reason().unwrap().to_owned();
                            tracing::warn!(context = ?work.context, committed_through = through, %reason,
                                "primary transaction not committed");
                            state = if semantic_failure(&reply) {
                                // Block this prefix, not the worker's entire
                                // lifetime. Rebuild only the complete real
                                // history on a later eligible decision. It
                                // must commit and pass the coordinator's legal
                                // set before primary can own any new input.
                                PrimaryState::Blocked {
                                    reason,
                                    retry_at: Instant::now() + backoff(attempts),
                                    attempts,
                                }
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
                            if let Some(forecast) = &forecast {
                                forecast.revoke();
                            }
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
    forecast: Option<&crate::bot::forecast::ForecastInput>,
) -> Result<BotResponse, String> {
    if let Some(forecast) = forecast {
        forecast.revoke();
    }
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
        .and_then(|reply| validate_restoration(reply, &prefix))
}

fn validate_restoration(reply: BotResponse, events: &[MjaiEvent]) -> Result<BotResponse, String> {
    if reply.unavailable_reason().is_some()
        || !events.iter().any(|e| {
            matches!(
                e,
                MjaiEvent::RestoreKyoku { .. } | MjaiEvent::ObservedDecision { .. }
            )
        })
    {
        return Ok(reply);
    }
    let meta = reply.meta.as_ref().and_then(|m| m.get("restoration"));
    if !meta.is_some_and(|m| {
        m["origin"] == "public_snapshot"
            && m["temporal_prefix"] == "reset"
            && m["missing_history"] == "unknown_fields"
    }) {
        return Err("Primary did not acknowledge the public-position restore contract; no incomplete or ignored snapshot can become ready.".into());
    }
    for event in events {
        if let MjaiEvent::RestoreKyoku { snapshot } = event {
            if meta.unwrap()["seat"].as_u64() != Some(u64::from(snapshot.seat)) {
                return Err("Primary restore acknowledged a different viewer.".into());
            }
        }
    }
    Ok(reply)
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
            let mut interrupted = false;
            while let Some(work) = observations.blocking_recv() {
                interrupted |= matches!(
                    work.event,
                    Some(MjaiEvent::InterruptedKyoku { .. } | MjaiEvent::RestoreKyoku { .. })
                );
                let mut legal = vec![];
                let mut response = None;
                let result = if interrupted {
                    through = work.through;
                    legal = work.interrupted_legal.unwrap_or_default();
                    response = if work.decide {
                        LocalStandby::observed_reply(&legal)
                    } else {
                        None
                    };
                    policy_error = (work.decide && response.is_none())
                        .then(|| "No safe action is proved by this restored live window.".into());
                    Ok(())
                } else {
                    match &mut engine {
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
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    #[tokio::test]
    async fn semantic_failure_rebuilds_the_complete_real_prefix_after_backoff() {
        use std::sync::{Arc, Mutex};
        struct Semantic {
            through: u64,
            failed: bool,
            calls: Arc<Mutex<Vec<Vec<MjaiEvent>>>>,
        }
        #[async_trait]
        impl BotRunner for Semantic {
            async fn react(&mut self, events: &[MjaiEvent]) -> anyhow::Result<BotResponse> {
                self.calls.lock().unwrap().push(events.to_vec());
                self.through += events.len() as u64;
                if !self.failed && self.through == 2 {
                    self.failed = true;
                    self.through = 1;
                    return Ok(BotResponse {
                        request: None,
                        action: MjaiEvent::None,
                        meta: Some(json!({
                            "delivery":{"through":1},"failure":{"kind":"semantic"},"unavailable":"injected semantic desync"
                        })),
                    });
                }
                Ok(BotResponse {
                    request: None,
                    action: MjaiEvent::None,
                    meta: Some(json!({"delivery":{"through":self.through}})),
                })
            }
            async fn reset(&mut self) -> anyhow::Result<()> {
                self.through = 0;
                Ok(())
            }
        }
        let calls = Arc::new(Mutex::new(vec![]));
        let mut events = crate::bot::recovery::tests::opening();
        events.push(MjaiEvent::Dahai {
            actor: 0,
            pai: "N".into(),
            tsumogiri: true,
        });
        events.push(MjaiEvent::Tsumo {
            actor: 0,
            pai: "E".into(),
        });
        let journal = Arc::new(Mutex::new(events.clone()));
        let (output, mut reports) = mpsc::unbounded_channel();
        let worker = PrimaryWorker::spawn(
            Box::new(Semantic {
                through: 0,
                failed: false,
                calls: calls.clone(),
            }),
            journal,
            output,
            None,
        );
        for (index, event) in events.into_iter().enumerate() {
            if index == 4 {
                tokio::time::sleep(Duration::from_millis(2100)).await;
            }
            let seq = index as u64 + 1;
            worker
                .input
                .send(Observation {
                    context: DecisionContext {
                        game: 1,
                        round: 1,
                        event_seq: seq,
                    },
                    decide: matches!(event, MjaiEvent::Tsumo { .. }),
                    event: Some(event),
                    through: seq,
                    interrupted_legal: None,
                })
                .unwrap();
            let Report::Primary {
                health, response, ..
            } = tokio::time::timeout(Duration::from_secs(3), reports.recv())
                .await
                .unwrap()
                .unwrap()
            else {
                panic!("primary")
            };
            if (2..=4).contains(&seq) {
                assert!(matches!(health, RunnerHealth::Blocked { .. }));
                assert!(response.is_none(), "blocked state is never a decision");
            }
            if seq == 5 {
                assert_eq!(health, RunnerHealth::Ready { through: 5 });
                assert!(response.is_some());
            }
        }
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            3,
            "no duplicate retry layers or per-event resets"
        );
        assert_eq!(calls[2].len(), 5, "recovery skips no real events");
        assert!(matches!(calls[2][0], MjaiEvent::StartGame { .. }));
        assert!(
            matches!(calls[2][3], MjaiEvent::Dahai { .. }),
            "standby-era confirmed move is replayed"
        );
    }

    #[tokio::test]
    async fn blocked_forecast_does_not_delay_the_second_formal_decision() {
        use crate::bot::forecast::tests::{context, receipt, Relay};
        use std::sync::{Arc, Mutex};

        struct Formal {
            through: u64,
            resets: Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait]
        impl BotRunner for Formal {
            async fn react(&mut self, events: &[MjaiEvent]) -> anyhow::Result<BotResponse> {
                self.through += events.len() as u64;
                let mut reply = receipt(self.through, "committed-session");
                if let Some(MjaiEvent::Tsumo { actor, pai }) = events.last() {
                    reply.action = MjaiEvent::Dahai {
                        actor: *actor,
                        pai: pai.clone(),
                        tsumogiri: true,
                    };
                    reply.meta.as_mut().unwrap()["forecast_error"] =
                        json!("injected optional snapshot capture failure");
                }
                Ok(reply)
            }
            async fn reset(&mut self) -> anyhow::Result<()> {
                self.resets
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.through = 0;
                Ok(())
            }
        }
        let relay = Relay::new(false);
        let resets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (output, mut reports) = mpsc::unbounded_channel();
        let worker = PrimaryWorker::spawn(
            Box::new(Formal {
                through: 0,
                resets: resets.clone(),
            }),
            Arc::new(Mutex::new(vec![])),
            output,
            Some(relay.worker.input.clone()),
        );
        let mut events = crate::bot::recovery::tests::opening();
        events.push(MjaiEvent::Dahai {
            actor: 0,
            pai: "N".into(),
            tsumogiri: true,
        });
        events.push(MjaiEvent::Tsumo {
            actor: 0,
            pai: "E".into(),
        });
        let mut decisions = 0;
        for (index, event) in events.into_iter().enumerate() {
            let seq = index as u64 + 1;
            let decide = matches!(event, MjaiEvent::Tsumo { .. });
            relay.worker.input.advance(context(seq), true);
            worker
                .input
                .send(Observation {
                    context: context(seq),
                    event: Some(event),
                    through: seq,
                    interrupted_legal: None,
                    decide,
                })
                .unwrap();
            let report = tokio::time::timeout(Duration::from_secs(2), reports.recv())
                .await
                .expect("formal event is independent of forecast I/O")
                .unwrap();
            let Report::Primary {
                health, response, ..
            } = report
            else {
                panic!("primary report");
            };
            assert_eq!(health, RunnerHealth::Ready { through: seq });
            if decide {
                let reply = response.unwrap();
                assert!(matches!(reply.action, MjaiEvent::Dahai { .. }));
                assert!(
                    reply.meta.as_ref().unwrap().get("forecast_error").is_none(),
                    "internal optional error is consumed"
                );
                decisions += 1;
            } else {
                assert!(response.is_none(), "optional forecasts are never actions");
            }
            if seq == 1 {
                relay.entered().await;
            }
        }
        assert_eq!(decisions, 2);
        assert!(
            !relay.files.path().join("release").exists(),
            "forecast is still blocked when both decisions finish"
        );
        assert_eq!(
            resets.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "optional latency causes no formal recovery"
        );
        drop(worker);
        relay.release();
        relay.worker.close().await;
    }

    #[test]
    fn only_the_requested_committed_prefix_can_supply_a_primary_action() {
        let reply = |through| BotResponse {
            request: None,
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
