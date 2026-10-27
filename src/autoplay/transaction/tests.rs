use super::*;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::platform::{DeclarationFacts, TargetIdentity};
use crate::autoplay::verify::{InputKind, InputWatch};

struct Harness {
    controller: TurnController,
    binding: Binding,
    facts: Facts,
    observation: SiteObservation,
    now: Instant,
    watch: InputWatch,
}

fn discard() -> MjaiEvent {
    MjaiEvent::Dahai {
        actor: 0,
        pai: "5mr".into(),
        tsumogiri: false,
    }
}
fn reach() -> MjaiEvent {
    MjaiEvent::Reach {
        actor: 0,
        pai: Some("5mr".into()),
        tsumogiri: Some(false),
    }
}
fn press(role: PressRole, action: &MjaiEvent, confirmation: Confirmation) -> PressIntent {
    PressIntent {
        role,
        action: action.clone(),
        confirmation,
    }
}
fn rect(x: f64, y: f64, width: f64, height: f64) -> CanvasRect {
    CanvasRect {
        x,
        y,
        width,
        height,
    }
}

impl Harness {
    fn new(site: &str, action: MjaiEvent, presses: Vec<PressIntent>) -> Self {
        let now = Instant::now();
        let watch = InputWatch::default();
        let context = DecisionContext {
            game: 3,
            round: 2,
            event_seq: 10,
        };
        let owner = Owner {
            session: 7,
            table: 2,
            settings: 1,
            manual: 8,
        };
        let binding = Binding {
            context,
            owner,
            window: format!("{site}-real-protocol-1"),
            input: watch.ticket(),
        };
        let target = InputTarget {
            identity: TargetIdentity {
                window: format!("{site}-native-1"),
                hand: vec!["copy-16:red-5m".into(), "copy-17:plain-5m".into()],
                control: "copy-16".into(),
            },
            bounds: rect(100., 500., 50., 75.),
            viewport: rect(0., 0., 900., 600.),
        };
        let facts = Facts {
            context: Some(context),
            owner,
            window: Some(binding.window.clone()),
            enabled: true,
            manual: false,
            input: watch.ticket(),
            command: false,
            latest: None,
        };
        let observation = SiteObservation {
            ready: true,
            target_kind: TargetKind::Commit,
            target: Some(target),
            layout: None,
            clock: Some(TurnClockFacts {
                started_at: now,
                fixed_ms: 20_000,
                bank_at_start_ms: None,
            }),
            continuation: None,
        };
        let mut controller = TurnController::default();
        assert!(controller.begin(
            binding.clone(),
            action,
            TurnPlan {
                presses,
                dealt: false
            },
            Settings {
                retries: 2,
                verify_ms: 100,
                gap_ms: 0,
                ready_ms: 8_000,
                reload_after: 2
            },
            now
        ));
        Self {
            controller,
            binding,
            facts,
            observation,
            now,
            watch,
        }
    }
    fn next(&mut self) -> Effect {
        self.controller
            .next(&self.facts, Some(&self.observation), self.now)
    }
    fn permit(&mut self) -> Permit {
        match self.next() {
            Effect::Pace { .. } => {
                assert!(self.controller.paced(self.now));
                let Effect::Press { permit, .. } = self.next() else {
                    panic!("expected physical-input permit");
                };
                permit
            }
            Effect::Press { permit, .. } => permit,
            _ => panic!(
                "expected physical-input permit, got {:?}",
                self.controller.phase()
            ),
        }
    }
    fn dispatch(&mut self) -> Permit {
        let permit = self.permit();
        assert!(self.controller.consume(
            permit,
            &self.facts,
            &self.observation,
            (125., 530.),
            100,
            self.now
        ));
        assert!(!self.controller.consume(
            permit,
            &self.facts,
            &self.observation,
            (125., 530.),
            100,
            self.now
        ));
        assert!(self.controller.dispatched(permit, true, self.now));
        permit
    }
    fn command(&mut self, kind: InputKind) {
        self.watch.note_sent(kind);
        self.facts.input = self.watch.ticket();
        self.facts.command = true;
    }
    fn outcome(&mut self) -> Outcome {
        let Effect::Finished { outcome, .. } = self.next() else {
            panic!("turn not finished: {:?}", self.controller.phase());
        };
        outcome
    }
}

#[test]
fn both_site_observations_use_the_same_discard_and_client_confirmation_lifetime() {
    for site in ["canvas", "dom"] {
        let action = discard();
        let mut h = Harness::new(
            site,
            action.clone(),
            vec![press(
                PressRole::Discard,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        h.dispatch();
        assert_eq!(
            h.controller.phase(),
            Some(Phase::WaitingCommand),
            "mouse dispatch is no client command"
        );
        h.command(InputKind::Discard);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
        assert!(!h.controller.begin(
            h.binding.clone(),
            action.clone(),
            TurnPlan {
                presses: vec![press(
                    PressRole::Discard,
                    &action,
                    Confirmation::ClientCommand
                )],
                dealt: false
            },
            Settings {
                retries: 0,
                verify_ms: 0,
                gap_ms: 0,
                ready_ms: 8_000,
                reload_after: 0
            },
            h.now
        ));
    }
}

#[test]
fn calls_candidates_and_local_reveal_are_common_dispatches_not_action_success() {
    for site in ["canvas", "dom"] {
        let action = MjaiEvent::Pon {
            actor: 0,
            target: 2,
            pai: "5m".into(),
            consumed: ["5m".into(), "5mr".into()],
        };
        let mut h = Harness::new(
            site,
            action.clone(),
            vec![
                press(PressRole::Action, &action, Confirmation::Dispatch),
                press(PressRole::Candidate, &action, Confirmation::ClientCommand),
            ],
        );
        h.dispatch();
        h.observation.target.as_mut().unwrap().identity.control = "exact-red-candidate".into();
        h.observation.target.as_mut().unwrap().identity.window = "candidate-menu".into();
        h.dispatch();
        h.command(InputKind::Other);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);

        let mut h = Harness::new(
            site,
            action.clone(),
            vec![press(
                PressRole::Action,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        h.observation.target_kind = TargetKind::Reveal;
        h.dispatch();
        assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
        assert!(
            matches!(h.next(), Effect::Observe),
            "unchanged reveal is not clicked twice immediately"
        );
        h.observation.target_kind = TargetKind::Commit;
        h.observation.target.as_mut().unwrap().identity.control = "actual-offered-candidate".into();
        h.observation.target.as_mut().unwrap().identity.window = "revealed-menu".into();
        h.dispatch();
        h.command(InputKind::Other);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn declaration_echo_is_one_exact_continuation_and_never_completes_the_declaring_discard() {
    let action = reach();
    let mut h = Harness::new(
        "dom",
        action.clone(),
        vec![
            press(
                PressRole::Declaration,
                &action,
                Confirmation::DeclarationEcho,
            ),
            press(PressRole::Discard, &action, Confirmation::ClientCommand),
        ],
    );
    h.dispatch();
    assert_eq!(h.controller.phase(), Some(Phase::WaitingDeclaration));
    let echoed = DecisionContext {
        event_seq: 11,
        ..h.binding.context
    };
    let actual = MjaiEvent::Reach {
        actor: 0,
        pai: None,
        tsumogiri: None,
    };
    h.facts.context = Some(echoed);
    h.facts.window = Some("actual-declaration-window".into());
    h.facts.latest = Some((echoed, actual));
    h.observation.continuation = Some(DeclarationFacts {
        window: h.facts.window.clone().unwrap(),
        hand: h.observation.target.as_ref().unwrap().identity.hand.clone(),
    });
    h.observation.target.as_mut().unwrap().identity.window =
        "actual-declaration-native-window".into();
    assert!(
        matches!(h.next(), Effect::Observe),
        "declaration evidence cannot also supply the next intent's target"
    );
    assert!(
        matches!(h.next(), Effect::Press { .. }),
        "real own echo advances to a freshly resolved tile"
    );
    assert_ne!(
        h.controller.phase(),
        Some(Phase::Finished(Outcome::ServerConfirmed))
    );
    let permit = h.controller.run.as_ref().unwrap().permit.unwrap();
    assert!(h
        .controller
        .consume(permit, &h.facts, &h.observation, (125., 530.), 100, h.now));
    assert!(h.controller.dispatched(permit, true, h.now));
    assert!(
        matches!(h.next(), Effect::Observe),
        "declaration echo cannot finish final discard"
    );
    let discarded = DecisionContext {
        event_seq: 12,
        ..echoed
    };
    h.facts.latest = Some((discarded, discard()));
    h.facts.context = Some(discarded);
    h.facts.window = None;
    assert_eq!(h.outcome(), Outcome::ServerConfirmed);
}

#[test]
fn a_fused_declaration_requires_the_declaration_command_not_a_plain_discard() {
    let action = reach();
    let mut h = Harness::new(
        "canvas",
        action.clone(),
        vec![
            press(PressRole::Declaration, &action, Confirmation::Dispatch),
            press(PressRole::Discard, &action, Confirmation::ClientCommand),
        ],
    );
    h.dispatch();
    h.observation.target.as_mut().unwrap().identity.control = "declaring-tile".into();
    h.dispatch();
    assert_eq!(h.controller.requirement(), InputRequirement::Declaration);
    h.watch.note_sent(InputKind::Discard);
    h.facts.input = h.watch.ticket();
    h.facts.command = false;
    assert!(matches!(h.outcome(), Outcome::Cancelled(_)));
}

#[test]
fn wrong_or_skipped_declaration_echo_cannot_rebind_context_window_or_hand() {
    for change in 0..5 {
        let action = reach();
        let mut h = Harness::new(
            "dom",
            action.clone(),
            vec![
                press(
                    PressRole::Declaration,
                    &action,
                    Confirmation::DeclarationEcho,
                ),
                press(PressRole::Discard, &action, Confirmation::ClientCommand),
            ],
        );
        h.dispatch();
        let context = DecisionContext {
            event_seq: 11,
            ..h.binding.context
        };
        h.facts.context = Some(context);
        h.facts.window = Some("next-window".into());
        h.facts.latest = Some((
            context,
            MjaiEvent::Reach {
                actor: 0,
                pai: None,
                tsumogiri: None,
            },
        ));
        h.observation.continuation = Some(DeclarationFacts {
            window: "next-window".into(),
            hand: h.observation.target.as_ref().unwrap().identity.hand.clone(),
        });
        match change {
            0 => h.facts.latest.as_mut().unwrap().0.event_seq += 1,
            1 => {
                h.facts.latest.as_mut().unwrap().1 = MjaiEvent::Reach {
                    actor: 1,
                    pai: None,
                    tsumogiri: None,
                }
            }
            2 => h.observation.continuation.as_mut().unwrap().window = h.binding.window.clone(),
            3 => h.observation.continuation.as_mut().unwrap().hand.reverse(),
            _ => h.facts.context.as_mut().unwrap().round += 1,
        }
        assert!(matches!(h.outcome(), Outcome::Cancelled(_)));
    }
}

#[test]
fn final_mouse_permission_rechecks_every_owner_context_input_and_physical_target_fact() {
    for site in ["canvas", "dom"] {
        for change in 0..13 {
            let action = discard();
            let mut h = Harness::new(
                site,
                action.clone(),
                vec![press(
                    PressRole::Discard,
                    &action,
                    Confirmation::ClientCommand,
                )],
            );
            let permit = h.permit();
            match change {
                0 => h.facts.enabled = false,
                1 => h.facts.manual = true,
                2 => h.facts.owner.session += 1,
                3 => h.facts.owner.table += 1,
                4 => h.facts.owner.settings += 1,
                5 => h.facts.owner.manual += 1,
                6 => h.facts.context = None,
                7 => h.facts.context.as_mut().unwrap().event_seq += 1,
                8 => h.facts.window = None,
                9 => {
                    h.observation.target.as_mut().unwrap().identity.control = "plain-copy-17".into()
                }
                10 => h
                    .observation
                    .target
                    .as_mut()
                    .unwrap()
                    .identity
                    .hand
                    .reverse(),
                11 => h.observation.target.as_mut().unwrap().bounds.x += 1.,
                _ => {
                    h.watch.note_sent(InputKind::Discard);
                    h.facts.input = h.watch.ticket();
                }
            }
            assert!(!h.controller.consume(
                permit,
                &h.facts,
                &h.observation,
                (125., 530.),
                100,
                h.now
            ));
            assert!(!h.controller.dispatched(permit, true, h.now));
            assert!(matches!(
                h.controller.phase(),
                Some(Phase::Finished(Outcome::Cancelled(_)))
            ));
        }
    }
}

#[test]
fn expired_clocks_invalid_geometry_and_unknown_dispatch_never_grant_another_press() {
    for invalid in 0..4 {
        let action = discard();
        let mut h = Harness::new(
            "either",
            action.clone(),
            vec![press(
                PressRole::Discard,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        match invalid {
            0 => h.observation.target.as_mut().unwrap().bounds.width = f64::NAN,
            1 => h.observation.target.as_mut().unwrap().bounds.x = -1.,
            2 => h.observation.clock = None,
            _ => h.observation.target.as_mut().unwrap().bounds.width = 1000.,
        }
        assert!(matches!(
            h.next(),
            Effect::Finished {
                outcome: Outcome::Cancelled(_),
                ..
            }
        ));
    }
    let action = discard();
    let mut h = Harness::new(
        "either",
        action.clone(),
        vec![press(
            PressRole::Discard,
            &action,
            Confirmation::ClientCommand,
        )],
    );
    let permit = h.permit();
    assert!(!h.controller.consume(
        permit,
        &h.facts,
        &h.observation,
        (125., 530.),
        100,
        h.now + Duration::from_millis(19_950)
    ));
    let mut h = Harness::new(
        "either",
        action.clone(),
        vec![press(
            PressRole::Discard,
            &action,
            Confirmation::ClientCommand,
        )],
    );
    let permit = h.permit();
    assert!(!h.controller.dispatched(permit, false, h.now));
    assert_eq!(
        h.outcome(),
        Outcome::Cancelled(CancelReason::DispatchUnknown)
    );
}

#[test]
fn retries_are_bounded_single_use_and_a_late_ack_cannot_be_transplanted_from_another_window() {
    for site in ["canvas", "dom"] {
        let action = discard();
        let mut h = Harness::new(
            site,
            action.clone(),
            vec![press(
                PressRole::Discard,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        let old = h.dispatch();
        h.now += Duration::from_millis(101);
        let permit = h.permit();
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
        assert!(h
            .controller
            .consume(permit, &h.facts, &h.observation, (125., 530.), 100, h.now));
        assert!(!h.controller.dispatched(old, true, h.now));
        assert!(h.controller.dispatched(permit, true, h.now));
        h.command(InputKind::Discard);
        h.facts.context.as_mut().unwrap().event_seq += 1;
        h.facts.window = Some("unrelated-window".into());
        assert!(
            matches!(h.outcome(), Outcome::Cancelled(_)),
            "a different window's true command is not ours"
        );
    }
}
