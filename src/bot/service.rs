//! A bot's resident service, run for as long as the app is open.
//!
//! A bot's `bot.py` lives for one game. A bot that needs something to outlive
//! its games — a model server that takes seconds to load, or that follows a
//! whole game — declares it in its manifest:
//!
//! ```toml
//! [service]
//! entry = "service/service.py"
//! ```
//!
//! Akagi owns that process. While the bot is the active one it installs the
//! environment the entry's directory declares (`pyproject.toml`, as for a
//! bot), runs the script there with the bot's resolved settings in
//! `AKAGI_BOT_CONFIG`, and starts it again if it exits. The contract with the
//! script is three pipes:
//!
//! - **stdout**: one JSON line `{"type":"ready", ...}` once it can serve. A
//!   `label` string and a `facts` list on it are shown to the user as they
//!   are; Akagi reads nothing else into them.
//! - **stderr**: its log, copied into Akagi's.
//! - **stdin**: never written. Akagi holds it open and the script exits when
//!   it closes, so the service ends with the app however the app ends.
//!
//! One service runs at a time: the active four-player bot's, or else the
//! active three-player bot's.

use crate::bot::lifecycle::{cancelled, stop_child, stopping, ReleaseFailure};
use crate::bot::manifest::{self, Manifest};
use crate::bot::registry::BotRegistry;
use crate::bot::runtime::{self, PythonRuntime};
use crate::bot::sync_guard::SyncGuard;
use crate::config::AppConfig;
use crate::monitor::{facts_of, Fact, MonitorHandle};
use crate::util;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{watch, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// How often the choice of bot is looked at again.
const RECHECK: Duration = Duration::from_secs(5);
/// Wait before starting a service again, doubled for each start that never
/// became ready.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_LONGEST: Duration = Duration::from_secs(60);
/// How long an exited service's log is still read for.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// Where the active bot's service stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceStatus {
    /// The active bot declares no service.
    None,
    /// The service's environment is being installed.
    Installing { bot: String },
    /// The process is up and has not said `ready`: for a model, loading and
    /// compiling.
    Starting { bot: String, since_ms: i64 },
    Ready {
        bot: String,
        /// What the service calls itself.
        label: Option<String>,
        /// What it says about itself.
        facts: Vec<Fact>,
        since_ms: i64,
        /// How long the process took to say `ready`.
        startup_ms: u64,
    },
    /// It could not be started, or it exited. It is started again at
    /// `retry_at_ms`.
    Failed {
        bot: String,
        error: String,
        retry_at_ms: i64,
    },
}

/// The service one bot declares.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    bot: String,
    bot_dir: PathBuf,
    manifest: Manifest,
    /// The entry's directory: its environment and working directory.
    dir: PathBuf,
    /// The entry's file name inside `dir`.
    script: String,
}

/// How one run of a service ended.
#[derive(Debug, PartialEq)]
enum End {
    Stopped,
    /// Another bot became the active one; nothing went wrong.
    Retargeted,
    Failed {
        error: String,
        was_ready: bool,
    },
}

/// What a service says on its `ready` line.
#[derive(Debug, PartialEq)]
struct Ready {
    label: Option<String>,
    facts: Vec<Fact>,
}

/// Owns the service task through installation, loading, ready and retry waits.
/// Dropping the handle also signals cancellation; normal shutdown must await it.
pub struct ServiceHandle {
    stop: watch::Sender<bool>,
    task: JoinHandle<Result<()>>,
}

impl ServiceHandle {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.stop.send(true);
        self.task.await.context("model service task failed")?
    }
}

pub fn start(
    config: Arc<RwLock<AppConfig>>,
    runtime: Option<PythonRuntime>,
    syncs_in_flight: Arc<Mutex<HashSet<String>>>,
    monitor: MonitorHandle,
) -> ServiceHandle {
    let (stop, signal) = watch::channel(false);
    let task = tokio::spawn(async move {
        let result = run(config, runtime, syncs_in_flight, monitor, signal).await;
        if let Err(error) = &result {
            error!(%error, "model service cleanup failed; account must remain held");
        }
        result
    });
    ServiceHandle { stop, task }
}

async fn run(
    config: Arc<RwLock<AppConfig>>,
    runtime: Option<PythonRuntime>,
    syncs_in_flight: Arc<Mutex<HashSet<String>>>,
    monitor: MonitorHandle,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    let mut delay = RETRY_FIRST;
    loop {
        let target = tokio::select! {
            biased;
            _ = cancelled(&mut stop) => return Ok(()),
            target = active_target(&config) => target,
        };
        let target = match target {
            Ok(Some(target)) => target,
            Ok(None) => {
                monitor.set_server(ServiceStatus::None);
                tokio::select! { biased; _ = cancelled(&mut stop) => return Ok(()), _ = tokio::time::sleep(RECHECK) => {} }
                continue;
            }
            Err(e) => {
                warn!("bot service: {e:#}");
                monitor.set_server(ServiceStatus::None);
                tokio::select! { biased; _ = cancelled(&mut stop) => return Ok(()), _ = tokio::time::sleep(RECHECK) => {} }
                continue;
            }
        };
        let end = match &runtime {
            Some(runtime) => {
                serve(
                    &target,
                    runtime,
                    &syncs_in_flight,
                    &monitor,
                    &config,
                    &mut stop,
                )
                .await?
            }
            None => End::Failed {
                error: "no python3+uv runtime found to run the service with".into(),
                was_ready: false,
            },
        };
        if matches!(end, End::Stopped) || stopping(&stop) {
            return Ok(());
        }
        let End::Failed { error, was_ready } = end else {
            delay = RETRY_FIRST;
            continue;
        };
        if was_ready {
            delay = RETRY_FIRST;
        }
        warn!(bot = %target.bot, "bot service stopped: {error}; starting it again in {delay:?}");
        monitor.set_server(ServiceStatus::Failed {
            bot: target.bot.clone(),
            error,
            retry_at_ms: now_ms() + delay.as_millis() as i64,
        });
        tokio::select! { biased; _ = cancelled(&mut stop) => return Ok(()), _ = tokio::time::sleep(delay) => {} }
        delay = next_delay(delay);
    }
}

fn next_delay(delay: Duration) -> Duration {
    (delay * 2).min(RETRY_LONGEST)
}

fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

/// The service of the active four-player bot, or else of the active
/// three-player bot. Read from the config and the bot directory each time, so
/// a bot selected or installed while the app runs is picked up.
async fn active_target(config: &Arc<RwLock<AppConfig>>) -> Result<Option<Target>> {
    let (root, names) = {
        let cfg = config.read().await;
        (
            util::resolve_dir(Path::new(&cfg.bot.dir)),
            [cfg.bot.active_4p.clone(), cfg.bot.active_3p.clone()],
        )
    };
    let registry = BotRegistry::scan(&root)?;
    for name in names {
        let Some(entry) = registry.find(&name) else {
            continue;
        };
        let Some(manifest) = entry.manifest.as_ref() else {
            continue;
        };
        let Some(spec) = manifest.service.as_ref() else {
            continue;
        };
        let (dir, script) = entry_parts(&spec.entry)
            .with_context(|| format!("{name}'s manifest: service entry {:?}", spec.entry))?;
        return Ok(Some(Target {
            bot: name,
            bot_dir: entry.dir.clone(),
            manifest: manifest.clone(),
            dir: entry.dir.join(dir),
            script,
        }));
    }
    Ok(None)
}

/// Split a manifest's `entry` into its directory and file name. The entry
/// stays inside the bot directory: a relative path of plain names.
fn entry_parts(entry: &str) -> Result<(PathBuf, String)> {
    let path = Path::new(entry);
    if !path
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        bail!("must be a relative path inside the bot directory");
    }
    let script = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("names no script")?;
    Ok((
        path.parent().unwrap_or(Path::new("")).to_path_buf(),
        script.to_owned(),
    ))
}

/// The `ready` line of a service; `None` for any other line.
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

/// Install, start and watch one service until it stops or another bot becomes
/// the active one.
async fn serve(
    target: &Target,
    runtime: &PythonRuntime,
    syncs_in_flight: &Arc<Mutex<HashSet<String>>>,
    monitor: &MonitorHandle,
    config: &Arc<RwLock<AppConfig>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<End> {
    let bot = target.bot.clone();
    let failed = |error: String, was_ready: bool| End::Failed { error, was_ready };

    if !runtime::is_synced(&target.dir) {
        monitor.set_server(ServiceStatus::Installing { bot: bot.clone() });
    }
    {
        // The same lock a bot's own install takes, under the service's name.
        let slot = format!("{bot}/service");
        let installing = tokio::select! { biased; _ = cancelled(stop) => return Ok(End::Stopped), guard = SyncGuard::acquire(syncs_in_flight, &slot) => guard };
        let Some(_installing) = installing else {
            return Ok(failed(
                "its environment is already being installed".into(),
                false,
            ));
        };
        match runtime.ensure_synced_until(&target.dir, stop).await {
            Ok(true) => {}
            Ok(false) => return Ok(End::Stopped),
            Err(e) if stopping(stop) || e.is::<ReleaseFailure>() => {
                return Err(e.context("stopping the service installer"))
            }
            Err(e) => {
                return Ok(failed(
                    format!("installing its environment failed: {e:#}"),
                    false,
                ))
            }
        }
    }

    let settings = match manifest::load_values(&target.bot_dir, &target.manifest)
        .and_then(|values| manifest::write_resolved(&target.bot_dir, &values))
    {
        Ok(path) => path,
        Err(e) => {
            return Ok(failed(
                format!("its settings could not be read: {e:#}"),
                false,
            ))
        }
    };
    if stopping(stop) {
        return Ok(End::Stopped);
    }

    let mut command = runtime.command_for(&target.dir, &["-u", &target.script]);
    command
        .env("AKAGI_BOT_CONFIG", &settings)
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
    info!(bot = %bot, pid = ?child.id(), dir = %target.dir.display(), "bot service started");
    monitor.set_server(ServiceStatus::Starting {
        bot: bot.clone(),
        since_ms: now_ms(),
    });

    // Held and never written: the service exits when this closes, which is
    // when this function returns or the app goes away.
    let stdin = child.stdin.take();
    let Some(stdout) = child.stdout.take() else {
        stop_child(&mut child, stdin, "model service").await?;
        return Ok(failed("its stdout is missing".into(), false));
    };
    let mut stdout = BufReader::new(stdout).lines();
    let last_log = Arc::new(std::sync::Mutex::new(String::new()));
    let mut log = child.stderr.take().map(|stderr| {
        let (bot, last_log) = (bot.clone(), last_log.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                info!(service = %bot, "{line}");
                *last_log.lock().expect("service log lock poisoned") = line;
            }
        })
    });

    let mut recheck = tokio::time::interval(RECHECK);
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
                let said = last_log.lock().expect("service log lock poisoned").clone();
                let status = status.map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                let error = if said.is_empty() { format!("it exited ({status})") } else { format!("it exited ({status}): {said}") };
                break failed(error, was_ready);
            }
            line = stdout.next_line(), if stdout_open => match line {
                Ok(Some(line)) => {
                    if let Some(ready) = parse_ready(&line) {
                        was_ready = true;
                        let startup_ms = started.elapsed().as_millis() as u64;
                        info!(bot = %bot, startup_ms, label = ?ready.label, "bot service ready");
                        monitor.set_server(ServiceStatus::Ready {
                            bot: bot.clone(),
                            label: ready.label,
                            facts: ready.facts,
                            since_ms: now_ms(),
                            startup_ms,
                        });
                    } else if !line.trim().is_empty() {
                        info!(service = %bot, "{line}");
                    }
                }
                _ => stdout_open = false,
            },
            _ = recheck.tick() => {
                let still = matches!(active_target(config).await, Ok(Some(now)) if now.bot == bot && now.dir == target.dir);
                if !still {
                    info!(bot = %bot, "bot service stopped: its bot is no longer the active one");
                    break End::Retargeted;
                }
            }
        }
    };
    // This is also required on retarget: the replacement must not race the
    // old process for its port, even if the old service ignores stdin EOF.
    stop_child(&mut child, stdin, "model service").await?;
    if let Some(mut log) = log {
        if tokio::time::timeout(LAST_WORDS, &mut log).await.is_err() {
            log.abort();
            let _ = log.await;
        }
    }
    Ok(end)
}

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "manifest_version = 1\n[bot]\nname = \"demo\"\n";

    fn config(root: &Path, four: &str, three: &str) -> Arc<RwLock<AppConfig>> {
        let mut cfg = AppConfig::default();
        cfg.bot.dir = root.to_string_lossy().into_owned();
        cfg.bot.active_4p = four.into();
        cfg.bot.active_3p = three.into();
        Arc::new(RwLock::new(cfg))
    }

    fn bot(root: &Path, name: &str, service: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bot.py"), "").unwrap();
        let table = service
            .map(|entry| format!("[service]\nentry = \"{entry}\"\n"))
            .unwrap_or_default();
        std::fs::write(dir.join("manifest.toml"), format!("{MANIFEST}{table}")).unwrap();
    }

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
    fn an_entry_names_a_script_inside_the_bot_directory() {
        assert_eq!(
            entry_parts("service/service.py").unwrap(),
            (PathBuf::from("service"), "service.py".to_owned())
        );
        // A script beside bot.py shares the bot's own directory.
        assert_eq!(
            entry_parts("serve.py").unwrap(),
            (PathBuf::new(), "serve.py".to_owned())
        );
        for outside in ["../service.py", "/abs/service.py", "service/../../x.py", ""] {
            assert!(entry_parts(outside).is_err(), "{outside:?}");
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

    #[tokio::test]
    async fn the_active_bot_with_a_service_is_the_one_served() {
        let root = tempfile::tempdir().unwrap();
        bot(root.path(), "plain", None);
        bot(root.path(), "served", Some("service/service.py"));

        let target = active_target(&config(root.path(), "served", ""))
            .await
            .unwrap()
            .expect("the active bot declares a service");
        assert_eq!(target.bot, "served");
        assert_eq!(target.dir, root.path().join("served").join("service"));
        assert_eq!(target.script, "service.py");

        // A bot without one runs none, and so does no bot at all.
        for (four, three) in [("plain", ""), ("", ""), ("missing", "")] {
            assert_eq!(
                active_target(&config(root.path(), four, three))
                    .await
                    .unwrap(),
                None
            );
        }
        // The four-player bot comes first; the three-player bot's service runs when it has none.
        let three = active_target(&config(root.path(), "plain", "served"))
            .await
            .unwrap();
        assert_eq!(three.map(|t| t.bot), Some("served".to_owned()));
    }

    #[tokio::test]
    async fn an_entry_outside_the_bot_directory_is_refused() {
        let root = tempfile::tempdir().unwrap();
        bot(root.path(), "escaping", Some("../other/service.py"));
        let error = active_target(&config(root.path(), "escaping", ""))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("inside the bot directory"));
    }

    #[test]
    fn the_status_reaches_the_frontend_tagged_by_state() {
        let ready = ServiceStatus::Ready {
            bot: "demo".into(),
            label: Some("Model A".into()),
            facts: vec![Fact {
                name: "Device".into(),
                value: "CPU".into(),
                note: None,
            }],
            since_ms: 5,
            startup_ms: 19_000,
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({"state": "ready", "bot": "demo", "label": "Model A", "facts": [{"name": "Device", "value": "CPU"}], "since_ms": 5, "startup_ms": 19000})
        );
        assert_eq!(
            serde_json::to_value(ServiceStatus::None).unwrap(),
            serde_json::json!({"state": "none"})
        );
    }
}
