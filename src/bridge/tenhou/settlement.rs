//! Public hand-end decoding and the ordered, sequential AGARI settlement.
//! No wall reconstruction or inference from another player's concealed hand.

use super::{state::State, tile::tenhou_to_mjai_one};
use crate::schema::MjaiEvent;
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;

fn csv<T: std::str::FromStr>(msg: &Value, key: &str) -> Result<Vec<T>> {
    let raw = msg
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing {key} CSV"))?;
    raw.split(',')
        .map(|part| {
            part.trim()
                .parse()
                .ok()
                .with_context(|| format!("invalid {key} CSV"))
        })
        .collect()
}

fn tiles(msg: &Value, key: &str) -> Result<Vec<u32>> {
    let ids = csv::<u32>(msg, key)?;
    ensure!(ids.iter().all(|id| *id < 136), "{key} tile outside 0..135");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    ensure!(
        unique.len() == ids.len(),
        "duplicate physical tile in {key}"
    );
    Ok(ids)
}

fn seat(msg: &Value, key: &str, state: &State) -> Result<u8> {
    let relative = msg
        .get(key)
        .and_then(|value| {
            value
                .as_str()
                .and_then(|v| v.parse::<u8>().ok())
                .or_else(|| value.as_u64().and_then(|v| u8::try_from(v).ok()))
        })
        .with_context(|| format!("missing {key} seat"))?;
    ensure!(relative < 4, "{key} seat out of range");
    let absolute = state.rel_to_abs(relative);
    ensure!(!state.is_ghost_abs(absolute), "{key} names the sanma ghost");
    Ok(absolute)
}

struct Payment {
    before: Vec<i32>,
    delta: Vec<i32>,
    after: Vec<i32>,
}

impl Payment {
    fn parse(msg: &Value, state: &State) -> Result<Self> {
        let sc = csv::<i32>(msg, "sc")?;
        ensure!(sc.len() == 8, "sc must contain four before/delta pairs");
        let mut payment = Self {
            before: vec![0; state.num_players as usize],
            delta: vec![0; state.num_players as usize],
            after: vec![0; state.num_players as usize],
        };
        for (relative, pair) in sc.as_chunks::<2>().0.iter().enumerate() {
            let absolute = state.rel_to_abs(relative as u8);
            if state.is_ghost_abs(absolute) {
                ensure!(*pair == [0, 0], "nonzero payment for sanma ghost");
                continue;
            }
            let seat = absolute as usize;
            payment.before[seat] = pair[0].checked_mul(100).context("sc before overflow")?;
            payment.delta[seat] = pair[1].checked_mul(100).context("sc delta overflow")?;
            payment.after[seat] = payment.before[seat]
                .checked_add(payment.delta[seat])
                .context("sc total overflow")?;
        }
        Ok(payment)
    }
}

struct Win {
    event: MjaiEvent,
    actor: u8,
    target: u8,
    tile: u32,
    payment: Payment,
}

impl Win {
    fn parse(msg: &Value, state: &State) -> Result<Self> {
        let actor = seat(msg, "who", state)?;
        let target = seat(msg, "fromWho", state)?;
        let tile = super::parse_u32(msg, "machi").context("missing machi")?;
        ensure!(tile < 136, "machi tile outside 0..135");
        let mut hand = tiles(msg, "hai")?;
        let won = hand
            .iter()
            .position(|id| *id == tile)
            .context("hai does not contain physical machi tile")?;
        hand.remove(won);
        ensure!(
            hand.len() <= 13 && hand.len() % 3 == 1,
            "winning closed hand has invalid size"
        );
        let payment = Payment::parse(msg, state)?;
        let ura_markers = if msg.get("doraHaiUra").is_some() {
            Some(
                tiles(msg, "doraHaiUra")?
                    .into_iter()
                    .map(tenhou_to_mjai_one)
                    .collect(),
            )
        } else {
            None
        };
        let event = MjaiEvent::Hora {
            actor,
            target,
            deltas: Some(payment.delta.clone()),
            ura_markers,
            pai: Some(tenhou_to_mjai_one(tile)),
            tehai: Some(hand.into_iter().map(tenhou_to_mjai_one).collect()),
            scores: None,
            score: None,
        };
        Ok(Self {
            event,
            actor,
            target,
            tile,
            payment,
        })
    }
}

/// A group is complete at NEXTREADY/INIT or the AGARI carrying owari.
/// Later AGARI sc baselines already include earlier winners' payments.
/// Poison the entire group on contradictory input; never publish a partial result.
#[derive(Default)]
pub(super) struct Wins {
    wins: Vec<Win>,
    invalid: bool,
}

impl Wins {
    pub fn is_pending(&self) -> bool {
        self.invalid || !self.wins.is_empty()
    }

    pub fn push(&mut self, msg: &Value, state: &State) -> Result<()> {
        let result = self.push_checked(msg, state);
        if result.is_err() {
            self.invalid = true;
        }
        result
    }

    fn push_checked(&mut self, msg: &Value, state: &State) -> Result<()> {
        ensure!(!self.invalid, "AGARI group was already invalidated");
        let win = Win::parse(msg, state)?;
        if let Some(previous) = self.wins.last() {
            ensure!(
                previous.actor != previous.target && win.actor != win.target,
                "multiple tsumo winners"
            );
            ensure!(
                previous.target == win.target && previous.tile == win.tile,
                "winners name different sources or physical tiles"
            );
            ensure!(
                previous.payment.after == win.payment.before,
                "AGARI sc baseline differs from previous settlement"
            );
            ensure!(
                !self.wins.iter().any(|prior| prior.actor == win.actor),
                "duplicate AGARI winner"
            );
        }
        self.wins.push(win);
        Ok(())
    }

    pub fn finish(&mut self) -> Vec<MjaiEvent> {
        let group = std::mem::take(self);
        if group.invalid || group.wins.is_empty() {
            return Vec::new();
        }
        let scores = group.wins.last().unwrap().payment.after.clone();
        let mut events = Vec::with_capacity(group.wins.len() + 1);
        for mut win in group.wins {
            if let MjaiEvent::Hora { scores: total, .. } = &mut win.event {
                *total = Some(scores.clone());
            }
            // Keep each provider payment once: history sums deltas, while
            // every winner supplies the same final scores to the tracker.
            events.push(win.event);
        }
        events.push(MjaiEvent::EndKyoku);
        events
    }
}

pub(super) fn draw(msg: &Value, state: &State) -> Result<MjaiEvent> {
    let reason = match msg
        .get("type")
        .map(|v| v.as_str().context("draw type is not a string"))
        .transpose()?
    {
        None => "exhaustive",
        Some("nm") => "nagashi",
        Some("yao9") => "kyuushu",
        Some("kaze4") => "four_winds",
        Some("reach4") => "four_riichis",
        Some("ron3") => "three_rons",
        Some("kan4") => "four_kans",
        Some(other) => bail!("unknown RYUUKYOKU type {other}"),
    };
    let payment = Payment::parse(msg, state)?;
    let mut hands = vec![None; state.num_players as usize];
    for relative in 0..4 {
        let key = format!("hai{relative}");
        if msg.get(&key).is_none() {
            continue;
        }
        let absolute = state.rel_to_abs(relative);
        ensure!(
            !state.is_ghost_abs(absolute),
            "revealed hand for sanma ghost"
        );
        hands[absolute as usize] = Some(
            tiles(msg, &key)?
                .into_iter()
                .map(tenhou_to_mjai_one)
                .collect(),
        );
    }
    // Web /3/ omits hai0: our concealed tiles are already known locally.
    // In an exhaustive draw, the actual noten settlement identifies whether
    // our hand is disclosed. Zero payment means all tenpai only if every
    // other real seat was disclosed. Do not invent unknown opponents' tiles.
    let own = state.seat as usize;
    let own_tenpai = payment.delta[own] > 0
        || (payment.delta.iter().all(|delta| *delta == 0)
            && hands
                .iter()
                .enumerate()
                .all(|(seat, hand)| seat == own || hand.is_some()));
    if reason == "exhaustive" && hands[own].is_none() && own_tenpai {
        ensure!(
            !state.hand.is_empty(),
            "missing own hand at exhaustive draw"
        );
        hands[own] = Some(state.hand.iter().copied().map(tenhou_to_mjai_one).collect());
    }
    Ok(MjaiEvent::Ryukyoku {
        deltas: Some(payment.delta),
        tehais: Some(hands),
        scores: Some(payment.after),
        reason: Some(reason.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_draw_omits_own_tiles_but_not_our_positive_tenpai_settlement() {
        // Real /3/ frame: observer absolute seat 3 gains 1000; hai0 is absent.
        let state = State {
            seat: 3,
            hand: vec![0, 1, 2, 4, 5, 6, 8],
            ..State::default()
        };
        let raw = json!({"tag":"RYUUKYOKU","ba":"0,0",
            "sc":"250,10,250,10,250,-30,250,10",
            "hai1":"46,55,89,91,94,98,102",
            "hai3":"41,42,43,48,52,92,95,116,117,118"});
        let MjaiEvent::Ryukyoku {
            tehais: Some(hands),
            deltas: Some(delta),
            ..
        } = draw(&raw, &state).unwrap()
        else {
            panic!("draw")
        };
        assert_eq!(delta, vec![1000, -3000, 1000, 1000]);
        assert_eq!(
            hands[3],
            Some(state.hand.iter().copied().map(tenhou_to_mjai_one).collect())
        );
        assert!(hands[1].is_none());
        assert!(hands[0].is_some() && hands[2].is_some());
    }

    #[test]
    fn zero_payment_uses_other_public_reveals_without_disclosing_a_noten_hand() {
        let state = State {
            seat: 0,
            hand: vec![0, 1, 2, 4, 5, 6, 8],
            ..State::default()
        };
        for all_tenpai in [false, true] {
            let mut raw = json!({"sc":"250,0,250,0,250,0,250,0"});
            if all_tenpai {
                for key in ["hai1", "hai2", "hai3"] {
                    raw[key] = json!("0,1,2,4,5,6,8");
                }
            }
            let MjaiEvent::Ryukyoku {
                tehais: Some(hands),
                ..
            } = draw(&raw, &state).unwrap()
            else {
                panic!("draw")
            };
            assert_eq!(hands[0].is_some(), all_tenpai);
        }
    }

    // Public double-ron fixture, with sequential sc baselines, from
    // kobalab/tenhou-log@03fc4a38/test/convlog.js (original protocol decoder).
    fn double_ron() -> [Value; 2] {
        [
            json!({"who":"3","fromWho":"2","machi":"52",
            "hai":"0,7,10,24,27,47,49,52,63,64,68",
            "sc":"382,0,120,0,88,-77,400,87"}),
            json!({"who":"0","fromWho":"2","machi":"52",
            "hai":"6,8,12,48,51,52,58,61,85,91,93",
            "sc":"382,39,120,0,11,-39,487,0"}),
        ]
    }

    #[test]
    fn double_ron_keeps_each_payment_once_and_shares_the_final_scores() {
        let state = State {
            seat: 1,
            ..State::default()
        };
        let mut group = Wins::default();
        for raw in double_ron() {
            group.push(&raw, &state).unwrap();
        }
        let events = group.finish();
        assert!(matches!(
            events.as_slice(),
            [
                MjaiEvent::Hora { .. },
                MjaiEvent::Hora { .. },
                MjaiEvent::EndKyoku
            ]
        ));
        for (index, event) in events[..2].iter().enumerate() {
            let MjaiEvent::Hora {
                actor,
                target,
                pai,
                tehai,
                deltas,
                scores,
                ..
            } = event
            else {
                panic!()
            };
            assert_eq!((*actor, *target), (index as u8, 3));
            assert_eq!(pai.as_deref(), Some("5pr"));
            assert_eq!(tehai.as_ref().unwrap().len(), 10);
            assert!(!tehai.as_ref().unwrap().contains(&"5pr".to_string()));
            assert_eq!(
                scores.as_deref(),
                Some([48700, 42100, 12000, -2800].as_slice())
            );
            assert_eq!(
                deltas.as_deref(),
                Some(
                    if index == 0 {
                        [8700, 0, 0, -7700]
                    } else {
                        [0, 3900, 0, -3900]
                    }
                    .as_slice()
                )
            );
        }
        assert!(group.finish().is_empty());
    }

    #[test]
    fn contradictory_second_win_invalidates_the_entire_group() {
        let state = State::default();
        for (key, value) in [
            ("who", json!(3)),
            ("fromWho", json!(1)),
            ("machi", json!(53)), // ordinary five is not the same physical red five
            ("sc", json!("382,39,120,0,88,-39,400,0")), // stale, pre-first-win baseline
        ] {
            let [first, mut second] = double_ron();
            second[key] = value;
            let mut group = Wins::default();
            group.push(&first, &state).unwrap();
            assert!(group.push(&second, &state).is_err(), "{key}");
            assert!(
                group.finish().is_empty(),
                "no partial winner may escape: {key}"
            );
        }
    }

    #[test]
    fn malformed_tiles_and_payment_vectors_are_rejected_without_guessing() {
        for (key, value) in [
            ("hai", json!("0,7,10,24,27,47,49,53,63,64,68")),
            ("hai", json!("0,7,10,24,27,47,49,52,52,64,68")),
            ("hai", json!("0,7,10,24,27,47,49,52,136,64,68")),
            ("sc", json!("382,0,120,0,88,-77,400")),
            ("sc", json!("382,0,invalid,0,88,-77,400,87")),
            ("sc", json!("2147483647,0,120,0,88,-77,400,87")),
            ("who", json!(256)),
            ("doraHaiUra", json!("45,bad")),
        ] {
            let mut raw = double_ron()[0].clone();
            raw[key] = value;
            assert!(Win::parse(&raw, &State::default()).is_err(), "{key}: {raw}");
        }
        for key in ["who", "fromWho", "hai", "machi", "sc"] {
            let mut raw = double_ron()[0].clone();
            raw.as_object_mut().unwrap().remove(key);
            assert!(
                Win::parse(&raw, &State::default()).is_err(),
                "missing {key}"
            );
        }
    }

    #[test]
    fn draw_kinds_reveals_and_score_totals_follow_the_wire_rotation() {
        let state = State {
            seat: 2,
            ..State::default()
        };
        for (kind, expected) in [
            (None, "exhaustive"),
            (Some("nm"), "nagashi"),
            (Some("yao9"), "kyuushu"),
            (Some("kaze4"), "four_winds"),
            (Some("reach4"), "four_riichis"),
            (Some("ron3"), "three_rons"),
            (Some("kan4"), "four_kans"),
        ] {
            let mut raw = json!({"sc":"250,15,250,-5,250,-5,250,-5", "hai0":"16,17,52,53,88,89,108,109,112,113,116,117,120"});
            if let Some(kind) = kind {
                raw["type"] = json!(kind);
            }
            let MjaiEvent::Ryukyoku {
                tehais: Some(hands),
                scores,
                deltas,
                reason,
            } = draw(&raw, &state).unwrap()
            else {
                panic!()
            };
            assert_eq!(reason.as_deref(), Some(expected));
            assert_eq!(scores, Some(vec![24500, 24500, 26500, 24500]));
            assert_eq!(deltas, Some(vec![-500, -500, 1500, -500]));
            assert!(hands[0].is_none() && hands[1].is_none() && hands[3].is_none());
            let own = hands[2].as_ref().unwrap();
            assert_eq!(&own[..6], &["5mr", "5m", "5pr", "5p", "5sr", "5s"]);
        }
        let no_tenpai = draw(&json!({"sc":"250,0,250,0,250,0,250,0"}), &state).unwrap();
        assert!(
            matches!(no_tenpai, MjaiEvent::Ryukyoku { tehais: Some(hands), .. } if hands == vec![None;4])
        );
        assert!(draw(
            &json!({"sc":"250,0,250,0,250,0,250,0", "type":"unknown"}),
            &state
        )
        .is_err());
        assert!(draw(
            &json!({"sc":"250,0,250,0,250,0,250,0", "hai1":"16,16"}),
            &state
        )
        .is_err());
    }

    #[test]
    fn sanma_omits_the_rotated_ghost_and_rejects_its_payment_or_reveal() {
        let state = State {
            seat: 1,
            hand: vec![0, 1, 2, 4, 5, 6, 8],
            is_3p: true,
            num_players: 3,
            ..State::default()
        };
        let raw = json!({"sc":"350,30,350,-15,0,0,350,-15"});
        let MjaiEvent::Ryukyoku { scores, .. } = draw(&raw, &state).unwrap() else {
            panic!()
        };
        assert_eq!(scores, Some(vec![33500, 38000, 33500]));
        for (key, value) in [
            ("sc", json!("350,30,350,-15,1,0,350,-15")),
            ("hai2", json!("16")),
        ] {
            let mut bad = raw.clone();
            bad[key] = value;
            assert!(draw(&bad, &state).is_err());
        }
    }
}
