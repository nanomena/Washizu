//! End-to-end: the engine host runs an engine package with a real interpreter.
//!
//! The engine here is a stand-in for a model server: it fails its first
//! start, says `ready` on its second, holds a port open while it lives,
//! publishes its endpoint, and leaves when its stdin closes. The test follows
//! it through the host's published state, the way every account's core does.
//!
//! Skips silently when no python3+uv runtime is found (neither bundled next to
//! the test binary nor on PATH), like the example-bot tests.

use akagi::bot::PythonRuntime;
use akagi::engine::host::{host, Publisher};
use akagi::engine::{EngineStatus, HostState, Packages, PROTOCOL};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

const PYPROJECT: &str = r#"[project]
name = "demo-engine"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = []

[tool.uv]
package = false
"#;

const ENGINE: &str = r#"
import json
import os
import socket
import sys
import time

here = os.environ["WASHIZU_ENGINE_DIR"]
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
port = listener.getsockname()[1]
with open(os.environ["WASHIZU_ENGINE_ENDPOINT"], "w") as handle:
    json.dump({"protocol": "washizu-engine/1", "host": "127.0.0.1", "port": port, "token": "t"}, handle)
print("a line that is not the ready line", flush = True)
print(json.dumps({"type": "ready", "label": "demo-model", "facts": [{"name": "Device", "value": "CPU"}]}), flush = True)

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

/// Wait for the published state to be one `wanted` accepts.
async fn state(
    publisher: &Publisher,
    what: &str,
    wanted: impl Fn(&EngineStatus) -> bool,
) -> EngineStatus {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let status = publisher.status();
            if wanted(&status) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the engine never became {what}; last state {:?}",
            publisher.status()
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_is_installed_started_restarted_and_stopped() {
    let Ok(runtime) = PythonRuntime::locate(None) else {
        eprintln!("skipped: no python3+uv runtime");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("demo");
    write(
        &dir.join("engine.toml"),
        &format!("protocol = \"{PROTOCOL}\"\ndisplay = \"Demo\"\nentry = \"engine.py\"\n"),
    );
    write(&dir.join("pyproject.toml"), PYPROJECT);
    write(&dir.join("engine.py"), ENGINE);
    let package = Packages::new(root.path().to_path_buf())
        .find("demo")
        .unwrap()
        .unwrap();
    let publisher = Arc::new(Publisher::new(package.state_file()));
    let (stop, signal) = watch::channel(false);
    let running = {
        let (package, publisher) = (package.clone(), publisher.clone());
        tokio::spawn(async move { host(&package, Some(runtime), publisher, signal).await })
    };

    // The first start exits: why is published, and it is started again.
    let EngineStatus::Failed { bot, error, .. } = state(&publisher, "failed", |s| {
        matches!(s, EngineStatus::Failed { .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(bot, "demo");
    assert!(error.contains("the first start fails"), "{error}");

    // The second says ready, with what it says of itself, and a core reads
    // the same from the file.
    let ready = state(&publisher, "ready", |s| {
        matches!(s, EngineStatus::Ready { .. })
    })
    .await;
    let EngineStatus::Ready { label, facts, .. } = ready.clone() else {
        unreachable!()
    };
    assert_eq!(label.as_deref(), Some("demo-model"));
    assert_eq!(
        (facts[0].name.as_str(), facts[0].value.as_str()),
        ("Device", "CPU")
    );
    let now = chrono::Local::now().timestamp_millis();
    assert_eq!(
        HostState::read_status(&package.state_file(), "demo", now),
        ready
    );

    // It is alive while the host runs it, at the endpoint it published...
    let endpoint = package.endpoint().read().unwrap();
    assert!(TcpStream::connect(("127.0.0.1", endpoint.port)).is_ok());

    // ...and gone once the host stops: nothing is left holding its port.
    stop.send(true).unwrap();
    running.await.unwrap().unwrap();
    assert!(
        TcpListener::bind(("127.0.0.1", endpoint.port)).is_ok(),
        "the host returned before the engine's listener was released"
    );
}
