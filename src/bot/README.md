# Bot Module

Runs mjai-protocol AI bots and routes `MjaiEvent`s from the platform
bridge to them. 

## Unavailable decisions

A reply with `meta.unavailable` set to a reason string means the model cannot
decide, for example after synchronization loss or a transport failure. The
manager records it for inspection/monitoring but never publishes it to the
autoplay response bus, including incomplete riichi replies. The marker is
part of the reply, so gating does not race the independent stderr notification
pump. The bot must mark every unavailable reply until it can decide again;
notifications still describe the fault to the user.

An ordinary `none` continues to mean a considered decline when asked at a
decision window; a `none` with `meta.show` is a displayed model PASS. Neither
is suppressed by this contract. Hand-boundary `none` remains withheld as before.
The monitor retains unresolved sync faults across new deals/reconnects; an
actionable model reply or displayed PASS in a valid hand confirms recovery.

## Submodules

- `types` — `BotResponse`. Wire shape matches mjai.app: an mjai action
  with an optional free-form `meta` JSON object for HUD-grade data
  (confidence, reasoning, q-values, …). The core reads of `meta` only what
  [What a bot says about itself](#what-a-bot-says-about-itself) names; the
  monitor shows the rest as written.
- `manifest` — per-bot `manifest.toml` schema + `settings.toml` values.
  See "Per-bot settings" below.
- `registry` — discovers bot directories under `mjai_bot/`. No Python
  invocation; only filesystem layout. Populates `BotEntry.manifest` from
  any `manifest.toml` found in the bot directory.
- `runtime` — `PythonRuntime`: locates bundled `python-build-standalone`
  + `uv` (or falls back to system `python3`/`uv`); runs `uv sync` on
  demand against a bot's `pyproject.toml`; produces a `tokio::process::Command`
  ready to spawn the bot. Sync is idempotent via a `mtime:size` stamp at
  `<bot>/.akagi/synced.stamp`. See "Bundled runtime" below for how the
  bundled binaries are fetched and shipped.
- `sync_guard` — `SyncGuard`: per-bot mutual exclusion for `uv sync`.
  Both `BotManager::spawn_runner` (game-start sync) and `service::run` (a
  bot's service, which may share the environment) acquire-or-bail on the
  bot name through a shared `Arc<Mutex<HashSet<String>>>` so two parallel
  syncs against the same venv can't trample each other.
- `runner` — `BotRunner` async trait + `SubprocessBot` impl. Talks JSONL
  over stdin/stdout, pumps stderr into `tracing` (`bot=<name>` field),
  enforces a 60 s default react timeout, and `kill_on_drop(true)` so a
  dropped runner can't leak children. `reset()` writes `[{"end_game"}]`,
  waits 500 ms, then SIGKILL's and respawns. The stderr pump doubles as a
  bot→user notification channel: a line prefixed with `NOTIFY_PREFIX`
  (`@@AKAGI_NOTIFY@@ `) followed by a JSON `Notification` is parsed and
  forwarded onto the `NotifyBus` (→ the monitor's notices); every other line is logged as before, and a malformed payload
  after the prefix is dropped with a `warn!`.
- `manager` — `BotManager`: subscribes to the **post-tracker bus**,
  accumulates events between decision points, flushes the pending batch
  through the `BotRunner`, and broadcasts every `BotResponse` (including
  `MjaiEvent::None`) on the `BotResponseBus`. A decision point is an event
  whose *shape* could open a decision for our seat (own tsumo / others'
  dahai-or-kakan / own chi-or-pon) **and** which the riichi engine says
  our seat can actually act on — see "Deciding what to ask" below. Round
  and game boundaries (hora / ryukyoku / end_kyoku / end_game) flush
  regardless: they open no decision, but they are how the bot hears the
  hand ended, and `end_game` is where the runner is torn down.
  Spawn point is `start_game` carrying the bot's seat in the `id` field
  and the table's `num_players`. The manager picks `active_4p` or
  `active_3p` from `BotConfig` based on `num_players`; an empty slot for
  the matching mode means analysis-only for that game (no runner spawned).
- `native` — the built-in, in-process bot (no Python, no subprocess).
  Two reserved names select it: `akagi-native` (4p) / `akagi-native3p`
  (3p). `NativeBot` always loads the embedded pure-Rust `native_bot` candle
  model, and re-reads `BotConfig.api` from the shared `AppConfig` at **every
  decision**: when `is_active()`, the decision is proxied to the remote
  inference API (see `api`) with the local model kept as a legal-action gate
  (skip the API when we can't act) and an error/timeout fallback. Re-reading
  per decision is what lets the user enable cloud inference, fix a mistyped
  key, or switch models mid-game — a change resets the `Breaker` so the new
  settings are tried on the very next move. `Breaker` is an exponential-backoff
  circuit breaker (5s → 120s): after a failed call the API is skipped for the
  window, so a dead server costs one slow turn per window instead of a request
  timeout on every decision. `native::build(actor_id, num_players, config,
  notify_tx)` seeds the session silently; `BotManager::spawn_runner` calls it
  for the reserved names, bypassing the registry / venv path entirely.
- `api` — `ApiClient`: a `reqwest` wrapper over the remote inference server's
  `/v3/react`. Auth is a bearer header. `react` carries its own short
  `REACT_TIMEOUT` because it blocks the bot's turn. Building an `ApiClient`
  builds a fresh connection pool, so hold one and reuse it. Consumed by
  `NativeBot` while cloud inference is on (`[bot.api]`).
- `service` — `service::run`: the resident process a bot may declare in its
  manifest (`[service]`), run for as long as the app is open. See
  [A bot's service](#a-bots-service).
- `supervisor` — `run_bot_manager`: constructs the `BotManager` and drives
  its run loop off the `PostTrackerBus`. Tolerates a missing Python runtime
  — the built-in `native` bots need none.
- `test_http` (test-only) — a tiny scripted HTTP mock shared by the `api`
  and `native` tests, so they can assert on the raw request
  (path, `Authorization` header, body) and script the response.

## Deciding what to ask

A bot is fed every event and answers every event, so its replies cannot say
which ones it was *asked*: an mjai `none` is the same three bytes for "I
weighed this call and decline it" and "this was never mine to answer". The
distinction matters to anything that acts on replies — autoplay pressed a
pass button for a filler `none` once, and the real `hora` behind it arrived
to find its decision window already spent.

The riichi engine knows the difference, so `BotManager` asks it rather than
the bot. `GameTracker` computes `can_act` for our seat the instant it applies
an event and ships it with that event on the `PostTrackerBus`
(`event_bus::TrackedEvent`); the manager gates the flush on it.

Computing it *there* is the point. One server frame can carry several seats'
actions, and the tracker digests all of them in microseconds while the
manager is still waiting on inference for the first — so a manager that
looked the answer up on arrival would be asking about a state several events
old. Riding along with the event is what makes the answer belong to it.

`can_act` is `None` when the engine has no opinion — no game in progress, no
seat tagged (observer / replay). That is not "cannot act": the manager falls
back to the event shape alone, so a stream the tracker cannot follow costs
wasted round-trips rather than a silent bot.

The rule the engine applies is the same one `native::is_decision_point` uses
on its own candidate set: a legal set that is empty, or that holds nothing
but `Pass`, is not a decision (riichienv hands a `Pass` to every seat while
it is in its response phase, including the seats with nothing to claim).

## The `meta.show` card (built-in bot)

The built-in bot attaches a `meta.show` card — the ranked candidates with their
policy probabilities — to its `BotResponse`. The monitor shows it as the
candidates of a decision. One rule governs it, and everything in
`native.rs` that touches `meta` exists to keep it true:

> **The card changes exactly when the bot chose something, and never otherwise.**

Both halves matter, and getting either wrong is invisible in tests but obvious in
a game.

*Never otherwise*: most events reaching the bot are not decisions — an opponent's
discard we cannot call, a draw that isn't ours. Those reply `MjaiEvent::None` with
`meta: None`, and the monitor leaves the card up. If they carried a card, it
would flicker through the whole hand.

*Exactly when it chose*: **declining a call is a choice**, and it must refresh the
card. `is_decision_point` is the gate — a legal set that is empty, or that contains
nothing but `Pass`, is not a decision. Anything else is, including a call window
where the bot passes, and there the pass is ranked as a row of its own against the
pon/chi/kan it turned down ("Pass 87% / Pon 13%"). That comparison is the most
useful thing on screen at that moment.

This was got wrong once (#190): the local path suppressed the card whenever the
top candidate was a pass, so declining a call left the *previous turn's* discard
advice on screen, reading as live advice for a decision that was already over. The
cloud-inference path had it right. Both now share the `PASS_LABEL` row, so the card
reads the same whichever one answered.

## Adding a new bot

Drop a folder under `mjai_bot/`:

```
mjai_bot/<name>/
├── bot.py            # JSONL stdin → JSONL stdout
├── pyproject.toml    # uv-resolved deps; requires-python = ">=3.12"
├── manifest.toml     # OPTIONAL — settings schema (see below)
├── settings.toml     # OPTIONAL — current values; gitignored, written by Akagi
└── README.md         # bot-specific notes (model paths, license, etc.)
```

`bot.py` MUST:

- read one JSON array per line from stdin (a batch of `MjaiEvent`s)
- write one JSON object per line to stdout (a single mjai action — use
  `{"type":"none"}` when no action is owed)
- print **only** protocol JSON to stdout; logs go to stderr
- exit cleanly when it sees `{"type":"end_game"}` in a batch

A bot MAY also surface a toast in the UI by writing an
`@@AKAGI_NOTIFY@@ {…}` line to stderr (level / title / body — see
[`mjai_bot/README.md`](../../mjai_bot/README.md)).

The bot's seat is delivered three ways (pick whichever fits):

- **argv:** `python bot.py <player_id>` — matches mjai.app convention,
  so unmodified mjai.app bots run as-is.
- **env:** `AKAGI_PLAYER_ID=<player_id>`.
- **`start_game.id`:** the field on the first `start_game` event.

What the game is played for is in **`AKAGI_GAME`**, when the platform's
bridge says: a JSON object with the `platform`, `num_players`, the room as
the platform numbers it (`mode_id`) and our rank on that room's ladder as the
game began (`level`: `id` and `score`, or `null`). A bot that plays to a
room's terms, such as a ranked room's payout for our rank, reads it there.
Only Mahjong Soul games are described so far:

```json
{"platform": "majsoul", "num_players": 4, "mode_id": 12, "level": {"id": 10403, "score": 3576}}
```

It names nothing that identifies the game, its record or a player, and it is
not part of the mjai stream: logs and cloud inference never see it.

## Per-bot settings

A bot can ship a `manifest.toml` declaring its configurable knobs (API
URLs, keys, model selection, …). Their values live in `settings.toml`
beside it, written by the bot's installer or by hand: Akagi has no form
for them.

Example `manifest.toml`:

```toml
manifest_version = 1

[bot]
name             = "mortal"
display          = "Mortal"
description      = "AGPL deep-RL mahjong bot."
version          = "0.5.0"
supported_modes  = ["4p"]   # ["3p"], ["4p"], or ["4p", "3p"]

[settings.api_url]
type    = "string"
label   = "API Server URL"
default = "https://api.example.com"
help    = "Endpoint for online inference. Leave blank to run offline."

[settings.api_key]
type    = "string"
label   = "API Key"
secret  = true
default = ""

[settings.online]
type    = "bool"
label   = "Online Mode"
default = false

[settings.temperature]
type    = "float"
label   = "Sampling Temperature"
default = 1.0
min     = 0.0
max     = 2.0
step    = 0.05

[settings.model]
type    = "enum"
label   = "Model"
default = "mortal.pth"
choices = ["mortal.pth", "mortal-1.5.pth"]
```

Field types: `string`, `bool`, `int`, `float`, `enum` (one of `choices`).
`secret = true` makes Akagi substitute the value with `***` in tracing.

`supported_modes` declares which game modes this bot can play. Accepted
values: `"4p"` (yonma) and `"3p"` (sanma). Defaults to `["4p"]` when the
field is absent — pre-3p manifests stay 4p-only without edits.

Bots see the resolved settings (defaults ⊕ on-disk values, validated
against the manifest) via the env var `AKAGI_BOT_CONFIG`, which points
at a JSON file like:

```json
{
  "api_url": "https://api.example.com",
  "online": true
}
```

The bot script can `json.load(open(os.environ["AKAGI_BOT_CONFIG"]))`.
Bots that don't read the env var simply ignore it.

### Secrets caveat

`secret = true` only changes *log substitution* — the value is still
stored in `settings.toml`. We
`.gitignore` `settings.toml` so it doesn't end up in source control.
OS keychain integration is on the roadmap; until then, treat
`settings.toml` as a credential file.

### When changes take effect

Updating settings does **not** restart the running bot subprocess. The
new values take effect on the next `start_game` event.

## A bot's service

`bot.py` lives for one game. A bot that needs something to outlive its games
— a model server that takes seconds to load, or that follows a whole game —
declares it:

```toml
[service]
entry = "service/service.py"
```

```
mjai_bot/<name>/
├── bot.py
└── service/
    ├── service.py        # the entry
    └── pyproject.toml    # its own environment, not the bot's
```

While the bot is the active one (four-player first, else three-player),
Akagi installs the environment the entry's directory declares, runs the entry
there with `AKAGI_BOT_CONFIG` pointing at the bot's resolved settings, and
starts it again when it exits: after 2 s, doubling up to 60 s for each start
that never became ready. The entry's side of the contract:

- **stdout** — one JSON line `{"type":"ready"}` once it can serve. A `label`
  string and a `facts` list on it are shown to the user as they are (see
  [What a bot says about itself](#what-a-bot-says-about-itself)).
- **stderr** — its log; every line is copied into Akagi's with `service=<bot>`.
- **stdin** — never written. Akagi holds it open; the entry exits when it
  closes. That is how the service is stopped, and why it cannot outlive the
  app however the app ends. On Windows a read left pending on the pipe stalls
  other threads of the interpreter, so poll it (`PeekNamedPipe`) instead.

The state — none, installing, starting, ready, failed — is `server` on the
monitor snapshot, separate from `service`, which is whether the bot has
reached its server.

## What a bot says about itself

The monitor window knows the game and the mjai actions; it knows nothing about
any bot. What a bot wants the user to see about itself, it says in its own
words, and the window shows the words as written:

- **`meta.facts`** on a reply — `[{"name": "...", "value": "...", "note": "..."}]`,
  `note` optional. Shown in the window's list of facts, the note on demand.
  The latest reply that carries the key replaces what stood before; a reply
  without it changes nothing. A service's `ready` line may carry `facts` too,
  for what does not change while it runs.
- **`label`** on a service's `ready` line — the name the service goes by.
- **`caption`** and **`hint`** on the `meta.show` card — what the rows' values
  are, in a word or two, and a sentence about them. Without a caption the
  values stand unexplained.
- **`position`** on the `meta.show` card — where this decision falls in the
  game by the bot's own count, as a short string in its own words
  (`"step 187"`; ten characters at most). The window numbers the decision by
  it. Without one, the decision goes by its place among the game's decisions,
  `#1` onwards, which starts again with every game.
- The window is named after the active bot's `display` in its manifest.

Card rows are read by their `label`: `Discard`, `Riichi`, `Chi`, `Pon`, `Kan`,
`Ankan`, `Kakan`, `Hora`, `Ron`, `Tsumo`, `Ryukyoku`, `Kita` and `Pass` are
matched against the action the bot chose and the action the game then took;
any other label is shown as text.

## Bundled runtime

A packaged build ships its own python and uv — no system install required.
The two binaries live beside the core, under
`<core's folder>/runtime/{python,uv}/<target-triple>/`, and are picked up by
`PythonRuntime::locate(None)` in `RuntimeMode::Bundled`.
On a dev box without bundled binaries, `locate` falls through to
`RuntimeMode::System` and uses whatever's on PATH.

### Layout

```
runtime/
├── python/<triple>/        # python-build-standalone tree (3.12.x)
│   ├── bin/python3         (linux/mac)
│   ├── lib/...
│   └── python.exe          (windows)
└── uv/<triple>/
    ├── uv                  (linux/mac)
    ├── uv.exe              (windows)
    └── uvx[.exe]
```

`<triple>` is the cargo target triple (e.g. `x86_64-unknown-linux-gnu`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`).

### Fetching locally

```sh
scripts/fetch-runtime.sh                          # host triple
scripts/fetch-runtime.sh x86_64-pc-windows-msvc   # cross-target
scripts/fetch-runtime.sh --force                  # re-download
```

Override the versions via env vars: `PYTHON_VERSION`, `PBS_RELEASE`,
`UV_VERSION`. The script is idempotent — re-runs are no-ops once the
target binaries exist.

### Bundling

`desktop/scripts/package.mjs` copies the tree beside the core in the
packaged application. The `runtime/` directory is `.gitignore`'d (`runtime/*`
with a `!runtime/.gitkeep` exception). CI runs the script once per target
before packaging.

## Why subprocess

- AGPL bots (Mortal et al.) link via stdin/stdout pipe only — arms-length,
  not derivative work. See plan §10.
- Crash isolation: a bot SIGSEGV does not take Akagi down.
- Hot model swap = kill + spawn, no in-process state to flush.

In-process embedding (PyO3) was considered and rejected: linking
libpython makes single-binary distribution painful on Windows/macOS and
couples Akagi's lifecycle to libriichi's.

## Per-mode active bot (4p / 3p)

`BotConfig` stores two slots: `active_4p` and `active_3p`. `BotManager`
picks the slot matching `start_game.num_players` (3 → `active_3p`, else
`active_4p`). Empty slot ⇒ no runner spawned for that game.

The slots are set in `config.toml` (`[bot] active_4p`, `active_3p`), by hand
or by a bot's installer. A bot whose Python environment is not installed yet
gets it installed at its first spawn (`uv sync`), which the react time limit
has room for only if the environment is small; a bot with a service has it
installed when the application starts instead.

Pre-3p config files with a single `[bot] active = "..."` key are
migrated on load: the legacy value is moved to `active_4p` once via
`BotConfig::migrate_legacy_active`, and the on-disk file is rewritten
without the legacy field on the next persist.

## What's NOT here

- Mortal weights. Users place `mortal.pth` inside their `mjai_bot/mortal/`
  folder; Akagi never ships, fetches, or configures weight paths. The
  bot script loads them itself.
- Anything drawn. `BotResponse`s land on the broadcast bus and in the
  monitor snapshot; the interface (`frontend/`) draws them.

## Positioned primary recovery

A bot declaring `[bot] protocol = "positioned_mjai"` uses the single recovery
coordinator when the runtime monitor and bridge clock are available. It must
supply `meta.delivery.through` on every stdout answer. The coordinator owns one
confirmed journal, independent primary/local workers and one source selection
per real input window. Plain `mjai` remains the default active protocol.
See [recovery design](RECOVERY.md), [shared control contract](CONTROL.md), and
[offline qualification](RECOVERY-TESTS.md). Matching relay/server and core changes
must be installed together; source changes alone are not a deployment.

Direct Native/API and plain MJAI responses now carry the same core-owned
original request as positioned responses. Direct `delivered` is separate from
positioned acknowledged `through`; unavailable direct responses never report
ready. Incomplete executable riichi is withheld, without speculative feeding
or suppressing its later real echo.
