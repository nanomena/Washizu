//! The engine host: the one process that owns an engine for the whole
//! application.
//!
//! The desktop launcher runs `akagi-core engine-host --engine <package>` when
//! the first account that uses the engine opens, and holds its stdin until the
//! application closes. The host installs the package's environment, runs its
//! entry, starts it again when it exits (after 2 s, doubling up to 60 s while
//! starts never become ready), and publishes where it stands in the package's
//! `host-state.json` for every account's core to read. When its stdin closes it
//! stops the engine, reaps it and exits. See `PROTOCOL.md`.
//!
//! The contract with the entry is three pipes:
//!
//! - **stdout**: one JSON line `{"type":"ready", ...}` once it serves. A
//!   `label` string and a `facts` list on it are shown to the user as they
//!   are; Washizu reads nothing else into them.
//! - **stderr**: its log, copied into the host's.
//! - **stdin**: never written. The host holds it open and the engine exits
//!   when it closes, so the engine ends with the host however the host ends.

use super::status::{EngineStatus, HostState, HEARTBEAT};
use super::{Package, Packages};
use crate::bot::lifecycle::{cancelled, stop_child, stopping, ReleaseFailure};
use crate::bot::runtime::{self, PythonRuntime};
use crate::monitor::{facts_of, Fact};
use anyhow::{Context as _, Result};
use clap::Parser;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::sync::watch;
use tracing::{error, info, warn};

/// Wait before starting the engine again, doubled for each start that never
/// became ready.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_LONGEST: Duration = Duration::from_secs(60);
/// How long an exited engine's log is still read for.
const LAST_WORDS: Duration = Duration::from_millis(500);

#[derive(Parser, Debug)]
#[command(
    name = "akagi-core engine-host",
    about = "Run one engine package for the whole application"
)]
struct Args {
    /// The engine package's folder, `<data>/engines/<name>`.
    #[arg(long)]
    engine: PathBuf,
}

/// Run the host until its stdin closes. `argv` starts with `engine-host`.
pub fn run(argv: impl IntoIterator<Item = std::ffi::OsString>) -> Result<()> {
    let args = Args::parse_from(argv);
    let dir = std::path::absolute(&args.engine)?;
    let (Some(root), Some(name)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str())) else {
        anyhow::bail!("{} is not an engine package folder", dir.display());
    };
    let package = Packages::new(root.to_path_buf())
        .find(name)?
        .with_context(|| format!("{} holds no engine.toml", dir.display()))?;
    let _session = crate::logger::init(&dir.join("logs"), "info", "info", &[])?;
    info!(engine = %package.name, dir = %dir.display(), "engine host started");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let (stop, signal) = watch::channel(false);
            // The launcher holds the other end of stdin for as long as the
            // application runs.
            tokio::spawn(async move {
                let mut stdin = tokio::io::stdin();
                let mut buffer = [0u8; 256];
                while matches!(stdin.read(&mut buffer).await, Ok(n) if n > 0) {}
                let _ = stop.send(true);
            });
            let publisher = Arc::new(Publisher::new(package.state_file()));
            let result = host(
                &package,
                PythonRuntime::locate(None).ok(),
                publisher.clone(),
                signal,
            )
            .await;
            publisher.withdraw();
            if let Err(error) = &result {
                error!(%error, "engine cleanup failed");
            }
            result
        })
}

/// Writes `host-state.json`: every change at once, and the current state
/// again every [`HEARTBEAT`].
pub struct Publisher {
    path: PathBuf,
    current: Mutex<EngineStatus>,
}

impl Publisher {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            current: Mutex::new(EngineStatus::None),
        }
    }

    fn set(&self, status: EngineStatus) {
        *self.current.lock().expect("host state lock poisoned") = status;
        self.refresh();
    }

    fn refresh(&self) {
        let state = HostState {
            pid: std::process::id(),
            updated_ms: now_ms(),
            status: self
                .current
                .lock()
                .expect("host state lock poisoned")
                .clone(),
        };
        if let Err(error) = state.write(&self.path) {
            warn!(%error, "engine host: state not published");
        }
    }

    pub fn status(&self) -> EngineStatus {
        self.current
            .lock()
            .expect("host state lock poisoned")
            .clone()
    }

    /// Remove the file this process wrote, and only that.
    fn withdraw(&self) {
        let ours = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HostState>(&bytes).ok())
            .is_some_and(|state| state.pid == std::process::id());
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// How one run of the engine ended.
#[derive(Debug, PartialEq)]
enum End {
    Stopped,
    Failed { error: String, was_ready: bool },
}

/// What an engine says on its `ready` line.
#[derive(Debug, PartialEq)]
struct Ready {
    label: Option<String>,
    facts: Vec<Fact>,
}

/// Keep the engine running until `stop`, its state published and kept fresh
/// throughout, installation included.
pub async fn host(
    package: &Package,
    runtime: Option<PythonRuntime>,
    publisher: Arc<Publisher>,
    stop: watch::Receiver<bool>,
) -> Result<()> {
    let heartbeat = {
        let (publisher, mut stop) = (publisher.clone(), stop.clone());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(HEARTBEAT);
            loop {
                tokio::select! {
                    biased;
                    _ = cancelled(&mut stop) => break,
                    _ = every.tick() => publisher.refresh(),
                }
            }
        })
    };
    let mut stop = stop;
    let result = run_engine(package, runtime.as_ref(), &publisher, &mut stop).await;
    heartbeat.abort();
    let _ = heartbeat.await;
    result
}

async fn run_engine(
    package: &Package,
    runtime: Option<&PythonRuntime>,
    publisher: &Publisher,
    stop: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut delay = RETRY_FIRST;
    loop {
        if stopping(stop) {
            return Ok(());
        }
        let end = match runtime {
            Some(runtime) => serve(package, runtime, publisher, stop).await?,
            None => End::Failed {
                error: "no python3+uv runtime found to run the engine with".into(),
                was_ready: false,
            },
        };
        let End::Failed { error, was_ready } = end else {
            return Ok(());
        };
        if stopping(stop) {
            return Ok(());
        }
        if was_ready {
            delay = RETRY_FIRST;
        }
        warn!(engine = %package.name, "engine stopped: {error}; starting it again in {delay:?}");
        publisher.set(EngineStatus::Failed {
            bot: package.name.clone(),
            error,
            retry_at_ms: now_ms() + delay.as_millis() as i64,
        });
        tokio::select! {
            biased;
            _ = cancelled(stop) => return Ok(()),
            _ = tokio::time::sleep(delay) => {}
        }
        delay = next_delay(delay);
    }
}

fn next_delay(delay: Duration) -> Duration {
    (delay * 2).min(RETRY_LONGEST)
}

fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// The `ready` line of an engine; `None` for any other line.
fn parse_ready(line: &str) -> Option<Ready> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("type")?.as_str()? != "ready" {
        return None;
    }
    Some(Ready {
        label: value
            .get("label")
            .and_then(|v| v.as_str())
            .map(String::from),
        facts: facts_of(value.get("facts")),
    })
}

/// Install, start and watch the engine until it stops.
async fn serve(
    package: &Package,
    runtime: &PythonRuntime,
    publisher: &Publisher,
    stop: &mut watch::Receiver<bool>,
) -> Result<End> {
    let name = package.name.clone();
    let failed = |error: String, was_ready: bool| End::Failed { error, was_ready };

    if !runtime::is_synced(&package.dir) {
        publisher.set(EngineStatus::Installing { bot: name.clone() });
    }
    match runtime.ensure_synced_until(&package.dir, stop).await {
        Ok(true) => {}
        Ok(false) => return Ok(End::Stopped),
        Err(e) if stopping(stop) || e.is::<ReleaseFailure>() => {
            return Err(e.context("stopping the engine installer"))
        }
        Err(e) => {
            return Ok(failed(
                format!("installing its environment failed: {e:#}"),
                false,
            ))
        }
    }
    if stopping(stop) {
        return Ok(End::Stopped);
    }

    // A previous engine that crashed left its endpoint behind; the new one
    // publishes its own once it listens.
    let endpoint = package.endpoint();
    let _ = std::fs::remove_file(&endpoint.0);
    let entry = package
        .entry
        .to_str()
        .context("the entry's path is not valid UTF-8")?;
    let mut command = runtime.command_for(&package.dir, &["-u", entry]);
    command
        .env("WASHIZU_ENGINE_DIR", &package.dir)
        .env("WASHIZU_ENGINE_ENDPOINT", &endpoint.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Ok(failed(format!("it could not be started: {e}"), false)),
    };
    let started = Instant::now();
    info!(engine = %name, pid = ?child.id(), "engine started");
    publisher.set(EngineStatus::Starting {
        bot: name.clone(),
        since_ms: now_ms(),
    });

    // Held and never written: the engine exits when this closes, which is
    // when this function returns or the host goes away.
    let stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        stop_child(&mut child, stdin, "engine").await?;
        return Ok(failed("its stdout is missing".into(), false));
    };
    let mut stdout = BufReader::new(stdout).lines();
    let last_log = Arc::new(Mutex::new(String::new()));
    let mut log = child.stderr.take().map(|stderr| {
        let (name, last_log) = (name.clone(), last_log.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                info!(engine = %name, "{line}");
                *last_log.lock().expect("engine log lock poisoned") = line;
            }
        })
    });

    let mut was_ready = false;
    let mut stdout_open = true;
    let end = loop {
        tokio::select! {
            biased;
            _ = cancelled(stop) => break End::Stopped,
            status = child.wait() => {
                // Its last words say why it left; give the copy a moment to
                // reach the end of what it wrote.
                if let Some(mut log) = log.take() {
                    if tokio::time::timeout(LAST_WORDS, &mut log).await.is_err() {
                        log.abort();
                        let _ = log.await;
                    }
                }
                let said = last_log.lock().expect("engine log lock poisoned").clone();
                let status = status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                let error = if said.is_empty() { format!("it exited ({status})") } else { format!("it exited ({status}): {said}") };
                break failed(error, was_ready);
            }
            line = stdout.next_line(), if stdout_open => match line {
                Ok(Some(line)) => {
                    if let Some(ready) = parse_ready(&line) {
                        was_ready = true;
                        let startup_ms = started.elapsed().as_millis() as u64;
                        info!(engine = %name, startup_ms, label = ?ready.label, "engine ready");
                        publisher.set(EngineStatus::Ready {
                            bot: name.clone(),
                            label: ready.label,
                            facts: ready.facts,
                            since_ms: now_ms(),
                            startup_ms,
                        });
                    } else if !line.trim().is_empty() {
                        info!(engine = %name, "{line}");
                    }
                }
                _ => stdout_open = false,
            },
        }
    };
    stop_child(&mut child, stdin, "engine").await?;
    if let Some(mut log) = log {
        if tokio::time::timeout(LAST_WORDS, &mut log).await.is_err() {
            log.abort();
            let _ = log.await;
        }
    }
    Ok(end)
}

/// The account's engine state for its monitor: the host's state of the engine
/// its active bot slot names, refreshed every second; `None` when it names no
/// engine. Runs for the core's lifetime.
pub async fn mirror_status(
    context: super::Context,
    config: Arc<tokio::sync::RwLock<crate::config::AppConfig>>,
    monitor: crate::monitor::MonitorHandle,
) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    let mut shown: Option<EngineStatus> = None;
    loop {
        every.tick().await;
        let names = {
            let cfg = config.read().await;
            [cfg.bot.active_4p.clone(), cfg.bot.active_3p.clone()]
        };
        let mut status = EngineStatus::None;
        for name in names {
            match context.packages.find(&name) {
                Ok(Some(package)) => {
                    status = HostState::read_status(&package.state_file(), &package.name, now_ms());
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    status = EngineStatus::Failed {
                        bot: name.clone(),
                        error: format!("{error:#}"),
                        retry_at_ms: now_ms() + RETRY_FIRST.as_millis() as i64,
                    };
                    break;
                }
            }
        }
        // A snapshot goes out on every change; the same failure read again
        // with a later retry time is not one.
        let same = match (&shown, &status) {
            (
                Some(EngineStatus::Failed { bot, error, .. }),
                EngineStatus::Failed {
                    bot: now_bot,
                    error: now_error,
                    ..
                },
            ) => bot == now_bot && error == now_error,
            (Some(previous), current) => previous == current,
            (None, _) => false,
        };
        if !same {
            monitor.set_server(status.clone());
            shown = Some(status);
        }
    }
}

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_line_carries_what_the_user_is_shown() {
        let line = r#"{"type": "ready", "label": "Model A", "facts": [{"name": "Device", "value": "CPU", "note": "Chosen at start."}, {"name": "Seed", "value": "7"}, {"name": "no value"}], "port": 9000}"#;
        assert_eq!(
            parse_ready(line),
            Some(Ready {
                label: Some("Model A".into()),
                facts: vec![
                    Fact {
                        name: "Device".into(),
                        value: "CPU".into(),
                        note: Some("Chosen at start.".into()),
                    },
                    // A fact is a name and a value; one without both is left out.
                    Fact {
                        name: "Seed".into(),
                        value: "7".into(),
                        note: None,
                    },
                ],
            })
        );
        // The line alone is the signal; what it adds is optional.
        assert_eq!(
            parse_ready(r#"{"type":"ready"}"#),
            Some(Ready {
                label: None,
                facts: vec![],
            })
        );
    }

    #[test]
    fn only_a_ready_object_is_a_ready_line() {
        for line in [
            "",
            "Ready after 19 s",
            r#"{"type":"none"}"#,
            r#"["ready"]"#,
            r#"{"ready":true}"#,
        ] {
            assert_eq!(parse_ready(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_start_that_never_became_ready_waits_longer_each_time() {
        let mut delay = RETRY_FIRST;
        let mut waits = vec![];
        for _ in 0..7 {
            waits.push(delay.as_secs());
            delay = next_delay(delay);
        }
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn the_host_withdraws_only_its_own_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-state.json");
        let publisher = Publisher::new(path.clone());
        publisher.set(EngineStatus::Installing { bot: "demo".into() });
        assert!(path.exists());
        publisher.withdraw();
        assert!(!path.exists());
        // A state another host wrote stays.
        HostState {
            pid: std::process::id().wrapping_add(1),
            updated_ms: 0,
            status: EngineStatus::None,
        }
        .write(&path)
        .unwrap();
        publisher.withdraw();
        assert!(path.exists());
    }

    #[tokio::test]
    async fn stopping_before_start_installs_and_spawns_nothing() {
        let root = tempfile::tempdir().unwrap();
        let package = super::super::tests::package(root.path(), "demo");
        let publisher = Arc::new(Publisher::new(package.state_file()));
        let (_owner, signal) = watch::channel(true);
        host(&package, None, publisher.clone(), signal)
            .await
            .unwrap();
        assert!(!package.dir.join(".akagi").exists());
        assert_eq!(publisher.status(), EngineStatus::None);
    }

    #[tokio::test]
    async fn without_a_runtime_the_failure_is_published_and_retried() {
        let root = tempfile::tempdir().unwrap();
        let package = super::super::tests::package(root.path(), "demo");
        let publisher = Arc::new(Publisher::new(package.state_file()));
        let (owner, signal) = watch::channel(false);
        let task = {
            let (package, publisher) = (package.clone(), publisher.clone());
            tokio::spawn(async move { host(&package, None, publisher, signal).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !matches!(publisher.status(), EngineStatus::Failed { .. }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            HostState::read_status(&package.state_file(), "demo", now_ms()),
            EngineStatus::Failed { error, .. } if error.contains("runtime")
        ));
        owner.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
}
