//! End-to-end: Akagi runs a bot's `[service]` with a real interpreter.
//!
//! The service here is a stand-in for a model server: it fails its first
//! start, says `ready` on its second, holds a port open while it lives, and
//! leaves when its stdin closes. The test follows it through the monitor, the
//! way the UI does.
//!
//! Skips silently when no python3+uv runtime is found (neither bundled next to
//! the test binary nor on PATH), like the example-bot tests.

use akagi::bot::service::{self, ServiceStatus};
use akagi::bot::PythonRuntime;
use akagi::config::AppConfig;
use akagi::monitor::MonitorHandle;
use std::collections::HashSet;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

const MANIFEST: &str = r#"manifest_version = 1

[bot]
name = "demo"

[service]
entry = "service/service.py"

[settings.model]
type = "string"
label = "Model"
default = "demo-model"
"#;

const PYPROJECT: &str = r#"[project]
name = "demo-service"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = []

[tool.uv]
package = false
"#;

const SERVICE: &str = r#"
import json
import os
import socket
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
with open(os.environ["AKAGI_BOT_CONFIG"], encoding = "utf-8") as handle:
    settings = json.load(handle)
runs = os.path.join(here, "runs")
count = (int(open(runs).read()) if os.path.exists(runs) else 0) + 1
with open(runs, "w") as handle:
    handle.write(str(count))
if count == 1:
    sys.stderr.write("the first start fails\n")
    sys.exit(3)

listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen()
with open(os.path.join(here, "port"), "w") as handle:
    handle.write(str(listener.getsockname()[1]))
print("a line that is not the ready line", flush = True)
print(json.dumps({"type": "ready", "label": settings["model"], "facts": [{"name": "Device", "value": "CPU"}]}), flush = True)

if os.name == "nt":
    import _winapi
    import msvcrt

    pipe = msvcrt.get_osfhandle(sys.stdin.fileno())
    try:
        while True:
            _winapi.PeekNamedPipe(pipe, 0)
            time.sleep(0.1)
    except OSError:
        pass
else:
    sys.stdin.buffer.read()
time.sleep(0.75)  # EOF is not proof that the listener has been released.
"#;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Wait for the monitor to show a server state `wanted` accepts.
async fn server_state(
    monitor: &MonitorHandle,
    what: &str,
    wanted: impl Fn(&ServiceStatus) -> bool,
) -> ServiceStatus {
    let mut updates = monitor.subscribe();
    let reached = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let server = updates.borrow_and_update().server.clone();
            if wanted(&server) {
                return server;
            }
            updates.changed().await.expect("monitor closed");
        }
    })
    .await;
    reached.unwrap_or_else(|_| {
        panic!(
            "the service never became {what}; last state {:?}",
            monitor.snapshot().server
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_service_is_installed_started_restarted_and_stopped() {
    let Ok(runtime) = PythonRuntime::locate(None) else {
        eprintln!("skipped: no python3+uv runtime");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let bot = root.path().join("demo");
    write(&bot.join("bot.py"), "");
    write(&bot.join("manifest.toml"), MANIFEST);
    write(&bot.join("service").join("pyproject.toml"), PYPROJECT);
    write(&bot.join("service").join("service.py"), SERVICE);

    let mut cfg = AppConfig::default();
    cfg.bot.dir = root.path().to_string_lossy().into_owned();
    cfg.bot.active_4p = "demo".into();
    cfg.bot.active_3p = String::new();
    let monitor = MonitorHandle::default();
    let running = service::start(
        Arc::new(RwLock::new(cfg)),
        Some(runtime),
        Arc::new(Mutex::new(HashSet::new())),
        monitor.clone(),
    );

    // The first start exits: the user is told why, and it is started again.
    let failed = server_state(&monitor, "failed", |s| {
        matches!(s, ServiceStatus::Failed { .. })
    })
    .await;
    let ServiceStatus::Failed {
        bot: name, error, ..
    } = failed
    else {
        unreachable!()
    };
    assert_eq!(name, "demo");
    assert!(error.contains("the first start fails"), "{error}");

    // The second says ready, under the name its settings gave it and with what it says of itself.
    let ready = server_state(&monitor, "ready", |s| {
        matches!(s, ServiceStatus::Ready { .. })
    })
    .await;
    let ServiceStatus::Ready { label, facts, .. } = ready else {
        unreachable!()
    };
    assert_eq!(label.as_deref(), Some("demo-model"));
    assert_eq!(facts.len(), 1);
    assert_eq!(
        (facts[0].name.as_str(), facts[0].value.as_str()),
        ("Device", "CPU")
    );

    // It is alive while Akagi runs it...
    let port: u16 = std::fs::read_to_string(bot.join("service").join("port"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());

    // ...and gone once Akagi stops: nothing is left holding its port.
    running.shutdown().await.unwrap();
    assert!(
        TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "shutdown returned before the owned listener was released"
    );
}
