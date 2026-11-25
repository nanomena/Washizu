//! Starting and stopping capture, and saying how it stands.
//!
//! Owns `state.capture_control` — the stop handle and the latest status — and
//! emits every change on `state.capture_status_bus`. Capture runs from one
//! [`Source`] at a time, as one task. [`start`] opens the source first: a proxy
//! binds its port there, so a port it cannot have fails `start` itself. It
//! then returns once the task is spawned, not when the browser has been
//! reached, so a later failure arrives as an `Error` status and a notice.

use crate::capture::embedded::EmbeddedGame;
use crate::capture::proxy::ProxyCapture;
use crate::capture::{CaptureCtx, ShutdownToken};
use crate::schema::{CaptureStatus, Notification};
use crate::state::AppState;
use anyhow::{bail, Result};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info};

/// What capture reads frames from.
#[derive(Debug)]
pub enum Source {
    /// The web game in the shell's own Chromium view, over CDP.
    Embedded(EmbeddedGame),
    /// A native client whose traffic is redirected to a local `CONNECT` proxy
    /// listening at this loopback address.
    Proxy { listen: SocketAddr },
}

impl Source {
    /// What the status shows capture is reading from.
    pub fn descriptor(&self) -> String {
        match self {
            Source::Embedded(game) => game.address(),
            Source::Proxy { listen } => format!("proxy {listen}"),
        }
    }

    async fn open(self) -> Result<Opened> {
        Ok(match self {
            Source::Embedded(game) => Opened::Embedded(game),
            Source::Proxy { listen } => Opened::Proxy(ProxyCapture::bind(listen).await?),
        })
    }
}

/// A source ready to run: the proxy's port is bound.
enum Opened {
    Embedded(EmbeddedGame),
    Proxy(ProxyCapture),
}

impl Opened {
    async fn run(self, ctx: CaptureCtx, shutdown: ShutdownToken) -> Result<()> {
        match self {
            Opened::Embedded(game) => game.run(ctx, shutdown).await,
            Opened::Proxy(proxy) => proxy.run(ctx, shutdown).await,
        }
    }
}

/// Stop capture, if it is running, and wait briefly for its task to say it has
/// stopped — so that a [`start`] that follows begins on a clean slate.
pub async fn stop_and_wait(state: &AppState, max_wait: Duration) {
    state.autoplay_context.invalidate_ui();
    // Subscribe *before* signalling shutdown so we can't lose the
    // resulting status emission to a race.
    let mut rx = state.capture_status_bus.subscribe();

    let Some(stop) = state.capture_control.lock().await.stop.take() else {
        return; // not running
    };
    let _ = stop.send(());

    let _ = tokio::time::timeout(max_wait, async {
        loop {
            match rx.recv().await {
                Ok(CaptureStatus::Stopped) | Ok(CaptureStatus::Error { .. }) => break,
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await;
}

/// Start capturing from `source`. Refused while capture is already running;
/// a source that cannot be opened is an error here and an `Error` status.
pub async fn start(state: AppState, source: Source) -> Result<()> {
    // Held from the check to the stop handle being stored, so two starts
    // cannot race two stop handles into the same slot.
    let mut ctl = state.capture_control.lock().await;
    if ctl.stop.is_some() {
        bail!("采集已在运行。");
    }
    state.autoplay_context.invalidate_ui();
    let descriptor = source.descriptor();
    let opened = match source.open().await {
        Ok(opened) => opened,
        Err(e) => {
            let status = failed(&state, &descriptor, &e);
            ctl.status = status.clone();
            drop(ctl);
            let _ = state.capture_status_bus.send(status);
            return Err(e);
        }
    };
    let (platform, http) = {
        let cfg = state.config.read().await;
        (cfg.platform.kind, cfg.capture.http.clone())
    };

    // Say `Running` up front, so the interface sees the change at once. A
    // connection that then fails flips it to `Error` from the task below.
    let running = CaptureStatus::Running {
        descriptor: descriptor.clone(),
    };
    let (shutdown, shutdown_tx) = ShutdownToken::new();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    ctl.status = running.clone();
    ctl.stop = Some(stop_tx);
    drop(ctl);
    let _ = state.capture_status_bus.send(running);
    tokio::spawn(async move {
        let _ = stop_rx.await;
        let _ = shutdown_tx.send(true);
    });

    let ctx = CaptureCtx {
        session: state.log_session.clone(),
        platform,
        mjai_bus: state.mjai_bus.clone(),
        notify_bus: state.notify_bus.clone(),
        account_bus: state.account_bus.clone(),
        autoplay: Some(state.autoplay_context.clone()),
        http,
    };
    info!("capture supervisor: capturing from {descriptor}");
    tokio::spawn(async move {
        let result = opened.run(ctx, shutdown).await;
        state.autoplay_context.invalidate_ui();
        // Input must never be left pointed at a page capture has let go of.
        *state.autoplay_context.page.write().await = None;
        *state.autoplay_context.canvas_rect.write().await = None;
        let next = match result {
            Ok(()) => {
                info!("capture supervisor: stopped cleanly");
                CaptureStatus::Stopped
            }
            Err(e) => failed(&state, &descriptor, &e),
        };
        {
            let mut ctl = state.capture_control.lock().await;
            ctl.stop = None;
            ctl.status = next.clone();
        }
        let _ = state.capture_status_bus.send(next);
    });

    Ok(())
}

/// Log a capture failure, tell the user, and return the status that says it.
fn failed(state: &AppState, descriptor: &str, error: &anyhow::Error) -> CaptureStatus {
    let msg = format!("{error:#}");
    error!("capture supervisor: {msg}");
    // A notice too, so it is seen without the link tab open. The sticky id
    // lets a later one replace it.
    let _ = state.notify_bus.send(
        Notification::error("Capture stopped")
            .body(msg.clone())
            .sticky()
            .id("capture-error"),
    );
    CaptureStatus::Error {
        descriptor: Some(descriptor.to_string()),
        error: msg,
    }
}
