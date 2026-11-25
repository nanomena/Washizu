//! The local HTTP `CONNECT` proxy a native client's traffic is redirected to.
//!
//! HoloMahjong has no web client. An external redirector (a per-process proxy
//! rule) sends the game's TCP connections to this listener on the account's
//! loopback `[capture] listen` address. Each connection must open with an
//! HTTP/1.1 `CONNECT host:port`; the proxy dials the target, answers `200`,
//! and relays both directions:
//!
//! - a tunnel that opens with the HTTP/2 client preface goes through the h2c
//!   tap ([`h2c::relay`]): bytes are forwarded first and parsed after, and each
//!   complete gRPC message on [`HUB_PATH`] goes to that tunnel's bridge, the
//!   inspector and the mjai bus like a WebSocket frame;
//! - anything else, TLS included, is relayed untouched and noted on the
//!   inspector timeline without its content. Nothing is decrypted, so the game
//!   needs no certificate.
//!
//! A `CONNECT` to loopback is refused: see "Loopback CONNECT is refused" in
//! the capture README. Stopping capture closes the listener and every tunnel.

pub mod h2c;

use super::flow::{slugify, FlowBridges, SharedBridge};
use super::{frame, CaptureCtx, ShutdownToken};
use crate::bridge::{BridgeHooks, Direction};
use crate::event_bus::{MjaiBus, NotifyBus};
use crate::inspector::InspectorWriter;
use crate::schema::{
    CaptureSource as InspectorSource, FrameRaw, HttpAnnotation, HttpExchange, HttpPhase,
    InspectorEntry, Notification,
};
use anyhow::{Context, Result};
use base64::Engine;
use chrono::Local;
use h2c::TapEvent;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tracing::{debug, info, warn};

/// The one gRPC call that carries every game message.
pub const HUB_PATH: &str = "/IMahjongHub/Connect";

/// Longest `CONNECT` request head accepted.
const HEAD_LIMIT: usize = 8 * 1024;
/// How long a new connection has to send its `CONNECT`.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the target has to accept the proxy's connection.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a tunnel waits for the client's first bytes before relaying
/// untapped (a server that speaks first ends the wait at once).
const PEEK_TIMEOUT: Duration = Duration::from_secs(3);

const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

/// The address an account's local proxy listens on: `[capture] listen`, which
/// must be a loopback address with a fixed port. The proxy relays to anywhere,
/// so it must never be reachable from another machine.
pub fn listen_addr(configured: &str) -> Result<SocketAddr, String> {
    if configured.is_empty() {
        return Err("这个账号没有设置本地代理地址。".into());
    }
    let addr: SocketAddr = configured
        .parse()
        .map_err(|_| format!("本地代理地址无效：{configured}"))?;
    if !addr.ip().is_loopback() {
        return Err("本地代理只能监听本机回环地址，例如 127.0.0.1:<端口>。".into());
    }
    if addr.port() == 0 {
        return Err("本地代理地址需要固定端口。".into());
    }
    Ok(addr)
}

/// The proxy with its port bound, ready to run.
pub struct ProxyCapture {
    listener: TcpListener,
}

impl std::fmt::Debug for ProxyCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyCapture")
            .field("listen", &self.listener.local_addr().ok())
            .finish()
    }
}

impl ProxyCapture {
    /// Bind `listen`. A port another program holds is an error.
    pub async fn bind(listen: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("本地代理无法监听 {listen}"))?;
        info!("local proxy: listening on {listen}");
        Ok(Self { listener })
    }

    /// Relay until `shutdown`; then close the listener and every tunnel.
    pub async fn run(self, ctx: CaptureCtx, shutdown: ShutdownToken) -> Result<()> {
        // Observe-only: the bridge writes the server's clock to the
        // time-budget slot, which bounds inference; nothing else of
        // autoplay reaches it.
        let hooks = BridgeHooks {
            time_budget: ctx.autoplay.as_ref().map(|a| a.time_budget.clone()),
            notify: Some(ctx.notify_bus.clone()),
            account: Some(ctx.account_bus.clone()),
            ..BridgeHooks::default()
        };
        let relay = Arc::new(Relay::new(
            FlowBridges::new(ctx.session.clone(), ctx.platform, hooks),
            ctx.session.inspector(),
            ctx.mjai_bus,
            ctx.notify_bus,
            public_dial(),
        ));
        serve(self.listener, relay, shutdown.wait()).await;
        Ok(())
    }
}

/// Accept connections until `shutdown`, each served as its own task. When it
/// comes, every tunnel task is aborted, which closes both its sockets, and the
/// listener is dropped.
async fn serve(listener: TcpListener, relay: Arc<Relay>, shutdown: impl Future<Output = ()>) {
    tokio::pin!(shutdown);
    let mut tunnels = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    tunnels.spawn(relay.clone().serve_client(stream, peer));
                }
                // Per-connection failures (a reset before accept) are
                // transient; the listener itself stays.
                Err(e) => warn!("local proxy: accept failed: {e}"),
            },
            Some(done) = tunnels.join_next(), if !tunnels.is_empty() => {
                if let Err(e) = done {
                    if e.is_panic() {
                        warn!("local proxy: a tunnel task panicked: {e}");
                    }
                }
            }
        }
    }
    let open = tunnels.len();
    tunnels.shutdown().await;
    info!("local proxy: stopped; closed {open} connection(s)");
}

/// Why a dial was not made or did not connect.
#[derive(Debug)]
enum DialError {
    /// The target resolves only to this machine.
    Local,
    Io(std::io::Error),
}

type DialFuture = Pin<Box<dyn Future<Output = Result<TcpStream, DialError>> + Send>>;
/// Opens the proxy's connection to a `CONNECT` target (`host` without
/// brackets).
type Dial = Arc<dyn Fn(String, u16) -> DialFuture + Send + Sync>;

fn public_dial() -> Dial {
    Arc::new(|host, port| Box::pin(dial_public(host, port)))
}

/// Resolve `host` and connect to the first of its addresses that is not this
/// machine and accepts. A name that resolves only to loopback is refused like
/// a loopback literal.
async fn dial_public(host: String, port: u16) -> Result<TcpStream, DialError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(DialError::Io)?
        .collect();
    if addrs.is_empty() {
        return Err(DialError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{host} has no address"),
        )));
    }
    let mut last = None;
    for addr in addrs.into_iter().filter(|a| !is_local(a.ip())) {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or(DialError::Local, DialError::Io))
}

/// This machine: loopback (IPv4-mapped included) or the unspecified address.
fn is_local(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    ip.is_loopback() || ip.is_unspecified()
}

/// Whether a `CONNECT` host names this machine: a loopback or unspecified
/// literal, `localhost`, or a name under `.localhost`.
fn is_local_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match host.parse::<IpAddr>() {
        Ok(ip) => is_local(ip),
        Err(_) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost")
        }
    }
}

/// A `CONNECT` request's target.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    /// As the client wrote it, for logs and the timeline.
    authority: String,
    /// Without IPv6 brackets.
    host: String,
    port: u16,
}

/// A request the proxy answers with an error and closes.
#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    /// Not a request line at all, or a `CONNECT` without `host:port`.
    BadRequest(String),
    /// Any method but `CONNECT`.
    NotConnect(String),
}

impl Refusal {
    fn response(&self) -> &'static [u8] {
        match self {
            Refusal::BadRequest(_) => {
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
            Refusal::NotConnect(_) => {
                b"HTTP/1.1 405 Method Not Allowed\r\nAllow: CONNECT\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
        }
    }
}

const FORBIDDEN: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const GATEWAY_TIMEOUT: &[u8] =
    b"HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Read a request head, up to its blank line. Returns the head and whatever
/// the client sent after it, or `None` if it closed first.
async fn read_head<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok(Some((buf, rest)));
        }
        if buf.len() > HEAD_LIMIT {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too long",
            ));
        }
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Parse a request head; only `CONNECT host:port HTTP/1.x` is accepted.
/// Header lines are read past and ignored.
fn parse_connect(head: &[u8]) -> Result<Target, Refusal> {
    let line = head
        .split(|&b| b == b'\n')
        .next()
        .and_then(|l| std::str::from_utf8(l).ok())
        .map(|l| l.trim_end_matches('\r'))
        .ok_or_else(|| Refusal::BadRequest("request line is not text".into()))?;
    let parts: Vec<&str> = line.split(' ').collect();
    let [method, target, version] = parts.as_slice() else {
        return Err(Refusal::BadRequest("not a request line".into()));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(Refusal::BadRequest(format!("version {version}")));
    }
    if *method != "CONNECT" {
        return Err(Refusal::NotConnect((*method).to_string()));
    }
    if target.contains('@') {
        return Err(Refusal::BadRequest("credentials in the target".into()));
    }
    let authority: http::uri::Authority = target
        .parse()
        .map_err(|_| Refusal::BadRequest("target is not host:port".into()))?;
    let port = authority
        .port_u16()
        .filter(|&p| p != 0)
        .ok_or_else(|| Refusal::BadRequest("target has no port".into()))?;
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err(Refusal::BadRequest("target has no host".into()));
    }
    Ok(Target {
        authority: (*target).to_string(),
        host: host.to_string(),
        port,
    })
}

/// Whether the client's first tunnel bytes could still become the HTTP/2
/// preface.
fn undecided(first: &[u8]) -> bool {
    first.len() < h2c::PREFACE.len() && h2c::PREFACE.starts_with(first)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Everything the tunnels of one running proxy share.
struct Relay {
    /// One bridge per tapped tunnel, keyed by tunnel number.
    bridges: FlowBridges<u64>,
    inspector: InspectorWriter,
    mjai_bus: MjaiBus,
    notify_bus: NotifyBus,
    dial: Dial,
    /// Set on the first refused loopback `CONNECT`, so the notice goes out
    /// once however many sockets a misconfigured redirector sends.
    loopback_noticed: AtomicBool,
    next_tunnel: AtomicU64,
}

impl Relay {
    fn new(
        bridges: FlowBridges<u64>,
        inspector: InspectorWriter,
        mjai_bus: MjaiBus,
        notify_bus: NotifyBus,
        dial: Dial,
    ) -> Self {
        Self {
            bridges,
            inspector,
            mjai_bus,
            notify_bus,
            dial,
            loopback_noticed: AtomicBool::new(false),
            next_tunnel: AtomicU64::new(1),
        }
    }

    /// One client connection: its `CONNECT`, then the tunnel.
    async fn serve_client(self: Arc<Self>, mut client: TcpStream, peer: SocketAddr) {
        let (head, rest) = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
            Ok(Ok(Some(head))) => head,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                debug!("local proxy: {peer}: unreadable request: {e}");
                let _ = client
                    .write_all(Refusal::BadRequest(String::new()).response())
                    .await;
                return;
            }
            Err(_) => {
                debug!("local proxy: {peer}: no request within {HEAD_TIMEOUT:?}");
                return;
            }
        };
        let target = match parse_connect(&head) {
            Ok(target) => target,
            Err(refusal) => {
                debug!("local proxy: {peer}: refused {refusal:?}");
                let _ = client.write_all(refusal.response()).await;
                let _ = client.shutdown().await;
                return;
            }
        };
        if is_local_host(&target.host) {
            self.refuse_loopback(&target, peer);
            let _ = client.write_all(FORBIDDEN).await;
            let _ = client.shutdown().await;
            return;
        }
        let dial = (self.dial)(target.host.clone(), target.port);
        let server = match tokio::time::timeout(DIAL_TIMEOUT, dial).await {
            Ok(Ok(server)) => server,
            Ok(Err(DialError::Local)) => {
                self.refuse_loopback(&target, peer);
                let _ = client.write_all(FORBIDDEN).await;
                let _ = client.shutdown().await;
                return;
            }
            Ok(Err(DialError::Io(e))) => {
                warn!("local proxy: CONNECT {}: {e}", target.authority);
                let _ = client.write_all(BAD_GATEWAY).await;
                let _ = client.shutdown().await;
                return;
            }
            Err(_) => {
                warn!(
                    "local proxy: CONNECT {}: no connection within {DIAL_TIMEOUT:?}",
                    target.authority
                );
                let _ = client.write_all(GATEWAY_TIMEOUT).await;
                let _ = client.shutdown().await;
                return;
            }
        };
        if client.write_all(ESTABLISHED).await.is_err() {
            return;
        }
        self.serve_tunnel(target, client, server, rest).await;
    }

    /// Relay one established tunnel. `first` holds what the client sent
    /// after its `CONNECT` head.
    async fn serve_tunnel(
        self: Arc<Self>,
        target: Target,
        mut client: TcpStream,
        mut server: TcpStream,
        mut first: Vec<u8>,
    ) {
        let authority = target.authority;
        // The client's first bytes decide the path. Keep reading while they
        // could still be a preface split across reads; stop early if the
        // server speaks first (then nothing here is HTTP/2) or nothing comes.
        if undecided(&first) {
            let peek = async {
                let mut buf = [0u8; 4096];
                while undecided(&first) {
                    let n = client.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    first.extend_from_slice(&buf[..n]);
                }
                Ok::<_, std::io::Error>(())
            };
            tokio::select! {
                r = tokio::time::timeout(PEEK_TIMEOUT, peek) => {
                    if let Ok(Err(e)) = r {
                        debug!("local proxy: tunnel to {authority}: client read failed: {e}");
                        return;
                    }
                }
                _ = server.readable() => {}
            }
        }
        if h2c::looks_like_h2c(&first) {
            let id = self.next_tunnel.fetch_add(1, Ordering::Relaxed);
            info!("local proxy: tapping cleartext HTTP/2 tunnel {id} to {authority}");
            let slot: Arc<StdMutex<Option<SharedBridge>>> = Arc::default();
            let sink = {
                let relay = self.clone();
                let slot = slot.clone();
                let authority = authority.clone();
                move |events: Vec<TapEvent>| relay.on_tap_events(id, &authority, &slot, events)
            };
            if let Err(e) = h2c::relay(client, server, &first, sink).await {
                debug!("local proxy: h2c tunnel {id} to {authority} ended: {e}");
            }
            let bridge = slot.lock().expect("tunnel bridge slot poisoned").take();
            if let Some(bridge) = bridge {
                self.bridges.release(&id, bridge);
            }
            info!("local proxy: h2c tunnel {id} to {authority} closed");
        } else {
            self.note_untapped(&authority, &target.host);
            if server.write_all(&first).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        }
    }

    fn on_tap_events(
        &self,
        id: u64,
        authority: &str,
        slot: &StdMutex<Option<SharedBridge>>,
        events: Vec<TapEvent>,
    ) {
        for event in events {
            match event {
                // The path only: header values carry the bearer token.
                TapEvent::Headers {
                    stream,
                    dir: Direction::Up,
                    headers,
                } => {
                    if let Some((_, path)) = headers.iter().find(|(n, _)| n == ":path") {
                        info!("local proxy: h2c tunnel {id} stream {stream}: {path}");
                    }
                }
                TapEvent::Headers { .. } => {}
                TapEvent::Message {
                    stream,
                    dir,
                    path,
                    compressed,
                    payload,
                } => {
                    if path.as_deref() != Some(HUB_PATH) {
                        debug!(
                            "local proxy: h2c tunnel {id} stream {stream}: a {}-byte message off the hub call",
                            payload.len()
                        );
                        continue;
                    }
                    if compressed {
                        warn!(
                            "local proxy: h2c tunnel {id} stream {stream}: skipping a compressed {}-byte gRPC message",
                            payload.len()
                        );
                        continue;
                    }
                    let bridge = slot
                        .lock()
                        .expect("tunnel bridge slot poisoned")
                        .get_or_insert_with(|| {
                            self.bridges.acquire(
                                id,
                                &slugify(authority),
                                &format!("h2c {authority}{HUB_PATH}"),
                            )
                        })
                        .clone();
                    frame::deliver(
                        &bridge,
                        dir,
                        &payload,
                        FrameRaw::Binary(b64(&payload)),
                        format!("proxy:{id}"),
                        &self.inspector,
                        &self.mjai_bus,
                    );
                }
                TapEvent::Desync { dir, reason } => warn!(
                    "local proxy: h2c tunnel {id} to {authority}: stopped decoding {}: {reason}; still relaying",
                    dir.as_str()
                ),
            }
        }
    }

    /// A `CONNECT` to this machine. A game never needs one — the proxy itself
    /// lives here — so it means the redirector also matched the game's own
    /// loopback sockets, which a proxied hop would hang.
    fn refuse_loopback(&self, target: &Target, peer: SocketAddr) {
        if self.loopback_noticed.swap(true, Ordering::Relaxed) {
            debug!(
                "local proxy: refused CONNECT to loopback {} from {peer}",
                target.authority
            );
            return;
        }
        warn!(
            "local proxy: refused CONNECT to loopback {} from {peer}: the redirector is sending the \
             game's own loopback sockets here; exclude localhost, 127.0.0.1 and ::1 from its rule",
            target.authority
        );
        let _ = self.notify_bus.send(
            Notification::warn("The redirector sends loopback traffic to the proxy")
                .body(format!(
                    "Refused CONNECT to {}. Games talk to themselves over loopback, and proxying \
                     that hangs them. Exclude localhost, 127.0.0.1 and ::1 from the rule that \
                     redirects the game.",
                    target.authority
                ))
                .sticky()
                .id("proxy-loopback-connect"),
        );
    }

    /// A tunnel relayed without looking inside, so the timeline says so
    /// rather than simply leaving the connection out.
    fn note_untapped(&self, authority: &str, host: &str) {
        self.inspector.record(InspectorEntry::Http {
            ts_ms: Local::now().timestamp_millis(),
            source: InspectorSource::Mitm,
            exchange: HttpExchange {
                exchange_id: None,
                phase: HttpPhase::Request,
                method: "CONNECT".to_string(),
                url: authority.to_string(),
                host: host.to_string(),
                version: "HTTP/1.1".to_string(),
                status: None,
                headers: Vec::new(),
                body: None,
                annotations: vec![HttpAnnotation {
                    kind: "akagi_bypass".to_string(),
                    summary: "relayed without inspection — not cleartext HTTP/2".to_string(),
                    data: serde_json::json!({
                        "reason": "only cleartext HTTP/2 tunnels are tapped; \
                                   other tunnels, TLS included, are relayed untouched",
                    }),
                }],
            },
        });
    }
}

#[cfg(test)]
mod tests;
