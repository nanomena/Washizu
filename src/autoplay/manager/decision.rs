//! Reading a decision: the bot's answer joined with everything known about
//! the table at that moment, owned so that the stages after it can borrow it.

use super::AutoplayManager;
use crate::autoplay::delay::{DecisionProbs, Reading};
use crate::autoplay::platform::ActionContext;
use crate::bot::control::{DecisionRequest, DecisionSource, WindowProof};
use crate::bot::BotResponse;
use crate::config::{DelayModelConfig, InputConfig, Platform};
use crate::game_state::snapshot::GameStateSnapshot;
use crate::schema::MjaiEvent;
use riichienv_core::action::Action;
use riichienv_core::state::legal_actions::GameStateLegalActions;
use riichienv_core::state_3p::legal_actions::GameState3PLegalActions;
use tracing::{info, warn};

/// The coordinator supplies provenance; UI accepts either available source
/// equally and only checks freshness. Missing/malformed identity is a failure,
/// never permission to rebind a late reply to the newest budget slot.
fn selected_window_current(
    resp: &BotResponse,
    current_context: Option<crate::monitor::DecisionContext>,
    current: Option<WindowProof>,
) -> bool {
    let Some(request) = resp.request else {
        return false;
    };
    resp.unavailable_reason().is_none()
        && current_context == Some(request.context)
        && request.window.is_some()
        && request.window.map(|w| w.proof()) == current
        && matches!(resp.control(), Ok(Some(control)) if control.source != DecisionSource::Unavailable
            && control.context == request.context && control.window == current)
}

/// One decision's reading of the table.
pub(super) struct Decision {
    pub(super) request: DecisionRequest,
    pub(super) owner: crate::autoplay::transaction::Owner,
    pub(super) cfg: InputConfig,
    pub(super) delay_cfg: DelayModelConfig,
    pub(super) platform: Platform,
    pub(super) probs: Option<DecisionProbs>,
    pub(super) our_seat: u8,
    pub(super) legal_actions: Vec<Action>,
    pub(super) snapshot: Option<GameStateSnapshot>,
    pub(super) num_players: u8,
    pub(super) reading: Reading,
    pub(super) pointer: Option<(f64, f64)>,
    pub(super) runner_up: Option<String>,
    pub(super) last_kawa_tile: Option<String>,
    last_self_tsumo: Option<String>,
    pub(super) self_riichi_accepted: bool,
}

impl Decision {
    pub(super) fn context<'a>(&'a self, action: &'a MjaiEvent) -> ActionContext<'a> {
        ActionContext {
            action,
            snapshot: self.snapshot.as_ref(),
            legal_actions: &self.legal_actions,
            our_seat: self.our_seat,
            last_kawa_tile: self.last_kawa_tile.as_deref(),
            last_self_tsumo: self.last_self_tsumo.as_deref(),
            self_riichi_accepted: self.self_riichi_accepted,
            num_players: self.num_players,
            pointer: self.pointer,
        }
    }
}

impl AutoplayManager {
    pub(super) fn current_context(&self) -> Option<crate::monitor::DecisionContext> {
        let s = self.monitor.snapshot();
        (s.sync_error.is_none() && !s.round_ended && !s.game_ended).then_some(s.context)
    }
    /// Read the table for `resp`. `None` when autoplay is off, or when the
    /// game has not started for our seat.
    pub(super) async fn read_decision(&mut self, resp: &BotResponse) -> Option<Decision> {
        if resp.unavailable_reason().is_some() {
            return None;
        }
        // Re-read config every time so `cfg.autoplay.enabled` can be
        // toggled at runtime via the Settings UI without restarting.
        let cfg_guard = self.cfg.read().await;
        let owner = super::turn::owner(&self.ctx);
        if !cfg_guard.autoplay.enabled {
            return None;
        }
        let request = resp.request?;
        let cfg = crate::autoplay::input_timing::InputTiming::for_clock(
            &cfg_guard.autoplay.input,
            request
                .window
                .and_then(crate::bot::control::InputWindow::fixed_ms),
        )
        .input;
        let delay_cfg = cfg_guard.autoplay.delay.clone();
        let platform = cfg_guard.platform.kind;
        drop(cfg_guard);

        if matches!(platform, Platform::Majsoul | Platform::Tenhou)
            && !selected_window_current(
                resp,
                self.current_context(),
                WindowProof::capture(&self.ctx, platform),
            )
        {
            warn!("autoplay: selected reply lacks the exact current coordinator window proof; withholding input");
            return None;
        }

        let probs = crate::autoplay::delay::probs::normalize_meta(resp.meta.as_ref());

        // Pull our seat + legal actions from the live engine state. This
        // bracket releases the tracker mutex before we sleep/click.
        let (our_seat, legal_actions, snapshot, num_players) = {
            let tracker = self.tracker.lock().await;
            // No seat: the game hasn't started or no perspective is tagged.
            let our_seat = tracker.our_seat()?;
            // Keep cached_our_seat up to date for handle_mjai_event.
            self.state.cached_our_seat = Some(our_seat);
            let snapshot = tracker.snapshot();
            let selected_without_full_trace = matches!(resp.control(), Ok(Some(control))
                if control.source == DecisionSource::NativeStandby
                || (control.source == DecisionSource::Primary && tracker.restored_live()));
            if snapshot.is_none() && !selected_without_full_trace {
                return None;
            }
            let num_players = tracker.num_players()?;
            let legal_actions: Vec<Action> = if num_players == 3 {
                tracker
                    .state_3p()
                    .map(|s| s._get_legal_actions_internal(our_seat))
                    .unwrap_or_default()
            } else {
                tracker
                    .state()
                    .map(|s| s._get_legal_actions_internal(our_seat))
                    .unwrap_or_default()
            };
            (our_seat, legal_actions, snapshot, num_players)
        };

        let mut reading = snapshot.as_ref().map_or_else(Reading::default, |snapshot| {
            self.reading(resp, snapshot, our_seat)
        });
        reading.pace = crate::autoplay::delay::probs::pace(resp.meta.as_ref());
        // One line a decision, for measuring a pace against afterwards.
        info!(action = ?resp.action, ?probs, ?reading, "autoplay: decision read");

        // Where the pointer is now: a discard of the tile it rests on presses
        // that copy, not its twin. The user's own mouse may have left it somewhere.
        self.hand.sync_with_user();
        let pointer = self.hand.at();
        let candidates = crate::autoplay::delay::probs::candidates(resp.meta.as_ref());
        let runner_up = match &resp.action {
            MjaiEvent::Dahai { pai, .. } => {
                crate::autoplay::pointer::runner_up(&candidates, pai).map(str::to_owned)
            }
            _ => None,
        };
        Some(Decision {
            request,
            owner,
            cfg,
            delay_cfg,
            platform,
            probs,
            our_seat,
            legal_actions,
            snapshot,
            num_players,
            reading,
            pointer,
            runner_up,
            last_kawa_tile: self.state.last_kawa_tile.clone(),
            last_self_tsumo: self.state.last_self_tsumo.clone(),
            self_riichi_accepted: self.state.self_riichi_accepted,
        })
    }

    /// What the table and a second opinion say about this decision (see
    /// `autoplay::table`, `autoplay::advisor`). Like the probabilities it
    /// feeds the delay model and cannot alter the chosen action.
    fn reading(
        &mut self,
        resp: &BotResponse,
        snapshot: &GameStateSnapshot,
        our_seat: u8,
    ) -> Reading {
        let (loose, runner_up_loose, safe) = match &resp.action {
            MjaiEvent::Dahai { pai, .. } => {
                let hand = snapshot
                    .players
                    .get(our_seat as usize)
                    .map_or(&[][..], |p| p.tehai.as_slice());
                let candidates = crate::autoplay::delay::probs::candidates(resp.meta.as_ref());
                let riichi_rivers: Vec<Vec<&str>> = snapshot
                    .players
                    .iter()
                    .filter(|p| p.seat != our_seat && p.riichi_declared)
                    .map(|p| p.river.iter().map(|d| d.tile.as_str()).collect())
                    .collect();
                (
                    Some(crate::autoplay::table::is_loose(pai, hand)),
                    crate::autoplay::pointer::runner_up(&candidates, pai)
                        .map(|other| crate::autoplay::table::is_loose(other, hand)),
                    crate::autoplay::table::is_safe(pai, &riichi_rivers),
                )
            }
            _ => (None, None, None),
        };
        Reading {
            loose,
            runner_up_loose,
            safe,
            human: self.advisor.view(&resp.action),
            pace: None,
        }
    }
}

#[cfg(test)]
mod control_tests {
    use super::*;
    use crate::autoplay::TimeBudget;
    use crate::bot::control::{DecisionControl, InputWindow, RunnerHealth};
    #[test]
    fn all_available_sources_keep_the_original_context_and_producer_window() {
        let budget = TimeBudget {
            fixed_ms: 5000,
            add_ms: 0,
            opened_at: std::time::Instant::now(),
            source: crate::autoplay::BudgetSource::DealTile,
        };
        let request = DecisionRequest {
            context: crate::monitor::DecisionContext {
                game: 2,
                round: 1,
                event_seq: 10,
            },
            window: Some(InputWindow::Majsoul(budget)),
        };
        let proof = request.window.map(|w| w.proof());
        for source in [
            DecisionSource::Primary,
            DecisionSource::NativeStandby,
            DecisionSource::Unavailable,
        ] {
            let mut reply = BotResponse {
                request: Some(request),
                action: MjaiEvent::None,
                meta: None,
            };
            reply.set_control(DecisionControl {
                context: request.context,
                window: proof,
                source,
                primary: RunnerHealth::DirectReady { delivered: 3 },
                standby: RunnerHealth::Ready { through: 3 },
            });
            assert_eq!(
                selected_window_current(&reply, Some(request.context), proof),
                source != DecisionSource::Unavailable
            );
            let changed = crate::monitor::DecisionContext {
                event_seq: 11,
                ..request.context
            };
            assert!(!selected_window_current(&reply, Some(changed), proof));
            assert!(!selected_window_current(&reply, None, proof));
            assert!(!selected_window_current(
                &reply,
                Some(request.context),
                None
            ));
            let next = InputWindow::Majsoul(TimeBudget {
                opened_at: budget.opened_at + std::time::Duration::from_nanos(1),
                ..budget
            });
            assert!(!selected_window_current(
                &reply,
                Some(request.context),
                Some(next.proof())
            ));
            // Wire metadata cannot manufacture the original in-process request.
            let wire = serde_json::to_vec(&reply).unwrap();
            let untrusted: BotResponse = serde_json::from_slice(&wire).unwrap();
            assert!(untrusted.request.is_none());
            assert!(!selected_window_current(
                &untrusted,
                Some(request.context),
                proof
            ));
        }
    }
}
