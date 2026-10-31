//! Run with AKAGI_TEST_PYTHON=<absolute Python> and --ignored. The Python
//! process is both a dependency-free installer fixture and the engine fixture.
use super::*;
use crate::bot::RuntimeMode;
use std::net::TcpListener;
use std::path::Path;

struct Fixture {
    root: tempfile::TempDir,
    package: Package,
    runtime: PythonRuntime,
    publisher: Arc<Publisher>,
}

fn fixture(phase: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let package = super::super::tests::package(root.path(), "demo");
    let dir = &package.dir;
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname='lifecycle-fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    std::fs::write(dir.join("phase"), phase).unwrap();
    for name in ["sync", "engine.py"] {
        std::fs::write(dir.join(name), include_str!("fixture.py")).unwrap();
    }
    let python = PathBuf::from(
        std::env::var_os("AKAGI_TEST_PYTHON")
            .expect("set AKAGI_TEST_PYTHON to an absolute Python executable"),
    );
    assert!(python.is_file());
    let runtime = PythonRuntime::from_paths(python.clone(), python, RuntimeMode::System);
    let publisher = Arc::new(Publisher::new(package.state_file()));
    Fixture {
        root,
        package,
        runtime,
        publisher,
    }
}

async fn marker(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture marker missing: {}", path.display()))
}

async fn until(publisher: &Publisher, ready: impl Fn(&EngineStatus) -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !ready(&publisher.status()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn start(fixture: &Fixture) -> (watch::Sender<bool>, tokio::task::JoinHandle<Result<()>>) {
    let (stop, signal) = watch::channel(false);
    let (package, runtime, publisher) = (
        fixture.package.clone(),
        fixture.runtime.clone(),
        fixture.publisher.clone(),
    );
    let task = tokio::spawn(async move { host(&package, Some(runtime), publisher, signal).await });
    (stop, task)
}

async fn shutdown_phase(phase: &str) {
    let fixture = fixture(phase);
    let dir = fixture.package.dir.clone();
    let (stop, task) = start(&fixture);
    let pid = marker(&dir.join("service.pid")).await;
    let port: u16 = marker(&dir.join("port")).await.parse().unwrap();
    if phase != "starting" {
        until(&fixture.publisher, |s| {
            matches!(s, EngineStatus::Ready { .. })
        })
        .await;
    }
    let started = Instant::now();
    stop.send(true).unwrap();
    if phase != "stubborn" {
        marker(&dir.join("eof")).await;
        assert!(!task.is_finished(), "stdin EOF is not process exit");
        assert!(
            TcpListener::bind(("127.0.0.1", port)).is_err(),
            "fixture still owns its listener"
        );
    }
    tokio::time::timeout(Duration::from_secs(7), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "the engine's port must be released before the host returns"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    println!(
        "teardown {phase}: pid={pid}, port={port}, released+reaped after {:?}",
        started.elapsed()
    );
    drop(fixture.root);
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn ready_engine_holds_shutdown_until_pid_and_port_are_released() {
    shutdown_phase("ready").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn loading_engine_is_reaped_before_it_ever_says_ready() {
    shutdown_phase("starting").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn eof_ignoring_engine_is_killed_and_reaped_within_the_bound() {
    shutdown_phase("stubborn").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn stopping_installation_reaps_installer_and_never_starts_the_engine() {
    let fixture = fixture("installing");
    let dir = fixture.package.dir.clone();
    let (stop, task) = start(&fixture);
    let pid = marker(&dir.join("installer.pid")).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(7), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!dir.join("service.pid").exists());
    assert!(!dir.join(".akagi/synced.stamp").exists());
    println!("installer {pid} cancelled and reaped; no engine or successful stamp");
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn stopping_during_a_retry_never_spawns_the_replacement() {
    let fixture = fixture("retry");
    let (stop, task) = start(&fixture);
    until(&fixture.publisher, |s| {
        matches!(s, EngineStatus::Failed { .. })
    })
    .await;
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
    tokio::time::sleep(RETRY_FIRST + Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(fixture.package.dir.join("runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn the_engine_is_told_its_package_and_endpoint_file() {
    let fixture = fixture("ready");
    let dir = fixture.package.dir.clone();
    let (stop, task) = start(&fixture);
    let told = marker(&dir.join("told")).await;
    assert_eq!(
        told.lines().collect::<Vec<_>>(),
        [
            dir.to_string_lossy().as_ref(),
            dir.join("endpoint.json").to_string_lossy().as_ref()
        ]
    );
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}
