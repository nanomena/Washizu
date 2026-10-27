//! Carrying a plan out: its sleeps, its waits for the client, its presses.

use super::decision::Decision;
use super::AutoplayManager;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::platform::{PlanResult, Step};
use crate::autoplay::pointer::{Approach, Touch};
use crate::schema::MjaiEvent;
use chromiumoxide::page::Page;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

impl AutoplayManager {
    /// Run `plan`'s steps for `action`, walking `approach` to the first
    /// press. False when the plan was given up part way: a window that
    /// closed while it slept, a client that never offered what it needed,
    /// or no page to act on.
    pub(super) async fn execute(
        &mut self,
        plan: &PlanResult,
        mut approach: Option<Approach>,
        rect: Option<CanvasRect>,
        decision: &Decision,
        action: &MjaiEvent,
    ) -> bool {
        // Whether this plan declares riichi: the reach declaration and its
        // tile go out in one plan (the declaring discard is pre-resolved onto
        // `Reach.pai`), so the reach action alone identifies it.
        let declares_reach = matches!(action, MjaiEvent::Reach { .. });
        let mut clicked = false;
        let mut click_index = 0;
        for step in &plan.steps {
            match step {
                Step::Sleep { duration_ms } => {
                    // The pause before the first press is the think time, already
                    // drawn; one between presses is a hand's, drawn here: long
                    // enough to see what the press before brought up.
                    let ms = if clicked {
                        crate::autoplay::humanize::next_press_gap(*duration_ms, &mut rand::rng())
                    } else {
                        *duration_ms
                    };
                    tokio::time::sleep(Duration::from_millis(u64::from(ms))).await;
                }
                Step::AwaitReady { timeout_ms } => {
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot wait for the client");
                        return false;
                    };
                    if !await_turn_ready(page, *timeout_ms).await {
                        return false;
                    }
                }
                Step::DomClick { selectors, label } => {
                    if self.tenhou_window_moved(decision.planned_window) {
                        warn!(
                            "autoplay: decision window closed mid-delay — dropping stale {label}"
                        );
                        return false;
                    }
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot press {label}");
                        return false;
                    };
                    // The client only renders the buttons it is currently
                    // offering, so a selector that matches nothing means the
                    // decision resolved while we were thinking. Report it and
                    // stop rather than pressing something else.
                    // The buttons exist only between the end of the
                    // client's animation for the triggering frame and
                    // whatever resolves the window, and neither edge is
                    // observable from here — a single look loses the race
                    // either way. Wait for the element instead, bounded by
                    // what is left of the turn.
                    if !press_when_offered(page, selectors, label).await {
                        return false;
                    }
                }
                Step::Discard { tile_index } => {
                    // The client's discard handler applies locally whether or
                    // not it is our turn — its own UI only reaches it while
                    // one is — so a stale call desyncs the board, not just
                    // wastes a frame. The riichi plan is the exception: its
                    // own button press replaces the window (the server acks
                    // the declaration and the bridge re-opens it), and the
                    // tile it owes is still this plan's to throw.
                    if discard_needs_window_guard(action)
                        && self.tenhou_window_moved(decision.planned_window)
                    {
                        warn!(
                            "autoplay: decision window closed mid-delay — dropping stale discard"
                        );
                        return false;
                    }
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot discard");
                        return false;
                    };
                    match crate::autoplay::cdp_input::discard_tile(page, *tile_index).await {
                        Ok(true) => info!("autoplay: discarded tile index {tile_index}"),
                        Ok(false) => {
                            warn!(
                                "autoplay: the client script was not instrumented, so its \
                                 discard handler is unreachable; skipping"
                            );
                            return false;
                        }
                        Err(e) => {
                            warn!("autoplay: discard failed: {e:#}");
                            return false;
                        }
                    }
                }
                Step::Click { x_norm, y_norm } => {
                    let Some(_) = rect else {
                        warn!("autoplay: click step with no canvas rect; skipping");
                        continue;
                    };
                    // Moving over a table whose window has closed presses
                    // nothing, so the walk comes before the check below and
                    // the check stays right against the press.
                    if let Some(approach) = approach.take() {
                        if !self.hand.walk(&approach).await {
                            return false;
                        }
                    }
                    let Some(token) = decision.ui_token else {
                        return false;
                    };
                    let Some(rect) = self.canvas.remeasure(&self.ctx).await else {
                        return false;
                    };
                    let (px, py) = rect.pixel(*x_norm, *y_norm);
                    if !rect.contains(px, py) {
                        warn!(
                            "autoplay: click ({px},{py}) outside canvas rect {:?}; skipping",
                            rect
                        );
                        continue;
                    }
                    // The approach ended on the first press; a later one —
                    // the riichi button's tile, a call's candidate — is
                    // reached the same way by the press itself.
                    //
                    // A lost press usually costs nothing — the decision
                    // window closes and the client passes. A lost *riichi*
                    // press is different: the discard that follows it still
                    // goes out, so the hand throws its riichi tile with no
                    // riichi behind it. That press gets the sturdier shape
                    // from the first attempt rather than only on a retry.
                    let touch = Touch {
                        hover_ms: decision.cfg.hover_delay_ms,
                        hold_ms: decision.cfg.click_hold_ms,
                        jiggle: declares_reach,
                    };
                    if !self
                        .decision_press(token, click_index, (px, py), rect, touch)
                        .await
                    {
                        return false;
                    }
                    click_index += 1;
                    clicked = true;
                }
                // Riichi City SendFrames never reach this loop — they are
                // sent by their own task (`riichi_city::send`).
                Step::SendFrame(_) => unreachable!("riichi frames run as tasks"),
            }
        }
        true
    }

    /// Has the Tenhou decision window been replaced since we planned?
    ///
    /// The delay model sleeps for seconds, and a window can resolve in that
    /// time — a claim we declined, or simply the next player acting. Acting on
    /// the stale plan then answers a decision that is already over. The
    /// window's `opened_at` is its identity, so a slot no longer holding the
    /// same instant means what we planned for is gone.
    ///
    /// `None` planned means we never had a window to go stale (other
    /// platforms), so nothing is dropped.
    fn tenhou_window_moved(
        &self,
        planned: Option<crate::autoplay::tenhou_state::DecisionWindow>,
    ) -> bool {
        let Some(planned) = planned else {
            return false;
        };
        let current = self
            .ctx
            .tenhou_state
            .read()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.window));
        current.map(|w| w.opened_at) != Some(planned.opened_at)
    }
}

/// Does a `Step::Discard` for this action require the decision window it
/// was planned against to still be open?
///
/// Everything but a riichi does: the window's identity is how a discard that
/// out-waited its turn — the client timed out and threw for us, or a human
/// beat the bot to it — is told apart from one that is still owed. A riichi
/// plan cannot use that test, because passing its own declaration button is
/// what replaces the window (the server acks with `REACH step=1` and the
/// bridge re-opens it for the tile), so the move is expected rather than
/// evidence of staleness — and the tile must still go out or the client sits
/// on its clock and times the hand out.
fn discard_needs_window_guard(action: &MjaiEvent) -> bool {
    !matches!(action, MjaiEvent::Reach { .. })
}

/// Wait until the client is taking input, or give up.
///
/// The turn does not begin when the frame arrives. Tenhou's server sends
/// as fast as the seats answer — against instant opponents that means
/// three seats' actions in one burst — and the client then animates them
/// for seconds before drawing its buttons and starting its clock. Timing
/// anything from frame arrival times it from the wrong instant, which is
/// why fixed delays kept landing either side of the window.
///
/// The client raises its clock display and its highlight together, so the
/// highlight appearing is the readiness signal.
async fn await_turn_ready(page: &Page, timeout_ms: u32) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(120);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    let started = Instant::now();
    loop {
        match crate::autoplay::cdp_input::turn_clock_running(page).await {
            Ok(true) => {
                debug!(
                    "autoplay: client ready after {}ms",
                    started.elapsed().as_millis()
                );
                return true;
            }
            Ok(false) => {}
            Err(e) => {
                warn!("autoplay: readiness probe failed: {e:#}");
                return false;
            }
        }
        if Instant::now() >= deadline {
            warn!(
                "autoplay: client never started its clock within {timeout_ms}ms; \
                 skipping this decision"
            );
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Press `selector` as soon as the client offers it.
///
/// A button is only in the DOM between the end of the client's animation
/// for the frame that opened the window and whatever closes it. Neither
/// edge is visible from here and the gap moves with animation length, so
/// a single `querySelector` races it — the first live run pressed
/// successfully three times and missed four, all with the client offering
/// nothing at the instant we looked. Polling turns that race into a
/// bounded wait.
async fn press_when_offered(page: &Page, selectors: &[String], label: &str) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(120);
    const WAIT_BUDGET: Duration = Duration::from_millis(2_400);

    let deadline = Instant::now() + WAIT_BUDGET;
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        match crate::autoplay::cdp_input::click_dom(page, selectors).await {
            Ok(true) => {
                info!("autoplay: pressed {label} ({selectors:?}) after {attempts} look(s)");
                return true;
            }
            Ok(false) => {}
            Err(e) => {
                warn!("autoplay: pressing {label} failed: {e:#}");
                return false;
            }
        }
        if Instant::now() >= deadline {
            let offered = crate::autoplay::cdp_input::list_action_buttons(page).await;
            warn!(
                "autoplay: {label} button ({selectors:?}) never appeared in {:?}; \
                 client is offering slots {offered:?} (its own order: highest first)",
                WAIT_BUDGET
            );
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (stale discard into a live board): the client's discard
    /// handler applies locally even when the turn is over, so a discard must
    /// be dropped once the window it was planned against is gone. The riichi
    /// plan is exempt — its own button press is what replaces the window,
    /// and the tile is still owed.
    #[test]
    fn a_discard_is_guarded_by_its_window_except_for_riichi() {
        let dahai = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: false,
        };
        assert!(discard_needs_window_guard(&dahai));
        let reach = MjaiEvent::Reach {
            actor: 0,
            pai: Some("2m".into()),
            tsumogiri: None,
        };
        assert!(!discard_needs_window_guard(&reach));
    }

    /// The window's `opened_at` is its identity: a slot holding a different
    /// instant — or nothing — means the plan is stale. No planned window
    /// (other platforms) never reports movement.
    #[test]
    fn tenhou_window_moved_tracks_the_slot() {
        use crate::autoplay::tenhou_state::{DecisionWindow, TenhouState};
        let m = AutoplayManager::for_tests();
        let w1 = DecisionWindow {
            ops: 0,
            opened_at: Instant::now(),
        };
        let put = |window| {
            *m.ctx.tenhou_state.write().unwrap() = Some(TenhouState {
                seat: 0,
                hand: vec![0],
                melds: Vec::new(),
                is_tsumo: true,
                window,
            });
        };

        put(Some(w1));
        assert!(
            !m.tenhou_window_moved(Some(w1)),
            "same instant — still live"
        );
        assert!(
            !m.tenhou_window_moved(None),
            "nothing planned, nothing stale"
        );

        put(None);
        assert!(m.tenhou_window_moved(Some(w1)), "window resolved — stale");

        let w2 = DecisionWindow {
            ops: 8,
            opened_at: Instant::now(),
        };
        put(Some(w2));
        assert!(m.tenhou_window_moved(Some(w1)), "window replaced — stale");
    }
}
