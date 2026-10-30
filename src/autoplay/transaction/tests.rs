use super::*;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::platform::{DeclarationFacts, TargetIdentity};
use crate::autoplay::verify::{InputKind, InputWatch};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

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
fn one_discard() -> (MjaiEvent, Vec<PressIntent>) {
    let action = discard();
    let presses = vec![press(
        PressRole::Discard,
        &action,
        Confirmation::ClientCommand,
    )];
    (action, presses)
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
            bounds: rect(110., 510., 30., 55.),
            hit: rect(100., 500., 50., 75.),
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
            blocked: None,
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
    fn run(&self) -> &Run {
        self.controller.run.as_ref().unwrap()
    }
    fn target(&mut self) -> &mut InputTarget {
        self.observation.target.as_mut().unwrap()
    }
    /// Move the whole control, aim and hit region together.
    fn shift(&mut self, dx: f64, dy: f64) {
        let target = self.target();
        for rect in [&mut target.bounds, &mut target.hit] {
            rect.x += dx;
            rect.y += dy;
        }
    }
    fn next(&mut self) -> Effect {
        self.controller
            .next(&self.facts, Observed::Seen(&self.observation), self.now)
    }
    /// Read until the control is at rest and the pacing policy is asked.
    fn pace(&mut self) -> InputTarget {
        for _ in 0..3 {
            match self.next() {
                Effect::Pace { target, .. } => return target,
                Effect::Observe => {}
                _ => panic!("expected pacing, got {:?}", self.controller.phase()),
            }
        }
        panic!("no pacing request after three reads")
    }
    fn permit(&mut self) -> Permit {
        for _ in 0..4 {
            match self.next() {
                Effect::Pace { .. } => assert!(self.controller.paced(self.now)),
                Effect::Press { permit, .. } => return permit,
                Effect::Observe => {}
                _ => panic!(
                    "expected physical-input permit, got {:?}",
                    self.controller.phase()
                ),
            }
        }
        panic!("no permit after four reads")
    }
    fn consume(&mut self, permit: Permit) -> bool {
        self.controller.consume(
            permit,
            &self.facts,
            Some(&self.observation),
            (125., 530.),
            100,
            self.now,
        )
    }
    fn dispatch(&mut self) -> Permit {
        let permit = self.permit();
        assert!(self.consume(permit));
        assert!(!self.consume(permit));
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
        let (action, presses) = one_discard();
        let mut h = Harness::new(site, action.clone(), presses.clone());
        h.dispatch();
        assert_eq!(
            h.controller.phase(),
            Some(Phase::AwaitingCommand),
            "mouse dispatch is no client command"
        );
        h.command(InputKind::Discard);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
        assert!(!h.controller.begin(
            h.binding.clone(),
            action.clone(),
            TurnPlan {
                presses,
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
        assert!(!h.consume(old));
        h.now += Duration::from_millis(500);
        assert!(matches!(h.next(), Effect::YieldManual));
        h.facts.manual = false;
        // Re-observe geometry rather than reviving the old point or permit.
        h.shift(5., 0.);
        let next = h.permit();
        assert_ne!(next, old);
        assert!(!h.consume(old));
        assert!(h.consume(next));
        assert!(h.controller.dispatched(next, true, &h.facts, h.now));
        h.command(InputKind::Other);
        assert_eq!(h.outcome(), Outcome::ClientAccepted);
    }
}

#[test]
fn manual_yield_before_press_is_handled_at_travel_gate_and_final_consume() {
    for boundary in 0..3 {
        let (action, presses) = one_discard();
        let mut h = Harness::new("either", action, presses);
        let permit = if boundary == 0 {
            h.pace();
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
            _ => assert!(!h.consume(permit.unwrap())),
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
        let (action, presses) = one_discard();
        let mut h = Harness::new("either", action, presses);
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
            7 => h.target().identity.window = "different-native-window".into(),
            _ => h.target().identity.hand.reverse(),
        }
        assert!(matches!(h.outcome(), Outcome::Cancelled(_)));
    }
}

#[test]
fn a_manual_revision_between_polls_revokes_permission_even_after_the_mouse_has_rested() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    let old = h.permit();
    h.facts.owner.manual += 1;
    assert!(matches!(h.next(), Effect::YieldManual));
    let next = h.permit();
    assert_ne!(next, old);
    assert!(!h.consume(old));
    assert!(h.consume(next));
}

#[test]
fn manual_input_after_dispatch_preserves_acknowledgement_and_never_repeats_the_press() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    let old = h.dispatch();
    h.facts.manual = true;
    h.facts.owner.manual += 1;
    h.command(InputKind::Discard);
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
    assert!(!h.consume(old));
}

#[test]
fn an_unknown_partial_dispatch_is_not_retried_as_a_manual_yield() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    let permit = h.permit();
    assert!(h.consume(permit));
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
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
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
    assert!(!h.consume(permit));
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
        h.target().identity.control = "exact-red-candidate".into();
        h.target().identity.window = "candidate-menu".into();
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
        assert_eq!(h.controller.phase(), Some(Phase::AwaitingReveal));
        assert!(
            matches!(h.next(), Effect::Observe),
            "unchanged reveal is not clicked twice immediately"
        );
        h.observation.target_kind = TargetKind::Commit;
        h.target().identity.control = "actual-offered-candidate".into();
        h.target().identity.window = "revealed-menu".into();
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
    assert_eq!(h.controller.phase(), Some(Phase::AwaitingDeclaration));
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
        hand: h.target().identity.hand.clone(),
    });
    h.target().identity.window = "actual-declaration-native-window".into();
    assert!(
        matches!(h.next(), Effect::Observe),
        "declaration evidence cannot also supply the next intent's target"
    );
    // The real own echo advances to a freshly resolved tile.
    let permit = h.permit();
    assert_ne!(
        h.controller.phase(),
        Some(Phase::Finished(Outcome::ServerConfirmed))
    );
    assert!(h.consume(permit));
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
    h.target().identity.control = "declaring-tile".into();
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
            hand: h.target().identity.hand.clone(),
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
            h.now = h.run().deadline;
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
            assert_eq!(h.controller.phase(), Some(Phase::Continuation));
            assert!(!h.consume(old_permit));
            h.now += Duration::from_millis(60);
            if browser_catches_up {
                h.observation.continuation = Some(DeclarationFacts {
                    window: "declaring-discard".into(),
                    hand: vec!["copy-16:red-5m".into(), "copy-17:plain-5m".into()],
                });
                assert!(matches!(h.next(), Effect::Observe));
                assert_eq!(h.controller.intent().unwrap().role, PressRole::Discard);
                assert_eq!(h.controller.input_ticket(), Some(h.facts.input));
                assert_eq!(h.controller.phase(), Some(Phase::Aiming));
            } else {
                assert!(matches!(h.next(), Effect::Observe));
                h.now += Duration::from_millis(41);
                assert!(
                    matches!(h.next(), Effect::Observe),
                    "the real declaration receipt outlives the click-only verification timeout"
                );
                h.now = h.run().deadline;
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
            hand: h.target().identity.hand.clone(),
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
        hand: h.target().identity.hand.clone(),
    });
    h.target().identity.window = "echoed-discard-native-window".into();
    h.target().identity.control = "actual-riichi-discard-tile".into();
}

#[test]
fn captured_339ms_and_342ms_server_echoes_outlive_300ms_click_verification_and_still_discard() {
    // 20261028-195936: actual declaration uplink was received, but the old
    // owner cancelled at +310ms before the +339/+342ms server echoes.
    for site in ["dom", "canvas"] {
        for echo_ms in [339, 342] {
            let mut h = receipt_turn(site);
            let old_permit = h.dispatch();
            let original_deadline = h.run().deadline;
            h.command(InputKind::Reach);
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(h.controller.phase(), Some(Phase::Continuation));
            h.now += Duration::from_millis(310);
            assert!(
                matches!(h.next(), Effect::Observe),
                "accepted declaration must not become Unconfirmed while its echo is in flight"
            );
            assert_eq!(h.controller.intent().unwrap().role, PressRole::Declaration);
            h.now += Duration::from_millis(echo_ms - 310);
            own_declaration_echo(&mut h);
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(h.controller.phase(), Some(Phase::Aiming));
            assert_eq!(h.run().deadline, original_deadline);
            assert_eq!(h.controller.intent().unwrap().role, PressRole::Discard);
            assert_eq!(h.controller.input_ticket(), Some(h.facts.input));
            assert!(!h.consume(old_permit));
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
        let original_deadline = h.run().deadline;
        if received {
            h.command(InputKind::Reach);
            assert!(matches!(h.next(), Effect::Observe));
        }
        h.now += Duration::from_millis(310);
        if received {
            assert!(matches!(h.next(), Effect::Observe));
            assert_eq!(h.run().deadline, original_deadline);
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
        for change in 0..14 {
            let (action, presses) = one_discard();
            let mut h = Harness::new(site, action, presses);
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
                9 => h.target().identity.control = "plain-copy-17".into(),
                10 => h.target().identity.hand.reverse(),
                11 => h.shift(60., 0.),
                12 => h.observation.target = None,
                _ => {
                    h.watch.note_sent(InputKind::Discard);
                    h.facts.input = h.watch.ticket();
                }
            }
            assert!(!h.consume(permit));
            assert!(!h.controller.dispatched(permit, true, &h.facts, h.now));
            match change {
                1 | 5 => assert_eq!(h.controller.phase(), Some(Phase::PausedManual)),
                // A moved or briefly missing control is aimed at again, not abandoned.
                11 | 12 => assert_eq!(h.controller.phase(), Some(Phase::Aiming)),
                _ => assert!(matches!(
                    h.controller.phase(),
                    Some(Phase::Finished(Outcome::Cancelled(_)))
                )),
            }
        }
    }
}

#[test]
fn expired_clocks_invalid_geometry_and_unknown_dispatch_never_grant_another_press() {
    for invalid in 0..4 {
        let (action, presses) = one_discard();
        let mut h = Harness::new("either", action, presses);
        match invalid {
            0 => h.target().bounds.width = f64::NAN,
            1 => h.target().bounds.x = -1.,
            2 => h.observation.clock = None,
            _ => h.target().bounds.width = 1000.,
        }
        let finished = (0..3).find_map(|_| match h.next() {
            Effect::Finished { outcome, .. } => Some(outcome),
            Effect::Observe => None,
            _ => panic!("invalid facts granted progress"),
        });
        assert!(matches!(finished, Some(Outcome::Cancelled(_))));
    }
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses.clone());
    let permit = h.permit();
    assert!(!h.controller.consume(
        permit,
        &h.facts,
        Some(&h.observation),
        (125., 530.),
        100,
        h.now + Duration::from_millis(19_950)
    ));
    let mut h = Harness::new("either", discard(), presses);
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
        let (action, presses) = one_discard();
        let mut h = Harness::new(site, action, presses);
        let old = h.dispatch();
        h.now += Duration::from_millis(101);
        let permit = h.permit();
        assert!(!h.consume(old));
        assert!(h.consume(permit));
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
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    h.controller.run.as_mut().unwrap().settings.verify_ms = 0;
    h.dispatch();
    for _ in 0..5 {
        h.now += Duration::from_millis(200);
        assert!(matches!(h.next(), Effect::Observe));
        assert_eq!(h.run().attempts, 0);
    }
    h.command(InputKind::Discard);
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
}

#[test]
fn retry_exhaustion_and_cross_round_failure_evidence_have_one_owner() {
    let (action, presses) = one_discard();
    let plan = TurnPlan {
        presses,
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
            h.target().identity.window = "new-round-native-window".into();
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
            assert_eq!(h.run().attempts, attempt);
            h.now += Duration::from_millis(101);
        }
        assert!(
            matches!(h.next(), Effect::Finished { outcome:Outcome::Cancelled(CancelReason::Unconfirmed), reload } if reload == (failure == 1))
        );
    }
}

/// Faces read on 2026-10-29 19:58 for a Chi button. 630 ms after the offer
/// the button was still entering; the second read matches the settled
/// button in native frames of the same client.
fn chi_face(entering: bool) -> CanvasRect {
    if entering {
        rect(685.5, 529., 48.5, 25.5)
    } else {
        rect(643., 521.5, 51.5, 35.)
    }
}
fn meld_button(face: CanvasRect) -> InputTarget {
    InputTarget {
        identity: TargetIdentity {
            window: "majsoul-budget".into(),
            hand: vec!["7m:false".into(), "8m:false".into()],
            control: "meld:button:Chi".into(),
        },
        bounds: rect(
            face.x + face.width * 0.22,
            face.y + face.height * 0.28,
            face.width * 0.56,
            face.height * 0.38,
        ),
        hit: face,
        viewport: rect(0., 0., 1280., 720.),
    }
}
fn chi_turn() -> Harness {
    let action = MjaiEvent::Chi {
        actor: 2,
        target: 1,
        pai: "9m".into(),
        consumed: ["7m".into(), "8m".into()],
    };
    let mut h = Harness::new(
        "canvas",
        action.clone(),
        vec![press(
            PressRole::Action,
            &action,
            Confirmation::ClientCommand,
        )],
    );
    h.observation.target = Some(meld_button(chi_face(true)));
    h
}

#[test]
fn an_entering_call_button_is_bound_and_pressed_only_where_it_comes_to_rest() {
    let mut h = chi_turn();
    let deadline = |h: &Harness| h.run().deadline;
    assert!(matches!(h.next(), Effect::Observe));
    let anchored = deadline(&h);
    h.observation.target = Some(meld_button(chi_face(false)));
    assert!(
        matches!(h.next(), Effect::Observe),
        "a face that moved since the last read is not at rest"
    );
    let mut jitter = meld_button(chi_face(false));
    jitter.hit.x += 0.5;
    jitter.hit.width -= 0.5;
    h.observation.target = Some(jitter);
    let paced = h.pace();
    assert_eq!(paced.hit.x.round(), 644.);
    assert!(h.controller.paced(h.now + Duration::from_millis(900)));
    // The pointer is aimed at the resting button only.
    let entering = chi_face(true);
    let mut rng = StdRng::seed_from_u64(1958);
    for _ in 0..200 {
        let point = crate::autoplay::pointer::aim::target(paced.bounds, None, &mut rng);
        assert!(chi_face(false).contains(point.0, point.1));
        assert!(point.0 < entering.x);
    }
    h.now += Duration::from_millis(900);
    let Effect::Press { permit, target, .. } = h.next() else {
        panic!("a resting button is pressed when pacing allows");
    };
    assert_eq!(target.identity.control, "meld:button:Chi");
    assert!(h.controller.consume(
        permit,
        &h.facts,
        Some(&h.observation),
        (660., 535.),
        100,
        h.now
    ));
    assert_eq!(deadline(&h), anchored, "settling never restarts the clock");
}

#[test]
fn a_control_that_moves_while_pacing_is_followed_without_restarting_the_clock() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    h.pace();
    let until = h.now + Duration::from_millis(500);
    assert!(h.controller.paced(until));
    let deadline = h.run().deadline;
    assert!(matches!(h.next(), Effect::WaitUntil(_)));
    h.shift(40., 0.);
    assert!(
        matches!(h.next(), Effect::Observe),
        "a control seen once in a new place is not at rest"
    );
    h.now = until;
    h.shift(0.5, 0.);
    let Effect::Press { target, .. } = h.next() else {
        panic!("the control at rest in its new place is pressed");
    };
    assert_eq!(target.hit.x, 140.5);
    assert_eq!(h.run().deadline, deadline);
    assert_eq!(h.run().attempts, 0);
}

#[test]
fn a_hover_grown_face_still_accepts_the_aimed_point() {
    let mut h = chi_turn();
    h.observation.target = Some(meld_button(chi_face(false)));
    let permit = h.permit();
    let mut hovered = chi_face(false);
    hovered.y -= 6.;
    hovered.height += 10.;
    h.observation.target = Some(meld_button(hovered));
    assert!(h.controller.consume(
        permit,
        &h.facts,
        Some(&h.observation),
        (660., 535.),
        100,
        h.now
    ));
}

#[test]
fn an_unreadable_or_missing_control_at_the_final_probe_never_spends_an_attempt() {
    for unreadable in [true, false] {
        let (action, presses) = one_discard();
        let mut h = Harness::new("either", action, presses);
        let permit = h.permit();
        let observation = (!unreadable).then_some(SiteObservation {
            ready: false,
            blocked: None,
            target_kind: TargetKind::Commit,
            target: None,
            layout: None,
            clock: h.observation.clock,
            continuation: None,
        });
        assert!(!h.controller.consume(
            permit,
            &h.facts,
            observation.as_ref(),
            (125., 530.),
            100,
            h.now
        ));
        assert!(!h.controller.dispatched(permit, false, &h.facts, h.now));
        assert_eq!(h.controller.phase(), Some(Phase::Aiming));
        assert_eq!(h.run().attempts, 0);
        // Reads that fail keep the turn aiming until its own deadline.
        assert!(matches!(
            h.controller.next(&h.facts, Observed::Failed, h.now),
            Effect::Observe
        ));
        let next = h.permit();
        assert_ne!(next, permit);
        h.now = h.run().deadline;
        assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Expired));
    }
}

#[test]
fn a_control_never_seen_at_rest_ends_as_unavailable_not_expired() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    for step in 0..10 {
        h.shift(if step % 2 == 0 { 20. } else { -20. }, 0.);
        assert!(matches!(h.next(), Effect::Observe));
    }
    assert!(matches!(
        h.controller.next(&h.facts, Observed::Failed, h.now),
        Effect::Observe
    ));
    h.now = h.run().deadline;
    assert_eq!(
        h.outcome(),
        Outcome::Cancelled(CancelReason::TargetUnavailable)
    );
}

#[test]
fn a_different_control_after_binding_is_a_changed_target() {
    let (action, presses) = one_discard();
    let mut h = Harness::new("either", action, presses);
    h.pace();
    assert!(h.controller.paced(h.now));
    h.target().identity.control = "copy-17".into();
    assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::TargetChanged));
}

#[test]
fn a_win_that_closes_the_round_after_our_command_is_accepted() {
    let action = MjaiEvent::Hora {
        actor: 0,
        target: 2,
        deltas: None,
        ura_markers: None,
        pai: Some("5p".into()),
        tehai: None,
        scores: None,
        score: None,
    };
    let mut h = Harness::new(
        "canvas",
        action.clone(),
        vec![press(
            PressRole::Action,
            &action,
            Confirmation::ClientCommand,
        )],
    );
    h.dispatch();
    h.command(InputKind::Other);
    h.facts.context = None;
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
}

#[test]
fn a_late_receipt_meeting_a_retry_probe_is_accepted_without_a_second_press() {
    // 2026-10-30 02:36: a win request arrived 330 ms after its press, while
    // the retry was already aiming at the same button.
    let (action, presses) = one_discard();
    let mut h = Harness::new("canvas", action, presses);
    h.dispatch();
    h.now += Duration::from_millis(101);
    let retry = h.permit();
    h.command(InputKind::Discard);
    assert!(
        !h.consume(retry),
        "the late receipt withholds the retry press"
    );
    assert!(!h.controller.dispatched(retry, false, &h.facts, h.now));
    assert_eq!(h.outcome(), Outcome::ClientAccepted);
}

#[test]
fn a_closed_candidate_menu_is_reopened_within_the_same_attempt_budget() {
    let action = MjaiEvent::Chi {
        actor: 0,
        target: 3,
        pai: "6s".into(),
        consumed: ["5s".into(), "7s".into()],
    };
    let mut h = Harness::new(
        "canvas",
        action.clone(),
        vec![press(
            PressRole::Action,
            &action,
            Confirmation::ClientCommand,
        )],
    );
    let opener = h.observation.target.clone().unwrap();
    let mut candidate = opener.clone();
    candidate.identity.control = "meld:candidate:1:2".into();
    for attempt in 0..3 {
        h.observation.target_kind = TargetKind::Reveal;
        h.observation.target = Some(opener.clone());
        h.dispatch();
        assert_eq!(h.controller.phase(), Some(Phase::AwaitingReveal));
        h.observation.target_kind = TargetKind::Commit;
        h.observation.target = Some(candidate.clone());
        h.dispatch();
        assert_eq!(h.run().attempts, attempt);
        // The candidate click went unconfirmed and the menu closed again.
        h.now += Duration::from_millis(101);
    }
    h.observation.target_kind = TargetKind::Reveal;
    h.observation.target = Some(opener);
    assert_eq!(h.outcome(), Outcome::Cancelled(CancelReason::Unconfirmed));
}

/// Random interleavings of reads, motion, manual input, probes, dispatch
/// results and time. Whatever happens: a permit presses at most once,
/// attempts stay within the budget, the anchored deadline never moves,
/// and every press follows two agreeing reads of the pressed control.
#[test]
fn no_interleaving_presses_twice_extends_time_or_skips_settling() {
    for seed in 0..300 {
        let mut rng = StdRng::seed_from_u64(seed);
        let (action, presses) = one_discard();
        let mut h = Harness::new("either", action, presses);
        let mut anchored = None;
        let mut previous: Option<InputTarget> = None;
        let mut pressed = Vec::new();
        let mut permits = Vec::new();
        for _ in 0..200 {
            let shown = h.observation.target.is_some();
            match rng.random_range(0..12) {
                0 if shown => h.shift(rng.random_range(-0.4..0.4), 0.),
                1 if shown => h.shift(rng.random_range(-30.0..30.0), 0.),
                2 => {
                    h.observation.target = if h.observation.target.is_some() {
                        None
                    } else {
                        Harness::new("either", discard(), one_discard().1)
                            .observation
                            .target
                    }
                }
                3 => h.facts.manual = rng.random_bool(0.3),
                4 => h.now += Duration::from_millis(rng.random_range(0..400)),
                5 => {
                    if let Some(&permit) = permits.last() {
                        let point = (rng.random_range(90.0..160.0), 530.);
                        if h.controller.consume(
                            permit,
                            &h.facts,
                            Some(&h.observation),
                            point,
                            100,
                            h.now,
                        ) {
                            assert!(!pressed.contains(&permit), "a permit pressed twice");
                            pressed.push(permit);
                        }
                    }
                }
                6 => {
                    if let Some(&permit) = permits.last() {
                        h.controller
                            .dispatched(permit, rng.random_bool(0.9), &h.facts, h.now);
                    }
                }
                7 if rng.random_bool(0.1) => h.command(InputKind::Discard),
                _ => {}
            }
            let observation_before = h.observation.target.clone();
            let effect = h.next();
            match effect {
                Effect::Pace { .. } => {
                    h.controller.paced(h.now + Duration::from_millis(200));
                }
                Effect::Press { permit, target, .. } => {
                    let last = previous.as_ref().expect("a press without a previous read");
                    assert_eq!(last.identity, target.identity);
                    assert!(same_place(last, &target), "a press without settling");
                    permits.push(permit);
                }
                Effect::Finished { .. } => break,
                _ => {}
            }
            previous = observation_before;
            let Some(run) = h.controller.run.as_ref() else {
                continue;
            };
            assert!(run.attempts <= run.settings.retries);
            if run.clock.is_some() {
                assert_eq!(*anchored.get_or_insert(run.deadline), run.deadline);
            }
        }
    }
}
