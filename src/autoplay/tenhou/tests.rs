use super::*;
use crate::autoplay::tenhou_state::{DecisionWindow, TenhouState, TurnClock, OP_REACH};

fn state(hand: &[u32]) -> TenhouState {
    TenhouState {
        hand: hand.to_vec(),
        is_tsumo: true,
        window: Some(DecisionWindow {
            ops: OP_REACH,
            opened_at: std::time::Instant::now(),
            clock: Some(TurnClock::NORMAL),
        }),
        ..Default::default()
    }
}

fn plan(action: &MjaiEvent, state: &TenhouState, accepted: bool) -> PlanResult {
    let snapshot = crate::game_state::snapshot::GameStateSnapshot {
        num_players: 4,
        players: Vec::new(),
        bakaze: "E".into(),
        kyoku: 1,
        honba: 0,
        kyotaku: 0,
        oya: 0,
        current_player: 0,
        turn_count: 0,
        phase: crate::game_state::snapshot::Phase::WaitAct,
        is_done: false,
        dora_markers: Vec::new(),
        our_seat: Some(0),
    };
    let cfg = crate::config::MajsoulAutoplayConfig::default();
    TenhouAutoplay.plan(&ActionContext {
        action,
        snapshot: &snapshot,
        legal_actions: &[],
        our_seat: 0,
        last_kawa_tile: None,
        last_self_tsumo: None,
        self_riichi_accepted: accepted,
        num_players: 4,
        cfg: &cfg,
        delay_cfg: crate::config::DelayModelConfig::default(),
        budget: None,
        probs: None,
        reading: Default::default(),
        delay_script: None,
        tenhou: Some(state),
        pointer: None,
    })
}

#[test]
fn riichi_uses_the_selected_physical_tedashi_then_waits_for_a_new_native_window() {
    let st = state(&[17, 0, 19]);
    let action = MjaiEvent::Reach {
        actor: 0,
        pai: Some("5m".into()),
        tsumogiri: Some(false),
    };
    let plan = plan(&action, &st, false);
    assert!(matches!(plan.steps.first(), Some(Step::AwaitReady { .. })));
    let declaration=plan.steps.iter().position(|step|matches!(step,Step::ClientChoice{payload,..} if payload==&serde_json::json!({"tag":"REACH"}))).unwrap();
    let ready = plan
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::AwaitReady { .. }))
        .unwrap();
    let discard = plan
        .steps
        .iter()
        .position(|step| matches!(step, Step::Discard { tile_index: 17 }))
        .unwrap();
    assert!(declaration < ready && ready < discard);
    assert_eq!(
        plan.steps
            .iter()
            .filter(|step| matches!(step, Step::Discard { .. }))
            .count(),
        1
    );
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
        .steps
        .is_empty());
    }
}

#[test]
fn accepted_riichi_unknown_clock_and_closed_window_issue_no_discard() {
    let action = MjaiEvent::Dahai {
        actor: 0,
        pai: "5m".into(),
        tsumogiri: true,
    };
    let mut st = state(&[17, 0, 19]);
    assert!(plan(&action, &st, true).steps.is_empty());
    st.window.as_mut().unwrap().clock = None;
    assert!(plan(&action, &st, false).steps.is_empty());
    st.window = None;
    assert!(plan(&action, &st, false).steps.is_empty());
}

#[test]
fn plain_and_drawn_copies_are_never_reinterpreted() {
    let st = state(&[16, 0, 19]);
    assert!(plan(
        &MjaiEvent::Dahai {
            actor: 0,
            pai: "5m".into(),
            tsumogiri: false
        },
        &st,
        false
    )
    .steps
    .is_empty());
    assert!(matches!(
        plan(
            &MjaiEvent::Dahai {
                actor: 0,
                pai: "5m".into(),
                tsumogiri: true
            },
            &st,
            false
        )
        .steps
        .last(),
        Some(Step::Discard { tile_index: 19 })
    ));
    assert!(matches!(
        plan(
            &MjaiEvent::Dahai {
                actor: 0,
                pai: "5mr".into(),
                tsumogiri: false
            },
            &st,
            false
        )
        .steps
        .last(),
        Some(Step::Discard { tile_index: 16 })
    ));
}
