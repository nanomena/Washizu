//! What a person playing a hand dama has the client's own toggles do.
//!
//! A hand is played dama when it is closed (concealed kans aside), not in
//! riichi, ready, and wins by ron on every one of its waits: it has a yaku
//! without the riichi. Such a hand is settled, and a person sitting on one
//! often lets the client take its win and stops being offered calls:
//!
//! - **Auto-win**, while the hand is below a mangan whichever way it wins,
//!   and not in the hand that may be the game's last. A big hand is watched,
//!   and in the last hand where the game ends decides whether a win is wanted.
//! - **No calls**, unless the hand holds a pair a call would pay for: a pair
//!   of dora, or a pair of plain fives while that suit's red five is still to
//!   be seen.
//!
//! The game's length is not known here, so every fourth hand counts as one
//! that may be the last, and so does every hand of an extra round.
//!
//! Read on the hand at rest, after our discard. Four players only.

use crate::game_state::snapshot::{GameStateSnapshot, MeldKind};
use riichienv_core::hand_evaluator::HandEvaluator;
use riichienv_core::types::{Conditions, Meld, MeldType, WinResult, Wind};

/// The toggles the hand asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dama {
    /// Switch the client's auto-win on.
    pub win: bool,
    /// Have the client's no-calls on.
    pub no_calls: bool,
}

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

/// The kind a dora indicator points at.
fn dora_of(indicator: u8) -> u8 {
    match indicator {
        0..=26 => indicator - indicator % 9 + (indicator % 9 + 1) % 9,
        27..=30 => 27 + (indicator - 27 + 1) % 4,
        _ => 31 + (indicator - 31 + 1) % 3,
    }
}

/// A mangan or more: five han, or four with 40 fu, or three with 70.
fn mangan(win: &WinResult) -> bool {
    win.yakuman || win.han >= 5 || (win.han == 4 && win.fu >= 40) || (win.han == 3 && win.fu >= 70)
}

/// What `seat`'s hand asks of the toggles, or `None` when it is not at rest
/// (a tile drawn and not yet discarded) or cannot be read.
pub fn read(s: &GameStateSnapshot, seat: u8) -> Option<Dama> {
    let me = s.players.get(usize::from(seat))?;
    if s.num_players != 4
        || me.riichi_declared
        || me.melds.iter().any(|m| m.kind != MeldKind::Ankan)
    {
        return Some(Dama::default());
    }
    let hand: Vec<&str> = me.tehai.iter().map(String::as_str).collect();
    let kans: Vec<&str> = me
        .melds
        .iter()
        .flat_map(|m| m.tiles.iter().map(String::as_str))
        .collect();
    if hand.len() + me.melds.len() * 3 != 13 {
        return None;
    }
    let all = ids(&[hand.as_slice(), kans.as_slice()].concat())?;
    let (hand_ids, kan_ids) = all.split_at(hand.len());
    let melds = kan_ids
        .chunks(4)
        .map(|kan| Meld::new(MeldType::Ankan, kan.to_vec(), false, -1, None))
        .collect();
    let evaluator = HandEvaluator::new(hand_ids.to_vec(), melds);
    if !evaluator.is_tenpai() {
        return Some(Dama::default());
    }
    let waits = evaluator.get_waits_u8();
    let indicators: Vec<u8> = s
        .dora_markers
        .iter()
        .map(|m| kind(m).map(|k| k * 4))
        .collect::<Option<_>>()?;
    let player_wind = Wind::from((seat + 4 - s.oya % 4) % 4);
    let round_wind = Wind::from(match s.bakaze.as_str() {
        "S" => 1,
        "W" => 2,
        "N" => 3,
        _ => 0,
    });
    let win = |wait: u8, tsumo: bool| {
        // The wait's last copy: never a red five.
        evaluator.calc(
            wait * 4 + 3,
            indicators.clone(),
            vec![],
            Some(Conditions {
                tsumo,
                player_wind,
                round_wind,
                num_players: 4,
                ..Conditions::default()
            }),
        )
    };
    let dama = !waits.is_empty() && waits.iter().all(|&w| win(w, false).is_win);
    if !dama {
        return Some(Dama::default());
    }

    let big = waits
        .iter()
        .any(|&w| mangan(&win(w, false)) || mangan(&win(w, true)));
    let last = s.kyoku >= 4 || matches!(s.bakaze.as_str(), "W" | "N");

    let count = |k: u8| hand.iter().filter(|t| kind(t) == Some(k)).count();
    let dora_pair = indicators.iter().any(|i| count(dora_of(i / 4)) >= 2);
    let seen = |tile: &str| {
        s.dora_markers.iter().any(|t| t == tile)
            || s.players.iter().any(|p| {
                p.tehai.iter().any(|t| t == tile)
                    || p.river.iter().any(|d| d.tile == tile)
                    || p.melds.iter().any(|m| m.tiles.iter().any(|t| t == tile))
            })
    };
    let fives = ["5m", "5p", "5s"].iter().any(|five| {
        hand.iter().filter(|&&t| t == *five).count() >= 2 && !seen(&format!("{five}r"))
    });

    Some(Dama {
        win: !big && !last,
        no_calls: !dora_pair && !fives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_state::snapshot::{DiscardEntry, MeldSnapshot, Phase, PlayerSnapshot};

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

    const BOTH: Dama = Dama {
        win: true,
        no_calls: true,
    };

    /// All simples, waiting on 2s and 5s: tanyao either way, a small hand.
    const TANYAO: &str = "2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 4s 6m 6m";

    #[test]
    fn a_small_closed_hand_with_a_yaku_on_every_wait_has_both() {
        assert_eq!(read(&table(TANYAO), 0), Some(BOTH));
    }

    #[test]
    fn a_hand_with_a_drawn_tile_is_not_read() {
        assert_eq!(read(&table(&format!("{TANYAO} 9m")), 0), None);
    }

    #[test]
    fn riichi_an_open_hand_or_one_not_ready_has_neither() {
        let mut riichi = table(TANYAO);
        riichi.players[0].riichi_declared = true;
        assert_eq!(read(&riichi, 0), Some(Dama::default()));

        let mut open = table("2m 3m 4m 4p 5p 6p 3s 4s 6m 6m");
        open.players[0].melds.push(MeldSnapshot {
            kind: MeldKind::Pon,
            tiles: vec!["7s".into(); 3],
            from_who: 2,
            called_tile: Some("7s".into()),
        });
        assert_eq!(read(&open, 0), Some(Dama::default()));

        let apart = table("2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 9s 6m 1p");
        assert_eq!(read(&apart, 0), Some(Dama::default()));
    }

    #[test]
    fn a_wait_without_a_yaku_has_neither() {
        // 1s and 4s: no simples, the pair is the round's wind, no pinfu.
        assert_eq!(
            read(&table("1m 2m 3m 4p 5p 6p 7s 8s 9s 2s 3s E E"), 0),
            Some(Dama::default())
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
        assert_eq!(read(&kan, 0), Some(BOTH));
    }

    #[test]
    fn a_mangan_either_way_keeps_auto_win_off() {
        // Tanyao and four dora: 6m twice, 4s and 2m.
        let mut big = table(TANYAO);
        big.dora_markers = vec!["5m".into(), "3s".into(), "1m".into()];
        assert!(!read(&big, 0).unwrap().win);
    }

    #[test]
    fn the_hand_that_may_be_the_last_keeps_auto_win_off() {
        for (wind, kyoku) in [("E", 4), ("S", 4), ("W", 1)] {
            let mut late = table(TANYAO);
            late.bakaze = wind.into();
            late.kyoku = kyoku;
            assert_eq!(
                read(&late, 0),
                Some(Dama {
                    win: false,
                    no_calls: true
                }),
                "{wind}{kyoku}"
            );
        }
    }

    #[test]
    fn a_pair_of_dora_keeps_calls_on() {
        let mut dora = table(TANYAO);
        dora.dora_markers = vec!["5m".into()];
        assert!(!read(&dora, 0).unwrap().no_calls);
    }

    #[test]
    fn a_pair_of_plain_fives_keeps_calls_on_while_their_red_is_unseen() {
        let fives = "2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 4s 5m 5m";
        assert!(!read(&table(fives), 0).unwrap().no_calls);

        let mut gone = table(fives);
        gone.players[2].river.push(DiscardEntry {
            tile: "5mr".into(),
            tedashi: true,
            is_riichi: false,
            called: false,
        });
        assert_eq!(read(&gone, 0), Some(BOTH));

        let red = "2m 3m 4m 4p 5p 6p 6s 7s 8s 3s 4s 5m 5mr";
        assert_eq!(read(&table(red), 0), Some(BOTH));
    }

    #[test]
    fn indicators_point_to_the_next_kind() {
        assert_eq!(dora_of(kind("9m").unwrap()), kind("1m").unwrap());
        assert_eq!(dora_of(kind("N").unwrap()), kind("E").unwrap());
        assert_eq!(dora_of(kind("C").unwrap()), kind("P").unwrap());
        assert_eq!(dora_of(kind("5sr").unwrap()), kind("6s").unwrap());
    }
}
