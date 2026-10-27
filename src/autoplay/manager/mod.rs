//! Autoplay manager: subscribes to bot decisions and mjai events, and turns
//! each decision into presses in the client (or a frame, for Riichi City).
//!
//! Lifecycle: spawned by `crate::lib::run` when `cfg.autoplay.enabled =
//! true`; one long-lived `tokio::select!` loop over the bot's answers, the
//! mjai stream, the bot's leans and the pointer's own looks between turns.
//!
//! The manager joins session events to the shared TurnController, PacingPolicy,
//! PointerController and read-only SiteAdapter. It owns no second turn state.
//! The separate UI machine owns lobby/result/continuation effects only.

mod decision;
mod events;
mod table_acts;
mod turn;
mod ui_input;

use crate::autoplay::canvas::Canvas;
use crate::autoplay::context::AutoplayContext;
use crate::autoplay::majsoul::MajsoulAutoplay;
use crate::autoplay::platform::{ActionContext, SiteAdapter};
use crate::autoplay::pointer::{self, PointerController};
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
use tracing::{info, warn};

pub struct AutoplayManager {
    cfg: Arc<RwLock<AppConfig>>,
    ctx: Arc<AutoplayContext>,
    tracker: Arc<Mutex<GameTracker>>,
    monitor: crate::monitor::MonitorHandle,
    mjai_bus: MjaiBus,
    /// For telling the user about a decision that came out wrong — see the
    /// shared turn confirmation and recovery effects.
    notify: NotifyBus,
    /// One implementation per supported platform, selected per decision from
    /// the live config so a platform switch takes effect without a restart.
    /// Both browser clients use the same pointer and selected-action lifecycle.
    majsoul: Arc<dyn SiteAdapter>,
    tenhou: Arc<dyn SiteAdapter>,
    /// Riichi City: no page to click — plans a protocol frame the proxy
    /// transmits (see `autoplay::inject`).
    riichi: RiichiCityAutoplay,
    state: ManagerState,
    /// The pointer over the current hand.
    hand: PointerController,
    turn: Arc<std::sync::Mutex<crate::autoplay::transaction::TurnController>>,
    /// Non-playing page phases and effects.
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
            hand: PointerController::new(ctx.clone()),
            turn: Default::default(),
            majsoul: Arc::new(MajsoulAutoplay::new(ctx.clone())),
            tenhou: Arc::new(TenhouAutoplay::new(ctx.clone())),
            ui: Default::default(),
            toggles: Default::default(),
            cfg,
            ctx,
            tracker,
            monitor: crate::monitor::MonitorHandle::default(),
            mjai_bus,
            notify,
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
        let Some(decision) = self.read_decision(&resp).await else {
            return;
        };
        self.load_pacing_policy(&decision).await;
        if decision.platform == Platform::RiichiCity {
            if let Some(frame) = self.riichi.plan(&decision.context(&resp.action)) {
                if let Ok(pacing) = self.pacing(&decision, &resp.action, None, false, 0) {
                    crate::autoplay::riichi_city::send::spawn(
                        frame,
                        pacing.wait_ms,
                        &decision.cfg,
                        &resp.action,
                        self.ctx.inject.clone(),
                    );
                }
            }
            return;
        }
        let Some(adapter) = self.adapter(decision.platform) else {
            return;
        };
        let plan = adapter.plan(&decision.context(&resp.action));
        let Some(window) = decision
            .request
            .window
            .map(|window| turn::window_identity(window.proof()))
        else {
            return;
        };
        let binding = crate::autoplay::transaction::Binding {
            context: decision.request.context,
            owner: decision.owner,
            window,
            input: self.ctx.input_watch.ticket(),
        };
        let settings = crate::autoplay::transaction::Settings {
            retries: decision.cfg.click_retries,
            verify_ms: decision.cfg.verify_input_ms,
            gap_ms: decision.cfg.inter_click_delay_ms,
            ready_ms: 8_000,
            reload_after: decision.cfg.reload_after_failures,
        };
        if !self.turn.lock().unwrap().begin(
            binding,
            resp.action.clone(),
            plan.clone(),
            settings,
            Instant::now(),
        ) {
            return;
        }
        self.hand.give_way().await;
        self.execute_turn(adapter, Arc::new(decision), resp.action, plan)
            .await;
    }

    fn adapter(&self, platform: Platform) -> Option<Arc<dyn SiteAdapter>> {
        match platform {
            Platform::Majsoul => Some(self.majsoul.clone()),
            Platform::Tenhou => Some(self.tenhou.clone()),
            _ => None,
        }
    }

    /// One look between turns: at one of the tiles the bot expects to throw,
    /// or nowhere new (see `autoplay::pointer::lean`), using measured site targets. The
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
        if !wanted || self.state.self_riichi_accepted {
            self.hand.stop_glances();
            return;
        }
        let context = self.current_context();
        self.hand.retain_current_forecast(context);
        let Some(spot) = self.hand.next_glance() else {
            return;
        };
        if crate::autoplay::cdp_input::user_active() {
            return;
        }
        let Some(adapter) = self.adapter(platform) else {
            return;
        };
        let Some(seat) = self.state.cached_our_seat else {
            return;
        };
        let snapshot = { self.tracker.lock().await.snapshot() };
        let Some(snapshot) = snapshot else {
            return;
        };
        if snapshot
            .players
            .get(seat as usize)
            .is_none_or(|player| player.tehai.len() % 3 != 1)
        {
            return;
        }
        let page = { self.ctx.page.read().await.clone() };
        let Some(page) = page else {
            return;
        };
        let observation = ActionContext {
            action: &MjaiEvent::None,
            snapshot: &snapshot,
            legal_actions: &[],
            our_seat: seat,
            last_kawa_tile: self.state.last_kawa_tile.as_deref(),
            last_self_tsumo: self.state.last_self_tsumo.as_deref(),
            self_riichi_accepted: self.state.self_riichi_accepted,
            num_players: snapshot.num_players,
            pointer: self.hand.at(),
        };
        let layout = adapter.pointer_layout(&page, &observation).await;
        if !self.hand.forecast_current(self.current_context()) {
            return;
        }
        if let Ok(Some(layout)) = layout {
            self.hand.set_layout(layout);
            self.hand.look_at(spot);
        }
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
        let plan = manager.majsoul.plan(&decision.context(&reply.action));
        let binding = crate::autoplay::transaction::Binding {
            context: decision.request.context,
            owner: decision.owner,
            window: turn::window_identity(decision.request.window.unwrap().proof()),
            input: manager.ctx.input_watch.ticket(),
        };
        assert!(manager.turn.lock().unwrap().begin(
            binding,
            reply.action.clone(),
            plan,
            crate::autoplay::transaction::Settings {
                retries: 0,
                verify_ms: 100,
                gap_ms: 0,
                ready_ms: 8_000,
                reload_after: 0
            },
            Instant::now()
        ));
        assert_eq!(
            manager.turn.lock().unwrap().phase(),
            Some(crate::autoplay::transaction::Phase::Preparing)
        );
        assert_eq!(manager.state.last_self_tsumo.as_deref(), Some("N"));
    }
}
