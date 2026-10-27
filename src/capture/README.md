# capture

Where the WebSocket frames a bridge parses come from.

There is one source: the game the desktop shell shows in a Chromium view of its
own. The core watches that page over the Chrome DevTools Protocol — the same
connection autoplay sends its input through — so capture needs no proxy, no
certificate and no second browser.

```text
shell: game view ── DevTools port (127.0.0.1) ──┐
                                                ▼
supervisor::start ─▶ EmbeddedGame::run ─▶ cdp::run ─▶ FlowBridges ─▶ Bridge ─▶ MjaiBus
        ▲                                   │
        │                                   └─▶ AutoplayContext.page  (the handle autoplay clicks with)
  headless `open_game {port, target}`
```

| File | What it holds |
| --- | --- |
| `supervisor.rs` | `start` / `stop_and_wait`: one capture task at a time, its stop handle in `AppState.capture_control`, every change of status on `CaptureStatusBus`. |
| `embedded.rs` | `EmbeddedGame` (the endpoint and the page to watch), `discover_endpoint` (finds the shell's DevTools endpoint and checks it is local), `game_url` (the only addresses the view may be sent to). |
| `cdp.rs` | The loop itself: attach to the page, subscribe to its `Network` events, hand WebSocket frames to the page's bridge, publish the page handle for autoplay. |
| `flow.rs` | `FlowBridges`: a bridge per WebSocket, created on first frame and dropped when the socket closes. A parser's request/response matching is per connection. |

## Things that are deliberate

- **One page, by target id.** The shell's DevTools port serves its own
  interface, and any sign-in popup, as well as the game. `EmbeddedPage.target`
  names the game's page, and `cdp::run` attaches to that one only.
- **The view starts blank and is sent to the game by the core**, after the
  listeners are attached. Opened the other way round, the first socket and
  `start_game` could go by before anything was listening.
- **Certificates are checked.** chromiumoxide's default configuration tells
  every page it attaches to ignore certificate errors; `cdp::run` connects with
  that turned off. This is the page the player is signed in on.
- **The page handle belongs to the tab, not to a socket.** Mahjong Soul opens
  and closes many short-lived sockets while a game runs on another; the handle
  autoplay uses is cleared only when the page itself goes, or capture stops.
- **`game_url` allows only the selected platform's sites over HTTPS.**
  Mahjong Soul profiles accept its own domains; Tenhou profiles accept
  `tenhou.net`. Credentials in URLs and cross-platform destinations are refused.
  The configuration cannot point a signed-in view somewhere else.

## Status

`CaptureStatus` is `stopped`, `running { descriptor }` or
`error { descriptor?, error }`; the descriptor is the local address capture
reaches the view at. `start` says `running` as soon as the task is spawned, so
the interface sees the change at once; a connection that then fails arrives as
`error`, with a notice.
