//! The proxy end to end, over real loopback sockets: a fake battle server
//! stands behind a test dialer (the real one refuses loopback targets), and a
//! test bridge echoes every message it is handed as an mjai event, so what
//! reaches the bridge can be read off the bus.

use super::*;
use crate::bridge::{Bridge, ParseResult};
use crate::config::Platform;
use crate::event_bus::CapturedEvent;
use crate::logger::{FlowLogger, Session};
use crate::schema::{MjaiEvent, ParsedFrame};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

const BATTLE_HOST: &str = "battle.example.invalid";

/// Hands back every message as `InterruptedKyoku { reason: "<dir>:<text>" }`.
struct EchoBridge;

impl Bridge for EchoBridge {
    fn parse(&mut self, direction: Direction, content: &[u8]) -> ParseResult {
        ParseResult {
            events: vec![MjaiEvent::InterruptedKyoku {
                reason: format!(
                    "{}:{}",
                    direction.as_str(),
                    String::from_utf8_lossy(content)
                ),
            }],
            parsed: Some(ParsedFrame {
                method: "Echo".into(),
                args: serde_json::Value::Null,
            }),
            shareable: true,
            window: None,
        }
    }

    fn build(&mut self, _command: &MjaiEvent) -> Option<Vec<u8>> {
        None
    }
}

fn echo_bridge(
    _: Platform,
    _: Option<Arc<FlowLogger>>,
    _: Option<Arc<Session>>,
    _: BridgeHooks,
) -> Box<dyn Bridge> {
    Box::new(EchoBridge)
}

struct Harness {
    proxy: SocketAddr,
    battle: TcpListener,
    relay: Arc<Relay>,
    mjai: broadcast::Receiver<CapturedEvent>,
    notices: broadcast::Receiver<Notification>,
    stop: Option<oneshot::Sender<()>>,
    served: JoinHandle<()>,
    dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        let battle = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let battle_addr = battle.local_addr().unwrap();
        // Only the fake battle host resolves, and to the local fake server.
        let dial: Dial = Arc::new(move |host, port| {
            Box::pin(async move {
                if host == BATTLE_HOST && port == 443 {
                    TcpStream::connect(battle_addr).await.map_err(DialError::Io)
                } else {
                    Err(DialError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "unknown test host",
                    )))
                }
            })
        });
        let dir = tempfile::TempDir::new().unwrap();
        let (inspector, _) = InspectorWriter::open(
            &dir.path().join("inspector.jsonl"),
            &dir.path().join("full.jsonl"),
            64,
        )
        .unwrap();
        let mjai_bus = crate::event_bus::mjai_bus();
        let notify_bus = crate::event_bus::notify_bus();
        let (mjai, notices) = (mjai_bus.subscribe(), notify_bus.subscribe());
        let bridges = FlowBridges::new(
            crate::capture::test_session(),
            Platform::HoloMahjong,
            BridgeHooks::default(),
        )
        .with_maker(echo_bridge);
        let relay = Arc::new(Relay::new(bridges, inspector, mjai_bus, notify_bus, dial));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let served = tokio::spawn(serve(listener, relay.clone(), async {
            let _ = stopped.await;
        }));
        Self {
            proxy,
            battle,
            relay,
            mjai,
            notices,
            stop: Some(stop),
            served,
            dir,
        }
    }

    /// Send `request` and return the proxy's status line, with the stream.
    async fn request(&self, request: &str) -> (String, TcpStream) {
        let mut game = TcpStream::connect(self.proxy).await.unwrap();
        game.write_all(request.as_bytes()).await.unwrap();
        let (head, rest) = within(read_head(&mut game)).await.unwrap().unwrap();
        assert!(rest.is_empty(), "nothing follows the proxy's answer");
        let head = String::from_utf8(head).unwrap();
        (head.lines().next().unwrap().to_string(), game)
    }

    /// An established tunnel to the fake battle server: (game, server).
    async fn tunnel(&self) -> (TcpStream, TcpStream) {
        let (status, game) = self
            .request(&format!(
                "CONNECT {BATTLE_HOST}:443 HTTP/1.1\r\nHost: {BATTLE_HOST}:443\r\n\r\n"
            ))
            .await;
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        let (server, _) = within(self.battle.accept()).await.unwrap();
        (game, server)
    }

    async fn next_echo(&mut self) -> String {
        match within(self.mjai.recv()).await.unwrap().event {
            MjaiEvent::InterruptedKyoku { reason } => reason,
            other => panic!("not an echo: {other:?}"),
        }
    }

    fn timeline(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
}

async fn within<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("timed out")
}

/// Read exactly `len` bytes.
async fn read_n(stream: &mut TcpStream, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    within(stream.read_exact(&mut buf)).await.unwrap();
    buf
}

/// Whether the peer has closed (EOF or reset) within the time limit.
async fn closed(stream: &mut TcpStream) -> bool {
    let mut buf = [0u8; 64];
    loop {
        match within(stream.read(&mut buf)).await {
            Ok(0) | Err(_) => return true,
            Ok(_) => continue,
        }
    }
}

fn frame(ftype: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    f.extend([ftype, flags]);
    f.extend(stream.to_be_bytes());
    f.extend(payload);
    f
}

fn grpc(compressed: bool, msg: &[u8]) -> Vec<u8> {
    let mut m = vec![u8::from(compressed)];
    m.extend((msg.len() as u32).to_be_bytes());
    m.extend(msg);
    m
}

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const SETTINGS: u8 = 0x4;
const END_HEADERS: u8 = 0x4;

#[tokio::test]
async fn h2c_tunnels_relay_untouched_and_hand_hub_messages_to_their_bridge() {
    let mut h = Harness::new().await;
    let (mut game, mut server) = h.tunnel().await;

    let mut enc = loona_hpack::Encoder::new();
    let mut headers = |path: &str| {
        enc.encode(vec![
            (&b":method"[..], &b"POST"[..]),
            (&b":path"[..], path.as_bytes()),
            (&b"authorization"[..], &b"Bearer fake-token"[..]),
        ])
    };
    let mut up = h2c::PREFACE.to_vec();
    up.extend(frame(SETTINGS, 0, 0, &[]));
    up.extend(frame(HEADERS, END_HEADERS, 1, &headers(HUB_PATH)));
    up.extend(frame(DATA, 0, 1, &grpc(false, b"ready")));
    up.extend(frame(HEADERS, END_HEADERS, 3, &headers("/IOther/Call")));
    up.extend(frame(DATA, 0, 3, &grpc(false, b"elsewhere")));
    up.extend(frame(DATA, 0, 1, &grpc(true, b"zipped")));
    up.extend(frame(DATA, 0, 1, &grpc(false, b"join")));
    // Split mid-preface, as a client may write it.
    game.write_all(&up[..10]).await.unwrap();
    game.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    game.write_all(&up[10..]).await.unwrap();
    assert_eq!(
        read_n(&mut server, up.len()).await,
        up,
        "client bytes reach the server"
    );

    let mut response = frame(HEADERS, END_HEADERS, 1, &[0x88]); // :status 200
    response.extend(frame(DATA, 0, 1, &grpc(false, b"welcome")));
    server.write_all(&response).await.unwrap();
    assert_eq!(
        read_n(&mut game, response.len()).await,
        response,
        "server bytes reach the client"
    );

    // Only the hub call's uncompressed messages reach the bridge, in order.
    assert_eq!(h.next_echo().await, "up:ready");
    assert_eq!(h.next_echo().await, "up:join");
    assert_eq!(h.next_echo().await, "down:welcome");
    assert_eq!(h.relay.bridges.len(), 1, "one bridge for the tunnel");

    // Each side's close reaches the other, and the tunnel's bridge is dropped.
    drop(game);
    assert!(closed(&mut server).await);
    drop(server);
    within(async {
        while !h.relay.bridges.is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(h.mjai.try_recv().is_err(), "nothing else was delivered");

    let full = h.timeline("full.jsonl");
    assert_eq!(full.matches("\"Echo\"").count(), 3, "{full}");
    assert!(full.contains("proxy:"), "{full}");
    assert!(
        !full.contains("fake-token"),
        "header values stay out: {full}"
    );
}

#[tokio::test]
async fn other_tunnels_are_relayed_untouched_and_noted() {
    let mut h = Harness::new().await;
    let (mut game, mut server) = h.tunnel().await;
    let hello = b"\x16\x03\x01\x00\x05hello";
    game.write_all(hello).await.unwrap();
    assert_eq!(read_n(&mut server, hello.len()).await, hello);
    server.write_all(b"reply").await.unwrap();
    assert_eq!(read_n(&mut game, 5).await, b"reply");
    drop(server);
    assert!(closed(&mut game).await);

    let shared = h.timeline("inspector.jsonl");
    assert!(shared.contains("akagi_bypass"), "{shared}");
    assert!(
        !shared.contains("hello"),
        "no content on the timeline: {shared}"
    );
    assert!(h.mjai.try_recv().is_err());
    assert!(h.relay.bridges.is_empty());
}

#[tokio::test]
async fn a_server_that_speaks_first_ends_the_wait() {
    let h = Harness::new().await;
    let (mut game, mut server) = h.tunnel().await;
    server.write_all(b"220 ready\r\n").await.unwrap();
    // Well before the peek's own time limit.
    let greeting = tokio::time::timeout(Duration::from_secs(2), async {
        let mut buf = vec![0u8; 11];
        game.read_exact(&mut buf).await.map(|_| buf)
    })
    .await
    .expect("relayed without waiting for the client")
    .unwrap();
    assert_eq!(greeting, b"220 ready\r\n");
    game.write_all(b"HELO").await.unwrap();
    assert_eq!(read_n(&mut server, 4).await, b"HELO");
}

#[tokio::test]
async fn loopback_targets_are_refused_with_one_notice() {
    let mut h = Harness::new().await;
    for target in ["127.0.0.1:9", "localhost:9", "[::1]:9", "0.0.0.0:9"] {
        let (status, mut game) = h
            .request(&format!("CONNECT {target} HTTP/1.1\r\n\r\n"))
            .await;
        assert_eq!(status, "HTTP/1.1 403 Forbidden", "{target}");
        assert!(closed(&mut game).await);
    }
    let notice = h.notices.try_recv().unwrap();
    assert_eq!(notice.id.as_deref(), Some("proxy-loopback-connect"));
    assert!(h.notices.try_recv().is_err(), "the notice goes out once");
    // A loopback literal is refused before anything is dialled.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), h.battle.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn anything_but_connect_is_refused() {
    let h = Harness::new().await;
    for (request, status) in [
        (
            "GET http://example.invalid/ HTTP/1.1\r\nHost: example.invalid\r\n\r\n",
            "HTTP/1.1 405 Method Not Allowed",
        ),
        ("hello\r\n\r\n", "HTTP/1.1 400 Bad Request"),
        (
            "CONNECT battle.example.invalid HTTP/1.1\r\n\r\n",
            "HTTP/1.1 400 Bad Request",
        ),
        (
            "CONNECT battle.example.invalid:443 HTTP/2\r\n\r\n",
            "HTTP/1.1 400 Bad Request",
        ),
        (
            "CONNECT unknown.example.invalid:443 HTTP/1.1\r\n\r\n",
            "HTTP/1.1 502 Bad Gateway",
        ),
    ] {
        let (got, mut game) = h.request(request).await;
        assert_eq!(got, status, "{request:?}");
        assert!(closed(&mut game).await, "{request:?}");
    }
}

#[tokio::test]
async fn stopping_closes_the_listener_and_every_tunnel() {
    let mut h = Harness::new().await;
    let (mut tapped, mut tapped_server) = h.tunnel().await;
    tapped.write_all(h2c::PREFACE).await.unwrap();
    read_n(&mut tapped_server, h2c::PREFACE.len()).await;
    // One still waiting for its client's first bytes, one without a request.
    let (mut peeking, mut peeking_server) = h.tunnel().await;
    let mut idle = TcpStream::connect(h.proxy).await.unwrap();

    h.stop.take().unwrap().send(()).unwrap();
    within(&mut h.served).await.unwrap();
    for stream in [
        &mut tapped,
        &mut tapped_server,
        &mut peeking,
        &mut peeking_server,
        &mut idle,
    ] {
        assert!(closed(stream).await);
    }
    assert!(
        TcpStream::connect(h.proxy).await.is_err(),
        "nothing listens"
    );
}

#[test]
fn connect_requests_parse_to_their_target() {
    let target = |line: &str| parse_connect(format!("{line}\r\nHost: x\r\n\r\n").as_bytes());
    assert_eq!(
        target("CONNECT battle.example.invalid:443 HTTP/1.1"),
        Ok(Target {
            authority: "battle.example.invalid:443".into(),
            host: "battle.example.invalid".into(),
            port: 443,
        })
    );
    assert_eq!(
        target("CONNECT [2001:db8::1]:8443 HTTP/1.0").map(|t| (t.host, t.port)),
        Ok(("2001:db8::1".into(), 8443))
    );
    assert!(matches!(
        target("GET / HTTP/1.1"),
        Err(Refusal::NotConnect(_))
    ));
    for bad in [
        "CONNECT battle.example.invalid HTTP/1.1",
        "CONNECT battle.example.invalid:0 HTTP/1.1",
        "CONNECT user:pass@battle.example.invalid:443 HTTP/1.1",
        "CONNECT  battle.example.invalid:443 HTTP/1.1",
        "CONNECT battle.example.invalid:443",
        "",
    ] {
        assert!(
            matches!(target(bad), Err(Refusal::BadRequest(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn local_hosts_are_detected() {
    for host in [
        "127.0.0.1",
        "127.9.9.9",
        "::1",
        "[::1]",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "::",
        "localhost",
        "LocalHost.",
        "game.localhost",
    ] {
        assert!(is_local_host(host), "{host}");
    }
    for host in [
        "192.0.2.10",
        "2001:db8::1",
        "battle.example.invalid",
        "localhost.example.invalid",
    ] {
        assert!(!is_local_host(host), "{host}");
    }
}

#[tokio::test]
async fn names_that_resolve_to_this_machine_are_not_dialled() {
    assert!(matches!(
        dial_public("localhost".into(), 9).await,
        Err(DialError::Local)
    ));
    assert!(matches!(
        dial_public("127.0.0.1".into(), 9).await,
        Err(DialError::Local)
    ));
}

#[test]
fn the_listen_address_is_a_fixed_loopback_port() {
    assert_eq!(
        listen_addr("127.0.0.1:23410"),
        Ok("127.0.0.1:23410".parse().unwrap())
    );
    assert!(listen_addr("[::1]:23410").is_ok());
    for bad in [
        "",
        "0.0.0.0:23410",
        "192.0.2.10:23410",
        "[::]:23410",
        "127.0.0.1:0",
        "127.0.0.1",
        "localhost:23410",
        " 127.0.0.1:23410",
    ] {
        assert!(listen_addr(bad).is_err(), "{bad:?}");
    }
}
