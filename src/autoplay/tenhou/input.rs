//! One selected plan's input lifetime. This never selects or retries a policy.
//! The executor checks ready/offers and atomically compares the bound native
//! window/hand at dispatch; this module owns only the plan's state permits.
use crate::autoplay::tenhou_state::{DecisionWindow, TenhouState};
use crate::bot::control::{DecisionRequest, InputWindow};
use crate::monitor::DecisionContext;
use crate::schema::MjaiEvent;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    Button,
    Discard(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Selected,
    RiichiDeclared,
    RiichiEchoed(DecisionWindow),
    Finished,
}

struct NativeBinding {
    window: String,
    hand: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allowed,
    AwaitingEcho,
    Obsolete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Owner {
    pub session: u64,
    pub settings: u64,
    pub user: u64,
}

pub(crate) struct Facts {
    pub context: Option<DecisionContext>,
    pub owner: Owner,
    pub enabled: bool,
    pub manual: bool,
    pub state: Option<TenhouState>,
    pub latest: Option<MjaiEvent>,
}

pub(crate) struct Run {
    request: DecisionRequest,
    window: DecisionWindow,
    owner: Owner,
    seat: u8,
    hand: Vec<u32>,
    discard: Option<u32>,
    action: MjaiEvent,
    stage: Stage,
    native: Option<NativeBinding>,
    retired_native_window: Option<String>,
    native_rebind_allowed: bool,
}

impl Run {
    pub fn new(
        request: DecisionRequest,
        state: &TenhouState,
        action: &MjaiEvent,
        owner: Owner,
    ) -> Option<Self> {
        let Some(InputWindow::Tenhou(window)) = request.window else {
            return None;
        };
        window.clock?;
        if state.window != Some(window) {
            return None;
        }
        let hand = physical_hand(&state.hand)?;
        let discard = match action {
            MjaiEvent::Dahai { pai, tsumogiri, .. }
            | MjaiEvent::Reach {
                pai: Some(pai),
                tsumogiri: Some(tsumogiri),
                ..
            } => Some(crate::bridge::tenhou::encode::dahai_index(
                state.hand_view(),
                pai,
                *tsumogiri,
            )?),
            _ => None,
        };
        Some(Self {
            request,
            window,
            owner,
            seat: state.seat,
            hand,
            discard,
            action: action.clone(),
            stage: Stage::Selected,
            native: None,
            retired_native_window: None,
            native_rebind_allowed: false,
        })
    }

    /// Bind the first ready native input snapshot, or verify the existing
    /// binding. A changed window/physical hand is never adopted during think
    /// time. The caller aborts the plan on false; this method does not retry.
    ///
    /// A second binding requires inspect(Discard) to have just allowed the
    /// real own REACH echo, and must name a different native window.
    pub fn bind_native(&mut self, window: &str, hand: &[u32]) -> bool {
        let allowed = match self.stage {
            Stage::Selected => true,
            Stage::RiichiEchoed(_) => {
                self.native_rebind_allowed && self.retired_native_window.is_some()
            }
            _ => false,
        };
        let Some(hand) = physical_hand(hand) else {
            return false;
        };
        if !allowed || window.trim().is_empty() || hand != self.hand {
            return false;
        }
        if let Some(bound) = &self.native {
            return bound.window == window && bound.hand == hand;
        }
        if self.retired_native_window.as_deref() == Some(window) {
            return false;
        }
        self.native = Some(NativeBinding {
            window: window.to_owned(),
            hand,
        });
        true
    }

    fn native_binding(&self) -> Option<&NativeBinding> {
        if self.stage == Stage::Finished
            || (matches!(self.stage, Stage::RiichiEchoed(_)) && !self.native_rebind_allowed)
        {
            return None;
        }
        self.native.as_ref()
    }

    pub fn native_window(&self) -> Option<&str> {
        self.native_binding().map(|bound| bound.window.as_str())
    }

    /// Sorted physical IDs, including multiplicities and red-five identity.
    pub fn native_hand(&self) -> Option<&[u32]> {
        self.native_binding().map(|bound| bound.hand.as_slice())
    }

    pub fn wait_deadline(&self) -> Instant {
        let clock = self
            .window
            .clock
            .expect("known clock admitted at construction");
        self.window.opened_at + Duration::from_millis(u64::from(clock.fixed_ms + clock.bank_ms))
    }

    /// Record the one permitted own REACH echo when it is first validated.
    /// No newer context/window may replace that accepted continuation.
    pub fn inspect(&mut self, effect: Effect, facts: &Facts) -> Verdict {
        self.native_rebind_allowed = false;
        let Some(state) = &facts.state else {
            return Verdict::Obsolete;
        };
        if self.stage == Stage::Finished
            || !facts.enabled
            || facts.manual
            || facts.owner != self.owner
            || state.seat != self.seat
            || physical_hand(&state.hand).as_ref() != Some(&self.hand)
        {
            return Verdict::Obsolete;
        }
        let Some(context) = facts.context else {
            return Verdict::Obsolete;
        };
        let intended = match (&self.action, effect, self.stage) {
            (MjaiEvent::Dahai { actor, .. }, Effect::Discard(tile), Stage::Selected) => {
                *actor == self.seat && self.discard == Some(tile)
            }
            (
                MjaiEvent::Reach {
                    actor,
                    pai: Some(_),
                    tsumogiri: Some(_),
                    ..
                },
                Effect::Button,
                Stage::Selected,
            ) => *actor == self.seat,
            (
                MjaiEvent::Reach {
                    actor,
                    pai: Some(_),
                    tsumogiri: Some(_),
                    ..
                },
                Effect::Discard(tile),
                Stage::RiichiDeclared | Stage::RiichiEchoed(_),
            ) => *actor == self.seat && self.discard == Some(tile),
            (MjaiEvent::None, Effect::Button, Stage::Selected) => {
                self.window.has_declinable_claim()
            }
            (
                MjaiEvent::Chi { actor, .. }
                | MjaiEvent::Pon { actor, .. }
                | MjaiEvent::Daiminkan { actor, .. }
                | MjaiEvent::Ankan { actor, .. }
                | MjaiEvent::Kakan { actor, .. }
                | MjaiEvent::Hora { actor, .. }
                | MjaiEvent::Kita { actor, .. },
                Effect::Button,
                Stage::Selected,
            ) => *actor == self.seat,
            (MjaiEvent::Ryukyoku { .. }, Effect::Button, Stage::Selected) => true,
            _ => false,
        };
        if !intended {
            return Verdict::Obsolete;
        }
        // A declaration we actually issued is the only legal continuation.
        // It is a real own REACH echo, never a new context sampled for an old reply.
        if matches!(self.stage, Stage::RiichiDeclared | Stage::RiichiEchoed(_)) {
            let Some(window) = state.window else {
                return Verdict::Obsolete;
            };
            let fresh_reach_window = window.ops == 0
                && window.opened_at > self.window.opened_at
                && window.clock == self.window.clock;
            if self.stage == Stage::RiichiDeclared && context == self.request.context {
                return if window == self.window || fresh_reach_window {
                    Verdict::AwaitingEcho
                } else {
                    Verdict::Obsolete
                };
            }
            if context.game == self.request.context.game
                && context.round == self.request.context.round
                && Some(context.event_seq) == self.request.context.event_seq.checked_add(1)
                && matches!(&facts.latest, Some(MjaiEvent::Reach { actor, .. }) if *actor == self.seat)
                && state.is_tsumo
                && state.in_riichi
                && fresh_reach_window
            {
                if matches!(self.stage, Stage::RiichiEchoed(accepted) if accepted != window) {
                    return Verdict::Obsolete;
                }
                self.stage = Stage::RiichiEchoed(window);
                self.native_rebind_allowed = true;
                return Verdict::Allowed;
            }
            return Verdict::Obsolete;
        }
        if context == self.request.context && state.window == Some(self.window) {
            return Verdict::Allowed;
        }
        Verdict::Obsolete
    }

    /// Called only after the client's handler confirms dispatch. An unknown
    /// CDP outcome aborts the plan, and is never retried through this method.
    pub fn issued(&mut self, effect: Effect) {
        self.native_rebind_allowed = false;
        self.stage = if self.stage == Stage::Selected
            && effect == Effect::Button
            && matches!(self.action, MjaiEvent::Reach { .. })
        {
            self.retired_native_window = self.native.take().map(|bound| bound.window);
            Stage::RiichiDeclared
        } else {
            self.native = None;
            self.retired_native_window = None;
            Stage::Finished
        };
    }
}

fn physical_hand(hand: &[u32]) -> Option<Vec<u32>> {
    if hand.iter().any(|tile| *tile >= 136) {
        return None;
    }
    let mut sorted = hand.to_vec();
    sorted.sort_unstable();
    Some(sorted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::tenhou_state::TurnClock;

    fn fixture(action: MjaiEvent) -> (Run, Facts) {
        let window = DecisionWindow {
            ops: 32,
            opened_at: Instant::now(),
            clock: Some(TurnClock::NORMAL),
        };
        let state = TenhouState {
            seat: 0,
            go_type: None,
            lobby: None,
            hand: vec![0, 4, 8, 16, 17, 44, 72, 76, 80, 108, 112, 116, 120, 124],
            is_tsumo: true,
            window: Some(window),
            ..Default::default()
        };
        let context = DecisionContext {
            game: 1,
            round: 2,
            event_seq: 30,
        };
        let request = DecisionRequest {
            context,
            window: Some(InputWindow::Tenhou(window)),
        };
        let owner = Owner {
            session: 7,
            settings: 9,
            user: 11,
        };
        let run = Run::new(request, &state, &action, owner).unwrap();
        (
            run,
            Facts {
                context: Some(context),
                state: Some(state),
                latest: None,
                owner,
                enabled: true,
                manual: false,
            },
        )
    }

    #[test]
    fn only_the_selected_physical_copy_is_permitted_once() {
        let (mut run, facts) = fixture(MjaiEvent::Dahai {
            actor: 0,
            pai: "5m".into(),
            tsumogiri: false,
        });
        assert_eq!(
            run.inspect(Effect::Discard(16), &facts),
            Verdict::Obsolete,
            "red copy is a different action"
        );
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        run.issued(Effect::Discard(17));
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
    }

    #[test]
    fn authoritative_changes_cancel_before_the_handler() {
        for change in 0..13 {
            let (mut run, mut facts) = fixture(MjaiEvent::Dahai {
                actor: 0,
                pai: "P".into(),
                tsumogiri: true,
            });
            match change {
                0 => facts.enabled = false,
                1 => facts.manual = true,
                2 => facts.owner.session += 1,
                3 => facts.owner.settings += 1,
                4 => facts.context.as_mut().unwrap().event_seq += 1,
                5 => facts.state.as_mut().unwrap().window = None,
                6 => facts
                    .state
                    .as_mut()
                    .unwrap()
                    .hand
                    .pop()
                    .map(|_| ())
                    .unwrap(),
                7 => facts.context = None,  // real tracker gap
                8 => facts.owner.user += 1, // user's hand has already come to rest
                9 => facts.context.as_mut().unwrap().game += 1,
                10 => facts.context.as_mut().unwrap().round += 1,
                11 => facts.state.as_mut().unwrap().seat = 1,
                12 => facts.state.as_mut().unwrap().hand[4] = 16, // same length, wrong physical copy
                _ => unreachable!(),
            }
            assert_eq!(
                run.inspect(Effect::Discard(124), &facts),
                Verdict::Obsolete,
                "change {change}"
            );
        }
    }

    #[test]
    fn riichi_continues_only_after_its_own_issued_declaration() {
        let (mut run, mut facts) = fixture(MjaiEvent::Reach {
            actor: 0,
            pai: Some("5m".into()),
            tsumogiri: Some(false),
        });
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
        assert_eq!(run.inspect(Effect::Button, &facts), Verdict::Allowed);
        run.issued(Effect::Button);
        assert_eq!(run.inspect(Effect::Button, &facts), Verdict::Obsolete);
        assert_eq!(
            run.inspect(Effect::Discard(17), &facts),
            Verdict::AwaitingEcho,
            "the unchanged original context/window is not a REACH echo"
        );
        let original = facts.state.as_ref().unwrap().window.unwrap();
        facts.state.as_mut().unwrap().window = Some(DecisionWindow {
            ops: 0,
            opened_at: original.opened_at + Duration::from_nanos(1),
            ..original
        });
        assert_eq!(
            run.inspect(Effect::Discard(17), &facts),
            Verdict::AwaitingEcho,
            "bridge is ahead of tracker"
        );
        facts.context.as_mut().unwrap().event_seq += 1;
        facts.latest = Some(MjaiEvent::Reach {
            actor: 1,
            pai: None,
            tsumogiri: None,
        });
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
        facts.latest = Some(MjaiEvent::Reach {
            actor: 0,
            pai: None,
            tsumogiri: None,
        });
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
        facts.state.as_mut().unwrap().in_riichi = true;
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        run.issued(Effect::Discard(17));
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
    }

    fn own_reach_echo(facts: &mut Facts) {
        let state = facts.state.as_mut().unwrap();
        let original = state.window.unwrap();
        state.window = Some(DecisionWindow {
            ops: 0,
            opened_at: original.opened_at + Duration::from_nanos(1),
            ..original
        });
        state.in_riichi = true;
        facts.context.as_mut().unwrap().event_seq += 1;
        facts.latest = Some(MjaiEvent::Reach {
            actor: 0,
            pai: None,
            tsumogiri: None,
        });
    }

    fn declared_native_run() -> (Run, Facts) {
        let (mut run, facts) = fixture(MjaiEvent::Reach {
            actor: 0,
            pai: Some("5m".into()),
            tsumogiri: Some(false),
        });
        assert!(run.bind_native("before-reach", &facts.state.as_ref().unwrap().hand));
        assert_eq!(run.inspect(Effect::Button, &facts), Verdict::Allowed);
        run.issued(Effect::Button);
        (run, facts)
    }

    #[test]
    fn native_binding_is_stable_across_hand_order_and_consumed_on_dispatch() {
        let (mut run, mut facts) = fixture(MjaiEvent::Dahai {
            actor: 0,
            pai: "5m".into(),
            tsumogiri: false,
        });
        let hand = &mut facts.state.as_mut().unwrap().hand;
        hand.reverse();
        assert!(run.bind_native("native-1", hand));
        assert_eq!(run.native_window(), Some("native-1"));
        let mut sorted = hand.clone();
        sorted.sort_unstable();
        assert_eq!(run.native_hand(), Some(sorted.as_slice()));
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        assert!(run.bind_native("native-1", &facts.state.as_ref().unwrap().hand));
        run.issued(Effect::Discard(17));
        assert!(run.native_window().is_none() && run.native_hand().is_none());
        assert!(!run.bind_native("native-2", &sorted));
    }

    #[test]
    fn native_binding_never_adopts_a_changed_window_or_incomplete_physical_hand() {
        for change in 0..6 {
            let (mut run, facts) = fixture(MjaiEvent::Dahai {
                actor: 0,
                pai: "5m".into(),
                tsumogiri: false,
            });
            let original = facts.state.as_ref().unwrap().hand.clone();
            assert!(run.bind_native("native-1", &original));
            let mut hand = original.clone();
            let mut window = "native-1";
            match change {
                0 => window = "native-2",
                1 => {
                    hand.pop();
                }
                2 => hand.push(17),
                3 => hand[4] = 16, // preserve length, replace the ordinary five by a red copy
                4 => hand[4] = 136,
                5 => window = "",
                _ => unreachable!(),
            }
            assert!(!run.bind_native(window, &hand), "change {change}");
            assert_eq!(run.native_window(), Some("native-1"));
            assert_eq!(run.native_hand(), Some(original.as_slice()));
        }
    }

    #[test]
    fn riichi_native_rebind_needs_the_verified_echo_and_a_new_native_window() {
        let (mut run, mut facts) = declared_native_run();
        let hand = facts.state.as_ref().unwrap().hand.clone();
        assert!(run.native_window().is_none() && run.native_hand().is_none());
        assert_eq!(
            run.inspect(Effect::Discard(17), &facts),
            Verdict::AwaitingEcho
        );
        assert!(!run.bind_native("after-reach", &hand));
        own_reach_echo(&mut facts);
        assert!(
            !run.bind_native("after-reach", &hand),
            "the real echo still needs inspection"
        );
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        assert!(
            !run.bind_native("before-reach", &hand),
            "the first window was consumed"
        );
        let mut reordered = hand.clone();
        reordered.rotate_left(3);
        assert!(run.bind_native("after-reach", &reordered));
        assert_eq!(run.native_window(), Some("after-reach"));
        assert_eq!(run.native_hand(), Some(hand.as_slice()));
        assert!(!run.bind_native("third-window", &hand));
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        run.issued(Effect::Discard(17));
        assert!(run.native_window().is_none() && run.native_hand().is_none());
        assert!(!run.bind_native("third-window", &hand));
    }

    #[test]
    fn invalid_echo_facts_do_not_authorize_native_rebinding() {
        for change in 0..11 {
            let (mut run, mut facts) = declared_native_run();
            let hand = facts.state.as_ref().unwrap().hand.clone();
            own_reach_echo(&mut facts);
            match change {
                0 => facts.context.as_mut().unwrap().game += 1,
                1 => facts.context.as_mut().unwrap().round += 1,
                2 => facts.context.as_mut().unwrap().event_seq += 1,
                3 => {
                    facts.latest = Some(MjaiEvent::Reach {
                        actor: 1,
                        pai: None,
                        tsumogiri: None,
                    })
                }
                4 => facts.latest = Some(MjaiEvent::ReachAccepted { actor: 0 }),
                5 => {
                    facts
                        .state
                        .as_mut()
                        .unwrap()
                        .window
                        .as_mut()
                        .unwrap()
                        .opened_at = run.window.opened_at
                }
                6 => facts.state.as_mut().unwrap().is_tsumo = false,
                7 => facts.state.as_mut().unwrap().in_riichi = false,
                8 => facts.state.as_mut().unwrap().window = None,
                9 => {
                    facts.state.as_mut().unwrap().hand.pop();
                }
                10 => facts.state.as_mut().unwrap().window.as_mut().unwrap().clock = None,
                _ => unreachable!(),
            }
            assert_eq!(
                run.inspect(Effect::Discard(17), &facts),
                Verdict::Obsolete,
                "change {change}"
            );
            assert!(!run.bind_native("after-reach", &hand), "change {change}");
        }
    }

    #[test]
    fn a_verified_echo_is_not_a_permit_to_adopt_another_bridge_window() {
        let (mut run, mut facts) = declared_native_run();
        let hand = facts.state.as_ref().unwrap().hand.clone();
        own_reach_echo(&mut facts);
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        assert!(run.bind_native("after-reach", &hand));
        facts
            .state
            .as_mut()
            .unwrap()
            .window
            .as_mut()
            .unwrap()
            .opened_at += Duration::from_nanos(1);
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Obsolete);
        assert!(!run.bind_native("different-window", &hand));
        assert!(run.native_window().is_none() && run.native_hand().is_none());
    }

    #[test]
    fn a_reach_echo_cannot_replace_the_missing_initial_native_binding() {
        let (mut run, mut facts) = fixture(MjaiEvent::Reach {
            actor: 0,
            pai: Some("5m".into()),
            tsumogiri: Some(false),
        });
        assert_eq!(run.inspect(Effect::Button, &facts), Verdict::Allowed);
        run.issued(Effect::Button);
        own_reach_echo(&mut facts);
        assert_eq!(run.inspect(Effect::Discard(17), &facts), Verdict::Allowed);
        assert!(!run.bind_native("after-reach", &facts.state.as_ref().unwrap().hand));
    }

    #[test]
    fn unknown_clock_never_gets_a_guessed_deadline() {
        let (run, mut facts) = fixture(MjaiEvent::Dahai {
            actor: 0,
            pai: "P".into(),
            tsumogiri: true,
        });
        let state = facts.state.as_mut().unwrap();
        state.window.as_mut().unwrap().clock = None;
        let request = DecisionRequest {
            window: state.window.map(InputWindow::Tenhou),
            ..run.request
        };
        assert!(Run::new(request, state, &run.action, run.owner).is_none());
    }
}
