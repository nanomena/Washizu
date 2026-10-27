//! Carrying a plan out: its sleeps, its waits for the client, its presses.

use super::decision::Decision;
use super::AutoplayManager;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::platform::{PlanResult, Step};
use crate::autoplay::pointer::{Approach, Touch};
use crate::autoplay::tenhou::input::{Effect, Facts, Owner, Run, Verdict};
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
        let mut tenhou = if decision.platform == crate::config::Platform::Tenhou {
            let Some(state) = decision.tenhou_state.as_ref() else {
                return false;
            };
            let Some(run) = Run::new(decision.request, state, action, decision.input_owner) else {
                warn!("autoplay: no original Tenhou input permit");
                return false;
            };
            Some(run)
        } else {
            None
        };
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
                    let Some(run) = tenhou.as_mut() else {
                        return false;
                    };
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
                    if !self.press_when_offered(page, selectors, label, run).await {
                        return false;
                    }
                }
                Step::Discard { tile_index } => {
                    let Some(run) = tenhou.as_mut() else {
                        return false;
                    };
                    let page_guard = self.ctx.page.read().await;
                    let Some(page) = page_guard.as_ref() else {
                        warn!("autoplay: no page handle — cannot discard");
                        return false;
                    };
                    if !self
                        .tenhou_effect_current(run, Effect::Discard(*tile_index))
                        .await
                    {
                        warn!("autoplay: withholding obsolete Tenhou discard");
                        return false;
                    }
                    match crate::autoplay::cdp_input::discard_tile(page, *tile_index).await {
                        Ok(true) => {
                            run.issued(Effect::Discard(*tile_index));
                            info!("autoplay: discarded tile index {tile_index}");
                        }
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
impl AutoplayManager {
    async fn tenhou_input_facts(&self) -> Facts {
        let cfg = self.cfg.read().await;
        let snapshot = self.monitor.snapshot();
        let state = self.ctx.tenhou_state.read().ok().and_then(|s| s.clone());
        Facts {
            context: (snapshot.sync_error.is_none()
                && !snapshot.round_ended
                && !snapshot.game_ended)
                .then_some(snapshot.context),
            state,
            owner: Owner {
                session: self.ctx.ui_session(),
                settings: self.ctx.ui_settings(),
                user: crate::autoplay::cdp_input::user_revision(),
            },
            enabled: cfg.autoplay.enabled && cfg.platform.kind == crate::config::Platform::Tenhou,
            manual: crate::autoplay::cdp_input::user_active(),
            latest: snapshot.events.back().map(|event| event.event.clone()),
        }
    }

    async fn tenhou_effect_current(&self, run: &Run, effect: Effect) -> bool {
        loop {
            match run.inspect(effect, &self.tenhou_input_facts().await) {
                Verdict::Allowed => return true,
                Verdict::Obsolete => return false,
                Verdict::AwaitingEcho if Instant::now() < run.wait_deadline() => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Verdict::AwaitingEcho => return false,
            }
        }
    }

    async fn press_when_offered(
        &self,
        page: &Page,
        selectors: &[String],
        label: &str,
        run: &mut Run,
    ) -> bool {
        const POLL_INTERVAL: Duration = Duration::from_millis(120);
        const WAIT_BUDGET: Duration = Duration::from_millis(2_400);

        let deadline = Instant::now() + WAIT_BUDGET;
        let mut attempts = 0u32;
        loop {
            if !self.tenhou_effect_current(run, Effect::Button).await {
                warn!("autoplay: withholding obsolete Tenhou {label}");
                return false;
            }
            attempts += 1;
            match crate::autoplay::cdp_input::click_dom(page, selectors).await {
                Ok(true) => {
                    run.issued(Effect::Button);
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
}
