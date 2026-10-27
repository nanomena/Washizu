//! One decision-source owner. Workers report positioned state; only this
//! coordinator selects a source and publishes a response. UI phases and
//! final input permission belong to the independent UI effect owner.

mod journal;
mod standby;
#[cfg(test)]
mod tests;
mod worker;

use crate::autoplay::AutoplayContext;
use crate::bot::control::{DecisionControl, DecisionSource, RunnerHealth, WindowProof};
use crate::bot::{BotResponse, BotRunner};
use crate::config::{AppConfig, Platform};
use crate::event_bus::{BotResponseBus, TrackedEvent};
use crate::inspector::InspectorWriter;
use crate::monitor::{DecisionContext, MonitorHandle};
use crate::schema::{BotReaction, InspectorEntry, MjaiEvent};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use worker::{Observation, PrimaryWorker, Report, StandbyWorker};

pub struct RecoveryHandle {
    input: mpsc::UnboundedSender<TrackedEvent>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for RecoveryHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RecoveryHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        primary: Box<dyn BotRunner>,
        seat: u8,
        players: u8,
        name: String,
        config: Arc<RwLock<AppConfig>>,
        clock: Arc<AutoplayContext>,
        monitor: MonitorHandle,
        output: BotResponseBus,
        inspector: InspectorWriter,
    ) -> Self {
        let (input, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            Coordinator::new(
                primary, seat, players, name, config, clock, monitor, output, inspector,
            )
            .run(events)
            .await;
        });
        Self { input, task }
    }

    pub fn feed(&self, event: TrackedEvent) -> anyhow::Result<()> {
        self.input
            .send(event)
            .map_err(|_| anyhow::anyhow!("decision coordinator stopped"))
    }
}

struct Pending {
    context: DecisionContext,
    window: Option<WindowProof>,
    trigger: MjaiEvent,
    started: Instant,
    deadline: Instant,
    reserve_ms: u32,
    remaining_ms: u32,
    monitor_id: Option<u64>,
    primary: Option<BotResponse>,
    standby: Option<BotResponse>,
    legal: Option<Vec<MjaiEvent>>,
}

struct Coordinator {
    journal: journal::Journal,
    primary: PrimaryWorker,
    standby: StandbyWorker,
    reports: mpsc::UnboundedReceiver<Report>,
    seat: u8,
    name: String,
    config: Arc<RwLock<AppConfig>>,
    clock: Arc<AutoplayContext>,
    monitor: MonitorHandle,
    output: BotResponseBus,
    inspector: InspectorWriter,
    pending: Option<Pending>,
    selected: Option<WindowProof>,
    primary_health: RunnerHealth,
    standby_health: RunnerHealth,
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    fn new(
        primary: Box<dyn BotRunner>,
        seat: u8,
        players: u8,
        name: String,
        config: Arc<RwLock<AppConfig>>,
        clock: Arc<AutoplayContext>,
        monitor: MonitorHandle,
        output: BotResponseBus,
        inspector: InspectorWriter,
    ) -> Self {
        let (reports_tx, reports) = mpsc::unbounded_channel();
        let journal = journal::Journal::new();
        Self {
            primary: PrimaryWorker::spawn(primary, journal.events.clone(), reports_tx.clone()),
            journal,
            standby: StandbyWorker::spawn(seat, players, reports_tx),
            reports,
            seat,
            name,
            config,
            clock,
            monitor,
            output,
            inspector,
            pending: None,
            selected: None,
            primary_health: RunnerHealth::Recovering {
                through: 0,
                reason: "Primary is starting.".into(),
            },
            standby_health: RunnerHealth::Recovering {
                through: 0,
                reason: "Local standby is starting.".into(),
            },
        }
    }

    async fn run(mut self, mut events: mpsc::UnboundedReceiver<TrackedEvent>) {
        loop {
            let deadline = self
                .pending
                .as_ref()
                .map(|p| tokio::time::Instant::from_std(p.deadline));
            tokio::select! {
                // Retire obsolete windows before interpreting an old model reply.
                biased;
                event = events.recv() => match event {
                    Some(event) => if !self.observe(event).await { return; },
                    None => return,
                },
                report = self.reports.recv() => match report {
                    Some(report) => { self.report(report); self.select(false); },
                    None => return,
                },
                _ = async { match deadline { Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await } } => { self.select(true); },
            }
        }
    }

    async fn observe(&mut self, tracked: TrackedEvent) -> bool {
        let Some(context) = tracked.context else {
            self.monitor.gap(
                "A real event lacked its tracked identity; no recovery decision can be validated."
                    .into(),
            );
            return false;
        };
        let (event, through, frontier) = match self.journal.ingest(context, tracked.event.clone()) {
            Ok(journal::Delivery::Duplicate) => return true,
            Ok(journal::Delivery::Observe {
                event,
                through,
                frontier,
            }) => (event, through, frontier),
            Err(error) => {
                self.monitor
                    .gap(format!("Recovery journal rejected real history: {error:#}"));
                return false;
            }
        };
        let boundary = matches!(
            tracked.event,
            MjaiEvent::StartGame { .. }
                | MjaiEvent::StartKyoku { .. }
                | MjaiEvent::EndKyoku
                | MjaiEvent::EndGame { .. }
                | MjaiEvent::Hora { .. }
                | MjaiEvent::Ryukyoku { .. }
        );
        if boundary {
            self.pending = None;
            self.selected = None;
        }
        if matches!(tracked.event, MjaiEvent::StartGame { .. }) {
            self.monitor.set_control(DecisionControl {
                context,
                window: None,
                source: DecisionSource::Unavailable,
                primary: self.primary_health.clone(),
                standby: self.standby_health.clone(),
            });
        }
        let cfg = self.config.read().await;
        let window = WindowProof::capture(&self.clock, cfg.platform.kind);
        let can_decide = frontier
            && !boundary
            && tracked.can_act == Some(true)
            && (opens_window(&tracked.event, self.seat) || self.pending.is_some())
            && (window.is_none() || window != self.selected);
        let work = Observation {
            context,
            event,
            through,
            decide: can_decide,
        };
        if self.primary.input.send(work.clone()).is_err() || self.standby.input.send(work).is_err()
        {
            self.monitor
                .gap("A recovery worker stopped; no decision source can be selected.".into());
            return false;
        }
        if can_decide {
            // Original identity comes only from TrackedEvent. A snapshot can
            // reject it as obsolete; it can never replace that identity.
            if self.monitor.snapshot().context != context {
                return true;
            }
            let now = Instant::now();
            let (deadline, reserve_ms, remaining_ms) = inference_deadline(&self.clock, &cfg, now);
            let id = self.monitor.begin(context, &self.name, &tracked.event);
            self.monitor.set_control(DecisionControl {
                context,
                window,
                source: DecisionSource::Unavailable,
                primary: self.primary_health.clone(),
                standby: self.standby_health.clone(),
            });
            self.pending = Some(Pending {
                context,
                window,
                trigger: tracked.event,
                started: now,
                deadline,
                reserve_ms,
                remaining_ms,
                monitor_id: id,
                primary: None,
                standby: None,
                legal: None,
            });
        } else if self.pending.as_ref().is_some_and(|p| {
            p.context.game != context.game || p.window != window || tracked.can_act != Some(true)
        }) {
            self.pending = None;
        }
        true
    }

    fn report(&mut self, report: Report) {
        match report {
            Report::Primary {
                context,
                health,
                response,
            } => {
                self.primary_health = health;
                if let Some(pending) = self.pending.as_mut().filter(|p| p.context == context) {
                    pending.primary = response;
                }
            }
            Report::Standby {
                context,
                health,
                response,
                legal,
            } => {
                self.standby_health = health;
                if let Some(pending) = self.pending.as_mut().filter(|p| p.context == context) {
                    pending.standby = response;
                    pending.legal = Some(legal);
                }
            }
        }
        self.monitor
            .set_runner_health(self.primary_health.clone(), self.standby_health.clone());
    }

    /// The sole arbitration point. Taking pending commits this window's
    /// source, so later reports cannot publish, replace or click it again.
    fn select(&mut self, deadline_elapsed: bool) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let snapshot = self.monitor.snapshot();
        if snapshot.context != pending.context
            || snapshot.sync_error.is_some()
            || pending.window.is_some_and(|w| !w.is_current(&self.clock))
        {
            self.pending = None;
            return;
        }
        let legal = pending.legal.as_deref().unwrap_or_default();
        let primary_valid = pending.window.is_some()
            && pending
                .primary
                .as_ref()
                .is_some_and(|r| standby::legal_response(r, legal));
        let standby_valid = pending.window.is_some()
            && pending
                .standby
                .as_ref()
                .is_some_and(|r| standby::legal_response(r, legal));
        let terminal_primary = matches!(self.primary_health, RunnerHealth::Blocked { .. });
        let source = if primary_valid && Instant::now() < pending.deadline {
            DecisionSource::Primary
        } else if (deadline_elapsed || terminal_primary) && standby_valid {
            DecisionSource::NativeStandby
        } else if deadline_elapsed {
            DecisionSource::Unavailable
        } else {
            return;
        };
        let pending = self.pending.take().expect("pending checked");
        self.selected = pending.window;
        if source != DecisionSource::Primary
            && matches!(self.primary_health, RunnerHealth::Ready { .. })
        {
            let RunnerHealth::Ready { through } = self.primary_health else {
                unreachable!()
            };
            let reason = if let Some(reply) = pending.primary.as_ref().filter(|_| !primary_valid) {
                format!(
                    "Primary reply rejected by the independent full legal set: {:?}",
                    reply.action
                )
            } else {
                "No legal primary answer for the current real event prefix before the inference deadline.".into()
            };
            tracing::warn!(context = ?pending.context, %reason, "primary cannot own this decision window");
            self.primary_health = RunnerHealth::Recovering { through, reason };
        }
        let control = DecisionControl {
            context: pending.context,
            window: pending.window,
            source,
            primary: self.primary_health.clone(),
            standby: self.standby_health.clone(),
        };
        let mut response = match source {
            DecisionSource::Primary => pending.primary.expect("validated primary"),
            DecisionSource::NativeStandby => pending.standby.expect("validated standby"),
            DecisionSource::Unavailable => BotResponse {
                action: MjaiEvent::None,
                meta: Some(
                    serde_json::json!({"unavailable":"No validated decision source answered within this window's inference budget."}),
                ),
            },
        };
        response.set_control(control.clone());
        let latency = pending.started.elapsed().as_millis() as u64;
        response.meta.as_mut().expect("control creates meta")["recovery_timing"] = serde_json::json!({
            "decision_ms": latency, "remaining_ms": pending.remaining_ms, "input_reserve_ms": pending.reserve_ms });
        self.monitor.set_control(control);
        if let Some(id) = pending.monitor_id {
            self.monitor.complete(id, &response, latency);
        }
        self.inspector.record(InspectorEntry::BotReaction {
            ts_ms: chrono::Local::now().timestamp_millis(),
            reaction: BotReaction {
                bot: self.name.clone(),
                actor_id: self.seat,
                trigger: pending.trigger,
                action: response.action.clone(),
                meta: response.meta.clone(),
                reaction_ms: latency,
            },
        });
        tracing::info!(?source, context = ?pending.context, latency_ms = latency,
            input_reserve_ms = pending.reserve_ms, "coordinator selected one decision source");
        if source != DecisionSource::Unavailable {
            let _ = self.output.send(response);
        }
    }
}

fn opens_window(event: &MjaiEvent, seat: u8) -> bool {
    match event {
        MjaiEvent::Tsumo { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. } => *actor == seat,
        MjaiEvent::Dahai { actor, .. } | MjaiEvent::Kakan { actor, .. } => *actor != seat,
        _ => false,
    }
}

fn inference_deadline(ctx: &AutoplayContext, cfg: &AppConfig, now: Instant) -> (Instant, u32, u32) {
    let click = &cfg.autoplay.majsoul;
    // Reserve a three-input plan, pointer travel, configured scheduling margin.
    let reserve = cfg
        .autoplay
        .delay
        .safety_margin_ms
        .saturating_add(3 * click.hover_delay_ms.saturating_add(click.click_hold_ms))
        .saturating_add(2 * click.inter_click_delay_ms)
        .saturating_add(600);
    let remaining = match cfg.platform.kind {
        Platform::Majsoul => ctx.time_budget.read().ok().and_then(|g| *g).map(|b| {
            b.fixed_ms
                .saturating_add(
                    ((f64::from(b.add_ms) * cfg.autoplay.delay.bank_use_fraction) as u32)
                        .min(cfg.autoplay.delay.bank_max_single_ms),
                )
                .saturating_sub(
                    u32::try_from(now.saturating_duration_since(b.opened_at).as_millis())
                        .unwrap_or(u32::MAX),
                )
        }),
        Platform::Tenhou => ctx
            .tenhou_state
            .read()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.window))
            .map(|w| {
                crate::autoplay::tenhou_state::TURN_BASE_MS.saturating_sub(
                    u32::try_from(now.saturating_duration_since(w.opened_at).as_millis())
                        .unwrap_or(u32::MAX),
                )
            }),
        _ => None,
    }
    .unwrap_or(0);
    (
        now + Duration::from_millis(u64::from(remaining.saturating_sub(reserve))),
        reserve,
        remaining,
    )
}
