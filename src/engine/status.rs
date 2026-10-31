//! Where the shared engine stands, as its host publishes it and as each
//! account's core reads it.
//!
//! The engine host owns `host-state.json` in the package folder: it writes
//! every change of state, and the same state again every [`HEARTBEAT`], so a
//! reader can tell a live host from a file a stopped one left behind. A core
//! only reads it, into its monitor snapshot's `server`.

use crate::monitor::Fact;
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// How often a running host rewrites its state.
pub const HEARTBEAT: Duration = Duration::from_secs(2);
/// A state older than this was left by a host that is no longer running.
pub const STALE: Duration = Duration::from_secs(10);

/// Where the shared engine stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineStatus {
    /// The account uses no engine.
    None,
    /// The engine's environment is being installed.
    Installing { bot: String },
    /// The process is up and has not said `ready`: for a model, loading and
    /// compiling.
    Starting { bot: String, since_ms: i64 },
    Ready {
        bot: String,
        /// What the engine calls itself.
        label: Option<String>,
        /// What it says about itself.
        facts: Vec<Fact>,
        since_ms: i64,
        /// How long the process took to say `ready`.
        startup_ms: u64,
    },
    /// It could not be started, or it exited, or its host is not running.
    /// The host starts it again at `retry_at_ms`.
    Failed {
        bot: String,
        error: String,
        retry_at_ms: i64,
    },
}

/// The file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostState {
    /// The host process that wrote it.
    pub pid: u32,
    /// When it was written, in milliseconds since the epoch.
    pub updated_ms: i64,
    pub status: EngineStatus,
}

impl HostState {
    /// Replace the file in one step, so a reader never sees half of it.
    pub fn write(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(self)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))
    }

    /// What a core shows for the engine `name`: the host's state while it is
    /// fresh, and a failure when there is none or it is stale.
    pub fn read_status(path: &Path, name: &str, now_ms: i64) -> EngineStatus {
        let failed = |error: String| EngineStatus::Failed {
            bot: name.to_owned(),
            error,
            retry_at_ms: now_ms + HEARTBEAT.as_millis() as i64,
        };
        let state = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<HostState>(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    return failed(format!("the engine host's state is unreadable: {error}"))
                }
            },
            Err(_) => {
                return failed("the engine host has not reported; the launcher starts it".into())
            }
        };
        if now_ms - state.updated_ms > STALE.as_millis() as i64 {
            return failed("the engine host is not running".into());
        }
        state.status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_reaches_the_frontend_tagged_by_state() {
        let ready = EngineStatus::Ready {
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
            serde_json::to_value(EngineStatus::None).unwrap(),
            serde_json::json!({"state": "none"})
        );
    }

    #[test]
    fn a_core_shows_a_fresh_state_and_a_failure_for_a_missing_or_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-state.json");
        assert!(matches!(
            HostState::read_status(&path, "demo", 1_000),
            EngineStatus::Failed { .. }
        ));
        let starting = EngineStatus::Starting {
            bot: "demo".into(),
            since_ms: 900,
        };
        HostState {
            pid: 7,
            updated_ms: 1_000,
            status: starting.clone(),
        }
        .write(&path)
        .unwrap();
        assert_eq!(HostState::read_status(&path, "demo", 1_500), starting);
        let later = 1_000 + STALE.as_millis() as i64 + 1;
        assert!(matches!(
            HostState::read_status(&path, "demo", later),
            EngineStatus::Failed { error, .. } if error.contains("not running")
        ));
        std::fs::write(&path, b"{").unwrap();
        assert!(matches!(
            HostState::read_status(&path, "demo", 1_500),
            EngineStatus::Failed { error, .. } if error.contains("unreadable")
        ));
    }
}
