# Majsoul Bridge

Decodes Majsoul's `lq.*` protobuf-over-WebSocket protocol into MJAI game,
round, draw, discard, meld and settlement events for the bot's seat.

## Public hand-end input

`ActionHule.hules[].hand` becomes optional `hora.tehai`, and `hu_tile` becomes
`hora.pai`, using the same `ms_to_mjai` mapping as ordinary tiles. Red fives
remain `5mr/5pr/5sr`. The hand excludes the winning tile for both ron and tsumo;
public melds continue to arrive through their own events. `ActionHule.scores`
becomes the shared post-settlement `hora.scores`, including for multi-ron and
the final hand. Malformed tile data or score-vector shapes reject the frame.
The dealer's heavenly win uses the opening partition described below.

`ActionNoTile.players[].hand` supplies `ryukyoku.tehais`, with `null` for
unrevealed/noten seats. This preserves all-tenpai versus no-tenpai even when
payments are zero. The explicit `old_scores` plus summed native-length
`delta_scores` supplies post-payment `ryukyoku.scores`; malformed vectors,
contradictory supplied baselines and integer overflow are errors.
`liujumanguan` distinguishes nagashi from an ordinary exhaustive draw.

Abortive kinds are confirmed against the [official client 0.11.252.w](https://game.maj-soul.com/1/v0.11.252.w/code.js)
(`mjcore.E_LiuJu`, `view.ActionLiuJu.play`): 1 nine terminals, 2 four winds,
3 four kans, 4 four riichi, 5 triple ron. Nine terminals forwards only the
declarer's `tiles`. Four riichi forwards pipe-delimited `allplayertiles` for
all seats; triple ron excludes the named discarder, even if its hand is
present in the payload. Four winds/kans disclose no hands. No live account
data or client source is committed to this repository.

Native consumers can now populate their existing hand-end reveal fields and
commit the final observation before `end_game`. Providers that omit these
fields leave them absent. For offline verification, `cargo run --example
replay_majsoul -- <inspector.jsonl>` replays a local capture through the actual
bridge without connecting to a client or network; output is local MJAI events
tagged by flow. Capture files can contain credentials and must stay local.

## Seat resolution & names

`MajsoulBridge` watches the `.lq.FastTest.authGame` exchange:

- **Request (client→server)**: `payload.account_id` → stored on the bridge.
- **Response (server→client)**:
  - Seat: index of `account_id` in `payload.seat_list` (0..=3 for 4p,
    0..=2 for 3p).
  - **`num_players`**: `seat_list.len()`. Stored on `MajsoulBridge` and
    threaded through every subsequent emission. Anything other than 3 or
    4 falls back to 4 with a warn.
  - Names: build `account_id → nickname` map from `payload.players[]`,
    then walk `seat_list` to produce a length-`num_players` Vec. Robot
    seats live under `payload.robots[]` without a nickname, so they get
    `""`. **No length-4 padding** — sanma authGame emits a length-3 names
    array natively.
  - `gameConfig.meta.mode_id` / `room_id` / `contest_uid` are captured (zero
    = "not applicable" → `None`) into the StartGame `game_meta.match_info`,
    together with the raw `game_uuid` from the authGame request; history
    persists them as `GameRecord.match_info`. Sanma AI rooms emit
    `mode_id = 0`; ranked sanma uses 21/22/26. Never gated on.
- Emits `MjaiEvent::StartGame { id: Some(seat), names, num_players, .. }`.

`aka_flag` / `kyoku_first` aren't known yet at authGame time, so they stay
`None` and are omitted from the JSON via `#[skip_serializing_none]`.

## ActionNewRound → start_kyoku + tsumo

`.lq.ActionPrototype` Notify with inner `name = "ActionNewRound"`.

Majsoul deals 14 tiles to the dealer and 13 to everyone else, then the
dealer discards. mjai instead deals 13 to everyone, then emits an explicit
`tsumo` for the dealer's first draw. The bridge converts:

- **We're the dealer** (`ju == self.seat`, payload carries 14 tiles):
  `tehais[seat]` = sorted first 13 tiles. Emit:
  - `start_kyoku` (others' rows = `["?"; 13]`)
  - `tsumo { actor: seat, pai: <14th tile, raw> }`
- **We're not the dealer** (13 tiles):
  `tehais[seat]` = sorted 13 tiles. Emit:
  - `start_kyoku` (others' rows = `["?"; 13]`)
  - `tsumo { actor: oya, pai: "?" }` — we don't know what the dealer drew.

Field mapping from `ActionNewRound`:

| mjai field    | majsoul field          |
|---------------|------------------------|
| `bakaze`      | `chang` → `E S W N`    |
| `kyoku`       | `ju + 1`               |
| `oya`         | `ju`                   |
| `honba`       | `ben`                  |
| `kyotaku`     | `liqibang`             |
| `dora_marker` | `doras[0]` (mjai-mapped) |
| `scores`      | `scores` (length matches `num_players` — 3 for sanma, 4 for yonma; **no padding**) |
| `tehais`      | length-`num_players` Vec; `tehais[seat]` is `tiles` (sorted, see above), all other rows are `["?"; 13]` |
| `num_players` | resolved from `seat_list.len()` at authGame; stamped on the emitted event |

Tile mapping uses `tile::ms_to_mjai` (`0m/0p/0s` → `5mr/5pr/5sr`,
`1z..7z` → `E S W N P F C`). Unknown tile strings error and the event is
skipped instead of corrupting the mjai stream.

The dealer's fourteen tiles are one Majsoul opening deal, not a distinct
thirteen-tile head plus a live draw. The official client `ViewPlayer_Me.NewGame`
sorts the deal; its first `ActionDiscardTile.moqie=false` can therefore discard
the tile this bridge chose as synthetic `tsumo`. If that exact tile has no
copy in the first thirteen, the bridge emits `dahai.tsumogiri=true`, the only
valid MJAI identity for that discard. With a matching hand copy it preserves
wire `moqie`; a different tile belongs to the thirteen-tile hand regardless
of the wire's opening slot flag. Ordinary and red fives are distinct. No tile is moved, invented,
removed twice, or inferred from an opponent's hand. Non-dealer and subsequent
discards retain their wire flags. A real draw, kan, kita or hand end retires
the opening context; double-riichi and GameRestore use the same conversion.

For our dealer's heavenly win, `HuleInfo.hand + hu_tile` must equal the
fourteen dealt tiles, including exact red identities and multiplicities.
The bridge emits `hora.pai` as the same synthetic draw and `hora.tehai` as
the same thirteen-tile head used in `start_kyoku`. This avoids a contradictory
winning-tile identity when the provider designates another opening tile.
Missing or contradictory opening reveals are rejected. Other players' wins
and wins after a discard/kan/real draw keep their supplied identities.

This projection never recomputes `fans`, yakuman units, `delta_scores` or
post-settlement `scores`: those remain the provider's actual result. Raw
frames retain the original `hu_tile`. The training/model kernel can keep
Tenhou scoring, including its single-unit yakuman variants, without replacing
Majsoul's actual settlement. This does not make its policy values equivalent
under both rule sets; it makes the observed state and settlement consistent.

This normalization is platform semantics, independent of room/rank/reward
settings. The single-seat inference rules continue to reject impossible hand
discards. In particular, loosening native legal-action checks would hide this
bridge error rather than fix it.

## Meld actions → chi / pon / daiminkan / ankan / kakan

`.lq.ActionPrototype` Notify with one of:

- `name = "ActionChiPengGang"` — open meld. `type = 0/1/2` →
  `chi` / `pon` / `daiminkan`. `actor = data.seat`. `target` and `pai`
  come from the unique non-actor seat in `froms[]`; the remaining tiles
  (where `froms[i] == actor`) are `consumed`. Mjai-mapped via
  `tile::ms_to_mjai`.
- `name = "ActionAnGangAddGang"` — closed/added kan. `tiles` is a single
  tile string. `type = 3` → `ankan` (consumed = 4 of that tile, with the
  red five at index 0 when applicable — every quad of 5m/5p/5s must
  contain the lone red); `type = 2` → `kakan` (`pai` = added tile,
  `consumed` = the 3 tiles already in the existing pon; if the new tile
  is the red five, the existing pon was three normals, otherwise the
  pon held the red).

### Dora flip timing (即乗り vs 後乗り)

Mjai distinguishes two cases — Akagi-Python and AkagiNG both conflate
them and emit dora at the wrong moment. The bridge's `DoraTiming` state
machine fixes that:

| Meld         | Sequence                                      | DoraTiming             |
|--------------|-----------------------------------------------|------------------------|
| **ankan**    | `ankan → dora → tsumo (rinshan) → dahai`      | `PendingBeforeRinshan` |
| **kakan**    | `kakan → tsumo (rinshan) → dora → dahai`      | `PendingAfterRinshan`  |
| **daiminkan**| `daiminkan → tsumo (rinshan) → dora → dahai`  | `PendingAfterRinshan`  |

On the wire the live server matches this split (confirmed against a
captured game with three kans in one kyoku, issue #244):

- **ankan** — the new marker rides the rinshan `ActionDealTile.doras`.
- **kakan / daiminkan** — the rinshan deal does *not* carry it; the
  grown `doras` array rides the declarer's commit event, i.e. the next
  `ActionDiscardTile` (or `ActionBaBei` when a 3p declarer pulls North
  instead of discarding).

Every payload that carries a `doras` array (`ActionDealTile`,
`ActionDiscardTile`, `ActionAnGangAddGang`, `ActionBaBei`) is diffed
against the local `doras` list by `consume_new_doras`, which emits
*all* newly appended markers and syncs the local list to the full
server array. The full sync is what prevents the list from falling
behind — a stale shorter list would make a later *unchanged* array
look like a new reveal and re-emit an old marker (the duplicated-dora
half of issue #244; the other half was `build_dahai` not reading
`doras` at all, which dropped every kakan/daiminkan reveal).

Markers that arrive *early* for an open kan (already on the rinshan
deal) are held in `deferred_doras` and flushed just before the `dahai`
(or `kita`) event. `start_kyoku` resets all three fields (`doras`,
`dora_timing`, `deferred_doras`) so stale state from the previous kyoku
can't bleed in.

## Riichi (ActionDiscardTile.is_liqi / .is_wliqi)

Majsoul carries riichi as a flag on the declaration discard, not a
separate action. When `is_liqi` (or `is_wliqi`, double riichi) is true on
`ActionDiscardTile`, the bridge emits `[reach, dahai]` and queues a
`reach_accepted` event in `pending_reach_accepted` (Actor of the
declarer).

Per the mjai spec (riichi declaration §12 + state machine), the points
deduction is bookkept by `reach_accepted` only **after** the declaration
tile passes through. The bridge implements that as: on the *next*
ActionPrototype event, `handle_action_prototype` prepends
`reach_accepted` to the resulting events and clears the queue.

Two exceptions to draining:

- **Ron on the declaration tile** (next action is `ActionHule`): riichi
  is voided. The queue is dropped without emitting `reach_accepted`.
- **`ActionDiscardTile` immediately after riichi**: shouldn't happen in
  practice (the declarer can't discard twice in a row), so we leave the
  queue intact for the next legitimate action to drain.

> Akagi-Python prepends `reach_accepted` (matches the spec). AkagiNG
> appends it after the next action's events (off by one position
> relative to the spec). Our bridge follows Akagi-Python here.

`pending_reach_accepted` is reset on every `start_kyoku` so a queued
acceptance from a previous round can't leak into the next.

## Game termination — NotifyGameEndResult

`.lq.NotifyGameEndResult` is a *top-level* notify (not wrapped in
`ActionPrototype`). Its `result.players[]` carries final standings
(seat → `total_point` / `part_point_1` etc.) — useful in the flow log
but not part of the mjai stream, since mjai `end_game` has no payload.
The bridge emits a single `EndGame` event and lets the consumer terminate.

## Round termination — ActionHule

`ActionHule` (胡牌) → one `hora` per entry in `data.hules[]`, followed
by a single `end_kyoku`.

| mjai field   | majsoul source                                           |
|--------------|----------------------------------------------------------|
| `actor`      | `hule.seat`                                              |
| `target`     | `actor` if `hule.zimo`, else `self.last_discard`         |
| `deltas`     | `data.delta_scores` (top-level)                          |
| `ura_markers`| `hule.li_doras` (mapped) when `hule.liqi`, else `None`   |

`HuleInfo` doesn't carry the target seat, so the bridge tracks
`last_revealed_tile_actor: Option<Actor>` — the seat whose most recent
action exposed a tile that another seat could ron. Updated by:

- `ActionDiscardTile` — normal ron.
- `ActionAnGangAddGang(kakan)` — 搶槓 (chankan).
- `ActionAnGangAddGang(ankan)` — Majsoul's 国士無双搶暗槓 (only kokushi
  can rob; the server only emits `ActionHule` in that valid case, so
  unconditional tracking is safe).
- `ActionBaBei` (3p) — 搶北 / search-the-kita. `build_kita` sets
  `last_revealed_tile_actor = Some(seat)`, so a follow-up `ActionHule`
  with `hu_tile = "4z"` attributes the win to the kita declarer.

If we just used the last discard, chankan and the ankan robbery edge
case would attribute the win to the wrong player. A ron with no prior
tile-revealing action is malformed and skipped with a warning. Reset at
every `start_kyoku`.

`li_doras` becomes `Some(vec![])` (not `None`) for a riichi win with no
ura markers, so consumers can distinguish "had riichi but no ura" from
"no riichi".

**Multi-ron**: each `hule` entry produces its own `Hora` event with the
same top-level `delta_scores` attached; the consumer dedupes if needed.

**Riichi voiding**: a ron on the declaration tile clears
`pending_reach_accepted` without emitting `reach_accepted` (state
machine: `dahai → hora` skips the `reach_accepted` branch). The hora
event itself still emits normally.

> Akagi-Python and AkagiNG both reduce `ActionHule` to just `end_kyoku`
> (Akagi has the per-event scaffold but it's commented out). Our bridge
> emits the full `[hora, …, end_kyoku]` sequence with deltas + ura.

## Round termination — ActionNoTile / ActionLiuJu

| Majsoul action | Cause | Mjai output |
|----------------|-------|-------------|
| `ActionNoTile` | 荒牌流局 (exhaustive draw, possibly tenpai redistribution / nagashi mangan) | `[ryukyoku{deltas}, end_kyoku]` |
| `ActionLiuJu`  | 途中流局 — 九種九牌 / 四風連打 / 四家立直 / 四開槓 / 三家和了 | `[ryukyoku{deltas:None}, end_kyoku]` |

`ActionNoTile.scores[]` is an array of per-event point change records;
each carries its own `delta_scores` array sized to `num_players` (3 for
sanma, 4 for yonma). The bridge sums them per seat into a single
`Vec<i32>` of native length — no padding. Empty / missing `scores` →
`deltas: None`.

`ActionLiuJu.type` is a `uint32` with no enum in `liqi.proto`; both
Akagi-Python and AkagiNG ignore the field and emit a generic ryukyoku.
We follow suit. Sanma's abortive set is reduced (no four-wind / four-
riichi — those need 4 players); kyushu_kyuhai and suukansansen still
apply.

> The mjai spec text restricts the `ryukyoku` event to 九種九牌, but
> `libriichi` / Mortal use the same event for noten payments and rely
> on the optional `deltas` field. We follow the libriichi convention so
> downstream stat code can attribute the payment correctly. Akagi-Python
> doesn't handle `ActionNoTile` at all; our bridge is more complete here.

If a riichi was declared on the round's final discard and the round
immediately ends in ryukyoku, `pending_reach_accepted` is drained as a
prepended `reach_accepted` before the terminal events (declaration tile
passed through with no ron, so the riichi is accepted).

## ActionDealTile → tsumo

`actor = data.seat` (default 0). When `actor == self.seat` and `data.tile`
is non-empty, `pai = ms_to_mjai(data.tile)`; otherwise `pai = "?"` (we
don't see other players' draws).

## ActionDiscardTile → dahai

`actor = data.seat` (default 0 — Majsoul omits the seat field on the
dealer's first discard). `pai = ms_to_mjai(data.tile)` (must be present;
empty/missing tile is treated as a malformed frame and the event is
skipped). `tsumogiri = data.moqie` (default false).

> Note: `parser.rs` sets `SerializeOptions::skip_default_fields(false)`
> so proto defaults (`moqie: false`, `seat: 0`, empty `tile`) appear
> verbatim in the JSON payload. Logic in `mod.rs` still defaults missing
> fields, so the two are equivalent at the dispatch layer — the visible
> setting matters mainly for the flow log.

## ActionBaBei → kita (3p only)

3-player tables get one extra action: `ActionBaBei` (北抜き / nukidora).
Player sets aside a North tile from hand and draws a rinshan replacement.
The bridge emits:

```json
{"type": "kita", "actor": <seat>, "pai": "N"}
```

Notes:

- Majsoul's payload omits the tile field — kita is always the North wind.
- `doras: []` in the captured payload — kita itself does **not** flip a
  new dora marker (Tenhou rule, confirmed live). The array is still
  diffed via `consume_new_doras`: after a kakan/daiminkan the declarer
  may pull North instead of discarding, making the babei the commit
  event that carries the kan's 後乗り reveal (emitted before `kita`,
  together with any `deferred_doras`).
- `last_revealed_tile_actor` updated to the kita declarer so a follow-up
  ron-on-kita (chankan-style 搶北) targets correctly in `build_hule`.
- `pending_reach_accepted` drains normally — kita is a non-Hule action.
- If `num_players != 3`, log warn but still emit (avoid silent loss).

## Chi suppression in 3p

Sanma has no chi. `build_chi_peng_gang` drops `ActionChiPengGang(chi)`
with a warn log when `num_players == 3` (defense in depth — Majsoul
shouldn't send it for sanma tables). Pon / kan stay enabled.

## Mid-game reconnect — syncGame / enterGame (GameRestore)

When a client reconnects to an in-progress match (page reload, or the
controlled-Chromium relaunch Akagi does on restart), Majsoul re-runs
`authGame` and then sends a `.lq.FastTest.syncGame` (or `.lq.FastTest.enterGame`)
**response** whose `game_restore.actions[]` replays every action of the current
kyoku, from `ActionNewRound` up to the present. Without this, a restarted Akagi
would have no hand until the next kyoku.

`handle_game_restore` reconstructs the in-progress hand by replaying that log:

- It reads `game_restore.actions[]` (each entry an
  `ActionPrototype { name, data, step }`). An absent/empty list — e.g. a
  first-entry `enterGame` or a finished game — is a no-op.
- Each action is decoded and re-wrapped into the same `{name, data}` shape a
  live `ActionPrototype` carries, then fed through `handle_action_prototype`, so
  **every `build_*` conversion and the `pending_reach_accepted` bookkeeping is
  reused verbatim**. A riichi left pending at the end of the replay drains onto
  the first subsequent live action, exactly as in live play.
- **No `start_game` is emitted** — the preceding `authGame` already did that
  (the single reset point for the downstream tracker / bot / autoplay). The
  leading `ActionMJStart` has no handler and is a no-op, as is any other
  unrecognised action. The `GameRestore.snapshot` field is ignored; replaying
  the actions reconstructs the full state.

> **No XOR on restore actions.** Unlike live `.lq.ActionPrototype.data` (which is
> XOR-obfuscated, see *Action XOR* below), the `data` of actions embedded in a
> `GameRestore` is plain base64 protobuf. `parser::decode_restore_action`
> base64-decodes and protobuf-decodes directly, skipping `wtf_decode` — running
> the XOR on them would corrupt the bytes.

The whole kyoku is emitted as one event burst; see `event_bus::DEFAULT_CAPACITY`
for why the mjai bus buffer must comfortably exceed a full kyoku's event count.

## mjai event log

When constructed with an `Arc<Session>`, the bridge rotates a fresh
`<session>/majsoul/majsoul_<ts>.mjai.jsonl` file every time it emits a
`start_game` event, then appends each subsequent emitted `MjaiEvent` as
one JSON line. One file per game; multiple games on the same WS flow
produce multiple files. Without a session (e.g. tests), rotation is a
no-op and only the parsed-frame text log (if any) is written.

## Replays → tenhou.net/6 and NAGA

Opening a replay in the client sends `.lq.Lobby.fetchGameRecord`; its
response holds the whole game, every seat's starting hand and draws included,
which the live feed never shows. When the bridge has a replay folder
(`with_replay_dir`; the embedded capture gives it `<home>/replays`), it
converts that response with `tenhou_log` and writes two files named by the
game's uuid:

- `<uuid>.json` — the tenhou.net/6 log of the whole game, for any viewer
  that reads that format.
- `<uuid>.naga.txt` — one `http://tenhou.net/6/#json=` line per round, the
  text NAGA's custom-game order form takes. Nothing is sent anywhere.

Opening the same replay again rewrites both. A record that comes only as a
`data_url`, a 3-player game or a malformed record is logged and skipped; the
replay plays on either way. `cargo run --example replay_majsoul --
<inspector.jsonl> <dir>` exports the replays a past capture opened.

The conversion is ported from honvl/Majsoul-to-NAGA (MIT, see `NOTICE`),
whose fixture is the test in `testdata/`. Abortive draws are named from
`RecordLiuJu.type` with tenhou's names (九種九牌, 四風連打, 四槓散了,
四家立直, 三家和了) rather than guessed from riichi and kan counts.

## Files

- `parser.rs` — `LiqiParser`, the per-flow decoder.
- `tenhou_log.rs` — a replay's game record → tenhou.net/6 log and NAGA lines.
- `testdata/` — the reference replay record and the log it converts to.
- `mod.rs` — `MajsoulBridge` that wraps the parser behind the `Bridge` trait.
- `tile.rs` — Majsoul ↔ mjai tile-string mapping + canonical mjai sort order.
- `liqi.json` — flat rpc-map: `".lq.Service.method" → {req, resp}` with
  leading-dot fully-qualified type names. Embedded with `include_str!`.
- `proto/liqi.proto` — message definitions. Compiled by `build.rs`
  (project root) into a `FileDescriptorSet` (`liqi_desc.bin` in `OUT_DIR`)
  embedded with `include_bytes!`. The Rust message structs are also
  generated but currently unused — `Wrapper` is decoded inline via a small
  `prost::Message` derive in `parser.rs`.

> Both files are generated by `scripts/extract_liqi.py`, which reconstructs
> the schema directly from the Mahjong Soul Unity client asset bundles
> (protobuf descriptors in `Protol/*_pb.lua`, service table in
> `docs/proto_config.bytes`). The `auto-liqi` GitHub workflow runs it daily
> and opens a PR when the schema changes. There is no dependency on any
> external proto release or on the legacy `res/proto/liqi.json` CDN file.

## Wire format

```
[type byte] [msg_id u16 LE?] [Wrapper protobuf]
                              ├─ name : string  (method name, e.g. ".lq.Lobby.oauth2Login")
                              └─ data : bytes   (inner message protobuf)
                                        └─ inner DynamicMessage
                                           └─ {name, data: base64(XOR(protobuf))}   (action only)
```

| Byte 0 | Meaning  | Has u16 LE msg_id at 1..3? |
|:------:|----------|:--------------------------:|
| `01`   | Notify   | no                         |
| `02`   | Request  | yes                        |
| `03`   | Response | yes                        |

### Method-name routing

`Wrapper.name` is fully-qualified, e.g. `.lq.Lobby.oauth2Login`.

- **2 parts** (`lq.NotifyX`) → look up `lq.NotifyX` directly in the
  descriptor pool.
- **3 parts** (`lq.Service.method`) → look up
  `liqi.json[".lq.<Service>.<method>"]` for the `{req, resp}` type names,
  then resolve those in the pool.

Responses carry an empty `name`, so the parser stores
`(msg_id → (method_name, response_descriptor))` in a `pending` HashMap on
each Request and consumes it on the matching Response.

### Action XOR

Some Notify payloads have shape `{name: "...", data: "<base64>"}`. The
parser base64-decodes `data`, runs `wtf_decode`, then decodes the result
as the protobuf message named by `name`. The XOR is position- and
length-dependent:

```rust
const KEYS: [u8; 9] = [0x84, 0x5E, 0x4E, 0x42, 0x39, 0xA2, 0x1F, 0x60, 0x1C];
let base = 23 ^ data.len();
for (i, b) in data.iter_mut().enumerate() {
    *b ^= (base + 5 * i + KEYS[i % 9] as usize) as u8;
}
```

The function is its own inverse (XOR), so the same call decrypts and
re-encrypts.

## Per-flow state

One `LiqiParser` per WebSocket flow. Each flow has its own `msg_id`
sequence, so sharing a parser across flows would corrupt the
request/response correlation.

The proxy handler (`src/proxy/handler.rs`) constructs a fresh
`MajsoulBridge` (and thus a fresh `LiqiParser`) inside `handle_websocket`.

## Adding new behaviour

- **New action types / messages / service methods**: don't hand-edit —
  re-run `scripts/extract_liqi.py` (or let the `auto-liqi` workflow do it) to
  regenerate `proto/liqi.proto` + `liqi.json` from the live Unity client.
  Pool lookup happens dynamically by name.
- **Mjai conversion**: add a state machine layer that consumes
  `ParsedMessage` and emits `MjaiEvent`. Game state (seat assignments,
  tehai, doras, …) lives there, not in the parser.
- **Outbound build**: implement `Bridge::build` to take an `MjaiEvent`
  and return the wire bytes (autoplay).

## References

- Majsoul protobuf descriptors — reconstructed from the Unity client asset
  bundles by `scripts/extract_liqi.py` into `proto/liqi.proto` + `liqi.json`.
- mjai protocol spec — used as the canonical event-type reference.

Third-party Rust / Python parsers exist for the same protocol under
copyleft licences; none of their source code is copied into this tree.
