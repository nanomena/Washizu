//! The presses between decisions ([`crate::autoplay::acts`]): through a
//! hand's result once its confirm button shows, on the client's auto-win
//! toggle after a riichi, and through the game's own screens after its end
//! to play again.
//!
//! Majsoul only, with autoplay on, each behind its own switch
//! (`monitor.quick_confirm`, `monitor.riichi_auto_win`, and
//! `monitor.auto_continue`, the level of autoplay that plays game after game).
//! An act that is not wanted when it comes due is dropped, and what would
//! have followed it with it: switching game after game off during a game
//! leaves its end to the player.

use super::AutoplayManager;
use crate::autoplay::acts::Act;
use crate::autoplay::cdp_input::user_active;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::humanize;
use crate::autoplay::majsoul::{coords, reach_of};
use crate::autoplay::pixels::{self, Glyph, Image, Sight};
use crate::autoplay::pointer::{aim, spots, Touch};
use crate::config::Platform;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

impl AutoplayManager {
    /// The act due now, if it still is and is wanted.
    pub(super) async fn table_act(&mut self) {
        let confirmed = self.ctx.input_watch.confirmed();
        let Some(act) = self.acts.next(Instant::now(), confirmed, &mut rand::rng()) else {
            return;
        };
        let (wanted, hover_ms, hold_ms) = {
            let cfg = self.cfg.read().await;
            let on = cfg.autoplay.enabled && cfg.platform.kind == Platform::Majsoul;
            let wanted = match act {
                Act::Look | Act::Burst(_) => on && cfg.monitor.quick_confirm,
                Act::AutoWinOn => on && cfg.monitor.riichi_auto_win,
                Act::EndLook | Act::EndPress(_) => on && cfg.monitor.auto_continue,
            };
            let majsoul = &cfg.autoplay.majsoul;
            (wanted, majsoul.hover_delay_ms, majsoul.click_hold_ms)
        };
        if !wanted {
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
        // The user's hand is on the mouse; and switching auto-win on can wait
        // for our own decision to be made.
        let deciding = self.ctx.time_budget.read().ok().and_then(|g| *g).is_some();
        if user_active() || (act == Act::AutoWinOn && deciding) {
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
            Act::AutoWinOn => self.switch_auto_win_on(hover_ms, hold_ms).await,
            Act::EndPress(sight) => self.press_end(sight, hover_ms, hold_ms).await,
            Act::Look | Act::EndLook => {}
        }
    }

    /// What the screen shows, read by `read`: of a hand's result, its
    /// confirm button, the yellow one of the game's own screens after its
    /// last result, or neither yet ([`pixels::result_sight`]); of the game's
    /// end, which of its own screens ([`pixels::end_sight`]).
    async fn look_at(&mut self, what: &str, read: fn(&Image, f64, CanvasRect) -> Sight) -> Sight {
        let Some(rect) = self.canvas.resolve(&self.ctx).await else {
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
    /// confirm, "play again" beside it, the rewards' band (pressed away),
    /// or the confirm of the question "play again" asks.
    async fn press_end(&mut self, sight: Sight, hover_ms: u32, hold_ms: u32) {
        let (target, what) = match sight {
            Sight::Rematch => (coords::REMATCH, "play again"),
            Sight::RematchAsk => (
                coords::REMATCH_ASK_CONFIRM,
                "the confirm of play again's question",
            ),
            Sight::Reward => (coords::REWARD_AWAY, "away the chest's rewards"),
            _ => (coords::GAME_CONFIRM, "the game's confirm"),
        };
        if let Some(rect) = self.canvas.resolve(&self.ctx).await {
            let at = aim::again(target, reach_of(target), &mut rand::rng());
            let touch = Touch {
                hover_ms,
                hold_ms,
                jiggle: false,
            };
            if self
                .hand
                .press("game's end", rect.pixel(at.0, at.1), rect, touch)
                .await
            {
                info!("autoplay: pressed {what}");
            }
        }
        self.acts
            .end_pressed(sight, Instant::now(), &mut rand::rng());
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

    /// Switch the client's auto-win on by its toggle, left of the table,
    /// reading it first and after. One already on is left alone, and one that
    /// cannot be seen (its strip folded away, or covered) is not pressed.
    async fn switch_auto_win_on(&mut self, hover_ms: u32, hold_ms: u32) {
        let Some(rect) = self.canvas.resolve(&self.ctx).await else {
            return;
        };
        match self.read_toggle(rect, coords::AUTO_WIN).await {
            Glyph::Unlit => {}
            Glyph::Lit => {
                debug!("autoplay: the client's auto-win is on already");
                return;
            }
            Glyph::Absent => {
                debug!("autoplay: the auto-win toggle is not in sight");
                return;
            }
        }
        let at = aim::again(
            coords::AUTO_WIN,
            reach_of(coords::AUTO_WIN),
            &mut rand::rng(),
        );
        let touch = Touch {
            hover_ms,
            hold_ms,
            jiggle: false,
        };
        let pressed = self
            .hand
            .press("auto-win", rect.pixel(at.0, at.1), rect, touch)
            .await;
        tokio::time::sleep(Duration::from_millis(humanize::TOGGLE_REDRAW_MS)).await;
        match self.read_toggle(rect, coords::AUTO_WIN).await {
            Glyph::Lit if pressed => info!("autoplay: switched the client's auto-win on"),
            glyph => warn!(
                "autoplay: switching the client's auto-win on did not take; it reads {glyph:?}"
            ),
        }
        self.hand.rest(rect);
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
