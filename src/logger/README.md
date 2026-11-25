# Logger Module

Structured text logging built on [`tracing`](https://crates.io/crates/tracing) and [`tracing-appender`](https://crates.io/crates/tracing-appender).

## Files

- `mod.rs` — `init(log_root, default_level, all_level, targets)` entry point.
- `session.rs` — `Session` (the active log session) and `LogTarget`.
- `flow.rs` — `FlowLogger` for one text log file per logical "flow" (e.g. one Majsoul WS connection).

## Session layout

`logger::init` creates a session directory:

```
<log_root>/<YYYYMMDD-HHMMSS>/
├── all.log              # every event from every target
├── bot.log              # one file per LogTarget (filtered by tracing target prefix)
├── inspector.jsonl      # shareable timeline: frames, mjai events, bot reactions
├── majsoul/             # one subdir per platform (FlowLogger)
│   ├── 000001-gateway.log                # one file per WS flow: <id:06>-<uri-slug>.log
│   ├── 000002-game-gateway.log
│   └── majsoul_<ts>.mjai.jsonl          # one file per game: emitted MjaiEvents
└── sensitive/           # may hold account credentials (SENSITIVE_DIR)
    ├── README.txt       # says so to whoever opens the folder
    └── inspector.jsonl  # the full timeline, every frame and HTTP exchange
```

## Shareable vs. sensitive

Everything outside `sensitive/` is meant to be shareable. Frame content gets
there only for frames on the bridge's allowlist (see "Shareable frames" in
`src/bridge/README.md`); any other frame is written as method, direction and
size. Two rules keep it that way when you add logging:

- Don't log a frame payload, a URL query or an HTTP header value unless the
  bridge marked the frame shareable. Use `bridge::shown(shareable, &payload)`
  for payloads and `util::redact_query` for URLs.
- Record inspector entries with `InspectorWriter::record_as(entry, shareable)`
  (or `record_sensitive`). `record` is only for content known to be safe; an
  HTTP exchange recorded with it still loses its credential headers, and all
  its content unless a recognizer annotated it.

Anything that reads frame content back from a session (scripts, the
`replay_majsoul` example) reads `sensitive/inspector.jsonl`, falling back to
`inspector.jsonl` for sessions recorded before the split.

`log_root` is `logging.dir`, under the home the shell gives the core (`util::resolve_dir`).

## Text log format

Each line carries: timestamp, level, tracing target, source `file:line`, message. ANSI colour is stripped in files. Console (stderr) keeps colour.

File outputs (`all.log`, per-target `*.log`) use a custom `CompactNoSpans` formatter that **omits the parent-span list**: third-party crates wrap handlers in nested `#[instrument]` spans whose rendering would otherwise bury the message.

The console layer uses `EnvFilter` honouring `RUST_LOG`; if unset, falls back to `default_level` (from `[logging] level`). The combined `all.log` is severity-filtered by `all_level` (from `[logging] all_level`, same `EnvFilter` syntax — e.g. `"info"` or `"akagi=debug,hyper=warn"`) so you can suppress trace/debug noise. Per-target files always capture every event so you can grep historic runs without re-running.

## Adding a new target file

In `headless::run`, append a `LogTarget` to the slice passed to `logger::init`:

```rust
&[
    logger::LogTarget::new("bot", "akagi::bot"),
    logger::LogTarget::new("ai",    "akagi::ai"),
]
```

`prefix` is matched against each event's tracing target (longest-prefix). Module path is the default target, so any `tracing::info!` inside `src/ai/` lands in `ai.log`.

## Per-flow text logging

```rust
let flow = session.flow_logger(
    "majsoul",
    "000001-gateway.log",
    "majsoul 127.0.0.1:54170 wss://.../gateway",
)?;
flow.writeln(&serde_json::to_string(&parsed)?);
```

Each call to `Session::flow_logger` opens a fresh file at
`<session>/<subdir>/<file_name>` (subdir auto-created; caller supplies the
full filename including extension, so the same logger handles `.log`,
`.mjai.jsonl`, etc.). Capture opens one per WebSocket
and hands the resulting `Arc<FlowLogger>` to
`bridge::for_platform`. Every parsed message gets written as a JSON line,
its payload withheld unless the frame is shareable.
When all references drop (both directions of the WS exit), the file
closes.

`MajsoulBridge` opens an additional `majsoul_<ts>.mjai.jsonl` file every
time it emits a `start_game` event — one file per game on the same flow,
each line a serialized `MjaiEvent`.

## Lifetime

`Session` owns the `tracing-appender` `WorkerGuard`s. Drop it only when the core stops — dropping flushes + closes the file appenders. `headless::run` keeps the `Arc<Session>` alive for the life of the process.
