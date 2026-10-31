use super::*;
use crate::autoplay::tenhou_state::{DecisionWindow, TurnClock, OP_REACH};
use crate::game_state::snapshot::{GameStateSnapshot, Phase};
use std::time::Duration;

fn state(hand: &[u32]) -> TenhouState {
    TenhouState {
        hand: hand.to_vec(),
        is_tsumo: true,
        window: Some(DecisionWindow {
            ops: OP_REACH,
            opened_at: Instant::now(),
            clock: Some(TurnClock::NORMAL),
        }),
        ..Default::default()
    }
}
fn snapshot() -> GameStateSnapshot {
    GameStateSnapshot {
        num_players: 4,
        players: vec![],
        bakaze: "E".into(),
        kyoku: 1,
        honba: 0,
        kyotaku: 0,
        oya: 0,
        current_player: 0,
        turn_count: 0,
        phase: Phase::WaitAct,
        is_done: false,
        dora_markers: vec![],
        our_seat: Some(0),
    }
}
fn plan(action: &MjaiEvent, state: &TenhouState, accepted: bool) -> TurnPlan {
    semantic_plan(
        &ActionContext {
            action,
            snapshot: Some(&snapshot()),
            legal_actions: &[],
            our_seat: 0,
            last_kawa_tile: None,
            last_self_tsumo: None,
            self_riichi_accepted: accepted,
            num_players: 4,
            pointer: None,
        },
        state,
    )
}
#[test]
fn riichi_plans_two_semantic_presses_and_requires_the_real_declaration_echo() {
    let st = state(&[17, 0, 19]);
    let action = MjaiEvent::Reach {
        actor: 0,
        pai: Some("5m".into()),
        tsumogiri: Some(false),
    };
    let plan = plan(&action, &st, false);
    assert_eq!(
        plan.presses,
        vec![
            PressIntent {
                role: PressRole::Declaration,
                action: action.clone(),
                confirmation: Confirmation::DeclarationEcho
            },
            PressIntent {
                role: PressRole::Discard,
                action: action.clone(),
                confirmation: Confirmation::ClientCommand
            },
        ]
    );
    assert_eq!(discard_tile(&action, &st), Some((17, false)));
}
#[test]
fn incomplete_or_incorrect_riichi_does_not_issue_a_declaration() {
    let st = state(&[17, 0, 19]);
    for (pai, tsumogiri) in [
        (None, Some(false)),
        (Some("5m".into()), None),
        (Some("6m".into()), Some(false)),
        (Some("1m".into()), Some(true)),
        (Some("5mr".into()), Some(false)),
    ] {
        assert!(plan(
            &MjaiEvent::Reach {
                actor: 0,
                pai,
                tsumogiri
            },
            &st,
            false
        )
        .presses
        .is_empty());
    }
}
#[test]
fn accepted_riichi_leaves_a_plain_draw_to_the_client_but_answers_a_win_or_kan_offer() {
    use riichienv_core::action::Action;
    let action = MjaiEvent::Dahai {
        actor: 0,
        pai: "5m".into(),
        tsumogiri: true,
    };
    let mut st = state(&[17, 0, 19]);
    st.in_riichi = true;
    st.window.as_mut().unwrap().ops = 0;
    let discard = Action::new(ActionType::Discard, Some(19), vec![], Some(0));
    let presses = |legal: &[Action], accepted: bool| {
        semantic_plan(
            &ActionContext {
                action: &action,
                snapshot: Some(&snapshot()),
                legal_actions: legal,
                our_seat: 0,
                last_kawa_tile: None,
                last_self_tsumo: None,
                self_riichi_accepted: accepted,
                num_players: 4,
                pointer: None,
            },
            &st,
        )
        .presses
        .len()
    };
    // 240 such discards across 31 runs: the client sent every one itself.
    assert_eq!(presses(std::slice::from_ref(&discard), true), 0);
    for offer in [ActionType::Tsumo, ActionType::Ankan] {
        let legal = [discard.clone(), Action::new(offer, None, vec![], Some(0))];
        assert_eq!(
            presses(&legal, true),
            1,
            "{offer:?} keeps the client waiting"
        );
    }
    assert_eq!(presses(std::slice::from_ref(&discard), false), 1);
    assert_eq!(
        presses(&[], true),
        1,
        "an unknown legal set keeps the press"
    );
}
#[test]
fn accepted_riichi_still_plans_a_discard_but_invalid_windows_and_other_actors_do_not() {
    let action = MjaiEvent::Dahai {
        actor: 0,
        pai: "5m".into(),
        tsumogiri: true,
    };
    let mut st = state(&[17, 0, 19]);
    st.in_riichi = true;
    let locked = plan(&action, &st, true);
    assert_eq!(locked.presses.len(), 1);
    assert_eq!(locked.presses[0].role, PressRole::Discard);
    assert_eq!(locked.presses[0].confirmation, Confirmation::ClientCommand);
    assert!(plan(
        &MjaiEvent::Dahai {
            actor: 1,
            pai: "5m".into(),
            tsumogiri: true
        },
        &st,
        false
    )
    .presses
    .is_empty());
    st.window.as_mut().unwrap().clock = None;
    assert!(plan(&action, &st, false).presses.is_empty());
    st.window = None;
    assert!(plan(&action, &st, false).presses.is_empty());
}
#[test]
fn plain_red_and_drawn_copies_are_never_reinterpreted() {
    let st = state(&[16, 0, 19]);
    let action = |pai: &str, tsumogiri| MjaiEvent::Dahai {
        actor: 0,
        pai: pai.into(),
        tsumogiri,
    };
    assert!(plan(&action("5m", false), &st, false).presses.is_empty());
    assert_eq!(discard_tile(&action("5m", true), &st), Some((19, true)));
    assert_eq!(discard_tile(&action("5mr", false), &st), Some((16, false)));
}
#[test]
fn raw_layout_retains_real_red_and_drawn_geometry_and_full_hand_identity() {
    let input:client::InputSnapshot=serde_json::from_value(serde_json::json!({
        "client":"web3","ready":true,"blocked":null,"window":"native-7","clock":null,"hand":[16,19],
        "viewport":{"x":0,"y":0,"width":900,"height":600},"offers":[],"nextDraw":null,
        "rest":{"x":400,"y":400,"width":50,"height":20},
        "tiles":[
          {"identity":"red-copy","tile":16,"drawn":false,"legal":true,"bounds":{"x":100,"y":500,"width":40,"height":60}},
          {"identity":"drawn-copy","tile":19,"drawn":true,"legal":true,"bounds":{"x":200,"y":500,"width":40,"height":60}}
        ]
    })).unwrap();
    let layout = layout(&input).unwrap();
    assert_eq!(layout.tiles[0].tile, "5mr");
    assert!(!layout.tiles[0].drawn);
    assert_eq!(layout.tiles[1].tile, "5m");
    assert!(layout.tiles[1].drawn);
    assert_eq!(layout.tiles[1].bounds.x, 200.0);
    assert!(same_hand(&input, &state(&[19, 16])));
    assert!(!same_hand(&input, &state(&[19, 17])));
}
#[test]
fn the_chosen_copy_is_found_on_the_page_whatever_the_client_marks_playable() {
    // A stray manual click can leave the client marking cards unplayable on our
    // own turn; the protocol's legal check already chose the tile.
    let input: client::InputSnapshot = serde_json::from_value(serde_json::json!({
        "client":"web3","ready":true,"blocked":null,"window":"native-7","clock":null,"hand":[16,19],
        "viewport":{"x":0,"y":0,"width":900,"height":600},"offers":[],"nextDraw":null,"rest":null,
        "tiles":[
          {"identity":"red-copy","tile":16,"drawn":false,"legal":false,"bounds":{"x":100,"y":500,"width":40,"height":60}},
          {"identity":"drawn-copy","tile":19,"drawn":true,"legal":true,"bounds":{"x":200,"y":500,"width":40,"height":60}}
        ]
    }))
    .unwrap();
    assert!(on_the_page(&input, 16, false));
    assert!(on_the_page(&input, 19, true));
    // The other physical copy is never substituted, and a tile not shown is not found.
    assert!(!on_the_page(&input, 16, true));
    assert!(!on_the_page(&input, 19, false));
    assert!(!on_the_page(&input, 20, false));
}
#[test]
fn native_clock_is_stable_and_bank_is_the_actual_grant_at_that_window_start() {
    let adapter = TenhouAutoplay::new(Arc::new(AutoplayContext::new()));
    let now = Instant::now();
    let raw = client::ClockFacts {
        elapsed_ms: 4000,
        fixed_ms: 3000,
        granted_bank_ms: 6000,
        remaining_base_ms: 0,
        remaining_bank_ms: 5000,
    };
    let facts = adapter.clock("protocol:native-7", &raw, now).unwrap();
    assert_eq!(facts.started_at, now - Duration::from_millis(4000));
    assert_eq!(facts.bank_at_start_ms, Some(6000));
    assert_eq!(
        facts.started_at
            + Duration::from_millis(u64::from(facts.fixed_ms + facts.bank_at_start_ms.unwrap())),
        now + Duration::from_millis(5000)
    );
    assert_eq!(
        adapter
            .clock("protocol:native-7", &raw, now + Duration::from_secs(1))
            .unwrap(),
        facts
    );
    assert_ne!(
        adapter
            .clock("protocol:native-8", &raw, now + Duration::from_secs(1))
            .unwrap()
            .started_at,
        facts.started_at
    );
}

#[test]
fn unclickable_target_keeps_the_real_clock_without_creating_input_permission() {
    let now = Instant::now();
    let shared = Arc::new(AutoplayContext::new());
    let adapter = TenhouAutoplay::new(shared);
    let state = state(&[0, 17, 19]);
    let raw=serde_json::from_value::<client::InputSnapshot>(serde_json::json!({
        "client":"web3","ready":false,"blocked":"phase","window":"native-unready","hand":[0,17,19],
        "viewport":{"x":0,"y":0,"width":900,"height":600},"tiles":[],"offers":[],"nextDraw":null,"rest":null,
        "clock":{"elapsedMs":3000,"fixedMs":0,"grantedBankMs":10000,"remainingBaseMs":0,"remainingBankMs":7000}
    })).unwrap();
    let action = MjaiEvent::Dahai {
        actor: 0,
        pai: "5m".into(),
        tsumogiri: false,
    };
    let facts = adapter.facts(&state, &raw, &action, now);
    assert!(!facts.ready);
    assert!(facts.target.is_none());
    let clock = facts.clock.unwrap();
    assert_eq!(clock.fixed_ms, 0);
    assert_eq!(clock.bank_at_start_ms, Some(10000));
    assert_eq!(clock.started_at, now - Duration::from_millis(3000));
}

#[test]
fn real_declaration_facts_advance_the_common_owner_while_the_declaration_target_is_gone() {
    use crate::autoplay::context::CanvasRect;
    use crate::autoplay::transaction::{
        Binding, Effect, Facts, Observed, Owner, Phase as TurnPhase, Settings, TurnController,
    };
    use crate::autoplay::verify::{InputKind, InputWatch};
    let now = Instant::now();
    let mut state = state(&[0, 17, 19]);
    let shared = Arc::new(AutoplayContext::new());
    let adapter = TenhouAutoplay::new(shared);
    let action = MjaiEvent::Reach {
        actor: 0,
        pai: Some("5m".into()),
        tsumogiri: Some(false),
    };
    let input = |window: &str| {
        serde_json::from_value::<client::InputSnapshot>(serde_json::json!({
        "client":"web3","ready":true,"blocked":null,"window":window,"hand":[0,17,19],
        "viewport":{"x":0,"y":0,"width":900,"height":600},"tiles":[],"offers":[],"nextDraw":null,"rest":null,
        "clock":{"elapsedMs":0,"fixedMs":5000,"grantedBankMs":6000,"remainingBaseMs":5000,"remainingBankMs":6000}
    })).unwrap()
    };
    let raw = input("native-before-declaration");
    let mut observation = adapter.facts(&state, &raw, &action, now);
    assert!(observation.continuation.is_none());
    let rect = CanvasRect {
        x: 100.0,
        y: 400.0,
        width: 60.0,
        height: 40.0,
    };
    observation.target = Some(InputTarget {
        identity: TargetIdentity {
            window: raw.window.clone().unwrap(),
            hand: raw.hand.iter().map(u32::to_string).collect(),
            control: "native-reach-button".into(),
        },
        bounds: rect,
        hit: rect,
        viewport: raw.viewport,
    });
    observation.ready = true;
    let watch = InputWatch::default();
    let context = crate::monitor::DecisionContext {
        game: 1,
        round: 1,
        event_seq: 7,
    };
    let owner = Owner {
        session: 1,
        table: 1,
        settings: 1,
        manual: 0,
    };
    let proof =
        window_identity(crate::bot::control::InputWindow::Tenhou(state.window.unwrap()).proof());
    let binding = Binding {
        context,
        owner,
        window: proof.clone(),
        input: watch.ticket(),
    };
    let mut facts = Facts {
        context: Some(context),
        owner,
        window: Some(proof),
        enabled: true,
        manual: false,
        input: watch.ticket(),
        command: false,
        latest: None,
    };
    let mut controller = TurnController::default();
    assert!(controller.begin(
        binding,
        action.clone(),
        plan(&action, &state, false),
        Settings {
            retries: 0,
            verify_ms: 100,
            gap_ms: 0,
            ready_ms: 8000,
            reload_after: 0
        },
        now
    ));
    // The first read only shows the button; the second shows it at rest.
    assert!(matches!(
        controller.next(&facts, Observed::Seen(&observation), now),
        Effect::Observe
    ));
    assert!(matches!(
        controller.next(&facts, Observed::Seen(&observation), now),
        Effect::Pace { .. }
    ));
    assert!(controller.paced(now));
    let Effect::Press { permit, .. } = controller.next(&facts, Observed::Seen(&observation), now)
    else {
        panic!("declaration target must be permitted");
    };
    assert!(controller.consume(permit, &facts, Some(&observation), (125.0, 420.0), 100, now));
    assert!(controller.dispatched(permit, true, &facts, now));
    assert_eq!(controller.phase(), Some(TurnPhase::AwaitingDeclaration));
    assert_eq!(controller.intent().unwrap().role, PressRole::Declaration);
    // Real own REACH, not an invented follow-up event; no declaration button
    // remains in the new native input window.
    watch.note_sent(InputKind::Reach);
    assert!(
        !state.in_riichi,
        "step 2 has not happened before the declaring discard"
    );
    state.window.as_mut().unwrap().ops = 0;
    state.window.as_mut().unwrap().opened_at += Duration::from_nanos(1);
    let echoed = crate::monitor::DecisionContext {
        event_seq: 8,
        ..context
    };
    let next = input("native-after-declaration");
    let observation = adapter.facts(&state, &next, &controller.intent().unwrap().action, now);
    assert!(observation.target.is_none());
    assert!(observation.continuation.is_some());
    facts.context = Some(echoed);
    facts.input = watch.ticket();
    facts.command = true;
    facts.window = Some(window_identity(
        crate::bot::control::InputWindow::Tenhou(state.window.unwrap()).proof(),
    ));
    facts.latest = Some((
        echoed,
        MjaiEvent::Reach {
            actor: 0,
            pai: None,
            tsumogiri: None,
        },
    ));
    assert!(matches!(
        controller.next(&facts, Observed::Seen(&observation), now),
        Effect::Observe
    ));
    assert_eq!(controller.intent().unwrap().role, PressRole::Discard);
    assert_ne!(controller.phase(), Some(TurnPhase::AwaitingDeclaration));
    // The declaring discard receives its own permit under the real step-1
    // window, with no acceptance flag and no second decision source.
    facts.command = false;
    let mut discard = observation;
    discard.ready = true;
    discard.target = Some(InputTarget {
        identity: TargetIdentity {
            window: next.window.clone().unwrap(),
            hand: next.hand.iter().map(u32::to_string).collect(),
            control: "native-tile-19".into(),
        },
        bounds: rect,
        hit: rect,
        viewport: raw.viewport,
    });
    assert!(matches!(
        controller.next(&facts, Observed::Seen(&discard), now),
        Effect::Observe
    ));
    let Effect::Press { permit, .. } = controller.next(&facts, Observed::Seen(&discard), now)
    else {
        panic!("declaring discard permit")
    };
    assert!(controller.consume(permit, &facts, Some(&discard), (125.0, 420.0), 100, now));
    assert!(controller.dispatched(permit, true, &facts, now));
    assert!(
        matches!(
            controller.next(&facts, Observed::Seen(&discard), now),
            Effect::Observe
        ),
        "dispatch alone does not prove acceptance"
    );
    watch.note_sent(InputKind::Discard);
    facts.input = watch.ticket();
    facts.command = true;
    assert!(matches!(
        controller.next(&facts, Observed::Seen(&discard), now),
        Effect::Finished {
            outcome: crate::autoplay::transaction::Outcome::ClientAccepted,
            ..
        }
    ));
}
