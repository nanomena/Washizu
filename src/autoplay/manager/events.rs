//! The bookkeeping the mjai stream drives.
//!
//! A game resets everything. A hand keeps the seat, the table size, the
//! canvas cache and the dead-click count, and forgets the rest — the tiles
//! last seen, the riichi, and all the pointer knows ([`Hand::reset`]). The
//! presses between decisions (`autoplay::acts`) learn of each result, riichi,
//! deal and game end here.
//!
//! [`Hand::reset`]: crate::autoplay::pointer::Hand::reset

use super::{AutoplayManager, ManagerState};
use crate::autoplay::canvas::Canvas;
use crate::schema::MjaiEvent;
use std::time::Instant;

impl AutoplayManager {
    pub(super) fn handle_mjai_event(&mut self, ev: &MjaiEvent) {
        self.advisor.note(ev);
        match ev {
            MjaiEvent::StartGame {
                id, num_players, ..
            } => {
                // Capture our seat directly from the StartGame event rather
                // than going through the tracker. This avoids the try_lock
                // race entirely and makes cached_our_seat available from
                // the very first event of the game.
                self.acts.hand_over();
                self.new_game();
                self.state.cached_our_seat = *id;
                self.state.num_players = *num_players;
            }
            MjaiEvent::EndGame { .. } => {
                // The game ends as its last result shows: that result is
                // still pressed through, up to the game's own screens, and
                // those are looked for after it.
                self.acts.game_over(Instant::now());
                self.new_game();
            }
            MjaiEvent::Hora { .. } | MjaiEvent::Ryukyoku { .. } => {
                let confirmed = self.ctx.input_watch.confirmed();
                let ours = matches!(ev, MjaiEvent::Hora { actor, .. }
                    if Some(*actor) == self.state.cached_our_seat);
                self.acts
                    .result_shown(confirmed, ours, Instant::now(), &mut rand::rng());
            }
            MjaiEvent::StartKyoku { .. } | MjaiEvent::EndKyoku => {
                if matches!(ev, MjaiEvent::StartKyoku { .. }) {
                    self.acts.hand_over();
                }
                // Per-kyoku reset: keep last seen rect cache and cached seat,
                // drop everything else. Keep last_kawa_tile as None so
                // push_random_pre_delay uses the max delay (opening-hand guard).
                // The dead-click count survives too: a client that has
                // stopped accepting presses stays that way across the kyoku
                // boundary, and zeroing it here would need the failures to
                // land inside one hand before anything recovered them.
                self.state = ManagerState {
                    cached_our_seat: self.state.cached_our_seat,
                    dead_clicks: self.state.dead_clicks,
                    num_players: self.state.num_players,
                    ..ManagerState::default()
                };
                self.hand.reset();
            }
            MjaiEvent::Tsumo { actor, pai } => {
                if self.is_ours(*actor) {
                    self.hand.drew();
                    self.state.last_self_tsumo = Some(pai.clone());
                    // The seat's own turn: no more looking ahead to it.
                    self.hand.stop_glances();
                }
            }
            MjaiEvent::Dahai { actor, pai, .. } => {
                self.state.last_kawa_tile = Some(pai.clone());
                if self.is_ours(*actor) {
                    self.state.last_self_tsumo = None;
                }
            }
            MjaiEvent::ReachAccepted { actor } => {
                if self.is_ours(*actor) {
                    self.state.self_riichi_accepted = true;
                    self.acts.riichi_accepted(Instant::now(), &mut rand::rng());
                }
            }
            MjaiEvent::Chi { actor, .. }
            | MjaiEvent::Pon { actor, .. }
            | MjaiEvent::Daiminkan { actor, .. }
            | MjaiEvent::Ankan { actor, .. }
            | MjaiEvent::Kakan { actor, .. }
                if self.is_ours(*actor) =>
            {
                self.state.last_self_tsumo = None;
                // The seat's own turn, as after a draw: no more looking ahead
                // to it. A look still due would read a hand the tracker has
                // not caught up with, and set off while the discard is chosen.
                self.hand.stop_glances();
            }
            _ => {}
        }
    }

    /// A new table: nothing carries over, the canvas cache included.
    fn new_game(&mut self) {
        self.state = ManagerState::default();
        self.hand.reset();
        self.canvas = Canvas::default();
    }

    fn is_ours(&self, actor: u8) -> bool {
        self.our_seat_cached() == Some(actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_game(id: Option<u8>) -> MjaiEvent {
        MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: None,
            id,
            num_players: 4,
            game_meta: None,
        }
    }

    /// Regression: `cached_our_seat` must be populated immediately when
    /// `StartGame` is received, before any bot response fires. Previously
    /// the seat was only cached inside `handle_bot_response`, so the first
    /// `Tsumo` event on the opening draw could arrive before the bot had
    /// responded and `last_self_tsumo` would be silently missed.
    #[test]
    fn start_game_sets_cached_seat_immediately() {
        let mut m = AutoplayManager::for_tests();

        // Before any event — no seat cached.
        assert!(m.state.cached_our_seat.is_none());

        // StartGame with id = Some(1) — seat must be cached right away.
        m.handle_mjai_event(&start_game(Some(1)));
        assert_eq!(
            m.state.cached_our_seat,
            Some(1),
            "seat must be cached from StartGame"
        );

        // A Tsumo by our seat (1) before any bot response should be recorded.
        m.handle_mjai_event(&MjaiEvent::Tsumo {
            actor: 1,
            pai: "3m".into(),
        });
        assert_eq!(
            m.state.last_self_tsumo.as_deref(),
            Some("3m"),
            "last_self_tsumo must be recorded even before first bot response"
        );
    }

    /// Seat is preserved across `StartKyoku` and `EndKyoku` resets so
    /// tsumo tracking continues to work from the first draw of each round.
    #[test]
    fn cached_seat_survives_kyoku_reset() {
        let mut m = AutoplayManager::for_tests();

        m.handle_mjai_event(&start_game(Some(2)));
        assert_eq!(m.state.cached_our_seat, Some(2));

        m.handle_mjai_event(&MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "1m".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 0,
            scores: vec![25_000; 4],
            tehais: vec![vec!["1m".into(); 13]; 4],
            num_players: 4,
        });
        assert_eq!(
            m.state.cached_our_seat,
            Some(2),
            "seat must survive StartKyoku reset"
        );

        m.handle_mjai_event(&MjaiEvent::EndKyoku);
        assert_eq!(
            m.state.cached_our_seat,
            Some(2),
            "seat must survive EndKyoku reset"
        );
    }

    /// A client that has stopped accepting presses stays that way across
    /// a kyoku boundary — the failures run to the end of the game — so the
    /// count has to survive one, or the threshold would only ever be
    /// reached by failures packed into a single hand. It does reset for a
    /// new game, which is a new page state.
    #[test]
    fn dead_click_count_survives_a_kyoku_but_not_a_game() {
        let mut m = AutoplayManager::for_tests();
        m.state.dead_clicks = 2;
        m.handle_mjai_event(&MjaiEvent::EndKyoku);
        assert_eq!(m.state.dead_clicks, 2, "the client is still not listening");

        m.handle_mjai_event(&start_game(Some(0)));
        assert_eq!(m.state.dead_clicks, 0, "a new table is a fresh start");
    }

    /// Our riichi and a hand's result reach the presses between decisions;
    /// another seat's riichi does not, and a new deal clears what was due.
    #[test]
    fn riichi_and_results_are_handed_to_the_presses_between_decisions() {
        let mut m = AutoplayManager::for_tests();
        m.handle_mjai_event(&start_game(Some(0)));
        m.handle_mjai_event(&MjaiEvent::ReachAccepted { actor: 2 });
        assert!(m.acts.due().is_none(), "not our riichi");
        m.handle_mjai_event(&MjaiEvent::ReachAccepted { actor: 0 });
        assert!(m.acts.due().is_some(), "auto-win may go on");
        m.handle_mjai_event(&MjaiEvent::Ryukyoku {
            deltas: None,
            tehais: None,
            scores: None,
            reason: None,
        });
        let result = m.acts.due().expect("the result is pressed through");
        assert!(result <= Instant::now() + std::time::Duration::from_secs(30));
        m.handle_mjai_event(&MjaiEvent::EndKyoku);
        assert!(
            m.acts.due().is_some(),
            "the result is still up after the hand's end"
        );
        m.handle_mjai_event(&MjaiEvent::StartKyoku {
            bakaze: "E".into(),
            dora_marker: "1m".into(),
            kyoku: 2,
            honba: 0,
            kyotaku: 0,
            oya: 1,
            scores: vec![25_000; 4],
            tehais: vec![vec!["1m".into(); 13]; 4],
            num_players: 4,
        });
        assert!(m.acts.due().is_none(), "a new deal: nothing left to press");
    }

    /// Observer/replay mode: `StartGame` with `id: None` must not cache a
    /// stale seat from a previous game.
    #[test]
    fn start_game_without_id_clears_cached_seat() {
        let mut m = AutoplayManager::for_tests();

        m.handle_mjai_event(&start_game(Some(0)));
        assert_eq!(m.state.cached_our_seat, Some(0));

        // New game, observer mode — no seat.
        m.handle_mjai_event(&start_game(None));
        assert!(
            m.state.cached_our_seat.is_none(),
            "stale seat must be cleared"
        );
    }
}
