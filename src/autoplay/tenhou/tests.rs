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
            snapshot: &snapshot(),
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
fn accepted_riichi_unknown_clock_closed_window_and_other_actor_issue_no_discard() {
    let action = MjaiEvent::Dahai {
        actor: 0,
        pai: "5m".into(),
        tsumogiri: true,
    };
    let mut st = state(&[17, 0, 19]);
    assert!(plan(&action, &st, true).presses.is_empty());
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
        "client":"web3","ready":true,"window":"native-7","clock":null,"hand":[16,19],
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
    assert_eq!(facts.remaining_bank_ms, Some(6000));
    assert_eq!(
        facts.started_at
            + Duration::from_millis(u64::from(facts.fixed_ms + facts.remaining_bank_ms.unwrap())),
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
