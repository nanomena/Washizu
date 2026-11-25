# capture

Where the frames a bridge parses come from.

There are two sources, one per kind of client. `supervisor::Source` names the
one capture runs from; only one runs at a time.

- **`Embedded`** — a web game (Mahjong Soul, Tenhou) the desktop shell shows
  in a Chromium view of its own. The core watches that page over the Chrome
  DevTools Protocol — the same connection autoplay sends its input through —
  so capture needs no proxy, no certificate and no second browser.
- **`Proxy`** — a native client (HoloMahjong) whose TCP an external
  redirector sends to a local HTTP `CONNECT` proxy on the account's loopback
  `[capture] listen` address. The proxy relays every tunnel and taps the
  cleartext HTTP/2 one that carries the game's gRPC call. It is observe-only:
  nothing is played on this platform.

```text
headless `open_game`  ─▶ supervisor::start(Source::Embedded) ─▶ cdp::run
headless `open_proxy` ─▶ supervisor::start(Source::Proxy)    ─▶ proxy

shell game view ── DevTools port (127.0.0.1) ──────────────▶ cdp::run ──┐
native client ── redirector ── 127.0.0.1:<listen> ─▶ proxy ─▶ h2c tap ──┤
                                                                        ▼
                     FlowBridges ─▶ frame::deliver ─▶ Bridge ─▶ MjaiBus, inspector

cdp::run ─▶ AutoplayContext.page  (embedded only: the handle autoplay clicks with)
```

| File | What it holds |
| --- | --- |
| `supervisor.rs` | `Source`, `start` / `stop_and_wait`: one capture task at a time, its stop handle in `AppState.capture_control`, every change of status on `CaptureStatusBus`. `start` opens the source first, so a proxy port it cannot bind fails `start` itself. |
| `embedded.rs` | `EmbeddedGame` (the endpoint and the page to watch), `discover_endpoint` (finds the shell's DevTools endpoint and checks it is local), `game_url` (the only addresses the view may be sent to). |
| `cdp.rs` | The embedded loop: attach to the page, subscribe to its `Network` events, hand WebSocket frames to the page's bridge, publish the page handle for autoplay. |
| `proxy/mod.rs` | The local proxy: `listen_addr` (the only addresses it may listen on), `ProxyCapture` (bound listener and its accept loop), the `CONNECT` handling and the tunnel relay. |
| `proxy/h2c.rs` | `H2Tap`, a passive HTTP/2 frame parser fed both directions that yields complete gRPC messages, and `relay`, which forwards every chunk before tapping it. |
| `flow.rs` | `FlowBridges`: a bridge per connection, created on first frame and dropped when the connection closes. A parser's request/response matching is per connection. |
| `frame.rs` | `deliver`: one frame through its bridge, onto the inspector timeline (content only if the bridge marks it shareable) and its events onto `MjaiBus`. Both sources use it. |

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

### The local proxy

- **Loopback only.** `listen_addr` accepts a loopback address with a fixed
  port and nothing else: the proxy relays to anywhere, so another machine must
  never reach it. A port another program holds is an error of `open_proxy`,
  an `error` status and a notice, never a quiet move to another port.
- **Only `CONNECT`.** Each connection must open with `CONNECT host:port
  HTTP/1.x`; anything else is answered `405` (another method) or `400` and
  closed. The target is dialled before the `200`, so a target that cannot be
  reached is a `502` (`504` past 10 s).
- **Bytes first, then the tap.** A tunnel whose first client bytes are the
  HTTP/2 client preface — read until they can no longer be a split preface,
  the server speaks first, or 3 s pass — goes through `h2c::relay`. Every chunk
  is forwarded before it is parsed, each side is half-closed at its peer's
  EOF, and a tap that loses track stops decoding that direction, logs why and
  keeps relaying. A tunnel that does not open so, TLS included, is relayed
  untouched with an `akagi_bypass` note on the timeline and no content.
  Nothing is decrypted, so the game needs no certificate.
- **Only the hub call reaches the bridge.** Each complete, uncompressed gRPC
  message on `/IMahjongHub/Connect` goes to that tunnel's bridge (one per
  tunnel) as the bytes after the 5-byte length prefix, `Up` client→server and
  `Down` server→client. A compressed message is skipped with a log line.
  Messages of other calls are not recorded.
- **Header values are never logged:** they carry the bearer token. Request
  `:path`s are.
- **Stopping closes everything.** Stopping capture (or the core's shutdown)
  drops the listener and aborts every connection task, whatever it is doing:
  waiting for a request, dialling, peeking or relaying.

### Loopback CONNECT is refused

A redirector rule that matches the game executable for *any* target also
matches the game's own loopback traffic, including the `bind` + `listen` +
self-connect that emulates `socketpair()` on Windows; the proxy then receives
`CONNECT 127.0.0.1:<ephemeral>`. Proxying that cannot work — such a
self-connect checks that the address it accepted is its own peer's, which a
proxied hop breaks — and the game hangs. So a `CONNECT` whose host is a
loopback or unspecified address, `localhost` or a `.localhost` name, or a name
that resolves only to this machine, is answered `403` at once. The first one
also raises a sticky warning notice (id `proxy-loopback-connect`) naming the
fix: exclude loopback from the redirector's rule. Later ones are logged only.

## Status

`CaptureStatus` is `stopped`, `running { descriptor }` or
`error { descriptor?, error }`. The descriptor is the local address capture
reaches the view at, or `proxy 127.0.0.1:<port>` for the proxy. `start` says
`running` as soon as the task is spawned, so the interface sees the change at
once; a connection that then fails arrives as `error`, with a notice.

A bridge retains the original per-frame input window in `ParseResult.window`.
`into_captured` attaches it only to that frame's final complete event prefix,
then `MjaiBus` carries `CapturedEvent` to the tracker and other consumers.
Capture never re-reads a newer shared slot to assign an older event's identity.
History and inspector consumers record the underlying MJAI event as before.
