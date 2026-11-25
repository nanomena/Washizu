//! A reconnect snapshot (`OnChongxinLianjieChenggong`) as a public position.
//!
//! A client that reconnects mid-hand gets the hand as a snapshot instead of
//! its history: the field, and per seat the score, the hand (ours in full,
//! the others' as `x1` per concealed tile) with its calls after commas, and
//! the river. That maps onto [`KyokuRestore`] as it is, with no history: no
//! draws, call timing or discard order across seats is made up.
//!
//! Each river entry is the discard as it was sent — `_` for a tsumogiri,
//! `*`/`@` on the riichi declaration — and a discard that was called keeps
//! its place with the caller's direction mark appended (`"p1-"`). The
//! restore's river leaves called tiles out (they live in the melds), so the
//! marked entries are instead matched one for one against the callers'
//! melds: a call without its discard, or a marked discard without its call,
//! rejects the snapshot. So does anything else the snapshot does not say
//! consistently (a riichi flag without its declaration, a tile count off),
//! and a three-player table, which [`KyokuRestore`] has no form for (kita
//! and the shorter wall). The bridge reports a rejected snapshot as
//! `interrupted_kyoku`.

use super::{
    kyotaku, seat_of,
    tile::{self, Meld},
    BAKAZE,
};
use crate::schema::restore::{KyokuRestore, RestoreMeld, RestoreMeldKind, RestorePlayer};
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value as JsonValue;

/// Who sits where in the snapshot's hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Table {
    /// mjai actor of each wire seat.
    pub actor_of: [Option<u8>; 4],
    pub my_seat: u8,
    pub my_actor: u8,
    pub hand_no: u8,
}

/// The seats of `body` (`[_, our_wind, server_time, field, players, zimo,
/// deadline]`, each player `[account, seat, wind, score, chip, hand, river,
/// wins, riichi_type, kita]`). `known_seat` is our seat from the game start,
/// when this connection saw it.
pub(super) fn table(body: &JsonValue, n: u8, known_seat: Option<u8>) -> Result<Table> {
    let my_wind = body
        .get(1)
        .and_then(JsonValue::as_u64)
        .and_then(|w| u8::try_from(w).ok())
        .filter(|&w| w < n)
        .context("the snapshot has no valid wind of ours")?;
    let hand_no = body
        .get(3)
        .and_then(|f| f.get(1))
        .and_then(JsonValue::as_u64)
        .and_then(|h| u8::try_from(h).ok())
        .filter(|&h| h < n)
        .context("the snapshot has no valid hand number")?;
    let players = players(body)?;
    ensure!(
        players.len() == n as usize,
        "the snapshot lists {} players at a {n}-player table",
        players.len()
    );
    let mut actor_of = [None; 4];
    let mut winds_seen = [false; 4];
    for p in players {
        let seat = p.get(1).and_then(seat_of).context("a player has no seat")?;
        let wind = p
            .get(2)
            .and_then(JsonValue::as_u64)
            .and_then(|w| u8::try_from(w).ok())
            .filter(|&w| w < n)
            .context("a player has no valid wind")?;
        ensure!(
            !std::mem::replace(&mut winds_seen[wind as usize], true),
            "two players share a wind"
        );
        ensure!(
            actor_of[seat as usize]
                .replace((wind + hand_no) % n)
                .is_none(),
            "two players share a seat"
        );
    }
    let my_actor = (my_wind + hand_no) % n;
    let my_seat = (0..4u8)
        .find(|&seat| actor_of[seat as usize] == Some(my_actor))
        .context("no player has our wind")?;
    if let Some(known) = known_seat {
        ensure!(
            known == my_seat,
            "the snapshot puts us at seat {my_seat}, the game start at seat {known}"
        );
    }
    Ok(Table {
        actor_of,
        my_seat,
        my_actor,
        hand_no,
    })
}

/// The position in `body`, checked by the same rules every restore goes
/// through ([`crate::game_state::restore::build`]).
pub(super) fn snapshot(body: &JsonValue, table: &Table, n: u8) -> Result<KyokuRestore> {
    ensure!(
        n == 4,
        "a three-player position has no restore form (kita and the three-player wall)"
    );
    let actor = |seat: u8| table.actor_of.get(seat as usize).copied().flatten();
    let field = body.get(3).context("the snapshot has no field")?;
    let bakaze = field
        .get(0)
        .and_then(JsonValue::as_u64)
        .and_then(|w| BAKAZE.get(usize::try_from(w).ok()?))
        .context("the snapshot has no valid round wind")?;
    let honba = field
        .get(2)
        .and_then(JsonValue::as_u64)
        .and_then(|h| u8::try_from(h).ok())
        .context("the snapshot has no valid honba")?;
    let sticks = field
        .get(3)
        .and_then(kyotaku)
        .context("the snapshot has no valid riichi sticks")?;
    let dora_markers = field
        .get(4)
        .and_then(JsonValue::as_array)
        .and_then(|d| {
            d.iter()
                .map(|t| t.as_str().and_then(tile::tile))
                .collect::<Option<Vec<_>>>()
        })
        .filter(|d| !d.is_empty())
        .context("the snapshot has no valid dora indicators")?;

    let mut scores = [0i32; 4];
    let mut hand = None;
    let mut out: [RestorePlayer; 4] = std::array::from_fn(|_| RestorePlayer::default());
    // Called discards: (discarding actor, calling actor, tile).
    let mut called: Vec<(u8, u8, String)> = Vec::new();
    let mut holding = 0;
    for p in players(body)? {
        let seat = p.get(1).and_then(seat_of).context("a player has no seat")?;
        let a = actor(seat).context("a player has no seat")?;
        let who = format!("seat {seat}");
        scores[a as usize] = p
            .get(3)
            .and_then(JsonValue::as_i64)
            .and_then(|s| i32::try_from(s).ok())
            .with_context(|| format!("{who} has no valid score"))?;

        let shown = p
            .get(5)
            .and_then(JsonValue::as_str)
            .with_context(|| format!("{who} has no hand"))?;
        let mut parts = shown.split(',');
        let concealed = parts.next().unwrap_or("");
        let concealed_count = if a == table.my_actor {
            let tiles = tile::hand(concealed).context("our hand is not fully shown")?;
            let count = tiles.len();
            hand = Some(tiles);
            count
        } else {
            ensure!(
                concealed
                    .replace("x1", "")
                    .chars()
                    .all(|c| matches!(c, '*' | '@')),
                "{who}'s hidden hand is not a tile count"
            );
            concealed.matches("x1").count()
        };
        let player = &mut out[a as usize];
        for part in parts.filter(|part| !part.is_empty()) {
            ensure!(part.len() != 2, "{who} has a kita tile at four players");
            let meld = tile::meld(part).with_context(|| format!("{who} has an unreadable meld"))?;
            player.melds.push(restore_meld(meld, seat, &actor)?);
        }
        let in_hand = concealed_count + 3 * player.melds.len();
        ensure!(
            (13..=14).contains(&in_hand),
            "{who} holds {in_hand} tiles counting calls"
        );
        holding += usize::from(in_hand == 14);

        let river = p
            .get(6)
            .and_then(JsonValue::as_array)
            .with_context(|| format!("{who} has no river"))?;
        for entry in river {
            let (d, caller) = entry
                .as_str()
                .and_then(river_entry)
                .with_context(|| format!("{who} has an unreadable river tile"))?;
            if d.riichi {
                ensure!(!player.riichi, "{who} declared riichi twice");
                player.riichi = true;
                player.riichi_at = Some(player.river.len());
            }
            match caller {
                Some(from) => {
                    // The mark is the caller's view of this seat, on 4-seat
                    // geometry: the caller sits `offset` seats before it.
                    let by = actor((seat + 4 - from.offset()) % 4)
                        .with_context(|| format!("{who}'s river names an empty seat"))?;
                    called.push((a, by, d.pai));
                }
                None => player.river.push(d.pai),
            }
        }
        let riichi_type = p
            .get(8)
            .and_then(JsonValue::as_u64)
            .with_context(|| format!("{who} has no riichi state"))?;
        ensure!(
            player.riichi == (riichi_type != 0),
            "{who}'s riichi state and river disagree"
        );
    }
    ensure!(holding <= 1, "{holding} players hold a drawn tile");

    // Every open call takes one marked discard of its source, and every
    // marked discard was taken by one call.
    for (a, player) in out.iter().enumerate() {
        for m in &player.melds {
            let (Some(target), Some(tile)) = (m.target, &m.called) else {
                continue;
            };
            let i = called
                .iter()
                .position(|(from, by, t)| *from == target && *by == a as u8 && t == tile)
                .with_context(|| format!("a call by actor {a} has no called discard"))?;
            called.swap_remove(i);
        }
    }
    if let Some((from, by, _)) = called.first() {
        bail!("actor {from}'s river shows a call by actor {by} that is not among its melds");
    }

    let kans: usize = out
        .iter()
        .flat_map(|p| &p.melds)
        .filter(|m| m.tiles.len() == 4)
        .count();
    // Each kan reveals one indicator; an open kan's waits for its discard.
    ensure!(
        dora_markers.len() == kans + 1 || dora_markers.len() == kans,
        "{} dora indicators for {kans} kan(s)",
        dora_markers.len()
    );

    let position = KyokuRestore {
        seat: table.my_actor,
        bakaze: bakaze.to_string(),
        kyoku: table.hand_no + 1,
        honba,
        kyotaku: sticks,
        oya: table.hand_no,
        scores,
        hand: hand.context("the snapshot has no hand of ours")?,
        dora_markers,
        players: out,
    };
    crate::game_state::restore::build(&position)?;
    Ok(position)
}

fn players(body: &JsonValue) -> Result<&Vec<JsonValue>> {
    body.get(4)
        .and_then(JsonValue::as_array)
        .context("the snapshot lists no players")
}

/// A river entry, `tile[_][*|@][+|=|-]`: the discard and the mark of the
/// seat that called it, if one did.
fn river_entry(s: &str) -> Option<(tile::Discard, Option<tile::Source>)> {
    let (body, from) = match s.char_indices().last()? {
        (i, '+') => (&s[..i], Some(tile::Source::Next)),
        (i, '=') => (&s[..i], Some(tile::Source::Opposite)),
        (i, '-') => (&s[..i], Some(tile::Source::Previous)),
        _ => (s, None),
    };
    Some((tile::discard(body)?, from))
}

/// A meld of `seat`; an open one's source is its mark, resolved on 4-seat
/// geometry like the live calls.
fn restore_meld(meld: Meld, seat: u8, actor: &impl Fn(u8) -> Option<u8>) -> Result<RestoreMeld> {
    let source = |from: tile::Source| {
        actor((seat + from.offset()) % 4).context("a meld names an empty seat as its source")
    };
    Ok(match meld {
        Meld::Chi {
            from,
            pai,
            consumed,
        } => open(RestoreMeldKind::Chi, source(from)?, pai, consumed),
        Meld::Pon {
            from,
            pai,
            consumed,
        } => open(RestoreMeldKind::Pon, source(from)?, pai, consumed),
        Meld::Daiminkan {
            from,
            pai,
            consumed,
        } => open(RestoreMeldKind::Daiminkan, source(from)?, pai, consumed),
        Meld::Kakan {
            from,
            pai,
            consumed,
        } => RestoreMeld {
            kind: RestoreMeldKind::Kakan,
            target: Some(source(from)?),
            called: Some(consumed[2].clone()),
            tiles: consumed.into_iter().chain([pai]).collect(),
        },
        Meld::Ankan { consumed } => RestoreMeld {
            kind: RestoreMeldKind::Ankan,
            tiles: consumed.to_vec(),
            target: None,
            called: None,
        },
    })
}

fn open<const N: usize>(
    kind: RestoreMeldKind,
    target: u8,
    pai: String,
    consumed: [String; N],
) -> RestoreMeld {
    RestoreMeld {
        kind,
        target: Some(target),
        called: Some(pai.clone()),
        tiles: [pai].into_iter().chain(consumed).collect(),
    }
}
