//! Majsoul protocol bridge.
//!
//! Wire format (see `parser.rs` for full layout):
//!   `[type byte] [msg_id u16 LE?] [Wrapper protobuf] ...`
//!
//! Type byte: 1=Notify, 2=Request, 3=Response. Request/Response carry a
//! little-endian u16 message id at offset 1..3; Notify does not.
//!
//! Each WebSocket flow has its own id sequence and pending request map, so
//! create one `MajsoulBridge` per connection.

pub mod parser;
pub mod tenhou_log;
pub mod tile;

use super::{Bridge, Direction, ParseResult};
use crate::{
    autoplay::budget::{BudgetSource, SharedTimeBudget, TimeBudget},
    config::Platform,
    event_bus::AccountBus,
    logger::{FlowLogger, Session},
    schema::{mjai::Actor, AccountLevels, GameMeta, Level, MatchInfo, MjaiEvent},
};
use anyhow::{bail, Context, Result};
use chrono::{Local, Utc};
use parser::{LiqiParser, MessageType, ParsedMessage};
use serde_json::{json, Value as JsonValue};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tile::{compare_pai, ms_to_mjai};
use tracing::{info, warn};

const METHOD_AUTH_GAME: &str = ".lq.FastTest.authGame";
const METHOD_ACTION_PROTOTYPE: &str = ".lq.ActionPrototype";
const METHOD_NOTIFY_GAME_END_RESULT: &str = ".lq.NotifyGameEndResult";
const METHOD_NOTIFY_GAME_TERMINATE: &str = ".lq.NotifyGameTerminate";
/// Reconnect replay: both responses carry a `GameRestore { actions[] }` of the
/// current kyoku. See `handle_game_restore`.
const METHOD_SYNC_GAME: &str = ".lq.FastTest.syncGame";
/// The two uplink calls that mean "the client accepted an input": every
/// discard, call, win and pass goes out as one of them. Watched so autoplay
/// can tell a click that registered from one the UI swallowed.
const METHOD_INPUT_OPERATION: &str = ".lq.FastTest.inputOperation";
const METHOD_CONFIRM_NEW_ROUND: &str = ".lq.FastTest.confirmNewRound";
const METHOD_INPUT_CHI_PENG_GANG: &str = ".lq.FastTest.inputChiPengGang";
/// `timeuse` the client stamps on an action it took by itself rather than
/// on one the player made — a decision window that ran out, or an auto
/// setting. Observed as exactly this value on both input methods.
const TIMEUSE_CLIENT_AUTO: u64 = 1_000_000;
const METHOD_ENTER_GAME: &str = ".lq.FastTest.enterGame";
/// The lobby's ways of logging in. Each answers with the account, ranks and all.
const METHODS_LOGIN: [&str; 3] = [
    ".lq.Lobby.oauth2Login",
    ".lq.Lobby.login",
    ".lq.Lobby.emailLogin",
];
/// An account's profile. The client asks for its own after every game, and
/// for other players' when their profile is opened.
const METHOD_FETCH_ACCOUNT_INFO: &str = ".lq.Lobby.fetchAccountInfo";
/// The lobby call a replay opens with; its answer is the whole game record.
const METHOD_FETCH_GAME_RECORD: &str = ".lq.Lobby.fetchGameRecord";
const ACTION_NEW_ROUND: &str = "ActionNewRound";
const ACTION_DEAL_TILE: &str = "ActionDealTile";
const ACTION_DISCARD_TILE: &str = "ActionDiscardTile";
const ACTION_CHI_PENG_GANG: &str = "ActionChiPengGang";
const ACTION_AN_GANG_ADD_GANG: &str = "ActionAnGangAddGang";
const ACTION_HULE: &str = "ActionHule";
const ACTION_NO_TILE: &str = "ActionNoTile";
const ACTION_LIU_JU: &str = "ActionLiuJu";
const ACTION_BA_BEI: &str = "ActionBaBei";

// ChiPengGang.type
const CHI_PENG_GANG_CHI: u64 = 0;
const CHI_PENG_GANG_PENG: u64 = 1;
const CHI_PENG_GANG_GANG: u64 = 2;

// AnGangAddGang.type
const AN_GANG_ADD_GANG_AN: u64 = 3;
const AN_GANG_ADD_GANG_ADD: u64 = 2;
const TEHAI_SIZE: usize = 13;
const TSUMO_TEHAI_SIZE: usize = 14;
const UNKNOWN_TILE: &str = "?";

/// Our dealer's undivided opening deal, projected onto MJAI's 13 + draw.
/// Keep exact tile identities (including red fives), not just tile kinds.
struct OpeningDeal {
    hand: Vec<String>,
    draw: String,
}

impl OpeningDeal {
    fn discard_is_tsumogiri(&self, tile: &str, wire: bool) -> bool {
        tile == self.draw && (wire || !self.hand.iter().any(|held| held == tile))
    }

    fn normalize_win(
        &self,
        pai: &mut Option<String>,
        tehai: &mut Option<Vec<String>>,
    ) -> Result<()> {
        let mut revealed = tehai.clone().context("opening win missing hand")?;
        revealed.push(pai.clone().context("opening win missing hu_tile")?);
        let mut dealt = self.hand.clone();
        dealt.push(self.draw.clone());
        revealed.sort();
        dealt.sort();
        if revealed != dealt {
            bail!("opening winning tiles differ from the fourteen dealt tiles");
        }
        // The provider can designate any of the fourteen as hu_tile. The
        // MJAI lane must finish on the same synthetic draw it started with.
        *pai = Some(self.draw.clone());
        *tehai = Some(self.hand.clone());
        Ok(())
    }
}

/// Per-flow Majsoul state. Holds the liqi parser and the game state mirror
/// needed to emit mjai events.
///
/// `account_id` is captured from the outbound `authGame` request; `seat` is
/// resolved on the matching response by indexing into `seat_list`. Once the
/// seat is known the bridge emits `start_game` with `id = seat` so downstream
/// bots know which seat is theirs.
///
/// `session`, when supplied, lets the bridge rotate a fresh
/// `<session>/majsoul/majsoul_<ts>.mjai.jsonl` file every time a new
/// `start_game` event is emitted. Each subsequent emitted `MjaiEvent` is
/// appended as one JSON line to that file.
/// Dora-flip scheduling for kan declarations. Mjai distinguishes ankan
/// (即乗り — dora flips *before* the rinshan tsumo) from kakan/daiminkan
/// (後乗り — dora flips *after* the rinshan tsumo, just before the next
/// dahai).
///
/// On the wire the live server matches this split: an ankan's new marker
/// arrives with the rinshan `ActionDealTile.doras`, while a kakan /
/// daiminkan marker is withheld until the kan declarer commits a tile —
/// it rides the grown `doras` array of that `ActionDiscardTile` (or
/// `ActionBaBei` in 3p). This state only schedules markers that arrive
/// *early* (i.e. already present on the rinshan deal, which older
/// captures suggested for open kans): `PendingAfterRinshan` holds them
/// back so they still flip just before the commit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoraTiming {
    /// Ankan just declared. The next `ActionDealTile` carries the new dora
    /// marker — emit `dora` *before* the rinshan `tsumo`.
    PendingBeforeRinshan,
    /// Kakan or daiminkan just declared. The next `ActionDealTile` still
    /// emits `tsumo` first; any marker it already carries is held back and
    /// flipped just before the next `dahai`.
    PendingAfterRinshan,
}

pub struct MajsoulBridge {
    parser: LiqiParser,
    flow_log: Option<Arc<FlowLogger>>,
    session: Option<Arc<Session>>,
    mjai_log: Option<Arc<FlowLogger>>,
    account_id: Option<u64>,
    game_id: Option<u64>,
    /// Raw `ReqAuthGame.game_uuid` — the paifu identifier, persisted into
    /// history's `MatchInfo` so the frontend can link to the replay.
    /// `game_id` above stays the FNV hash used for reconnect detection.
    game_uuid: Option<String>,
    seat: Option<Actor>,
    /// Mjai-mapped dora indicators seen so far this kyoku. Compared
    /// against the `doras` array that any action payload may carry
    /// (`ActionDealTile`, `ActionDiscardTile`, `ActionAnGangAddGang`,
    /// `ActionBaBei`) to detect newly revealed markers.
    doras: Vec<String>,
    /// Pending kan-dora timing, set by meld actions and consumed by
    /// `build_tsumo` / `build_dahai`.
    dora_timing: Option<DoraTiming>,
    /// Mjai-mapped dora markers held for emission immediately before the
    /// next commit event — `dahai`, or `kita` in 3p (kakan / daiminkan
    /// flow).
    deferred_doras: Vec<String>,
    /// Seat of the actor whose most recent action *revealed* a tile that
    /// another seat could ron — i.e. the legitimate `target` for a hora.
    /// Updated by:
    ///
    /// - `ActionDiscardTile` — normal ron.
    /// - `ActionAnGangAddGang(kakan)` — 搶槓 (chankan).
    /// - `ActionAnGangAddGang(ankan)` — 国士無双 robbing an ankan
    ///   (Majsoul-specific rule; only valid for kokushi musou).
    /// - `ActionBaBei` (3p) — 胡拔北; will be wired when 3p support lands.
    ///
    /// `HuleInfo` doesn't carry the target seat itself, so without this
    /// tracking a ron on a kan / babei would attribute the win to the
    /// wrong player. Reset at every `start_kyoku`.
    last_revealed_tile_actor: Option<Actor>,
    /// Seat that just declared riichi via `ActionDiscardTile.is_liqi`.
    /// Drained as a `reach_accepted` event prepended to the *next* action
    /// (per mjai state machine: declaration tile must pass through before
    /// the seat is debited 1000 points). Cleared without emission if the
    /// next action is `ActionHule` — a ron on the declaration tile voids
    /// the riichi.
    pending_reach_accepted: Option<Actor>,
    /// Present only until our dealer commits the opening deal. Majsoul
    /// has fourteen dealt tiles; MJAI requires a thirteen-tile hand and a
    /// synthetic draw. Discards and heavenly wins use that same partition.
    opening_deal: Option<OpeningDeal>,
    /// 3 (sanma) or 4 (yonma). Resolved from `seat_list.len()` on the
    /// `authGame` response. Defaults to 4 so per-flow code that runs before
    /// auth (none today, but be defensive) sees a sane value.
    num_players: u8,
    /// Shared slot for the server's per-decision-window time budget
    /// (`operation.time_fixed` / `time_add`, both ms). `None` when the
    /// autoplay context isn't wired (MITM path, tests).
    time_budget: Option<SharedTimeBudget>,
    /// True while `handle_game_restore` replays historical actions through
    /// `handle_action_prototype`. Replayed operations must not clobber the
    /// live budget slot — the last one is committed after the replay with
    /// `passed_waiting_time` folded in.
    replaying: bool,
    /// The budget carried by the most recent replayed action (if its
    /// operation was addressed to our seat). Committed by
    /// `handle_game_restore` once the replay finishes.
    restore_budget: Option<TimeBudget>,
    /// Counter of the client's own uplink input commands, shared with the
    /// autoplay manager for click verification (see `autoplay::verify`).
    /// `None` when the autoplay context isn't wired (MITM path, tests).
    input_watch: Option<crate::autoplay::verify::SharedInputWatch>,
    /// Where our own ranks go when the server says them. `None` when no one
    /// listens (tests).
    account_bus: Option<AccountBus>,
    /// Where an opened replay is written as a tenhou.net/6 log (see
    /// `tenhou_log`). `None` leaves replays alone (tests, offline tools that
    /// do not ask for it).
    replay_dir: Option<PathBuf>,
}

impl MajsoulBridge {
    pub fn new(flow_log: Option<Arc<FlowLogger>>, session: Option<Arc<Session>>) -> Self {
        Self {
            parser: LiqiParser::new(),
            flow_log,
            session,
            mjai_log: None,
            account_id: None,
            game_id: None,
            game_uuid: None,
            seat: None,
            doras: Vec::new(),
            dora_timing: None,
            deferred_doras: Vec::new(),
            last_revealed_tile_actor: None,
            pending_reach_accepted: None,
            opening_deal: None,
            num_players: 4,
            time_budget: None,
            replaying: false,
            restore_budget: None,
            input_watch: None,
            account_bus: None,
            replay_dir: None,
        }
    }

    /// Install the folder opened replays are exported to (builder-style;
    /// used by `bridge::for_platform`).
    pub fn with_replay_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.replay_dir = dir;
        self
    }

    /// Write the game record a replay just fetched as a tenhou.net/6 log and
    /// NAGA's order-form lines. Failures are logged and go no further: the
    /// replay itself plays on regardless.
    fn export_replay(&self, payload: &JsonValue) {
        let Some(dir) = &self.replay_dir else { return };
        match tenhou_log::export(dir, payload) {
            Ok(out) => info!(
                target: "akagi::bridge::majsoul",
                "replay {} exported: {} rounds, {}",
                out.uuid,
                out.rounds,
                out.naga.display()
            ),
            Err(e) => warn!(
                target: "akagi::bridge::majsoul",
                "replay not exported: {e:#}"
            ),
        }
    }

    /// Install the shared time-budget slot (builder-style; used by
    /// `bridge::for_platform`).
    pub fn with_time_budget(mut self, slot: Option<SharedTimeBudget>) -> Self {
        self.time_budget = slot;
        self
    }

    /// Install the shared input-command counter (builder-style; used by
    /// `bridge::for_platform`).
    pub fn with_input_watch(
        mut self,
        watch: Option<crate::autoplay::verify::SharedInputWatch>,
    ) -> Self {
        self.input_watch = watch;
        self
    }

    /// Install the bus our own ranks are said on (builder-style; used by
    /// `bridge::for_platform`).
    pub fn with_account_bus(mut self, bus: Option<AccountBus>) -> Self {
        self.account_bus = bus;
        self
    }

    /// Say our own ranks, from an account's or a seated player's `level` and
    /// `level3`, if it has either.
    fn publish_account(&self, account: &JsonValue) {
        let Some(bus) = &self.account_bus else { return };
        if let Some(levels) = account_levels(account) {
            let _ = bus.send(levels);
        }
    }

    /// Open a fresh `majsoul_<ts>.mjai.jsonl` in the platform subdir and
    /// install it as the current mjai log. No-op when no session is wired.
    fn rotate_mjai_log(&mut self) {
        let Some(session) = &self.session else { return };
        let ts = Local::now().format("%Y%m%d-%H%M%S%.3f").to_string();
        let file_name = format!("majsoul_{ts}.mjai.jsonl");
        let label = format!("majsoul mjai {ts}");
        match session.flow_logger(Platform::Majsoul.subdir(), &file_name, label) {
            Ok(log) => {
                info!(
                    target: "akagi::bridge::majsoul",
                    "opened mjai log {file_name}"
                );
                self.mjai_log = Some(log);
            }
            Err(e) => {
                warn!(
                    target: "akagi::bridge::majsoul",
                    "failed to open mjai log {file_name}: {e:#}"
                );
                self.mjai_log = None;
            }
        }
    }

    /// Append every `MjaiEvent` in `events` as a JSON line to the current
    /// mjai log (if any).
    fn write_mjai(&self, events: &[MjaiEvent]) {
        let Some(log) = &self.mjai_log else { return };
        for ev in events {
            match serde_json::to_string(ev) {
                Ok(line) => log.writeln(&line),
                Err(e) => warn!(
                    target: "akagi::bridge::majsoul",
                    "failed to serialize MjaiEvent: {e:#}"
                ),
            }
        }
    }

    /// Translate `msg` into 0+ mjai events, mutating bridge state as needed.
    fn dispatch(&mut self, msg: &ParsedMessage) -> Vec<MjaiEvent> {
        match (&msg.msg_type, msg.method_name.as_ref()) {
            (MessageType::Request, METHOD_AUTH_GAME) => {
                self.account_id = msg.payload.get("account_id").and_then(JsonValue::as_u64);
                self.game_uuid = msg
                    .payload
                    .get("game_uuid")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string);
                self.game_id = self.game_uuid.as_deref().map(stable_game_id);
                if self.account_id.is_none() {
                    warn!(
                        target: "akagi::bridge::majsoul",
                        "authGame request missing account_id: {}",
                        msg.payload
                    );
                }
                Vec::new()
            }
            (MessageType::Response, METHOD_AUTH_GAME) => {
                // New game: whatever window the previous game left in the
                // budget slot is stale.
                self.store_budget(None);
                self.handle_auth_game_response(&msg.payload)
            }
            // The client only sends these once it has accepted an input,
            // so they are the proof an autoplay click landed. No mjai event
            // comes of them — the server's echo carries the game state.
            (MessageType::Request, METHOD_INPUT_OPERATION)
            | (MessageType::Request, METHOD_INPUT_CHI_PENG_GANG) => {
                if let Some(watch) = &self.input_watch {
                    if is_client_initiated(&msg.payload) {
                        watch.note_sent(input_kind(msg.method_name.as_ref(), &msg.payload));
                    }
                }
                Vec::new()
            }
            // The result of a hand confirmed: it is gone from the screen.
            (MessageType::Request, METHOD_CONFIRM_NEW_ROUND) => {
                if let Some(watch) = &self.input_watch {
                    watch.note_confirmed();
                }
                Vec::new()
            }
            (MessageType::Response, ".lq.Lobby.startUnifiedMatch")
            | (MessageType::Response, ".lq.Lobby.matchGame") => {
                if !msg
                    .payload
                    .get("error")
                    .is_some_and(|error| !error.is_null())
                {
                    if let Some(watch) = &self.input_watch {
                        watch.note_matching();
                    }
                }
                Vec::new()
            }
            (MessageType::Notify, METHOD_ACTION_PROTOTYPE) => self.handle_action_prototype(msg),
            (MessageType::Response, METHOD_SYNC_GAME)
            | (MessageType::Response, METHOD_ENTER_GAME) => self.handle_game_restore(&msg.payload),
            (MessageType::Notify, METHOD_NOTIFY_GAME_END_RESULT) => {
                info!(
                    target: "akagi::bridge::majsoul",
                    "game ended: {}", msg.payload
                );
                self.store_budget(None);
                let (final_scores, final_ranks) =
                    parse_game_end_standings(&msg.payload, self.num_players)
                        .map(|(scores, ranks)| (Some(scores), Some(ranks)))
                        .unwrap_or((None, None));
                let rank_points = parse_rank_points(&msg.payload, self.num_players);
                vec![MjaiEvent::confirmed_game(final_scores, final_ranks)
                    .with_rank_points(rank_points)]
            }
            // Logging in says who we are and where we stand.
            (MessageType::Response, method) if METHODS_LOGIN.contains(&method) => {
                if let Some(id) = msg.payload.get("account_id").and_then(JsonValue::as_u64) {
                    self.account_id = Some(id);
                }
                if let Some(account) = msg.payload.get("account") {
                    self.publish_account(account);
                }
                Vec::new()
            }
            // After a game the client asks for its own account again, with
            // the ranks that game left; other players' profiles come the same
            // way and are not ours to tell.
            (MessageType::Response, METHOD_FETCH_ACCOUNT_INFO) => {
                if let Some(account) = msg.payload.get("account").filter(|account| {
                    self.account_id.is_some()
                        && account.get("account_id").and_then(JsonValue::as_u64) == self.account_id
                }) {
                    self.publish_account(account);
                }
                Vec::new()
            }
            (MessageType::Notify, METHOD_NOTIFY_GAME_TERMINATE) => {
                info!(
                    target: "akagi::bridge::majsoul",
                    "game terminated: {}", msg.payload
                );
                self.store_budget(None);
                vec![MjaiEvent::terminated_game()]
            }
            _ => Vec::new(),
        }
    }

    /// Replay a `GameRestore` carried by a `.lq.FastTest.syncGame` /
    /// `.lq.FastTest.enterGame` response. Majsoul sends this right after
    /// `authGame` when a client reconnects to an in-progress match;
    /// `game_restore.actions[]` is the full action log of the current kyoku
    /// (from `ActionNewRound` up to the present moment). Replaying each action
    /// through the normal per-action path rebuilds the mjai stream so the
    /// engine / bot / UI recover the in-progress hand.
    ///
    /// Each entry is an `ActionPrototype { name, data, step }`. Unlike the live
    /// `.lq.ActionPrototype` notify, the embedded `data` is plain base64
    /// protobuf with no XOR — see [`parser::decode_restore_action`]. We re-wrap
    /// each decoded action into the same `{name, data}` shape
    /// `handle_action_prototype` expects and feed it through, so every `build_*`
    /// conversion and the `pending_reach_accepted` bookkeeping is reused
    /// verbatim (a reach left pending at the end of the replay is drained by the
    /// first subsequent live action, exactly as in live play).
    ///
    /// No `start_game` is emitted — the preceding `authGame` already did that
    /// (the single reset point for the downstream tracker / bot / autoplay). The
    /// leading `ActionMJStart` has no handler and is a no-op, as is any other
    /// unrecognised action. The `snapshot` field is intentionally ignored;
    /// replaying the actions reconstructs the full state.
    fn handle_game_restore(&mut self, payload: &JsonValue) -> Vec<MjaiEvent> {
        let is_end = payload
            .get("is_end")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let step = payload.get("step").and_then(JsonValue::as_u64);
        let actions = payload
            .pointer("/game_restore/actions")
            .and_then(JsonValue::as_array);
        let Some(actions) = actions.filter(|a| !a.is_empty()) else {
            // First-entry `enterGame` (or a finished game) carries no replay
            // actions — nothing to reconstruct.
            info!(
                target: "akagi::bridge::majsoul",
                "GameRestore with no actions to replay (step={step:?} is_end={is_end})"
            );
            return Vec::new();
        };
        info!(
            target: "akagi::bridge::majsoul",
            "replaying {} GameRestore actions (step={step:?} is_end={is_end})",
            actions.len()
        );

        // Replayed operations must not clobber the live budget slot with
        // historical windows; only the final state of the replay matters.
        self.replaying = true;
        self.restore_budget = None;

        let mut events = Vec::new();
        for action in actions {
            let Some(name) = action.get("name").and_then(JsonValue::as_str) else {
                warn!(
                    target: "akagi::bridge::majsoul",
                    "GameRestore action missing name: {action}"
                );
                continue;
            };
            let Some(b64) = action.get("data").and_then(JsonValue::as_str) else {
                warn!(
                    target: "akagi::bridge::majsoul",
                    "GameRestore action {name} missing data string"
                );
                continue;
            };
            let decoded = match parser::decode_restore_action(name, b64) {
                Ok(decoded) => decoded,
                Err(e) => {
                    warn!(
                        target: "akagi::bridge::majsoul",
                        "failed to decode GameRestore action {name}: {e:#}"
                    );
                    continue;
                }
            };
            // Re-wrap as the same `{name, data}` shape a live ActionPrototype
            // carries, then run it through the shared conversion path.
            let synthetic = ParsedMessage {
                msg_type: MessageType::Notify,
                msg_id: None,
                method_name: Arc::from(METHOD_ACTION_PROTOTYPE),
                payload: json!({ "name": name, "data": decoded }),
            };
            events.extend(self.handle_action_prototype(&synthetic));
        }
        self.replaying = false;

        // Commit the decision window left open at the end of the replay
        // (if any), backdating `opened_at` by the server-reported time
        // already consumed in that window. The unit of
        // `passed_waiting_time` is not confirmed (observed values 13–60);
        // seconds is assumed. Over-estimating elapsed time only makes the
        // delay model act sooner — it can never cause an overtime.
        let passed = payload
            .pointer("/game_restore/passed_waiting_time")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let committed = self.restore_budget.take().map(|mut b| {
            b.opened_at = Instant::now()
                .checked_sub(Duration::from_secs(passed))
                .unwrap_or_else(Instant::now);
            b
        });
        self.store_budget(committed);
        events
    }

    /// Refresh the shared time-budget slot from a live (non-replay) action:
    /// an operation list addressed to our seat opens a decision window; its
    /// absence means no window is open (`None` clears the slot). During a
    /// replay the value is parked in `restore_budget` instead — see
    /// `handle_game_restore`.
    fn update_time_budget(&mut self, action_name: &str, data: &JsonValue) {
        if self.time_budget.is_none() {
            return;
        }
        // The slot is shared by every flow bridge (spectate tabs, the
        // dying socket during a reconnect). A bridge that never resolved
        // a seat cannot own a decision window — it must not write, or
        // its unconditional `None`s would clear the playing flow's
        // freshly committed budget.
        if self.seat.is_none() {
            return;
        }
        let budget = self.extract_budget(action_name, data);
        if self.replaying {
            self.restore_budget = budget;
        } else {
            self.store_budget(budget);
        }
    }

    fn store_budget(&self, budget: Option<TimeBudget>) {
        if let Some(slot) = &self.time_budget {
            if let Ok(mut guard) = slot.write() {
                *guard = budget;
            }
        }
    }

    /// Pull a `TimeBudget` out of `data.operation` if the operation list is
    /// addressed to our seat. `operation` is emitted as `null` for actions
    /// without one (`skip_default_fields(false)` serialization), hence the
    /// `as_object` guard. Wire unit is milliseconds.
    fn extract_budget(&self, action_name: &str, data: &JsonValue) -> Option<TimeBudget> {
        let op = data.get("operation")?.as_object()?;
        let seat = op.get("seat").and_then(JsonValue::as_u64)?;
        if Some(seat) != self.seat.map(u64::from) {
            return None;
        }
        let source = match action_name {
            ACTION_NEW_ROUND => BudgetSource::NewRound,
            ACTION_DEAL_TILE => BudgetSource::DealTile,
            ACTION_DISCARD_TILE => BudgetSource::DiscardTile,
            ACTION_CHI_PENG_GANG => BudgetSource::ChiPengGang,
            // Chankan (ron on kakan / kokushi on ankan) and 3p 胡拔北
            // windows also carry an operation for our seat.
            ACTION_AN_GANG_ADD_GANG => BudgetSource::AnGangAddGang,
            ACTION_BA_BEI => BudgetSource::BaBei,
            _ => return None,
        };
        let fixed_ms = op.get("time_fixed").and_then(JsonValue::as_u64)?;
        // A zero base time means the semantics are unknown (possibly an
        // unlimited-thinking room). Treat as "no budget" rather than
        // producing a zero-width window that would floor every delay.
        if fixed_ms == 0 {
            return None;
        }
        let add_ms = op.get("time_add").and_then(JsonValue::as_u64).unwrap_or(0);
        Some(TimeBudget {
            fixed_ms: u32::try_from(fixed_ms).unwrap_or(u32::MAX),
            add_ms: u32::try_from(add_ms).unwrap_or(u32::MAX),
            opened_at: Instant::now(),
            source,
        })
    }

    fn handle_action_prototype(&mut self, msg: &ParsedMessage) -> Vec<MjaiEvent> {
        let action_name = msg.payload.get("name").and_then(JsonValue::as_str);
        let action_data = msg.payload.get("data");
        let (Some(action_name), Some(action_data)) = (action_name, action_data) else {
            warn!(
                target: "akagi::bridge::majsoul",
                "ActionPrototype payload missing name/data: {}", msg.payload
            );
            return Vec::new();
        };

        self.update_time_budget(action_name, action_data);

        // Drain queued reach_accepted before the next non-Hule action. A
        // Hule on the riichi declaration tile voids the riichi, so we
        // discard the queued event in that case.
        let pending_reach = if action_name == ACTION_HULE || action_name == ACTION_DISCARD_TILE {
            // ActionDiscardTile right after a riichi declaration shouldn't
            // happen in practice (the declarer can't immediately discard
            // again), but if it does we leave the queue alone so the next
            // legitimate action drains it.
            // ActionHule clears the queue.
            if action_name == ACTION_HULE {
                self.pending_reach_accepted = None;
            }
            None
        } else {
            self.pending_reach_accepted.take()
        };

        // Only the initial decision belongs to the undivided opening deal.
        // A kan/kita followed by a real replacement draw must use wire moqie.
        let ends_opening = matches!(
            action_name,
            ACTION_DEAL_TILE
                | ACTION_AN_GANG_ADD_GANG
                | ACTION_CHI_PENG_GANG
                | ACTION_BA_BEI
                | ACTION_NO_TILE
                | ACTION_LIU_JU
                | ACTION_HULE
        );
        let result = match action_name {
            ACTION_NEW_ROUND => self.build_start_kyoku(action_data),
            ACTION_DEAL_TILE => self.build_tsumo(action_data),
            ACTION_DISCARD_TILE => self.build_dahai(action_data),
            ACTION_CHI_PENG_GANG => self.build_chi_peng_gang(action_data),
            ACTION_AN_GANG_ADD_GANG => self.build_an_gang_add_gang(action_data),
            ACTION_NO_TILE => self.build_no_tile(action_data),
            ACTION_LIU_JU => self.build_liu_ju(action_data),
            ACTION_HULE => self.build_hule(action_data),
            ACTION_BA_BEI => self.build_kita(action_data),
            _ => return Vec::new(),
        };
        let events = match result {
            Ok(events) => {
                if ends_opening {
                    self.opening_deal = None;
                }
                events
            }
            Err(e) => {
                warn!(
                    target: "akagi::bridge::majsoul",
                    "{action_name} → mjai conversion failed: {e:#}"
                );
                Vec::new()
            }
        };

        match pending_reach {
            Some(actor) => {
                let mut combined = Vec::with_capacity(events.len() + 1);
                combined.push(MjaiEvent::ReachAccepted { actor });
                combined.extend(events);
                combined
            }
            None => events,
        }
    }

    /// `ActionDealTile` → mjai `tsumo`. Server omits the `tile` field for
    /// other players' draws (we don't see what they got), so an empty /
    /// missing tile becomes `"?"`. Our own draws carry the real tile.
    ///
    /// When this deal is the rinshan after an ankan, the new dora marker
    /// arrives in `data.doras` and flips before the tsumo (即乗り). For
    /// kakan/daiminkan the live server withholds the marker until the kan
    /// declarer's next discard (`build_dahai` picks it up there); if it
    /// nevertheless shows up on the rinshan deal, it is deferred to the
    /// commit event per 後乗り (see `DoraTiming`).
    fn build_tsumo(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let self_seat = self.seat.context("seat unresolved at ActionDealTile")?;
        let actor = data.get("seat").and_then(JsonValue::as_u64).unwrap_or(0) as Actor;
        let tile_raw = data.get("tile").and_then(JsonValue::as_str).unwrap_or("");
        let pai = if actor == self_seat && !tile_raw.is_empty() {
            ms_to_mjai(tile_raw)?.to_string()
        } else {
            UNKNOWN_TILE.into()
        };

        let mut new_markers = self.consume_new_doras(data)?;
        let timing = self.dora_timing.take();
        let mut events = Vec::with_capacity(new_markers.len() + 1);

        match timing {
            Some(DoraTiming::PendingAfterRinshan) => {
                // Kakan/daiminkan: 後乗り — rinshan tsumo first. Markers
                // usually arrive with the upcoming discard instead, but if
                // this deal already carries them, hold them for the commit.
                self.deferred_doras.append(&mut new_markers);
                events.push(MjaiEvent::Tsumo { actor, pai });
            }
            Some(DoraTiming::PendingBeforeRinshan) => {
                // Ankan: 即乗り — flip dora, then rinshan tsumo.
                if new_markers.is_empty() {
                    warn!(
                        target: "akagi::bridge::majsoul",
                        "rinshan deal after ankan missing new dora marker"
                    );
                }
                for dora_marker in new_markers {
                    events.push(MjaiEvent::Dora { dora_marker });
                }
                events.push(MjaiEvent::Tsumo { actor, pai });
            }
            None => {
                // Unexpected dora bump outside a kan flow. Fall back to
                // emitting it before tsumo (Akagi-style) and warn so the
                // protocol drift is visible in logs.
                for dora_marker in new_markers {
                    warn!(
                        target: "akagi::bridge::majsoul",
                        "new dora marker {dora_marker} without preceding kan; emitting before tsumo"
                    );
                    events.push(MjaiEvent::Dora { dora_marker });
                }
                events.push(MjaiEvent::Tsumo { actor, pai });
            }
        }
        Ok(events)
    }

    /// `ActionDiscardTile` → mjai `dahai`. `moqie` (default false) maps
    /// to `tsumogiri` after the opening deal is normalized. `seat` defaults to 0 — Majsoul omits it
    /// for the dealer's first discard.
    ///
    /// Kan dora (後乗り): after a kakan/daiminkan the live server reveals
    /// the new marker by growing this message's `doras` array — the
    /// rinshan deal does *not* carry it. Any markers found here (plus any
    /// deferred from the rinshan deal) are emitted *before* the dahai
    /// event, matching the mjai state machine (tsumo → dora → dahai).
    ///
    /// Riichi: when `is_liqi` (or `is_wliqi`, double riichi) is set, a
    /// `reach` event precedes the `dahai` and `pending_reach_accepted` is
    /// queued for the *next* action — the mjai spec puts `reach_accepted`
    /// only after the declaration tile passes through (i.e. before the
    /// next tsumo / chi / pon / daiminkan), and a ron on that tile voids
    /// the riichi.
    fn build_dahai(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let actor = data.get("seat").and_then(JsonValue::as_u64).unwrap_or(0) as Actor;
        let tile_raw = data
            .get("tile")
            .and_then(JsonValue::as_str)
            .context("ActionDiscardTile missing tile")?;
        if tile_raw.is_empty() {
            bail!("ActionDiscardTile.tile is empty");
        }
        let pai = ms_to_mjai(tile_raw)?.to_string();
        let wire_tsumogiri = data
            .get("moqie")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        // Project the discard onto the same 13 + draw partition used at
        // start_kyoku. Opponents' concealed draw identities stay unknown.
        let tsumogiri = match &self.opening_deal {
            Some(deal) if Some(actor) == self.seat => {
                deal.discard_is_tsumogiri(&pai, wire_tsumogiri)
            }
            _ => wire_tsumogiri,
        };
        let is_riichi = data
            .get("is_liqi")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false)
            || data
                .get("is_wliqi")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);

        let mut dora_markers = std::mem::take(&mut self.deferred_doras);
        dora_markers.extend(self.consume_new_doras(data)?);

        let mut events = Vec::with_capacity(dora_markers.len() + 2);
        for dora_marker in dora_markers {
            events.push(MjaiEvent::Dora { dora_marker });
        }
        if is_riichi {
            events.push(MjaiEvent::Reach {
                actor,
                pai: None,
                tsumogiri: None,
            });
        }
        events.push(MjaiEvent::Dahai {
            actor,
            pai,
            tsumogiri,
        });
        if is_riichi {
            self.pending_reach_accepted = Some(actor);
        }
        self.last_revealed_tile_actor = Some(actor);
        self.opening_deal = None;
        Ok(events)
    }

    /// Compare `data.doras` against `self.doras`. If the server array has
    /// grown, return every newly appended entry (mjai-mapped, in reveal
    /// order) and sync `self.doras` to the full server array. Returns an
    /// empty vec when nothing is new.
    ///
    /// Syncing the full tail (not just the last entry) matters: the local
    /// list must never fall behind the server's, otherwise a later payload
    /// whose array hasn't grown would still compare longer and re-emit a
    /// stale marker (the double-`9m` in issue #244).
    ///
    /// The already-seen prefix is also validated. Indicators flip in dead
    /// wall slot order, so the server array should only ever *extend*
    /// what we've reported; a rewritten prefix means that assumption
    /// broke, and since mjai cannot retract an emitted marker the best we
    /// can do is warn and adopt the server's tail going forward.
    fn consume_new_doras(&mut self, data: &JsonValue) -> Result<Vec<String>> {
        let arr = match data.get("doras").and_then(JsonValue::as_array) {
            Some(a) => a,
            None => return Ok(Vec::new()),
        };
        if arr.is_empty() {
            return Ok(Vec::new());
        }
        let mapped = arr
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .context("doras entry not a string")
                    .and_then(|raw| ms_to_mjai(raw).map(|t| t.to_string()))
            })
            .collect::<Result<Vec<String>>>()?;

        let prefix_len = self.doras.len().min(mapped.len());
        if self.doras[..prefix_len] != mapped[..prefix_len] {
            warn!(
                target: "akagi::bridge::majsoul",
                "server dora array {mapped:?} does not extend the markers reported so far {local:?}; \
                 adopting the server tail (already-emitted markers cannot be retracted)",
                local = self.doras
            );
        }
        if mapped.len() <= self.doras.len() {
            return Ok(Vec::new());
        }
        let new_markers = mapped[self.doras.len()..].to_vec();
        self.doras = mapped;
        Ok(new_markers)
    }

    /// `ActionChiPengGang` → `chi` / `pon` / `daiminkan`. `froms[i]`
    /// identifies whose discard supplied tile `tiles[i]`; the sole
    /// non-actor seat is the meld's `target`, its tile is `pai`, and the
    /// remaining tiles are `consumed`.
    fn build_chi_peng_gang(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let actor = data
            .get("seat")
            .and_then(JsonValue::as_u64)
            .context("ActionChiPengGang missing seat")? as Actor;
        let kind = data
            .get("type")
            .and_then(JsonValue::as_u64)
            .context("ActionChiPengGang missing type")?;
        let tiles = data
            .get("tiles")
            .and_then(JsonValue::as_array)
            .context("ActionChiPengGang missing tiles")?;
        let froms = data
            .get("froms")
            .and_then(JsonValue::as_array)
            .context("ActionChiPengGang missing froms")?;
        if tiles.len() != froms.len() {
            bail!(
                "ActionChiPengGang tiles/froms length mismatch: {} vs {}",
                tiles.len(),
                froms.len()
            );
        }

        let mut target = actor;
        let mut pai = String::new();
        let mut consumed: Vec<String> = Vec::new();
        for (idx, from) in froms.iter().enumerate() {
            let from_seat =
                from.as_u64()
                    .context("ActionChiPengGang.froms[i] not a uint")? as Actor;
            let tile_raw = tiles[idx]
                .as_str()
                .context("ActionChiPengGang.tiles[i] not a string")?;
            let tile = ms_to_mjai(tile_raw)?.to_string();
            if from_seat == actor {
                consumed.push(tile);
            } else {
                target = from_seat;
                pai = tile;
            }
        }
        if target == actor {
            bail!("ActionChiPengGang: no foreign seat in froms");
        }
        if pai.is_empty() {
            bail!("ActionChiPengGang: target tile not found");
        }

        let event = match kind {
            CHI_PENG_GANG_CHI => {
                if self.num_players == 3 {
                    // Sanma has no chi. Majsoul shouldn't send it for 3p
                    // tables but log loudly and drop if it does, rather than
                    // propagate a malformed mjai stream.
                    warn!(
                        target: "akagi::bridge::majsoul",
                        "received ActionChiPengGang(chi) in 3p flow; dropping (data={data})"
                    );
                    return Ok(Vec::new());
                }
                if consumed.len() != 2 {
                    bail!("chi expects 2 consumed tiles, got {}", consumed.len());
                }
                let consumed: [String; 2] = consumed
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("chi consumed array conversion failed"))?;
                MjaiEvent::Chi {
                    actor,
                    target,
                    pai,
                    consumed,
                }
            }
            CHI_PENG_GANG_PENG => {
                if consumed.len() != 2 {
                    bail!("pon expects 2 consumed tiles, got {}", consumed.len());
                }
                let consumed: [String; 2] = consumed
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("pon consumed array conversion failed"))?;
                MjaiEvent::Pon {
                    actor,
                    target,
                    pai,
                    consumed,
                }
            }
            CHI_PENG_GANG_GANG => {
                if consumed.len() != 3 {
                    bail!("daiminkan expects 3 consumed tiles, got {}", consumed.len());
                }
                let consumed: [String; 3] = consumed
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("daiminkan consumed array conversion failed"))?;
                // 後乗り: dora is flipped after the rinshan tsumo, before
                // the next dahai.
                self.dora_timing = Some(DoraTiming::PendingAfterRinshan);
                MjaiEvent::Daiminkan {
                    actor,
                    target,
                    pai,
                    consumed,
                }
            }
            other => bail!("unknown ActionChiPengGang.type: {other}"),
        };
        Ok(vec![event])
    }

    /// `ActionAnGangAddGang` → `ankan` (type 3) or `kakan` (type 2). The
    /// `tiles` field is a single tile string (not a list) — for ankan it
    /// names the four-of-a-kind, for kakan it names the new tile being
    /// added on top of an existing pon.
    ///
    /// The payload has a `doras` field; live servers deliver kan reveals
    /// on later messages instead, but it is consumed here defensively so
    /// a marker arriving this early is neither lost nor double-emitted:
    /// for ankan it flips right after the `ankan` event (即乗り), for
    /// kakan it is deferred to the commit event (後乗り).
    fn build_an_gang_add_gang(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let actor = data
            .get("seat")
            .and_then(JsonValue::as_u64)
            .context("ActionAnGangAddGang missing seat")? as Actor;
        let kind = data
            .get("type")
            .and_then(JsonValue::as_u64)
            .context("ActionAnGangAddGang missing type")?;
        let tile_raw = data
            .get("tiles")
            .and_then(JsonValue::as_str)
            .context("ActionAnGangAddGang.tiles not a string")?;
        let pai = ms_to_mjai(tile_raw)?.to_string();

        let mut new_markers = self.consume_new_doras(data)?;
        let mut events = Vec::with_capacity(new_markers.len() + 1);
        match kind {
            AN_GANG_ADD_GANG_AN => {
                let consumed = ankan_consumed(&pai);
                events.push(MjaiEvent::Ankan { actor, consumed });
                if new_markers.is_empty() {
                    // 即乗り: the reveal rides the upcoming rinshan deal.
                    self.dora_timing = Some(DoraTiming::PendingBeforeRinshan);
                } else {
                    // Reveal already delivered with the kan itself.
                    for dora_marker in new_markers {
                        events.push(MjaiEvent::Dora { dora_marker });
                    }
                }
            }
            AN_GANG_ADD_GANG_ADD => {
                let consumed = kakan_consumed(&pai);
                // 後乗り: dora flips after rinshan tsumo, before dahai.
                self.deferred_doras.append(&mut new_markers);
                self.dora_timing = Some(DoraTiming::PendingAfterRinshan);
                events.push(MjaiEvent::Kakan {
                    actor,
                    pai,
                    consumed,
                });
            }
            other => bail!("unknown ActionAnGangAddGang.type: {other}"),
        }
        // Both kan types reveal a tile that another seat may rob:
        //   - kakan → 搶槓 (chankan).
        //   - ankan → 国士無双搶暗槓 (only kokushi can rob; the server
        //     would only emit `ActionHule` in that valid case anyway, so
        //     unconditional tracking is safe).
        // If a ron follows, `build_hule` uses this seat as the `target`.
        self.last_revealed_tile_actor = Some(actor);
        Ok(events)
    }

    /// `ActionNoTile` (荒牌流局, exhaustive draw) → `[ryukyoku{deltas},
    /// end_kyoku]`. The mjai spec text only mentions ryukyoku for
    /// 九種九牌, but libriichi/Mortal use the same event for noten
    /// payments and carry the deltas in the optional `deltas` field; we
    /// follow that convention so downstream stat code can attribute the
    /// payment correctly.
    ///
    /// `data.scores[]` is an array of `NoTileScoreInfo` — each entry
    /// carries its own `delta_scores` array sized to `num_players`, or an
    /// empty one when no points change hands. Multiple entries can occur
    /// (rare — e.g. tenpai redistribution + nagashi mangan in the same
    /// frame); the bridge sums them per seat.
    fn build_no_tile(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let deltas = sum_delta_scores(data, self.num_players)?;
        let tehais = data
            .get("players")
            .map(|value| {
                let players = value.as_array().context("players is not an array")?;
                // An absent/empty array does not describe any public hand.
                if players.is_empty() {
                    return Ok(None);
                }
                if players.len() != usize::from(self.num_players) {
                    bail!("public draw player count differs from num_players");
                }
                players
                    .iter()
                    .map(|player| {
                        if player.get("tingpai").and_then(JsonValue::as_bool) == Some(false) {
                            return Ok(None);
                        }
                        let hand = public_tiles(player.get("hand"))?;
                        Ok(hand.filter(|tiles| !tiles.is_empty()))
                    })
                    .collect::<Result<Vec<_>>>()
                    .map(Some)
            })
            .transpose()?
            .flatten();
        let scores = draw_settlement_scores(data, self.num_players, deltas.as_deref())?;
        let nagashi = data
            .get("liujumanguan")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        Ok(vec![
            MjaiEvent::Ryukyoku {
                deltas,
                tehais,
                scores,
                reason: Some(if nagashi { "nagashi" } else { "exhaustive" }.into()),
            },
            MjaiEvent::EndKyoku,
        ])
    }

    /// `ActionHule` (胡牌) → one `hora` per entry in `data.hules[]`,
    /// followed by `end_kyoku`.
    ///
    /// Per mjai semantics:
    /// - `actor = hule.seat`.
    /// - `target = actor` when `hule.zimo` (self-tsumo); otherwise the
    ///   discarder, which mjai requires us to track ourselves —
    ///   `HuleInfo` doesn't carry it. We use `self.last_revealed_tile_actor` set by
    ///   the most recent `build_dahai`.
    /// - `deltas = data.delta_scores` (top-level, the round's net point
    ///   change). For multi-ron we attach the same total to each `hora`
    ///   event; the consumer can dedupe if it cares (Mortal-style stats
    ///   only count points once anyway).
    /// - `ura_markers = hule.li_doras` when `hule.liqi` is true (riichi
    ///   reveals the ura), else `None`. An empty `li_doras` array under
    ///   `liqi=true` still becomes `Some(vec![])` so consumers can tell
    ///   "had a riichi but no ura markers" from "no riichi".
    fn build_hule(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let hules = data
            .get("hules")
            .and_then(JsonValue::as_array)
            .context("ActionHule missing hules")?;
        if hules.is_empty() {
            bail!("ActionHule with empty hules");
        }
        let deltas = data
            .get("delta_scores")
            .and_then(JsonValue::as_array)
            .map(|a| parse_deltas(a))
            .transpose()?;

        let scores = data
            .get("scores")
            .map(|value| {
                let scores = parse_deltas(value.as_array().context("scores is not an array")?)?;
                if scores.len() != usize::from(self.num_players) {
                    bail!("settlement score count differs from num_players");
                }
                Ok(scores)
            })
            .transpose()?;
        let mut events = Vec::with_capacity(hules.len() + 1);
        for hule in hules {
            let actor = hule
                .get("seat")
                .and_then(JsonValue::as_u64)
                .context("HuleInfo missing seat")? as Actor;
            let zimo = hule
                .get("zimo")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            let target = if zimo {
                actor
            } else {
                self.last_revealed_tile_actor
                    .context("ron win without preceding tile-revealing action")?
            };
            let liqi = hule
                .get("liqi")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            let ura_markers = if liqi {
                let arr = hule
                    .get("li_doras")
                    .and_then(JsonValue::as_array)
                    .map(|a| -> Result<Vec<String>> {
                        a.iter()
                            .map(|v| {
                                v.as_str()
                                    .context("li_doras entry not a string")
                                    .and_then(|s| ms_to_mjai(s).map(String::from))
                            })
                            .collect()
                    })
                    .transpose()?
                    .unwrap_or_default();
                Some(arr)
            } else {
                None
            };
            let mut pai = hule
                .get("hu_tile")
                .map(|value| {
                    value
                        .as_str()
                        .context("hu_tile is not a string")
                        .and_then(|tile| ms_to_mjai(tile).map(String::from))
                })
                .transpose()?;
            let mut tehai = hule
                .get("hand")
                .map(|value| {
                    value
                        .as_array()
                        .context("hand is not a tile array")?
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .context("hand tile is not a string")
                                .and_then(|tile| ms_to_mjai(tile).map(String::from))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            if zimo && Some(actor) == self.seat {
                if let Some(deal) = &self.opening_deal {
                    deal.normalize_win(&mut pai, &mut tehai)?;
                }
            }
            events.push(MjaiEvent::Hora {
                score: hand_score(hule),
                actor,
                target,
                deltas: deltas.clone(),
                ura_markers,
                pai,
                tehai,
                scores: scores.clone(),
            });
        }
        events.push(MjaiEvent::EndKyoku);
        Ok(events)
    }

    /// `ActionLiuJu` (途中流局, abortive draw) → `[ryukyoku, end_kyoku]`
    /// without deltas. Covers 九種九牌 / 四風連打 / 四家立直 / 四開槓 /
    /// 三家和了 (4p) and 九種九牌 / 四開槓 (3p — sufuurenta and suucha
    /// riichi need 4 players and don't apply to sanma).
    ///
    /// Official client 0.11.252.w confirms E_LiuJu 1..5 and pipe-delimited
    /// `allplayertiles`. Four riichi shows all seats; triple ron excludes
    /// its named discarder. Nine terminals reveals the declarer's tiles.
    fn build_liu_ju(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let kind = data.get("type").and_then(JsonValue::as_u64);
        let reason = match kind {
            Some(1) => Some("kyuushu"),
            Some(2) => Some("four_winds"),
            Some(3) => Some("four_kans"),
            Some(4) => Some("four_riichis"),
            Some(5) => Some("three_rons"),
            _ => None,
        };
        let tehais = if kind == Some(1) {
            let hand = public_tiles(data.get("tiles"))?.filter(|tiles| !tiles.is_empty());
            hand.map(|hand| {
                let actor = data.get("seat").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                if actor >= usize::from(self.num_players) {
                    bail!("public abortive draw seat out of range");
                }
                let mut hands = vec![None; usize::from(self.num_players)];
                hands[actor] = Some(hand);
                Ok(hands)
            })
            .transpose()?
        } else if matches!(kind, Some(4 | 5)) {
            data.get("allplayertiles")
                .map(|value| {
                    let rows = value.as_array().context("allplayertiles is not an array")?;
                    if rows.is_empty() {
                        return Ok(None);
                    }
                    if rows.len() != usize::from(self.num_players) {
                        bail!("public abortive hand count differs from num_players");
                    }
                    let discarder =
                        data.get("seat").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
                    if discarder >= usize::from(self.num_players) {
                        bail!("public abortive draw seat out of range");
                    }
                    rows.iter()
                        .enumerate()
                        .map(|(seat, row)| {
                            if kind == Some(5) && seat == discarder {
                                return Ok(None);
                            }
                            let row = row
                                .as_str()
                                .context("public abortive hand is not a string")?;
                            if row.is_empty() {
                                return Ok(None);
                            }
                            row.split('|')
                                .map(|tile| ms_to_mjai(tile).map(str::to_owned))
                                .collect::<Result<Vec<_>>>()
                                .map(Some)
                        })
                        .collect::<Result<Vec<_>>>()
                        .map(Some)
                })
                .transpose()?
                .flatten()
        } else if matches!(kind, Some(2 | 3)) {
            Some(vec![None; usize::from(self.num_players)])
        } else {
            None
        };
        Ok(vec![
            MjaiEvent::Ryukyoku {
                deltas: None,
                tehais,
                scores: None,
                reason: reason.map(str::to_owned),
            },
            MjaiEvent::EndKyoku,
        ])
    }

    /// Convert one Majsoul `ActionNewRound` payload into `start_kyoku`
    /// (followed by `tsumo` for the dealer's first draw — known tile when
    /// we're the dealer, `"?"` when we aren't).
    ///
    /// The seat must already be resolved via the prior `authGame` exchange;
    /// otherwise we have no way to know which tehai slot to fill in.
    ///
    /// Resets per-kyoku dora-tracking state so stale `deferred_doras` from
    /// a previous kyoku can't bleed into this one.
    fn build_start_kyoku(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let seat = self.seat.context("seat unresolved at ActionNewRound")?;

        // Missing protobuf uint fields default to 0 in the JSON payload.
        let chang = data.get("chang").and_then(JsonValue::as_u64).unwrap_or(0) as usize;
        let ju = data.get("ju").and_then(JsonValue::as_u64).unwrap_or(0) as u8;
        let ben = data.get("ben").and_then(JsonValue::as_u64).unwrap_or(0) as u8;
        let liqibang = data
            .get("liqibang")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0) as u8;

        let bakaze: String = match chang {
            0 => "E",
            1 => "S",
            2 => "W",
            3 => "N",
            other => bail!("invalid chang value: {other}"),
        }
        .into();

        let dora_marker = data
            .get("doras")
            .and_then(JsonValue::as_array)
            .and_then(|a| a.first())
            .and_then(JsonValue::as_str)
            .context("ActionNewRound missing doras[0]")?;
        let dora_marker = ms_to_mjai(dora_marker)?.to_string();

        let scores = parse_scores(data)?;

        let tiles_raw = data
            .get("tiles")
            .and_then(JsonValue::as_array)
            .context("ActionNewRound missing tiles")?;
        let my_tiles: Vec<String> = tiles_raw
            .iter()
            .map(|v| {
                v.as_str()
                    .context("non-string tile in ActionNewRound.tiles")
                    .and_then(|s| ms_to_mjai(s).map(|t| t.to_string()))
            })
            .collect::<Result<Vec<_>>>()?;

        let oya = ju;
        let n = self.num_players as usize;
        let mut tehais: Vec<Vec<String>> = (0..n)
            .map(|_| vec![UNKNOWN_TILE.to_string(); TEHAI_SIZE])
            .collect();

        let tsumo_event = match my_tiles.len() {
            TEHAI_SIZE => {
                // Non-dealer: our 13 tiles fill our row; dealer's first draw
                // is unknown to us.
                let mut my_row = my_tiles.clone();
                my_row.sort_by(|a, b| compare_pai(a, b));
                fill_seat_row(&mut tehais, seat, my_row)?;
                if oya == seat {
                    bail!("dealer must receive 14 tiles, got 13");
                }
                MjaiEvent::Tsumo {
                    actor: oya,
                    pai: UNKNOWN_TILE.into(),
                }
            }
            TSUMO_TEHAI_SIZE => {
                // Dealer: first 13 tiles → our tehai (sorted), 14th tile is
                // the dealer's opening tsumo.
                if oya != seat {
                    bail!("non-dealer must receive 13 tiles, got 14");
                }
                let mut my_row: Vec<String> = my_tiles[..TEHAI_SIZE].to_vec();
                my_row.sort_by(|a, b| compare_pai(a, b));
                let tsumo_pai = my_tiles[TEHAI_SIZE].clone();
                fill_seat_row(&mut tehais, seat, my_row)?;
                MjaiEvent::Tsumo {
                    actor: seat,
                    pai: tsumo_pai,
                }
            }
            n => bail!("unexpected tile count {n} in ActionNewRound (expected 13 or 14)"),
        };

        info!(
            target: "akagi::bridge::majsoul",
            "start_kyoku bakaze={bakaze} kyoku={kyoku} oya={oya} honba={ben} kyotaku={liqibang}",
            kyoku = oya + 1,
        );

        // Fresh kyoku — reset dora + riichi + discard bookkeeping.
        self.doras = vec![dora_marker.clone()];
        self.dora_timing = None;
        self.deferred_doras.clear();
        self.last_revealed_tile_actor = None;
        self.pending_reach_accepted = None;
        self.opening_deal = if my_tiles.len() == TSUMO_TEHAI_SIZE {
            Some(OpeningDeal {
                hand: tehais[seat as usize].clone(),
                draw: my_tiles[TEHAI_SIZE].clone(),
            })
        } else {
            None
        };

        Ok(vec![
            MjaiEvent::StartKyoku {
                bakaze,
                dora_marker,
                kyoku: oya + 1,
                honba: ben,
                kyotaku: liqibang,
                oya,
                scores,
                tehais,
                num_players: self.num_players,
            },
            tsumo_event,
        ])
    }

    /// `ActionBaBei` (3p 北抜き / nukidora) → mjai `kita`.
    ///
    /// Captured Majsoul payload (3p AI room, 2026-04-29):
    ///
    /// ```json
    /// {"data":{"doras":[],"moqie":false,"seat":0,"tile_state":0,
    ///          "tingpais":[],"zhenting":false},
    ///  "name":"ActionBaBei","step":2}
    /// ```
    ///
    /// Notes:
    /// - No `tile` field — kita is always the North tile, so we hardcode
    ///   `pai = "N"` per the mjai 3p extension.
    /// - `doras: []` — kita itself does NOT flip a new dora indicator
    ///   (Tenhou rule, confirmed live). The array is still checked: after
    ///   a kakan/daiminkan the declarer may pull North instead of
    ///   discarding, and that babei is then the commit event carrying the
    ///   kan's 後乗り reveal — emitted before the `kita` event, together
    ///   with any marker deferred from the rinshan deal.
    /// - `last_revealed_tile_actor` is updated so a follow-up ron-on-kita
    ///   (chankan-style) gets the correct `target` in `build_hule`.
    ///
    /// Outside 3p tables Majsoul should never emit this; if it does, log
    /// a warning but still emit so the data isn't silently lost.
    fn build_kita(&mut self, data: &JsonValue) -> Result<Vec<MjaiEvent>> {
        let actor = data
            .get("seat")
            .and_then(JsonValue::as_u64)
            .context("ActionBaBei missing seat")? as Actor;
        if self.num_players != 3 {
            warn!(
                target: "akagi::bridge::majsoul",
                "ActionBaBei received in {}p flow (expected 3p): {data}",
                self.num_players
            );
        }
        let mut dora_markers = std::mem::take(&mut self.deferred_doras);
        dora_markers.extend(self.consume_new_doras(data)?);

        let mut events = Vec::with_capacity(dora_markers.len() + 1);
        for dora_marker in dora_markers {
            events.push(MjaiEvent::Dora { dora_marker });
        }
        self.last_revealed_tile_actor = Some(actor);
        events.push(MjaiEvent::Kita {
            actor,
            pai: Some("N".to_string()),
        });
        Ok(events)
    }

    fn handle_auth_game_response(&mut self, payload: &JsonValue) -> Vec<MjaiEvent> {
        let Some(account_id) = self.account_id else {
            warn!(
                target: "akagi::bridge::majsoul",
                "authGame response received without prior request — cannot resolve seat"
            );
            return Vec::new();
        };
        let Some(seat_list) = payload.get("seat_list").and_then(JsonValue::as_array) else {
            warn!(
                target: "akagi::bridge::majsoul",
                "authGame response missing seat_list: {payload}"
            );
            return Vec::new();
        };
        let position = seat_list
            .iter()
            .position(|v| v.as_u64() == Some(account_id));
        let Some(seat) = position else {
            warn!(
                target: "akagi::bridge::majsoul",
                "account_id {account_id} not found in seat_list {seat_list:?}"
            );
            return Vec::new();
        };
        let seat = seat as Actor;
        self.seat = Some(seat);
        // 3p tables produce length-3 seat_list; 4p length-4. Anything else
        // would be a protocol surprise — clamp into [3, 4] but log loudly.
        let detected = seat_list.len() as u8;
        if detected != 3 && detected != 4 {
            warn!(
                target: "akagi::bridge::majsoul",
                "unexpected seat_list length {detected}; defaulting num_players=4"
            );
            self.num_players = 4;
        } else {
            self.num_players = detected;
        }
        let meta_u32 = |key: &str| {
            payload
                .pointer(&format!("/game_config/meta/{key}"))
                .and_then(JsonValue::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|&value| value != 0)
        };
        let mode_id = meta_u32("mode_id");
        let room_id = meta_u32("room_id");
        let contest_uid = meta_u32("contest_uid");
        let match_mode = payload
            .pointer("/game_config/mode/mode")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u8::try_from(value).ok());
        let names = names_from_payload(payload, seat_list);
        // Each seated player comes with their ranks; ours is where we stand
        // as this game begins.
        let us = payload
            .get("players")
            .and_then(JsonValue::as_array)
            .and_then(|players| {
                players
                    .iter()
                    .find(|p| p.get("account_id").and_then(JsonValue::as_u64) == Some(account_id))
            });
        if let Some(us) = us {
            self.publish_account(us);
        }
        let ladder = if self.num_players == 3 {
            "level3"
        } else {
            "level"
        };
        let our_level = level_of(us.and_then(|us| us.get(ladder)));
        info!(
            target: "akagi::bridge::majsoul",
            "seat resolved: account_id={account_id} seat={seat} num_players={np} mode_id={mode_id:?} match_mode={match_mode:?} names={names:?}",
            np = self.num_players,
        );
        vec![MjaiEvent::StartGame {
            names,
            kyoku_first: None,
            aka_flag: None,
            id: Some(seat),
            num_players: self.num_players,
            game_meta: Some(GameMeta {
                game_id: self.game_id,
                match_mode,
                match_info: Some(MatchInfo::Majsoul {
                    game_uuid: self.game_uuid.clone(),
                    mode_id,
                    room_id,
                    contest_uid,
                }),
                our_level,
            }),
        }]
    }
}

fn stable_game_id(game_uuid: &str) -> u64 {
    // FNV-1a gives a compact, stable reconnect key for the recorder's dedup.
    // The raw UUID is kept separately on the bridge and persisted via
    // `MatchInfo` into the local history index (never uploaded).
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in game_uuid.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// A `{id, score}` rank of an `Account` or a seated `PlayerGameView`; `None`
/// when it names none (id 0: a ladder never played).
fn level_of(value: Option<&JsonValue>) -> Option<Level> {
    let value = value?;
    let id = u32::try_from(value.get("id")?.as_u64()?)
        .ok()
        .filter(|&id| id != 0)?;
    // A zero score may be left out on the wire.
    let score = match value.get("score") {
        None => 0,
        Some(score) => i32::try_from(score.as_i64()?).ok()?,
    };
    Some(Level { id, score })
}

/// The ranks of an account or a seated player, stamped now; `None` when it
/// has neither ladder.
fn account_levels(account: &JsonValue) -> Option<AccountLevels> {
    let level = level_of(account.get("level"));
    let level3 = level_of(account.get("level3"));
    (level.is_some() || level3.is_some()).then(|| AccountLevels {
        level,
        level3,
        at: Utc::now(),
    })
}

/// How a `HuleInfo` scored its hand: the yaku it lists (`fans`), han, fu and
/// whether it is a yakuman. `None` when it lists no yaku.
fn hand_score(hule: &JsonValue) -> Option<crate::schema::mjai::HandScore> {
    let number = |value: Option<&JsonValue>| {
        value
            .and_then(JsonValue::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let yaku: Vec<(u32, u32)> = hule
        .get("fans")?
        .as_array()?
        .iter()
        .filter_map(|fan| Some((number(fan.get("id"))?, number(fan.get("val")).unwrap_or(0))))
        .collect();
    if yaku.is_empty() {
        return None;
    }
    Some(crate::schema::mjai::HandScore {
        yaku,
        han: number(hule.get("count")).unwrap_or(0),
        fu: number(hule.get("fu")).unwrap_or(0),
        yakuman: hule
            .get("yiman")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false),
    })
}

/// What the game did to each seat's rank points, by seat, from the
/// final-result payload (`grading_score`). `None` unless every seat has
/// one. A room that is not ranked reports zeros.
fn parse_rank_points(payload: &JsonValue, num_players: u8) -> Option<Vec<i32>> {
    let n = usize::from(num_players);
    let players = payload.pointer("/result/players")?.as_array()?;
    if players.len() != n {
        return None;
    }
    let mut points = vec![None; n];
    for player in players {
        let seat = usize::try_from(player.get("seat")?.as_u64()?).ok()?;
        let change = i32::try_from(player.get("grading_score")?.as_i64()?).ok()?;
        *points.get_mut(seat)? = Some(change);
    }
    points.into_iter().collect()
}

/// `result.players` is ordered by placement and each row carries its absolute
/// seat plus the raw final score (`part_point_1`). Convert both to seat order.
fn parse_game_end_standings(payload: &JsonValue, num_players: u8) -> Option<(Vec<i32>, Vec<u8>)> {
    let n = usize::from(num_players);
    if !(3..=4).contains(&n) {
        return None;
    }
    let players = payload.pointer("/result/players")?.as_array()?;
    if players.len() != n {
        return None;
    }

    let mut scores = vec![None; n];
    let mut ranks = vec![None; n];
    for (rank0, player) in players.iter().enumerate() {
        let seat = usize::try_from(player.get("seat")?.as_u64()?).ok()?;
        let score = i32::try_from(player.get("part_point_1")?.as_i64()?).ok()?;
        if seat >= n || scores[seat].is_some() {
            return None;
        }
        scores[seat] = Some(score);
        ranks[seat] = Some((rank0 + 1) as u8);
    }
    Some((
        scores.into_iter().collect::<Option<Vec<_>>>()?,
        ranks.into_iter().collect::<Option<Vec<_>>>()?,
    ))
}

/// Parse `data.scores` into a `Vec<i32>` of native length (3 for sanma, 4 for
/// yonma). No padding — the wire format reflects the table's actual seat count.
fn parse_scores(data: &JsonValue) -> Result<Vec<i32>> {
    let arr = data
        .get("scores")
        .and_then(JsonValue::as_array)
        .context("ActionNewRound missing scores")?;
    arr.iter()
        .map(|v| {
            v.as_i64()
                .context("non-integer score")?
                .try_into()
                .context("score out of i32 range")
        })
        .collect()
}

/// Place a 13-tile row into `tehais[seat]`. Errors if `seat` is out of range
/// for the supplied tehais Vec or the row isn't exactly 13 tiles long.
fn fill_seat_row(tehais: &mut [Vec<String>], seat: Actor, row: Vec<String>) -> Result<()> {
    if (seat as usize) >= tehais.len() {
        bail!("seat {seat} out of range");
    }
    if row.len() != TEHAI_SIZE {
        bail!(
            "expected {TEHAI_SIZE} tiles for seat row, got {}",
            row.len()
        );
    }
    tehais[seat as usize] = row;
    Ok(())
}

/// Parse a JSON array of integers into `Vec<i32>` of native length.
fn parse_deltas(arr: &[JsonValue]) -> Result<Vec<i32>> {
    arr.iter()
        .map(|v| {
            v.as_i64()
                .context("delta_scores entry not an integer")?
                .try_into()
                .context("delta_scores entry out of i32 range")
        })
        .collect()
}

fn public_tiles(value: Option<&JsonValue>) -> Result<Option<Vec<String>>> {
    value
        .map(|value| {
            value
                .as_array()
                .context("public hand is not an array")?
                .iter()
                .map(|tile| {
                    ms_to_mjai(tile.as_str().context("public hand tile is not a string")?)
                        .map(str::to_owned)
                })
                .collect()
        })
        .transpose()
}

/// All entries settle against the same explicitly supplied pre-payment scores.
fn draw_settlement_scores(
    data: &JsonValue,
    num_players: u8,
    deltas: Option<&[i32]>,
) -> Result<Option<Vec<i32>>> {
    let Some(entries) = data.get("scores").and_then(JsonValue::as_array) else {
        return Ok(None);
    };
    let mut old: Option<Vec<i32>> = None;
    for entry in entries {
        let Some(value) = entry.get("old_scores") else {
            continue;
        };
        let scores = parse_deltas(value.as_array().context("old_scores is not an array")?)?;
        if scores.is_empty() {
            continue;
        }
        if scores.len() != usize::from(num_players) {
            bail!("old_scores count differs from num_players");
        }
        if old.as_ref().is_some_and(|old| *old != scores) {
            bail!("draw entries name different old_scores");
        }
        old = Some(scores);
    }
    old.map(|scores| {
        scores
            .into_iter()
            .enumerate()
            .map(|(seat, score)| {
                score
                    .checked_add(deltas.map_or(0, |deltas| deltas[seat]))
                    .context("draw settlement score overflow")
            })
            .collect()
    })
    .transpose()
}

/// Sum every `delta_scores` array under `data.scores[]` into a single
/// `Vec<i32>` sized to `num_players`. Returns `None` when no entries are
/// present (no point change — kept distinguishable from an explicit all-zero
/// delta). An entry whose `delta_scores` is empty is a draw in which nobody
/// paid and adds nothing. Malformed native-length vectors and integer
/// overflow are errors.
fn sum_delta_scores(data: &JsonValue, num_players: u8) -> Result<Option<Vec<i32>>> {
    let arr = match data.get("scores") {
        Some(value) => value.as_array().context("draw scores is not an array")?,
        _ => return Ok(None),
    };
    if arr.is_empty() {
        return Ok(None);
    }
    let n = num_players as usize;
    let mut total = vec![0i32; n];
    for entry in arr {
        let deltas = match entry.get("delta_scores") {
            Some(value) => value.as_array().context("delta_scores is not an array")?,
            None => continue,
        };
        // The server sends no deltas at all, rather than zeros, when every
        // seat is tenpai or every seat is noten.
        if deltas.is_empty() {
            continue;
        }
        if deltas.len() != n {
            bail!("delta_scores count differs from num_players");
        }
        for (i, v) in deltas.iter().enumerate() {
            let val: i32 = v
                .as_i64()
                .context("non-integer delta_scores entry")?
                .try_into()
                .context("delta_scores out of i32 range")?;
            total[i] = total[i]
                .checked_add(val)
                .context("draw delta_scores overflow")?;
        }
    }
    Ok(Some(total))
}

/// Mjai uses one red-five token (`5mr`/`5pr`/`5sr`) and three normals when a
/// red is in the kan. `pai` may itself be either form. Returns 4 tiles with
/// at most one red five, placed at index 0 when present.
fn ankan_consumed(pai: &str) -> [String; 4] {
    let normal = pai.trim_end_matches('r').to_string();
    let mut out = std::array::from_fn(|_| normal.clone());
    if pai_has_red_form(&normal) {
        out[0] = format!("{normal}r");
    }
    out
}

/// Same shape as `ankan_consumed`, but for the 3 tiles already in the
/// existing pon. The kan'd tile (`pai`) is the new addition and reported
/// separately on the `kakan` event.
fn kakan_consumed(pai: &str) -> [String; 3] {
    let normal = pai.trim_end_matches('r').to_string();
    let mut out = std::array::from_fn(|_| normal.clone());
    if pai_has_red_form(&normal) && !pai.ends_with('r') {
        out[0] = format!("{normal}r");
    }
    out
}

/// True if `pai` is a numbered 5 in m/p/s suits (the only tiles with a red
/// counterpart). Honors and non-fives never have a red form.
fn pai_has_red_form(pai: &str) -> bool {
    matches!(pai, "5m" | "5p" | "5s")
}

/// Resolve seat → display name via `payload.players[]` (account_id → nickname).
/// Robot seats are absent from `players` (they live under `robots[]` without
/// a nickname), so they get an empty string. Returned `Vec` length matches
/// `seat_list.len()` (3 for sanma, 4 for yonma).
fn names_from_payload(payload: &JsonValue, seat_list: &[JsonValue]) -> Vec<String> {
    let mut nick: HashMap<u64, String> = HashMap::new();
    if let Some(players) = payload.get("players").and_then(JsonValue::as_array) {
        for p in players {
            if let (Some(id), Some(name)) = (
                p.get("account_id").and_then(JsonValue::as_u64),
                p.get("nickname").and_then(JsonValue::as_str),
            ) {
                nick.insert(id, name.to_string());
            }
        }
    }
    seat_list
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|id| nick.get(&id).cloned())
                .unwrap_or_default()
        })
        .collect()
}

/// `inputOperation` op types autoplay needs to tell apart. Anything else
/// (and every `inputChiPengGang`) counts as `Other` — presence is what
/// matters there, not identity. See `autoplay::verify::InputKind` for why
/// these two are singled out:
/// Riichi goes out as `inputOperation` with the reach op type; a plain
/// discard of the same tile is `type: 1`. Telling them apart is what lets
/// autoplay notice that a riichi declaration was lost while its discard
/// went through anyway.
const OP_TYPE_REACH: u64 = 7;
/// A plain discard. Distinguished because an own-turn window that expires
/// is answered by the client with a tsumogiri carrying a small, plausible
/// `timeuse` and no `auto_operation` flag — `is_client_initiated` cannot
/// filter it, so the discard kind lets the verifier refuse it as proof
/// that an action-button press landed.
const OP_TYPE_DISCARD: u64 = 1;

fn input_kind(method: &str, payload: &JsonValue) -> crate::autoplay::InputKind {
    if method != METHOD_INPUT_OPERATION {
        return crate::autoplay::InputKind::Other;
    }
    match payload.get("type").and_then(JsonValue::as_u64) {
        Some(OP_TYPE_REACH) => crate::autoplay::InputKind::Reach,
        Some(OP_TYPE_DISCARD) => crate::autoplay::InputKind::Discard,
        _ => crate::autoplay::InputKind::Other,
    }
}

/// Whether an input command was the player's own press rather than the
/// client acting for them (auto-discard setting, or a decision window
/// that ran out). The latter must not count as proof a click landed.
fn is_client_initiated(payload: &JsonValue) -> bool {
    if payload
        .get("auto_operation")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    payload
        .get("timeuse")
        .and_then(JsonValue::as_u64)
        .is_none_or(|t| t < TIMEUSE_CLIENT_AUTO)
}

impl Default for MajsoulBridge {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl Bridge for MajsoulBridge {
    /// Parse a raw Majsoul WS binary frame, log the decoded message to the
    /// flow log (if any), and emit any resulting mjai events.
    fn parse(&mut self, direction: Direction, content: &[u8]) -> ParseResult {
        use crate::schema::ParsedFrame;
        match self.parser.parse(content) {
            Ok(msg) => {
                let kind = match msg.msg_type {
                    MessageType::Notify => "NOTIFY",
                    MessageType::Request => "REQUEST",
                    MessageType::Response => "RESPONSE",
                };
                let id_str = msg
                    .msg_id
                    .map(|i| format!("#{i}"))
                    .unwrap_or_else(|| "-".into());
                info!(
                    target: "akagi::bridge::majsoul",
                    "{} {kind} {id_str} {} {}",
                    direction.as_str(),
                    msg.method_name,
                    msg.payload
                );
                if let Some(log) = &self.flow_log {
                    let line = json!({
                        "ts": Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        "dir": direction.as_str(),
                        "type": kind,
                        "msg_id": msg.msg_id,
                        "method": msg.method_name.as_ref(),
                        "payload": msg.payload,
                    });
                    log.writeln(&line.to_string());
                }
                // Build the inspector-facing parsed view BEFORE dispatch
                // consumes the payload — `dispatch` only borrows `&msg`,
                // so we can clone the bits we want and still let it run.
                let parsed = Some(ParsedFrame {
                    method: msg.method_name.to_string(),
                    args: json!({
                        "kind": kind,
                        "msg_id": msg.msg_id,
                        "payload": msg.payload.clone(),
                    }),
                });
                if msg.msg_type == MessageType::Response
                    && msg.method_name.as_ref() == METHOD_FETCH_GAME_RECORD
                {
                    self.export_replay(&msg.payload);
                }
                let events = self.dispatch(&msg);
                // Rotate before writing so the StartGame event itself lands
                // in the freshly-opened file, not the previous game's file.
                if events
                    .iter()
                    .any(|e| matches!(e, MjaiEvent::StartGame { .. }))
                {
                    self.rotate_mjai_log();
                }
                self.write_mjai(&events);
                ParseResult { events, parsed }
            }
            Err(e) => {
                warn!(
                    target: "akagi::bridge::majsoul",
                    "{} liqi parse failed (len={}): {e:#}",
                    direction.as_str(),
                    content.len()
                );
                if let Some(log) = &self.flow_log {
                    let line = json!({
                        "ts": Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        "dir": direction.as_str(),
                        "type": "PARSE_ERROR",
                        "len": content.len(),
                        "error": format!("{e:#}"),
                    });
                    log.writeln(&line.to_string());
                }
                ParseResult::empty()
            }
        }
    }

    fn build(&mut self, _command: &MjaiEvent) -> Option<Vec<u8>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::ParsedMessage;
    use serde_json::json;

    fn req(method: &str, payload: JsonValue) -> ParsedMessage {
        ParsedMessage {
            msg_type: MessageType::Request,
            msg_id: Some(1),
            method_name: Arc::from(method),
            payload,
        }
    }

    fn resp(method: &str, payload: JsonValue) -> ParsedMessage {
        ParsedMessage {
            msg_type: MessageType::Response,
            msg_id: Some(1),
            method_name: Arc::from(method),
            payload,
        }
    }

    /// Our own ranks are said at login, when a game begins and when the
    /// client asks for its own account again after a game; another player's
    /// profile, or another seat, is never taken for ours.
    #[test]
    fn our_own_ranks_are_said_and_no_one_elses() {
        let bus = crate::event_bus::account_bus();
        let mut heard = bus.subscribe();
        let mut lobby = MajsoulBridge::new(None, None).with_account_bus(Some(bus.clone()));

        lobby.dispatch(&resp(
            ".lq.Lobby.oauth2Login",
            json!({
                "account_id": 7,
                "account": {
                    "account_id": 7,
                    "nickname": "demo",
                    "level": { "id": 10203, "score": 761 },
                    "level3": { "id": 20101, "score": 0 },
                },
            }),
        ));
        let said = heard.try_recv().expect("login says our ranks");
        assert_eq!(
            said.level,
            Some(Level {
                id: 10203,
                score: 761
            })
        );
        assert_eq!(
            said.level3,
            Some(Level {
                id: 20101,
                score: 0
            })
        );

        lobby.dispatch(&resp(
            METHOD_FETCH_ACCOUNT_INFO,
            json!({ "account": { "account_id": 8, "level": { "id": 10301, "score": 900 } } }),
        ));
        assert!(heard.try_recv().is_err(), "someone else's profile");

        lobby.dispatch(&resp(
            METHOD_FETCH_ACCOUNT_INFO,
            json!({
                "account": {
                    "account_id": 7,
                    "level": { "id": 10203, "score": 800 },
                    "level3": { "id": 20101 },
                },
            }),
        ));
        let said = heard.try_recv().expect("our own account, asked for again");
        assert_eq!(
            said.level,
            Some(Level {
                id: 10203,
                score: 800
            })
        );
        assert_eq!(
            said.level3,
            Some(Level {
                id: 20101,
                score: 0
            }),
            "a score left out is zero"
        );

        // A game has a connection of its own: our seat's ranks as it begins.
        let mut game = MajsoulBridge::new(None, None).with_account_bus(Some(bus));
        game.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 7 })));
        let started = game.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [
                    { "account_id": 9, "nickname": "a", "level": { "id": 10202, "score": 296 } },
                    {
                        "account_id": 7,
                        "nickname": "b",
                        "level": { "id": 10203, "score": 833 },
                        "level3": { "id": 20101, "score": 0 },
                    },
                ],
                "seat_list": [9u64, 3, 7u64, 2],
            }),
        ));
        let said = heard.try_recv().expect("our seat's ranks");
        assert_eq!(
            said.level,
            Some(Level {
                id: 10203,
                score: 833
            })
        );
        assert!(heard.try_recv().is_err(), "only our seat");
        // And the game keeps where we stood on its own ladder.
        assert!(matches!(
            &started[0],
            MjaiEvent::StartGame { game_meta: Some(meta), .. }
                if meta.our_level == Some(Level { id: 10203, score: 833 })
        ));
    }

    /// A three-player game keeps our place on the three-player ladder.
    #[test]
    fn a_three_player_game_keeps_our_three_player_rank() {
        let mut game = MajsoulBridge::new(None, None);
        game.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 7 })));
        let started = game.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{
                    "account_id": 7,
                    "nickname": "demo",
                    "level": { "id": 10203, "score": 833 },
                    "level3": { "id": 20102, "score": 410 },
                }],
                "seat_list": [7u64, 1, 2],
            }),
        ));
        assert!(matches!(
            &started[0],
            MjaiEvent::StartGame { game_meta: Some(meta), num_players: 3, .. }
                if meta.our_level == Some(Level { id: 20102, score: 410 })
        ));
    }

    /// Nothing is said of an account before we know it is ours, nor of a
    /// ladder never played (id 0).
    #[test]
    fn ranks_wait_for_who_we_are() {
        let bus = crate::event_bus::account_bus();
        let mut heard = bus.subscribe();
        let mut lobby = MajsoulBridge::new(None, None).with_account_bus(Some(bus));

        lobby.dispatch(&resp(
            METHOD_FETCH_ACCOUNT_INFO,
            json!({ "account": { "account_id": 7, "level": { "id": 10203, "score": 800 } } }),
        ));
        assert!(heard.try_recv().is_err(), "whose account is it?");

        lobby.dispatch(&resp(
            ".lq.Lobby.login",
            json!({
                "account_id": 7,
                "account": { "account_id": 7, "level": { "id": 0, "score": 0 } },
            }),
        ));
        assert_eq!(lobby.account_id, Some(7));
        assert!(heard.try_recv().is_err(), "no ladder played");
    }

    /// Autoplay tells a click that registered from one the UI swallowed by
    /// watching for the client's own uplink command. Both input methods
    /// must count, and only the uplink: a response coming back is not the
    /// client accepting anything.
    #[test]
    fn a_confirmed_result_is_counted() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));
        assert_eq!(watch.confirmed(), 0);
        let events = bridge.dispatch(&req(METHOD_CONFIRM_NEW_ROUND, json!({})));
        assert!(events.is_empty());
        assert_eq!(watch.confirmed(), 1);
    }

    #[test]
    fn only_successful_matching_responses_end_the_click_sequence() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));
        for method in [".lq.Lobby.startUnifiedMatch", ".lq.Lobby.matchGame"] {
            let before = watch.matching();
            bridge.dispatch(&req(method, json!({})));
            assert_eq!(watch.matching(), before, "a request can still fail");
            let mut response = req(method, json!({"error": {"code": 1001}}));
            response.msg_type = MessageType::Response;
            bridge.dispatch(&response);
            assert_eq!(watch.matching(), before);
            response.payload = json!({});
            assert!(bridge.dispatch(&response).is_empty());
            assert_eq!(watch.matching(), before + 1);
        }
    }

    #[test]
    fn input_commands_are_counted_for_click_verification() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));
        let ticket = watch.ticket();

        let events = bridge.dispatch(&req(
            METHOD_INPUT_OPERATION,
            json!({ "type": 1, "tile": "1m" }),
        ));
        assert!(events.is_empty(), "an input command is not an mjai event");
        assert!(watch.sent_since(ticket), "a discard must count");

        let ticket = watch.ticket();
        bridge.dispatch(&req(METHOD_INPUT_CHI_PENG_GANG, json!({ "type": 1 })));
        assert!(watch.sent_since(ticket), "a call must count");

        // The server's acknowledgement is not the client sending an input.
        let ticket = watch.ticket();
        bridge.dispatch(&resp(METHOD_INPUT_OPERATION, json!({})));
        assert!(!watch.sent_since(ticket));
    }

    /// The client acts on its own when a decision window runs out, and
    /// stamps that with `timeuse: 1000000`. Counting it would report a
    /// retry as having registered at the exact moment the press
    /// demonstrably failed — the one case the check exists to catch.
    #[test]
    fn the_clients_own_timeout_action_is_not_a_click() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));

        let ticket = watch.ticket();
        bridge.dispatch(&req(
            METHOD_INPUT_CHI_PENG_GANG,
            json!({ "cancel_operation": true, "index": 0, "timeuse": 1_000_000, "type": 0 }),
        ));
        assert!(
            !watch.sent_since(ticket),
            "a timed-out window is not a press"
        );

        // The explicit auto flag, where the message carries one.
        bridge.dispatch(&req(
            METHOD_INPUT_OPERATION,
            json!({ "auto_operation": true, "timeuse": 3, "type": 1, "tile": "1m" }),
        ));
        assert!(!watch.sent_since(ticket), "an auto action is not a press");

        // A human-scale think time still counts.
        bridge.dispatch(&req(
            METHOD_INPUT_CHI_PENG_GANG,
            json!({ "cancel_operation": true, "index": 0, "timeuse": 2, "type": 0 }),
        ));
        assert!(watch.sent_since(ticket));
    }

    /// A riichi and a plain discard of the same tile are both
    /// `inputOperation`; only the op type separates them. Autoplay needs
    /// that separation because a riichi plan whose declaration press was
    /// lost still sends the discard, and presence alone would read as
    /// success.
    #[test]
    fn a_riichi_is_distinguished_from_a_plain_discard() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));

        let ticket = watch.ticket();
        bridge.dispatch(&req(
            METHOD_INPUT_OPERATION,
            json!({ "type": 1, "tile": "4z", "moqie": false, "timeuse": 7 }),
        ));
        assert!(watch.sent_since(ticket), "the discard was sent");
        assert!(!watch.reach_since(ticket), "but it declared nothing");

        let ticket = watch.ticket();
        bridge.dispatch(&req(
            METHOD_INPUT_OPERATION,
            json!({ "type": 7, "tile": "4z", "moqie": false, "timeuse": 5 }),
        ));
        assert!(watch.reach_since(ticket), "type 7 is the declaration");

        // A call is not a riichi either, whatever its own type number.
        let ticket = watch.ticket();
        bridge.dispatch(&req(
            METHOD_INPUT_CHI_PENG_GANG,
            json!({ "type": 7, "index": 0, "timeuse": 2 }),
        ));
        assert!(watch.sent_since(ticket) && !watch.reach_since(ticket));
    }

    /// An own-turn window that runs out is answered by the client with a
    /// tsumogiri stamped with a plausible `timeuse` and *no*
    /// `auto_operation` flag — on the wire it is indistinguishable from a
    /// pressed tile, so `is_client_initiated` rightly lets it through. It
    /// must still not satisfy the button-press proof standard: a plan that
    /// clicked only action buttons (e.g. an ankan) never presses a tile,
    /// so a plain discard cannot mean its press landed.
    #[test]
    fn a_turn_timeout_tsumogiri_is_a_discard_not_a_button_press() {
        let watch: crate::autoplay::verify::SharedInputWatch = Default::default();
        let mut bridge = MajsoulBridge::new(None, None).with_input_watch(Some(watch.clone()));

        let ticket = watch.ticket();
        // The exact shape of the offending frame.
        bridge.dispatch(&req(
            METHOD_INPUT_OPERATION,
            json!({
                "auto_operation": false, "cancel_operation": false,
                "moqie": true, "tile": "4m", "timeuse": 5, "type": 1
            }),
        ));
        assert!(watch.sent_since(ticket), "it is a real input command");
        assert!(
            !watch.non_discard_since(ticket),
            "but a plain discard cannot prove an action-button press"
        );

        // A call — what a landed claim press actually produces — does.
        let ticket = watch.ticket();
        bridge.dispatch(&req(
            METHOD_INPUT_CHI_PENG_GANG,
            json!({ "type": 2, "index": 0, "timeuse": 3 }),
        ));
        assert!(watch.non_discard_since(ticket));
    }

    /// Without a watch attached (the state the bridge is in on the MITM
    /// path) the same frames must still parse harmlessly.
    #[test]
    fn input_commands_are_harmless_without_a_watch() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge.dispatch(&req(METHOD_INPUT_OPERATION, json!({ "type": 1 })));
        assert!(events.is_empty());
    }

    /// Full `.lq.FastTest.syncGame` response captured from a real mid-game
    /// reconnect (4p; our account at seat 3; 32 replay actions; East-1, no
    /// melds/riichi/kan). This is exactly the JSON the parser produces for the
    /// response — each action `data` is still base64 (decoded during replay).
    const SYNC_GAME_SAMPLE: &str = r#"{"game_restore":{"actions":[{"data":"","name":"ActionMJStart","step":0},{"data":"CAAQABgAIgI0cCICMW0iAjNwIgI0eiICN3oiAjZ6IgIwcCICM3MiAjBtIgI0cyICMXMiAjlzIgIyejIMqMMBqMMBqMMBqMMBQABYAGhFcgIxenoCCAB6AggBegIIAnoCCAOaAUA1NWQ2NzQ3MTRjNjAzODFhNGJjOTJmYzBmOWIwNWVjNDU4OWZlMzI0NTQ4OWVmOGY3NTc5NTVmMzIzZWIzZTE1qgFAYzFmNmY1YTQwOGFjMzgyZGEyZGE1MTA3OTUzYmUzYTg1N2M5OTdmNGNkMjg2M2JlZjczY2M3ZjE3MDllMDA4Mw==","name":"ActionNewRound","step":1},{"data":"CAASAjR6GAAoADAASAA=","name":"ActionDiscardTile","step":2},{"data":"CAEYRDgA","name":"ActionDealTile","step":3},{"data":"CAESAjJwGAAoATAASAA=","name":"ActionDiscardTile","step":4},{"data":"CAIYQzgA","name":"ActionDealTile","step":5},{"data":"CAISAjlwGAAoADAASAA=","name":"ActionDiscardTile","step":6},{"data":"CAMSAjdwGEIiDAgDEgIIASAAKOCnEjgA","name":"ActionDealTile","step":7},{"data":"CAMSAjd6GAAoADAASAA=","name":"ActionDiscardTile","step":8},{"data":"CAAYQTgA","name":"ActionDealTile","step":9},{"data":"CAASAjN6GAAoATAASAA=","name":"ActionDiscardTile","step":10},{"data":"CAEYQDgA","name":"ActionDealTile","step":11},{"data":"CAESAjR6GAAoADAASAA=","name":"ActionDiscardTile","step":12},{"data":"CAIYPzgA","name":"ActionDealTile","step":13},{"data":"CAISAjRwGAAiEwgDEgkIAhIFM3B8MHAgACjgpxIoATAASAA=","name":"ActionDiscardTile","step":14},{"data":"CAMSAjZ6GD4iDAgDEgIIASAAKOCnEjgA","name":"ActionDealTile","step":15},{"data":"CAMSAjR6GAAoADAASAA=","name":"ActionDiscardTile","step":16},{"data":"CAAYPTgA","name":"ActionDealTile","step":17},{"data":"CAASAjV6GAAoADAASAA=","name":"ActionDiscardTile","step":18},{"data":"CAEYPDgA","name":"ActionDealTile","step":19},{"data":"CAESAjNzGAAoATAASAA=","name":"ActionDiscardTile","step":20},{"data":"CAIYOzgA","name":"ActionDealTile","step":21},{"data":"CAISAjlzGAAoADAASAA=","name":"ActionDiscardTile","step":22},{"data":"CAMSAjRtGDoiDAgDEgIIASAAKOCnEjgA","name":"ActionDealTile","step":23},{"data":"CAMSAjFtGAAoADAASAA=","name":"ActionDiscardTile","step":24},{"data":"CAAYOTgA","name":"ActionDealTile","step":25},{"data":"CAASAjd6GAAoADAASAA=","name":"ActionDiscardTile","step":26},{"data":"CAEYODgA","name":"ActionDealTile","step":27},{"data":"CAESAjZ6GAAiEwgDEgkIAxIFNnp8NnogACjgpxIoADAASAA=","name":"ActionDiscardTile","step":28},{"data":"CAIYNzgA","name":"ActionDealTile","step":29},{"data":"CAISAjF6GAAoADAASAA=","name":"ActionDiscardTile","step":30},{"data":"CAMSAjdzGDYiDAgDEgIIASAAKOCnEjgA","name":"ActionDealTile","step":31}],"game_state":1,"last_pause_time_ms":0,"passed_waiting_time":16,"start_time":0},"is_end":false,"step":32}"#;

    /// Resolve the bridge to seat 3 / 4 players via a real authGame exchange,
    /// matching `SYNC_GAME_SAMPLE` (our account_id 12345 sits at `seat_list[3]`).
    fn seated_for_sample() -> MajsoulBridge {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 12345 })));
        bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{ "account_id": 12345, "nickname": "player_a" }],
                "seat_list": [10u64, 20, 30, 12345u64],
            }),
        ));
        assert_eq!(bridge.seat, Some(3));
        assert_eq!(bridge.num_players, 4);
        bridge
    }

    /// A `.lq.FastTest.syncGame` response replays the whole in-progress kyoku
    /// through the normal conversion path, rebuilding the mjai stream. Regression
    /// for mid-game reconnect recovery (issue #147).
    #[test]
    fn sync_game_replays_in_progress_kyoku() {
        let mut bridge = seated_for_sample();
        let payload: JsonValue = serde_json::from_str(SYNC_GAME_SAMPLE).unwrap();
        let events = bridge.dispatch(&resp(METHOD_SYNC_GAME, payload));

        // 1 start_kyoku + 16 tsumo (dealer's opening draw + 15 draws) + 15 dahai.
        assert_eq!(events.len(), 32, "unexpected replay event count");

        // The replay must NOT emit start_game — that belongs to the preceding
        // authGame (the single downstream reset point).
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, MjaiEvent::StartGame { .. })),
            "replay must not emit start_game"
        );
        // Exactly one start_kyoku, and it is the very first event.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, MjaiEvent::StartKyoku { .. }))
                .count(),
            1
        );
        match &events[0] {
            MjaiEvent::StartKyoku {
                bakaze,
                kyoku,
                oya,
                honba,
                kyotaku,
                num_players,
                tehais,
                ..
            } => {
                assert_eq!(bakaze.as_str(), "E");
                assert_eq!(*kyoku, 1);
                assert_eq!(*oya, 0);
                assert_eq!(*honba, 0);
                assert_eq!(*kyotaku, 0);
                assert_eq!(*num_players, 4);
                // Our seat-3 tehai is fully known (13 real tiles, no "?"); the
                // restore actions decoded WITHOUT the XOR step.
                assert_eq!(tehais[3].len(), TEHAI_SIZE);
                assert!(tehais[3].iter().all(|t| t != UNKNOWN_TILE));
                // Opponents' hands stay hidden.
                assert!(tehais[0].iter().all(|t| t == UNKNOWN_TILE));
            }
            other => panic!("expected StartKyoku first, got {other:?}"),
        }
        // Dealer's opening draw is unknown to us.
        assert!(matches!(&events[1], MjaiEvent::Tsumo { actor: 0, pai } if pai == "?"));
        // First discard: seat 0 cuts 4z (= mjai "N"), tedashi.
        assert!(
            matches!(&events[2], MjaiEvent::Dahai { actor: 0, pai, tsumogiri: false } if pai == "N")
        );
        // Our own draw (step 7) carries the real tile 7p.
        assert!(events
            .iter()
            .any(|e| matches!(e, MjaiEvent::Tsumo { actor: 3, pai } if pai == "7p")));
        // Replay ends on our latest draw (step 31): 7s, known.
        assert!(
            matches!(events.last().unwrap(), MjaiEvent::Tsumo { actor: 3, pai } if pai == "7s")
        );

        let dahai = events
            .iter()
            .filter(|e| matches!(e, MjaiEvent::Dahai { .. }))
            .count();
        let tsumo = events
            .iter()
            .filter(|e| matches!(e, MjaiEvent::Tsumo { .. }))
            .count();
        assert_eq!(dahai, 15);
        assert_eq!(tsumo, 16);
    }

    /// A riichi declared on the LAST replayed action leaves `reach_accepted`
    /// pending; it must be drained (prepended) onto the first subsequent *live*
    /// action — identical to live semantics, proving the replay loop carries
    /// `pending_reach_accepted` across the replay→live boundary.
    #[test]
    fn game_restore_riichi_drains_on_next_live_action() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);

        // GameRestore whose only action is a riichi discard by seat 1
        // (ActionDiscardTile{seat:1, tile:"5p", is_liqi:true}, plain base64).
        let restore = resp(
            METHOD_SYNC_GAME,
            json!({
                "game_restore": {
                    "actions": [
                        { "name": "ActionDiscardTile", "data": "CAESAjVwGAE=", "step": 1 }
                    ]
                },
                "is_end": false,
                "step": 2
            }),
        );
        let events = bridge.dispatch(&restore);
        assert!(matches!(&events[0], MjaiEvent::Reach { actor: 1, pai, .. } if pai.is_none()));
        assert!(matches!(&events[1], MjaiEvent::Dahai { actor: 1, pai, .. } if pai == "5p"));
        assert_eq!(bridge.pending_reach_accepted, Some(1));

        // First live action after the replay drains the queued reach_accepted.
        let live = bridge.dispatch(&action_msg("ActionDealTile", json!({ "seat": 2 })));
        assert!(matches!(&live[0], MjaiEvent::ReachAccepted { actor: 1 }));
        assert!(matches!(&live[1], MjaiEvent::Tsumo { actor: 2, pai } if pai == "?"));
        assert_eq!(bridge.pending_reach_accepted, None);
    }

    /// A first-entry `.lq.FastTest.enterGame` (or any response) with no replay
    /// actions is a no-op — no events, no panic.
    #[test]
    fn enter_game_empty_restore_is_noop() {
        let mut bridge = seated_for_sample();
        // Empty actions array.
        let empty = bridge.dispatch(&resp(
            METHOD_ENTER_GAME,
            json!({ "game_restore": { "actions": [] }, "is_end": false, "step": 0 }),
        ));
        assert!(empty.is_empty());
        // Only the leading ActionMJStart (no handler) → still nothing.
        let mj_start_only = bridge.dispatch(&resp(
            METHOD_ENTER_GAME,
            json!({
                "game_restore": {
                    "actions": [{ "name": "ActionMJStart", "data": "", "step": 0 }]
                }
            }),
        ));
        assert!(mj_start_only.is_empty());
        // Missing game_restore entirely.
        let missing = bridge.dispatch(&resp(METHOD_ENTER_GAME, json!({ "is_end": false })));
        assert!(missing.is_empty());
    }

    #[test]
    fn auth_game_resolves_seat_and_names() {
        let mut bridge = MajsoulBridge::new(None, None);

        // Req captures account_id, no events yet.
        let events = bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 12345 })));
        assert!(events.is_empty());
        assert_eq!(bridge.account_id, Some(12345));
        assert_eq!(bridge.seat, None);

        // Res shape mirrors a real authGame response against AI: one human in
        // `players`, three robots referenced only by id in `seat_list`.
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{ "account_id": 12345, "nickname": "player_a" }],
                "seat_list": [1, 3, 12345u64, 2],
            }),
        ));
        assert_eq!(bridge.seat, Some(2));
        assert_eq!(events.len(), 1);
        match &events[0] {
            MjaiEvent::StartGame {
                id,
                names,
                num_players,
                ..
            } => {
                assert_eq!(*id, Some(2));
                assert_eq!(*num_players, 4);
                assert_eq!(
                    names,
                    &vec![
                        "".to_string(),
                        "".to_string(),
                        "player_a".to_string(),
                        "".to_string()
                    ]
                );
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
        assert_eq!(bridge.num_players, 4);
    }

    #[test]
    fn auth_game_carries_private_length_and_reconnect_metadata() {
        let mut bridge = MajsoulBridge::new(None, None);
        let game_uuid = "230723-test-game-uuid";
        bridge.dispatch(&req(
            METHOD_AUTH_GAME,
            json!({ "account_id": 100, "game_uuid": game_uuid }),
        ));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "game_config": {
                    "category": 2,
                    "meta": { "mode_id": 12 },
                    "mode": { "mode": 2 }
                },
                "players": [
                    { "account_id": 100, "nickname": "me" }
                ],
                "seat_list": [100u64, 1u64, 2u64, 3u64]
            }),
        ));
        match &events[0] {
            MjaiEvent::StartGame { game_meta, .. } => {
                let meta = game_meta.as_ref().expect("Mahjong Soul metadata");
                assert_eq!(meta.game_id, Some(stable_game_id(game_uuid)));
                assert_eq!(meta.match_mode, Some(2));
                match &meta.match_info {
                    Some(MatchInfo::Majsoul {
                        game_uuid: uuid,
                        mode_id,
                        room_id,
                        contest_uid,
                    }) => {
                        assert_eq!(uuid.as_deref(), Some(game_uuid));
                        assert_eq!(*mode_id, Some(12));
                        assert_eq!(*room_id, None);
                        assert_eq!(*contest_uid, None);
                    }
                    other => panic!("expected Majsoul match_info, got {other:?}"),
                }
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
        assert!(serde_json::to_value(&events[0])
            .unwrap()
            .get("game_meta")
            .is_none());
    }

    #[test]
    fn names_filled_for_four_human_players() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 100 })));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [
                    { "account_id": 100, "nickname": "alice" },
                    { "account_id": 200, "nickname": "bob" },
                    { "account_id": 300, "nickname": "carol" },
                    { "account_id": 400, "nickname": "dave" },
                ],
                "seat_list": [200u64, 100u64, 400u64, 300u64],
            }),
        ));
        match &events[0] {
            MjaiEvent::StartGame { id, names, .. } => {
                assert_eq!(*id, Some(1));
                assert_eq!(
                    names,
                    &vec![
                        "bob".to_string(),
                        "alice".to_string(),
                        "dave".to_string(),
                        "carol".to_string(),
                    ]
                );
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
    }

    /// Real captured 3p authGame response (single human + 2 robots, AI casual
    /// match `mode_id=0`). Verifies (a) num_players resolves to 3 from
    /// `seat_list.len()`, (b) emitted names array is length 3 with the human
    /// nickname at the right seat, (c) seat resolves to the human's index in
    /// the seat_list. `mode_id=0` is normal for AI rooms (only ranked sanma
    /// rooms use 21/22/26) — bridge logs it but doesn't gate on it.
    #[test]
    fn three_player_real_auth_game_payload() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 12345 })));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "game_config": {
                    "category": 1,
                    "meta": { "contest_uid": 0, "mode_id": 0, "room_id": 33123 },
                    "mode": { "ai": true, "mode": 12 }
                },
                "players": [{ "account_id": 12345, "nickname": "player_a" }],
                "robots": [
                    { "account_id": 1, "nickname": "" },
                    { "account_id": 2, "nickname": "" },
                ],
                "seat_list": [2u64, 12345u64, 1u64],
            }),
        ));
        assert_eq!(bridge.seat, Some(1));
        assert_eq!(bridge.num_players, 3);
        match &events[0] {
            MjaiEvent::StartGame {
                id,
                names,
                num_players,
                game_meta,
                ..
            } => {
                assert_eq!(*id, Some(1));
                assert_eq!(*num_players, 3);
                assert_eq!(
                    names,
                    &vec!["".to_string(), "player_a".to_string(), "".to_string()]
                );
                // Zero-valued meta ids mean "not applicable" and are dropped;
                // the friendly/AI room number is kept as-is.
                match &game_meta.as_ref().expect("game meta").match_info {
                    Some(MatchInfo::Majsoul {
                        mode_id,
                        room_id,
                        contest_uid,
                        ..
                    }) => {
                        assert_eq!(*mode_id, None);
                        assert_eq!(*room_id, Some(33123));
                        assert_eq!(*contest_uid, None);
                    }
                    other => panic!("expected Majsoul match_info, got {other:?}"),
                }
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
    }

    /// 3-player table: `seat_list` is length 3, names array stays length 3
    /// (no padding to 4), and `num_players` is stamped on the StartGame event.
    #[test]
    fn three_player_seat_list_emits_length_three_names_and_num_players() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 50 })));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{ "account_id": 50, "nickname": "me" }],
                "seat_list": [1, 50u64, 2],
            }),
        ));
        match &events[0] {
            MjaiEvent::StartGame {
                id,
                names,
                num_players,
                ..
            } => {
                assert_eq!(*id, Some(1));
                assert_eq!(*num_players, 3);
                assert_eq!(
                    names,
                    &vec!["".to_string(), "me".to_string(), "".to_string()]
                );
            }
            other => panic!("expected StartGame, got {other:?}"),
        }
        assert_eq!(bridge.num_players, 3);
    }

    #[test]
    fn auth_game_response_without_request_warns_and_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({ "seat_list": [1, 2, 3, 4] }),
        ));
        assert!(events.is_empty());
        assert_eq!(bridge.seat, None);
    }

    #[test]
    fn auth_game_account_id_not_in_seat_list_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 999 })));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({ "seat_list": [1, 2, 3, 4] }),
        ));
        assert!(events.is_empty());
        assert_eq!(bridge.seat, None);
    }

    #[test]
    fn unrelated_methods_pass_through() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge.dispatch(&req(
            ".lq.Lobby.heatbeat",
            json!({ "no_operation_counter": 0 }),
        ));
        assert!(events.is_empty());
    }

    /// Each `start_game` event should land in a freshly-opened
    /// `majsoul_<ts>.mjai.jsonl` file under `<session>/majsoul/`. The
    /// previous game's file must NOT receive subsequent events.
    #[test]
    fn start_game_rotates_mjai_log_and_appends_event() {
        use crate::logger::FlowLogger;
        use std::fs;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let mut bridge = MajsoulBridge::new(None, None);

        // Manually install a "first game" log to simulate what
        // rotate_mjai_log would have done — without needing a full Session.
        let first = Arc::new(
            FlowLogger::new(tmp.path(), "majsoul", "majsoul_first.mjai.jsonl", "first").unwrap(),
        );
        bridge.mjai_log = Some(first);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 1 })));
        let events = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{ "account_id": 1, "nickname": "p1" }],
                "seat_list": [1u64, 2, 3, 4],
            }),
        ));
        assert_eq!(events.len(), 1);
        bridge.write_mjai(&events);

        // Now simulate a second game on the same flow: rotate, then write.
        // Sleep > 1ms so the timestamp differs (filename includes millis).
        std::thread::sleep(Duration::from_millis(2));
        let second = Arc::new(
            FlowLogger::new(tmp.path(), "majsoul", "majsoul_second.mjai.jsonl", "second").unwrap(),
        );
        bridge.mjai_log = Some(second);
        bridge.dispatch(&req(METHOD_AUTH_GAME, json!({ "account_id": 2 })));
        let events2 = bridge.dispatch(&resp(
            METHOD_AUTH_GAME,
            json!({
                "players": [{ "account_id": 2, "nickname": "p2" }],
                "seat_list": [5u64, 2u64, 6, 7],
            }),
        ));
        bridge.write_mjai(&events2);

        let first_content =
            fs::read_to_string(tmp.path().join("majsoul/majsoul_first.mjai.jsonl")).unwrap();
        let second_content =
            fs::read_to_string(tmp.path().join("majsoul/majsoul_second.mjai.jsonl")).unwrap();

        // First file: exactly one start_game with id=0 (account 1 at index 0).
        let first_lines: Vec<&str> = first_content.lines().collect();
        assert_eq!(first_lines.len(), 1, "first file should have one line");
        assert!(first_lines[0].contains(r#""type":"start_game""#));
        assert!(first_lines[0].contains(r#""id":0"#));
        assert!(first_lines[0].contains(r#""p1""#));

        // Second file: exactly one start_game with id=1 (account 2 at index 1).
        let second_lines: Vec<&str> = second_content.lines().collect();
        assert_eq!(second_lines.len(), 1, "second file should have one line");
        assert!(second_lines[0].contains(r#""type":"start_game""#));
        assert!(second_lines[0].contains(r#""id":1"#));
        assert!(second_lines[0].contains(r#""p2""#));

        // First file must not have leaked the second game's data.
        assert!(!first_content.contains(r#""p2""#));
    }

    /// `rotate_mjai_log` is a no-op when no session is wired (parse-only mode).
    #[test]
    fn rotate_without_session_is_noop() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.rotate_mjai_log();
        assert!(bridge.mjai_log.is_none());
    }

    /// Helper: build an ActionPrototype Notify ParsedMessage carrying an
    /// already-decoded ActionNewRound payload (mirrors what the parser
    /// produces after `maybe_decode_action`).
    fn new_round_msg(action_data: JsonValue) -> ParsedMessage {
        ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_ACTION_PROTOTYPE),
            payload: json!({
                "step": 1,
                "name": ACTION_NEW_ROUND,
                "data": action_data,
            }),
        }
    }

    /// Sample ActionNewRound payload from the user-supplied real Majsoul
    /// frame: human player (us) at seat 2, ju=0 → dealer is seat 0 (a bot).
    /// We're not the dealer, so we get exactly 13 tiles.
    #[test]
    fn action_new_round_non_dealer_emits_start_kyoku_then_unknown_tsumo() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = new_round_msg(json!({
            "doras": ["4s"],
            "left_tile_count": 69,
            "scores": [25000, 25000, 25000, 25000],
            "tiles": ["7p","3p","3s","1s","2z","2m","8m","6p","7m","5m","0s","6s","7z"],
        }));

        let events = bridge.dispatch(&msg);
        assert_eq!(events.len(), 2);

        match &events[0] {
            MjaiEvent::StartKyoku {
                bakaze,
                dora_marker,
                kyoku,
                honba,
                kyotaku,
                oya,
                scores,
                tehais,
                num_players,
            } => {
                assert_eq!(bakaze, "E");
                assert_eq!(dora_marker, "4s");
                assert_eq!(*kyoku, 1);
                assert_eq!(*honba, 0);
                assert_eq!(*kyotaku, 0);
                assert_eq!(*oya, 0);
                assert_eq!(*num_players, 4);
                assert_eq!(scores, &vec![25000, 25000, 25000, 25000]);
                // Other seats stay unknown.
                for s in [0, 1, 3] {
                    assert!(tehais[s].iter().all(|t| t == "?"));
                }
                // Our row: 13 tiles, sorted, mapped (0s → 5sr, 2z → S, 7z → C).
                assert_eq!(
                    tehais[2],
                    vec![
                        "2m", "5m", "7m", "8m", "3p", "6p", "7p", "1s", "3s", "5sr", "6s", "S",
                        "C",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
                );
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 0); // dealer
                assert_eq!(pai, "?");
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// Same shape but with us as dealer (seat 0, ju=0). We get 14 tiles;
    /// the 14th is our opening tsumo (kept raw, not sorted in).
    #[test]
    fn action_new_round_dealer_emits_start_kyoku_then_self_tsumo() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let msg = new_round_msg(json!({
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": [
                "1m","2m","3m","4m","5m","6m","7m","8m","9m",
                "1p","2p","3p","4p",
                "0p"
            ],
        }));

        let events = bridge.dispatch(&msg);
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::StartKyoku {
                oya,
                tehais,
                dora_marker,
                ..
            } => {
                assert_eq!(*oya, 0);
                assert_eq!(dora_marker, "1m");
                // First 13 tiles, sorted (already in mjai order here).
                assert_eq!(
                    tehais[0],
                    [
                        "1m", "2m", "3m", "4m", "5m", "6m", "7m", "8m", "9m", "1p", "2p", "3p",
                        "4p",
                    ]
                    .map(String::from)
                );
                for s in [1, 2, 3] {
                    assert!(tehais[s].iter().all(|t| t == "?"));
                }
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai, "5pr"); // 0p → red 5p
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// chang/ju/honba/liqibang explicit non-defaults are reflected verbatim.
    #[test]
    fn action_new_round_propagates_chang_ju_honba_liqibang() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        let msg = new_round_msg(json!({
            "chang": 1,
            "ju": 2,
            "ben": 3,
            "liqibang": 1,
            "doras": ["7m"],
            "scores": [24000, 26000, 25000, 25000],
            "tiles": ["1m","2m","3m","4m","5m","6m","7m","8m","9m","1p","2p","3p","4p"],
        }));
        let events = bridge.dispatch(&msg);
        match &events[0] {
            MjaiEvent::StartKyoku {
                bakaze,
                kyoku,
                honba,
                kyotaku,
                oya,
                scores,
                ..
            } => {
                assert_eq!(bakaze, "S");
                assert_eq!(*kyoku, 3); // ju + 1
                assert_eq!(*oya, 2);
                assert_eq!(*honba, 3);
                assert_eq!(*kyotaku, 1);
                assert_eq!(scores, &vec![24000, 26000, 25000, 25000]);
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
    }

    /// Real captured 3p `ActionNewRound` payload (East 1, honba 1, dealer at
    /// seat 0 = user). Confirms the bridge handles a real 3p deal end-to-end:
    /// length-3 scores, length-3 tehais, dealer's 14th tile becomes opening
    /// tsumo. dora_marker `4z` → `N`. `left_tile_count` 54 = 108 − 14 dead
    /// wall − 13×3 dealt − 1 dealer's first draw.
    #[test]
    fn action_new_round_three_player_real_payload() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        let msg = new_round_msg(json!({
            "al": false,
            "ben": 1,
            "chang": 0,
            "doras": ["4z"],
            "ju": 0,
            "left_tile_count": 54,
            "liqibang": 0,
            "scores": [37000, 34000, 34000],
            "tiles": ["7s","9m","6s","8s","1m","6p","9s","1p","5z","9s","8s","2s","4p","6p"],
            "tingpais0": [],
            "tingpais1": [],
        }));
        let events = bridge.dispatch(&msg);
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::StartKyoku {
                bakaze,
                dora_marker,
                kyoku,
                honba,
                kyotaku,
                oya,
                scores,
                tehais,
                num_players,
            } => {
                assert_eq!(bakaze, "E");
                assert_eq!(dora_marker, "N");
                assert_eq!(*kyoku, 1);
                assert_eq!(*honba, 1);
                assert_eq!(*kyotaku, 0);
                assert_eq!(*oya, 0);
                assert_eq!(*num_players, 3);
                assert_eq!(scores, &vec![37000, 34000, 34000]);
                assert_eq!(tehais.len(), 3, "no phantom 4th seat");
                // Dealer's 13 tiles, sorted, mapped (5z → P). Note the
                // captured payload has TWO 6p (index 5 in tehai and index
                // 13 = opening tsumo), so the sorted tehai has one 6p, two
                // 8s and two 9s.
                assert_eq!(
                    tehais[0],
                    vec![
                        "1m", "9m", "1p", "4p", "6p", "2s", "6s", "7s", "8s", "8s", "9s", "9s",
                        "P",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
                );
                for s in [1, 2] {
                    assert!(tehais[s].iter().all(|t| t == "?"));
                }
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 0);
                // 14th tile in payload (6p) is the dealer's opening tsumo.
                assert_eq!(pai, "6p");
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// Synthetic 3p table: native-length scores/tehais (no padding), with the
    /// user as dealer. Slimmer fixture than the real-payload test above.
    #[test]
    fn action_new_round_three_player_emits_native_length() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        let msg = new_round_msg(json!({
            "doras": ["1m"],
            "scores": [35000, 35000, 35000],
            "tiles": [
                "1m","9m",
                "1p","2p","3p","4p","5p","6p","7p","8p","9p",
                "1s","2s","3s"
            ],
        }));
        let events = bridge.dispatch(&msg);
        match &events[0] {
            MjaiEvent::StartKyoku {
                scores,
                tehais,
                num_players,
                ..
            } => {
                assert_eq!(*num_players, 3);
                assert_eq!(scores, &vec![35000, 35000, 35000]);
                assert_eq!(tehais.len(), 3, "no phantom 4th seat");
                for hand in tehais {
                    assert_eq!(hand.len(), 13);
                }
            }
            other => panic!("expected StartKyoku, got {other:?}"),
        }
    }

    /// Tile count mismatch with seat role must error out (no events emitted).
    #[test]
    fn action_new_round_seat_role_mismatch_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        // We say we're seat 0 but ju=1 (dealer is seat 1) and we got 14 tiles.
        bridge.seat = Some(0);
        let msg = new_round_msg(json!({
            "ju": 1,
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": [
                "1m","2m","3m","4m","5m","6m","7m","8m","9m",
                "1p","2p","3p","4p","5p"
            ],
        }));
        let events = bridge.dispatch(&msg);
        assert!(events.is_empty(), "mismatch should produce no events");
    }

    /// ActionNewRound before authGame (seat unresolved) must skip gracefully.
    #[test]
    fn action_new_round_before_seat_resolved_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        let msg = new_round_msg(json!({
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": ["1m","2m","3m","4m","5m","6m","7m","8m","9m","1p","2p","3p","4p"],
        }));
        let events = bridge.dispatch(&msg);
        assert!(events.is_empty());
    }

    fn action_msg(name: &str, data: JsonValue) -> ParsedMessage {
        ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_ACTION_PROTOTYPE),
            payload: json!({ "step": 1, "name": name, "data": data }),
        }
    }

    fn opening_dealer(last: &str, hand_five: &str) -> MajsoulBridge {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        let opening = bridge.dispatch(&new_round_msg(json!({
            "ju": 1, "doras": ["9p"], "scores": [25000,25000,25000,25000],
            "tiles": ["1m","1m","2m","3m","4m",hand_five,"6m",
                      "1p","2p","3p","4s","5s","6s",last]
        })));
        assert_eq!(opening.len(), 2);
        bridge
    }

    #[test]
    fn opening_deal_normalizes_only_a_tile_absent_from_the_head() {
        for (last, five, discard, wire, expected) in [
            ("3z", "5m", "3z", false, true),
            ("4z", "5m", "4z", false, true),
            ("8s", "5m", "8s", false, true),
            ("1m", "5m", "1m", false, false), // real hand copy
            ("0m", "5m", "0m", false, true),  // red is not ordinary
            ("5m", "0m", "5m", false, true),
            ("5m", "5m", "5m", false, false),
            ("4z", "5m", "1m", false, false),
            ("4z", "5m", "4z", true, true),
            ("4z", "5m", "1m", true, false), // partition, not the wire's slot
            ("1m", "5m", "1m", true, true),  // identical draw and hand copy
        ] {
            let mut bridge = opening_dealer(last, five);
            let events = bridge.dispatch(&action_msg(
                ACTION_DISCARD_TILE,
                json!({
                    "seat": 1, "tile": discard, "moqie": wire
                }),
            ));
            assert!(
                matches!(&events[..], [MjaiEvent::Dahai { tsumogiri, .. }] if *tsumogiri == expected),
                "last={last} five={five} discard={discard}: {events:?}"
            );
            assert!(bridge.opening_deal.is_none());
            // No second discard, even of that tile, inherits the opening rule.
            let next = bridge.dispatch(&action_msg(
                ACTION_DISCARD_TILE,
                json!({
                    "seat": 1, "tile": discard, "moqie": false
                }),
            ));
            assert!(matches!(
                &next[..],
                [MjaiEvent::Dahai {
                    tsumogiri: false,
                    ..
                }]
            ));
        }
    }

    #[test]
    fn opening_normalization_ends_at_draw_kan_kita_or_hand_end() {
        for (name, data) in [
            (ACTION_DEAL_TILE, json!({"seat": 1, "tile": "7s"})),
            (
                ACTION_AN_GANG_ADD_GANG,
                json!({"seat": 1, "type": 3, "tiles": "1m"}),
            ),
            (ACTION_BA_BEI, json!({"seat": 1})),
            (ACTION_NO_TILE, json!({})),
            (ACTION_LIU_JU, json!({"type": 1, "seat": 1})),
        ] {
            let mut bridge = opening_dealer("4z", "5m");
            assert!(
                !bridge.dispatch(&action_msg(name, data)).is_empty(),
                "{name}"
            );
            assert!(bridge.opening_deal.is_none(), "{name}");
        }
        let mut bridge = opening_dealer("4z", "5m");
        bridge.dispatch(&new_round_msg(json!({
            "ju": 2, "doras": ["9p"], "scores": [25000,25000,25000,25000],
            "tiles": ["1m","1m","2m","3m","4m","5m","6m",
                      "1p","2p","3p","4s","5s","6s"]
        })));
        assert!(bridge.opening_deal.is_none());
    }

    #[test]
    fn opening_heavenly_win_uses_one_partition_and_preserves_provider_scoring() {
        for tiles in [
            [
                "1m", "1m", "1m", "2p", "2p", "2p", "3s", "3s", "3s", "6z", "6z", "6z", "1z", "1z",
            ],
            [
                "0m", "5m", "5m", "1m", "2m", "3m", "4p", "5p", "6p", "7s", "8s", "9s", "1z", "1z",
            ],
        ] {
            // Neither the deal's last slot nor the provider's hu_tile has
            // a unique mahjong meaning before the dealer's first action.
            for draw in 0..tiles.len() {
                let mut ordered = tiles.to_vec();
                let synthetic = ordered.remove(draw);
                ordered.push(synthetic);
                for won in 0..tiles.len() {
                    let mut hand = tiles.to_vec();
                    let hu_tile = hand.remove(won);
                    hand.reverse(); // reveal order need not match the deal
                    let mut bridge = MajsoulBridge::new(None, None);
                    bridge.seat = Some(1);
                    let opening = bridge.build_start_kyoku(&json!({
                        "ju": 1, "doras": ["9p"], "scores": [25000,25000,25000,25000], "tiles": ordered
                    })).unwrap();
                    let settlement = json!({
                        "delta_scores": [-48000,144000,-48000,-48000],
                        "scores": [-23000,169000,-23000,-23000],
                        "hules": [{"seat": 1, "zimo": true, "hu_tile": hu_tile,
                            "hand": hand, "fans": [{"id": 35, "val": 1}, {"id": 48, "val": 2}],
                            "count": 3, "fu": 0, "yiman": true}]
                    });
                    let events = bridge.dispatch(&action_msg(ACTION_HULE, settlement.clone()));
                    assert_eq!(events.len(), 2);
                    let MjaiEvent::StartKyoku { tehais, .. } = &opening[0] else {
                        panic!()
                    };
                    let MjaiEvent::Tsumo { pai: draw_tile, .. } = &opening[1] else {
                        panic!()
                    };
                    let MjaiEvent::Hora {
                        pai,
                        tehai,
                        score,
                        deltas,
                        scores,
                        actor,
                        target,
                        ..
                    } = &events[0]
                    else {
                        panic!()
                    };
                    assert_eq!((*actor, *target), (1, 1));
                    assert_eq!(pai.as_ref(), Some(draw_tile));
                    assert_eq!(tehai.as_ref(), Some(&tehais[1]));
                    assert_eq!(*deltas, Some(vec![-48000, 144000, -48000, -48000]));
                    assert_eq!(*scores, Some(vec![-23000, 169000, -23000, -23000]));
                    let score = score.as_ref().unwrap();
                    assert_eq!(score.yaku, vec![(35, 1), (48, 2)]);
                    assert_eq!((score.han, score.fu, score.yakuman), (3, 0, true));
                    assert!(matches!(events[1], MjaiEvent::EndKyoku));
                    assert!(bridge.opening_deal.is_none());
                    assert_eq!(settlement["hules"][0]["hu_tile"], hu_tile);
                }
            }
        }
    }

    #[test]
    fn opening_win_rejects_a_different_multiset_or_missing_reveal() {
        let deal = OpeningDeal {
            hand: [
                "5mr", "5m", "5m", "1m", "2m", "3m", "4p", "5p", "6p", "7s", "8s", "9s", "E",
            ]
            .map(String::from)
            .to_vec(),
            draw: "E".into(),
        };
        let mut wrong_red = deal.hand.clone();
        wrong_red[0] = "5m".into();
        for (pai, hand) in [
            (None, Some(deal.hand.clone())),
            (Some(deal.draw.clone()), None),
            (Some("S".into()), Some(deal.hand.clone())),
            (Some(deal.draw.clone()), Some(wrong_red)),
            (Some(deal.draw.clone()), Some(deal.hand[..12].to_vec())),
        ] {
            let (mut pai, mut hand) = (pai, hand);
            let before = (pai.clone(), hand.clone());
            assert!(deal.normalize_win(&mut pai, &mut hand).is_err());
            assert_eq!((pai, hand), before);
        }
    }

    #[test]
    fn opponents_opening_discard_and_win_keep_the_wire_identity() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        bridge
            .build_start_kyoku(&json!({
                "ju": 1, "doras": ["9p"], "scores": [25000,25000,25000,25000],
                "tiles": ["1m","1m","2m","3m","4m","5m","6m","1p","2p","3p","4s","5s","6s"]
            }))
            .unwrap();
        assert!(bridge.opening_deal.is_none());
        let hand = [
            "5m", "5m", "1m", "2m", "3m", "4p", "5p", "6p", "7s", "8s", "9s", "1z", "1z",
        ];
        let win = bridge
            .build_hule(&json!({"hules": [{"seat": 1, "zimo": true,
            "hu_tile": "0m", "hand": hand}]}))
            .unwrap();
        assert!(
            matches!(&win[0], MjaiEvent::Hora { pai: Some(pai), tehai: Some(hand), .. }
            if pai == "5mr" && hand.len() == 13 && hand[0] == "5m")
        );
        let discard = bridge
            .build_dahai(&json!({"seat": 1, "tile": "0m", "moqie": false}))
            .unwrap();
        assert!(matches!(
            &discard[0],
            MjaiEvent::Dahai {
                tsumogiri: false,
                ..
            }
        ));
    }

    #[test]
    fn opening_riichi_keeps_one_discard_and_acceptance_order() {
        let mut bridge = opening_dealer("4z", "5m");
        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({
                "seat": 1, "tile": "4z", "moqie": false, "is_wliqi": true
            }),
        ));
        assert!(matches!(
            &events[..],
            [
                MjaiEvent::Reach { actor: 1, .. },
                MjaiEvent::Dahai {
                    actor: 1,
                    tsumogiri: true,
                    ..
                }
            ]
        ));
        let next = bridge.dispatch(&action_msg(ACTION_DEAL_TILE, json!({"seat": 2})));
        assert!(matches!(
            &next[..],
            [
                MjaiEvent::ReachAccepted { actor: 1 },
                MjaiEvent::Tsumo { actor: 2, .. }
            ]
        ));
    }

    #[test]
    fn restored_opening_uses_the_same_normalization_as_live_actions() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use prost::Message;

        let encode = |name: &str, data: JsonValue| {
            let desc = parser::POOL
                .get_message_by_name(&format!("lq.{name}"))
                .unwrap();
            let text = data.to_string();
            let message = prost_reflect::DynamicMessage::deserialize(
                desc,
                &mut serde_json::Deserializer::from_str(&text),
            )
            .unwrap();
            STANDARD.encode(message.encode_to_vec())
        };
        let mut bridge = opening_dealer("4z", "5m");
        let opening = json!({
            "ju": 1, "doras": ["9p"], "scores": [25000,25000,25000,25000],
            "tiles": ["1m","1m","2m","3m","4m","5m","6m",
                      "1p","2p","3p","4s","5s","6s","3z"]
        });
        let events = bridge.dispatch(&resp(METHOD_SYNC_GAME, json!({
            "game_restore": {"actions": [
                {"name": ACTION_NEW_ROUND, "data": encode(ACTION_NEW_ROUND, opening), "step": 1},
                {"name": ACTION_DISCARD_TILE, "data": encode(ACTION_DISCARD_TILE,
                    json!({"seat": 1, "tile": "3z", "moqie": false})), "step": 2},
                {"name": ACTION_DEAL_TILE, "data": encode(ACTION_DEAL_TILE,
                    json!({"seat": 2})), "step": 3}
            ]}, "step": 4, "is_end": false
        })));
        assert!(matches!(&events[..], [MjaiEvent::StartKyoku { .. },
            MjaiEvent::Tsumo { actor: 1, pai },
            MjaiEvent::Dahai { actor: 1, tsumogiri: true, .. },
            MjaiEvent::Tsumo { actor: 2, .. }] if pai == "W"));
        assert!(bridge.opening_deal.is_none());
    }

    /// Other player's draw — server omits the tile field. We must emit
    /// tsumo with pai = "?" rather than fabricating one.
    #[test]
    fn action_deal_tile_other_player_uses_unknown() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_DEAL_TILE,
            json!({ "left_tile_count": 68, "seat": 1 }),
        );
        let events = bridge.dispatch(&msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "?");
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// Our own draw — server tells us the actual tile, mapped through
    /// `ms_to_mjai`.
    #[test]
    fn action_deal_tile_self_uses_real_tile() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_DEAL_TILE,
            json!({ "left_tile_count": 67, "seat": 2, "tile": "0p" }),
        );
        let events = bridge.dispatch(&msg);
        match &events[0] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 2);
                assert_eq!(pai, "5pr"); // 0p → red five
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// Even if the server somehow includes a tile addressed to a different
    /// seat, we must not leak it — mjai sees `"?"` for non-self draws.
    #[test]
    fn action_deal_tile_ignores_tile_for_other_seat() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(ACTION_DEAL_TILE, json!({ "seat": 1, "tile": "5m" }));
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "?");
            }
            other => panic!("expected Tsumo, got {other:?}"),
        }
    }

    /// Dealer's first discard: Majsoul omits the seat field (default 0).
    /// `moqie` is also absent → tsumogiri = false.
    #[test]
    fn action_discard_tile_first_discard_defaults_to_seat_zero() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(ACTION_DISCARD_TILE, json!({ "tile": "9s" }));
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Dahai {
                actor,
                pai,
                tsumogiri,
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai, "9s");
                assert!(!*tsumogiri);
            }
            other => panic!("expected Dahai, got {other:?}"),
        }
    }

    /// Tsumogiri (`moqie: true`) — drawn-and-immediately-discarded.
    #[test]
    fn action_discard_tile_tsumogiri_propagates() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_DISCARD_TILE,
            json!({ "moqie": true, "seat": 1, "tile": "4z" }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Dahai {
                actor,
                pai,
                tsumogiri,
            } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "N"); // 4z → N (north)
                assert!(*tsumogiri);
            }
            other => panic!("expected Dahai, got {other:?}"),
        }
    }

    /// Discard with `moqie:false` explicit — `tsumogiri` must be false.
    #[test]
    fn action_discard_tile_explicit_non_tsumogiri() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_DISCARD_TILE,
            json!({ "moqie": false, "seat": 3, "tile": "0s" }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Dahai {
                actor,
                pai,
                tsumogiri,
            } => {
                assert_eq!(*actor, 3);
                assert_eq!(pai, "5sr"); // red 5s
                assert!(!*tsumogiri);
            }
            other => panic!("expected Dahai, got {other:?}"),
        }
    }

    /// ActionDealTile before authGame must skip — we can't decide self vs other.
    #[test]
    fn action_deal_tile_before_seat_resolved_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        let msg = action_msg(ACTION_DEAL_TILE, json!({ "seat": 1, "tile": "5m" }));
        assert!(bridge.dispatch(&msg).is_empty());
    }

    /// Discard with empty/missing tile must be skipped, not turned into a
    /// `dahai("")` that would crash downstream.
    #[test]
    fn action_discard_tile_missing_tile_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        assert!(bridge
            .dispatch(&action_msg(ACTION_DISCARD_TILE, json!({})))
            .is_empty());
        assert!(bridge
            .dispatch(&action_msg(ACTION_DISCARD_TILE, json!({ "tile": "" })))
            .is_empty());
    }

    #[test]
    fn ankan_consumed_with_red_five() {
        // Ankan of 5m: must contain exactly one 5mr (the only red 5m in
        // the wall) and three regular 5m.
        let consumed = super::ankan_consumed("5m");
        assert_eq!(consumed, ["5mr", "5m", "5m", "5m"].map(String::from));
        let consumed = super::ankan_consumed("5mr");
        assert_eq!(consumed, ["5mr", "5m", "5m", "5m"].map(String::from));
    }

    #[test]
    fn ankan_consumed_no_red_form() {
        let consumed = super::ankan_consumed("E");
        assert_eq!(consumed, ["E", "E", "E", "E"].map(String::from));
        let consumed = super::ankan_consumed("1p");
        assert_eq!(consumed, ["1p", "1p", "1p", "1p"].map(String::from));
    }

    #[test]
    fn kakan_consumed_red_handling() {
        // Adding normal 5m on top of an existing pon → the pon must
        // already contain the red.
        assert_eq!(
            super::kakan_consumed("5m"),
            ["5mr", "5m", "5m"].map(String::from)
        );
        // Adding the red 5m → existing pon was three normal 5m.
        assert_eq!(
            super::kakan_consumed("5mr"),
            ["5m", "5m", "5m"].map(String::from)
        );
        // Non-five tiles never have a red form.
        assert_eq!(
            super::kakan_consumed("9p"),
            ["9p", "9p", "9p"].map(String::from)
        );
    }

    /// Pon by us on actor 1's discard. Three of the four entries in
    /// `froms` are `actor`; the one foreign seat is the discarder.
    #[test]
    fn action_chi_peng_gang_emits_pon() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 2,
                "type": 1,
                "tiles": ["4m", "4m", "4m"],
                "froms": [2, 2, 1],
            }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Pon {
                actor,
                target,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 2);
                assert_eq!(*target, 1);
                assert_eq!(pai, "4m");
                assert_eq!(consumed, &["4m", "4m"].map(String::from));
            }
            other => panic!("expected Pon, got {other:?}"),
        }
        // pon doesn't schedule any kan-dora.
        assert!(bridge.dora_timing.is_none());
    }

    /// Chi from kamicha (seat 0 → seat 1). Carries red five in the run.
    #[test]
    fn action_chi_peng_gang_emits_chi_with_red() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        let msg = action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 1,
                "type": 0,
                "tiles": ["3m", "4m", "0m"],
                "froms": [0, 1, 1],
            }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Chi {
                actor,
                target,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 1);
                assert_eq!(*target, 0);
                assert_eq!(pai, "3m");
                assert_eq!(consumed, &["4m", "5mr"].map(String::from));
            }
            other => panic!("expected Chi, got {other:?}"),
        }
    }

    /// 3p tables must never see a chi event. If Majsoul somehow sends one,
    /// drop it with a warning rather than propagating malformed mjai.
    #[test]
    fn action_chi_peng_gang_chi_dropped_in_three_player() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        bridge.num_players = 3;
        let msg = action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 1,
                "type": 0,
                "tiles": ["3m", "4m", "5m"],
                "froms": [0, 1, 1],
            }),
        );
        let events = bridge.dispatch(&msg);
        assert!(events.is_empty(), "chi must be dropped in 3p");
    }

    /// `ActionBaBei` (3p kita) → mjai `kita` event with hardcoded `pai = "N"`
    /// (Majsoul omits the tile field; kita is always the North tile).
    /// Real payload captured from a live 3p AI match.
    #[test]
    fn action_ba_bei_emits_kita_event() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        bridge.num_players = 3;
        let events = bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({
                "doras": [],
                "moqie": false,
                "seat": 0,
                "tile_state": 0,
                "tingpais": [],
                "zhenting": false,
            }),
        ));
        assert_eq!(events.len(), 1);
        match &events[0] {
            MjaiEvent::Kita { actor, pai } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai.as_deref(), Some("N"));
            }
            other => panic!("expected Kita, got {other:?}"),
        }
    }

    /// Kita updates `last_revealed_tile_actor` so a ron on the abstained
    /// north tile (chankan-style search-the-kita) targets the abstainer in
    /// the subsequent `ActionHule`.
    #[test]
    fn action_ba_bei_updates_last_revealed_tile_actor() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        bridge.num_players = 3;
        bridge.last_revealed_tile_actor = Some(99); // garbage to be overwritten
        bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({ "seat": 0, "moqie": false, "doras": [], "tingpais": [], "zhenting": false, "tile_state": 0 }),
        ));
        assert_eq!(bridge.last_revealed_tile_actor, Some(0));
    }

    /// Kita does NOT flip a new dora marker (Tenhou rule). The bridge's
    /// dora bookkeeping must stay untouched after a kita event.
    #[test]
    fn action_ba_bei_does_not_change_dora_state() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        bridge.doras = vec!["E".to_string()];
        bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({ "seat": 1, "moqie": false, "doras": [], "tingpais": [], "zhenting": false, "tile_state": 0 }),
        ));
        assert_eq!(bridge.doras, vec!["E".to_string()]);
        assert!(bridge.dora_timing.is_none());
        assert!(bridge.deferred_doras.is_empty());
    }

    /// Riichi declaration tile passes through normally, then a kita
    /// happens. `reach_accepted` must be drained before the kita event
    /// (mjai state machine: declaration tile must pass through before any
    /// non-Hule action).
    #[test]
    fn action_ba_bei_drains_pending_reach_accepted() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "5p", "is_liqi": true }),
        ));
        assert_eq!(bridge.pending_reach_accepted, Some(1));
        let events = bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({ "seat": 2, "moqie": false, "doras": [], "tingpais": [], "zhenting": false, "tile_state": 0 }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::ReachAccepted { actor } => assert_eq!(*actor, 1),
            other => panic!("expected ReachAccepted first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Kita { actor, .. } => assert_eq!(*actor, 2),
            other => panic!("expected Kita second, got {other:?}"),
        }
        assert!(bridge.pending_reach_accepted.is_none());
    }

    /// Daiminkan schedules dora 後乗り — flag must be set.
    #[test]
    fn action_chi_peng_gang_daiminkan_schedules_after_rinshan_dora() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let msg = action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 2,
                "type": 2,
                "tiles": ["7s", "7s", "7s", "7s"],
                "froms": [2, 2, 2, 0],
            }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Daiminkan {
                actor,
                target,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 2);
                assert_eq!(*target, 0);
                assert_eq!(pai, "7s");
                assert_eq!(consumed, &["7s", "7s", "7s"].map(String::from));
            }
            other => panic!("expected Daiminkan, got {other:?}"),
        }
        assert_eq!(bridge.dora_timing, Some(DoraTiming::PendingAfterRinshan));
    }

    /// Ankan: schedules dora 即乗り (before rinshan). Consumed includes red.
    #[test]
    fn action_an_gang_add_gang_ankan() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        let msg = action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 3, "tiles": "5m" }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Ankan { actor, consumed } => {
                assert_eq!(*actor, 1);
                assert_eq!(consumed, &["5mr", "5m", "5m", "5m"].map(String::from));
            }
            other => panic!("expected Ankan, got {other:?}"),
        }
        assert_eq!(bridge.dora_timing, Some(DoraTiming::PendingBeforeRinshan));
    }

    /// Kakan: schedules dora 後乗り; pai is the new tile, consumed is the
    /// existing 3 from the pon.
    #[test]
    fn action_an_gang_add_gang_kakan() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let msg = action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 0, "type": 2, "tiles": "0p" }),
        );
        match &bridge.dispatch(&msg)[0] {
            MjaiEvent::Kakan {
                actor,
                pai,
                consumed,
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(pai, "5pr");
                // New tile is the red, so existing 3 are normal.
                assert_eq!(consumed, &["5p", "5p", "5p"].map(String::from));
            }
            other => panic!("expected Kakan, got {other:?}"),
        }
        assert_eq!(bridge.dora_timing, Some(DoraTiming::PendingAfterRinshan));
    }

    /// Ankan flow end-to-end: ankan → ActionDealTile must produce
    /// `[Dora, Tsumo]` in that order (即乗り).
    #[test]
    fn ankan_then_rinshan_emits_dora_before_tsumo() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        bridge.doras = vec!["E".to_string()];

        // Ankan of 1z (E).
        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 3, "tiles": "1z" }),
        ));

        // Rinshan deal carries the new dora marker.
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 1, "tile": "5p", "doras": ["1z", "2z"] }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "S"),
            other => panic!("expected Dora first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "5p");
            }
            other => panic!("expected Tsumo second, got {other:?}"),
        }
        assert!(bridge.dora_timing.is_none());
        assert!(bridge.deferred_doras.is_empty());
    }

    /// Daiminkan flow as the live server sends it: the rinshan
    /// ActionDealTile does NOT carry the new marker; the grown `doras`
    /// array rides the declarer's next ActionDiscardTile. The dora must
    /// still flip before the dahai event (後乗り).
    #[test]
    fn daiminkan_then_rinshan_then_discard_emits_dora_before_dahai() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        bridge.doras = vec!["E".to_string()];

        bridge.dispatch(&action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 2,
                "type": 2,
                "tiles": ["7s", "7s", "7s", "7s"],
                "froms": [2, 2, 2, 0],
            }),
        ));

        // Rinshan: tsumo only — the server hasn't revealed the marker yet.
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 2, "tile": "9p", "doras": ["1z"] }),
        ));
        assert_eq!(events.len(), 1);
        match &events[0] {
            MjaiEvent::Tsumo { actor, pai } => {
                assert_eq!(*actor, 2);
                assert_eq!(pai, "9p");
            }
            other => panic!("expected single Tsumo, got {other:?}"),
        }
        assert!(bridge.deferred_doras.is_empty());
        assert!(bridge.dora_timing.is_none());

        // The discard carries the reveal: dora first, then dahai.
        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "9p", "moqie": true, "doras": ["1z", "3z"] }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "W"),
            other => panic!("expected Dora first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Dahai {
                actor,
                pai,
                tsumogiri,
            } => {
                assert_eq!(*actor, 2);
                assert_eq!(pai, "9p");
                assert!(*tsumogiri);
            }
            other => panic!("expected Dahai second, got {other:?}"),
        }
        assert!(bridge.deferred_doras.is_empty());
        assert_eq!(bridge.doras, vec!["E".to_string(), "W".to_string()]);
    }

    /// Kakan follows the same 後乗り timing as daiminkan (reveal rides
    /// the declarer's discard, flips before the dahai event).
    #[test]
    fn kakan_then_rinshan_then_discard_emits_dora_before_dahai() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.doras = vec!["E".to_string()];

        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 0, "type": 2, "tiles": "5m" }),
        ));

        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 0, "tile": "1p" }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Tsumo { .. }));
        assert!(bridge.deferred_doras.is_empty());

        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "1p", "moqie": true, "doras": ["1z", "4z"] }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "N"),
            other => panic!("expected Dora first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::Dahai { .. }));
    }

    /// Defensive: if a kakan's marker nevertheless arrives already on the
    /// rinshan deal (older captures suggested this shape), it must be
    /// deferred to the dahai — and the discard's own unchanged `doras`
    /// array must not double-emit it.
    #[test]
    fn kakan_dora_on_rinshan_deal_is_deferred_until_dahai() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.doras = vec!["E".to_string()];

        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 0, "type": 2, "tiles": "5m" }),
        ));

        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 0, "tile": "1p", "doras": ["1z", "4z"] }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Tsumo { .. }));
        assert_eq!(bridge.deferred_doras, vec!["N".to_string()]);

        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "1p", "moqie": true, "doras": ["1z", "4z"] }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "N"),
            other => panic!("expected Dora first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::Dahai { .. }));
        assert!(bridge.deferred_doras.is_empty());
    }

    /// Regression for issue #244 (reported via #243): East 1 with three
    /// kans — kakan hatsu, ankan 1p, kakan North. Real wire shape: kan
    /// reveals ride the declarer's ActionDiscardTile, ankan's rides the
    /// rinshan ActionDealTile. The old code dropped the kakan reveals and
    /// re-emitted a stale marker (`9m` twice, `5m`/`5p` never). The full
    /// event-kind sequence is pinned so ordering regressions relative to
    /// tsumo/dahai are caught here too.
    #[test]
    fn issue_244_multiple_kans_emit_correct_dora_sequence() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(3);
        bridge.doras = vec!["8p".to_string()];
        let mut emitted: Vec<String> = Vec::new();
        let mut collect = |evs: Vec<MjaiEvent>| {
            for e in evs {
                emitted.push(match &e {
                    MjaiEvent::Dora { dora_marker } => format!("dora:{dora_marker}"),
                    MjaiEvent::Tsumo { .. } => "tsumo".into(),
                    MjaiEvent::Dahai { .. } => "dahai".into(),
                    MjaiEvent::Ankan { .. } => "ankan".into(),
                    MjaiEvent::Kakan { .. } => "kakan".into(),
                    other => format!("{other:?}"),
                });
            }
        };

        // Kakan hatsu by seat 2; reveal (5m) rides the discard.
        collect(bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 2, "type": 2, "tiles": "6z" }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 2, "tile": "" }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "1z", "moqie": true, "doras": ["8p", "5m"] }),
        )));

        // Ankan 1p by seat 0; reveal (9m) rides the rinshan deal.
        collect(bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 0, "type": 3, "tiles": "1p" }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 0, "tile": "", "doras": ["8p", "5m", "9m"] }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "2z", "moqie": true }),
        )));

        // Kakan North by seat 3; rinshan deal repeats the unchanged
        // 3-entry array (this used to re-emit a stale 9m), reveal (5p)
        // rides the discard.
        collect(bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 3, "type": 2, "tiles": "4z" }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 3, "tile": "5p", "doras": ["8p", "5m", "9m"] }),
        )));
        collect(bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 3, "tile": "5p", "moqie": true, "doras": ["8p", "5m", "9m", "5p"] }),
        )));

        assert_eq!(
            emitted,
            vec![
                // kakan hatsu: reveal flips before the declarer's dahai
                "kakan", "tsumo", "dora:5m", "dahai", //
                // ankan 1p: reveal flips before the rinshan tsumo
                "ankan", "dora:9m", "tsumo", "dahai", //
                // kakan North: unchanged deal array must not re-emit 9m
                "kakan", "tsumo", "dora:5p", "dahai",
            ],
            "mjai event sequence must match the game's reveals and spec ordering"
        );
        assert_eq!(
            bridge.doras,
            vec![
                "8p".to_string(),
                "5m".to_string(),
                "9m".to_string(),
                "5p".to_string()
            ]
        );
    }

    /// Consecutive kans by one declarer with no discard in between:
    /// kakan (reveal still pending) → rinshan → ankan → rinshan. Both
    /// flips arrive together on the ankan's rinshan deal in slot order —
    /// the pending kakan marker first, then the ankan's, both before the
    /// tsumo. The following discard's unchanged array adds nothing.
    #[test]
    fn consecutive_kans_flush_both_markers_at_ankan_rinshan() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.doras = vec!["E".to_string()];

        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 2, "tiles": "5m" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 1, "tile": "" }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Tsumo { .. }));

        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 3, "tiles": "9s" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 1, "tile": "", "doras": ["1z", "4z", "5z"] }),
        ));
        assert_eq!(events.len(), 3);
        match (&events[0], &events[1]) {
            (
                MjaiEvent::Dora { dora_marker: first },
                MjaiEvent::Dora {
                    dora_marker: second,
                },
            ) => {
                assert_eq!(first, "N");
                assert_eq!(second, "P");
            }
            other => panic!("expected two Dora events first, got {other:?}"),
        }
        assert!(matches!(events[2], MjaiEvent::Tsumo { .. }));

        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "1m", "moqie": true, "doras": ["1z", "4z", "5z"] }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Dahai { .. }));
        assert_eq!(
            bridge.doras,
            vec!["E".to_string(), "N".to_string(), "P".to_string()]
        );
    }

    /// If the local list somehow falls several markers behind, every
    /// missing entry is emitted in order and the list fully resyncs.
    #[test]
    fn consume_new_doras_emits_all_missing_markers() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.doras = vec!["E".to_string()];

        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 0, "tile": "1p", "doras": ["1z", "2z", "3z"] }),
        ));
        // No kan pending → warn-and-emit-before-tsumo path.
        assert_eq!(events.len(), 3);
        match (&events[0], &events[1]) {
            (
                MjaiEvent::Dora { dora_marker: first },
                MjaiEvent::Dora {
                    dora_marker: second,
                },
            ) => {
                assert_eq!(first, "S");
                assert_eq!(second, "W");
            }
            other => panic!("expected two Dora events first, got {other:?}"),
        }
        assert!(matches!(events[2], MjaiEvent::Tsumo { .. }));
        assert_eq!(
            bridge.doras,
            vec!["E".to_string(), "S".to_string(), "W".to_string()]
        );
    }

    /// Defensive: an ankan whose ActionAnGangAddGang already carries the
    /// grown `doras` array flips right after the ankan event, and the
    /// rinshan deal repeating the same array must not double-emit.
    #[test]
    fn ankan_dora_on_kan_message_emits_once() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(1);
        bridge.doras = vec!["E".to_string()];

        let events = bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 3, "tiles": "1z", "doras": ["1z", "2z"] }),
        ));
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], MjaiEvent::Ankan { .. }));
        match &events[1] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "S"),
            other => panic!("expected Dora after Ankan, got {other:?}"),
        }
        assert!(bridge.dora_timing.is_none());

        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 1, "tile": "5p", "doras": ["1z", "2z"] }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Tsumo { .. }));
    }

    /// 3p: after a kakan the declarer may pull North instead of
    /// discarding — the babei is then the commit event carrying the kan's
    /// reveal, which must flip before the kita event.
    #[test]
    fn kakan_then_babei_emits_dora_before_kita() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        bridge.doras = vec!["E".to_string()];

        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 2, "tiles": "5m" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 1, "tile": "" }),
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MjaiEvent::Tsumo { .. }));

        let events = bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({ "seat": 1, "moqie": false, "doras": ["1z", "4z"], "tingpais": [], "zhenting": false, "tile_state": 0 }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Dora { dora_marker } => assert_eq!(dora_marker, "N"),
            other => panic!("expected Dora first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Kita { actor, .. } => assert_eq!(*actor, 1),
            other => panic!("expected Kita second, got {other:?}"),
        }
        assert!(bridge.deferred_doras.is_empty());
    }

    /// Riichi declaration: dahai with `is_liqi=true` produces
    /// `[reach, dahai]` and queues a reach_accepted for the next action.
    #[test]
    fn discard_with_is_liqi_emits_reach_then_dahai_and_queues_accept() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "1z", "moqie": false, "is_liqi": true }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Reach { actor, .. } => assert_eq!(*actor, 1),
            other => panic!("expected Reach first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Dahai {
                actor,
                pai,
                tsumogiri,
            } => {
                assert_eq!(*actor, 1);
                assert_eq!(pai, "E");
                assert!(!*tsumogiri);
            }
            other => panic!("expected Dahai second, got {other:?}"),
        }
        assert_eq!(bridge.pending_reach_accepted, Some(1));
    }

    /// Double riichi: `is_wliqi` triggers the same flow as `is_liqi`.
    #[test]
    fn discard_with_is_wliqi_also_triggers_reach() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "9m", "moqie": true, "is_wliqi": true }),
        ));
        assert!(matches!(events[0], MjaiEvent::Reach { actor: 0, .. }));
        assert_eq!(bridge.pending_reach_accepted, Some(0));
    }

    /// Declaration tile passes through to next player's draw — mjai spec
    /// requires `reach_accepted` *before* that next tsumo.
    #[test]
    fn reach_accepted_drains_before_next_tsumo() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "1z", "is_liqi": true }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_DEAL_TILE,
            json!({ "seat": 2, "tile": "5p" }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::ReachAccepted { actor } => assert_eq!(*actor, 1),
            other => panic!("expected ReachAccepted first, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Tsumo { actor, .. } => assert_eq!(*actor, 2),
            other => panic!("expected Tsumo second, got {other:?}"),
        }
        assert!(bridge.pending_reach_accepted.is_none());
    }

    /// Declaration tile gets called (chi/pon/daiminkan) — riichi is still
    /// accepted, reach_accepted prepended to the call event.
    #[test]
    fn reach_accepted_drains_before_chi_pon_daiminkan() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "5m", "is_liqi": true }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_CHI_PENG_GANG,
            json!({
                "seat": 2,
                "type": 1,
                "tiles": ["5m", "5m", "5m"],
                "froms": [2, 2, 1],
            }),
        ));
        assert!(matches!(events[0], MjaiEvent::ReachAccepted { actor: 1 }));
        assert!(matches!(
            events[1],
            MjaiEvent::Pon {
                actor: 2,
                target: 1,
                ..
            }
        ));
    }

    /// New kyoku must clear any leftover reach state.
    #[test]
    fn start_kyoku_resets_pending_reach_accepted() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.pending_reach_accepted = Some(2);
        bridge.dispatch(&new_round_msg(json!({
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": [
                "1m","2m","3m","4m","5m","6m","7m","8m","9m",
                "1p","2p","3p","4p","0p"
            ],
        })));
        assert!(bridge.pending_reach_accepted.is_none());
    }

    #[test]
    fn public_draw_hands_red_fives_and_settlement_are_forwarded() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge.build_no_tile(&json!({
            "players": [
                { "tingpai": false, "hand": [] },
                { "tingpai": true, "hand": ["0p", "6p", "8p", "8p"] },
                { "tingpai": true, "hand": ["1m", "2m", "3m", "3m", "4m", "5m", "6m", "6m", "7m", "8m", "6s", "7s", "8s"] },
                { "tingpai": false, "hand": [] }
            ],
            "scores": [{ "old_scores": [27200,22600,41300,7900], "delta_scores": [-1500,1500,1500,-1500] }]
        })).unwrap();
        match &events[0] {
            MjaiEvent::Ryukyoku {
                tehais,
                scores,
                reason,
                ..
            } => {
                let hands = tehais.as_ref().unwrap();
                assert!(hands[0].is_none() && hands[3].is_none());
                assert_eq!(hands[1].as_ref().unwrap(), &["5pr", "6p", "8p", "8p"]);
                assert_eq!(*scores, Some(vec![25700, 24100, 42800, 6400]));
                assert_eq!(reason.as_deref(), Some("exhaustive"));
            }
            other => panic!("expected public draw, got {other:?}"),
        }
    }

    #[test]
    fn all_and_no_tenpai_reveals_do_not_depend_on_zero_payments() {
        let mut bridge = MajsoulBridge::new(None, None);
        for hand in [json!([]), json!(["0m"])] {
            let events = bridge
                .build_no_tile(&json!({
                    "players": [{"hand": hand}, {"hand": hand}, {"hand": hand}, {"hand": hand}],
                    "scores": [{"old_scores": [25000,25000,25000,25000], "delta_scores": [0,0,0,0]}]
                }))
                .unwrap();
            let MjaiEvent::Ryukyoku {
                tehais: Some(hands),
                ..
            } = &events[0]
            else {
                panic!();
            };
            assert!(hands
                .iter()
                .all(|tiles| tiles.is_some() == !hand.as_array().unwrap().is_empty()));
        }
    }

    #[test]
    fn nine_terminals_forwards_only_the_declarers_public_hand() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge
            .build_liu_ju(&json!({ "type": 1, "seat": 3, "tiles": [
            "2m","5m","1p","6p","7p","9p","2s","9s","2z","3z","5z","6z","7z","1s"
        ] }))
            .unwrap();
        let MjaiEvent::Ryukyoku {
            tehais: Some(hands),
            reason,
            ..
        } = &events[0]
        else {
            panic!();
        };
        assert_eq!(reason.as_deref(), Some("kyuushu"));
        assert!(hands[..3].iter().all(Option::is_none));
        assert_eq!(hands[3].as_ref().unwrap().len(), 14);
    }

    #[test]
    fn malformed_public_draw_metadata_is_an_error() {
        let mut bridge = MajsoulBridge::new(None, None);
        for data in [
            json!({"players": [{"hand": []}]}),
            json!({"players": [{"hand": "0m"}, {}, {}, {}]}),
            json!({"players": [{"hand": ["bad"]}, {}, {}, {}]}),
            json!({"scores": [{"delta_scores": [0]}]}),
            json!({"scores": [{"delta_scores": []}, {"delta_scores": [0,0,0]}]}),
            json!({"scores": [{"old_scores": [25000], "delta_scores": [0,0,0,0]}]}),
            json!({"scores": [{"old_scores": [25000,25000,25000,25000]}, {"old_scores": [24000,25000,25000,25000]}]}),
        ] {
            assert!(bridge.build_no_tile(&data).is_err());
        }
    }

    #[test]
    fn abortive_draws_follow_the_official_clients_public_reveals() {
        let mut bridge = MajsoulBridge::new(None, None);
        for (kind, reason) in [
            (2, "four_winds"),
            (3, "four_kans"),
            (4, "four_riichis"),
            (5, "three_rons"),
        ] {
            let events = bridge.build_liu_ju(&json!({"type": kind, "seat": 2,
                "allplayertiles": ["0m|1m", "0p|1p", "bad hidden data", "0s|1s"]
            }));
            if kind == 4 {
                assert!(events.is_err());
                continue;
            }
            let events = events.unwrap();
            let MjaiEvent::Ryukyoku {
                tehais: Some(hands),
                reason: Some(actual),
                ..
            } = &events[0]
            else {
                panic!();
            };
            assert_eq!(actual, reason);
            if kind == 5 {
                assert!(hands[2].is_none());
                assert_eq!(hands[0].as_ref().unwrap()[0], "5mr");
                assert_eq!(hands[1].as_ref().unwrap()[0], "5pr");
                assert_eq!(hands[3].as_ref().unwrap()[0], "5sr");
            } else {
                assert!(hands.iter().all(Option::is_none));
            }
        }
        let events = bridge
            .build_liu_ju(&json!({"type": 4, "allplayertiles": ["0m", "0p", "0s", "1z"]}))
            .unwrap();
        let MjaiEvent::Ryukyoku {
            tehais: Some(hands),
            ..
        } = &events[0]
        else {
            panic!();
        };
        assert!(hands.iter().all(Option::is_some));
    }

    /// Real ActionNoTile sample (1 human in tenpai, 3 noten) →
    /// `[ryukyoku{deltas:[3000,-1000,-1000,-1000]}, end_kyoku]`.
    #[test]
    fn action_no_tile_emits_ryukyoku_with_deltas_then_end_kyoku() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "gameend": false,
                "liujumanguan": false,
                "players": [],
                "scores": [{
                    "delta_scores": [3000, -1000, -1000, -1000],
                    "old_scores": [25000, 17300, 32700, 25000],
                    "seat": 0,
                }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => {
                assert_eq!(*deltas, Some(vec![3000, -1000, -1000, -1000]));
            }
            other => panic!("expected Ryukyoku first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// Real captured `ActionNoTile` payload of a draw with all four seats
    /// noten: `delta_scores` arrives empty, not as four zeros. The hand
    /// still ends, with zero deltas and the scores unchanged.
    #[test]
    fn action_no_tile_with_empty_deltas_emits_zero_deltas() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "gameend": false,
                "hules_history": [],
                "liujumanguan": false,
                "players": [
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                ],
                "scores": [{
                    "delta_scores": [],
                    "doras": [],
                    "hand": [],
                    "lines": [],
                    "ming": [],
                    "old_scores": [25000, 25000, 25000, 25000],
                    "score": 0,
                    "seat": 0,
                    "taxes": [],
                }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Ryukyoku {
                deltas,
                tehais,
                scores,
                reason,
            } => {
                assert_eq!(*deltas, Some(vec![0, 0, 0, 0]));
                assert_eq!(*tehais, Some(vec![None, None, None, None]));
                assert_eq!(*scores, Some(vec![25000, 25000, 25000, 25000]));
                assert_eq!(reason.as_deref(), Some("exhaustive"));
            }
            other => panic!("expected Ryukyoku first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// The other draw nobody pays for: every seat tenpai. Empty
    /// `delta_scores` must not cost the hands the frame discloses.
    #[test]
    fn action_no_tile_with_empty_deltas_keeps_disclosed_hands() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "players": [
                    { "tingpai": true, "hand": ["0p", "6p", "8p", "8p"] },
                    { "tingpai": true, "hand": ["4s", "4s", "6s", "7s"] },
                    { "tingpai": true, "hand": ["1z"] },
                ],
                "scores": [{ "delta_scores": [], "old_scores": [36000, 35000, 34000] }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Ryukyoku {
                deltas,
                tehais,
                scores,
                ..
            } => {
                assert_eq!(*deltas, Some(vec![0, 0, 0]));
                let hands = tehais.as_ref().unwrap();
                assert_eq!(hands[0].as_ref().unwrap(), &["5pr", "6p", "8p", "8p"]);
                assert_eq!(hands[1].as_ref().unwrap(), &["4s", "4s", "6s", "7s"]);
                assert_eq!(hands[2].as_ref().unwrap(), &["E"]);
                assert_eq!(*scores, Some(vec![36000, 35000, 34000]));
            }
            other => panic!("expected Ryukyoku first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// Multiple `scores` entries (e.g. tenpai redistribution + nagashi
    /// mangan in the same frame) must be summed per seat.
    #[test]
    fn action_no_tile_sums_multiple_score_entries() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "scores": [
                    { "delta_scores": [1000, -500, 0, -500] },
                    { "delta_scores": [500, 1500, -1000, -1000] },
                ],
            }),
        ));
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => {
                assert_eq!(*deltas, Some(vec![1500, 1000, -1000, -1500]));
            }
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
    }

    /// Real captured 3p `ActionNoTile` payload (1 tenpai / 2 noten).
    /// `delta_scores: [2000, -1000, -1000]` → length-3 deltas, no padding.
    /// Tenpai pool 2000 split per the mjai 3p extension.
    #[test]
    fn action_no_tile_three_player_real_payload() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "gameend": false,
                "hules_history": [],
                "liujumanguan": false,
                "players": [
                    {
                        "already_hule": false,
                        "hand": ["2p","2p","2p","5p","6p","7p","8p","9p","4s","0s","6s","8s","8s"],
                        "tingpai": true,
                        "tings": [
                            { "tile": "7p", "fu": 40, "fu_zimo": 30 },
                            { "tile": "4p", "fu": 40, "fu_zimo": 30 },
                        ],
                    },
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                    { "already_hule": false, "hand": [], "tingpai": false, "tings": [] },
                ],
                "scores": [{
                    "delta_scores": [2000, -1000, -1000],
                    "doras": [],
                    "old_scores": [35000, 35000, 35000],
                    "seat": 0,
                }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => {
                assert_eq!(*deltas, Some(vec![2000, -1000, -1000]));
            }
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// 3p ryukyoku: `delta_scores` arrives with 3 entries; emitted mjai
    /// `deltas` keeps native length 3 (no padding).
    #[test]
    fn action_no_tile_three_player_deltas_native_length() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({
                "scores": [{ "delta_scores": [2000, -1000, -1000] }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => {
                assert_eq!(*deltas, Some(vec![2000, -1000, -1000]));
            }
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
    }

    /// Empty / missing `scores` → `deltas: None`.
    #[test]
    fn action_no_tile_without_scores_emits_no_deltas() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(ACTION_NO_TILE, json!({})));
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => assert!(deltas.is_none()),
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// `ActionLiuJu` (any abortive type) → `[ryukyoku{None}, end_kyoku]`.
    /// No point redistribution.
    #[test]
    fn action_liu_ju_emits_ryukyoku_no_deltas_then_end_kyoku() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(ACTION_LIU_JU, json!({ "type": 1, "seat": 0 })));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Ryukyoku { deltas, .. } => assert!(deltas.is_none()),
            other => panic!("expected Ryukyoku, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// Riichi declared on the very last possible turn, then the round
    /// immediately ends in ryukyoku — `reach_accepted` must still be
    /// emitted (declaration tile passed through, no ron) before the
    /// terminal events.
    #[test]
    fn reach_accepted_drains_before_no_tile_ryukyoku() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "9p", "is_liqi": true }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_NO_TILE,
            json!({ "scores": [{ "delta_scores": [-1500, -1500, 0, -1500] }] }),
        ));
        assert_eq!(events.len(), 3);
        match &events[0] {
            MjaiEvent::ReachAccepted { actor } => assert_eq!(*actor, 2),
            other => panic!("expected ReachAccepted first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::Ryukyoku { .. }));
        assert!(matches!(events[2], MjaiEvent::EndKyoku));
        assert!(bridge.pending_reach_accepted.is_none());
    }

    #[test]
    fn action_hule_forwards_public_red_hands_tiles_and_shared_scores() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.last_revealed_tile_actor = Some(0);
        let data = json!({
            "scores": [12000, 32000, 31000, 25000],
            "delta_scores": [-13000, 7000, 6000, 0],
            "hules": [
                {"seat": 1, "zimo": false, "hu_tile": "0p", "hand": ["2m","3m","4m","6p","7p","8p","3s","4s","0s","6s","7s","8s","5p"]},
                {"seat": 2, "zimo": false, "hu_tile": "0p", "hand": ["3m","4m","0m","6m","7m","8m","2p","3p","4p","4s","5s","6s","5p"]}
            ]
        });
        let events = bridge.build_hule(&data).unwrap();
        assert_eq!(events.len(), 3);
        for event in &events[..2] {
            let wire = serde_json::to_value(event).unwrap();
            assert_eq!(wire["pai"], "5pr");
            assert_eq!(wire["tehai"].as_array().unwrap().len(), 13);
            assert_eq!(wire["scores"], data["scores"]);
            assert_eq!(wire["target"], 0);
        }
        let first = serde_json::to_value(&events[0]).unwrap();
        let second = serde_json::to_value(&events[1]).unwrap();
        assert_eq!(first["tehai"][8], "5sr");
        assert_eq!(second["tehai"][2], "5mr");
        assert!(matches!(events[2], MjaiEvent::EndKyoku));
        assert!(bridge
            .build_hule(&json!({"scores": [25000], "hules": [{"seat": 1, "zimo": true}]}))
            .is_err());
        assert!(bridge
            .build_hule(&json!({"hules": [{"seat": 1, "zimo": true, "hand": [0]}]}))
            .is_err());
    }

    /// Real ActionHule sample (single ron, non-riichi): seat 3 rons off
    /// seat 2's discard. Expected: `[Hora{actor:3, target:2, deltas, no
    /// ura}, EndKyoku]`.
    #[test]
    fn action_hule_ron_emits_hora_then_end_kyoku() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        // Simulate seat 2 having just discarded.
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "0p" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, 0, -2000, 2000],
                "old_scores": [25000, 25000, 25000, 25000],
                "scores": [25000, 25000, 23000, 27000],
                "hules": [{
                    "seat": 3,
                    "zimo": false,
                    "liqi": false,
                    "hu_tile": "0p",
                    "li_doras": [],
                }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Hora {
                actor,
                target,
                deltas,
                ura_markers,
                ..
            } => {
                assert_eq!(*actor, 3);
                assert_eq!(*target, 2);
                assert_eq!(*deltas, Some(vec![0, 0, -2000, 2000]));
                assert!(ura_markers.is_none(), "no riichi → no ura_markers");
            }
            other => panic!("expected Hora first, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// Self-tsumo win: actor == target, deltas applies to self & all
    /// payers, no ura when no riichi.
    #[test]
    fn action_hule_tsumo_actor_equals_target() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [4000, -2000, -1000, -1000],
                "hules": [{ "seat": 0, "zimo": true, "liqi": false }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora {
                actor,
                target,
                deltas,
                ura_markers,
                ..
            } => {
                assert_eq!(*actor, 0);
                assert_eq!(*target, 0);
                assert_eq!(*deltas, Some(vec![4000, -2000, -1000, -1000]));
                assert!(ura_markers.is_none());
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// Riichi win surfaces ura markers from `li_doras` (mjai-mapped).
    #[test]
    fn action_hule_riichi_win_emits_ura_markers() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "1m" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, 0, -8000, 8000],
                "hules": [{
                    "seat": 3,
                    "zimo": false,
                    "liqi": true,
                    "li_doras": ["7z", "0s"],
                }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora { ura_markers, .. } => {
                assert_eq!(
                    ura_markers.as_deref(),
                    Some(&["C".to_string(), "5sr".to_string()][..]),
                );
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// Riichi win with `li_doras: []` still produces `Some(vec![])` so
    /// consumers can tell apart "had riichi but no ura" from "no riichi".
    #[test]
    fn action_hule_riichi_with_empty_li_doras_is_some_empty() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "1m" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, 0, -1000, 1000],
                "hules": [{ "seat": 3, "zimo": false, "liqi": true, "li_doras": [] }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora { ura_markers, .. } => {
                assert_eq!(ura_markers.as_deref(), Some(&[][..]));
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// Multi-ron (double): two `Hora` events then one `EndKyoku`. Same
    /// total deltas attached to each.
    #[test]
    fn action_hule_double_ron_emits_two_hora_then_end_kyoku() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "5m" }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [-9000, 4000, 0, 5000],
                "hules": [
                    { "seat": 1, "zimo": false, "liqi": false },
                    { "seat": 3, "zimo": false, "liqi": false },
                ],
            }),
        ));
        assert_eq!(events.len(), 3);
        match &events[0] {
            MjaiEvent::Hora { actor, target, .. } => {
                assert_eq!(*actor, 1);
                assert_eq!(*target, 0);
            }
            other => panic!("expected Hora, got {other:?}"),
        }
        match &events[1] {
            MjaiEvent::Hora { actor, target, .. } => {
                assert_eq!(*actor, 3);
                assert_eq!(*target, 0);
            }
            other => panic!("expected Hora, got {other:?}"),
        }
        assert!(matches!(events[2], MjaiEvent::EndKyoku));
    }

    /// Ron on the declaration tile: riichi voided, so no `reach_accepted`
    /// prepended even though `pending_reach_accepted` was set by the
    /// declarer's discard.
    #[test]
    fn action_hule_on_declaration_tile_voids_riichi() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 2, "tile": "1z", "is_liqi": true }),
        ));
        assert_eq!(bridge.pending_reach_accepted, Some(2));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, 0, -8000, 8000],
                "hules": [{ "seat": 3, "zimo": false, "liqi": false, "hu_tile": "1z" }],
            }),
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, MjaiEvent::ReachAccepted { .. })),
            "ron on declaration tile must NOT emit reach_accepted"
        );
        assert!(bridge.pending_reach_accepted.is_none());
        // Still emits the hora and end_kyoku.
        assert!(events.iter().any(|e| matches!(e, MjaiEvent::Hora { .. })));
        assert!(events.iter().any(|e| matches!(e, MjaiEvent::EndKyoku)));
    }

    /// Ron without any preceding `dahai` is malformed (no discarder to
    /// target). Skip with a warning instead of guessing.
    #[test]
    fn action_hule_ron_without_last_revealed_tile_actor_skips() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, 0, -2000, 2000],
                "hules": [{ "seat": 3, "zimo": false }],
            }),
        ));
        assert!(events.is_empty());
    }

    /// 搶槓 (chankan): seat 1 declares kakan; seat 3 rons. Target must be
    /// the kakan declarer (seat 1), not whoever happened to discard last.
    #[test]
    fn action_hule_chankan_targets_kakan_declarer() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        // Earlier in the round: seat 0 discarded (this should NOT be the
        // ron target if a kakan happens in between).
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "9m" }),
        ));
        // Seat 1 declares kakan on 5m.
        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 1, "type": 2, "tiles": "5m" }),
        ));
        assert_eq!(bridge.last_revealed_tile_actor, Some(1));

        // Seat 3 rons (chankan).
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [0, -8000, 0, 8000],
                "hules": [{ "seat": 3, "zimo": false, "liqi": false, "hu_tile": "5m" }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora { actor, target, .. } => {
                assert_eq!(*actor, 3);
                assert_eq!(*target, 1, "chankan target must be the kakan declarer");
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// 国士無双搶暗槓: seat 2 declares ankan; seat 0 (holding kokushi)
    /// rons. Mjai target = ankan declarer.
    #[test]
    fn action_hule_kokushi_robs_ankan_targets_ankan_declarer() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 3, "tile": "9p" }),
        ));
        // Seat 2 declares ankan on East (1z → "E").
        bridge.dispatch(&action_msg(
            ACTION_AN_GANG_ADD_GANG,
            json!({ "seat": 2, "type": 3, "tiles": "1z" }),
        ));
        assert_eq!(bridge.last_revealed_tile_actor, Some(2));

        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [32000, 0, -32000, 0],
                "hules": [{
                    "seat": 0,
                    "zimo": false,
                    "liqi": false,
                    "hu_tile": "1z",
                    "yiman": true,
                }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora { actor, target, .. } => {
                assert_eq!(*actor, 0);
                assert_eq!(*target, 2, "kokushi rob ankan: target = ankan declarer");
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// Real captured 3p `ActionHule` payload: seat 2 rons off seat 0
    /// (riichi+haitei, dama with ura). `delta_scores: [-12000, 0, 13000]`
    /// length 3, with the +1000 honba/riichi-stick bundled into the winner's
    /// 13000. Verifies length-3 deltas, ura markers from `li_doras`.
    #[test]
    fn action_hule_three_player_real_ron_payload() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.num_players = 3;
        // Seat 0 just discarded — sets last_revealed_tile_actor for the ron target.
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 0, "tile": "3s", "moqie": false }),
        ));
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "baopai": 0,
                "delta_scores": [-12000, 0, 13000],
                "doras": [],
                "old_scores": [35000, 35000, 34000],
                "scores": [23000, 35000, 47000],
                "wait_timeout": 0,
                "hules": [{
                    "baida_changed": [],
                    "baopai": 0,
                    "baopai_seats": [],
                    "count": 7,
                    "dadian": 12000,
                    "doras": ["6s"],
                    "fans": [
                        { "id": 25, "val": 2 },
                        { "id": 2,  "val": 1 },
                        { "id": 33, "val": 2 },
                        { "id": 34, "val": 2 },
                    ],
                    "fu": 25,
                    "hand": ["9m","9m","3p","3p","6p","6p","2s","2s","3s","5s","5s","9s","9s"],
                    "hu_tile": "3s",
                    "hu_tile_bai_da_changed": "",
                    "li_doras": ["5p"],
                    "lines": [],
                    "liqi": true,
                    "ming": [],
                    "point_rong": 12000,
                    "point_sum": 12000,
                    "qinjia": false,
                    "seat": 2,
                    "yiman": false,
                    "zimo": false,
                }],
            }),
        ));
        assert_eq!(events.len(), 2);
        match &events[0] {
            MjaiEvent::Hora {
                actor,
                target,
                deltas,
                ura_markers,
                score,
                ..
            } => {
                assert_eq!(*actor, 2);
                assert_eq!(*target, 0);
                assert_eq!(*deltas, Some(vec![-12000, 0, 13000]));
                assert_eq!(ura_markers.as_deref(), Some(&["5p".to_string()][..]));
                // How the hand was scored rides beside the event: its yaku by the
                // provider's ids, with their han.
                let score = score.as_ref().expect("the win says how it was scored");
                assert_eq!(score.yaku, [(25, 2), (2, 1), (33, 2), (34, 2)]);
                assert_eq!((score.han, score.fu, score.yakuman), (7, 25, false));
            }
            other => panic!("expected Hora, got {other:?}"),
        }
        assert!(matches!(events[1], MjaiEvent::EndKyoku));
    }

    /// **Ron on kita (搶北)** — Majsoul wire format: `ActionBaBei` followed
    /// by `ActionHule` with `hu_tile: "4z"`, `zimo: false`. The ron's
    /// `target` must attribute to the kita declarer (whoever set aside the
    /// north tile), not whoever discarded last.
    ///
    /// Confirmed against Akagi-Python and AkagiNG: no separate
    /// "kita_chankan" message exists; the hule arrives in the normal
    /// `ActionHule` slot. `build_kita` sets `last_revealed_tile_actor` so
    /// `build_hule` picks up the kita declarer as `target` automatically.
    /// `chankan` yaku is NOT awarded on ron-on-kita (Tenhou rule), but that
    /// distinction is the bot's concern — the bridge just emits the events.
    #[test]
    fn ron_on_kita_targets_kita_declarer() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(2); // observer is the ronner
        bridge.num_players = 3;
        // Earlier in the round seat 1 had discarded — must NOT be the ron target.
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "5p", "moqie": false }),
        ));
        // Seat 0 declares kita.
        bridge.dispatch(&action_msg(
            ACTION_BA_BEI,
            json!({ "seat": 0, "moqie": false, "doras": [], "tingpais": [], "zhenting": false, "tile_state": 0 }),
        ));
        assert_eq!(bridge.last_revealed_tile_actor, Some(0));
        // Seat 2 rons the kita tile (hu_tile = 4z = N).
        let events = bridge.dispatch(&action_msg(
            ACTION_HULE,
            json!({
                "delta_scores": [-8000, 0, 8000],
                "hules": [{
                    "seat": 2,
                    "zimo": false,
                    "liqi": false,
                    "hu_tile": "4z",
                }],
            }),
        ));
        match &events[0] {
            MjaiEvent::Hora { actor, target, .. } => {
                assert_eq!(*actor, 2);
                assert_eq!(*target, 0, "ron-on-kita target = kita declarer");
            }
            other => panic!("expected Hora, got {other:?}"),
        }
    }

    /// `last_revealed_tile_actor` resets at start_kyoku — a ron in the new round must
    /// not target a discarder from the previous round.
    #[test]
    fn start_kyoku_resets_last_revealed_tile_actor() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.last_revealed_tile_actor = Some(2);
        bridge.dispatch(&new_round_msg(json!({
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": [
                "1m","2m","3m","4m","5m","6m","7m","8m","9m",
                "1p","2p","3p","4p","0p"
            ],
        })));
        assert!(bridge.last_revealed_tile_actor.is_none());
    }

    /// Earlier reach_voided test used an empty `{}` ActionHule payload.
    /// With proper hule support that returns Err (no hules); ensure
    /// queue still gets cleared regardless of build success.
    #[test]
    fn reach_voided_by_ron_on_declaration_tile() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dispatch(&action_msg(
            ACTION_DISCARD_TILE,
            json!({ "seat": 1, "tile": "5m", "is_liqi": true }),
        ));
        assert_eq!(bridge.pending_reach_accepted, Some(1));
        let events = bridge.dispatch(&action_msg(ACTION_HULE, json!({})));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, MjaiEvent::ReachAccepted { .. })),
            "no reach_accepted on ron of declaration tile"
        );
        assert!(bridge.pending_reach_accepted.is_none());
    }

    /// `NotifyGameEndResult` (top-level Notify, NOT ActionPrototype) →
    /// `[EndGame]`. Standings remain private metadata; mjai JSON stays empty.
    /// What a game did to each seat's rank points is read by seat, whatever
    /// order the result lists them in; it is all of them or none.
    #[test]
    fn rank_points_are_read_by_seat() {
        let ranked = json!({ "result": { "players": [
            { "seat": 1, "part_point_1": 43800, "grading_score": 120 },
            { "seat": 3, "part_point_1": 24700, "grading_score": 45 },
            { "seat": 0, "part_point_1": 20500, "grading_score": -30 },
            { "seat": 2, "part_point_1": 11000, "grading_score": -165 },
        ] } });
        assert_eq!(
            parse_rank_points(&ranked, 4),
            Some(vec![-30, 120, -165, 45])
        );
        // A result that does not say for every seat says nothing.
        let partial = json!({ "result": { "players": [
            { "seat": 0, "grading_score": 10 }, { "seat": 1 }, { "seat": 2 }, { "seat": 3 },
        ] } });
        assert_eq!(parse_rank_points(&partial, 4), None);
        assert_eq!(parse_rank_points(&ranked, 3), None);

        // And it reaches the event that ends the game.
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.num_players = 4;
        let events = bridge.dispatch(&ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_NOTIFY_GAME_END_RESULT),
            payload: ranked,
        });
        assert!(matches!(
            &events[0],
            MjaiEvent::EndGame { rank_points: Some(points), .. } if points == &[-30, 120, -165, 45]
        ));
    }

    #[test]
    fn notify_game_end_result_emits_end_game() {
        let mut bridge = MajsoulBridge::new(None, None);
        let msg = ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_NOTIFY_GAME_END_RESULT),
            payload: json!({
                "result": {
                    "players": [
                        { "seat": 1, "total_point":  33800, "part_point_1": 43800 },
                        { "seat": 3, "total_point":   4700, "part_point_1": 24700 },
                        { "seat": 0, "total_point":  -9500, "part_point_1": 20500 },
                        { "seat": 2, "total_point": -29000, "part_point_1": 11000 },
                    ]
                }
            }),
        };
        let events = bridge.dispatch(&msg);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            MjaiEvent::EndGame {
                reason: crate::schema::GameEndReason::Confirmed,
                final_scores: Some(scores),
                final_ranks: Some(ranks),
                ..
            } if scores == &[20500, 43800, 11000, 24700]
                && ranks == &[3, 1, 4, 2]
        ));
        assert_eq!(
            serde_json::to_string(&events[0]).unwrap(),
            r#"{"type":"end_game"}"#
        );
    }

    #[test]
    fn notify_game_terminate_emits_non_finalising_end_game() {
        let mut bridge = MajsoulBridge::new(None, None);
        let events = bridge.dispatch(&ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_NOTIFY_GAME_TERMINATE),
            payload: json!({ "reason": "client left game" }),
        });
        assert_eq!(events, vec![MjaiEvent::terminated_game()]);
    }

    /// State must reset on a new kyoku — stray `deferred_doras` from the
    /// previous round can't leak into the next.
    #[test]
    fn start_kyoku_resets_dora_state() {
        let mut bridge = MajsoulBridge::new(None, None);
        bridge.seat = Some(0);
        bridge.dora_timing = Some(DoraTiming::PendingBeforeRinshan);
        bridge.deferred_doras = vec!["S".into()];

        bridge.dispatch(&new_round_msg(json!({
            "doras": ["1m"],
            "scores": [25000, 25000, 25000, 25000],
            "tiles": [
                "1m","2m","3m","4m","5m","6m","7m","8m","9m",
                "1p","2p","3p","4p","0p"
            ],
        })));
        assert!(bridge.dora_timing.is_none());
        assert!(bridge.deferred_doras.is_empty());
        assert_eq!(bridge.doras, vec!["1m".to_string()]);
    }

    // ---------------------------------------------------------------
    // Time budget (autoplay::budget) extraction
    // ---------------------------------------------------------------

    fn budget_bridge(seat: Actor) -> (MajsoulBridge, crate::autoplay::budget::SharedTimeBudget) {
        let slot = crate::autoplay::budget::new_shared();
        let mut bridge = MajsoulBridge::new(None, None).with_time_budget(Some(slot.clone()));
        bridge.seat = Some(seat);
        (bridge, slot)
    }

    /// An operation list addressed to our seat fills the shared budget
    /// slot with the wire values (milliseconds).
    #[test]
    fn budget_written_from_our_seat_operation() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({
                "seat": 2,
                "tile": "1m",
                "operation": { "seat": 2, "time_fixed": 5000, "time_add": 20000 },
            }),
        ));
        let b = slot.read().unwrap().expect("budget must be written");
        assert_eq!(b.fixed_ms, 5000);
        assert_eq!(b.add_ms, 20000);
        assert_eq!(b.source, crate::autoplay::budget::BudgetSource::DealTile);
        assert!(b.elapsed_ms() < 1000, "opened_at must be fresh");
    }

    /// An action with no operation list closes the window: the slot is
    /// cleared, not left holding the previous window's budget.
    #[test]
    fn budget_cleared_when_no_operation() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({
                "seat": 2,
                "tile": "1m",
                "operation": { "seat": 2, "time_fixed": 5000, "time_add": 0 },
            }),
        ));
        assert!(slot.read().unwrap().is_some());

        // Another seat draws; no operation for us. `operation` arrives as
        // JSON null on such frames (skip_default_fields(false)).
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({ "seat": 0, "tile": "", "operation": null }),
        ));
        assert!(
            slot.read().unwrap().is_none(),
            "slot must be cleared when no window is open"
        );
    }

    /// Defensive: an operation list addressed to a different seat (the
    /// server never sends these today) must not be treated as our budget.
    #[test]
    fn budget_ignores_other_seat_operation() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({
                "seat": 1,
                "tile": "",
                "operation": { "seat": 1, "time_fixed": 5000, "time_add": 0 },
            }),
        ));
        assert!(slot.read().unwrap().is_none());
    }

    /// Regression: a `GameRestore` replay must not clobber the live slot
    /// with each historical window. Only the window still open at the end
    /// of the replay is committed, with `opened_at` backdated by
    /// `passed_waiting_time` so the delay model knows how much of the
    /// window is already gone.
    ///
    /// `SYNC_GAME_SAMPLE`'s final action (step 31) is our seat-3 draw
    /// carrying `operation { seat: 3, time_fixed: 300000 }`, and the
    /// restore reports `passed_waiting_time: 16`.
    #[test]
    fn game_restore_commits_only_final_window_backdated() {
        let slot = crate::autoplay::budget::new_shared();
        let mut bridge = seated_for_sample().with_time_budget(Some(slot.clone()));

        // Pretend a stale window from before the disconnect is in the slot.
        *slot.write().unwrap() = Some(TimeBudget {
            fixed_ms: 1,
            add_ms: 1,
            opened_at: Instant::now(),
            source: BudgetSource::DiscardTile,
        });

        let payload: JsonValue = serde_json::from_str(SYNC_GAME_SAMPLE).unwrap();
        bridge.dispatch(&resp(METHOD_SYNC_GAME, payload));

        let b = slot
            .read()
            .unwrap()
            .expect("final window must be committed");
        assert_eq!(b.fixed_ms, 300_000, "wire value of the final window");
        assert_eq!(b.source, BudgetSource::DealTile);
        assert!(
            b.elapsed_ms() >= 16_000,
            "opened_at must be backdated by passed_waiting_time (got {}ms)",
            b.elapsed_ms()
        );
        assert!(!bridge.replaying, "replay flag must be reset");
    }

    /// Regression: chankan / babei windows (`ActionAnGangAddGang`,
    /// `ActionBaBei`) carry an operation too — they must fill the slot,
    /// not fall through and clear it.
    #[test]
    fn budget_covers_kan_and_babei_windows() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionAnGangAddGang",
            json!({
                "seat": 0,
                "tiles": "1m",
                "type": 2,
                "operation": { "seat": 2, "time_fixed": 5000, "time_add": 0 },
            }),
        ));
        let b = slot
            .read()
            .unwrap()
            .expect("chankan window must fill the slot");
        assert_eq!(
            b.source,
            crate::autoplay::budget::BudgetSource::AnGangAddGang
        );
    }

    /// Regression: `time_fixed == 0` has unknown semantics (possibly an
    /// unlimited room) — treat as no budget instead of creating a
    /// zero-width window that floors every delay.
    #[test]
    fn budget_zero_fixed_time_means_no_budget() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({
                "seat": 2,
                "tile": "1m",
                "operation": { "seat": 2, "time_fixed": 0, "time_add": 20000 },
            }),
        ));
        assert!(slot.read().unwrap().is_none());
    }

    /// New-game boundaries clear the slot: authGame response and
    /// NotifyGameEndResult.
    #[test]
    fn budget_cleared_at_game_boundaries() {
        let (mut bridge, slot) = budget_bridge(2);
        bridge.dispatch(&action_msg(
            "ActionDealTile",
            json!({
                "seat": 2,
                "tile": "1m",
                "operation": { "seat": 2, "time_fixed": 5000, "time_add": 0 },
            }),
        ));
        assert!(slot.read().unwrap().is_some());
        bridge.dispatch(&ParsedMessage {
            msg_type: MessageType::Notify,
            msg_id: None,
            method_name: Arc::from(METHOD_NOTIFY_GAME_END_RESULT),
            payload: json!({ "result": {} }),
        });
        assert!(slot.read().unwrap().is_none());
    }
}
