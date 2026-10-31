//! What the core's parts share for as long as it runs: the configuration, the
//! buses, the monitor, and the handles capture and autoplay are driven by.
//!
//! `headless` builds one [`AppState`] at start and hands a clone to each
//! request from the shell; everything in it is an `Arc` or a channel sender.

use crate::autoplay::AutoplayContext;
use crate::capture::embedded::EmbeddedGame;
use crate::config::AppConfig;
use crate::event_bus::{
    self, AccountBus, BotResponseBus, BotStatusBus, CaptureStatusBus, MjaiBus, NotifyBus,
};
use crate::game_state::GameTracker;
use crate::logger::Session;
use crate::monitor::MonitorHandle;
use crate::schema::CaptureStatus;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};

/// The running capture's control handle.
///
/// `stop` is `Some` while the capture task is alive; sending `()` asks it to
/// shut down. After it has, the supervisor sets `stop = None` and updates
/// `status`.
pub struct CaptureControl {
    pub status: CaptureStatus,
    pub stop: Option<oneshot::Sender<()>>,
}

impl Default for CaptureControl {
    fn default() -> Self {
        Self {
            status: CaptureStatus::Stopped,
            stop: None,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub monitor: MonitorHandle,
    pub config: Arc<RwLock<AppConfig>>,
    pub config_path: Arc<PathBuf>,
    pub log_session: Arc<Session>,
    /// The games that have been played, on disk.
    pub history: Arc<crate::history::HistoryStore>,

    pub mjai_bus: MjaiBus,
    pub bot_response_bus: BotResponseBus,
    pub bot_status_bus: BotStatusBus,
    pub capture_status_bus: CaptureStatusBus,
    pub notify_bus: NotifyBus,
    /// The player's own ranks, from capture to the monitor.
    pub account_bus: AccountBus,

    /// The game view the shell has made, once it has told the core of one.
    pub embedded_game: Arc<Mutex<Option<EmbeddedGame>>>,
    pub capture_control: Arc<Mutex<CaptureControl>>,
    /// The live mirror of the game. Autoplay reads it to place its input.
    pub game_tracker: Arc<Mutex<GameTracker>>,
    /// Shared by capture, which writes the game page's handle here when it
    /// sees the game's WebSocket open, and autoplay, which reads it to send
    /// input. Always present: switching autoplay on needs no re-plumbing.
    pub autoplay_context: Arc<AutoplayContext>,
    /// Set once the autoplay manager has been started. It then runs for the
    /// life of the process and reads `autoplay.enabled` for every decision.
    pub autoplay_manager_started: Arc<AtomicBool>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: AppConfig,
        config_path: PathBuf,
        log_session: Arc<Session>,
        history: Arc<crate::history::HistoryStore>,
        mjai_bus: MjaiBus,
        bot_response_bus: BotResponseBus,
        bot_status_bus: BotStatusBus,
        capture_status_bus: CaptureStatusBus,
        notify_bus: NotifyBus,
        game_tracker: Arc<Mutex<GameTracker>>,
    ) -> Self {
        Self {
            monitor: MonitorHandle::default(),
            config: Arc::new(RwLock::new(config)),
            config_path: Arc::new(config_path),
            log_session,
            history,
            mjai_bus,
            bot_response_bus,
            bot_status_bus,
            capture_status_bus,
            notify_bus,
            account_bus: event_bus::account_bus(),
            embedded_game: Arc::new(Mutex::new(None)),
            capture_control: Arc::new(Mutex::new(CaptureControl::default())),
            game_tracker,
            autoplay_context: Arc::new(AutoplayContext::new()),
            autoplay_manager_started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Change the configuration and write the keys the change touched back to
    /// the file it was loaded from. Everything else in that file stays as it
    /// is on disk, including what another writer changed after this core
    /// loaded it.
    pub async fn save_config(
        &self,
        change: impl FnOnce(&mut AppConfig),
    ) -> Result<AppConfig, String> {
        let mut config = self.config.write().await;
        let mut next = config.clone();
        change(&mut next);
        let saved = crate::config::merge_changes(
            &*config,
            &next,
            &std::fs::read_to_string(&*self.config_path).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        std::fs::write(&*self.config_path, saved).map_err(|e| e.to_string())?;
        // Revoke before publishing input settings, including an off/on
        // interval entirely inside a pending press. Unrelated UI preferences
        // do not interrupt a live decision. Capture/profile ownership uses
        // invalidate_ui at its separate session boundary.
        if config.autoplay.enabled != next.autoplay.enabled
            || config.platform.kind != next.platform.kind
            || config.monitor.quick_confirm != next.monitor.quick_confirm
            || config.monitor.auto_continue != next.monitor.auto_continue
            || config.monitor.auto_win != next.monitor.auto_win
        {
            self.autoplay_context.settings_changed();
        }
        *config = next.clone();
        Ok(next)
    }
}
