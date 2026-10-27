//! Sending a planned Riichi City action, off the plan loop.
//!
//! A mapped Riichi City frame uses the common pacing result in its own task instead
//! of inline: inline execution serialized every decision's sleeps, window
//! waits and verify pauses, so queued plans stacked — measured injections
//! drifting 2→17 s later as a session progressed. A task gates its send on
//! the identity of the decision window it was planned against, so parallel
//! tasks cannot send into a later window by mistake.

use crate::autoplay::inject::{InjectFrame, SharedInjectBus};

use crate::config::InputConfig;
use crate::schema::MjaiEvent;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Send the mapped frame using the wait already selected by the common policy.
pub fn spawn(
    frame: Vec<u8>,
    sleep_ms: u32,
    cfg: &InputConfig,
    action: &MjaiEvent,
    inject: SharedInjectBus,
) {
    let verify_input_ms = cfg.verify_input_ms;
    let retries = cfg.click_retries;
    let action = action.clone();
    let window_open_at_plan = inject.window_is_open();
    let window_at_plan = inject.window_opened_at();
    let plan_created = Instant::now();
    tokio::spawn(async move {
        execute(
            sleep_ms as u64,
            frame,
            action,
            inject,
            verify_input_ms,
            retries,
            window_open_at_plan,
            window_at_plan,
            plan_created,
        )
        .await;
    });
}

/// How long an action planned with no window open may wait for its window.
/// Own-turn actions trail the deal animation by seconds; claim offers
/// trail their discard broadcast by milliseconds, so a tight bound keeps
/// a timed-out claim from ever landing in the next window.
fn future_window_grace(action: &MjaiEvent) -> Duration {
    match action {
        MjaiEvent::Dahai { .. }
        | MjaiEvent::Reach { .. }
        | MjaiEvent::Ryukyoku { deltas: None, .. } => Duration::from_secs(15),
        _ => Duration::from_secs(3),
    }
}

/// Execute one Riichi City decision: sleep the (already clamped) think time,
/// wait for OUR decision window, hold the minimum visible think, send, and
/// verify the server's ack — retrying the send if no ack lands.
///
/// Parallel tasks are safe because each gates its send on the *identity*
/// of the window it was planned against (`window_opened_at`): the window
/// that was open when the plan was created, or — for own-turn actions —
/// the first window to open after planning. A task whose window resolved
/// while it slept aborts instead of sending into whatever window is open
/// now.
#[allow(clippy::too_many_arguments)]
async fn execute(
    sleep_ms: u64,
    frame: Vec<u8>,
    action: MjaiEvent,
    inject: SharedInjectBus,
    verify_input_ms: u32,
    retries: u32,
    window_open_at_plan: bool,
    window_at_plan: Option<Instant>,
    plan_created: Instant,
) {
    if !inject.in_game() {
        debug!("autoplay: dropping frame for {action:?} — no game in progress");
        return;
    }

    // Resolve our window identity.
    let identity = if window_open_at_plan {
        // The window open at planning time is ours; require it to still
        // be the one open.
        let Some(id) = window_at_plan else {
            return;
        };
        id
    } else {
        // Planned before any window opened: either an own-turn action
        // whose window trails the deal animation, or a claim whose offer
        // frame lost the race against the bot response (measured:
        // responses can be planned milliseconds before the bridge
        // processes the offer). Wait for the first window to open after
        // planning, bounded per action kind — acting before the window
        // opens is rejected (rsp code 1).
        let deadline = plan_created + future_window_grace(&action);
        loop {
            if inject.window_is_open() {
                if let Some(opened) = inject.window_opened_at() {
                    if opened >= plan_created {
                        break opened;
                    }
                }
            }
            if Instant::now() >= deadline {
                debug!("autoplay: {action:?} window never opened; dropping");
                return;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    };

    // Wait until our window is open.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !(inject.window_is_open() && inject.window_opened_at() == Some(identity)) {
        if Instant::now() >= deadline
            || (inject.window_is_open() && inject.window_opened_at() != Some(identity))
        {
            debug!("autoplay: dropping stale {action:?} — the window moved on");
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Hold the humanizer's think time, measured from the WINDOW OPENING —
    // the Lua contract: delay_ms is what the server observes between
    // offering the decision and receiving the action. Anchoring to the
    // window (not the triggering event) is what keeps the dealer's
    // opening discard human-paced: the deal animation precedes the window
    // and must not eat the think. Capped well under the ~15s window;
    // floored so the timer is always visibly up before the send.
    const MIN_VISIBLE_THINK_MS: u64 = 2_000;
    const THINK_CAP_MS: u64 = 10_000;
    let target_ms = sleep_ms.clamp(MIN_VISIBLE_THINK_MS, THINK_CAP_MS);
    let elapsed_ms = identity.elapsed().as_millis() as u64;
    if elapsed_ms < target_ms {
        tokio::time::sleep(Duration::from_millis(target_ms - elapsed_ms)).await;
    }
    // The window may have resolved during the think.
    if !(inject.window_is_open() && inject.window_opened_at() == Some(identity)) {
        debug!("autoplay: dropping stale {action:?} — the window moved on");
        return;
    }

    let ticket = inject.rsp_ticket();
    let mut acked = false;
    for attempt in 0..=retries {
        if !inject.send(InjectFrame {
            gameplay: true,
            bytes: frame.clone(),
        }) {
            warn!(
                "autoplay: no injection relay subscribed — is capture running? \
                   dropping frame for {action:?}"
            );
            return;
        }
        info!("autoplay: injected frame for {action:?} (attempt {attempt})");
        // The server's round trip is ~200ms; the floor keeps a short
        // verify_input_ms from spuriously retrying.
        let wait = u64::from(verify_input_ms.max(500));
        let deadline = Instant::now() + Duration::from_millis(wait);
        loop {
            if inject.rsp_since(ticket) {
                acked = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if acked {
            break;
        }
        warn!("autoplay: no rsp_game_action within {wait}ms for {action:?} (attempt {attempt})");
    }
    if acked {
        let code = inject.last_rsp_code();
        if code != 0 {
            warn!("autoplay: the server rejected {action:?} (rsp code {code})");
        } else if retries > 0 {
            debug!("autoplay: rsp ok for {action:?}");
        }
    } else {
        warn!("autoplay: {action:?} was never answered by the server — the action did not happen");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Own-turn actions wait out the deal animation (long grace); claim
    /// offers follow their discard within milliseconds, so their grace is
    /// tight — a timed-out claim must never land in the next window.
    #[test]
    fn future_window_grace_is_tight_for_claims() {
        let own = MjaiEvent::Dahai {
            actor: 0,
            pai: "1m".into(),
            tsumogiri: false,
        };
        assert_eq!(future_window_grace(&own), Duration::from_secs(15));
        let claim = MjaiEvent::Chi {
            actor: 0,
            target: 1,
            pai: "3p".into(),
            consumed: ["2p".into(), "4p".into()],
        };
        assert_eq!(future_window_grace(&claim), Duration::from_secs(3));
        assert_eq!(
            future_window_grace(&MjaiEvent::None),
            Duration::from_secs(3)
        );
        // Kyuushu rides our own first-turn draw, so it gets the own-turn
        // grace — not the claim-tight one.
        assert_eq!(
            future_window_grace(&MjaiEvent::Ryukyoku {
                deltas: None,
                tehais: None,
                scores: None,
                reason: None
            }),
            Duration::from_secs(15)
        );
    }
}
