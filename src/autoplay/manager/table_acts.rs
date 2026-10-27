//! The presses between decisions ([`crate::autoplay::acts`]): through a
//! hand's result once its confirm button shows, on the client's auto-win
//! toggle after a riichi or for a ready hand, and through the game's
//! own screens after its end to play again.
//!
//! Majsoul only, with autoplay on, each behind its own switch
//! (`monitor.quick_confirm`, `monitor.auto_win`, and `monitor.auto_continue`,
//! the level of autoplay that plays game after game). An act that is not
//! wanted when it comes due is dropped, and what would have followed it with
//! it: switching game after game off during a game leaves its end to the
//! player.

use super::AutoplayManager;
use crate::autoplay::acts::Act;
use crate::autoplay::cdp_input::user_active;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::humanize;
use crate::autoplay::majsoul::{coords, reach_of};
use crate::autoplay::pixels::{self, Glyph, Image, Sight};
use crate::autoplay::pointer::{aim, spots, Touch};
use crate::autoplay::tenpai;
use crate::config::Platform;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

impl AutoplayManager {
    /// The act due now, if it still is and is wanted.
    pub(super) async fn table_act(&mut self) {
        self.acts.matching_seen(self.ctx.input_watch.matching());
        let confirmed = self.ctx.input_watch.confirmed();
        let Some(act) = self.acts.next(Instant::now(), confirmed, &mut rand::rng()) else {
            return;
        };
        let (wanted, hover_ms, hold_ms) = {
            let cfg = self.cfg.read().await;
            let on = cfg.autoplay.enabled && cfg.platform.kind == Platform::Majsoul;
            let wanted = match act {
                Act::Look | Act::Burst(_) => on && cfg.monitor.quick_confirm,
                Act::AutoWinOn | Act::TenpaiLook | Act::TenpaiWinOn => on && cfg.monitor.auto_win,
                Act::EndLook | Act::EndPress(_) => on && cfg.monitor.auto_continue,
            };
            let majsoul = &cfg.autoplay.majsoul;
            (wanted, majsoul.hover_delay_ms, majsoul.click_hold_ms)
        };
        if !wanted {
            return;
        }
        // A read of the hand presses nothing.
        if act == Act::TenpaiLook {
            self.read_tenpai().await;
            return;
        }
        // A look moves nothing: it goes ahead whatever the user's hand does.
        if act == Act::Look {
            let sight = self.look_at("result", pixels::result_sight).await;
            self.acts.looked(
                sight,
                Instant::now(),
                self.ctx.input_watch.confirmed(),
                &mut rand::rng(),
            );
            return;
        }
        if act == Act::EndLook {
            let sight = self.look_at("game's end", pixels::end_sight).await;
            self.acts
                .end_looked(sight, Instant::now(), &mut rand::rng());
            return;
        }
        // The user's hand is on the mouse; and switching a toggle can wait
        // for our own decision to be made.
        let deciding = self.ctx.time_budget.read().ok().and_then(|g| *g).is_some();
        let toggle = matches!(act, Act::AutoWinOn | Act::TenpaiWinOn);
        if user_active() || (toggle && deciding) {
            self.acts.later(
                act,
                Instant::now(),
                Duration::from_millis(humanize::ACT_PUT_OFF_MS),
            );
            return;
        }
        // A rest or a look under way brakes where it is.
        self.hand.give_way().await;
        match act {
            Act::Burst(presses) => self.press_through(presses, hover_ms, hold_ms).await,
            Act::AutoWinOn => match self.riichi_small().await {
                Some(true) => {
                    self.switch_toggle(coords::AUTO_WIN, "auto-win", true, hover_ms, hold_ms)
                        .await;
                }
                Some(false) => {
                    info!("autoplay: our riichi is a mangan or more; auto-win stays off")
                }
                // Read again in a moment: the hand is in the middle of a draw.
                None => self.acts.later(
                    act,
                    Instant::now(),
                    Duration::from_millis(humanize::ACT_PUT_OFF_MS),
                ),
            },
            Act::TenpaiWinOn => {
                let lit = self
                    .switch_toggle(coords::AUTO_WIN, "auto-win", true, hover_ms, hold_ms)
                    .await;
                self.acts.tenpai_win_read(lit);
            }
            Act::EndPress(sight) => self.press_end(sight, hover_ms, hold_ms).await,
            Act::Look | Act::EndLook | Act::TenpaiLook => {}
        }
    }

    /// What the screen shows, read by `read`: of a hand's result, its
    /// confirm button, the yellow one of the game's own screens after its
    /// last result, or neither yet ([`pixels::result_sight`]); of the game's
    /// end, which of its own screens ([`pixels::end_sight`]).
    async fn look_at(&mut self, what: &str, read: fn(&Image, f64, CanvasRect) -> Sight) -> Sight {
        let Some(rect) = self.canvas.remeasure(&self.ctx).await else {
            return Sight::Unread;
        };
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref() else {
            return Sight::Unread;
        };
        let sight = match pixels::shot(page).await {
            Ok((image, scale)) => read(&image, scale, rect),
            Err(e) => {
                warn!("autoplay: reading the {what}'s screen failed: {e:#}");
                Sight::Unread
            }
        };
        match sight {
            Sight::Nothing => debug!("autoplay: a look at the {what}: no button yet"),
            sight => info!("autoplay: a look at the {what}: {sight:?}"),
        }
        sight
    }

    /// One press on the game's own screen showing `sight`: its yellow
    /// confirm, "play again" beside it, or the confirm of the question
    /// "play again" asks.
    async fn press_end(&mut self, sight: Sight, hover_ms: u32, hold_ms: u32) {
        let Some(target) = sight.end_target() else {
            self.acts.end_press_failed(Instant::now());
            return;
        };
        let mut pressed = false;
        if let Some(rect) = self.canvas.remeasure(&self.ctx).await {
            let at = aim::again(target, reach_of(target), &mut rand::rng());
            let touch = Touch {
                hover_ms,
                hold_ms,
                jiggle: false,
            };
            let watch = self.ctx.input_watch.clone();
            let matching = self.acts.matching_ticket();
            let cfg = self.cfg.clone();
            pressed = self.hand.press_when(
                "game's end", rect.pixel(at.0, at.1), rect, touch,
                move |page| async move {
                    let measured = crate::autoplay::cdp_input::evaluate_canvas_rect(&page).await.ok();
                    let observed = match pixels::shot(&page).await {
                        Ok((image, scale)) => measured.map(|r| pixels::end_sight(&image, scale, r)),
                        Err(_) => None,
                    };
                    // Resizing during the screenshot invalidates both its
                    // scale and the point reached before it was taken.
                    let after_shot = crate::autoplay::cdp_input::evaluate_canvas_rect(&page).await.ok();
                    let cfg = cfg.read().await;
                    let allowed = !user_active() && cfg.autoplay.enabled && cfg.monitor.auto_continue
                        && cfg.platform.kind == Platform::Majsoul && Some(watch.matching()) == matching
                        && measured == after_shot
                        && end_click_allowed(sight, observed, rect, after_shot);
                    if !allowed {
                        info!("autoplay: withheld ending press {sight:?}; current {observed:?}, canvas before {measured:?}, after {after_shot:?}, planned {rect:?}");
                    }
                    allowed
                },
            ).await;
        }
        if pressed {
            info!("autoplay: pressed recognized ending control {sight:?}; waiting for transition");
            self.acts
                .end_pressed(sight, Instant::now(), &mut rand::rng());
        } else {
            self.canvas.forget(&self.ctx).await;
            self.acts.end_press_failed(Instant::now());
        }
    }

    /// One burst of `presses` at the result's confirm button: the first
    /// press as any press between decisions, the rest quick and on the spot.
    /// A burst once begun is pressed to its end, as a hand clicking fast does
    /// not stop the moment the screen changes: the confirm button's corner is
    /// empty table in the next hand. Only the user's hand back on the mouse
    /// stops it; whether another burst follows is asked after it.
    async fn press_through(&mut self, presses: u32, hover_ms: u32, hold_ms: u32) {
        let watch = self.ctx.input_watch.clone();
        if let Some(rect) = self.canvas.resolve(&self.ctx).await {
            let reach = reach_of(coords::CONFIRM);
            let (cx, cy) = coords::CONFIRM;
            let nudged = |at: (f64, f64), x: humanize::Spread, y: humanize::Spread| {
                spots::nudge(at, coords::CONFIRM, reach, x, y, &mut rand::rng())
            };
            // A hand still on the button from the burst before stays on its
            // spot, a little moved; one coming from elsewhere lands anywhere
            // in it.
            let on_button =
                self.hand.at().map(|(x, y)| rect.norm(x, y)).filter(|p| {
                    (p.0 - cx).abs() <= reach.half_x && (p.1 - cy).abs() <= reach.half_y
                });
            let mut at = match on_button {
                Some(p) => nudged(p, humanize::STAY_DRIFT_X, humanize::STAY_DRIFT_Y),
                None => aim::again(coords::CONFIRM, reach, &mut rand::rng()),
            };
            for press in 0..presses {
                if press > 0 {
                    let gap = humanize::BURST_GAP.draw_ms(&mut rand::rng());
                    tokio::time::sleep(Duration::from_millis(u64::from(gap))).await;
                }
                let gone = press == 0 && !self.acts.result_up(watch.confirmed());
                if gone || user_active() {
                    break;
                }
                let touch = if press == 0 {
                    Touch {
                        hover_ms,
                        hold_ms,
                        jiggle: false,
                    }
                } else {
                    // A hand clicking fast barely moves, and does not hover.
                    at = nudged(at, humanize::TAP_DRIFT_X, humanize::TAP_DRIFT_Y);
                    Touch {
                        hover_ms: 0,
                        hold_ms: humanize::TAP_HOLD_MS,
                        jiggle: false,
                    }
                };
                let pressed = self
                    .hand
                    .press("result", rect.pixel(at.0, at.1), rect, touch)
                    .await;
                if !pressed {
                    break;
                }
            }
            debug!("autoplay: pressed through a result, a burst of {presses}");
        }
        self.acts
            .burst_done(Instant::now(), watch.confirmed(), &mut rand::rng());
    }

    /// Read our hand at rest: whether it is ready and wants auto-win
    /// ([`tenpai::tenpai_win`]).
    async fn read_tenpai(&mut self) {
        let Some(seat) = self.state.cached_our_seat else {
            return;
        };
        let read = {
            let tracker = self.tracker.lock().await;
            tracker
                .snapshot()
                .and_then(|s| tenpai::tenpai_win(&s, seat))
        };
        let Some(win) = read else {
            debug!("autoplay: our hand was not at rest to be read");
            return;
        };
        if win {
            info!("autoplay: our hand is ready, below a mangan");
        }
        self.acts.tenpai_read(win, Instant::now(), &mut rand::rng());
    }

    /// Whether our hand in riichi is below a mangan ([`tenpai::riichi_small`]);
    /// `None` while it cannot be read.
    async fn riichi_small(&self) -> Option<bool> {
        let seat = self.state.cached_our_seat?;
        let tracker = self.tracker.lock().await;
        tracker
            .snapshot()
            .and_then(|s| tenpai::riichi_small(&s, seat))
    }

    /// Switch the client's toggle at `at` (`name` in the log), left of the
    /// table, on or off, reading it first and after. One that already shows
    /// as wanted is left alone, and one that cannot be seen (its strip folded
    /// away, or covered) is not pressed. Returns whether it is lit at the
    /// end, `None` when it could not be seen.
    async fn switch_toggle(
        &mut self,
        at: (f64, f64),
        name: &str,
        on: bool,
        hover_ms: u32,
        hold_ms: u32,
    ) -> Option<bool> {
        let state = if on { "on" } else { "off" };
        let rect = self.canvas.resolve(&self.ctx).await?;
        match self.read_toggle(rect, at).await {
            Glyph::Absent => {
                debug!("autoplay: the {name} toggle is not in sight");
                return None;
            }
            glyph if (glyph == Glyph::Lit) == on => {
                debug!("autoplay: the client's {name} is {state} already");
                return Some(on);
            }
            _ => {}
        }
        let aim_at = aim::again(at, reach_of(at), &mut rand::rng());
        let touch = Touch {
            hover_ms,
            hold_ms,
            jiggle: false,
        };
        let pressed = self
            .hand
            .press(name, rect.pixel(aim_at.0, aim_at.1), rect, touch)
            .await;
        tokio::time::sleep(Duration::from_millis(humanize::TOGGLE_REDRAW_MS)).await;
        let glyph = self.read_toggle(rect, at).await;
        if pressed && (glyph == Glyph::Lit) == on && glyph != Glyph::Absent {
            info!("autoplay: switched the client's {name} {state}");
        } else {
            warn!(
                "autoplay: switching the client's {name} {state} did not take; it reads {glyph:?}"
            );
        }
        self.hand.rest(rect);
        match glyph {
            Glyph::Absent => None,
            glyph => Some(glyph == Glyph::Lit),
        }
    }

    /// What the toggle whose glyph is at `at` (a normalised point) shows.
    async fn read_toggle(&self, rect: CanvasRect, at: (f64, f64)) -> Glyph {
        let half = coords::TOGGLE_HALF;
        let (x0, y0) = rect.pixel(at.0 - half, at.1 - half);
        let (x1, y1) = rect.pixel(at.0 + half, at.1 + half);
        let page_guard = self.ctx.page.read().await;
        let Some(page) = page_guard.as_ref() else {
            return Glyph::Absent;
        };
        pixels::glyph(page, x0, y0, x1 - x0, y1 - y0)
            .await
            .unwrap_or_else(|e| {
                warn!("autoplay: reading a toggle failed: {e:#}");
                Glyph::Absent
            })
    }
}

/// A click cannot inherit an old frame's identity or viewport geometry.
fn end_click_allowed(
    expected: Sight,
    observed: Option<Sight>,
    planned: CanvasRect,
    measured: Option<CanvasRect>,
) -> bool {
    expected.end_target().is_some() && observed == Some(expected) && measured == Some(planned)
}

#[cfg(test)]
mod ending_tests {
    use super::*;

    #[test]
    fn click_plan_requires_the_same_recognized_control_and_current_geometry() {
        let rect = CanvasRect {
            x: 14.0,
            y: 34.0,
            width: 1280.0,
            height: 720.0,
        };
        for expected in [
            Sight::Continue,
            Sight::Reward,
            Sight::Rematch,
            Sight::RematchAsk,
        ] {
            assert!(end_click_allowed(
                expected,
                Some(expected),
                rect,
                Some(rect)
            ));
            for absent in [None, Some(Sight::Nothing), Some(Sight::Unread)] {
                assert!(!end_click_allowed(expected, absent, rect, Some(rect)));
            }
            for other in [
                Sight::Continue,
                Sight::Reward,
                Sight::Rematch,
                Sight::RematchAsk,
            ] {
                assert_eq!(
                    end_click_allowed(expected, Some(other), rect, Some(rect)),
                    expected == other
                );
            }
            assert!(!end_click_allowed(expected, Some(expected), rect, None));
            assert!(!end_click_allowed(
                expected,
                Some(expected),
                rect,
                Some(CanvasRect {
                    width: 960.0,
                    height: 540.0,
                    ..rect
                })
            ));
        }
        for unknown in [Sight::Nothing, Sight::Unread, Sight::Button] {
            assert!(!end_click_allowed(unknown, Some(unknown), rect, Some(rect)));
        }
        assert_eq!(Sight::Reward.end_target(), Sight::Continue.end_target());
        assert_ne!(Sight::Reward.end_target(), Sight::Rematch.end_target());
    }
}
