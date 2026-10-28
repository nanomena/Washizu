//! Akagi without a window: everything the application does except draw.
//!
//! A desktop shell (`desktop/`) starts this process, shows the game in a
//! Chromium view of its own, and tells this side where that view's DevTools
//! endpoint is. From there the core is the application: it captures the game
//! over CDP, tracks it, asks the bot, plays the answer, and keeps the monitor
//! snapshot the shell's interface draws.
//!
//! # Protocol
//!
//! JSON lines, one message a line. The shell writes requests to the core's
//! stdin, the core answers on its stdout, and nothing else is ever written
//! there — logs go to the session's files and to stderr.
//!
//! ```text
//! shell → core   {"id": 7, "method": "get_monitor", "params": {}}
//! core → shell   {"id": 7, "result": {…}}        or   {"id": 7, "error": "…"}
//! core → shell   {"event": "monitor-updated", "payload": {…}}
//! ```
//!
//! | Method                 | Params                   | Result            |
//! |------------------------|--------------------------|-------------------|
//! | `get_monitor`          | —                        | `MonitorSnapshot` |
//! | `get_hand_events`      | `{round}`                | `[MonitorEvent]`  |
//! | `get_history`          | —                        | `[GameRecord]`    |
//! | `set_monitor_trail`    | `{enabled}`              | `MonitorConfig`   |
//! | `set_monitor_lean`     | `{enabled}`              | `MonitorConfig`   |
//! | `set_monitor_confirm`  | `{enabled}`              | `MonitorConfig`   |
//! | `set_monitor_auto_win` | `{enabled}`              | `MonitorConfig`   |
//! | `set_monitor_continue` | `{enabled}`              | `MonitorConfig`   |
//! | `set_user_pointer`     | `{window, active, x?, y?}` | `bool`            |
//! | `set_monitor_autoplay` | `{enabled}`              | `null`            |
//! | `open_game`            | `{port, target}`         | `null`            |
//!
//! | Event             | Payload                                              |
//! |-------------------|------------------------------------------------------|
//! | `ready`           | `{brand, platform, monitor, version}` — the first line written |
//! | `monitor-updated` | `MonitorSnapshot`                                    |
//! | `pointer`         | `{x, y, down}` — autoplay's pointer, while the trail is on |
//! | `shutdown-complete` | `{ok, error}` — terminal, flushed only after owned service cleanup |
//!
//! `MonitorSnapshot.forecast` is the optional pointer prediction's own status:
//! `{owner, state, ...}`. `state` is disabled, unsupported, waiting, running,
//! ready or unavailable. Running/ready carry `{context, through, position}`;
//! unsupported/unavailable carry `reason`. Private session references never
//! enter this DTO. This field changes neither decision source nor primary
//! health. It uses the existing snapshot revision and `monitor-updated` stream;
//! `set_monitor_lean` remains the single setting for this feature.
//!
//! # Lifetime
//!
//! The core lives as long as its stdin is open. The shell holds the other end
//! and never has to remember to stop anything: when it goes, for whatever
//! reason, the core stops capture, cancels and reaps its resident service,
//! flushes `shutdown-complete`, and exits. Missing or failed cleanup must keep
//! the shell's account ownership held. See `bot/SERVICE-LIFETIME.md`.

use crate::capture::cdp::EmbeddedPage;
use crate::capture::embedded::{self, EmbeddedGame};
use crate::capture::supervisor;
use crate::config::{self, AppConfig, MonitorConfig, Platform};
use crate::monitor::MonitorSnapshot;
use crate::state::AppState;
use crate::{autoplay, bot, event_bus, game_state, history, logger, monitor, schema, util};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "akagi-core",
    about = "Washizu's core, for a desktop shell to run"
)]
struct Cli {
    /// The folder everything is kept in: `config.toml`, and the bots, logs and
    /// history beside it. Created if it does not exist.
    #[arg(long)]
    home: PathBuf,
}

/// One request from the shell.
#[derive(Debug, Deserialize)]
struct Request {
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

/// What the shell needs before it can open its window.
#[derive(Debug, Serialize)]
struct Ready {
    /// The name the window goes by.
    brand: String,
    platform: Platform,
    monitor: MonitorConfig,
    version: &'static str,
}

/// Whether the shell is drawing autoplay's pointer: its marks go out only then.
static TRAIL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct UserPointer {
    window: String,
    active: bool,
    /// Where the user's mouse rests on the game view, in the game page's CSS
    /// pixels, when it comes to rest there.
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Hand {
    round: u64,
}

#[derive(Debug, Deserialize)]
struct Enabled {
    enabled: bool,
}

/// Where the shell's game view can be reached.
#[derive(Debug, Deserialize)]
struct Game {
    /// The local port the shell's Chromium serves DevTools on.
    port: u16,
    /// The game view's target id there; the shell's own pages share the port.
    target: String,
}

/// How long capture is given to let go of a page before a new one is opened.
const CAPTURE_STOP: Duration = Duration::from_secs(6);
/// How long capture is given to stop once the shell has gone.
const LAST_WORDS: Duration = Duration::from_secs(2);

/// Run the core until the shell closes its stdin. Returns only on a failure to
/// start; a normal end leaves through `std::process::exit`.
pub fn run() -> anyhow::Result<()> {
    let args = Cli::parse();
    let home = std::path::absolute(&args.home)?;
    std::fs::create_dir_all(&home)?;
    util::set_home(home.clone());

    let config_path = home.join("config.toml");
    let cfg = config::load_config(&config_path);
    let targets = [logger::LogTarget::new("bot", "akagi::bot")];
    let session = Arc::new(logger::init(
        &util::resolve_dir(&cfg.logging.dir),
        &cfg.logging.level,
        &cfg.logging.all_level,
        &targets,
    )?);
    info!("Config loaded: {cfg:?}");
    info!("Log session at {}", session.dir().display());

    let history_root = util::resolve_dir(Path::new("./history"));
    let history_store = Arc::new(history::HistoryStore::new(history_root.clone())?);
    info!("History store at {}", history_root.display());

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(cfg, config_path, session, history_store));
    Ok(())
}

async fn serve(
    cfg: AppConfig,
    config_path: PathBuf,
    session: Arc<logger::Session>,
    history_store: Arc<history::HistoryStore>,
) {
    // The executable's own folder is where a package puts the Python runtime.
    let runtime = bot::PythonRuntime::locate(None).ok();
    match &runtime {
        Some(rt) => info!(
            "bot runtime: python={} uv={} mode={:?}",
            rt.python().display(),
            rt.uv().display(),
            rt.mode()
        ),
        None => warn!(
            "no python3+uv runtime found (neither bundled nor on PATH); bots that need one cannot start"
        ),
    }

    TRAIL.store(cfg.monitor.pointer_trail, Ordering::Relaxed);
    let ready = Ready {
        brand: monitor::brand(&cfg),
        platform: cfg.platform.kind,
        monitor: cfg.monitor.clone(),
        version: env!("CARGO_PKG_VERSION"),
    };
    let bot_enabled = cfg.bot.enabled;
    let autoplay_enabled = cfg.autoplay.enabled;
    let history_platform =
        history::recorder::shared_platform(schema::Platform::from(cfg.platform.kind));

    let mjai_bus = event_bus::mjai_bus();
    // What the tracker has applied is what the bot is asked about.
    let post_tracker_bus = event_bus::post_tracker_bus();
    let state = AppState::new(
        cfg,
        config_path,
        session,
        history_store.clone(),
        mjai_bus.clone(),
        event_bus::bot_response_bus(),
        event_bus::bot_status_bus(),
        event_bus::capture_status_bus(),
        event_bus::notify_bus(),
        game_state::tracker::new_handle(),
    );
    state.monitor.set_brand(ready.brand.clone());
    state.monitor.set_autoplay(autoplay_enabled);

    mirror_statuses(&state);
    tokio::spawn(game_state::tracker::drive_loop_with_monitor(
        state.game_tracker.clone(),
        mjai_bus.subscribe(),
        Some(post_tracker_bus.clone()),
        Some(state.monitor.clone()),
    ));
    tokio::spawn(history::recorder::drive_loop(
        history_store,
        event_bus::history_bus(),
        history_platform,
        mjai_bus.subscribe(),
    ));
    tokio::spawn(record_events(
        mjai_bus.subscribe(),
        state.log_session.inspector(),
    ));
    let service = if bot_enabled {
        // A bot's environment is installed by whichever of the two needs it
        // first, the manager or the service; this keeps them from doing it at once.
        let syncs_in_flight = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
        let manager = bot::run_bot_manager(
            state.config.clone(),
            post_tracker_bus,
            state.bot_response_bus.clone(),
            state.bot_status_bus.clone(),
            state.notify_bus.clone(),
            state.log_session.inspector(),
            runtime.clone(),
            syncs_in_flight.clone(),
            Some(state.monitor.clone()),
            state.autoplay_context.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = manager.await {
                error!("Bot manager failed: {e:#}");
            }
        });
        // The process the active bot needs across games lives and dies with
        // the core.
        Some(bot::service::start(
            state.config.clone(),
            runtime,
            syncs_in_flight,
            state.monitor.clone(),
        ))
    } else {
        None
    };
    start_autoplay(&state, autoplay_enabled);

    // One writer owns stdout, so a reply and an event never share a line.
    let (out, lines) = mpsc::unbounded_channel::<String>();
    let (finish, last_line) = oneshot::channel();
    let writer = tokio::spawn(write_lines(lines, last_line));
    send(&out, &json!({ "event": "ready", "payload": ready }));
    tokio::spawn(forward_monitor(state.monitor.subscribe(), out.clone()));
    tokio::spawn(forward_pointer(out.clone()));

    let mut requests = BufReader::new(tokio::io::stdin()).lines();
    loop {
        match requests.next_line().await {
            Ok(Some(line)) if line.trim().is_empty() => {}
            Ok(Some(line)) => match serde_json::from_str::<Request>(&line) {
                Ok(request) => {
                    let state = state.clone();
                    let out = out.clone();
                    tokio::spawn(async move {
                        let reply = match answer(&state, &request.method, request.params).await {
                            Ok(result) => json!({ "id": request.id, "result": result }),
                            Err(error) => json!({ "id": request.id, "error": error }),
                        };
                        send(&out, &reply);
                    });
                }
                Err(e) => warn!("shell: a line that is not a request: {e}"),
            },
            Ok(None) => break,
            Err(e) => {
                warn!("shell: reading requests failed: {e}");
                break;
            }
        }
    }

    info!("the shell has gone; stopping");
    supervisor::stop_and_wait(&state, LAST_WORDS).await;
    let cleanup = match service {
        Some(service) => service.shutdown().await,
        None => Ok(()),
    };
    let ok = cleanup.is_ok();
    let error = cleanup.err().map(|error| format!("{error:#}"));
    if let Some(error) = &error {
        error!(%error, "account service cleanup failed");
    }
    // The terminal line is flushed by the same stdout owner as every reply.
    // A core crash or a missing acknowledgement must not release an account.
    let _ = finish
        .send(json!({ "event": "shutdown-complete", "payload": { "ok": ok, "error": error } }));
    let _ = tokio::time::timeout(Duration::from_millis(500), writer).await;
    std::process::exit(if ok { 0 } else { 1 });
}

/// Answer one request. An `Err` is a message the shell's interface shows as is.
async fn answer(state: &AppState, method: &str, params: Value) -> Result<Value, String> {
    match method {
        "get_monitor" => reply(state.monitor.snapshot()),
        // Every game on record, oldest first. A game's own events stay on disk.
        "get_history" => reply(state.history.read_all().map_err(|e| format!("{e:#}"))?),
        "get_hand_events" => {
            let Hand { round } = parse(params)?;
            state
                .monitor
                .hand_events(round)
                .map_or_else(|| Err(format!("this game has no hand {round}")), reply)
        }
        "set_monitor_trail" => {
            let Enabled { enabled } = parse(params)?;
            let config = state
                .save_config(|c| c.monitor.pointer_trail = enabled)
                .await?;
            TRAIL.store(enabled, Ordering::Relaxed);
            reply(config.monitor)
        }
        // The shell watches the system cursor; autoplay holds its presses
        // while the user's own mouse moves over the game, and goes on from
        // where it came to rest.
        "set_user_pointer" => {
            let UserPointer {
                window,
                active,
                x,
                y,
            } = parse(params)?;
            if x.is_some() != y.is_some() {
                return Err("Pointer position requires both coordinates".into());
            }
            let at = x.zip(y);
            if at.is_some_and(|(x, y)| !x.is_finite() || !y.is_finite()) {
                return Err("Invalid pointer position".into());
            }
            reply(
                autoplay::cdp_input::set_user_pointer(&window, active, at)
                    .map_err(|error| error.to_string())?,
            )
        }
        "set_monitor_lean" => {
            let Enabled { enabled } = parse(params)?;
            let config = state
                .save_config(|c| c.monitor.pointer_lean = enabled)
                .await?;
            reply(config.monitor)
        }
        "set_monitor_confirm" => {
            let Enabled { enabled } = parse(params)?;
            let config = state
                .save_config(|c| c.monitor.quick_confirm = enabled)
                .await?;
            reply(config.monitor)
        }
        "set_monitor_auto_win" => {
            let Enabled { enabled } = parse(params)?;
            let config = state.save_config(|c| c.monitor.auto_win = enabled).await?;
            reply(config.monitor)
        }
        "set_monitor_continue" => {
            let Enabled { enabled } = parse(params)?;
            let config = state
                .save_config(|c| c.monitor.auto_continue = enabled)
                .await?;
            reply(config.monitor)
        }
        "set_monitor_autoplay" => {
            let Enabled { enabled } = parse(params)?;
            state.save_config(|c| c.autoplay.enabled = enabled).await?;
            state.monitor.set_autoplay(enabled);
            start_autoplay(state, enabled);
            Ok(Value::Null)
        }
        "open_game" => {
            open_game(state, parse(params)?).await?;
            Ok(Value::Null)
        }
        other => Err(format!("unknown method: {other}")),
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, String> {
    serde_json::from_value(params).map_err(|e| format!("invalid params: {e}"))
}

fn reply<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Capture from the game view the shell has just made, and send that view to
/// the game. Asked again — the shell made a new view — capture moves over.
async fn open_game(state: &AppState, game: Game) -> Result<(), String> {
    let config = state.config.read().await.clone();
    let url = embedded::game_url(config.platform.kind, &config.capture.url)?;
    let endpoint = embedded::discover_endpoint(game.port)
        .await
        .map_err(|e| format!("{e:#}"))?;

    supervisor::stop_and_wait(state, CAPTURE_STOP).await;
    if matches!(
        state.capture_control.lock().await.status,
        schema::CaptureStatus::Running { .. }
    ) {
        return Err("采集仍在停止，请稍后重试。".into());
    }
    *state.embedded_game.lock().await = Some(EmbeddedGame {
        endpoint,
        page: EmbeddedPage {
            url: url.to_string(),
            target: Some(game.target),
        },
    });
    supervisor::start(state.clone())
        .await
        .map_err(|e| e.to_string())
}

/// Start the autoplay manager the first time autoplay is switched on in this
/// process. It then runs for the life of the process and reads
/// `autoplay.enabled` afresh for every bot response, so switching it off needs
/// nothing more than the config change.
fn start_autoplay(state: &AppState, enabled: bool) {
    let started = state.autoplay_manager_started.clone();
    if !enabled || started.swap(true, Ordering::SeqCst) {
        return;
    }
    let manager = autoplay::run_autoplay_manager(
        state.config.clone(),
        state.autoplay_context.clone(),
        state.game_tracker.clone(),
        state.mjai_bus.clone(),
        state.bot_response_bus.clone(),
        state.notify_bus.clone(),
        // The default delay script sits beside the configuration file.
        state
            .config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        state.monitor.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = manager.await {
            error!("Autoplay manager failed: {e:#}");
            started.store(false, Ordering::SeqCst);
        }
    });
}

/// Keep the monitor in step with the bot's and capture's status, with what the
/// core has to say and with the player's own ranks, and the capture control's
/// copy of its status with it.
fn mirror_statuses(state: &AppState) {
    mirror_forecast(
        crate::bot::forecast::subscribe_status(),
        state.monitor.clone(),
    );

    let mut bots = state.bot_status_bus.subscribe();
    let monitor = state.monitor.clone();
    tokio::spawn(async move {
        loop {
            match bots.recv().await {
                Ok(status) => monitor.set_bot(status),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let mut captures = state.capture_status_bus.subscribe();
    let (monitor, control) = (state.monitor.clone(), state.capture_control.clone());
    tokio::spawn(async move {
        loop {
            match captures.recv().await {
                Ok(status) => {
                    monitor.set_capture(status.clone());
                    control.lock().await.status = status;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let mut notices = state.notify_bus.subscribe();
    let monitor = state.monitor.clone();
    tokio::spawn(async move {
        loop {
            match notices.recv().await {
                Ok(notice) => monitor.notification(&notice),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    monitor.gap(format!("Missed {n} status notices."))
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });

    let mut accounts = state.account_bus.subscribe();
    let monitor = state.monitor.clone();
    tokio::spawn(async move {
        loop {
            match accounts.recv().await {
                Ok(levels) => monitor.set_account(levels),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// Subscribe before the bot manager starts. Seed synchronously, then copy only
/// the producer's latest value; never hold a watch borrow across an await.
fn mirror_forecast(
    mut statuses: tokio::sync::watch::Receiver<crate::bot::forecast::ForecastStatus>,
    monitor: crate::monitor::MonitorHandle,
) {
    let initial = statuses.borrow_and_update().clone();
    monitor.set_forecast(initial);
    tokio::spawn(async move {
        while statuses.changed().await.is_ok() {
            let status = statuses.borrow_and_update().clone();
            monitor.set_forecast(status);
        }
    });
}

/// Mirror every event the bridge emits into the session's inspector timeline,
/// so frames, events and the bot's reactions read as one record.
async fn record_events(
    mut events: broadcast::Receiver<event_bus::CapturedEvent>,
    inspector: crate::inspector::InspectorWriter,
) {
    loop {
        match events.recv().await {
            Ok(event) => inspector.record(schema::InspectorEntry::MjaiEvent {
                ts_ms: chrono::Local::now().timestamp_millis(),
                event: event.event,
            }),
            // The timeline is a view; the session's own files hold every event.
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Send the shell every state the monitor reaches. A watch channel: a burst of
/// changes costs one line, and the latest state is never the one dropped.
async fn forward_monitor(
    mut snapshots: tokio::sync::watch::Receiver<MonitorSnapshot>,
    out: mpsc::UnboundedSender<String>,
) {
    while snapshots.changed().await.is_ok() {
        let snapshot = snapshots.borrow_and_update().clone();
        send(
            &out,
            &json!({ "event": "monitor-updated", "payload": snapshot }),
        );
    }
}

/// Send the shell where autoplay's pointer goes, while it draws it. Some
/// seventy marks a second while the pointer moves, none while it rests; a
/// lagging reader loses the oldest, which a fading trail can spare.
async fn forward_pointer(out: mpsc::UnboundedSender<String>) {
    let mut marks = autoplay::cdp_input::pointer_marks();
    loop {
        match marks.recv().await {
            Ok(mark) if TRAIL.load(Ordering::Relaxed) => {
                send(&out, &json!({ "event": "pointer", "payload": mark }));
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn send(out: &mpsc::UnboundedSender<String>, message: &Value) {
    // The writer only goes away with the process.
    let _ = out.send(message.to_string());
}

async fn write_lines(
    mut lines: mpsc::UnboundedReceiver<String>,
    mut finish: oneshot::Receiver<Value>,
) {
    let mut stdout = tokio::io::stdout();
    loop {
        let (mut line, last) = tokio::select! {
            biased;
            final_line = &mut finish => match final_line { Ok(value) => (value.to_string(), true), Err(_) => return },
            line = lines.recv() => match line { Some(line) => (line, false), None => return },
        };
        line.push('\n');
        let written = stdout.write_all(line.as_bytes()).await;
        if written.is_err() || stdout.flush().await.is_err() {
            // Nobody is reading: the shell has gone, and its stdin goes next.
            return;
        }
        if last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forecast_is_seeded_then_forwarded_on_the_existing_snapshot_stream() {
        use crate::bot::forecast::{ForecastPhase, ForecastStatus};
        let monitor = crate::monitor::MonitorHandle::default();
        let initial = ForecastStatus {
            owner: 12,
            phase: ForecastPhase::Waiting,
        };
        let (statuses, updates) = tokio::sync::watch::channel(initial.clone());
        mirror_forecast(updates, monitor.clone());
        // Startup exposes the current worker state before any task is polled.
        assert_eq!(monitor.snapshot().forecast, initial);
        let (out, mut lines) = mpsc::unbounded_channel();
        tokio::spawn(forward_monitor(monitor.subscribe(), out));
        let context = monitor.snapshot().context;
        for phase in [
            ForecastPhase::Running {
                context,
                through: 17,
                position: 4,
            },
            ForecastPhase::Ready {
                context,
                through: 17,
                position: 4,
            },
            ForecastPhase::Unavailable {
                reason: "forecast timed out".into(),
            },
            ForecastPhase::Unsupported {
                reason: "protocol lacks forecast".into(),
            },
            ForecastPhase::Disabled,
            ForecastPhase::Idle,
        ] {
            let status = ForecastStatus { owner: 12, phase };
            statuses.send_replace(status.clone());
            let line = tokio::time::timeout(std::time::Duration::from_secs(1), lines.recv())
                .await
                .unwrap()
                .unwrap();
            let message: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(message["event"], "monitor-updated");
            assert_eq!(
                message["payload"],
                serde_json::to_value(monitor.snapshot()).unwrap()
            );
            assert_eq!(
                message["payload"]["forecast"],
                serde_json::to_value(status).unwrap()
            );
            assert!(!line.contains("session"));
        }
    }

    #[test]
    fn a_request_needs_no_params() {
        let request: Request = serde_json::from_str(r#"{"id":3,"method":"get_monitor"}"#).unwrap();
        assert_eq!((request.id, request.method.as_str()), (3, "get_monitor"));
        assert!(request.params.is_null());
    }

    #[test]
    fn the_users_mouse_may_say_where_it_came_to_rest() {
        assert!(parse::<UserPointer>(json!({ "active": true })).is_err());
        let busy: UserPointer = parse(json!({ "window": "1234", "active": true })).unwrap();
        assert!(busy.active && busy.x.is_none() && busy.y.is_none());
        let rested: UserPointer =
            parse(json!({ "window": "1234", "active": false, "x": 600.5, "y": 350.0 })).unwrap();
        assert_eq!(
            (rested.active, rested.x, rested.y),
            (false, Some(600.5), Some(350.0))
        );
    }

    #[test]
    fn params_that_do_not_fit_are_refused_by_name() {
        let error = parse::<Game>(json!({ "port": "x" })).unwrap_err();
        assert!(error.starts_with("invalid params:"), "{error}");
        let game: Game = parse(json!({ "port": 9222, "target": "T" })).unwrap();
        assert_eq!((game.port, game.target.as_str()), (9222, "T"));
    }

    #[test]
    fn the_ready_line_names_the_window_and_its_options() {
        let ready = Ready {
            brand: "Washizu".into(),
            platform: Platform::Majsoul,
            monitor: MonitorConfig {
                quick_confirm: true,
                ..MonitorConfig::default()
            },
            version: "1.2.3",
        };
        assert_eq!(
            serde_json::to_value(ready).unwrap(),
            json!({
                "brand": "Washizu",
                "platform": "Majsoul",
                "monitor": {
                    "pointer_trail": false, "pointer_lean": false,
                    "quick_confirm": true, "auto_win": false, "auto_continue": false,
                },
                "version": "1.2.3",
            })
        );
    }
}
