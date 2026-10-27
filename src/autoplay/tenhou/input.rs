//! One selected plan's input lifetime. This never selects or retries a policy.
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
    Finished,
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
            hand: state.hand.clone(),
            discard,
            action: action.clone(),
            stage: Stage::Selected,
        })
    }

    pub fn wait_deadline(&self) -> Instant {
        let clock = self
            .window
            .clock
            .expect("known clock admitted at construction");
        self.window.opened_at + Duration::from_millis(u64::from(clock.fixed_ms + clock.bank_ms))
    }

    pub fn inspect(&self, effect: Effect, facts: &Facts) -> Verdict {
        let Some(state) = &facts.state else {
            return Verdict::Obsolete;
        };
        if self.stage == Stage::Finished
            || !facts.enabled
            || facts.manual
            || facts.owner != self.owner
            || state.seat != self.seat
            || state.hand != self.hand
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
                Stage::RiichiDeclared,
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
        if context == self.request.context && state.window == Some(self.window) {
            return Verdict::Allowed;
        }
        // A declaration we actually issued is the only legal continuation.
        // It is a real own REACH echo, never a new context sampled for an old reply.
        if self.stage == Stage::RiichiDeclared && matches!(effect, Effect::Discard(_)) {
            if context == self.request.context {
                return Verdict::AwaitingEcho;
            }
            if context.game == self.request.context.game
                && context.round == self.request.context.round
                && context.event_seq == self.request.context.event_seq + 1
                && matches!(&facts.latest, Some(MjaiEvent::Reach { actor, .. }) if *actor == self.seat)
                && state.is_tsumo
                && state.in_riichi
                && state
                    .window
                    .is_some_and(|w| w.ops == 0 && w.opened_at >= self.window.opened_at)
            {
                return Verdict::Allowed;
            }
        }
        Verdict::Obsolete
    }

    /// Called only after the client's handler confirms dispatch. An unknown
    /// CDP outcome aborts the plan, and is never retried through this method.
    pub fn issued(&mut self, effect: Effect) {
        self.stage = if self.stage == Stage::Selected
            && effect == Effect::Button
            && matches!(self.action, MjaiEvent::Reach { .. })
        {
            Stage::RiichiDeclared
        } else {
            Stage::Finished
        };
    }
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
        for change in 0..9 {
            let (run, mut facts) = fixture(MjaiEvent::Dahai {
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
