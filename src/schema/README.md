# Schema Module

Shared types used across the project. Anything that crosses module boundaries
— protocol events, what the core's parts report about themselves, persisted records — lives
here so it isn't owned by a single subsystem.

## Existing schemas

- `mjai/` — `MjaiEvent` enum covering all 15 mjai event variants from the
  mjai protocol spec **plus** the 3p-only `Kita` variant
  (北抜き / nukidora). Used by
  `bridge::Bridge` to expose parsed game events, and by anything
  downstream that consumes them (bots, loggers, the monitor). Tiles
  are kept as `String`. JSON serialization uses `#[serde(tag = "type")]`,
  so `"type"` is always the first key. Player-shaped fields (`names`,
  `scores`, `tehais`, `deltas`) are `Vec<T>` of native length; `StartGame`
  and `StartKyoku` carry `num_players: u8` (serde default `4` for
  backward-compat with pre-3p log lines).

`interrupted_kyoku { reason }` is an explicit observation gap on the internal
event bus, also visible in diagnostics. It is not a synthetic `start_kyoku`
and is never sent to a bot's MJAI transport. The tracker discards incomplete
rules state; the coordinator can select only the restricted actions proved by
subsequent original provider windows. Positioned Primary waits for the next
real deal and a fresh lifecycle. Unpositioned runners stop until that boundary.
The recorder drops incomplete history instead of counting it as a full game.
`ActionContext.snapshot` is absent during this interval: consumers must not
invent a native rules snapshot for pacing or targets.

`hora` also carries optional public `pai`, `tehai` and `scores`. `pai` is the
winning tile, retaining red-five identity. `tehai` contains only the revealed
closed tiles, excluding `pai`; melds are already public. `scores` contains
authoritative post-settlement point totals, one per player. Multi-ron events
share the same score vector. Absent provider data stays absent. The interface
mirrors these fields in `frontend/src/types.ts`.

`ryukyoku.tehais` optionally carries one closed-hand list or `null` per absolute
seat. `null` explicitly means that seat's hand was not disclosed. Its optional
`scores` are post-payment totals; `reason` names `exhaustive`, `nagashi`,
`kyuushu`, `four_winds`, `four_kans`, `four_riichis` or `three_rons` when confirmed.
Draw hands retain red identity and contain no extra winning tile.

## Adding a new schema

1. Create `src/schema/<name>/mod.rs` (or `src/schema/<name>.rs`) with your
   types. Derive `Serialize`/`Deserialize` so the type is usable on both
   sides of any boundary it might cross.
2. Register the module in `src/schema/mod.rs` and re-export the main type.
3. If the type reaches the interface — in the monitor snapshot — mirror it in
   `frontend/src/types.ts`; nothing generates that file.

## Conventions

- Keep schemas decoupled from runtime/library types (e.g. don't use a
  third-party `Tile` struct here — use `String`). The schema module should
  not pull in heavy deps.
- For tagged enums that go to JSON, prefer `#[serde(tag = "type", rename_all = "snake_case")]`
  so the discriminant is a stable, predictable string.
- Prefer `serde_with::skip_serializing_none` over hand-written
  `skip_serializing_if` attributes when an enum has many `Option` fields.
