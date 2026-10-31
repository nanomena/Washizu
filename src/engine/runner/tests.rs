use super::*;
use crate::engine::tests::package;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// What the fake engine does differently from a well-behaved one.
#[derive(Default, Clone)]
struct Behaviour {
    /// Answer the first events request only after this long.
    slow_first: Option<Duration>,
    /// Close the connection on reading the nth events request (from 1).
    drop_at: Option<usize>,
    /// Answer with a receipt for another range.
    wrong_receipt: bool,
    /// Answer the greeting with this token instead of the endpoint's.
    ready_token: Option<String>,
}

/// A loopback engine speaking the protocol: one session per connection,
/// every request recorded.
struct FakeEngine {
    _root: tempfile::TempDir,
    package: Package,
    requests: Arc<Mutex<Vec<Value>>>,
    connections: Arc<Mutex<usize>>,
    behaviour: Arc<Mutex<Behaviour>>,
}

impl FakeEngine {
    async fn start(behaviour: Behaviour) -> Self {
        let root = tempfile::tempdir().unwrap();
        let package = package(root.path(), "fake");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            package.endpoint().0,
            json!({"protocol": PROTOCOL, "host": "127.0.0.1", "port": port, "token": "secret"})
                .to_string(),
        )
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(0));
        let behaviour = Arc::new(Mutex::new(behaviour));
        {
            let (requests, connections, behaviour) =
                (requests.clone(), connections.clone(), behaviour.clone());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    *connections.lock().unwrap() += 1;
                    tokio::spawn(serve(stream, requests.clone(), behaviour.clone()));
                }
            });
        }
        Self {
            _root: root,
            package,
            requests,
            connections,
            behaviour,
        }
    }

    fn runner(&self) -> (EngineRunner, tokio::sync::broadcast::Receiver<Notification>) {
        let notify = crate::event_bus::notify_bus();
        let receiver = notify.subscribe();
        let mut runner = EngineRunner::new(
            &self.package,
            Greeting {
                account: "account-1".into(),
                seat: 2,
                lean: true,
                game: Some(json!({"platform": "tenhou", "num_players": 4})),
            },
            notify,
        );
        runner.set_wait(Duration::from_millis(500));
        (runner, receiver)
    }

    fn events(&self) -> Vec<(u64, usize)> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["type"] == "events")
            .map(|r| {
                (
                    r["start"].as_u64().unwrap(),
                    r["events"].as_array().unwrap().len(),
                )
            })
            .collect()
    }
}

async fn serve(
    stream: TcpStream,
    requests: Arc<Mutex<Vec<Value>>>,
    behaviour: Arc<Mutex<Behaviour>>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut committed = 0u64;
    while let Ok(Some(line)) = lines.next_line().await {
        let request: Value = serde_json::from_str(&line).unwrap();
        let count = {
            let mut all = requests.lock().unwrap();
            all.push(request.clone());
            all.iter().filter(|r| r["type"] == "events").count()
        };
        let behaviour = behaviour.lock().unwrap().clone();
        let reply = match request["type"].as_str() {
            Some("hello") => {
                let token = behaviour.ready_token.unwrap_or_else(|| "secret".into());
                if request["token"] != "secret" {
                    json!({"type": "none", "meta": {"unavailable": "wrong token secret"}})
                } else {
                    json!({"type": "ready", "token": token, "notify": [{"level": "info", "title": "Playing", "body": "the room's target", "id": "engine-target"}]})
                }
            }
            Some("events") => {
                if behaviour.drop_at == Some(count) {
                    return;
                }
                if count == 1 {
                    if let Some(slow) = behaviour.slow_first {
                        tokio::time::sleep(slow).await;
                    }
                }
                let start = request["start"].as_u64().unwrap();
                let end = start + request["events"].as_array().unwrap().len() as u64;
                assert_eq!(start, committed, "a session extends its committed prefix");
                committed = end;
                let (s, e) = if behaviour.wrong_receipt {
                    (start + 1, end + 1)
                } else {
                    (start, end)
                };
                json!({"type": "none", "receipt": {"start": s, "end": e, "through": e},
                       "meta": {"show": {"title": "engine card", "items": []}, "pace": 0.5}})
            }
            _ => json!({"type": "none", "meta": {"unavailable": "unknown request"}}),
        };
        let mut bytes = serde_json::to_vec(&reply).unwrap();
        bytes.push(b'\n');
        if writer.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

fn tsumo(pai: &str) -> MjaiEvent {
    MjaiEvent::Tsumo {
        actor: 2,
        pai: pai.into(),
    }
}

fn through(response: &BotResponse) -> u64 {
    response.meta.as_ref().unwrap()["delivery"]["through"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
async fn the_greeting_names_the_account_and_seat_and_carries_no_settings() {
    let engine = FakeEngine::start(Behaviour::default()).await;
    let (mut runner, mut toasts) = engine.runner();
    let response = runner.react(&[tsumo("1m")]).await.unwrap();
    assert!(response.unavailable_reason().is_none());
    assert_eq!(through(&response), 1);
    let meta = response.meta.as_ref().unwrap();
    assert_eq!(
        meta["show"]["title"], "Test engine",
        "the card goes by the package's name"
    );
    assert_eq!(meta["pace"], 0.5);
    let hello = engine.requests.lock().unwrap()[0].clone();
    assert_eq!(
        hello,
        json!({"type": "hello", "protocol": PROTOCOL, "token": "secret", "account": "account-1",
               "seat": 2, "lean": true, "game": {"platform": "tenhou", "num_players": 4}})
    );
    let titles: Vec<String> = std::iter::from_fn(|| toasts.try_recv().ok())
        .map(|t| t.title)
        .collect();
    assert!(titles.contains(&"Playing".to_owned()), "{titles:?}");
    assert!(
        titles.contains(&"Test engine connected".to_owned()),
        "{titles:?}"
    );
}

#[tokio::test]
async fn each_batch_sends_only_the_uncommitted_suffix() {
    let engine = FakeEngine::start(Behaviour::default()).await;
    let (mut runner, _) = engine.runner();
    assert_eq!(
        through(&runner.react(&[tsumo("1m"), tsumo("2m")]).await.unwrap()),
        2
    );
    assert_eq!(through(&runner.react(&[tsumo("3m")]).await.unwrap()), 3);
    assert_eq!(engine.events(), [(0, 2), (2, 1)]);
    assert_eq!(*engine.connections.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_lost_connection_replays_the_whole_real_prefix_on_a_new_session() {
    let engine = FakeEngine::start(Behaviour {
        drop_at: Some(2),
        ..Behaviour::default()
    })
    .await;
    let (mut runner, _) = engine.runner();
    assert_eq!(through(&runner.react(&[tsumo("1m")]).await.unwrap()), 1);
    let lost = runner.react(&[tsumo("2m")]).await.unwrap();
    assert!(lost.unavailable_reason().is_some());
    assert_eq!(
        through(&lost),
        0,
        "nothing is committed without a connection"
    );
    let back = runner.react(&[tsumo("3m")]).await.unwrap();
    assert!(back.unavailable_reason().is_none());
    assert_eq!(through(&back), 3);
    assert_eq!(engine.events(), [(0, 1), (1, 1), (0, 3)]);
    assert_eq!(*engine.connections.lock().unwrap(), 2);
}

#[tokio::test]
async fn a_late_answer_is_skipped_before_the_next_request_and_never_reused() {
    let engine = FakeEngine::start(Behaviour {
        slow_first: Some(Duration::from_millis(800)),
        ..Behaviour::default()
    })
    .await;
    let (mut runner, _) = engine.runner();
    let late = runner.react(&[tsumo("1m")]).await.unwrap();
    assert!(late.unavailable_reason().is_some());
    assert_eq!(through(&late), 0);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let next = runner.react(&[tsumo("2m")]).await.unwrap();
    assert!(next.unavailable_reason().is_none());
    assert_eq!(through(&next), 2, "the late reply committed 1; this one 2");
    assert_eq!(engine.events(), [(0, 1), (1, 1)]);
    assert_eq!(
        *engine.connections.lock().unwrap(),
        1,
        "the connection outlived the late answer"
    );
}

#[tokio::test]
async fn a_receipt_for_another_range_is_refused() {
    let engine = FakeEngine::start(Behaviour {
        wrong_receipt: true,
        ..Behaviour::default()
    })
    .await;
    let (mut runner, _) = engine.runner();
    let refused = runner.react(&[tsumo("1m")]).await.unwrap();
    assert!(refused.unavailable_reason().is_some());
    assert_eq!(through(&refused), 0);
}

#[tokio::test]
async fn another_instance_on_the_port_is_never_trusted_and_said_once() {
    let engine = FakeEngine::start(Behaviour {
        ready_token: Some("other".into()),
        ..Behaviour::default()
    })
    .await;
    let (mut runner, mut toasts) = engine.runner();
    for _ in 0..2 {
        assert!(runner
            .react(&[tsumo("1m")])
            .await
            .unwrap()
            .unavailable_reason()
            .is_some());
    }
    assert!(
        engine.events().is_empty(),
        "no events reach an unverified engine"
    );
    let unreachable = std::iter::from_fn(|| toasts.try_recv().ok())
        .filter(|t| t.title == "Test engine not reachable")
        .count();
    assert_eq!(unreachable, 1);
    engine.behaviour.lock().unwrap().ready_token = None;
    assert!(runner
        .react(&[tsumo("2m")])
        .await
        .unwrap()
        .unavailable_reason()
        .is_none());
    assert_eq!(
        engine.events(),
        [(0, 3)],
        "the whole game so far reaches the engine"
    );
}

#[tokio::test]
async fn without_an_endpoint_the_engine_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let package = package(root.path(), "absent");
    let mut runner = EngineRunner::new(
        &package,
        Greeting {
            account: "a".into(),
            seat: 0,
            lean: false,
            game: None,
        },
        crate::event_bus::notify_bus(),
    );
    let reply = runner.react(&[tsumo("1m")]).await.unwrap();
    assert_eq!(reply.unavailable_reason(), Some(UNAVAILABLE));
    assert_eq!(through(&reply), 0);
}

#[tokio::test]
async fn reset_starts_a_new_game_on_a_fresh_session() {
    let engine = FakeEngine::start(Behaviour::default()).await;
    let (mut runner, _) = engine.runner();
    runner.react(&[tsumo("1m"), tsumo("2m")]).await.unwrap();
    runner.reset().await.unwrap();
    assert_eq!(through(&runner.react(&[tsumo("3m")]).await.unwrap()), 1);
    assert_eq!(engine.events(), [(0, 2), (0, 1)]);
    assert_eq!(*engine.connections.lock().unwrap(), 2);
}
