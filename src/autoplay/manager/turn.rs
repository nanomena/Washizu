//! Session orchestration of the common turn owner. No website dispatch,
//! retry policy or declaration progression lives in this driver.

use super::{decision::Decision, AutoplayManager};
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::delay::policy::{PacingContext, PacingPlan, PacingPolicy};
use crate::autoplay::delay::{self, DecisionKind};
use crate::autoplay::platform::{self, SiteAdapter, TurnClockFacts, TurnPlan};
use crate::autoplay::pointer::{self, ApproachInput, Touch};
use crate::autoplay::transaction::{
    self, CancelReason, Effect, Facts, InputRequirement, Outcome, Owner,
};
use crate::bot::control::WindowProof;
use crate::config::{AppConfig, Platform};
use crate::monitor::MonitorHandle;
use crate::schema::{MjaiEvent, Notification};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{info, warn};

pub(super) use platform::window_identity;

pub(super) fn owner(ctx: &AutoplayContext) -> Owner {
    Owner {
        session: ctx.ui_session(),
        table: ctx.input_watch.ui_table(),
        settings: ctx.ui_settings(),
        manual: crate::autoplay::cdp_input::user_revision(),
    }
}

async fn facts(
    ctx: &AutoplayContext,
    cfg: &RwLock<AppConfig>,
    monitor: &MonitorHandle,
    platform: Platform,
    turn: &Mutex<transaction::TurnController>,
) -> Facts {
    let (ticket, required) = {
        let turn = turn.lock().unwrap();
        (turn.input_ticket(), turn.requirement())
    };
    let cfg = cfg.read().await;
    let snapshot = monitor.snapshot();
    let input = ctx.input_watch.ticket();
    let command = ticket.is_some_and(|ticket| match required {
        InputRequirement::Any => input.sent_since(ticket),
        InputRequirement::NonDiscard => input.non_discard_since(ticket),
        InputRequirement::Declaration => input.reach_since(ticket),
    });
    Facts {
        context: (snapshot.sync_error.is_none() && !snapshot.round_ended && !snapshot.game_ended)
            .then_some(snapshot.context),
        owner: owner(ctx),
        window: WindowProof::capture(ctx, platform).map(window_identity),
        enabled: cfg.autoplay.enabled && cfg.platform.kind == platform,
        manual: crate::autoplay::cdp_input::user_active(),
        input,
        command,
        latest: snapshot.events.back().map(|event| {
            (
                crate::monitor::DecisionContext {
                    event_seq: event.seq,
                    ..snapshot.context
                },
                event.event.clone(),
            )
        }),
    }
}

impl AutoplayManager {
    pub(super) async fn load_pacing_policy(&mut self, decision: &Decision) {
        let cfg = self.cfg.read().await;
        let root = crate::util::resolve_dir(std::path::Path::new(&cfg.bot.dir));
        let directory = root.join(cfg.bot.active_for(decision.num_players));
        let profile = decision.delay_cfg.policy_override.as_ref().map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                self.config_dir.join(path)
            }
        });
        drop(cfg);
        let manifest = match crate::bot::manifest::Manifest::load(&directory) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.pacing_policy_error(format!("pacing manifest unavailable: {error:#}"));
                return;
            }
        };
        let source = delay::source::resolve_policy_source(
            &directory,
            manifest
                .as_ref()
                .and_then(|manifest| manifest.pacing.as_ref())
                .map(|pacing| pacing.entry.as_str()),
            profile.as_deref(),
        );
        match source {
            Ok(source) => {
                if let Err(reason) = self.delay_script.reload(
                    &source,
                    decision.delay_cfg.mode == crate::config::DelayMode::Lua,
                ) {
                    self.pacing_policy_error(reason);
                }
            }
            Err(reason) => self.pacing_policy_error(reason),
        }
    }

    fn pacing_policy_error(&mut self, reason: String) {
        // ScriptHost/PacingPolicy own the documented built-in policy path.
        // Clear any previous script and report the source error, retaining
        // the selected action and its original input window.
        let _ = self
            .delay_script
            .reload(&delay::source::PolicySource::Bundled, false);
        warn!(%reason, "autoplay: using built-in pacing after policy error");
        let _ = self.notify.send(
            Notification::warn("时延策略不可用")
                .body(reason)
                .id("autoplay-pacing-policy"),
        );
    }

    pub(super) fn pacing(
        &self,
        decision: &Decision,
        action: &MjaiEvent,
        clock: Option<TurnClockFacts>,
        dealt: bool,
        overhead: u32,
    ) -> Result<PacingPlan, delay::policy::PacingError> {
        let now = Instant::now();
        let context = PacingContext {
            action,
            snapshot: decision.snapshot.as_ref(),
            legal_actions: &decision.legal_actions,
            our_seat: decision.our_seat,
            kind: kind(action),
            // A snapshot import has no known previous event. It is not a
            // new hand and must not acquire the opening-turn delay.
            first_action_of_kyoku: decision.snapshot.is_some() && decision.last_kawa_tile.is_none(),
            in_riichi: decision.self_riichi_accepted,
            opening_animation: dealt,
            probs: decision.probs,
            reading: decision.reading,
            clock,
            now,
            earliest_press_at: clock.map(|_| now),
            click_overhead_ms: overhead,
        };
        PacingPolicy::new(
            &decision.cfg,
            &decision.delay_cfg,
            self.delay_script.script(),
        )
        .decide(&context, &mut rand::rng())
    }

    pub(super) async fn execute_turn(
        &mut self,
        adapter: Arc<dyn SiteAdapter>,
        decision: Arc<Decision>,
        action: MjaiEvent,
        plan: TurnPlan,
    ) {
        let mut prepared = None;
        let mut viewport = None;
        loop {
            let intent = { self.turn.lock().unwrap().intent() };
            let Some(intent) = intent else {
                return;
            };
            let page = { self.ctx.page.read().await.clone() };
            let Some(page) = page else {
                self.turn.lock().unwrap().cancel(CancelReason::Ownership);
                return;
            };
            let observed = adapter
                .observe(&page, &decision.context(&action), &intent, None)
                .await;
            let observation = match observed {
                Ok(observation) => observation,
                Err(error) => {
                    warn!(%error, "autoplay: target observation unavailable");
                    self.turn.lock().unwrap().cancel(CancelReason::Target);
                    return;
                }
            };
            let current = facts(
                &self.ctx,
                &self.cfg,
                &self.monitor,
                decision.platform,
                &self.turn,
            )
            .await;
            let effect = {
                self.turn
                    .lock()
                    .unwrap()
                    .next(&current, Some(&observation), Instant::now())
            };
            match effect {
                Effect::Observe => tokio::time::sleep(Duration::from_millis(20)).await,
                Effect::YieldManual => {
                    prepared = None;
                    self.hand.cancel();
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Effect::WaitUntil(at) => {
                    tokio::time::sleep_until(
                        at.min(Instant::now() + Duration::from_millis(20)).into(),
                    )
                    .await
                }
                Effect::Pace { clock } => {
                    let Some(target) = observation.target.as_ref() else {
                        self.turn.lock().unwrap().cancel(CancelReason::Target);
                        continue;
                    };
                    if let Some(layout) = observation.layout.clone() {
                        self.hand.set_layout(layout);
                    }
                    let presses = plan.presses.len() as u32;
                    // First travel is allocated inside the granted wait. Later
                    // unknown positions are bounded by this observed viewport.
                    let later_travel = pointer::travel_ms(
                        target.viewport.width.hypot(target.viewport.height),
                        target.viewport.width / 16.0,
                    );
                    let overhead = presses
                        .saturating_mul(
                            decision
                                .cfg
                                .hover_delay_ms
                                .saturating_add(decision.cfg.click_hold_ms),
                        )
                        .saturating_add(
                            presses.saturating_sub(1).saturating_mul(
                                decision
                                    .cfg
                                    .inter_click_delay_ms
                                    .saturating_add(later_travel),
                            ),
                        );
                    let pacing =
                        match self.pacing(&decision, &action, Some(clock), plan.dealt, overhead) {
                            Ok(pacing) => pacing,
                            Err(error) => {
                                warn!(%error, "autoplay: pacing deadline refused");
                                self.turn.lock().unwrap().cancel(CancelReason::Expired);
                                continue;
                            }
                        };
                    if !self.turn.lock().unwrap().paced(pacing.start_at) {
                        continue;
                    }
                    let point =
                        pointer::aim::target(target.bounds, self.hand.at(), &mut rand::rng());
                    let glance = if pacing.hesitate {
                        if let MjaiEvent::Dahai { pai, .. } = &action {
                            observation
                                .layout
                                .as_ref()
                                .and_then(|layout| {
                                    decision
                                        .runner_up
                                        .as_ref()
                                        .filter(|tile| *tile != pai)
                                        .and_then(|tile| {
                                            layout.target(
                                                &pointer::lean::Spot::Hand(tile.clone()),
                                                self.hand.at(),
                                            )
                                        })
                                })
                                .map(pointer::layout::center)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let approach = self.hand.plan_approach(
                        ApproachInput {
                            think_ms: pacing.wait_ms,
                            target: point,
                            glance,
                            look_ms: pacing.look_ms,
                            rect: target.viewport,
                        },
                        &action,
                    );
                    if !self
                        .wait_turn_until(
                            Instant::now() + Duration::from_millis(u64::from(approach.pause_ms)),
                            decision.platform,
                        )
                        .await
                    {
                        prepared = None;
                        self.hand.cancel();
                        continue;
                    }
                    if !self.hand.walk(&approach).await {
                        let current = facts(
                            &self.ctx,
                            &self.cfg,
                            &self.monitor,
                            decision.platform,
                            &self.turn,
                        )
                        .await;
                        self.turn
                            .lock()
                            .unwrap()
                            .approach_failed(&current, Instant::now());
                        prepared = None;
                        self.hand.cancel();
                        continue;
                    }
                    prepared = Some((target.clone(), point));
                }
                Effect::Press {
                    permit,
                    target,
                    intent,
                    retry,
                } => {
                    if let Some(layout) = observation.layout.clone() {
                        self.hand.set_layout(layout);
                    }
                    let point = prepared
                        .take()
                        .filter(|(bound, point)| {
                            bound == &target && target.bounds.contains(point.0, point.1)
                        })
                        .map(|(_, point)| point)
                        .unwrap_or_else(|| {
                            pointer::aim::target(target.bounds, self.hand.at(), &mut rand::rng())
                        });
                    let touch = Touch {
                        hover_ms: decision.cfg.hover_delay_ms,
                        hold_ms: if retry == 0 {
                            decision.cfg.click_hold_ms
                        } else {
                            decision
                                .cfg
                                .click_hold_ms
                                .saturating_mul(retry + 1)
                                .min(2_000)
                        },
                        jiggle: matches!(action, MjaiEvent::Reach { .. }) || retry > 1,
                    };
                    let ctx = self.ctx.clone();
                    let cfg = self.cfg.clone();
                    let monitor = self.monitor.clone();
                    let turn = self.turn.clone();
                    let adapter = adapter.clone();
                    let platform = decision.platform;
                    let decision = decision.clone();
                    let selected = action.clone();
                    let pressed = self
                        .hand
                        .press_when(
                            "selected turn input",
                            point,
                            target.viewport,
                            touch,
                            move |page| async move {
                                let observed = adapter
                                    .observe(
                                        &page,
                                        &decision.context(&selected),
                                        &intent,
                                        Some(point),
                                    )
                                    .await;
                                let current =
                                    facts(&ctx, &cfg, &monitor, decision.platform, &turn).await;
                                let Ok(observed) = observed else {
                                    turn.lock().unwrap().cancel(CancelReason::Target);
                                    return false;
                                };
                                let allowed = turn.lock().unwrap().consume(
                                    permit,
                                    &current,
                                    &observed,
                                    point,
                                    touch.hold_ms,
                                    Instant::now(),
                                );
                                allowed
                            },
                        )
                        .await;
                    let current =
                        facts(&self.ctx, &self.cfg, &self.monitor, platform, &self.turn).await;
                    self.turn
                        .lock()
                        .unwrap()
                        .dispatched(permit, pressed, &current, Instant::now());
                    viewport = Some(target.viewport);
                }
                Effect::Finished { outcome, reload } => {
                    info!(?outcome, ?action, "autoplay: selected turn finished");
                    if matches!(outcome, Outcome::ClientAccepted | Outcome::ServerConfirmed) {
                        if let Some(viewport) = viewport {
                            self.hand.rest_after(viewport, &action);
                        }
                    } else {
                        self.hand.cancel();
                    }
                    if reload {
                        let _ =
                            self.notify
                                .send(Notification::warn("Reloading the game").body(
                                    "Client input remained unconfirmed; reconnecting to recover.",
                                ));
                        self.ctx.invalidate_ui();
                        if let Err(error) = page.reload().await {
                            warn!(%error, "autoplay: client reload failed");
                        }
                        self.canvas.forget(&self.ctx).await;
                    }
                    return;
                }
            }
        }
    }
    async fn wait_turn_until(&self, until: Instant, platform: Platform) -> bool {
        loop {
            let current = facts(&self.ctx, &self.cfg, &self.monitor, platform, &self.turn).await;
            let effect = {
                self.turn
                    .lock()
                    .unwrap()
                    .next(&current, None, Instant::now())
            };
            if matches!(effect, Effect::Finished { .. } | Effect::YieldManual) {
                return false;
            }
            let now = Instant::now();
            if now >= until {
                return true;
            }
            tokio::time::sleep_until(until.min(now + Duration::from_millis(20)).into()).await;
        }
    }
}

fn kind(action: &MjaiEvent) -> DecisionKind {
    match action {
        MjaiEvent::Dahai { .. } => DecisionKind::Dahai,
        MjaiEvent::Reach { .. } => DecisionKind::Reach,
        MjaiEvent::Chi { .. } => DecisionKind::Chi,
        MjaiEvent::Pon { .. } => DecisionKind::Pon,
        MjaiEvent::Daiminkan { .. } => DecisionKind::Daiminkan,
        MjaiEvent::Ankan { .. } => DecisionKind::Ankan,
        MjaiEvent::Kakan { .. } => DecisionKind::Kakan,
        MjaiEvent::Hora { .. } => DecisionKind::Hora,
        MjaiEvent::Ryukyoku { .. } => DecisionKind::Ryukyoku,
        MjaiEvent::Kita { .. } => DecisionKind::Kita,
        _ => DecisionKind::Pass,
    }
}
