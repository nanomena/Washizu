//! Final machine-issued permission for pixel and Tenhou Web UI effects.

use super::AutoplayManager;
use crate::autoplay::context::{AutoplayContext, CanvasRect};
use crate::autoplay::majsoul::coords;
use crate::autoplay::pointer::{Point, Touch};
use crate::autoplay::tenhou::client::{self, Dispatch, TargetStatus};
use crate::autoplay::{cdp_input, pixels, ui};
use crate::config::{AppConfig, Platform};
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub(super) async fn environment(
    ctx: &AutoplayContext,
    cfg: &RwLock<AppConfig>,
    page_available: bool,
    monitor: &crate::monitor::MonitorHandle,
) -> ui::Environment {
    let cfg = cfg.read().await;
    let s = monitor.snapshot();
    ui::Environment {
        client: match cfg.platform.kind {
            Platform::Majsoul => ui::Client::Majsoul,
            Platform::Tenhou => ui::Client::TenhouWeb,
            _ => ui::Client::Unsupported,
        },
        context: (s.sync_error.is_none() && !s.round_ended && !s.game_ended).then_some(s.context),
        settings: ui::Settings {
            enabled: cfg.autoplay.enabled
                && matches!(cfg.platform.kind, Platform::Majsoul | Platform::Tenhou),
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
    pub(super) async fn web_ui_press(
        &mut self,
        ticket: ui::Ticket,
        control: &ui::tenhou_web::Control,
    ) -> Dispatch {
        let (kind, identity) = control.dispatch_key();
        let target = {
            let guard = self.ctx.page.read().await;
            let Some(page) = guard.as_ref() else {
                return Dispatch::Changed;
            };
            match client::probe_ui_target(page, kind, identity, None).await {
                Ok(probe) if probe.status == TargetStatus::Ready => probe.target,
                Ok(_) => None,
                Err(error) => {
                    warn!("autoplay: Web UI mouse target unavailable: {error:#}");
                    None
                }
            }
        };
        let Some(target) = target else {
            return Dispatch::Changed;
        };
        let point = (
            target.bounds.x + target.bounds.width / 2.0,
            target.bounds.y + target.bounds.height / 2.0,
        );
        let (hover_ms, hold_ms) = self.touch_times().await;
        let ctx = self.ctx.clone();
        let cfg = self.cfg.clone();
        let machine = self.ui.clone();
        let monitor = self.monitor.clone();
        let control = control.clone();
        let before = self.ui.lock().unwrap().phase();
        let pressed = self.hand.press_when("Web UI effect", point, target.viewport,
            Touch { hover_ms, hold_ms, jiggle: false }, move |page| async move {
                // PointerController holds this page's lease through mouse-up.
                // Both reads are observers; only the common mouse sends input.
                let observation = match client::observe_tenhou_ui(&page).await {
                    Ok(observation) => Some(observation),
                    Err(error) => { warn!("autoplay: Web UI final observation unavailable: {error:#}"); None }
                };
                let (kind, identity) = control.dispatch_key();
                let hit = client::probe_ui_target(&page, kind, identity, Some((&target.identity, point))).await;
                let valid = match hit {
                    Ok(probe) => probe.status == TargetStatus::Ready && probe.target.as_ref().is_some_and(|current| {
                        current.identity == target.identity && current.bounds == target.bounds && current.viewport == target.viewport
                    }),
                    Err(error) => { warn!("autoplay: Web UI final mouse target unavailable: {error:#}"); false }
                };
                let env = environment(&ctx, &cfg, true, &monitor).await;
                let mut ui = machine.lock().unwrap();
                ui.synchronize(env, Instant::now());
                let allowed = ui.consume_web(ticket, &control, observation.as_ref().filter(|_| valid));
                if !allowed {
                    warn!(?ticket, ?control, phase = ?ui.phase(), "autoplay: withheld Web UI mouse-down");
                }
                allowed
            }).await;
        let (after, reason) = {
            let machine = self.ui.lock().unwrap();
            (machine.phase(), machine.waiting_reason())
        };
        self.report_ui_transition(before, after, "Web final mouse permission", reason);
        // The effect slot records whether final permission was consumed. A
        // transport failure after permission is ambiguous and stops the FSM;
        // a refusal before mouse-down cannot be mistaken for an accepted click.
        if pressed {
            Dispatch::Sent
        } else {
            Dispatch::Unavailable
        }
    }

    pub(super) fn ui_transition<T>(
        &self,
        event: &str,
        change: impl FnOnce(&mut ui::Machine) -> T,
    ) -> T {
        let mut ui = self.ui.lock().unwrap();
        let before = ui.phase();
        let result = change(&mut ui);
        let after = ui.phase();
        let reason = ui.waiting_reason();
        drop(ui);
        self.report_ui_transition(before, after, event, reason);
        result
    }

    fn report_ui_transition(
        &self,
        before: ui::Phase,
        after: ui::Phase,
        event: &str,
        reason: Option<ui::WaitingReason>,
    ) {
        if before == after {
            return;
        }
        info!(?before, ?after, event, waiting_reason = ?reason, "autoplay: UI phase changed");
        if let ui::Phase::Web(ui::tenhou_web::Stage::Stopped(reason)) = after {
            let detail = match reason {
                ui::WaitingReason::NoKnownTable => {
                    "缺少上一场的真实大厅与规则信息，请手动选择并开始一场。"
                }
                ui::WaitingReason::TargetUnavailable => {
                    "当前大厅缺少唯一可用的同规则预约项，请检查房间、规则与预约资格。"
                }
                ui::WaitingReason::OwnershipChanged => {
                    "手动输入或设置变更已撤销本次连战流程，请手动确认下一场预约。"
                }
                ui::WaitingReason::ObservationUnavailable => {
                    "无法读取经过验证的 Web 客户端界面状态。"
                }
                ui::WaitingReason::DispatchUnavailable => "无法确认客户端操作是否已送达。",
                ui::WaitingReason::EndTimeout => "等待结果页或大厅转移超时。",
                ui::WaitingReason::PressLimit => "界面状态持续变化，已达到本阶段操作次数上限。",
                ui::WaitingReason::Lobby => "客户端已返回大厅，请手动确认下一场预约。",
                ui::WaitingReason::NoGame => "当前没有已确认的对局。",
                ui::WaitingReason::SessionChanged => "客户端会话已经改变，需要新的真实对局信息。",
                ui::WaitingReason::ClientReload => "客户端正在重新加载。",
            };
            let _ = self
                .notify
                .send(crate::schema::Notification::warn("天凤自动继续已停止").body(detail));
        }
    }

    pub(super) async fn sync_ui(&mut self) {
        let page_available = self.ctx.page.read().await.is_some();
        let env = environment(&self.ctx, &self.cfg, page_available, &self.monitor).await;
        let (before, after, reason) = {
            let mut ui = self.ui.lock().unwrap();
            let before = ui.phase();
            ui.synchronize(env, Instant::now());
            (before, ui.phase(), ui.waiting_reason())
        };
        if before != after {
            self.report_ui_transition(before, after, "synchronize", reason);
            if !matches!(after, ui::Phase::Table(_)) {
                self.hand.cancel();
            }
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
        let monitor = self.monitor.clone();
        let pressed = self
            .hand
            .press_when("UI effect", to, rect, touch, move |page| async move {
                let measured = cdp_input::evaluate_canvas_rect(&page).await.ok();
                let captured = pixels::shot(&page).await.ok();
                let after = cdp_input::evaluate_canvas_rect(&page).await.ok();
                // Hand holds this page's read lease through dispatch; do not
                // reacquire it behind a queued capture writer.
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
                let env = environment(&ctx, &cfg, true, &monitor).await;
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
