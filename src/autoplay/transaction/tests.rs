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
        assert!(self
            .controller
            .dispatched(permit, true, &self.facts, self.now));
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
fn manual_yield_resumes_the_same_abort_action_with_a_new_single_use_permit() {
    for site in ["canvas", "dom"] {
        let action = MjaiEvent::Ryukyoku {
            deltas: None,
            tehais: None,
            scores: None,
            reason: None,
        };
        let mut h = Harness::new(
            site,
            action.clone(),
            vec![press(
                PressRole::Action,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        let old = h.permit();
        h.facts.manual = true;
        h.facts.owner.manual += 1;
        assert!(matches!(h.next(), Effect::YieldManual));
        assert_eq!(h.controller.phase(), Some(Phase::PausedManual));
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
        h.now += Duration::from_millis(500);
        assert!(matches!(h.next(), Effect::YieldManual));
        h.facts.manual = false;
        // Re-observe geometry rather than reviving the old point or permit.
        h.observation.target.as_mut().unwrap().bounds.x += 5.;
        let next = h.permit();
        assert_ne!(next, old);
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
        assert!(h
            .controller
            .consume(next, &h.facts, &h.observation, (125., 530.), 100, h.now));
        assert!(h.controller.dispatched(next, true, &h.facts, h.now));
        h.command(InputKind::Other);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn manual_yield_before_press_is_handled_at_travel_gate_and_final_consume() {
    for boundary in 0..3 {
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
        let permit = if boundary == 0 {
            assert!(matches!(h.next(), Effect::Pace { .. }));
            assert!(h.controller.paced(h.now));
            None
        } else {
            Some(h.permit())
        };
        h.facts.manual = true;
        h.facts.owner.manual += 1;
        match boundary {
            0 => {
                // The common pacing walk precedes permission creation.
                h.controller.approach_failed(&h.facts, h.now);
            }
            1 => assert!(!h
                .controller
                .dispatched(permit.unwrap(), false, &h.facts, h.now)),
            _ => assert!(!h.controller.consume(
                permit.unwrap(),
                &h.facts,
                &h.observation,
                (125., 530.),
                100,
                h.now
            )),
        }
        assert_eq!(h.controller.phase(), Some(Phase::PausedManual));
        h.facts.manual = false;
        h.dispatch();
        h.command(InputKind::Discard);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn manual_yield_never_extends_deadline_or_reuses_a_changed_context_window_or_input() {
    for change in 0..9 {
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
        h.permit();
        h.facts.manual = true;
        h.facts.owner.manual += 1;
        assert!(matches!(h.next(), Effect::YieldManual));
        h.facts.manual = false;
        match change {
            0 => h.now += Duration::from_secs(20),
            1 => h.facts.context.as_mut().unwrap().event_seq += 1,
            2 => h.facts.window = Some("another-window".into()),
            3 => {
                h.watch.note_sent(InputKind::Discard);
                h.facts.input = h.watch.ticket();
            }
            4 => h.facts.owner.session += 1,
            5 => h.facts.owner.table += 1,
            6 => h.facts.owner.settings += 1,
            7 => {
                h.observation.target.as_mut().unwrap().identity.window =
                    "different-native-window".into()
            }
            _ => h
                .observation
                .target
                .as_mut()
                .unwrap()
                .identity
                .hand
                .reverse(),
        }
        assert!(matches!(h.outcome(), Outcome::Cancelled(_)));
    }
}

#[test]
fn a_manual_revision_between_polls_revokes_permission_even_after_the_mouse_has_rested() {
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
    let old = h.permit();
    h.facts.owner.manual += 1;
    assert!(matches!(h.next(), Effect::YieldManual));
    let next = h.permit();
    assert_ne!(next, old);
    assert!(!h
        .controller
        .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
    assert!(h
        .controller
        .consume(next, &h.facts, &h.observation, (125., 530.), 100, h.now));
}

#[test]
fn manual_input_after_dispatch_preserves_acknowledgement_and_never_repeats_the_press() {
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
    let old = h.dispatch();
    h.facts.manual = true;
    h.facts.owner.manual += 1;
    h.command(InputKind::Discard);
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
    assert!(!h
        .controller
        .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
}

#[test]
fn an_unknown_partial_dispatch_is_not_retried_as_a_manual_yield() {
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
    assert!(h
        .controller
        .consume(permit, &h.facts, &h.observation, (125., 530.), 100, h.now));
    h.facts.manual = true;
    h.facts.owner.manual += 1;
    assert!(!h.controller.dispatched(permit, false, &h.facts, h.now));
    assert_eq!(
        h.outcome(),
        Outcome::Cancelled(CancelReason::DispatchUnknown)
    );
}

#[test]
fn manual_yield_before_geometry_uses_the_real_clock_once_without_extending_it() {
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
    h.facts.manual = true;
    h.observation.ready = false;
    assert!(matches!(h.next(), Effect::YieldManual));
    // The real window grants 20 seconds; initial readiness was only 8 seconds.
    h.now += Duration::from_secs(12);
    h.facts.manual = false;
    h.observation.ready = true;
    // A later observation may not restart the original clock.
    h.observation.clock.as_mut().unwrap().started_at = h.now;
    let permit = h.permit();
    h.now += Duration::from_secs(9);
    assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Expired));
    assert!(!h
        .controller
        .consume(permit, &h.facts, &h.observation, (125., 530.), 100, h.now));
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
    assert!(h.controller.dispatched(permit, true, &h.facts, h.now));
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
        if matches!(change, 2 | 3) {
            assert!(matches!(h.next(), Effect::Observe));
            h.now = h.controller.run.as_ref().unwrap().deadline;
        }
        assert!(matches!(h.outcome(), Outcome::Cancelled(_)));
    }
}

#[test]
fn declaration_echo_before_browser_probe_waits_without_spending_the_discard_or_resetting_time() {
    for site in ["dom", "canvas"] {
        for browser_catches_up in [true, false] {
            let action = reach();
            let mut h = Harness::new(
                site,
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
            let old_permit = h.dispatch();
            h.command(InputKind::Reach);
            let echoed = DecisionContext {
                event_seq: 11,
                ..h.binding.context
            };
            h.facts.context = Some(echoed);
            h.facts.window = Some("declaring-discard".into());
            h.facts.latest = Some((
                echoed,
                MjaiEvent::Reach {
                    actor: 0,
                    pai: None,
                    tsumogiri: None,
                },
            ));
            h.observation.ready = false;
            h.observation.target = None;
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(
                h.controller.phase(),
                Some(Phase::WaitingDeclarationContinuation)
            );
            assert!(!h.controller.consume(
                old_permit,
                &h.facts,
                &h.observation,
                (125., 530.),
                100,
                h.now
            ));
            h.now += Duration::from_millis(60);
            if browser_catches_up {
                h.observation.continuation = Some(DeclarationFacts {
                    window: "declaring-discard".into(),
                    hand: vec!["copy-16:red-5m".into(), "copy-17:plain-5m".into()],
                });
                assert!(matches!(h.next(), Effect::Observe));
                assert_eq!(h.controller.intent().unwrap().role, PressRole::Discard);
                assert_eq!(h.controller.input_ticket(), Some(h.facts.input));
                assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
            } else {
                assert!(matches!(h.next(), Effect::Observe));
                h.now += Duration::from_millis(41);
                assert!(
                    matches!(h.next(), Effect::Observe),
                    "the real declaration receipt outlives the click-only verification timeout"
                );
                h.now = h.controller.run.as_ref().unwrap().deadline;
                assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Expired));
            }
        }
    }
}

#[test]
fn a_discard_or_other_command_during_declaration_can_never_be_consumed_as_the_followup() {
    for kind in [InputKind::Discard, InputKind::Other, InputKind::Reach] {
        let action = reach();
        let mut h = Harness::new(
            "either",
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
        h.command(InputKind::Reach);
        h.command(kind); // two declarations are also superseding input
        let echoed = DecisionContext {
            event_seq: 11,
            ..h.binding.context
        };
        h.facts.context = Some(echoed);
        h.facts.window = Some("declaring-discard".into());
        h.facts.latest = Some((
            echoed,
            MjaiEvent::Reach {
                actor: 0,
                pai: None,
                tsumogiri: None,
            },
        ));
        h.observation.continuation = Some(DeclarationFacts {
            window: "declaring-discard".into(),
            hand: h.observation.target.as_ref().unwrap().identity.hand.clone(),
        });
        assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Context));
    }
}

fn receipt_turn(site: &str) -> Harness {
    let action = reach();
    let mut h = Harness::new(
        site,
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
    h.controller.run.as_mut().unwrap().settings.verify_ms = 300;
    h.observation.clock.as_mut().unwrap().fixed_ms = 3000;
    h.observation.clock.as_mut().unwrap().bank_at_start_ms = Some(5000);
    h
}

fn own_declaration_echo(h: &mut Harness) {
    let context = DecisionContext {
        event_seq: 11,
        ..h.binding.context
    };
    h.facts.context = Some(context);
    h.facts.window = Some("echoed-discard-window".into());
    h.facts.latest = Some((
        context,
        MjaiEvent::Reach {
            actor: 0,
            pai: None,
            tsumogiri: None,
        },
    ));
    h.observation.continuation = Some(DeclarationFacts {
        window: "echoed-discard-window".into(),
        hand: h.observation.target.as_ref().unwrap().identity.hand.clone(),
    });
    h.observation.target.as_mut().unwrap().identity.window = "echoed-discard-native-window".into();
    h.observation.target.as_mut().unwrap().identity.control = "actual-riichi-discard-tile".into();
}

#[test]
fn captured_339ms_and_342ms_server_echoes_outlive_300ms_click_verification_and_still_discard() {
    // 20261028-195936: actual declaration uplink was received, but the old
    // owner cancelled at +310ms before the +339/+342ms server echoes.
    for site in ["dom", "canvas"] {
        for echo_ms in [339, 342] {
            let mut h = receipt_turn(site);
            let old_permit = h.dispatch();
            let original_deadline = h.controller.run.as_ref().unwrap().deadline;
            h.command(InputKind::Reach);
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(
                h.controller.phase(),
                Some(Phase::WaitingDeclarationContinuation)
            );
            h.now += Duration::from_millis(310);
            assert!(
                matches!(h.next(), Effect::Observe),
                "accepted declaration must not become Unconfirmed while its echo is in flight"
            );
            assert_eq!(h.controller.intent().unwrap().role, PressRole::Declaration);
            h.now += Duration::from_millis(echo_ms - 310);
            own_declaration_echo(&mut h);
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
            assert_eq!(
                h.controller.run.as_ref().unwrap().deadline,
                original_deadline
            );
            assert_eq!(h.controller.intent().unwrap().role, PressRole::Discard);
            assert_eq!(h.controller.input_ticket(), Some(h.facts.input));
            assert!(!h.controller.consume(
                old_permit,
                &h.facts,
                &h.observation,
                (125., 530.),
                100,
                h.now
            ));
            h.facts.command = false; // the already-consumed REACH is not a discard receipt
            h.dispatch();
            assert!(matches!(h.next(), Effect::Observe));
            h.command(InputKind::Discard);
            assert_eq!(h.outcome(), Outcome::ClientAccepted);
        }
    }
}

#[test]
fn missing_declaration_receipt_keeps_click_timeout_but_a_real_receipt_keeps_only_original_turn_deadline(
) {
    for received in [false, true] {
        let mut h = receipt_turn("either");
        h.dispatch();
        let original_deadline = h.controller.run.as_ref().unwrap().deadline;
        if received {
            h.command(InputKind::Reach);
            assert!(matches!(h.next(), Effect::Observe));
        }
        h.now += Duration::from_millis(310);
        if received {
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(
                h.controller.run.as_ref().unwrap().deadline,
                original_deadline
            );
            h.now = original_deadline;
            assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Expired));
        } else {
            assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Unconfirmed));
        }
    }
}

#[test]
fn waiting_for_a_declaration_echo_never_survives_replacement_ownership_input_or_unrelated_context()
{
    for change in 0..5 {
        let mut h = receipt_turn("either");
        h.dispatch();
        h.command(InputKind::Reach);
        assert!(matches!(h.next(), Effect::Observe));
        h.now += Duration::from_millis(400);
        match change {
            0 => h.command(InputKind::Discard),
            1 => h.facts.owner.table += 1,
            2 => h.facts.context.as_mut().unwrap().event_seq += 1,
            3 => h.facts.enabled = false,
            _ => h.controller.cancel(CancelReason::Replaced),
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
            assert!(!h.controller.dispatched(permit, true, &h.facts, h.now));
            if change == 1 || change == 5 {
                assert_eq!(h.controller.phase(), Some(Phase::PausedManual));
            } else if change == 11 {
                assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
            } else {
                assert!(matches!(
                    h.controller.phase(),
                    Some(Phase::Finished(Outcome::Cancelled(_)))
                ));
            }
        }
    }
}

#[test]
fn same_control_geometry_can_settle_during_pacing_without_restarting_the_clock() {
    for site in ["canvas", "dom"] {
        let action = MjaiEvent::Chi {
            actor: 0,
            target: 3,
            pai: "9m".into(),
            consumed: ["7m".into(), "8m".into()],
        };
        let mut h = Harness::new(
            site,
            action.clone(),
            vec![press(
                PressRole::Action,
                &action,
                Confirmation::ClientCommand,
            )],
        );
        h.observation.target.as_mut().unwrap().viewport = rect(0., 0., 1280., 720.);
        h.observation.target.as_mut().unwrap().bounds = rect(696.17, 536.14, 27.16, 9.69);
        assert!(matches!(h.next(), Effect::Pace { .. }));
        assert!(h.controller.paced(h.now));
        let deadline = h.controller.run.as_ref().unwrap().deadline;
        h.observation.target.as_mut().unwrap().bounds = rect(654.33, 531.3, 28.84, 13.3);
        let Effect::Press {
            permit: old,
            target,
            ..
        } = h.next()
        else {
            panic!("fresh physical target expected")
        };
        assert_eq!(target.bounds, h.observation.target.as_ref().unwrap().bounds);
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (710., 541.), 100, h.now));
        assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
        assert!(!h.controller.dispatched(old, true, &h.facts, h.now));
        let permit = h.permit();
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (668., 537.), 100, h.now));
        assert!(h
            .controller
            .consume(permit, &h.facts, &h.observation, (668., 537.), 100, h.now));
        assert!(h.controller.dispatched(permit, true, &h.facts, h.now));
        assert_eq!(h.controller.run.as_ref().unwrap().deadline, deadline);
        h.command(InputKind::Other);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn lost_geometry_and_an_unconfirmed_click_reacquire_only_within_the_retry_budget() {
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
        let old = h.permit();
        let original = h.observation.target.take().unwrap();
        h.observation.ready = false;
        assert!(!h
            .controller
            .consume(old, &h.facts, &h.observation, (125., 530.), 100, h.now));
        assert_eq!(h.controller.phase(), Some(Phase::WaitingReady));
        assert!(matches!(h.next(), Effect::Observe));
        h.observation.target = Some(InputTarget {
            bounds: rect(130., 500., 50., 75.),
            ..original
        });
        h.observation.ready = true;
        let fresh = h.permit();
        assert!(h
            .controller
            .consume(fresh, &h.facts, &h.observation, (155., 530.), 100, h.now));
        assert!(h.controller.dispatched(fresh, true, &h.facts, h.now));
        h.now += Duration::from_millis(101);
        h.observation.target.as_mut().unwrap().bounds.x += 20.;
        let Effect::Press {
            permit: retry,
            retry: attempt,
            ..
        } = h.next()
        else {
            panic!("an unconfirmed click may retry on freshly measured geometry")
        };
        assert_eq!(attempt, 1);
        assert!(h
            .controller
            .consume(retry, &h.facts, &h.observation, (175., 530.), 100, h.now));
        assert!(h.controller.dispatched(retry, true, &h.facts, h.now));
        h.command(InputKind::Discard);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn fresh_geometry_does_not_reset_the_unconfirmed_click_retry_limit() {
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
        let deadline = h.controller.run.as_ref().unwrap().deadline;
        for expected in 1..=2 {
            h.now += Duration::from_millis(101);
            h.observation.target.as_mut().unwrap().bounds.x += 20.;
            let Effect::Press {
                permit,
                target,
                retry,
                ..
            } = h.next()
            else {
                panic!("retry expected")
            };
            assert_eq!(retry, expected);
            let point = (target.bounds.x + 25., target.bounds.y + 30.);
            assert!(h
                .controller
                .consume(permit, &h.facts, &h.observation, point, 100, h.now));
            assert!(h.controller.dispatched(permit, true, &h.facts, h.now));
            assert_eq!(h.controller.run.as_ref().unwrap().deadline, deadline);
        }
        h.now += Duration::from_millis(101);
        h.observation.target.as_mut().unwrap().bounds.x += 20.;
        assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Unconfirmed));
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
    assert!(!h.controller.dispatched(permit, false, &h.facts, h.now));
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
        assert!(!h.controller.dispatched(old, true, &h.facts, h.now));
        assert!(h.controller.dispatched(permit, true, &h.facts, h.now));
        h.command(InputKind::Discard);
        h.facts.context.as_mut().unwrap().event_seq += 1;
        h.facts.window = Some("unrelated-window".into());
        assert!(
            matches!(h.outcome(), Outcome::Cancelled(_)),
            "a different window's true command is not ours"
        );
    }
}

#[test]
fn disabling_automatic_verification_never_becomes_a_one_millisecond_retry_loop() {
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
    h.controller.run.as_mut().unwrap().settings.verify_ms = 0;
    h.dispatch();
    for _ in 0..5 {
        h.now += Duration::from_millis(200);
        assert!(matches!(h.next(), Effect::Observe));
        assert_eq!(h.controller.run.as_ref().unwrap().retries, 0);
    }
    h.command(InputKind::Discard);
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
}

#[test]
fn retry_exhaustion_and_cross_round_failure_evidence_have_one_owner() {
    let action = discard();
    let plan = TurnPlan {
        presses: vec![press(
            PressRole::Discard,
            &action,
            Confirmation::ClientCommand,
        )],
        dealt: false,
    };
    let mut h = Harness::new("either", action.clone(), plan.presses.clone());
    for failure in 0..2 {
        if failure > 0 {
            h.binding.context.round += 1;
            h.binding.context.event_seq += 1;
            h.binding.owner.table += 1;
            h.binding.window = "new-round-protocol-window".into();
            h.facts.context = Some(h.binding.context);
            h.facts.owner = h.binding.owner;
            h.facts.window = Some(h.binding.window.clone());
            h.observation.target.as_mut().unwrap().identity.window =
                "new-round-native-window".into();
            h.observation.clock.as_mut().unwrap().started_at = h.now;
            assert!(h.controller.begin(
                h.binding.clone(),
                action.clone(),
                plan.clone(),
                Settings {
                    retries: 2,
                    verify_ms: 100,
                    gap_ms: 0,
                    ready_ms: 8_000,
                    reload_after: 2
                },
                h.now
            ));
        }
        for attempt in 0..3 {
            h.dispatch();
            assert_eq!(h.controller.run.as_ref().unwrap().retries, attempt);
            h.now += Duration::from_millis(101);
        }
        assert!(
            matches!(h.next(), Effect::Finished { outcome:Outcome::Cancelled(CancelReason::Unconfirmed), reload } if reload == (failure == 1))
        );
    }
}
