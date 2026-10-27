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
use tracing::{info, warn};

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
        for (index, step) in plan.steps.iter().enumerate() {
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
                    let Some(run) = tenhou.as_mut() else {
                        return false;
                    };
                    let Some(effect) = plan.steps[index + 1..].iter().find_map(|next| match next {
                        Step::ClientChoice { .. } => Some(Effect::Button),
                        Step::Discard { tile_index } => Some(Effect::Discard(*tile_index)),
                        _ => None,
                    }) else {
                        return false;
                    };
                    let guard = self.ctx.page.read().await;
                    let Some(page) = guard.as_ref() else {
                        return false;
                    };
                    if !self
                        .await_tenhou_ready(page, run, effect, *timeout_ms)
                        .await
                    {
                        return false;
                    }
                }
                Step::ClientChoice { payload, label } => {
                    let Some(run) = tenhou.as_mut() else {
                        return false;
                    };
                    let guard = self.ctx.page.read().await;
                    let Some(page) = guard.as_ref() else {
                        return false;
                    };
                    if !self.tenhou_effect_current(run, Effect::Button).await {
                        return false;
                    }
                    let current = match crate::autoplay::tenhou::client::read_input(page).await {
                        Ok(current) => current,
                        Err(error) => {
                            warn!("autoplay: Web3 offer probe failed: {error:#}");
                            return false;
                        }
                    };
                    if !native_matches(run, &current) {
                        return false;
                    }
                    let Some(offer) = current.matching_offer(payload) else {
                        warn!("autoplay: Web3 has no exact native offer for {label}");
                        return false;
                    };
                    if !self.tenhou_effect_current(run, Effect::Button).await {
                        return false;
                    }
                    match crate::autoplay::tenhou::client::choose(
                        page,
                        current.window.as_deref().unwrap(),
                        &offer.identity,
                    )
                    .await
                    {
                        Ok(crate::autoplay::tenhou::client::Dispatch::Sent) => {
                            run.issued(Effect::Button);
                            clicked = true;
                            info!("autoplay: invoked native Web3 {label}");
                        }
                        result => {
                            warn!("autoplay: Web3 choice was not dispatched: {result:?}");
                            return false;
                        }
                    }
                }
                Step::Discard { tile_index } => {
                    let Some(run) = tenhou.as_mut() else {
                        return false;
                    };
                    let guard = self.ctx.page.read().await;
                    let Some(page) = guard.as_ref() else {
                        return false;
                    };
                    if !self
                        .tenhou_effect_current(run, Effect::Discard(*tile_index))
                        .await
                    {
                        return false;
                    }
                    let (Some(window), Some(hand)) = (run.native_window(), run.native_hand())
                    else {
                        return false;
                    };
                    match crate::autoplay::tenhou::client::discard(page, window, hand, *tile_index)
                        .await
                    {
                        Ok(crate::autoplay::tenhou::client::Dispatch::Sent) => {
                            run.issued(Effect::Discard(*tile_index));
                            clicked = true;
                            info!("autoplay: invoked native Web3 discard {tile_index}");
                        }
                        result => {
                            warn!("autoplay: Web3 discard was not dispatched: {result:?}");
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

    async fn tenhou_effect_current(&self, run: &mut Run, effect: Effect) -> bool {
        matches!(
            run.inspect(effect, &self.tenhou_input_facts().await),
            Verdict::Allowed
        )
    }

    async fn await_tenhou_ready(
        &self,
        page: &Page,
        run: &mut Run,
        effect: Effect,
        timeout_ms: u32,
    ) -> bool {
        let deadline = (Instant::now() + Duration::from_millis(u64::from(timeout_ms)))
            .min(run.wait_deadline());
        loop {
            match run.inspect(effect, &self.tenhou_input_facts().await) {
                Verdict::Obsolete => return false,
                Verdict::AwaitingEcho => {}
                Verdict::Allowed => match crate::autoplay::tenhou::client::read_input(page).await {
                    Ok(current) if current.ready => {
                        return current
                            .window
                            .as_deref()
                            .is_some_and(|window| run.bind_native(window, &current.hand));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!("autoplay: Web3 input unavailable: {error:#}");
                        return false;
                    }
                },
            }
            if Instant::now() >= deadline {
                warn!(
                    "autoplay: Web3 input did not become ready within the original decision permit"
                );
                return false;
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
    }
}

fn native_matches(run: &Run, current: &crate::autoplay::tenhou::client::InputSnapshot) -> bool {
    current.ready
        && current.window.as_deref() == run.native_window()
        && Some(current.hand.as_slice()) == run.native_hand()
}
