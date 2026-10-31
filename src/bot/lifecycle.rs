//! Cancellation and reaping shared by the engine host and its installer.

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::process::{Child, ChildStdin};
use tokio::sync::watch;
use tracing::{info, warn};

const GRACE: Duration = Duration::from_secs(3);
const KILL_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct ReleaseFailure(anyhow::Error);

impl std::fmt::Display for ReleaseFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "owned process release failed: {:#}", self.0)
    }
}
impl std::error::Error for ReleaseFailure {}

pub(crate) fn stopping(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}

pub(crate) async fn cancelled(stop: &mut watch::Receiver<bool>) {
    while !stopping(stop) {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// Close the owned pipe, then reap the owned process. A kill request alone is
/// never evidence that its resources have been released.
pub(crate) async fn stop_child(
    child: &mut Child,
    stdin: Option<ChildStdin>,
    kind: &str,
) -> Result<()> {
    stop_child_inner(child, stdin, kind)
        .await
        .map_err(|error| ReleaseFailure(error).into())
}

async fn stop_child_inner(child: &mut Child, stdin: Option<ChildStdin>, kind: &str) -> Result<()> {
    let pid = child.id();
    drop(stdin);
    let status = match tokio::time::timeout(GRACE, child.wait()).await {
        Ok(status) => status.with_context(|| format!("waiting for {kind} {pid:?}"))?,
        Err(_) => {
            warn!(
                ?pid,
                kind, "shutdown grace expired; terminating owned process"
            );
            child
                .start_kill()
                .with_context(|| format!("terminating {kind} {pid:?}"))?;
            tokio::time::timeout(KILL_WAIT, child.wait())
                .await
                .with_context(|| format!("{kind} {pid:?} did not exit after termination"))?
                .with_context(|| format!("reaping {kind} {pid:?}"))?
        }
    };
    info!(?pid, kind, %status, "owned process released and reaped");
    Ok(())
}
