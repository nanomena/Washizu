//! ホロじゃん / HoloMahjong protocol bridge. Observe-only.
//!
//! The battle server is a MagicOnion StreamingHub reached over gRPC on
//! cleartext HTTP/2; the capture proxy relays and taps that connection and
//! hands this bridge one hub message per call (the gRPC message body, after
//! its 5-byte length prefix). [`hub`] decodes the envelope; this module turns
//! the server's broadcasts into mjai events. Tile, hand and meld strings
//! follow majiang-core's notation — see [`tile`].
//!
//! Bodies are MessagePack-CSharp objects serialized as arrays indexed by
//! their `[Key(n)]`. Event DTOs start at key 1, so index 0 is always nil;
//! nested structs start at key 0. The layouts in the comments below are
//! those keys.
//!
//! Seats on the wire are fixed for the whole game, but how the server numbers
//! them (a 3-player table in particular) is not assumed. mjai actor 0 is the
//! first dealer and actors follow turn order, so each deal maps a seat to
//! `(its wind + hand number) mod players` — winds follow turn order whatever
//! the seat numbers are. Likewise the target of a call is the seat that made
//! the discard being called, not a reading of the meld's direction mark.
//!
//! A reconnect's snapshot becomes `restore_kyoku` (see [`restore`]) or, when
//! it cannot be imported as it is, `interrupted_kyoku`; nothing of the hand's
//! history is made up.

pub mod hub;
mod restore;
pub mod tile;

use super::{shown, Bridge, Direction, ParseResult};
use crate::{
    autoplay::{budget::SharedTimeBudget, BudgetSource, TimeBudget},
    bot::control::InputWindow,
    config::Platform,
    logger::{FlowLogger, Session},
    schema::{mjai::Tile, MjaiEvent, ParsedFrame},
};
use chrono::Local;
use hub::HubMessage;
use serde_json::{json, Value as JsonValue};
use std::{sync::Arc, time::Instant};
use tile::Meld;
use tracing::{debug, info, warn};

const LOG: &str = "akagi::bridge::holomahjong";
const BAKAZE: [&str; 4] = ["E", "S", "W", "N"];
const HAND_SIZE: usize = 13;

pub struct HoloMahjongBridge {
    /// Our seat, from `OnKaiju` (or the deal, when we joined mid-game).
    my_seat: Option<u8>,
    /// Display names by wire seat, from `OnKaiju`.
    names: [String; 4],
    /// 3 or 4, from `OnKaiju`'s rule (or the first deal seen).
    num_players: Option<u8>,
    /// mjai actor of each wire seat; set by every deal.
    actor_of: [Option<u8>; 4],
    /// This hand's number within the round wind; `PingjuEvent` reports
    /// players by wind, and `wind + hand_no` is their actor.
    hand_no: u8,
    /// Seat that made the last discard while it can still be called.
    last_discard: Option<u8>,
    /// `start_game` has gone out and no `end_game` yet.
    game_open: bool,
    /// Inside a hand: between `start_kyoku` (or `restore_kyoku`) and
    /// `end_kyoku`.
    kyoku_open: bool,
    /// This connection saw the game from its first message (`OnKaiju`) and
    /// every hand since. A reconnect's game is not a complete record, so its
    /// end goes out as terminated, as the Tenhou bridge does for a rejoin.
    complete: bool,
    /// The decision the server is waiting on us for, from the clock on its
    /// last draw, discard or call (see [`window_change`]).
    window: Option<TimeBudget>,
    /// The account's shared time-budget slot, mirrored from `window` so the
    /// decision coordinator can tell a current window from a closed one.
    time_budget: Option<SharedTimeBudget>,
    flow_log: Option<Arc<FlowLogger>>,
    session: Option<Arc<Session>>,
    mjai_log: Option<Arc<FlowLogger>>,
}

impl HoloMahjongBridge {
    pub fn new(flow_log: Option<Arc<FlowLogger>>, session: Option<Arc<Session>>) -> Self {
        Self {
            my_seat: None,
            names: Default::default(),
            num_players: None,
            actor_of: [None; 4],
            hand_no: 0,
            last_discard: None,
            game_open: false,
            kyoku_open: false,
            complete: false,
            window: None,
            time_budget: None,
            flow_log,
            session,
            mjai_log: None,
        }
    }

    pub fn with_time_budget(mut self, slot: Option<SharedTimeBudget>) -> Self {
        self.time_budget = slot;
        self
    }

    /// Follow the server's clock: a broadcast that opens, keeps or closes a
    /// decision of ours updates `window` and the shared slot. A connection
    /// that never learned our seat owns no decision and writes nothing.
    fn follow_clock(&mut self, msg: &HubMessage, now: Instant) {
        if self.my_seat.is_none() {
            return;
        }
        let Some(window) = window_change(msg.method, &msg.body, now) else {
            return;
        };
        self.window = window;
        if let Some(slot) = &self.time_budget {
            if let Ok(mut guard) = slot.write() {
                *guard = window;
            }
        }
    }

    fn actor(&self, seat: u8) -> Option<u8> {
        self.actor_of.get(seat as usize).copied().flatten()
    }

    fn players(&self) -> u8 {
        self.num_players.unwrap_or(4)
    }

    /// Per-seat values (`[[seat, value, …], …]`, any order) as a vector in
    /// actor order. `seat_at`/`value_at` index into each entry.
    fn by_actor(&self, entries: &JsonValue, seat_at: usize, value_at: usize) -> Option<Vec<i32>> {
        let mut out = vec![0; self.players() as usize];
        for e in entries.as_array()? {
            let seat = seat_of(e.get(seat_at)?)?;
            *out.get_mut(self.actor(seat)? as usize)? =
                i32::try_from(e.get(value_at)?.as_i64()?).ok()?;
        }
        Some(out)
    }

    fn dispatch(&mut self, msg: &HubMessage) -> Vec<MjaiEvent> {
        let b = &msg.body;
        let events = match msg.method {
            hub::ON_KAIJU => self.on_kaiju(b, true),
            // A reconnect opens with the game's KaijuEvent; the snapshot of
            // the hand in progress follows (`on_resume`).
            hub::ON_ZHUNBEI_CHONGLIAN if !self.game_open => {
                b.get(1).and_then(|k| self.on_kaiju(k, false))
            }
            hub::ON_QIPAI => self.on_qipai(b),
            hub::ON_CHONGXIN_LIANJIE_CHENGGONG => Some(self.on_resume(b)),
            hub::ON_ZIMO if self.kyoku_open => self.on_zimo(b),
            hub::ON_DAPAI if self.kyoku_open => self.on_dapai(b),
            hub::ON_FULOU if self.kyoku_open => self.on_fulou(b),
            hub::ON_AN_GANG_ADD_GANG if self.kyoku_open => self.on_fulou(b),
            hub::ON_ADD_BAOPAI if self.kyoku_open => self.on_add_baopai(b),
            hub::ON_BABEI if self.kyoku_open => self.on_babei(b),
            hub::ON_LIZHI if self.kyoku_open => self.on_lizhi(b),
            hub::ON_HULE if self.kyoku_open => self.on_hule(b),
            hub::ON_PINGJU if self.kyoku_open => self.on_pingju(b),
            hub::ON_JIEJU if self.game_open => self.on_jieju(b),
            hub::ON_UPDATE_END_GAME_VOTE if self.game_open => Some(self.on_end_game_vote(b)),
            hub::ON_SYSTEM_CALL_EVENT if self.game_open => Some(self.on_system_call(b)),
            _ => Some(Vec::new()),
        };
        events.unwrap_or_else(|| {
            warn!(target: LOG, "unexpected {} body shape; skipped", msg.label());
            Vec::new()
        })
    }

    /// KaijuEvent `[_, game_info, seat, rule, timeout, players, extended_rules,
    /// default_field]`. `rule[1]` is the player count; each player is a
    /// MessagePack union `[0, [seat, account, detail, connected, is_ai]]`
    /// with `detail[1]` the display name. `from_start` is false when it
    /// comes inside a reconnect's `OnZhunbeiChonglian`.
    fn on_kaiju(&mut self, b: &JsonValue, from_start: bool) -> Option<Vec<MjaiEvent>> {
        let mut events = Vec::new();
        if self.game_open {
            // A game that ended without a result. Logged here: the reset
            // below drops that game's mjai log.
            let end = MjaiEvent::terminated_game();
            self.write_mjai(std::slice::from_ref(&end));
            events.push(end);
        }
        let n = b
            .get(3)
            .and_then(|r| r.get(1))
            .and_then(JsonValue::as_u64)
            .and_then(|n| u8::try_from(n).ok())
            .unwrap_or(4);
        if !(3..=4).contains(&n) {
            warn!(target: LOG, "{n}-player game is not supported; ignoring it");
        }
        let mut names: [String; 4] = Default::default();
        for p in b.get(5)?.as_array()? {
            let info = p.get(1)?;
            let seat = seat_of(info.get(0)?)? as usize;
            if let (Some(slot), Some(name)) = (
                names.get_mut(seat),
                info.get(2)
                    .and_then(|x| x.get(1))
                    .and_then(JsonValue::as_str),
            ) {
                *slot = name.to_string();
            }
        }
        *self = Self {
            my_seat: Some(seat_of(b.get(2)?)?),
            names,
            num_players: Some(n),
            complete: from_start,
            flow_log: self.flow_log.take(),
            session: self.session.take(),
            // The account's slot outlives a game; the window itself does not.
            time_budget: self.time_budget.take(),
            ..Self::new(None, None)
        };
        info!(
            target: LOG,
            "game {}: seat {:?}",
            if from_start { "opened" } else { "rejoined" },
            self.my_seat
        );
        Some(events)
    }

    /// QipaiEvent `[_, seat, server_time, deadline, field, players, timeout]`
    /// with field `[round_wind, hand_no, honba, riichi_sticks, dora_indicators,
    /// wall, …]` and each player `[account, seat, wind, score, chip, hand,
    /// discards, wins, riichi, kita]`.
    fn on_qipai(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let players = b.get(5)?.as_array()?;
        let n = match self.num_players {
            Some(n) => n,
            None => u8::try_from(players.len()).ok()?,
        };
        if !(3..=4).contains(&n) {
            return Some(Vec::new());
        }
        // Read and check the whole deal before touching any state, so a deal
        // we cannot read leaves the bridge exactly as it was.
        let field = b.get(4)?;
        let bakaze = BAKAZE
            .get(usize::try_from(field.get(0)?.as_u64()?).ok()?)?
            .to_string();
        let hand_no = u8::try_from(field.get(1)?.as_u64()?)
            .ok()
            .filter(|&h| h < n)?;
        let honba = u8::try_from(field.get(2)?.as_u64()?).ok()?;
        let kyotaku = kyotaku(field.get(3)?)?;
        let dora_marker = tile::tile(field.get(4)?.get(0)?.as_str()?)?;
        let me = match self.my_seat {
            Some(seat) => seat,
            None => seat_of(b.get(1)?)?,
        };
        if players.len() != n as usize {
            return None;
        }
        let mut actor_of = [None; 4];
        let mut winds_seen = [false; 4];
        let mut scores = vec![0; n as usize];
        let mut my_hand = None;
        for p in players {
            let seat = seat_of(p.get(1)?)?;
            let wind = u8::try_from(p.get(2)?.as_u64()?).ok().filter(|&w| w < n)?;
            if std::mem::replace(&mut winds_seen[wind as usize], true)
                || actor_of[seat as usize].is_some()
            {
                return None;
            }
            let actor = (wind + hand_no) % n;
            actor_of[seat as usize] = Some(actor);
            scores[actor as usize] = i32::try_from(p.get(3)?.as_i64()?).ok()?;
            if seat == me {
                my_hand = Some(tile::hand(p.get(5)?.as_str()?).filter(|h| h.len() == HAND_SIZE)?);
            }
        }
        let my_actor = actor_of[me as usize]?;
        let mut tehais = vec![vec!["?".to_string(); HAND_SIZE]; n as usize];
        tehais[my_actor as usize] = my_hand?;

        let mut events = Vec::new();
        if self.kyoku_open {
            // The last hand ended through something we could not read.
            events.push(MjaiEvent::EndKyoku);
        }
        if self.game_open && actor_of != self.actor_of {
            warn!(target: LOG, "seat order changed mid-game: {:?} -> {actor_of:?}", self.actor_of);
        }
        self.my_seat = Some(me);
        self.num_players = Some(n);
        self.actor_of = actor_of;
        self.hand_no = hand_no;
        self.last_discard = None;
        if !self.game_open {
            events.push(self.open_game(my_actor, n));
        }
        events.push(MjaiEvent::StartKyoku {
            bakaze,
            dora_marker,
            kyoku: hand_no + 1,
            honba,
            kyotaku,
            // The dealer's wind is 0.
            oya: hand_no,
            scores,
            tehais,
            num_players: n,
        });
        self.kyoku_open = true;
        Some(events)
    }

    /// ChongxinLianjieChenggongEvent `[_, our_wind, server_time, field,
    /// players, zimo, deadline]`: the hand in progress after a reconnect, as
    /// a snapshot. It goes out as `restore_kyoku` when [`restore`] can import
    /// it as it is, otherwise as `interrupted_kyoku`, and play is then
    /// followed from the next deal.
    fn on_resume(&mut self, b: &JsonValue) -> Vec<MjaiEvent> {
        if self.kyoku_open {
            // This connection watched the hand; nothing is missing.
            return Vec::new();
        }
        let n = match self.num_players {
            Some(n) => n,
            None => match b
                .get(4)
                .and_then(JsonValue::as_array)
                .and_then(|p| u8::try_from(p.len()).ok())
            {
                Some(n) => n,
                None => return vec![interrupted("the snapshot lists no players")],
            },
        };
        if !(3..=4).contains(&n) {
            return Vec::new();
        }
        let table = match restore::table(b, n, self.my_seat) {
            Ok(table) => table,
            Err(e) => {
                // Without the seats there is no game to name either.
                warn!(target: LOG, "reconnect: {e:#}");
                return vec![interrupted(&format!("{e:#}"))];
            }
        };
        self.my_seat = Some(table.my_seat);
        self.num_players = Some(n);
        self.actor_of = table.actor_of;
        self.hand_no = table.hand_no;
        // Who discarded last is not in a snapshot: a call before the next
        // discard is resolved by its mark.
        self.last_discard = None;
        // This hand's history before the snapshot was not seen.
        self.complete = false;
        let mut events = Vec::new();
        if !self.game_open {
            events.push(self.open_game(table.my_actor, n));
        }
        match restore::snapshot(b, &table, n) {
            Ok(snapshot) => {
                info!(target: LOG, "reconnect: restoring the hand in progress from its snapshot");
                events.push(MjaiEvent::RestoreKyoku {
                    snapshot: Box::new(snapshot),
                });
                self.kyoku_open = true;
            }
            Err(e) => {
                warn!(target: LOG, "reconnect: {e:#}; following play again at the next deal");
                events.push(interrupted(&format!("{e:#}")));
                self.kyoku_open = false;
            }
        }
        events
    }

    /// `start_game` for the game this connection follows, and a fresh mjai
    /// log for it.
    fn open_game(&mut self, my_actor: u8, n: u8) -> MjaiEvent {
        self.rotate_mjai_log();
        self.game_open = true;
        MjaiEvent::StartGame {
            names: self.names_by_actor(n),
            kyoku_first: None,
            aka_flag: None,
            id: Some(my_actor),
            num_players: n,
            game_meta: None,
        }
    }

    fn names_by_actor(&self, n: u8) -> Vec<String> {
        let mut names = vec![String::new(); n as usize];
        for (seat, actor) in self.actor_of.iter().enumerate() {
            if let Some(a) = actor {
                if let Some(slot) = names.get_mut(*a as usize) {
                    *slot = self.names[seat].clone();
                }
            }
        }
        names
    }

    /// ZimoEvent `[_, seat, tile, kan_offers, kita_offers, tsumo_offer,
    /// timeout]`; the tile is `""` for another seat's draw.
    fn on_zimo(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let actor = self.actor(seat_of(b.get(1)?)?)?;
        self.last_discard = None;
        let pai = match b.get(2)?.as_str()? {
            "" => "?".to_string(),
            t => tile::tile(t)?,
        };
        Some(vec![MjaiEvent::Tsumo { actor, pai }])
    }

    /// DapaiEvent `[_, seat, discard, chi_offers, pon_offers, kan_offers,
    /// ron_offer, call_advice, riichi_type, hand_after, timeout]`.
    fn on_dapai(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let seat = seat_of(b.get(1)?)?;
        let actor = self.actor(seat)?;
        let d = tile::discard(b.get(2)?.as_str()?)?;
        self.last_discard = Some(seat);
        let mut events = Vec::new();
        let riichi_type = b.get(8).and_then(JsonValue::as_u64).unwrap_or(0);
        if d.riichi || riichi_type != 0 {
            events.push(MjaiEvent::Reach {
                actor,
                pai: None,
                tsumogiri: None,
            });
        }
        events.push(MjaiEvent::Dahai {
            actor,
            pai: d.pai,
            tsumogiri: d.tsumogiri,
        });
        Some(events)
    }

    /// FulouEvent and AnGangAddGangEvent `[_, seat, kind, meld, …]`; the
    /// meld string alone says which call it is.
    fn on_fulou(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let seat = seat_of(b.get(1)?)?;
        let event = self.meld_event(seat, b.get(3)?.as_str()?)?;
        self.last_discard = None;
        Some(vec![event])
    }

    fn meld_event(&self, seat: u8, s: &str) -> Option<MjaiEvent> {
        let actor = self.actor(seat)?;
        // A call takes the discard just made. The direction mark is the
        // fallback: it counts seats around a 4-seat table, also at three
        // players (seat 3 empty), so it is resolved on seats, not actors.
        let last = self.last_discard;
        let target = |from: tile::Source| match last {
            Some(discarder) => self.actor(discarder),
            None => self.actor((seat + from.offset()) % 4),
        };
        Some(match tile::meld(s)? {
            Meld::Chi {
                from,
                pai,
                consumed,
            } => MjaiEvent::Chi {
                actor,
                target: target(from)?,
                pai,
                consumed,
            },
            Meld::Pon {
                from,
                pai,
                consumed,
            } => MjaiEvent::Pon {
                actor,
                target: target(from)?,
                pai,
                consumed,
            },
            Meld::Daiminkan {
                from,
                pai,
                consumed,
            } => MjaiEvent::Daiminkan {
                actor,
                target: target(from)?,
                pai,
                consumed,
            },
            Meld::Kakan { pai, consumed, .. } => MjaiEvent::Kakan {
                actor,
                pai,
                consumed,
            },
            Meld::Ankan { consumed } => MjaiEvent::Ankan { actor, consumed },
        })
    }

    /// BabeiEvent `[_, seat, tile, ron_offer, timeout]` — a North set aside
    /// at a 3-player table. The tile carries the discard marks: `"z4_"` when
    /// it is the tile just drawn.
    fn on_babei(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let actor = self.actor(seat_of(b.get(1)?)?)?;
        let pai = tile::discard(b.get(2)?.as_str()?)?.pai;
        self.last_discard = None;
        Some(vec![MjaiEvent::Kita {
            actor,
            pai: Some(pai),
        }])
    }

    /// LizhiEvent `[_, seat, riichi_type, riichi_sticks, scores_by_seat]` —
    /// sent once the riichi discard has passed, i.e. the stick is down.
    fn on_lizhi(&self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let actor = self.actor(seat_of(b.get(1)?)?)?;
        Some(vec![MjaiEvent::ReachAccepted { actor }])
    }

    /// HuleEvent `[_, results, scores, timeout]`; each result is `[seat,
    /// dealt_in_seat, hand, win_tile, fu, limit, han, yaku, points, chip, dora,
    /// ura_dora]` (`dealt_in_seat` nil on a tsumo) and scores `[[seat, delta,
    /// pao, chip], …]`, riichi sticks and honba included.
    ///
    /// The wire has one combined payment for all winners, so every `hora`
    /// carries it, as Mahjong Soul's do (history counts an exact repeat
    /// once). The yaku are named by display string, not a provider id, so no
    /// `HandScore` is attached.
    fn on_hule(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let combined = self.by_actor(b.get(2)?, 0, 1)?;
        let mut events = Vec::new();
        for h in b.get(1)?.as_array()? {
            let winner = seat_of(h.get(0)?)?;
            let dealt_in = match h.get(1)? {
                JsonValue::Null => winner,
                v => seat_of(v)?,
            };
            // The win tile carries the caller-relative mark of a ron.
            let pai = h
                .get(3)
                .and_then(JsonValue::as_str)
                .and_then(|s| tile::tile(s.get(..2)?));
            // Disclosed only when it is the closed hand without the win tile.
            let tehai = h
                .get(2)
                .and_then(JsonValue::as_str)
                .and_then(tile::hand)
                .filter(|t| t.len() <= HAND_SIZE && t.len() % 3 == 1);
            let ura: Vec<Tile> = h
                .get(11)
                .and_then(JsonValue::as_array)
                .map(|v| v.iter().filter_map(|t| tile::tile(t.as_str()?)).collect())
                .unwrap_or_default();
            events.push(MjaiEvent::Hora {
                actor: self.actor(winner)?,
                target: self.actor(dealt_in)?,
                deltas: Some(combined.clone()),
                ura_markers: (!ura.is_empty()).then_some(ura),
                pai,
                tehai,
                scores: None,
                score: None,
            });
        }
        if events.is_empty() {
            return None;
        }
        events.push(MjaiEvent::EndKyoku);
        self.kyoku_open = false;
        Some(events)
    }

    /// PingjuEvent `[_, kind, players, timeout]`; each player is `[_, wind,
    /// delta, hand, nagashi]` — by wind, not seat. `hand` is the hand shown
    /// at the draw, empty when it is not shown.
    fn on_pingju(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let n = self.players();
        let mut deltas = vec![0; n as usize];
        let mut tehais = vec![None; n as usize];
        for p in b.get(2)?.as_array()? {
            let wind = u8::try_from(p.get(1)?.as_u64()?).ok().filter(|&w| w < n)?;
            let actor = ((wind + self.hand_no) % n) as usize;
            deltas[actor] = i32::try_from(p.get(2)?.as_i64()?).ok()?;
            tehais[actor] = p
                .get(3)
                .and_then(JsonValue::as_str)
                .filter(|s| !s.is_empty())
                .and_then(tile::hand);
        }
        self.kyoku_open = false;
        Some(vec![
            MjaiEvent::Ryukyoku {
                deltas: Some(deltas),
                tehais: Some(tehais),
                scores: None,
                reason: None,
            },
            MjaiEvent::EndKyoku,
        ])
    }

    /// AddBaopaiEvent `[_, dora_indicator]` — after a kan, forwarded where
    /// the server sends it.
    fn on_add_baopai(&self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let dora_marker = tile::tile(b.get(1)?.as_str()?)?;
        Some(vec![MjaiEvent::Dora { dora_marker }])
    }

    /// JiejuEvent `[_, players, timeout, result]`; each player is `[_, seat,
    /// rank, score, points, …]`.
    fn on_jieju(&mut self, b: &JsonValue) -> Option<Vec<MjaiEvent>> {
        let n = self.players() as usize;
        let (mut scores, mut ranks) = (vec![0; n], vec![0; n]);
        for p in b.get(1)?.as_array()? {
            let a = self.actor(seat_of(p.get(1)?)?)? as usize;
            *ranks.get_mut(a)? = u8::try_from(p.get(2)?.as_u64()?).ok()?;
            *scores.get_mut(a)? = i32::try_from(p.get(3)?.as_i64()?).ok()?;
        }
        self.game_open = false;
        self.kyoku_open = false;
        Some(vec![if self.complete {
            MjaiEvent::confirmed_game(Some(scores), Some(ranks))
        } else {
            MjaiEvent::terminated_game()
        }])
    }

    /// UpDateEndGameVoteEvent `[_, end_result, agreements]`; `end_result` 0
    /// means the table agreed to stop, which ends the game without a result.
    fn on_end_game_vote(&mut self, b: &JsonValue) -> Vec<MjaiEvent> {
        if b.get(1).and_then(JsonValue::as_u64) != Some(0) {
            return Vec::new();
        }
        self.game_open = false;
        self.kyoku_open = false;
        vec![MjaiEvent::terminated_game()]
    }

    /// SystemCallEvent `[type, message]` (a struct, so keys start at 0); type
    /// 1 is TerminateBattle, the server ending the game.
    fn on_system_call(&mut self, b: &JsonValue) -> Vec<MjaiEvent> {
        if b.get(0).and_then(JsonValue::as_u64) != Some(1) {
            return Vec::new();
        }
        self.game_open = false;
        self.kyoku_open = false;
        vec![MjaiEvent::terminated_game()]
    }

    /// Open a fresh `holomahjong_<ts>.mjai.jsonl` per game. No-op without a
    /// session (tests).
    fn rotate_mjai_log(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let ts = Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
        let file_name = format!("holomahjong_{ts}.mjai.jsonl");
        match session.flow_logger(
            Platform::HoloMahjong.subdir(),
            &file_name,
            format!("holomahjong mjai {ts}"),
        ) {
            Ok(log) => {
                info!(target: LOG, "opened mjai log {file_name}");
                self.mjai_log = Some(log);
            }
            Err(e) => {
                warn!(target: LOG, "failed to open mjai log {file_name}: {e:#}");
                self.mjai_log = None;
            }
        }
    }

    fn write_mjai(&self, events: &[MjaiEvent]) {
        let Some(log) = &self.mjai_log else { return };
        for ev in events {
            match serde_json::to_string(ev) {
                Ok(line) => log.writeln(&line),
                Err(e) => warn!(target: LOG, "failed to serialize MjaiEvent: {e:#}"),
            }
        }
    }

    fn log_frame(&self, direction: Direction, len: usize, msg: Option<(&HubMessage, bool)>) {
        let Some(log) = &self.flow_log else { return };
        let ts = Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let line = match msg {
            Some((msg, shareable)) => json!({
                "ts": ts,
                "dir": direction.as_str(),
                "method": msg.label(),
                "msg_id": msg.message_id,
                "len": len,
                "payload": if shareable { msg.body.clone() } else { json!(crate::bridge::WITHHELD) },
            }),
            None => json!({
                "ts": ts,
                "dir": direction.as_str(),
                "method": JsonValue::Null,
                "len": len,
                "payload": crate::bridge::WITHHELD,
            }),
        };
        log.writeln(&line.to_string());
    }
}

/// How a broadcast changes the decision the server waits on us for.
///
/// Draws, discards and calls end with the server's clock,
/// `[opened_ms, deadline_ms, fixed_ms, bank_ms]`; the deadline is zero when
/// this client has nothing to answer (another seat's draw, our own discard).
/// A non-zero deadline opens a window with the base time and what is left
/// of the bank; whether our seat really has a choice in it is the tracker's
/// to say. The end of a hand or game closes it. The timers on the deal, the
/// game start and the results are for their screens, not for a decision,
/// and anything else leaves the window as it was (`None`).
fn window_change(method: i64, body: &JsonValue, now: Instant) -> Option<Option<TimeBudget>> {
    let source = match method {
        hub::ON_ZIMO => BudgetSource::DealTile,
        hub::ON_DAPAI => BudgetSource::DiscardTile,
        hub::ON_FULOU | hub::ON_AN_GANG_ADD_GANG => BudgetSource::ChiPengGang,
        hub::ON_BABEI => BudgetSource::BaBei,
        hub::ON_KAIJU
        | hub::ON_QIPAI
        | hub::ON_HULE
        | hub::ON_PINGJU
        | hub::ON_JIEJU
        | hub::ON_UPDATE_END_GAME_VOTE
        | hub::ON_SYSTEM_CALL_EVENT
        | hub::ON_CHONGXIN_LIANJIE_CHENGGONG => return Some(None),
        _ => return None,
    };
    let clock = body
        .as_array()
        .and_then(|b| b.last())
        .and_then(JsonValue::as_array)
        .filter(|c| c.len() == 4)
        .and_then(|c| c.iter().map(|v| v.as_i64()).collect::<Option<Vec<i64>>>());
    let Some(clock) = clock else {
        warn!(target: LOG, "a {} without the server's clock; no decision window", hub::method_name(method).unwrap_or("message"));
        return Some(None);
    };
    if clock[1] <= 0 {
        return Some(None);
    }
    let (Ok(fixed_ms), Ok(add_ms)) = (u32::try_from(clock[2]), u32::try_from(clock[3])) else {
        warn!(target: LOG, "a clock with out-of-range times {clock:?}; no decision window");
        return Some(None);
    };
    Some(Some(TimeBudget {
        fixed_ms,
        add_ms,
        opened_at: now,
        source,
    }))
}

/// A seat index; anything past a 4-player table is rejected.
fn seat_of(v: &JsonValue) -> Option<u8> {
    v.as_u64()
        .and_then(|s| u8::try_from(s).ok())
        .filter(|&s| s < 4)
}

/// mjai `kyotaku` (1000-point sticks) from LizhiBangStruct, which counts
/// sticks of 100 / 500 / 1000 / 5000 / 10000 points.
fn kyotaku(sticks: &JsonValue) -> Option<u8> {
    const VALUES: [u64; 5] = [100, 500, 1000, 5000, 10000];
    let counts = sticks.as_array()?;
    let points = VALUES.iter().zip(counts).try_fold(0u64, |sum, (v, c)| {
        sum.checked_add(c.as_u64()?.checked_mul(*v)?)
    })?;
    u8::try_from(points / 1000).ok()
}

fn interrupted(why: &str) -> MjaiEvent {
    MjaiEvent::InterruptedKyoku {
        reason: format!("HoloMahjong reconnect snapshot not restored: {why}"),
    }
}

/// Messages whose content may go into the shareable logs. `JoinRoomAsync`
/// carries the account and battle-session ids, `OnKaiju`,
/// `OnZhunbeiChonglian` and the reconnect snapshot every player's account id
/// and name, and `OnJieju` the result screen's account data, so none of them
/// is on it.
fn is_shareable(msg: &HubMessage) -> bool {
    if msg.method == hub::ON_QIPAI {
        // Each player carries an account slot; it has only been seen nil.
        return msg
            .body
            .get(5)
            .and_then(JsonValue::as_array)
            .is_some_and(|ps| ps.iter().all(|p| p.get(0).is_some_and(JsonValue::is_null)));
    }
    matches!(
        msg.method,
        hub::ON_ZIMO
            | hub::ON_DAPAI
            | hub::ON_FULOU
            | hub::ON_LIZHI
            | hub::ON_HULE
            | hub::ON_PINGJU
            | hub::ON_UPDATE_FIELD_INFO
            | hub::ON_UPDATE_PLAYER_INFO
            | hub::ON_END_GAME_VOTE_START
            | hub::ON_UPDATE_END_GAME_VOTE
            | hub::ON_RESPONSE_END_GAME_VOTE
            | hub::ON_AN_GANG_ADD_GANG
            | hub::ON_ADD_BAOPAI
            | hub::ON_HUANG_PAI
            | hub::ON_BABEI
            | hub::ON_SYSTEM_CALL_EVENT
            | hub::READY
            | hub::KAIJU_EVENT_COMPLETE
            | hub::QIPAI_EVENT_COMPLETE
            | hub::SELECT_DAPAI
            | hub::SELECT_FULOU
            | hub::SELECT_ACTION_SKIP
            | hub::SELECT_HULE
            | hub::SELECT_AN_GANG_ADD_GANG
            | hub::SELECT_DAOPAI
            | hub::RESULT_COMPLETE
            | hub::RESUME_CONTROL
            | hub::END_GAME_VOTE_START
    )
}

impl Bridge for HoloMahjongBridge {
    fn parse(&mut self, direction: Direction, content: &[u8]) -> ParseResult {
        let Some(msg) = hub::decode(content) else {
            debug!(target: LOG, "{} non-hub message, {} bytes", direction.as_str(), content.len());
            self.log_frame(direction, content.len(), None);
            return ParseResult::empty();
        };
        let shareable = is_shareable(&msg);
        debug!(
            target: LOG,
            "{} {} {}",
            direction.as_str(),
            msg.label(),
            shown(shareable, &msg.body)
        );
        self.log_frame(direction, content.len(), Some((&msg, shareable)));
        // Only broadcasts drive play; requests, responses and our own calls
        // are recorded but carry nothing the broadcasts don't.
        let events = if direction == Direction::Down && msg.message_id.is_none() {
            let events = self.dispatch(&msg);
            self.follow_clock(&msg, Instant::now());
            events
        } else {
            Vec::new()
        };
        self.write_mjai(&events);
        let method = match msg.message_id {
            Some(_) if direction == Direction::Down => format!("{} (response)", msg.label()),
            _ => msg.label(),
        };
        ParseResult {
            window: self.window.map(InputWindow::HoloMahjong),
            events,
            parsed: Some(ParsedFrame {
                method,
                args: msg.body,
            }),
            shareable,
        }
    }

    fn build(&mut self, _command: &MjaiEvent) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests;
