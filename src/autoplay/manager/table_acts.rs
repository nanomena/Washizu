//! Execute machine effects and hand-policy reads, then return completions.

use super::AutoplayManager;
use crate::autoplay::majsoul::{coords, reach_of};
use crate::autoplay::pixels::{Glyph, Sight};
use crate::autoplay::pointer::{aim, Touch};
use crate::autoplay::toggles::Act;
use crate::autoplay::{humanize, pixels, tenpai, ui};
use std::time::{Duration, Instant};
use tracing::{debug, info};

impl AutoplayManager {
    pub(super) async fn table_act(&mut self) {
        self.sync_ui().await;
        let effect = self.ui_transition("clock", |ui| ui.tick(Instant::now()));
        if let Some(effect) = effect {
            match effect {
                ui::Effect::ObserveWeb(ticket) => {
                    let observation = {
                        let guard = self.ctx.page.read().await;
                        match guard.as_ref() {
                            Some(page) => {
                                match crate::autoplay::cdp_input::observe_tenhou_ui(page).await {
                                    Ok(observation) => Some(observation),
                                    Err(error) => {
                                        tracing::warn!(
                                            "autoplay: Web UI observation unavailable: {error:#}"
                                        );
                                        None
                                    }
                                }
                            }
                            None => None,
                        }
                    };
                    self.sync_ui().await;
                    self.ui_transition("Web observation", |ui| {
                        ui.web_observed(ticket, observation, Instant::now(), &mut rand::rng())
                    });
                }
                ui::Effect::InvokeWeb { ticket, control } => {
                    let result = self.web_ui_invoke(ticket, &control).await;
                    self.sync_ui().await;
                    self.ui_transition("Web dispatch", |ui| {
                        ui.web_done(ticket, result, Instant::now())
                    });
                    info!(?control, ?result, "autoplay: Web UI effect completed");
                }
                ui::Effect::Observe(ticket) => {
                    let sight = self.look_at().await;
                    self.sync_ui().await;
                    self.ui_transition("pixels", |ui| {
                        ui.observed(ticket, sight, Instant::now(), &mut rand::rng())
                    });
                }
                ui::Effect::Press { ticket, sight } => {
                    let (hover_ms, hold_ms) = self.touch_times().await;
                    let target = match sight {
                        Sight::Button => Some(coords::CONFIRM),
                        sight => sight.end_target(),
                    };
                    let mut pressed = false;
                    if let (Some(target), Some(rect)) =
                        (target, self.canvas.remeasure(&self.ctx).await)
                    {
                        self.hand.give_way().await;
                        let at = aim::again(target, reach_of(target), &mut rand::rng());
                        pressed = self
                            .ui_press(
                                ticket,
                                rect.pixel(at.0, at.1),
                                rect,
                                Touch {
                                    hover_ms,
                                    hold_ms,
                                    jiggle: false,
                                },
                            )
                            .await;
                    }
                    if pressed {
                        info!(
                            ?sight,
                            "autoplay: dispatched observed UI control; awaiting transition"
                        );
                    }
                    self.sync_ui().await;
                    self.ui_transition("dispatch", |ui| {
                        ui.screen_done(ticket, pressed, Instant::now(), &mut rand::rng())
                    });
                }
            }
            return;
        }
        let Some(act) = self.toggles.next(Instant::now()) else {
            return;
        };
        if self.cfg.read().await.platform.kind != crate::config::Platform::Majsoul {
            return;
        }
        let phase = self.ui.lock().unwrap().phase();
        if !matches!(phase, ui::Phase::Table(_)) {
            return;
        }
        if !self.cfg.read().await.monitor.auto_win {
            return;
        }
        if act == Act::ReadTenpai {
            self.read_tenpai().await;
            return;
        }
        if phase != ui::Phase::Table(ui::TablePhase::Waiting) {
            self.toggles.later(
                act,
                Instant::now(),
                Duration::from_millis(humanize::ACT_PUT_OFF_MS),
            );
            return;
        }
        let (hover_ms, hold_ms) = self.touch_times().await;
        match act {
            Act::RiichiWin => match self.riichi_small().await {
                Some(true) => {
                    self.switch_auto_win(hover_ms, hold_ms).await;
                }
                Some(false) => {
                    info!("autoplay: our riichi is a mangan or more; auto-win stays off")
                }
                None => self.toggles.later(
                    act,
                    Instant::now(),
                    Duration::from_millis(humanize::ACT_PUT_OFF_MS),
                ),
            },
            Act::TenpaiWin => {
                let lit = self.switch_auto_win(hover_ms, hold_ms).await;
                self.toggles.win_read(lit);
            }
            Act::ReadTenpai => {}
        }
    }

    async fn touch_times(&self) -> (u32, u32) {
        let cfg = self.cfg.read().await;
        (
            cfg.autoplay.majsoul.hover_delay_ms,
            cfg.autoplay.majsoul.click_hold_ms,
        )
    }

    async fn look_at(&mut self) -> Sight {
        let Some(rect) = self.canvas.remeasure(&self.ctx).await else {
            return Sight::Unread;
        };
        let guard = self.ctx.page.read().await;
        let Some(page) = guard.as_ref() else {
            return Sight::Unread;
        };
        match pixels::shot(page).await {
            Ok((image, scale)) => pixels::end_sight(&image, scale, rect),
            Err(e) => {
                debug!("autoplay: observing UI failed: {e:#}");
                Sight::Unread
            }
        }
    }

    async fn read_tenpai(&mut self) {
        let Some(seat) = self.state.cached_our_seat else {
            return;
        };
        let read = self
            .tracker
            .lock()
            .await
            .snapshot()
            .and_then(|s| tenpai::tenpai_win(&s, seat));
        if let Some(win) = read {
            self.toggles
                .tenpai_read(win, Instant::now(), &mut rand::rng());
        }
    }

    async fn riichi_small(&self) -> Option<bool> {
        let seat = self.state.cached_our_seat?;
        self.tracker
            .lock()
            .await
            .snapshot()
            .and_then(|s| tenpai::riichi_small(&s, seat))
    }

    async fn switch_auto_win(&mut self, hover_ms: u32, hold_ms: u32) -> Option<bool> {
        let rect = self.canvas.remeasure(&self.ctx).await?;
        match self.read_toggle(rect).await {
            Glyph::Absent => return None,
            Glyph::Lit => return Some(true),
            Glyph::Unlit => {}
        }
        self.sync_ui().await;
        let ticket = self.ui.lock().unwrap().auto_win()?;
        let at = aim::again(
            coords::AUTO_WIN,
            reach_of(coords::AUTO_WIN),
            &mut rand::rng(),
        );
        let pressed = self
            .ui_press(
                ticket,
                rect.pixel(at.0, at.1),
                rect,
                Touch {
                    hover_ms,
                    hold_ms,
                    jiggle: false,
                },
            )
            .await;
        self.ui.lock().unwrap().toggle_done(ticket);
        if !pressed {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(humanize::TOGGLE_REDRAW_MS)).await;
        let glyph = self.read_toggle(rect).await;
        info!(?glyph, "autoplay: auto-win after dispatched toggle");
        self.hand.rest(rect);
        match glyph {
            Glyph::Absent => None,
            Glyph::Lit => Some(true),
            Glyph::Unlit => Some(false),
        }
    }

    async fn read_toggle(&self, rect: crate::autoplay::CanvasRect) -> Glyph {
        let guard = self.ctx.page.read().await;
        let Some(page) = guard.as_ref() else {
            return Glyph::Absent;
        };
        match pixels::shot(page).await {
            Ok((image, scale)) => pixels::toggle_in(&image, scale, rect, coords::AUTO_WIN),
            Err(_) => Glyph::Absent,
        }
    }
}
