# `src/history/` — Game History Recorder

This module persists every cleanly-ended mjai game to a stable
store. The on-disk shape (under `<home>/history/`) is:

```
history/
  index.jsonl              # one JSON-encoded GameRecord per line, append-only
  games/
    <ulid>.mjai.jsonl      # full event stream for that record
```

`<ulid>` is a [ULID](https://github.com/ulid/spec) generated at finalisation
time — lexicographically sortable by start time, doubles as the filename stem.

## Pipeline

```
                    ┌──────────────┐
   MjaiBus  ──────▶ │  recorder::  │ ──▶ HistoryStore.append() ──▶ index.jsonl
                    │  drive_loop  │                                games/<id>.mjai.jsonl
                    └──────────────┘
                            │
                            ▼
                       HistoryBus -> monitor.history_revision -> statistics refresh
```

A `RecorderState` machine buffers events between `StartGame` and a confirmed
`EndGame`. On a Mahjong Soul reconnect to the same table it retains completed
rounds and replaces the server-restored copy of the current round. A different
next game, explicit server termination, or shutdown drops the incomplete
buffer. A confirmed final standing can survive an earlier capture gap, so a
finished game is not automatically a complete detailed-statistics sample.

The headless service subscribes before the recorder starts. A successful append
publishes a HistoryBus record and advances `monitor.history_revision`; a lagged
consumer likewise invalidates its cached list. The existing monitor stream
delivers that revision to both clients' statistics tab. It reads committed
history immediately, independently of tracker EndGame timing or native account
reports. Rank/balance display continues to use only the platform's own report.
There is no timer-based refresh or guessed post-game balance.

## Modules

- **`aggregator.rs`** — Pure function `aggregate(events, ...) -> Option<GameRecord>`.
  Based on the `Stat::from_game` aggregator from Mortal's `libriichi`. It walks
  `Reach`/`ReachAccepted`/`Hora`/`Ryukyoku` deltas for statistics, prefers a
  platform's authoritative final standings when supplied, and otherwise
  normalises totals to 100k (4p) / 105k (3p). Stat counts mirror `Stat`'s field
  set, scoped to one game.
- **`store.rs`** — `HistoryStore` (JSONL append-only index + per-game event
  files). Single mutex serialises writes; reads are lock-free (re-open).
- **`recorder.rs`** — `drive_loop(store, history_bus, platform, mjai_rx)`. The
  long-running task spawned in `lib.rs::run`.

## Platform and coverage contracts

Each account has its own history store and recorder. `SharedPlatform` follows
the selected bridge. Room identity and yaku ids retain their native platform;
Tenhou id 0 is tsumo, not a missing id, and its ids are not Majsoul's ids.

`stats_version: 1` counts declaration and tsumo turns from one, counts a
multi-ron deal-in once per hand, and applies each distinct payment vector once.
Repeated combined Majsoul payments cannot safely be split between winners;
their loss amount is stored in `houjuu_point_unattributed`. Yakuman uses explicit
score metadata, never a payment threshold. Abortive draws count without a
payment vector.

`stats_complete` requires closed hands with results, own payment data, and a
known player seat. Detailed figures use only these records. Final placement
and pt remain usable independently. `yaku_wins: Some(n)` is the exact number
of captured own wins with nonempty score metadata; `None` means legacy sample
coverage is unknown. Yaku frequencies use only known samples and show captured
wins versus available wins. Unknown metadata must not be presented as zero.

`Hora.score` is private in-process metadata (`serde(skip)`); the index stores
the aggregated yaku. A public mjai export deliberately does not contain it.

## Repairing existing finalized records

`node scripts/repair-history-stats.mjs --home=ACCOUNT --out=NEW_PROPOSAL.json`
prepares a dry run. It recomputes the corrected counters from each existing
record's own saved history events and matches raw result metadata by native
game id, round, honba and winning seat. Reconnect round identity comes from
the recorded public restore. Contradictory metadata is excluded; legacy yaku
aggregates without exact sample coverage are preserved without a guessed count.
No missing outcomes or new game records are synthesized.

After all application processes have exited normally, apply with
`--apply=PROPOSAL.json --backup=NEW_DIRECTORY`. The Windows process guard,
original-byte SHA256, immutable game-field comparison and byte-identical backup
must pass before atomic replacement. A newly appended game requires a fresh
proposal. This maintenance migration is not part of live capture or inference.

## Adding a new GameStats field

1. Add the field to `crate::schema::history::GameStats` (default-initialised
   to keep deserialisation backward-compatible with old index lines).
2. Update `aggregator.rs` to populate it.
3. Older `index.jsonl` lines will deserialise with the new field defaulting;
   no migration step is required so long as the new field has a `Default`.

## Match identity (`GameRecord.match_info`)

`schema::history::MatchInfo` is a platform-tagged enum carrying the raw room
/ rank-lobby ids and the platform's own game (paifu) id — e.g. Majsoul
`mode_id` + `game_uuid`, Tenhou `<GO type>` bitfield + log id. Bridges attach
it to `StartGame.game_meta` (a `#[serde(skip)]` field, so mjai logs and the
inference API never see it) and the aggregator copies it into the record.

Rules of the road:

- Store **raw platform values**, not names for them: an unknown id then
  reads as a number instead of a wrong name.
- Every field is `Option` + `#[serde(default)]` — bridges fill what their
  wire carried, old index lines deserialise with `match_info: None`.
- To extend: add fields/variants in `schema/history.rs`, fill them in the
  bridge's StartGame emission.

## Adding a filter dimension

1. Add the field to `HistoryFilter` (in `schema/history.rs`).
2. Update `HistoryFilter::matches`.

## Don't

- Don't backfill from `<log_dir>/majsoul/*.mjai.jsonl` — those are bridge debug
  logs and may include incomplete / duplicated streams. The history store is
  populated *only* by the live recorder.
- Don't write into `<log_dir>` from this module — keep the two surfaces
  decoupled.
