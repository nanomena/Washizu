//! All Mahjong Soul mouse-downs use this machine-issued permission.

use super::AutoplayManager;
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::majsoul::coords;
use crate::autoplay::pointer::{Point, Touch};
use crate::autoplay::{cdp_input, pixels, ui};
use crate::config::{AppConfig, Platform};
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub(super) async fn environment(
    ctx: &AutoplayContext,
    cfg: &RwLock<AppConfig>,
    page_available: bool,
) -> ui::Environment {
    let cfg = cfg.read().await;
    ui::Environment {
        settings: ui::Settings {
            enabled: cfg.autoplay.enabled && cfg.platform.kind == Platform::Majsoul,
            quick_confirm: cfg.monitor.quick_confirm,
            auto_continue: cfg.monitor.auto_continue,
            auto_win: cfg.monitor.auto_win,
        },
        generation: ui::Generation {
            session: ctx.ui_session(),
            table: ctx.input_watch.ui_table(),
            manual: cdp_input::user_revision(),
            settings: ctx.ui_settings(),
        },
        manual: cdp_input::user_active(),
        page_available,
        window: ctx.time_budget.read().ok().and_then(|g| *g).map(Into::into),
        confirmed: ctx.input_watch.confirmed(),
        lobby: ctx.input_watch.lobby(),
        input: ctx.input_watch.ticket(),
    }
}

impl AutoplayManager {
    pub(super) fn ui_transition<T>(
        &self,
        event: &str,
        change: impl FnOnce(&mut ui::Machine) -> T,
    ) -> T {
        let mut ui = self.ui.lock().unwrap();
        let before = ui.phase();
        let result = change(&mut ui);
        let after = ui.phase();
        if before != after {
            info!(?before, ?after, event, waiting_reason = ?ui.waiting_reason(), "autoplay: UI phase changed");
        }
        result
    }
    pub(super) async fn sync_ui(&mut self) {
        let page_available = self.ctx.page.read().await.is_some();
        let env = environment(&self.ctx, &self.cfg, page_available).await;
        let (before, after, reason) = {
            let mut ui = self.ui.lock().unwrap();
            let before = ui.phase();
            ui.synchronize(env, Instant::now());
            (before, ui.phase(), ui.waiting_reason())
        };
        if before != after {
            info!(?before, ?after, waiting_reason = ?reason, "autoplay: UI phase changed");
            if !matches!(after, ui::Phase::Table(_)) {
                self.hand.cancel();
            }
        }
    }

    pub(super) async fn decision_press(
        &mut self,
        token: ui::DecisionToken,
        click: u32,
        to: Point,
        rect: CanvasRect,
        touch: Touch,
    ) -> bool {
        self.sync_ui().await;
        let ticket = self.ui.lock().unwrap().decision_click(token, click);
        match ticket {
            Some(ticket) => self.ui_press(ticket, to, rect, touch).await,
            None => false,
        }
    }

    pub(super) async fn ui_press(
        &mut self,
        ticket: ui::Ticket,
        to: Point,
        rect: CanvasRect,
        touch: Touch,
    ) -> bool {
        let ctx = self.ctx.clone();
        let cfg = self.cfg.clone();
        let machine = self.ui.clone();
        let pressed = self
            .hand
            .press_when("UI effect", to, rect, touch, move |page| async move {
                let measured = cdp_input::evaluate_canvas_rect(&page).await.ok();
                let captured = pixels::shot(&page).await.ok();
                let after = cdp_input::evaluate_canvas_rect(&page).await.ok();
                // Hand holds this page's read lease through dispatch; do not
                // reacquire it behind a queued capture writer.
                let env = environment(&ctx, &cfg, true).await;
                let frame = match (captured, measured) {
                    (Some((image, scale)), Some(measured)) => ui::Frame {
                        stable_geometry: measured == rect
                            && after == Some(rect)
                            && rect.contains(to.0, to.1),
                        sight: pixels::end_sight(&image, scale, measured),
                        table_visible: pixels::table_view(&image, scale, measured),
                        auto_win: pixels::toggle_in(&image, scale, measured, coords::AUTO_WIN),
                    },
                    _ => ui::Frame {
                        stable_geometry: false,
                        sight: pixels::Sight::Unread,
                        table_visible: false,
                        auto_win: pixels::Glyph::Absent,
                    },
                };
                let mut ui = machine.lock().unwrap();
                let before = ui.phase();
                ui.synchronize(env, Instant::now());
                if before != ui.phase() { info!(?before, after = ?ui.phase(), waiting_reason = ?ui.waiting_reason(), "autoplay: final input facts changed UI phase"); }
                let allowed = ui.consume(ticket, frame);
                if !allowed {
                    warn!(?ticket, ?frame, phase = ?ui.phase(), generation = ?env.generation,
                        settings = ?env.settings, manual = env.manual, "autoplay: withheld UI mouse-down");
                }
                allowed
            })
            .await;
        if !pressed {
            self.canvas.forget(&self.ctx).await;
        }
        pressed
    }
}
