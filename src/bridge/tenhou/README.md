# Tenhou bridge

Translates Tenhou (天鳳) WebSocket frames into the mjai event stream the rest
of AkagiV3 consumes, and back again for autoplay.

Gameplay comes from server → client frames. The sole observed client boundary
is `NEXTREADY`, the official client's acknowledgement of its complete result
queue; it flushes a pending win group. Other client input is ignored.
`Bridge::build` goes the other way — it encodes a bot's chosen
action as the client frame that performs it (see `encode.rs`). Autoplay does
**not** use it: the Tenhou client owns its board state and freezes if a discard
reaches the server without going through its own handler, so `autoplay::tenhou`
drives the client's input path instead and takes only the tile-index lookup
from that module.

## Wire format at a glance

Tenhou's WS frames are plain JSON: one event per frame, dispatched by `tag`.
The complete tag inventory and field semantics follow the original Akagi
Python Tenhou bridge; this is a faithful Rust port, with comments calling
out any deliberate divergence.

### Tags we handle

| Tag | Trigger | mjai output |
|---|---|---|
| `<Z/>` | heartbeat | (none) |
| `HELO` / `REJOIN` / `SAIKAI` / `BYE` / `SHUFFLE` | session control | (none) |
| `GO` | rules / room, before `TAIKYOKU` | (none; stashed for `MatchInfo`, bit `0x10` = sanma) |
| `UN` | roster, wire-relative names | (none; consumed by `start_game`, an empty slot = sanma) |
| `TAIKYOKU` | start of game | `start_game` (resolves our seat from `oya`) |
| `INIT` | start of kyoku | `start_kyoku` (sanma detected via 0-score slot) |
| `REINIT` | rejoin: snapshot of the hand in progress | (none — see *Reconnecting mid-hand*) |
| `T<n>` / `U<n>` / `V<n>` / `W<n>` | tsumo (rel seats 0..3) | `tsumo` |
| `D<n>` / `E<n>` / `F<n>` / `G<n>` (uppercase) | tedashi (discard from the hand) | `dahai { tsumogiri: false }` |
| `d<n>` / `e<n>` / `f<n>` / `g<n>` (lowercase) | tsumogiri (discard of the just-drawn tile) | `dahai { tsumogiri: true }` |
| `N` with `m` | call (chi/pon/kan/kakan/nukidora) | `chi` / `pon` / `daiminkan` / `kakan` / `ankan` / `kita` |
| `REACH step=1` | declare riichi | `reach` |
| `REACH step=2` | riichi accepted | `reach_accepted` |
| `DORA` | new dora indicator | `dora` |
| `AGARI` (no `owari`) | win | buffer until the result boundary |
| uplink `NEXTREADY` / next `INIT` | complete win group | all `hora`, then one `end_kyoku`, before any new opening |
| `AGARI` (with `owari`) | win at game end | flush all pending `hora` + one `end_kyoku` + `end_game` |
| `RYUUKYOKU` (no `owari`) | exhaustive draw | `ryukyoku` + `end_kyoku` |
| `RYUUKYOKU` (with `owari`) | draw at game end | `ryukyoku` + `end_kyoku` + `end_game` |

The tag case only decides tsumogiri for *other* seats. Our own discards
compare the tile against our last draw instead (`on_dahai`), which also
covers the digit-less own-discard echo (a `D`/`d` tag with no tile index).

### Tile encoding

`AGARI.machi` is the physical winning tile; `hai` includes it. The bridge
removes exactly that ID once to emit `hora.tehai`, preserving ordinary/red
five identities and duplicate tile kinds. Missing/invalid IDs and malformed
score vectors reject the group. All seats, hands and score vectors rotate
from wire-relative to MJAI-absolute order, omitting only the sanma ghost.

`sc` contains four interleaved before/delta pairs in units of 100 points.
In a double ron, the second baseline includes the first winner's settlement.
The bridge validates that chain and gives both `hora` events the same final
`scores`; each keeps its own partial `deltas`, so history adds each payment
once. Duplicate winners, different winning tiles/sources and contradictory
baselines invalidate the whole pending group. There is no timing heuristic.

`RYUUKYOKU.hai0..hai3` reveal only the supplied hands, with `null` for other
seats. Its explicit `type` maps `nm/yao9/kaze4/reach4/ron3/kan4` to
`nagashi/kyuushu/four_winds/four_riichis/three_rons/four_kans`; absent `type`
means exhaustive. Unknown types fail. The actual `sc` totals settle the hand
immediately. No hidden opponent hand is supplied and training rules stay put.

The live boundary is verified against the [official client](https://tenhou.net/4/latest.js)
(`R.c6`, `Xb.hj`, `Nb.AGARI`); sequential double-ron payments are also covered
by the original decoder's [public raw fixture](https://github.com/kobalab/tenhou-log/blob/03fc4a38cbd10f64ea8a883cffd2a4f0c204ba83/test/convlog.js).
Public official XML replays can be read at `https://tenhou.net/0/log/?<log-id>`;
they require rotation/hiding for a chosen viewer before live-bridge use.

Tenhou tiles are integer indices `0..=135`. `index / 4` gives tile type
(`0..=33`); `index % 4` is the variant. Red 5s live at exactly `16`, `52`, `88`
(serialize as `5mr`, `5pr`, `5sr`). See `tile.rs`.

### Seat encoding

Tenhou messages always use *relative* seats: rel 0 is the observing player.
`State::rel_to_abs` / `abs_to_rel` translate to mjai's absolute frame. The
bridge resolves our absolute seat from `<TAIKYOKU oya="N"/>`: `seat = (4 - N) % 4`.
Wire-abs 0 is whoever dealt E1, and the wire stays 4-positional in sanma (the
absent seat is a ghost at wire-abs 3; `seed[0]` skips its deal, so E1 E2 E3 S1
are 0 1 2 4).

A flow that starts mid-game — a reconnect, or Akagi attaching between hands —
never sees `<TAIKYOKU/>`. Every kyoku frame (`INIT` / `REINIT`) carries the
same information in another form: the dealer of kyoku index `seed[0]` sits at
wire-abs `seed[0] % 4`, and `oya` is that dealer's seat relative to us, so
`state::seat_from_kyoku` recovers ours. `TAIKYOKU` stays authoritative when
both were seen; the kyoku frame only fills in when it was not
(`State::seat_resolved`).

### Player count

`start_game.aka_flag` comes from known `GO.type & 0x02` (set means no red
fives); absent GO leaves it unknown. East/East-South is `GO.type & 0x08`,
stored as `game_meta.match_mode` 1/2. A complete UN roster supplies our raw
`dan` entry 0 regardless of our absolute seat. It is consumed with that
roster; partial reconnect names and missing/bad rank entries supply no rank.
`MatchInfo::Tenhou` persists optional `dan`; the bot's game description is
exactly `{platform:"tenhou", num_players, go_type, lobby, dan}`, using JSON
nulls for unknown metadata. No player name, replay ID or inferred rank enters
that description.

`DecisionWindow.clock: Option<TurnClock>` carries the known standard clock:
`TurnClock { fixed_ms, bank_ms }`. Public GO lobby 0, known standard flags and
no custom `csrule` yield normal 5000/10000 or fast (0x40) 3000/5000 ms, as in
the [official manual](https://tenhou.net/man/) and client's GO handler.
Private/custom/unknown rules yield `None`. `bank_ms` is the initial reserve
cap, not a measured remaining balance; consumers must not assume it is still
available. Parent integration owns inference-deadline and delay consumption.

### Offline qualification (2026-10-27)

The isolated owner harness uses integration base `05d48f3` plus these owned
bridge/schema/game-description/window changes; other owners' unfinished input
implementation is excluded. Baseline test literals receive explicit unknown
clock / non-riichi fields in that generated harness only. Results: 92 Tenhou
bridge tests, 5 shared-window tests, 7 history-schema tests and 34 bot-manager
tests passed (**138 total**). The canonical combined build remains the parent
session's integration check; this qualification does not claim that check.

The public official game
[`2016031822gm-0009-10011-896da481`](https://tenhou.net/0/log/?2016031822gm-0009-10011-896da481)
was converted into four private live views, retaining only each viewer's own
hand/draws and actual public reveals. All 5864 protocol frames went through
the actual Rust bridge; all **5912 MJAI events / 52 viewer-hands / 693 own
draws** replayed through the native NGK single-seat Tracker without desync.
Every explicit terminal score matched the provider's settlement. No hidden
wall or opponent hand was passed to inference, and no model weights changed.

Local untracked reproduction inputs/drivers are `.qa/tenhou-public-frames.jsonl`,
`.qa/replay_tenhou.rs`, `.qa/qualify_tenhou_public.py`, and the aggregate
`.qa/tenhou-public-qualification.json`. The named harness artifacts are
`design/staged/core-harness/tests/tenhou-contract-tests.exe` and
`design/staged/core-harness/tests/qa_tenhou.exe`. They are offline tools, not a
production deployment. Live result timing, GUI input and real-account play
were not tested here.

`start_game` must say 3 or 4, and only the E1H0 `<INIT/>` proves it (a 0 in
`ten` at the very first deal is the absent seat; nobody starts a real game on
0 points). That check is final. Before it fires — and on a flow that joined
mid-game and will never see it — the bridge takes the earlier hints in this
order: `<GO type>` bit `0x10`, then a `<UN/>` roster with exactly one empty
slot, then the table total on the kyoku frame itself (`ten` plus the riichi
sticks in `seed[2]`: Tenhou deals 100000 points in yonma and 105000 in sanma,
and a bust ends the game, so the total never changes — see
`state::three_players_from_scores`), and as a last resort on `REINIT` the
shape of the snapshot: the ghost's score still 0, its river empty, and no
2m–8m in our hand or any river.

### Meld bitfield

`<N m="..."/>` packs the meld kind, target seat, and tile composition into one
integer. Bit decoding lives in `meld.rs` and follows
<http://tenhou.net/img/mentsu136.txt> exactly. Nukidora (北抜き) is the special
case `(m & 0x3F) == 0x20` — handled before the structured decoder.

### The `t` attribute — decision windows

Tsumo and discard frames may carry a `t` bitmask naming what the server is
offering us. It plays the same role Majsoul's `OptionalOperationList` does, and
the bridge tracks it in `State::window` for `autoplay::tenhou_state`. The bits
mean different things depending on which frame carried them, but the two sets do
not overlap:

| bit | on our draw (`T<n>`) | on a discard (`D`/`E`/`F`/`G<n>`) |
|---|---|---|
| 1 | — | pon |
| 2 | — | daiminkan |
| 4 | — | chi |
| 8 | — | ron |
| 16 | tsumo agari | — |
| 32 | riichi | — |
| 64 | 九種九牌 | — |

Ankan and kakan are not in the mask — the client derives them from the hand, and
so does Akagi, via the riichi engine's legal actions. Parsing is unconditional
even though only autoplay consumes it: it is cheap, and a stale window is worse
than none.

One more frame carries the claim bits: an opponent's kakan `N` when the kan can
be robbed (chankan). The client builds its ron menu straight from that
attribute — `(u=~~a.t)&&ec.Ni(u)` in its `N` handler — and the bridge opens a
window from it the same way it does for a discard. Our own calls never open one
off their own frame; the rinshan draw that follows does.

## Encoding actions (`encode.rs`)

The inverse direction, and the only implementation of `Bridge::build` for this
platform. Nothing sends its output today — see above — but the tile lookup at
its core is what autoplay uses to name a discard, and the frame table is the
executable statement of the client protocol.

Tenhou addresses tiles by index in `0..=135` — the specific physical copy — so
an mjai tile *string* only resolves against a tracked hand, which is why this
lives in the bridge rather than in autoplay.
Lookup scans candidates in descending index order and matches the red/plain
distinction exactly, so a request for a plain five never consumes the red copy;
an unsatisfiable request fails instead of substituting the wrong tile. Matched
tiles are removed from a working pool, so a pon of two identical tiles yields
two distinct indices.

Session control (`JOIN` / `GOK` / `NEXTREADY`) is deliberately **not** encoded:
Akagi observes a real client, which sends those itself.

## Reconnecting mid-hand (`REINIT`)

When the web client rejoins a game it opens a fresh socket, so the bridge
that sees the rejoin is a fresh one. The captured sequence is

```
↑ HELO            ↓ GO (type, lobby)      ↓ UN (full roster)
↑ GOK             ↓ SAIKAI (ba, oya, sc)  ↓ REINIT …   then live play
```

— **no `TAIKYOKU`**. `<REINIT/>` stands in for `<INIT/>`: `seed` / `ten` /
`oya` / `hai` as in `INIT`, plus `m<rel>` for every seat that has called
(comma-separated `<N m=…/>` bitfields; absent otherwise) and `kawa<rel>`
(tile indices, with `255` marking the riichi declaration — the next entry is
the riichi tile). It is a *snapshot*, not a log — nothing says when a call
happened relative to the discards — so the mjai event stream for the hand in
progress cannot be reconstructed. (Majsoul's `GameRestore` is an ordered
action log, which is why the Majsoul bridge can replay and this one cannot.)

The bridge therefore:

1. resolves our seat and the player count from the frame (see above), so the
   rest of the game is attributed correctly even though `<TAIKYOKU/>` was
   never seen on this flow;
2. rebuilds only what this flow itself needs — our hand (`hai`), our calls
   (`m0`), riichi (`255` in `kawa0`) and whether a draw is in hand (tile
   count) — so autoplay and `Bridge::build` keep working for the remainder of
   the hand;
3. emits **nothing** and sets `State::suspended`: every handler still runs
   (the hand and window keep tracking), but `dispatch` drops their events
   until the next `<INIT/>`. Feeding them to the tracker would apply live
   play to a hand it last saw several turns ago (or, with a seat-0 bridge, to
   the wrong seats), and each `?` draw that lands on our seat renders as 1m;
4. posts a warning toast (via `BridgeHooks::notify`) and owes a fresh
   `start_game` at the next `<INIT/>`, which reopens the game for the
   tracker, bots and history on the right seat and player count.

The game's end is the one event that still passes while suspended, so the
tracker closes the game, the bot manager stops its runner and History resets.
Whether it arrives during the suspended hand or after play resumed, the
`end_game` of a game this flow joined mid-way is emitted as *terminated*
(`MjaiEvent::terminated_game`): the hands before the rejoin were never seen
here, so History drops the game instead of filing the remainder as a complete
one with its own per-hand stats. The flag clears at the next `<TAIKYOKU/>`,
or at an E1H0 `<INIT/>` — the first deal is a game start whether or not a
`TAIKYOKU` announced it, and it re-seats the flow the same way `TAIKYOKU`
would.

## Adding a new tag handler

1. Add a `match` arm in `TenhouBridge::dispatch` (`mod.rs`) that routes the new
   tag to a private handler.
2. Implement the handler. It should return `Vec<MjaiEvent>`. If you need new
   per-flow state, add it to `state::State` (and reset in `reset_for_kyoku`
   when appropriate).
3. Add a unit test covering at least one realistic JSON input.

## Why client frames are still not parsed

`parse(Direction::Up, ..)` remains a no-op even now that autoplay exists. What
autoplay does goes through the client's own handlers, and the client then sends
the frame itself — after which the server echoes the action to all seats, so it
arrives on the downlink like any other event. Parsing the uplink would only
duplicate that.

## References

- Bit-level meld spec: <http://tenhou.net/img/mentsu136.txt>
- Tile-image table: <http://tenhou.net/img/tehai.js>
- mjai event types: `src/schema/mjai/mod.rs`
- Sister bridge for protobuf platforms: `src/bridge/majsoul/`
