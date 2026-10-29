//! Snapshot legality uses only public registers and the known viewer's hand.
//! No draws or chronological moves are synthesized. Unknown scoring flags
//! never authorize a win: original provider offers supply that constraint.

use crate::schema::restore::{KyokuRestore, RestoreMeldKind};
use anyhow::{ensure, Context, Result};
use riichienv_core::{
    parser::mjai_to_tid,
    rule::GameRule,
    state::GameState,
    types::{Meld, MeldType},
};

pub fn build(snapshot: &KyokuRestore) -> Result<GameState> {
    ensure!(
        snapshot.seat < 4 && snapshot.oya < 4 && (1..=4).contains(&snapshot.kyoku),
        "Invalid restore seat/round"
    );
    let wind = ["E", "S", "W", "N"]
        .iter()
        .position(|w| *w == snapshot.bakaze)
        .context("Invalid restore wind")?;
    ensure!(
        (1..=5).contains(&snapshot.dora_markers.len()),
        "Invalid restore indicators"
    );
    let tile =
        |name: &str| mjai_to_tid(name).with_context(|| format!("Invalid restored tile {name}"));
    let mut state = GameState::new(0, true, Some(0), wind as u8, GameRule::default_tenhou());
    state.wall.tiles.clear(); // No hidden wall is part of an observation.
    state.wall.dora_indicators = snapshot
        .dora_markers
        .iter()
        .map(|s| tile(s))
        .collect::<Result<_>>()?;
    state.round_wind = wind as u8;
    state.kyoku_idx = snapshot.kyoku - 1;
    state.oya = snapshot.oya;
    state.honba = snapshot.honba;
    state.riichi_sticks = u32::from(snapshot.kyotaku);
    state.active_players.clear();
    state.drawn_tile = None;
    state.last_discard = None;
    state.is_first_turn = false; // A snapshot does not prove an uninterrupted first turn.
    let mut visible = Vec::new();
    let mut taken = 0i32;
    for (seat, observed) in snapshot.players.iter().enumerate() {
        let p = &mut state.players[seat];
        p.reset_round();
        p.hand.clear(); // Never allocate an opponent's concealed tiles.
        p.score = snapshot.scores[seat];
        ensure!(
            p.score % 100 == 0 && observed.melds.len() <= 4,
            "Invalid restore score/meld count"
        );
        p.riichi_declared = observed.riichi;
        p.discards = observed
            .river
            .iter()
            .map(|s| tile(s))
            .collect::<Result<_>>()?;
        visible.extend(p.discards.iter().copied());
        taken += p.discards.len() as i32;
        for m in &observed.melds {
            let kind = match m.kind {
                RestoreMeldKind::Chi => MeldType::Chi,
                RestoreMeldKind::Pon => MeldType::Pon,
                RestoreMeldKind::Daiminkan => MeldType::Daiminkan,
                RestoreMeldKind::Ankan => MeldType::Ankan,
                RestoreMeldKind::Kakan => MeldType::Kakan,
            };
            let tiles = m
                .tiles
                .iter()
                .map(|s| tile(s))
                .collect::<Result<Vec<_>>>()?;
            let closed = m.kind == RestoreMeldKind::Ankan;
            ensure!(
                tiles.len()
                    == if matches!(m.kind, RestoreMeldKind::Chi | RestoreMeldKind::Pon) {
                        3
                    } else {
                        4
                    },
                "Invalid restore meld width"
            );
            ensure!(
                closed == m.target.is_none() && closed == m.called.is_none(),
                "Invalid restore claim identity"
            );
            ensure!(
                m.target.is_none_or(|s| s < 4 && s as usize != seat),
                "Invalid restore claim seat"
            );
            let called = m.called.as_deref().map(tile).transpose()?;
            ensure!(
                called.is_none_or(|t| tiles.contains(&t)),
                "Restored called tile is not in its meld"
            );
            visible.extend(tiles.iter().copied());
            taken += tiles.len() as i32 - 3;
            p.melds.push(Meld::new(
                kind,
                tiles,
                !closed,
                m.target.map_or(-1, |s| s as i8),
                called,
            ));
        }
    }
    // A called tile still belongs to its source's discard set for furiten,
    // but no former river position or cross-seat action order is asserted.
    for observed in &snapshot.players {
        for m in &observed.melds {
            if let (Some(source), Some(name)) = (m.target, &m.called) {
                state.players[source as usize].discards.push(tile(name)?);
            }
        }
    }
    let own = &mut state.players[snapshot.seat as usize];
    own.hand = snapshot
        .hand
        .iter()
        .map(|s| tile(s))
        .collect::<Result<_>>()?;
    ensure!(
        (13..=14).contains(&(own.hand.len() + 3 * own.melds.len())),
        "Invalid restored concealed hand count"
    );
    taken += (own.hand.len() + 3 * own.melds.len()) as i32 - 13;
    visible.extend(own.hand.iter().copied());
    visible.extend(state.wall.dora_indicators.iter().copied());
    let mut counts = [0u8; 34];
    let mut reds = [0u8; 3];
    for t in visible {
        counts[(t / 4) as usize] += 1;
        if let Some(i) = [16, 52, 88].iter().position(|r| *r == t) {
            reds[i] += 1;
        }
    }
    ensure!(
        counts.iter().all(|n| *n <= 4) && reds.iter().all(|n| *n <= 1),
        "Restored visible inventory exceeds a real deck"
    );
    ensure!((0..=70).contains(&taken), "Invalid restored wall count");
    state.wall.drawable_count = (70 - taken) as u8;
    Ok(state)
}
