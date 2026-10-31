# Decision engine protocol (`washizu-engine/1`)

A decision engine is a separate program that chooses actions. Washizu owns
everything around the choice: the accounts and their windows, observing the
table, the positioned event stream, the decision-source coordinator and its
local standby, pacing, the pointer and every input. The engine owns the
choice: its model, every inference parameter, which objective a table is
played for, and the words the monitor shows about it. Washizu never stores or
forwards an inference parameter, and an engine never sees an input.

This is the same split as a Go GUI and its analysis engine, or a chess GUI and
its UCI engine: the client picks which engine, the engine's own configuration
says how it plays.

## Packages

An engine is installed as a folder under the application data directory:

```
<data>/engines/
├── engines.toml          # default = "<name>" — what new accounts use
└── <name>/
    ├── engine.toml       # protocol, display name, entry
    ├── <entry>           # the program Washizu runs
    ├── pyproject.toml    # its environment, pinned by uv.lock
    ├── uv.lock
    ├── endpoint.json     # written by the running engine
    └── host-state.json   # written by the engine host
```

`engine.toml`:

```toml
protocol = "washizu-engine/1"
display  = "Example engine"
entry    = "engine.py"
```

Washizu runs **one** instance of an engine for the whole application,
however many accounts are open, from the first account that uses it until the
application closes: the launcher starts `akagi-core engine-host --engine
<data>/engines/<name>` and holds its stdin. The host installs the folder's
environment with the bundled Python and `uv` (`uv sync` into
`<package>/.akagi/venv`, skipped while `pyproject.toml` and `uv.lock` are
unchanged), then runs the entry with that environment's interpreter, in the
package folder, with:

- `WASHIZU_ENGINE_DIR` — the package folder;
- `WASHIZU_ENGINE_ENDPOINT` — the file the engine publishes its endpoint to.

The entry's side of the process contract:

- **stdout** — one JSON line `{"type":"ready", "label": "...", "facts": [...]}`
  once it serves. `label` and `facts` are shown as written.
- **stderr** — its log, copied into Washizu's.
- **stdin** — never written; Washizu holds it open and the engine exits when
  it closes, so it never outlives the application.

A process that exits is started again after 2 s, doubling up to 60 s while
starts never become ready. When the launcher closes the host's stdin, the host
closes the engine's, waits up to 3 s for it to exit, terminates it otherwise,
reaps it, and exits.

### Host state

The host publishes where the engine stands in `host-state.json`, replaced in
one step on every change and again every 2 s:

```json
{"pid": 4242, "updated_ms": 1793400000000,
 "status": {"state": "ready", "bot": "<name>", "label": "...", "facts": [], "since_ms": 0, "startup_ms": 19000}}
```

`state` is `installing`, `starting`, `ready` or `failed` (with `error` and
`retry_at_ms`). Each account's core reads it into its monitor snapshot's
`server`; a file older than 10 s, or none, shows as `failed`: the host is not
running. The host removes the file on exit if it is still its own.

### Endpoint

When it listens, the engine writes `WASHIZU_ENGINE_ENDPOINT` atomically:

```json
{"protocol": "washizu-engine/1", "host": "127.0.0.1", "port": 50123, "token": "<random>"}
```

`token` is random per process. Every greeting carries it, and the engine
answers with it, so a client can never reach a stale or foreign listener on a
reused port. The engine removes the file on exit only if it still holds its
own token.

### Accounts

An account's `config.toml` names the engine in the existing slots,
`[bot] active_4p` / `active_3p`. A name that is an installed engine package
selects this protocol; any other name is a plain mjai bot under `mjai_bot/`
or a built-in bot, as before. A new account takes `engines.toml`'s `default`.

## Wire

TCP on loopback. UTF-8 JSON, one object per line. On one connection every
request is answered by exactly one line, in order.

A game seat holds one **decision connection** for the whole game. Forecasts,
when wanted, use their own connections and never delay a decision.

### `hello` → `ready`

```json
{"type": "hello", "protocol": "washizu-engine/1", "token": "...",
 "account": "<opaque account id>", "seat": 2, "lean": true,
 "game": {"platform": "tenhou", "num_players": 4, "...": "public table facts"}}
```

- `account` is Washizu's stable id of the account. It means nothing to
  Washizu beyond identity; an engine may key its own per-account policy by it.
- `game` is what Washizu observed of the table before the game: the platform
  and its public room, length and rank facts, exactly as observed. It never
  carries a setting.
- `lean` asks the engine to offer forecast snapshots on this session.

There are no inference settings on the wire. The engine decides them from its
own configuration, the table and the account.

The engine answers:

```json
{"type": "ready", "token": "...",
 "notify": [{"level": "info", "title": "...", "body": "...", "id": "..."}]}
```

`token` must be the endpoint's; any other answer is never trusted. What the
engine says about itself while it runs belongs on the process's own `ready`
line; what it says about this session belongs in `notify`.

or refuses with `{"type": "none", "meta": {"unavailable": "<reason>"}, "notify": [...]}`.
A refused greeting leaves the connection open for a corrected one and never
keeps a previous game.

### `events` → reaction

```json
{"type": "events", "start": 41, "events": [{"type": "tsumo", "actor": 2, "pai": "5p"}]}
```

`start` is the position of the first event in the game's real event stream,
counted from `start_game` at 0. A batch extends the committed prefix exactly;
an identical repeat of a committed batch returns its original reply; anything
else is an error.

Washizu waits 20 s for a reply. A reply that comes later keeps the
connection: it is read and dropped (its `notify` shown) before the next
request, so it can never answer a later one. A lost connection is opened
again at the next batch, on a fresh session that receives the whole real
stream from 0.

The reply is one mjai reaction (`{"type": "none"}` when nothing is owed) with:

- `receipt: {"start": S, "end": E, "through": T}` — the committed prefix.
  An executable reaction commits the whole batch (`T == E`).
- `meta.unavailable` — when the engine could not decide; never executable.
- `meta.show`, `meta.facts` — the monitor card and facts, as for mjai bots.
  The card's `items[].prob` and `type` say whether a runner-up is worth a
  look; its `title` is replaced with the package's `display`.
- `meta.pace` — optional, in `[0, 1]`: where this decision falls among the
  engine's own decisions by difficulty. Pacing reads it; it never changes the
  action.
- `meta.forecast` — optional snapshot reference `{session, through, position}`
  for a forecast connection.
- `notify` — optional toasts.

### `forecast`

On its own connection:

```json
{"type": "forecast", "token": "...", "session": "...", "through": 41, "position": 3}
```

answered by `{"type": "forecast", "session", "through", "position",
"status": "ready" | "obsolete" | "unavailable", "lean"?: {...}, "reason"?: "..."}`:
exactly the request's identity, `lean` only when ready, `reason` only when
unavailable, and no other field. A forecast is display only: it can place the
pointer, never act.

## Verification

`cargo test --lib engine` runs the runner, forecast and host tests against
loopback fakes. The process fixtures need a real interpreter: set
`AKAGI_TEST_PYTHON` to an absolute Python (and `PYTHONDONTWRITEBYTECODE=1`
for a shared one), then

```sh
cargo test --lib engine::host::lifecycle_tests -- --ignored --nocapture --test-threads=1
cargo test --test engine_host -- --nocapture
```

They cover a ready, a loading and an EOF-ignoring engine (shutdown waits
until the process is reaped and its port can be bound again), a cancelled
installation (no engine, no success stamp), a stop during a retry (no
replacement), and what the engine is told. `node desktop/tests/engine-host-teardown.mjs
--core=<built core> --python=<python> --uv=<uv>` runs the same through the
built core and the launcher's own host owner.
