//! Starting and stopping capture from the game view, and saying how it stands.
//!
//! Owns `state.capture_control` — the stop handle and the latest status — and
//! emits every change on `state.capture_status_bus`. Capture runs as one task;
//! [`start`] returns once that task is spawned, not when the browser has been
//! reached, so a failure to connect arrives as an `Error` status and a notice.

use crate::capture::{CaptureCtx, ShutdownToken};
use crate::schema::{CaptureStatus, Notification};
use crate::state::AppState;
use anyhow::{bail, Result};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

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

/// Start capturing from the game view in `state.embedded_game`. Does nothing
/// if capture is already running.
pub async fn start(state: AppState) -> Result<()> {
    // If a previous task is still alive, refuse rather than race two stop
    // handles into the same control slot.
    if state.capture_control.lock().await.stop.is_some() {
        warn!("capture supervisor: already running, ignoring start");
        return Ok(());
    }
    let Some(game) = state.embedded_game.lock().await.clone() else {
        bail!("there is no game view to capture from");
    };
    state.autoplay_context.invalidate_ui();
    let (platform, http) = {
        let cfg = state.config.read().await;
        (cfg.platform.kind, cfg.capture.http.clone())
    };

    // Say `Running` up front, so the interface sees the change at once. A
    // connection that then fails flips it to `Error` from the task below.
    let address = game.address();
    let running = CaptureStatus::Running {
        descriptor: address.clone(),
    };
    let (shutdown, shutdown_notify) = ShutdownToken::new();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut ctl = state.capture_control.lock().await;
        ctl.status = running.clone();
        ctl.stop = Some(stop_tx);
    }
    let _ = state.capture_status_bus.send(running);
    tokio::spawn(async move {
        let _ = stop_rx.await;
        shutdown_notify.notify_waiters();
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
    info!("capture supervisor: capturing from the game view at {address}");
    tokio::spawn(async move {
        let result = game.run(ctx, shutdown).await;
        state.autoplay_context.invalidate_ui();
        // Input must never be left pointed at a page capture has let go of.
        *state.autoplay_context.page.write().await = None;
        *state.autoplay_context.canvas_rect.write().await = None;
        let next = match result {
            Ok(()) => {
                info!("capture supervisor: stopped cleanly");
                CaptureStatus::Stopped
            }
            Err(e) => {
                let msg = format!("{e:#}");
                error!("capture supervisor: {msg}");
                // A notice too, so it is seen without the link tab open. The
                // sticky id lets a later one replace it.
                let _ = state.notify_bus.send(
                    Notification::error("Capture stopped")
                        .body(msg.clone())
                        .sticky()
                        .id("capture-error"),
                );
                CaptureStatus::Error {
                    descriptor: Some(address),
                    error: msg,
                }
            }
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
