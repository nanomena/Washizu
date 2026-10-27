# config

Loads `config.toml` into typed structs, and saves changes back without
disturbing what it does not own.

## Where the file is

One place: `<home>/config.toml`, where the home is the folder the shell names
when it starts the core (`akagi-core --home <dir>`). `load_config(path)` writes
the defaults there if there is no file, then reads it. A file that cannot be
written, read or parsed leaves the defaults in force and says so on stderr.

Every relative directory the configuration names — `bot.dir`, `logging.dir` —
lives under the same home (`util::resolve_dir`).

## Saving (`merge.rs`)

`AppState::save_config` does **not** serialize `AppConfig` over the file. A
`config.toml` can hold keys this build never declared — another build's, or the
user's own notes — and every section struct is `#[serde(default)]`, so loading
passes over them in silence. Serializing back over the file would then delete
them for good, along with every comment.

`merge_into(&config, &existing)` parses the file with `toml_edit`, writes only
the keys this build produces, and leaves everything else (unknown sections,
unknown keys inside known sections, comments, blank lines, key order) exactly
where it is. It is idempotent: saving twice produces identical bytes.

One thing to keep in step: `OPAQUE_TABLES` in `merge.rs` lists the dotted paths
of map-like tables (`HashMap`/`BTreeMap` fields, currently
`autoplay.delay.lognormal`). Those are replaced wholesale instead of merged,
because merging a map by key would make a deleted entry immortal — nothing in
the new document names it, so nothing overwrites it. **Add the path of any new
map-like config field there**, or users won't be able to delete entries from
it.

## Sections

- `logging` (`logging.rs`) — log root dir, console default level (overridden by
  `RUST_LOG`), `all_level` severity filter for `all.log` (`EnvFilter` syntax).
- `platform` (`platform.rs`) — which `Bridge` parses the captured traffic.
- `bot` (`bot.rs`) — whether a bot runs, the active bot per player count, the
  `mjai_bot/` root, and whether to run `uv sync` before spawning. Nests
  `[bot.api]` (`NativeApiConfig`): cloud inference for the built-in bot.
- `capture` (`capture.rs`) — `url`, the address the game view is sent to (it
  must belong to the account's selected platform over HTTPS), and `[capture.http]`
  (`HttpCaptureConfig`): what to record of the HTTP traffic capture sees, on
  top of the WebSocket frames. `record_all` defaults to **off**: full capture
  puts tokens and cookies into the session's files.
- `autoplay` (`autoplay.rs`) — whether a bot's decisions are played, and how:
  click timings for Mahjong Soul and the delay model.
- `monitor` (`monitor.rs`) — pointer display and autoplay between-hand options. OBS output belongs to the desktop shell and is stored in obs.json.

## Adding a section

1. Create `src/config/foo.rs` with a struct that derives `Serialize`,
   `Deserialize` and `Default` (or implements it) and is `#[serde(default)]`,
   so a file that names nothing of it still loads.
2. Register it in `src/config/mod.rs` (`mod foo; pub use foo::FooConfig;`) and
   add the field to `AppConfig`.
3. If it has a map-like field, add its path to `OPAQUE_TABLES`.
