//! Autoplay manager: subscribes to bot decisions and mjai events, and turns
//! each decision into presses in the client (or a frame, for Riichi City).
//!
//! Lifecycle: spawned by `crate::lib::run` when `cfg.autoplay.enabled =
//! true`; one long-lived `tokio::select!` loop over the bot's answers, the
//! mjai stream, the bot's leans and the pointer's own looks between turns.
//!
//! A decision goes through four stages, each with its own module:
//!
//! - [`decision`] reads the table and the bot's answer into a `Decision`;
//! - the platform plans it ([`PlatformAutoplay::plan`]), and the pointer
//!   plans its way to the first press ([`crate::autoplay::pointer::Hand`]);
//! - [`steps`] carries the plan out;
//! - [`press`] reports input evidence and executes only machine-authorized
//!   retries or client reload effects.
//!
//! [`events`] keeps the per-game bookkeeping the mjai stream drives, and
//! [`table_acts`] executes observed UI effects. [`ui_input`] applies the same
//! final capture and single-use permission to every Mahjong Soul mouse-down.
//!
//! Missing pages, unknown pixels, changed ownership and obsolete effects
//! withhold input. Phase changes and refused presses include their reasons
//! in the log. Model recovery and source selection belong to the coordinator.

mod decision;
mod events;
mod press;
mod steps;
mod table_acts;
mod ui_input;

use crate::autoplay::canvas::Canvas;
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::humanize;
use crate::autoplay::majsoul::MajsoulAutoplay;
use crate::autoplay::platform::{PlatformAutoplay, Step};
use crate::autoplay::pointer::{self, Hand};
use crate::autoplay::riichi_city::RiichiCityAutoplay;
use crate::autoplay::tenhou::TenhouAutoplay;
use crate::bot::BotResponse;
use crate::config::{AppConfig, Platform};
use crate::event_bus::{BotResponseBus, MjaiBus, NotifyBus};
use crate::game_state::tracker::GameTracker;
use crate::schema::MjaiEvent;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast::error::RecvError, Mutex, RwLock};
use tracing::{debug, info, warn};

pub struct AutoplayManager {
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    monitor: crate::monitor::MonitorHandle,
    mjai_bus: MjaiBus,
    /// For telling the user about a decision that came out wrong — see the
    /// riichi check and the dead-click reload in [`press`].
    notify: NotifyBus,
    /// One implementation per supported platform, selected per decision from
    /// the live config so a platform switch takes effect without a restart.
    /// Majsoul synthesises clicks; Tenhou encodes a client frame.
    majsoul: MajsoulAutoplay,
    tenhou: TenhouAutoplay,
    /// Riichi City: no page to click — plans a protocol frame the proxy
    /// transmits (see `autoplay::inject`).
    riichi: RiichiCityAutoplay,
    state: ManagerState,
    /// The pointer over the current hand.
    hand: Hand,
    /// Sole Mahjong Soul UI phase/effect/input authority.
    ui: Arc<std::sync::Mutex<crate::autoplay::ui::Machine>>,
    /// Auto-win wishes, independent of phase and without click authority.
    toggles: crate::autoplay::toggles::Toggles,
    /// When the canvas rect was last measured.
    canvas: Canvas,
    /// User Lua delay policy (hot-reloaded from disk; see
    /// `autoplay::delay::script`).
    delay_script: crate::autoplay::delay::ScriptHost,
    /// The built-in bot following the game, for a second opinion on how
    /// obvious each decision is (see `autoplay::advisor`).
    advisor: crate::autoplay::advisor::Advisor,
    /// Directory holding the loaded config file; the script lives at
    /// `<config_dir>/delay.lua`.
    config_dir: std::path::PathBuf,
}

/// What the manager keeps over a game; [`events`] says what each hand keeps.
#[derive(Default)]
struct ManagerState {
    last_kawa_tile: Option<String>,
    last_self_tsumo: Option<String>,
    self_riichi_accepted: bool,
    /// Cached seat index for our player. Captured directly from
    /// `StartGame { id }` and kept across kyoku resets. Avoids try_lock
    /// failures in the synchronous mjai event handler causing missed
    /// tsumo/dahai updates, and is available from the very first event
    /// rather than waiting for the first successful `handle_bot_response`.
    cached_our_seat: Option<u8>,
    /// Tenhou: the decision window we last acted on, so the extra bot
    /// responses that arrive for the same one are dropped before they are
    /// planned rather than after.
    acted_window: Option<Instant>,
    /// Players at the table, from `start_game`.
    num_players: u8,
}

impl AutoplayManager {
    pub fn new(
        cfg: Arc<RwLock<AppConfig>>,
        ctx: Arc<AutoplayContext>,
        tracker: Arc<Mutex<GameTracker>>,
        mjai_bus: MjaiBus,
        notify: NotifyBus,
        config_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            hand: Hand::new(ctx.clone()),
            ui: Default::default(),
            toggles: Default::default(),
            cfg,
            ctx,
            tracker,
            monitor: crate::monitor::MonitorHandle::default(),
            mjai_bus,
            notify,
            majsoul: MajsoulAutoplay::new(),
            tenhou: TenhouAutoplay::new(),
            riichi: RiichiCityAutoplay::new(),
            state: ManagerState::default(),
            canvas: Canvas::default(),
            delay_script: crate::autoplay::delay::ScriptHost::default(),
            advisor: crate::autoplay::advisor::Advisor::default(),
            config_dir,
        }
    }

    pub fn with_monitor(mut self, monitor: crate::monitor::MonitorHandle) -> Self {
        self.monitor = monitor;
        self
    }

    /// Run forever. Returns `Err` only on bus closure (process exit).
    pub async fn run(mut self, response_bus: BotResponseBus) -> anyhow::Result<()> {
        let mut bot_rx = response_bus.subscribe();
        let mut mjai_rx = self.mjai_bus.subscribe();
        let mut lean_rx = pointer::lean::subscribe();
        info!("autoplay manager started");
        // The round-advance watcher runs on its own subscription: at hand
        // end the plan loop is busy draining stale plans and the OK press
        // must not inherit that delay.
        let advance_cfg = self.cfg.clone();
        let advance_inject = self.ctx.inject.clone();
        let advance_bus = self.mjai_bus.clone();
        tokio::spawn(async move {
            crate::autoplay::riichi_city::round_advance::round_advance_watcher(
                advance_cfg,
                advance_inject,
                advance_bus,
            )
            .await;
        });

        loop {
            self.sync_ui().await;
            let context = self.current_context();
            self.hand.retain_current_forecast(context);
            // The pointer's own pace between turns, apart from any event.
            let glance_at = self.hand.glance_due();
            // And its presses between decisions.
            let act_at = [self.ui.lock().unwrap().due(), self.toggles.due()]
                .into_iter()
                .flatten()
                .min();
            // And its unhurried moves, a frame at a time, so a press can cut them short.
            let motion_at = self.hand.motion_due();
            tokio::select! {
                // Sample settings/manual ownership even on an idle lobby.
                // This timer reports facts; it never authorizes a click.
                _ = tokio::time::sleep(Duration::from_millis(150)) => {},
                _ = tokio::time::sleep_until(motion_at.unwrap_or_else(Instant::now).into()), if motion_at.is_some() => {
                    self.hand.step().await;
                }
                _ = tokio::time::sleep_until(glance_at.unwrap_or_else(Instant::now).into()), if glance_at.is_some() => {
                    self.glance().await;
                }
                _ = tokio::time::sleep_until(act_at.unwrap_or_else(Instant::now).into()), if act_at.is_some() => {
                    self.table_act().await;
                }
                msg = lean_rx.recv() => match msg {
                    Ok(lean) => {
                        let requested = { let cfg = self.cfg.read().await; cfg.monitor.pointer_lean && cfg.autoplay.enabled };
                        if requested && lean.is_current(self.current_context()) { self.hand.take_lean(lean); }
                    },
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return Ok(()),
                },
                msg = bot_rx.recv() => match msg {
                    Ok(resp) => {
                        // A response is produced after tracker application, while
                        // this consumer can still have the corresponding raw
                        // protocol events queued. Admit only after applying them
                        // in their original order to the sole UI phase owner.
                        self.apply_pending_protocol(&mut mjai_rx).await;
                        self.handle_bot_response(resp).await;
                    },
                    Err(RecvError::Lagged(n)) => warn!("autoplay: bot bus lagged {n}"),
                    Err(RecvError::Closed) => {
                        info!("autoplay: bot bus closed; exiting");
                        return Ok(());
                    }
                },
                msg = mjai_rx.recv() => match msg {
                    Ok(ev) => {
                        self.sync_ui().await;
                        self.handle_mjai_event(&ev.event);
                    }
                    Err(RecvError::Lagged(n)) => {
                        warn!("autoplay: mjai bus lagged {n}; revoking UI session");
                        self.ctx.invalidate_ui();
                        self.hand.cancel();
                    },
                    Err(RecvError::Closed) => {
                        info!("autoplay: mjai bus closed; exiting");
                        return Ok(());
                    }
                },
            }
        }
    }

    async fn apply_pending_protocol(
        &mut self,
        rx: &mut tokio::sync::broadcast::Receiver<crate::event_bus::CapturedEvent>,
    ) {
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    self.sync_ui().await;
                    self.handle_mjai_event(&event.event);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    self.ctx.invalidate_ui();
                    self.hand.cancel();
                }
                Err(_) => break,
            }
        }
    }

    /// One decision, from the bot's answer to the pointer's rest after it.
    async fn handle_bot_response(&mut self, resp: BotResponse) {
        self.sync_ui().await;
        let Some(mut decision) = self.read_decision(&resp).await else {
            return;
        };
        // Every reply that gets here is one the engine asked for: the bot
        // manager only reacts where our seat can act (`bot::manager`), so a
        // `None` means a decline and not "nothing to say". Belt and braces
        // all the same — one window is answered once. A second reply for it
        // is at best a wasted press and at worst one aimed at whatever
        // replaced it.
        if let Some(w) = decision.planned_window {
            if self.state.acted_window == Some(w.opened_at) {
                debug!("autoplay: already acted on this window; ignoring extra bot reply");
                return;
            }
        }

        let (mut plan, glance) = {
            let action_ctx =
                decision.context(&resp.action, &self.state, self.delay_script.script());
            let platform: &dyn PlatformAutoplay = match decision.platform {
                Platform::Tenhou => &self.tenhou,
                Platform::RiichiCity => &self.riichi,
                _ => &self.majsoul,
            };
            let plan = platform.plan(&action_ctx);
            // The runner-up's place in the hand, when the delay model asked
            // for a look at it and the bot's two likeliest candidates are
            // discards of different tiles (see `autoplay::pointer`).
            let glance = match (&resp.action, plan.hesitate) {
                (MjaiEvent::Dahai { pai, .. }, true) => {
                    let candidates = crate::autoplay::delay::probs::candidates(resp.meta.as_ref());
                    pointer::runner_up(&candidates, pai).and_then(|other| {
                        crate::autoplay::majsoul::discard_point(other, &action_ctx)
                    })
                }
                _ => None,
            };
            (plan, glance)
        };
        if plan.steps.is_empty() {
            return;
        }
        // A press lands somewhere in its target's middle, not on its exact middle.
        if decision.platform == Platform::Majsoul {
            pointer::aim::scatter(
                &mut plan,
                decision.pointer,
                crate::autoplay::majsoul::reach_of,
                &mut rand::rng(),
            );
        }
        debug!(
            "autoplay: action={:?} steps={}",
            resp.action,
            plan.steps.len(),
        );

        // Only a click needs to know where the canvas is. A plan built of
        // `Send` steps talks to the page's socket and would otherwise be
        // held hostage by a rect query it never uses.
        let needs_canvas = plan.steps.iter().any(|s| matches!(s, Step::Click { .. }));
        let rect = if needs_canvas {
            // Cache + TTL. If we can't resolve one, drop the click — the page
            // handle isn't ready yet (e.g. user still on the lobby), or the
            // chromium backend isn't running at all.
            match self.canvas.resolve(&self.ctx).await {
                Some(r) => Some(r),
                None => {
                    warn!(
                        "autoplay: no canvas rect — skipping click for {:?}",
                        resp.action
                    );
                    return;
                }
            }
        } else {
            None
        };

        // Ticket taken before the first press: the check afterwards asks
        // whether the client sent *another* input command, so an input from
        // the previous decision cannot be mistaken for this one landing.
        let ticket = self.ctx.input_watch.ticket();

        if decision.platform == Platform::Majsoul {
            let Some(_window) = decision
                .planned_budget
                .map(crate::autoplay::ui::Window::from)
            else {
                warn!("autoplay: no authoritative Majsoul window; withholding semantic input");
                return;
            };
            self.sync_ui().await;
            decision.ui_token = self
                .ui
                .lock()
                .unwrap()
                .begin_decision(decision.request, decision.cfg.click_retries);
            if decision.ui_token.is_none() {
                debug!("autoplay: phase/window already consumed or unavailable; withholding reply");
                return;
            }
        }

        if let Some(w) = decision.planned_window {
            self.state.acted_window = Some(w.opened_at);
        }

        if decision.platform == Platform::RiichiCity {
            crate::autoplay::riichi_city::send::spawn(
                &plan,
                &decision.cfg,
                &resp.action,
                self.ctx.inject.clone(),
            );
            return;
        }

        // The pointer's way to the first press is made inside the thinking
        // time, not after it: the pause that leads the plan gives up
        // exactly what the way takes, so the press lands when it would
        // have (see `autoplay::pointer`).
        // Not before what it answers can have been seen: the window opened with its frame,
        // or, at the dealer's opening, with a deal still to be laid out.
        let dealt = if plan.dealt { humanize::DEAL_MS } else { 0 };
        let shown_at = decision
            .planned_budget
            .map(|b| b.opened_at + Duration::from_millis(u64::from(dealt)));
        // A rest or a look under way brakes where it is; the way to the
        // press starts from there.
        if rect.is_some() {
            self.hand.give_way().await;
        }
        let approach = rect.and_then(|rect| {
            self.hand
                .plan_approach(&mut plan, rect, glance, shown_at, &resp.action)
        });
        if approach.as_ref().is_some_and(|a| a.looks_elsewhere()) {
            info!(
                "autoplay: looking at the runner-up before {:?}",
                resp.action
            );
        }

        if !self
            .execute(&plan, approach, rect, &decision, &resp.action)
            .await
        {
            if let Some(token) = decision.ui_token {
                self.ui.lock().unwrap().finish_decision(token);
            }
            return;
        }
        self.prove(&plan, rect, ticket, &decision, &resp.action)
            .await;
        if let Some(token) = decision.ui_token {
            self.ui.lock().unwrap().finish_decision(token);
        }
    }

    /// One look between turns: at one of the tiles the bot expects to throw,
    /// or nowhere new (see `autoplay::pointer::lean`). Majsoul only, the
    /// seat's hand at rest, autoplay on and not in riichi; otherwise the
    /// looks stop.
    async fn glance(&mut self) {
        let (wanted, platform) = {
            let cfg = self.cfg.read().await;
            (
                cfg.monitor.pointer_lean && cfg.autoplay.enabled,
                cfg.platform.kind,
            )
        };
        if !wanted || platform != Platform::Majsoul || self.state.self_riichi_accepted {
            self.hand.stop_glances();
            return;
        }
        let context = self.current_context();
        self.hand.retain_current_forecast(context);
        let Some(spot) = self.hand.next_glance() else {
            return;
        };
        // The user's hand is on the mouse over the game: this look can wait for the next.
        if crate::autoplay::cdp_input::user_active() {
            return;
        }
        let Some(seat) = self.state.cached_our_seat else {
            return;
        };
        let tehai = {
            let tracker = self.tracker.lock().await;
            match tracker
                .snapshot()
                .and_then(|s| s.players.get(seat as usize).map(|p| p.tehai.clone()))
            {
                Some(tehai) => tehai,
                None => return,
            }
        };
        // Between turns a hand holds 13, 10, 7, 4 or 1 tiles.
        if tehai.len() % 3 != 1 {
            return;
        }
        let Some(rect) = self.canvas.resolve(&self.ctx).await else {
            return;
        };
        if !self.hand.forecast_current(self.current_context()) {
            return;
        }
        // Where its next draw lands is kept clear until the tile is there.
        self.hand.saw_hand(tehai.len(), rect);
        self.hand.look_at(spot, &tehai, rect);
    }

    /// Best-effort seat lookup. Uses the cached seat from `StartGame` first,
    /// falling back to try_lock on the tracker.
    fn our_seat_cached(&self) -> Option<u8> {
        self.state
            .cached_our_seat
            .or_else(|| self.tracker.try_lock().ok().and_then(|t| t.our_seat()))
    }

    /// A manager with no page and default config, for the stages' tests.
    #[cfg(test)]
    fn for_tests() -> Self {
        Self::new(
            Arc::new(RwLock::new(AppConfig::default())),
            Arc::new(AutoplayContext::default()),
            crate::game_state::tracker::new_handle(),
            crate::event_bus::mjai_bus(),
            crate::event_bus::notify_bus(),
            std::env::temp_dir(),
        )
    }
}

/// Spawn point for the autoplay loop. Must be called on the Tokio runtime:
/// the manager spawns its own helper tasks there.
pub async fn run_autoplay_manager(
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    mjai_bus: MjaiBus,
    response_bus: BotResponseBus,
    notify: NotifyBus,
    config_dir: std::path::PathBuf,
    monitor: crate::monitor::MonitorHandle,
) -> anyhow::Result<()> {
    AutoplayManager::new(cfg, ctx, tracker, mjai_bus, notify, config_dir)
        .with_monitor(monitor)
        .run(response_bus)
        .await
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[tokio::test]
    async fn a_fast_new_game_reply_is_admitted_after_its_queued_protocol_boundaries() {
        use crate::autoplay::{BudgetSource, TimeBudget};
        use crate::bot::control::{
            DecisionControl, DecisionRequest, DecisionSource, InputWindow, RunnerHealth,
        };
        let mut manager = AutoplayManager::for_tests();
        manager.cfg.write().await.autoplay.enabled = true;
        let budget = TimeBudget {
            fixed_ms: 5000,
            add_ms: 0,
            opened_at: Instant::now(),
            source: BudgetSource::DealTile,
        };
        *manager.ctx.time_budget.write().unwrap() = Some(budget);
        let mut rx = manager.mjai_bus.subscribe();
        for event in crate::bot::recovery::tests::opening() {
            let mut tracker = manager.tracker.lock().await;
            tracker.handle(&event).unwrap();
            manager.monitor.game_event(&event, tracker.snapshot(), None);
            manager.mjai_bus.send(event.into()).unwrap();
        }
        let request = DecisionRequest {
            context: manager.monitor.snapshot().context,
            window: Some(InputWindow::Majsoul(budget)),
        };
        let mut reply = crate::bot::BotResponse {
            request: Some(request),
            action: MjaiEvent::Dahai {
                actor: 0,
                pai: "N".into(),
                tsumogiri: true,
            },
            meta: None,
        };
        reply.set_control(DecisionControl {
            context: request.context,
            window: Some(request.window.unwrap().proof()),
            source: DecisionSource::NativeStandby,
            primary: RunnerHealth::Recovering {
                through: 2,
                reason: "late".into(),
            },
            standby: RunnerHealth::Ready { through: 3 },
        });
        manager.apply_pending_protocol(&mut rx).await;
        // This offline test supplies an available fake browser; production
        // sync_ui reads the actual page handle and final capture separately.
        let mut env =
            ui_input::environment(&manager.ctx, &manager.cfg, true, &manager.monitor).await;
        env.manual = false; // fake browser has no human pointer owner
        manager.ui.lock().unwrap().synchronize(env, Instant::now());
        let decision = manager.read_decision(&reply).await.unwrap();
        assert_eq!(decision.request, request);
        let mut ui = manager.ui.lock().unwrap();
        assert!(ui.begin_decision(decision.request, 0).is_some(), "{ui:?}");
        assert_eq!(manager.state.last_self_tsumo.as_deref(), Some("N"));
    }
}
