# Washizu desktop

The Electron application: a shared account chooser and independent account
clients, each with its own game view, dock and local DevTools port.

## Account profiles

Opening Washizu shows saved accounts. Create a Mahjong Soul or Tenhou profile,
or rename and open one already saved. Multiple different profiles can run
simultaneously. Opening a starting or active profile again focuses its existing
client. Settings → App holds the account chooser button and the application's
About/license details. The game window has no application menu bar. Showing
the chooser during a game does not stop that client. Closing the chooser while
clients run minimizes it; Quit is prevented until those clients have closed.
If the chooser process is lost, its clients continue independently.

Closing an account waits for its core to stop and acknowledge managed-service
cleanup. The account process then acknowledges release to the chooser. Only
that profile stays occupied until both the acknowledgement and the process's
close arrive. An abnormal exit or missing cleanup acknowledgement locks only
that profile and displays the failure. Other profiles remain usable. A proven
failure to start can be retried. With no live clients, the chooser can exit
even when a failed profile remains locked.
The chooser itself starts no core, model, game page or network connection.

`--data` names the account library root. Its `profiles.json` stores stable ids,
display names, platforms and the last opening time. Fresh profiles live under
`profiles/<id>/`, each with its own configuration, `browser/`, history, logs,
window state and bot settings. Renaming a profile never changes its directory.
`ProfileSessions` owns a map keyed by profile id, with explicit `starting`,
`active`, `closing` and `failed` states. The chooser's list/event protocol is
`{revision, profiles, clients, error}`; runtime client states are separate from
the unchanged persistent registry schema.

An existing home is adopted **in place** as the first profile. Its browser,
Local State, cookies, bot settings and history are never moved, copied or
rewritten by the chooser. Its game still uses `<root>/browser` and
`persist:game`. The chooser uses `<root>/launcher/browser`, apart from it.
Packaged launches discover the sibling `../data` home or the former
`<appData>/akagi-desktop`; `--data` explicitly selects a home. Development
defaults to `<appData>/Washizu-development` and never discovers production data.
Do not use production data for development or tests.

New profiles do not copy another account's bot settings or credentials.
Mahjong Soul starts with the core's built-in bot and autoplay off. Tenhou
starts with the bot and autoplay off: this change provides its separate
login, capture and records, while miai rule/reward/settlement qualification
remains separate work. The original account retains its configured miai.

`main.js` chooses the launcher or account process. `profile-main.js` owns the
chooser and starts an account with an internal `--profile=<id>` argument;
`game-main.js` owns that account's existing shell. Each account fixes its
Chromium userData before Electron is ready. App packaging, titles and product
metadata say Washizu; the internal `akagi-core`, IPC names and the external
Spout sender `Akagi` keep their existing identities. Upstream notices stay in
the distribution. Deployment scripts and installed executables are unchanged
by source development.

The chooser is another view of the same React/TypeScript frontend and uses
its existing fonts, palette, buttons and four-language text catalog. The chooser
is a small, non-resizable, always-on-top guide that stays open when clients
start and shows each profile's runtime status and failure. It has no game
canvas or page zoom: body text remains 13 DIP and headings use Golos Text 500
with the same system CJK fallback as the monitor.

`profile-layout.js` derives its content size: 640 wide = 48 outer padding +
352 name + 88 platform + 152 actions. Its vertical Grid is 48 outer padding +
48 toolbar + 24 status + (32 header + 2..4 × 64 account rows) + 24 footer,
giving 304/368/432 DIP heights. More rows scroll inside the list. Account
state and language changes keep the same slots. Small displays cap its size
to leave room for the native frame. The main window independently retains
`layout.js`'s 4:3 ratio, 1200×900 opening basis, 880×660 minimum, Windows frame
correction and uniformly scaled 1280×960 game/dock canvas.

Offline verification:

```sh
cd desktop
npm run check
npm test
xvfb-run -a npm run smoke:profiles
xvfb-run -a npm run smoke
```

Both smoke tests use a fake core and temporary data. The profile smoke opens
multiple real Electron account clients with game pages kept at `about:blank`,
checks independent persistent cookies on an offline `.invalid` origin,
chooser close/Quit and mid-game account-button behavior, and the cleanup-ACK
plus process-close boundary. It crashes one fixture client to verify that
only its profile locks while other clients continue or open independently.
Four-language compact layouts are checked without visiting a game site.
On bare Xvfb, which has no window manager, the fixture observes the actual
native minimize request rather than claiming OS iconification was verified.
The real-process service shutdown regressions and commands are documented in
[`src/bot/SERVICE-LIFETIME.md`](../src/bot/SERVICE-LIFETIME.md).

Capture and autoplay both go through the Chrome DevTools Protocol, and of the
webviews an operating system provides only Windows' is a Chromium. This shell
brings its own Chromium (Electron), so there is one capture path and one input
path on every platform.

## The three parts

| Part | Where | What it does |
| --- | --- | --- |
| Shell | `desktop/` | The window, the two views in it, and the local DevTools port. Nothing about mahjong. |
| Core | `src/headless`, built as `akagi-core` | Everything else: capture, the bridge, the tracker, the bot, autoplay, the monitor snapshot. No window, no webview. |
| Interface | `frontend/`, the monitor page | Draws the dock from the core's snapshot. |

The account shell starts the core as a child process and talks to it in JSON lines over
the child's stdin and stdout; `src/headless/mod.rs` documents the protocol. The
core lives exactly as long as that stdin is open, so there is nothing to clean
up when the shell goes, however it goes.

The interface reaches the shell through two functions the preload script
exposes, `invoke(command, args)` and `listen(event, callback)`
(`frontend/src/lib/host.ts`). The shell answers the commands about the window
itself and passes the rest to the core.

The game is a second view in the same window, in a browser session of its own
(`persist:game`): its cookies and storage are kept between runs and are not
shared with the interface. It has no preload script and no way to reach the
shell. When the page opens it, the shell tells the core the DevTools port and
the view's target id; the core attaches to that one page and sends it to the
game.

Input sent over DevTools shows no cursor, so autoplay's pointer can be drawn
over the game when the settings ask for it (`pointer_trail`). The core then
sends where the pointer goes (`pointer` events) and the shell draws them in a
transparent window of its own over the game view (`src/trail.*`), which takes
no input: clicks pass through it to the game. Nothing is added to the game's
page.

## Streaming the game

With `stream_output` on, the shell offers the game view to a streaming program
on the same computer — the game alone: not the dock, not the pointer's trail,
not the system cursor (`src/stream.js`):

```text
http://127.0.0.1:<port>/<token>/              a page that shows the stream filling its window, for a browser source
http://127.0.0.1:<port>/<token>/stream.mjpg   the frames themselves, as Motion JPEG
```

The token is a random secret: without it every address is a 404, and a request
under any host name but `127.0.0.1` or `localhost` is refused. The port and the
token are kept in the configuration, so the address stays the same from run to
run; the settings tab shows it and copies it.

Frames are made only while something watches (`src/frames.js`): screenshots of
the game view, up to 30 a second, of exactly what it shows — its own size in
screen pixels. Nothing about the game's page is changed to take them: not its
scale, not its size, so the window looks the same and every position autoplay
clicks at stays where it is. A stream wants 1920 × 1080; a game view with fewer
pixels than that is scaled up by the page that shows it. (Telling the page it
is on a denser screen would draw more pixels, but it changes the page while it
is played: its device pixel ratio, and after a move to a screen of another
scale even its layout.)

In OBS, add a **Browser** source with the address, 1920 × 1080. The page picks
the stream up again by itself when the application restarts. A Media source
can open `stream.mjpg` too, but Motion JPEG carries no timing, and ffmpeg
plays it at 25 frames a second whatever comes in.

### The game drawn offscreen

The settings tab can have the game drawn apart from the window, from the next
start (`game_view = offscreen`, kept in the data folder's `view.json`). The
game's page is then always 1280 × 720 CSS pixels drawn at twice that, 2560 ×
1440 — the size Mahjong Soul draws its own canvas at — whatever the window's
size, screen or scale. Chromium hands the shell each frame as a texture on the
GPU, and the shell lends it on (`src/relay.js`):

- to the dock, which shows it on its stage as a video (`preload.cjs`) and sends
  the user's mouse, wheel and keyboard back in the page's pixels
  (`src/input.js`); text and pastes go in as text, so input methods work;
- on Windows, with Spout output on, to a Spout sender (`native/spout`): in OBS,
  a *Spout2 Capture* source (the Spout2 plugin) with the sender `Akagi`, on a
  2560 × 1440 canvas scaled down to the output.

No frame passes through the CPU on the way. The pointer's trail lies over the
dock's picture, so it is never in what the streaming program gets. Autoplay's
input still goes over DevTools, to the same page. If the GPU process goes, a
page drawn offscreen draws nothing more until it is loaded again: the shell
reloads it, and after a second time within ten minutes the window view is used
from the next start. The Motion JPEG stream still works offscreen, but its
screenshots hold up the page's frames; with Spout there is no need for it.

### Pages of your own beside the game

For overlays and control pages around the game, the same server says what the
table is doing and serves a folder of your own pages (`src/feed.js`,
`src/pages.js`):

```text
http://127.0.0.1:<port>/<token>/feed        server-sent events: `state` (the table, on every change) and `results` (the games on record)
http://127.0.0.1:<port>/<token>/feed.json   the same two, once: {"state": …, "results": […]}
http://127.0.0.1:<port>/<token>/board       server-sent events: `board` (every value, at once) and `set` ({name, value}, each value put)
http://127.0.0.1:<port>/<token>/board.json  the board, once
PUT  …/<token>/board/<name>                 put a JSON value under a name, from a page of this origin
http://127.0.0.1:<port>/<token>/pages/…     the files in <data>/stream-pages/, a folder's index.html for the folder
```

The feed is what anyone watching the table could see, and nothing about the
bot: `state` has the names, our seat, the round (wind, hand, honba, sticks,
dealer), each seat's score and riichi, the dora indicators, how each hand of
this game ended with the hands shown when it did, and the player's own ranks
(`account`: the four- and three-player `{id, score}` as Mahjong Soul last said
them, at login, a game's start or after a game, and `at`, when) and when this
run of the app began (`started_at`); `results` has
the games on record — scores, placements, rank points, our rank as the game
began (`our_level`), per-game counts and yaku — without the path of their logs
or the ids that lead to a replay, and never who the player is. No decision, no
candidate, nothing a bot said of itself, and not whether autoplay is on. Every
field is named in `src/feed.js`; one the core adds later stays out until it is
named there. `state.version` goes up when a field changes meaning or goes away.

The pages folder is yours: put a page there and add it to the streaming
program as a browser source (an overlay) or as a custom browser dock (a
control page). Its pages may load their own files and open a WebSocket to a
program on this computer (`ws://127.0.0.1:*`, OBS's remote control), and
nothing else. Names with `..` or a leading dot are refused; a link inside the
folder, such as a junction to a music library, is followed. Media files answer
range requests, so audio and video can seek. A page under `pages/` finds the
feed at the top of its own address: `/<token>/feed`.

The board (`src/board.js`) is how those pages talk to each other: a control
page says what an overlay should show, a music page says what it is playing.
They share an origin, but a streaming program may run them in browsers that
share no storage (OBS keeps its docks apart from its sources), so they meet
here. A name is lower-case letters, digits and dashes; a value is JSON, 64 KB
at most, 32 names at most. The board is kept in memory while the application
runs: a page that owns a value puts it again when it connects. Only a page of
this origin may write (a browser names the page a write comes from); the
secret in the address keeps everything else out.

## Running it from a checkout

```sh
cargo build --release --bin akagi-core                         # the core
(cd frontend && npm ci && npm run build)                        # the interface -> frontend/dist
cd desktop && npm ci
node node_modules/electron/install.js                           # fetch Electron itself, once
npm start
```

`npm start` finds the core in `target/release` and the interface in
`frontend/dist`. To point it elsewhere:

| Option | Environment | Default |
| --- | --- | --- |
| `--data=<dir>` | `AKAGI_DATA` | discovered existing home, otherwise `<appData>/Washizu` (development: `Washizu-development`) |
| `--core=<file>` | `AKAGI_CORE` | the packaged core, or `target/release/akagi-core` |
| `--dock=<dir>` | `AKAGI_DOCK` | the packaged interface, or `frontend/dist` |
| `--cdp-port=<n>` | `AKAGI_CDP_PORT` | any free port |

## Where things are kept

Everything is under the data folder, and nothing is written beside the
executables:

```text
<data>/config.toml    the configuration (created with defaults on first run)
<data>/mjai_bot/      installed bots
<data>/logs/          one folder per run
<data>/history/       finished games
<data>/replays/       replays opened in the game, as tenhou.net/6 logs and NAGA lines
<data>/browser/       the Chromium profile, the game's session included
<data>/window.json    where the window was left
```

The adopted first profile uses these paths directly. A new profile has this
same layout under `<data>/profiles/<id>/`; the chooser's registry and browser
are kept outside each account home.

## Checks

```sh
npm run check   # type-check the JavaScript (JSDoc types, no build step)
npm test        # the window's geometry, the content policy, the stream's server, frames, feed, board and pages, and the shell's end of the protocol against a stand-in core
npm run smoke   # the shell for real — Electron, the built interface, a stand-in core — looked at over its DevTools port
```

`npm run smoke` needs a display (`xvfb-run -a npm run smoke` without one) and
runs on Linux and macOS. It is the check that sees whether the page still
draws when served the way the shell serves it: a content policy one directive
short breaks nothing loudly, the tiles just turn into text.

## Packaging

```sh
bash ../scripts/fetch-runtime.sh   # python + uv for this machine -> runtime/
npm run package                    # -> dist/desktop/Akagi-<platform>-<arch>/
```

`scripts/package.mjs` puts Electron, the shell, the core with its Python
runtime, and the interface into one folder that runs from wherever it is put.
The `Desktop` workflow does this on Linux, macOS and Windows and uploads each
as an artifact. Nothing is signed:

- **macOS** quarantines an unsigned application that was downloaded. Clear it
  once after unpacking: `xattr -dr com.apple.quarantine Akagi.app`.
- **Linux** distributions that restrict unprivileged user namespaces
  (Ubuntu 24.04 and later) refuse Chromium's sandbox from an unpacked folder.
  Either give the helper its permissions —
  `sudo chown root chrome-sandbox && sudo chmod 4755 chrome-sandbox` — or start
  the application with `--no-sandbox`.

## Things to know

- **The DevTools port is local and unauthenticated.** It is bound to
  `127.0.0.1`, on a port chosen at start, and it is how the core sees and
  drives the game. Any program running as the same user could connect to it.
- **The game view is never put to sleep.** It keeps drawing and taking input
  while the window is covered or minimized, because autoplay has to keep
  working then.
- **The ES-module entry point must not `await app.whenReady()` at its top
  level.** Electron waits for the entry module to finish before it becomes
  ready, so that wait never ends. `main.js` awaits only what has to happen
  before `ready`, and continues from `whenReady().then(…)`.
- **On Windows the 4:3 lock is approximate while an edge is being dragged** and
  exact when the drag ends: Electron's aspect-ratio lock counts the title bar
  there, so the shell asks for the ratio that suits the current width and
  corrects the size at the end.
