//! Riichi City protocol target mapping. Pacing belongs to the common policy.
//! This native client has no browser mouse surface.

use crate::autoplay::platform::ActionContext;
use crate::bridge::riichi_city::build;

use crate::schema::MjaiEvent;

pub mod round_advance;
pub mod send;

#[derive(Default)]
pub struct RiichiCityAutoplay;

impl RiichiCityAutoplay {
    pub fn new() -> Self {
        Self
    }
}

impl RiichiCityAutoplay {
    pub fn plan(&self, ctx: &ActionContext) -> Option<Vec<u8>> {
        // Only our own decisions have an uplink frame; everything else
        // (other seats, transitions) is `None` from `encode_action`.
        if !is_our_decision(ctx.action, ctx.our_seat) {
            return None;
        }
        // A win names its tile: the drawn tile for a tsumo, the claimed
        // discard for a ron. Discards and calls carry rack positions, so
        // pass our tehai and the held draw through (see
        // `build::encode_action_with`). The snapshot's `drawn_tile` — not
        // `last_self_tsumo`, which stays stale through a post-call
        // discard — says whether a draw is actually in the rack now.
        let player = ctx.snapshot?.players.get(ctx.our_seat as usize);
        let hand = player.map(|p| p.tehai.clone());
        let drawn = player.and_then(|p| p.drawn_tile.clone());
        build::encode_action_with(
            ctx.action,
            drawn.as_deref(),
            ctx.last_kawa_tile,
            hand.as_deref(),
        )
    }
}

/// Whether the event is a decision by `our_seat` — the only decisions we
/// can transmit. Other seats' actions pass through the bus too and would
/// otherwise be echoed back to the server as if we had made them.
fn is_our_decision(ev: &MjaiEvent, our_seat: u8) -> bool {
    let actor = match ev {
        MjaiEvent::Dahai { actor, .. }
        | MjaiEvent::Reach { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Daiminkan { actor, .. }
        | MjaiEvent::Ankan { actor, .. }
        | MjaiEvent::Kakan { actor, .. }
        | MjaiEvent::Hora { actor, .. }
        | MjaiEvent::Kita { actor, .. } => Some(*actor),
        // `None` is the bot declining *our* call window — ours to send.
        // `Ryukyoku` (kyuushu, `deltas: None`) is the bot declaring on
        // *our* first-turn draw — no actor field, inherently ours.
        MjaiEvent::None | MjaiEvent::Ryukyoku { deltas: None, .. } => None,
        _ => return false,
    };
    actor.is_none_or(|a| a == our_seat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::platform::ActionContext;
    use crate::bridge::riichi_city::packet::WPacket;
    use crate::game_state::snapshot::{GameStateSnapshot, Phase};

    /// Owns the values an `ActionContext` borrows, so tests can build one
    /// without fighting lifetimes.
    struct CtxFixture {
        snapshot: GameStateSnapshot,
    }

    impl CtxFixture {
        fn new() -> Self {
            Self {
                snapshot: GameStateSnapshot {
                    bakaze: "E".into(),
                    kyoku: 1,
                    honba: 0,
                    kyotaku: 0,
                    oya: 0,
                    current_player: 0,
                    turn_count: 0,
                    phase: Phase::WaitAct,
                    is_done: false,
                    num_players: 4,
                    players: Vec::new(),
                    dora_markers: Vec::new(),
                    our_seat: Some(0),
                },
            }
        }

        fn ctx<'a>(&'a self, action: &'a MjaiEvent) -> ActionContext<'a> {
            ActionContext {
                action,
                snapshot: Some(&self.snapshot),
                legal_actions: &[],
                our_seat: 0,
                last_kawa_tile: None,
                last_self_tsumo: None,
                self_riichi_accepted: false,
                num_players: 4,
                pointer: None,
            }
        }
    }

    #[test]
    fn discard_plan_encodes_the_selected_action() {
        let ev = MjaiEvent::Dahai {
            actor: 0,
            pai: "5mr".into(),
            tsumogiri: false,
        };
        let f = CtxFixture::new();
        let plan = RiichiCityAutoplay::new().plan(&f.ctx(&ev));
        let pkts = WPacket::parse_frame(&plan.expect("encoded frame"));
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].body["cmd"], "req_game_action");
    }

    #[test]
    fn other_seats_actions_produce_no_plan() {
        let ev = MjaiEvent::Dahai {
            actor: 2,
            pai: "1p".into(),
            tsumogiri: true,
        };
        let f = CtxFixture::new();
        assert!(RiichiCityAutoplay::new().plan(&f.ctx(&ev)).is_none());
    }

    /// A kyuushu declaration is ours to send (no actor field — it can
    /// only come from the bot's own decision) and plans like any button
    /// decision: think, then frame. Regression for the live timeout where
    /// the declaration fell through `is_our_decision` and the server's
    /// countdown had to auto-discard for us.
    #[test]
    fn kyuushu_declaration_plans_a_frame() {
        let ev = MjaiEvent::Ryukyoku {
            deltas: None,
            tehais: None,
            scores: None,
            reason: None,
        };
        let f = CtxFixture::new();
        let plan = RiichiCityAutoplay::new().plan(&f.ctx(&ev));
        let pkts = WPacket::parse_frame(&plan.expect("encoded frame"));
        assert_eq!(pkts[0].body["data"]["action"], 12);
    }

    /// An observed exhaustive draw (deltas present) is not a decision —
    /// it must not plan, let alone send.
    #[test]
    fn observed_draw_does_not_plan() {
        let ev = MjaiEvent::Ryukyoku {
            deltas: Some(vec![1500, -500, -500, -500]),
            tehais: None,
            scores: None,
            reason: None,
        };
        let f = CtxFixture::new();
        assert!(RiichiCityAutoplay::new().plan(&f.ctx(&ev)).is_none());
    }
}
