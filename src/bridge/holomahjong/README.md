# HoloMahjong bridge

ホロじゃん / HoloMahjong (Aiming). 4- and 3-player, observe-only. The game is
a native Windows client, so its traffic is read through the core's local
capture proxy rather than a browser page.

## Transport

Gameplay runs on a per-game battle server reached by IP on a high port. The
client speaks gRPC to it over **cleartext HTTP/2** (h2c with prior knowledge):
a single MagicOnion StreamingHub call, `/IMahjongHub/Connect`, carries every
message of the game in both directions. The server address changes from game to
game; the lobby hands it out just before the match.

The game has no proxy setting of its own; an external redirector sends its TCP
to the account's capture proxy (`[capture] listen`) as `CONNECT`s. The proxy
relays each tunnel and, on a battle connection, hands this bridge one complete
gRPC message body per call (after the 5-byte length prefix, uncompressed),
with one bridge per battle tunnel.

## Messages

`hub.rs` splits the envelope: `[method, body]` is a server broadcast or a
client fire-and-forget call, `[message_id, method, body]` a request and its
response. `method` is MagicOnion's id for the hub method — FNV-1a 32 over its
name as a signed int — which `hub::method_id` computes at compile time.

Bodies are MessagePack-CSharp objects serialized as arrays indexed by their
`[Key(n)]`. Event DTOs start at key 1 (index 0 is always nil); nested structs
start at key 0. Each handler in `mod.rs` documents the layout it reads.

| Server broadcast | mjai |
|---|---|
| `OnKaiju` (game start) | — (seat, names, player count kept) |
| `OnQipai` (deal) | `start_game` (first deal of a game), `start_kyoku` |
| `OnZimo` | `tsumo` |
| `OnDapai` | `reach` (riichi discard) + `dahai` |
| `OnFulou` | `chi` / `pon` / `daiminkan` |
| `OnAnGangAddGang` | `ankan` / `kakan` |
| `OnAddBaopai` | `dora` |
| `OnBabei` (3-player) | `kita` |
| `OnLizhi` | `reach_accepted` |
| `OnHule` | `hora` per winner + `end_kyoku` |
| `OnPingju` | `ryukyoku` + `end_kyoku` |
| `OnJieju` (result) | `end_game` with final scores and ranks |
| `OnUpDateEndGameVote` (accepted) | `end_game` (terminated) |
| `OnSystemCallEvent` (TerminateBattle) | `end_game` (terminated) |
| `OnZhunbeiChonglian` (reconnect) | — (seat, names, player count kept) |
| `OnChongxinLianjieChenggong` (reconnect) | `start_game` + `restore_kyoku` or `interrupted_kyoku` — see below |

Client calls and responses are recorded for the Inspector but drive nothing.

A kan's new dora (`OnAddBaopai`) is forwarded where the server sends it.

A win's `hora` carries the win tile and, when the result's hand string is the
closed hand without it, that hand. The wire has one combined payment for all
winners, so each `hora` of a multiple ron carries it whole, as Mahjong Soul's
do; history counts an exact repeat once. The yaku are named by display string,
not by a provider id, so no hand score is attached. A draw's `ryukyoku`
carries the hands it showed.

## Notation

Tiles, hands and melds use majiang-core's notation (see `tile.rs`): `m`/`p`/`s`/`z`
plus a digit, `0` for the red five; a discard may carry `_` (tsumogiri) and `*`
(riichi) or `@` (open riichi), and a kita tile the same `_`; in a meld the
called tile's digit is followed by `+` (next seat), `=` (opposite) or `-`
(previous seat), e.g. `p1-23`, `z444+`, `m5505=` (open kan), `m555=5` (added
kan), `m5550` (closed kan).

## Seats

Wire seats are fixed for the game, but nothing assumes how they are numbered
(contiguous, or a 4-seat table with a gap at 3-player). mjai actor 0 is the
first dealer and actors follow turn order, and so do winds, so every deal maps
a seat to `(its wind + hand number) mod players`. `PingjuEvent` reports
players by **wind**, which the same formula resolves.

The target of a chi, pon or open kan is the seat that made the discard being
called. The meld string's direction mark (`+ = -`) is only a fallback for a
call whose discard we did not see, and it counts seats around a **4-seat**
table even at three players, where seat 3 is empty: seat 2 calling seat 0 —
its next player in turn order — is marked `=`. Reading the mark in turn order
would point those calls at the wrong player.

## Reconnects

A reconnect opens a new battle connection, so a new bridge. It gets the game
start again (`OnZhunbeiChonglian`: seat and names) and then, mid-hand, a
snapshot (`OnChongxinLianjieChenggong`): the field, and per seat the score,
the hand (ours in full, the others' as `x1` per tile) with its calls after
commas, and the river.

`restore.rs` maps that onto `restore_kyoku` (`schema::restore::KyokuRestore`,
the contract the Tenhou bridge uses for `REINIT`) as it stands: current
scores and sticks, every dora indicator, our concealed hand (a held draw
included), and per seat the river, the calls with their sources and called
tiles, and where the riichi declaration stands. Nothing of the history is
made up: no draws, no call or kan timing, no order across seats.

Each river entry is the discard as sent — `_` tsumogiri, `*`/`@` riichi — and
a called discard stays in place with the caller's direction mark appended. The
restore's river leaves called tiles out, so the marked entries are matched one
for one against the callers' melds instead. The snapshot is rejected when it
does not say something consistently: a call without its called discard or
the other way round, a riichi flag without its declaration in the river, a
tile count off, dora indicators that do not fit the kans, a seat that
disagrees with the game start, or a position the restore rules refuse
(`game_state::restore::build`). A three-player table is always rejected:
`KyokuRestore` has no form for kita or the three-player wall.

A rejected snapshot goes out as `interrupted_kyoku` with the reason, and the
bridge follows play again from the next deal. A game this connection joined
mid-way is not a complete record, so its result goes out as a terminated
`end_game`, as the Tenhou bridge does for a rejoin.

## Limitations

- Compressed gRPC messages are not decoded (none observed).
- No autoplay.
- Three-player reconnects are not restored.

## Shareable frames

`is_shareable` allowlists the gameplay broadcasts and the client's game calls.
`JoinRoomAsync` (account and battle-session ids), `OnKaiju`,
`OnZhunbeiChonglian` and the reconnect snapshot (every player's account id
and name) and `OnJieju` (result-screen account data) are read but stay off
the list: the flow log records their method, direction and size only, and
the full message stays under `sensitive/`. `OnQipai` has an account slot per
player that has only been seen nil; it is shared only while every slot is
nil.

## Adding a message

1. Add `CONST = "MethodName"` to the `methods!` list in `hub.rs`.
2. Add a handler in `mod.rs` reading the body by key index, and a match arm in
   `dispatch`.
3. Decide whether it is shareable.
4. Add a fixture-based test in `tests.rs` — made-up names and ids only.
