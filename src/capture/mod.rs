//! Capture — where the frames a [`crate::bridge::Bridge`] parses come from.
//!
//! There are two sources, one per kind of client ([`supervisor::Source`]):
//!
//! - a web game the desktop shell shows in a Chromium view of its own, watched
//!   over the Chrome DevTools Protocol ([`embedded`], [`cdp`]);
//! - a native client whose traffic an external redirector sends to a local
//!   HTTP `CONNECT` proxy, which taps its cleartext HTTP/2 gRPC tunnel
//!   ([`proxy`]).
//!
//! [`supervisor`] starts and stops the one that runs and says how it stands;
//! [`flow`] gives each connection a bridge of its own, and [`frame`] takes
//! every frame through that bridge to the inspector and
//! [`crate::event_bus::MjaiBus`].

pub mod cdp;
pub mod embedded;
pub mod flow;
pub mod frame;
pub mod proxy;
pub mod supervisor;

use crate::autoplay::AutoplayContext;
use crate::config::Platform;
use crate::event_bus::{AccountBus, MjaiBus, NotifyBus};
use crate::logger::Session;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::watch;

/// Long-lived shutdown signal. The supervisor holds the sender and sends
/// `true` when capture is to stop. The capture loop treats the future returned
/// by [`ShutdownToken::wait`] as a cooperative cancellation point. The signal
/// stays set, so a loop that starts waiting only after it was sent still
/// stops.
#[derive(Clone)]
pub struct ShutdownToken(watch::Receiver<bool>);

impl ShutdownToken {
    pub fn new() -> (Self, watch::Sender<bool>) {
        let (tx, rx) = watch::channel(false);
        (Self(rx), tx)
    }

    /// Resolves once shutdown has been requested (or its sender is gone).
    pub fn wait(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut rx = self.0.clone();
        Box::pin(async move {
            let _ = rx.wait_for(|stop| *stop).await;
        })
    }
}

/// One logging session for every capture test in this process: a session
/// installs the process-wide tracing subscriber, so there can be only one.
#[cfg(test)]
pub(crate) fn test_session() -> Arc<Session> {
    static SESSION: std::sync::OnceLock<Arc<Session>> = std::sync::OnceLock::new();
    SESSION
        .get_or_init(|| {
            let root = std::env::temp_dir()
                .join(format!("capture-tests-{}", std::process::id()))
                .join("logs");
            Arc::new(Session::init(&root, "info", "info", &[]).expect("test log session"))
        })
        .clone()
}

/// Everything capture needs to push parsed mjai events into the rest of the
/// core. Cheap to clone (all `Arc`/`Sender`/`Notify`).
#[derive(Clone)]
pub struct CaptureCtx {
    pub session: Arc<Session>,
    pub platform: Platform,
    pub mjai_bus: MjaiBus,
    pub notify_bus: NotifyBus,
    /// Where the player's own ranks go when the platform says them.
    pub account_bus: AccountBus,
    /// Shared with the autoplay manager. Capture writes the game page's
    /// `Page` handle here when it sees the game's WebSocket open; autoplay
    /// reads it to dispatch `Input.dispatchMouseEvent`.
    pub autoplay: Option<Arc<AutoplayContext>>,
    /// What to record of the HTTP traffic capture sees.
    pub http: crate::config::HttpCaptureConfig,
}
