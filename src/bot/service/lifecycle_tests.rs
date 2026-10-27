//! Run with AKAGI_TEST_PYTHON=<absolute Python> and --ignored. The Python
//! process is both a dependency-free installer fixture and the model fixture.
use super::*;
use crate::bot::RuntimeMode;
use std::net::TcpListener;

fn fixture(
    phase: &str,
) -> (
    tempfile::TempDir,
    Arc<RwLock<AppConfig>>,
    PythonRuntime,
    MonitorHandle,
) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("demo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bot.py"), "").unwrap();
    std::fs::write(
        dir.join("manifest.toml"),
        "manifest_version=1\n[bot]\nname='demo'\n[service]\nentry='service.py'\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pyproject.toml"),
        "[project]\nname='lifecycle-fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    std::fs::write(dir.join("phase"), phase).unwrap();
    for name in ["sync", "service.py"] {
        std::fs::write(dir.join(name), include_str!("fixture.py")).unwrap();
    }
    let python = PathBuf::from(
        std::env::var_os("AKAGI_TEST_PYTHON")
            .expect("set AKAGI_TEST_PYTHON to an absolute Python executable"),
    );
    assert!(python.is_file());
    let runtime = PythonRuntime::from_paths(python.clone(), python, RuntimeMode::System);
    let mut config = AppConfig::default();
    config.bot.dir = root.path().to_string_lossy().into_owned();
    config.bot.active_4p = "demo".into();
    (
        root,
        Arc::new(RwLock::new(config)),
        runtime,
        MonitorHandle::default(),
    )
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

async fn shutdown_phase(phase: &str) {
    let (root, config, runtime, monitor) = fixture(phase);
    let dir = root.path().join("demo");
    let mut snapshots = monitor.subscribe();
    let service = start(config, Some(runtime), Arc::default(), monitor);
    let pid = marker(&dir.join("service.pid")).await;
    let port: u16 = marker(&dir.join("port")).await.parse().unwrap();
    if phase != "starting" {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(snapshots.borrow().server, ServiceStatus::Ready { .. }) {
                    break;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
    let started = Instant::now();
    let shutdown = tokio::spawn(service.shutdown());
    if phase != "stubborn" {
        marker(&dir.join("eof")).await;
        assert!(!shutdown.is_finished(), "stdin EOF is not process exit");
        assert!(
            TcpListener::bind(("127.0.0.1", port)).is_err(),
            "fixture still owns its listener"
        );
    }
    tokio::time::timeout(Duration::from_secs(7), shutdown)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        TcpListener::bind(("127.0.0.1", port)).is_ok(),
        "service port must be released before shutdown returns"
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
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn ready_service_holds_shutdown_until_pid_and_port_are_released() {
    shutdown_phase("ready").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn loading_service_is_reaped_before_it_ever_says_ready() {
    shutdown_phase("starting").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn eof_ignoring_service_is_killed_and_reaped_within_the_bound() {
    shutdown_phase("stubborn").await;
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn cancelling_installation_reaps_installer_and_never_starts_service() {
    let (root, config, runtime, monitor) = fixture("installing");
    let dir = root.path().join("demo");
    let service = start(config, Some(runtime), Arc::default(), monitor);
    let pid = marker(&dir.join("installer.pid")).await;
    tokio::time::timeout(Duration::from_secs(7), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(!dir.join("service.pid").exists());
    assert!(!dir.join(".akagi/synced.stamp").exists());
    println!("installer {pid} cancelled and reaped; no service or successful stamp");
}

#[tokio::test]
#[ignore = "process fixture: set AKAGI_TEST_PYTHON"]
async fn cancelling_retry_never_spawns_the_replacement() {
    let (root, config, runtime, monitor) = fixture("retry");
    let mut snapshots = monitor.subscribe();
    let service = start(config, Some(runtime), Arc::default(), monitor);
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if matches!(snapshots.borrow().server, ServiceStatus::Failed { .. }) {
                break;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    service.shutdown().await.unwrap();
    tokio::time::sleep(RETRY_FIRST + Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("demo/runs"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_before_start_does_not_install_or_spawn() {
    let root = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.bot.dir = root.path().to_string_lossy().into_owned();
    let config = Arc::new(RwLock::new(config));
    let (_owner, signal) = watch::channel(true);
    run(
        config,
        None,
        Arc::default(),
        MonitorHandle::default(),
        signal,
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
