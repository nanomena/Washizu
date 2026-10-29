//! REINIT normalization into a site-independent public position.
use super::*;
use crate::schema::restore::{KyokuRestore, RestoreMeld, RestoreMeldKind, RestorePlayer};
use anyhow::{ensure, Result};

pub(super) fn snapshot(state: &State, msg: &JsonValue) -> Result<KyokuRestore> {
    ensure!(
        state.num_players == 4,
        "The deployed v3 model requires four players"
    );
    let seed = parse_csv_i32(msg, "seed");
    let ten = parse_csv_i32(msg, "ten");
    ensure!(
        seed.len() >= 6 && ten.len() == 4,
        "Incomplete REINIT round metadata"
    );
    ensure!(
        seed[0] >= 0
            && seed[0] < 16
            && seed[1] >= 0
            && seed[1] <= 255
            && seed[2] >= 0
            && seed[2] <= 255,
        "Invalid REINIT round metadata"
    );
    let mut players: [RestorePlayer; 4] = std::array::from_fn(|_| RestorePlayer::default());
    for rel in 0..4u8 {
        let seat = state.rel_to_abs(rel) as usize;
        for t in parse_csv_u32(msg, &format!("kawa{rel}")) {
            match t {
                255 => players[seat].riichi = true,
                254 => {} // Missing tags are not treated as tedashi proof.
                0..=135 => players[seat].river.push(tenhou_to_mjai_one(t)),
                _ => anyhow::bail!("Invalid REINIT river tile"),
            }
        }
        // Native REINIT lists newest melds first; slots are normalized once.
        for bits in parse_csv_u32(msg, &format!("m{rel}")).into_iter().rev() {
            ensure!(
                bits & 0x3f != 0x20,
                "Nukidora is not a four-player restore meld"
            );
            let m = Meld::parse(bits);
            ensure!(
                m.tiles.iter().all(|t| *t < 136),
                "Invalid restored meld tile"
            );
            let closed = m.kind == MeldKind::Ankan;
            let kind = match m.kind {
                MeldKind::Chi => RestoreMeldKind::Chi,
                MeldKind::Pon => RestoreMeldKind::Pon,
                MeldKind::Ankan => RestoreMeldKind::Ankan,
                MeldKind::Daiminkan => RestoreMeldKind::Daiminkan,
                MeldKind::Kakan => RestoreMeldKind::Kakan,
            };
            let called = if m.kind == MeldKind::Kakan {
                m.tiles.get(1).copied()
            } else {
                m.tiles.first().copied()
            };
            players[seat].melds.push(RestoreMeld {
                kind,
                tiles: tenhou_to_mjai(&m.tiles),
                target: (!closed).then_some((seat as u8 + m.target_rel) % 4),
                called: (!closed).then(|| tenhou_to_mjai_one(called.unwrap())),
            });
        }
    }
    let mut scores = [0; 4];
    for rel in 0..4u8 {
        scores[state.rel_to_abs(rel) as usize] = ten[rel as usize] * 100;
    }
    let oya = parse_u8(msg, "oya")
        .filter(|s| *s < 4)
        .ok_or_else(|| anyhow::anyhow!("Invalid REINIT dealer"))?;
    let result = KyokuRestore {
        seat: state.seat,
        bakaze: BAKAZE[(seed[0] / 4) as usize].into(),
        kyoku: (seed[0] % 4 + 1) as u8,
        honba: seed[1] as u8,
        kyotaku: seed[2] as u8,
        oya: state.rel_to_abs(oya),
        scores,
        hand: tenhou_to_mjai(&state.hand),
        dora_markers: seed[5..]
            .iter()
            .map(|t| tenhou_to_mjai_one(*t as u32))
            .collect(),
        players,
    };
    crate::game_state::restore::build(&result)?;
    Ok(result)
}
