# Washizu desktop

The Electron application: a shared account chooser and independent account
clients, each with its own game view, dock and local DevTools port.

## Account profiles

Opening Washizu shows saved accounts. Create a Mahjong Soul or Tenhou profile,
or rename and open one already saved. Multiple different profiles can run
simultaneously. Opening a starting or active profile again focuses its existing
client. The first **Open** shows and focuses the account as soon as its dock
loads; no second **Show** click is needed. First opening, repeated Open and
second-instance requests use the same restore/show/focus path.
Settings → App holds the account chooser button and the application's
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
uses the `/3/` Web client and new Tenhou profiles keep the bot and autoplay
off. Ordinary four-player miai service qualification is complete; actual
Web3 automated games and consecutive-game flow still need online acceptance.
Separate login, capture and records do not imply that online automation has
been accepted. The original account retains its configured miai.

`main.js` chooses the launcher or account process. `profile-main.js` owns the
chooser and starts an account with an internal `--profile=<id>` argument;
`game-main.js` owns that account's existing shell. Each account fixes its
Chromium userData before Electron is ready. App packaging, titles and product
metadata say Washizu; the internal `akagi-core` and IPC names keep their
existing identities. Spout sender names follow the account's stable identity
as described below. Upstream notices stay in
the distribution. Deployment scripts and installed executables are unchanged
by source development.

The chooser is another view of the same React/TypeScript frontend and uses
its existing fonts, palette, buttons and four-language text catalog. The chooser
is a small, non-resizable guide that stays open when clients
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

## OBS output and the offscreen game

The game always renders offscreen at 1280 × 720 CSS pixels, scale 2:
2560 × 1440 GPU textures at up to 60 fps, independent of the account window's
size and the desktop's scaling. The dock displays those textures through
`relay.js` and sends mouse/keyboard input back in the game's coordinates.
Turning output off does not change rendering, capture, input or autoplay.

Settings provide one **OBS 输出** switch. On Windows it starts/stops the
Spout2 sender; there is no HTTP browser source, MJPEG stream, output URL,
feed/board/pages server or alternative window-rendering mode.

In OBS on Windows, install the Spout2 plugin and add a **Spout2 Capture**
source. For the original account choose **Washizu** instead of the old
**Akagi** sender. Other profiles use `Washizu <platform> <stable-id>`; the
stable id keeps the sender unchanged when an account is renamed and avoids
collisions between account windows. The game alone is shared: the dock,
autoplay pointer trail and system cursor are not part of the texture.

The protocol is `set_obs_output({ enabled: boolean }) -> Options`.
The output fields are `obs_output`, `obs_available`, `obs_name` and
`obs_state` (`off`, `starting`, `sending`, `failed` or `stopped`).
Availability requires Windows and the packaged Spout sender. An unavailable
sender is reported explicitly; enabling it is refused.

Each account stores only `{ "enabled": boolean }` in `obs.json`. The old
`view.json` Spout flag is imported once; its rendering mode is discarded.
After that only `obs.json` is read. A lost GPU process is reported and the
same offscreen target reloaded; the shell never silently switches to an
embedded window or rewrites the output preference.

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
npm test        # geometry, content policy, GPU relay, OBS IPC/lifecycle and the core protocol
npm run smoke   # the shell for real — Electron, the built interface, a stand-in core — looked at over its DevTools port
```

`npm run smoke` needs a display (`xvfb-run -a npm run smoke` without one) and
runs on Linux and macOS. It is the check that sees whether the page still
draws when served the way the shell serves it: a content policy one directive
short breaks nothing loudly, the tiles just turn into text.
The OBS contract probe uses synthetic Electron bindings under plain Node;
it exercises the real account module without starting a GUI, core or model.
On Windows, `npm run smoke:window` checks the real profile launcher and native
window visibility on the first Open. It briefly opens a temporary test window,
uses no account or model, and verifies cleanup. Mocked `show()` calls cannot
detect Windows startup flags suppressing the first native window presentation.

## Packaging

```sh
bash ../scripts/fetch-runtime.sh   # python + uv for this machine -> runtime/
npm run package                    # -> dist/desktop/Washizu-<platform>-<arch>/
```

`scripts/package.mjs` puts Electron, the shell, the core with its Python
runtime, and the interface into one folder that runs from wherever it is put.
The `Desktop` workflow does this on Linux, macOS and Windows and uploads each
as an artifact. Nothing is signed:

- **macOS** quarantines an unsigned application that was downloaded. Clear it
  once after unpacking: `xattr -dr com.apple.quarantine Washizu.app`.
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
