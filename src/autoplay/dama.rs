//! Whether a person would have the client's own auto-win on for a settled
//! hand: one played dama, or one in riichi.
//!
//! A hand is played dama when it is closed (concealed kans aside), not in
//! riichi, ready, and wins by ron on every one of its waits: it has a yaku
//! without the riichi. Such a hand is settled, and a person sitting on one
//! often lets the client take its win — while it is below a mangan whichever
//! way it wins, and not in the hand that may be the game's last. A big hand is
//! watched, and in the last hand where the game ends decides whether a win is
//! wanted. The game's length is not known here, so every fourth hand counts
//! as one that may be the last, and so does every hand of an extra round.
//!
//! A hand in riichi has auto-win likewise only below a mangan, its riichi
//! counted (ura dora and ippatsu cannot be known ahead).
//!
//! The client's no-calls is not switched: a hand played dama may still fold,
//! and call at the end to be ready when the wall runs out.
//!
//! Read on the hand at rest: after our discard, or with the tile just drawn
//! set aside. Four players only.

use crate::game_state::snapshot::{GameStateSnapshot, MeldKind, PlayerSnapshot};
use riichienv_core::hand_evaluator::HandEvaluator;
use riichienv_core::types::{Conditions, Meld, MeldType, WinResult, Wind};

/// A tile's kind, 0..34: the numbers of each suit, then the honours. A red
/// five is a five.
fn kind(tile: &str) -> Option<u8> {
    let tile = tile.strip_suffix('r').unwrap_or(tile);
    if let Some(honour) = ["E", "S", "W", "N", "P", "F", "C"]
        .iter()
        .position(|h| *h == tile)
    {
        return Some(27 + honour as u8);
    }
    let mut chars = tile.chars();
    let number = chars.next()?.to_digit(10)? as u8;
    let suit = match chars.next()? {
        'm' => 0,
        'p' => 9,
        's' => 18,
        _ => return None,
    };
    (1..=9).contains(&number).then_some(suit + number - 1)
}

/// The tiles as the evaluator counts them, one id each out of 136: a red
/// five is its suit's first five, any other tile the next copy of its kind.
/// `None` for a tile not read, or a fifth copy.
fn ids(tiles: &[&str]) -> Option<Vec<u8>> {
    let mut used = [0u8; 34];
    tiles
        .iter()
        .map(|tile| {
            let k = kind(tile)?;
            if tile.ends_with('r') {
                return Some(k * 4);
            }
            let five = matches!(k, 4 | 13 | 22);
            let copy = used[k as usize] + u8::from(five);
            used[k as usize] += 1;
            (copy <= 3).then_some(k * 4 + copy)
        })
        .collect()
}

/// A mangan or more: five han, or four with 40 fu, or three with 70.
fn mangan(win: &WinResult) -> bool {
    win.yakuman || win.han >= 5 || (win.han == 4 && win.fu >= 40) || (win.han == 3 && win.fu >= 70)
}

/// The concealed hand at rest: as it is between turns, or with the tile just
/// drawn set aside. `None` in the middle of anything else.
fn at_rest(me: &PlayerSnapshot) -> Option<Vec<&str>> {
    let mut hand: Vec<&str> = me.tehai.iter().map(String::as_str).collect();
    match hand.len() + me.melds.len() * 3 {
        13 => Some(hand),
        14 => {
            let drawn = me.drawn_tile.as_deref()?;
            let at = hand.iter().rposition(|t| *t == drawn)?;
            hand.remove(at);
            Some(hand)
        }
        _ => None,
    }
}

/// A closed hand that is ready, and how each of its waits would score.
struct Ready {
    evaluator: HandEvaluator,
    waits: Vec<u8>,
    indicators: Vec<u8>,
    conditions: Conditions,
}

impl Ready {
    /// `seat`'s hand at rest: `None` when it cannot be read, `Some(None)`
    /// when it is open (concealed kans aside) or not ready.
    fn read(s: &GameStateSnapshot, seat: u8) -> Option<Option<Ready>> {
        let me = s.players.get(usize::from(seat))?;
        if me.melds.iter().any(|m| m.kind != MeldKind::Ankan) {
            return Some(None);
        }
        let hand = at_rest(me)?;
        let kans: Vec<&str> = me
            .melds
            .iter()
            .flat_map(|m| m.tiles.iter().map(String::as_str))
            .collect();
        let all = ids(&[hand.as_slice(), kans.as_slice()].concat())?;
        let (hand_ids, kan_ids) = all.split_at(hand.len());
        let melds = kan_ids
            .chunks(4)
            .map(|kan| Meld::new(MeldType::Ankan, kan.to_vec(), false, -1, None))
            .collect();
        let evaluator = HandEvaluator::new(hand_ids.to_vec(), melds);
        if !evaluator.is_tenpai() {
            return Some(None);
        }
        let waits = evaluator.get_waits_u8();
        let indicators = s
            .dora_markers
            .iter()
            .map(|m| kind(m).map(|k| k * 4))
            .collect::<Option<_>>()?;
        let conditions = Conditions {
            player_wind: Wind::from((seat + 4 - s.oya % 4) % 4),
            round_wind: Wind::from(match s.bakaze.as_str() {
                "S" => 1,
                "W" => 2,
                "N" => 3,
                _ => 0,
            }),
            riichi: me.riichi_declared,
            double_riichi: me.double_riichi,
            num_players: 4,
            ..Conditions::default()
        };
        Some(Some(Ready {
            evaluator,
            waits,
            indicators,
            conditions,
        }))
    }

    /// The hand won on `wait` (its last copy: never a red five), by ron or tsumo.
    fn win(&self, wait: u8, tsumo: bool) -> WinResult {
        self.evaluator.calc(
            wait * 4 + 3,
            self.indicators.clone(),
            vec![],
            Some(Conditions {
                tsumo,
                ..self.conditions.clone()
            }),
        )
    }

    /// A mangan or more on some wait, by ron or by tsumo.
    fn big(&self) -> bool {
        self.waits
            .iter()
            .any(|&w| mangan(&self.win(w, false)) || mangan(&self.win(w, true)))
    }
}

/// Whether `seat`'s hand, not in riichi, is played dama and wants auto-win;
/// `None` when it is not at rest or cannot be read.
pub fn dama_win(s: &GameStateSnapshot, seat: u8) -> Option<bool> {
    let me = s.players.get(usize::from(seat))?;
    if s.num_players != 4 || me.riichi_declared {
        return Some(false);
    }
    let Some(ready) = Ready::read(s, seat)? else {
        return Some(false);
    };
    let dama = !ready.waits.is_empty() && ready.waits.iter().all(|&w| ready.win(w, false).is_win);
    let last = s.kyoku >= 4 || matches!(s.bakaze.as_str(), "W" | "N");
    Some(dama && !last && !ready.big())
}

/// Whether `seat`'s hand in riichi is below a mangan whichever way it wins,
/// its riichi counted; `None` when it is not at rest or cannot be read.
pub fn riichi_small(s: &GameStateSnapshot, seat: u8) -> Option<bool> {
    let me = s.players.get(usize::from(seat))?;
    if s.num_players != 4 || !me.riichi_declared {
        return None;
    }
    let ready = Ready::read(s, seat)??;
    Some(!ready.big())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_state::snapshot::{MeldSnapshot, Phase};

    /// Seat 0's hand on an East 1 table with nothing else on it.
    fn table(hand: &str) -> GameStateSnapshot {
        let players = (0..4u8)
            .map(|seat| PlayerSnapshot {
                seat,
                tehai: if seat == 0 {
                    hand.split_whitespace().map(String::from).collect()
                } else {
                    Vec::new()
                },
                melds: Vec::new(),
                river: Vec::new(),
                score: 25000,
                riichi_declared: false,
                riichi_stage: false,
                double_riichi: false,
                riichi_declaration_index: None,
                kita_tiles: Vec::new(),
                drawn_tile: None,
            })
            .collect();
        GameStateSnapshot {
            bakaze: "E".into(),
            kyoku: 1,
            honba: 0,
            kyotaku: 0,
            oya: 1,
            current_player: 1,
            turn_count: 0,
            phase: Phase::WaitResponse,
            is_done: false,
            num_players: 4,
            players,
            dora_markers: vec!["9p".into()],
            our_seat: Some(0),
        }
    }

    /// All simples, waiting on 2s and 5s: tanyao and pinfu either way, a small hand.
    const TANYAO: &str = "2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 4s 6m 6m";

    #[test]
    fn a_small_closed_hand_with_a_yaku_on_every_wait_wants_auto_win() {
        assert_eq!(dama_win(&table(TANYAO), 0), Some(true));
    }

    #[test]
    fn a_tile_just_drawn_is_set_aside_and_any_other_fourteenth_is_not_read() {
        let mut drawn = table(&format!("{TANYAO} 9m"));
        assert_eq!(dama_win(&drawn, 0), None);
        drawn.players[0].drawn_tile = Some("9m".into());
        assert_eq!(dama_win(&drawn, 0), Some(true));
    }

    #[test]
    fn riichi_an_open_hand_or_one_not_ready_is_not_played_dama() {
        let mut riichi = table(TANYAO);
        riichi.players[0].riichi_declared = true;
        assert_eq!(dama_win(&riichi, 0), Some(false));

        let mut open = table("2m 3m 4m 4p 5p 6p 3s 4s 6m 6m");
        open.players[0].melds.push(MeldSnapshot {
            kind: MeldKind::Pon,
            tiles: vec!["7s".into(); 3],
            from_who: 2,
            called_tile: Some("7s".into()),
        });
        assert_eq!(dama_win(&open, 0), Some(false));

        let apart = table("2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 9s 6m 1p");
        assert_eq!(dama_win(&apart, 0), Some(false));
    }

    #[test]
    fn a_wait_without_a_yaku_is_not_played_dama() {
        // 1s and 4s: no simples, the pair is the round's wind, no pinfu.
        assert_eq!(
            dama_win(&table("1m 2m 3m 4p 5p 6p 7s 8s 9s 2s 3s E E"), 0),
            Some(false)
        );
    }

    #[test]
    fn a_concealed_kan_keeps_the_hand_closed() {
        let mut kan = table("2m 3m 4m 4p 5p 6p 3s 4s 6m 6m");
        kan.players[0].melds.push(MeldSnapshot {
            kind: MeldKind::Ankan,
            tiles: vec!["7p".into(); 4],
            from_who: -1,
            called_tile: None,
        });
        assert_eq!(dama_win(&kan, 0), Some(true));
    }

    #[test]
    fn a_mangan_either_way_keeps_auto_win_off() {
        // Tanyao, pinfu and four dora: 6m twice, 4s and 2m.
        let mut big = table(TANYAO);
        big.dora_markers = vec!["5m".into(), "3s".into(), "1m".into()];
        assert_eq!(dama_win(&big, 0), Some(false));
        // Two dora are a mangan by tsumo: tanyao, pinfu, tsumo and the two.
        big.dora_markers = vec!["5m".into()];
        assert_eq!(dama_win(&big, 0), Some(false));
    }

    #[test]
    fn the_hand_that_may_be_the_last_keeps_auto_win_off() {
        for (wind, kyoku) in [("E", 4), ("S", 4), ("W", 1)] {
            let mut late = table(TANYAO);
            late.bakaze = wind.into();
            late.kyoku = kyoku;
            assert_eq!(dama_win(&late, 0), Some(false), "{wind}{kyoku}");
        }
    }

    #[test]
    fn a_riichi_is_small_below_a_mangan_whichever_way_it_wins() {
        // Riichi, tanyao, pinfu: 3 han by ron, 4 by tsumo at 20 fu.
        let mut small = table(TANYAO);
        small.players[0].riichi_declared = true;
        assert_eq!(riichi_small(&small, 0), Some(true));

        // One dora more is a mangan by tsumo.
        let mut big = small.clone();
        big.dora_markers = vec!["3s".into()];
        assert_eq!(riichi_small(&big, 0), Some(false));

        // A hand not in riichi is not one to ask about.
        assert_eq!(riichi_small(&table(TANYAO), 0), None);
    }
}
