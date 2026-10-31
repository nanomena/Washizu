use super::*;
use crate::autoplay::{BudgetSource, TimeBudget};
use crate::event_bus::bot_response_bus;
use crate::game_state::tracker::GameTracker;
use anyhow::{bail, Result};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Mutex;
use tokio::sync::Notify;

struct Primary {
    through: u64,
    gate: Option<Arc<Notify>>,
    first: bool,
    bare_reach: bool,
    failed: bool,
    fail_at: Option<u64>,
    calls: Arc<Mutex<Vec<Vec<MjaiEvent>>>>,
    last: BotResponse,
    restored: bool,
}

impl Primary {
    fn new(gate: Option<Arc<Notify>>) -> Self {
        Self {
            through: 0,
            restored: false,
            gate,
            first: true,
            bare_reach: false,
            failed: false,
            fail_at: None,
            calls: Arc::new(Mutex::new(vec![])),
            last: BotResponse {
                request: None,
                action: MjaiEvent::None,
                meta: None,
            },
        }
    }
}

#[async_trait]
impl BotRunner for Primary {
    async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
        self.calls.lock().unwrap().push(events.to_vec());
        self.through += events.len() as u64;
        self.restored |= events
            .iter()
            .any(|e| matches!(e, MjaiEvent::RestoreKyoku { .. }));
        if self.fail_at == Some(self.through) && !self.failed {
            self.failed = true;
            bail!("injected broken primary pipe");
        }
        if let Some(event) = events.last() {
            self.last.action = match event {
                MjaiEvent::ObservedDecision { legal, .. } => legal
                    .iter()
                    .find(|e| {
                        matches!(
                            e,
                            MjaiEvent::Dahai {
                                tsumogiri: false,
                                ..
                            }
                        )
                    })
                    .cloned()
                    .unwrap_or(MjaiEvent::None),
                MjaiEvent::Tsumo { actor: 0, pai } => {
                    if self.first {
                        self.first = false;
                        if let Some(gate) = &self.gate {
                            gate.notified().await;
                        }
                        if self.bare_reach {
                            self.last.action = MjaiEvent::Reach {
                                actor: 0,
                                pai: None,
                                tsumogiri: None,
                            };
                            self.last.meta = Some(json!({"delivery":{"through":self.through}}));
                            return Ok(self.last.clone());
                        }
                    }
                    MjaiEvent::Dahai {
                        actor: 0,
                        pai: pai.clone(),
                        tsumogiri: true,
                    }
                }
                MjaiEvent::Dora { .. } => self.last.action.clone(),
                MjaiEvent::Pon { actor: 0, .. } => MjaiEvent::Dahai {
                    actor: 0,
                    pai: "1m".into(),
                    tsumogiri: false,
                },
                _ => MjaiEvent::None,
            };
        }
        self.last.meta = Some(json!({"delivery":{"through":self.through}}));
        if self.restored {
            self.last.meta.as_mut().unwrap()["restoration"] = json!({"origin":"public_snapshot", "temporal_prefix":"reset", "missing_history":"unknown_fields", "seat":0});
        }
        Ok(self.last.clone())
    }
    async fn reset(&mut self) -> Result<()> {
        self.through = 0;
        Ok(())
    }
}

pub(crate) fn opening() -> Vec<MjaiEvent> {
    serde_json::from_value(json!([
        {"type":"start_game","id":0,"names":["","","", ""],"aka_flag":true,"num_players":4},
        {"type":"start_kyoku","bakaze":"E","kyoku":1,"honba":0,"kyotaku":0,"oya":0,
         "dora_marker":"C","scores":[25000,25000,25000,25000],
         "tehais":[["1m","3m","6m","7m","2p","4p","5pr","1s","3s","5sr","7s","F","F"],
                   ["?","?","?","?","?","?","?","?","?","?","?","?","?"],
                   ["?","?","?","?","?","?","?","?","?","?","?","?","?"],
                   ["?","?","?","?","?","?","?","?","?","?","?","?","?"]]},
        {"type":"tsumo","actor":0,"pai":"N"}
    ]))
    .unwrap()
}

/// Windows test binary -> engine runner -> real WSL engine socket/tiny JAX
/// model -> coordinator + embedded NativeBot. The barrier changes latency
/// only. The test engine's ready file names its port and token.
#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "requires own nanogamekits venv; set AKAGI_RECOVERY_WSL_ROOT"]
async fn offline_python_primary_and_native_standby_recover_across_processes() {
    real_cross_process_recovery(0, false).await;
}

#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "requires own nanogamekits venv; set AKAGI_RECOVERY_WSL_ROOT"]
async fn offline_python_inference_failure_recovers_after_native_takes_control() {
    real_cross_process_recovery(2, false).await;
}

#[cfg(target_os = "windows")]
#[tokio::test]
#[ignore = "offline only; own WSL venv, optional AKAGI_RECOVERY_CHECKPOINT; no installed games may be running"]
async fn offline_python_snapshot_primary_recovers_in_this_hand_across_processes() {
    real_cross_process_recovery(0, true).await;
}

#[cfg(target_os = "windows")]
async fn real_cross_process_recovery(failures: u32, restored: bool) {
    use crate::bridge::{Bridge, Direction, TenhouBridge};
    use crate::engine::runner::{EngineRunner, Greeting};
    use std::path::{Path, PathBuf};
    use tokio::process::Command;

    fn wsl_path(path: &Path) -> String {
        let raw = path.to_str().unwrap().replace('\\', "/");
        assert_eq!(raw.as_bytes()[1], b':');
        format!("/mnt/{}/{}", raw[..1].to_lowercase(), &raw[3..])
    }
    struct ServiceStop(PathBuf);
    impl Drop for ServiceStop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "stop");
        }
    }
    async fn wait_file(path: &Path, seconds: u64) {
        tokio::time::timeout(Duration::from_secs(seconds), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("real Python service reached its barrier");
    }

    let root = std::env::var("AKAGI_RECOVERY_WSL_ROOT").expect("own Python checkout in WSL");
    let files = tempfile::tempdir().unwrap();
    let ready = files.path().join("ready.json");
    let release = files.path().join("release");
    let stop = files.path().join("stop");
    let mut service = Command::new("wsl.exe");
    service.args([
        "-d",
        "Ubuntu",
        "--cd",
        &root,
        "--",
        ".venv/bin/python",
        "-m",
        "tools.riichi.tests.recovery_service",
        "--ready",
        &wsl_path(&ready),
        "--release",
        &wsl_path(&release),
        "--stop",
        &wsl_path(&stop),
    ]);
    service.args(["--failures", &failures.to_string()]);
    if restored {
        if let Ok(checkpoint) = std::env::var("AKAGI_RECOVERY_CHECKPOINT") {
            service.args(["--checkpoint", &wsl_path(Path::new(&checkpoint))]);
        }
    }
    service
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null());
    let mut child = service.spawn().expect("start actual tiny model service");
    let guard = ServiceStop(stop);
    wait_file(&ready, 180).await;
    let info: serde_json::Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    let package = crate::engine::tests::package(files.path(), "recovery");
    std::fs::write(
        package.endpoint().0,
        serde_json::to_vec(
            &json!({"protocol": crate::engine::PROTOCOL, "host": "127.0.0.1",
                                   "port": info["port"], "token": info["token"]}),
        )
        .unwrap(),
    )
    .unwrap();
    let primary = EngineRunner::new(
        &package,
        Greeting {
            account: "recovery-test".into(),
            seat: 0,
            lean: false,
            game: None,
        },
        crate::event_bus::notify_bus(),
    );
    let mut rig = Rig::with_runner(Box::new(primary));
    rig.budget_ms = 3_000; // includes Windows/WSL process startup, then a real inference timeout
    let mut bridge =
        TenhouBridge::new(None, None).with_shared_state(Some(rig.clock.tenhou_state.clone()));
    if restored {
        for frame in [
            r#"{"tag":"GO","type":"65","lobby":"0"}"#,
            r#"{"tag":"REINIT","seed":"0,0,0,3,0,23","ten":"250,250,250,250","oya":"0","hai":"14,18,22,36,37,48,52,57,58,61,89,93,96","kawa0":"1,106,74","kawa1":"79,122,3","kawa2":"72,100,9","kawa3":"107,78,67"}"#,
            r#"{"tag":"T120"}"#,
        ] {
            for captured in bridge
                .parse(Direction::Down, frame.as_bytes())
                .into_captured()
            {
                rig.captured(captured);
            }
        }
    } else {
        for event in opening() {
            rig.feed(event);
        }
    }
    let local = rig.reply().await;
    assert_eq!(
        local.control().unwrap().unwrap().source,
        DecisionSource::NativeStandby
    );
    assert!(
        release.with_extension("entered").exists(),
        "actual JAX call is waiting"
    );
    assert!(matches!(local.action, MjaiEvent::Dahai { .. }));
    let first_context = local.request.unwrap().context;
    if restored {
        assert!(
            matches!(&local.action, MjaiEvent::Dahai { pai, tsumogiri: true, .. } if pai == "N")
        );
        for frame in [
            r#"{"tag":"D120"}"#,
            r#"{"tag":"U"}"#,
            r#"{"tag":"E116"}"#,
            r#"{"tag":"V"}"#,
            r#"{"tag":"F132"}"#,
            r#"{"tag":"W"}"#,
            r#"{"tag":"G124"}"#,
        ] {
            for captured in bridge
                .parse(Direction::Down, frame.as_bytes())
                .into_captured()
            {
                rig.captured(captured);
            }
        }
    } else {
        rig.feed(local.action.clone());
        for (actor, tile) in [(1, "W"), (2, "C"), (3, "P")] {
            rig.feed(MjaiEvent::Tsumo {
                actor,
                pai: "?".into(),
            });
            rig.feed(MjaiEvent::Dahai {
                actor,
                pai: tile.into(),
                tsumogiri: false,
            });
        }
    }
    std::fs::write(&release, "release actual inference").unwrap();
    if failures == 0 {
        rig.caught_up(10).await;
    } else {
        tokio::time::sleep(Duration::from_millis(2_200)).await;
        assert!(matches!(
            rig.monitor.snapshot().control.unwrap().primary,
            RunnerHealth::Recovering { .. }
        ));
    }
    assert!(
        rig.output.try_recv().is_err(),
        "late actual model answer cannot replace standby"
    );
    rig.budget_ms = 3_000;
    if restored {
        for captured in bridge
            .parse(Direction::Down, br#"{"tag":"T104"}"#)
            .into_captured()
        {
            rig.captured(captured);
        }
    } else {
        rig.feed(MjaiEvent::Tsumo {
            actor: 0,
            pai: "9s".into(),
        });
    }
    let primary = rig.reply().await;
    assert_eq!(
        primary.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    let meta = primary.meta.as_ref().unwrap();
    assert_eq!(meta["model"], info["model"]);
    assert_eq!(meta["delivery"]["through"], 11);
    if restored {
        let context = primary.request.unwrap().context;
        assert_eq!(
            (context.game, context.round),
            (first_context.game, first_context.round),
            "main restored before any new hand/game"
        );
        assert_eq!(meta["restoration"]["origin"], "public_snapshot");
    }
    assert!(
        meta["step"].as_u64().unwrap() > 0,
        "cache did not reset after standby's move"
    );
    eprintln!(
        "real cross-process recovery: native={}, primary={}, model={}",
        local.meta.unwrap()["recovery_timing"],
        meta["recovery_timing"],
        meta["model"]
    );
    drop(rig);
    drop(guard);
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
}

struct Rig {
    budget_ms: u32,
    handle: RecoveryHandle,
    monitor: MonitorHandle,
    tracker: GameTracker,
    clock: Arc<AutoplayContext>,
    output: tokio::sync::broadcast::Receiver<BotResponse>,
    _directory: tempfile::TempDir,
    last: Option<TrackedEvent>,
}

impl Rig {
    fn new(primary: Primary) -> Self {
        Self::with_runner(Box::new(primary))
    }
    fn with_runner(primary: Box<dyn BotRunner>) -> Self {
        let mut cfg = AppConfig::default();
        cfg.autoplay.enabled = true;
        cfg.autoplay.delay.safety_margin_ms = 0;
        cfg.autoplay.input.hover_delay_ms = 0;
        cfg.autoplay.input.click_hold_ms = 0;
        cfg.autoplay.input.inter_click_delay_ms = 0;
        Self::with_config(primary, cfg)
    }
    fn with_config(primary: Box<dyn BotRunner>, cfg: AppConfig) -> Self {
        let config = Arc::new(RwLock::new(cfg));
        let clock = Arc::new(AutoplayContext::new());
        let monitor = MonitorHandle::default();
        let output = bot_response_bus();
        let receiver = output.subscribe();
        let directory = tempfile::tempdir().unwrap();
        let inspector = InspectorWriter::open(&directory.path().join("inspector.jsonl"), 32)
            .unwrap()
            .0;
        let handle = RecoveryHandle::spawn(
            primary,
            0,
            4,
            "test-primary".into(),
            config,
            clock.clone(),
            monitor.clone(),
            output,
            inspector,
            None,
        );
        Self {
            budget_ms: 1_400,
            handle,
            monitor,
            tracker: GameTracker::new(),
            clock,
            output: receiver,
            _directory: directory,
            last: None,
        }
    }

    fn feed(&mut self, event: MjaiEvent) {
        self.tracker.handle(&event).unwrap();
        let can_act = self.tracker.our_seat_can_act();
        if can_act == Some(true) && opens_window(&event, 0) {
            *self.clock.time_budget.write().unwrap() = Some(TimeBudget {
                fixed_ms: self.budget_ms,
                add_ms: 0,
                opened_at: Instant::now(),
                source: BudgetSource::DealTile,
            });
        } else if can_act != Some(true) {
            *self.clock.time_budget.write().unwrap() = None;
        }
        let window = self
            .clock
            .time_budget
            .read()
            .unwrap()
            .map(InputWindow::Majsoul);
        self.publish(event, can_act, window);
    }

    fn captured(&mut self, captured: crate::event_bus::CapturedEvent) {
        self.tracker.handle(&captured.event).unwrap();
        let can_act = if self.tracker.interrupted() {
            Some(captured.window.is_some())
        } else {
            self.tracker.our_seat_can_act()
        };
        self.publish(captured.event, can_act, captured.window);
    }

    fn publish(&mut self, event: MjaiEvent, can_act: Option<bool>, window: Option<InputWindow>) {
        let context = self
            .monitor
            .game_event(&event, self.tracker.snapshot(), None);
        let tracked = TrackedEvent {
            window,
            event: self.tracker.delivered_observation(&event, window),
            context: Some(context),
            can_act,
        };
        self.last = Some(tracked.clone());
        self.handle.feed(tracked).unwrap();
    }

    async fn reply(&mut self) -> BotResponse {
        tokio::time::timeout(Duration::from_secs(8), self.output.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn caught_up(&self, through: u64) {
        let mut updates = self.monitor.subscribe();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if updates.borrow().control.as_ref().is_some_and(
                    |c| matches!(c.primary, RunnerHealth::Ready { through: n } if n == through),
                ) {
                    return;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn valid_reinit_restores_primary_in_the_same_hand_and_retires_the_old_window() {
    use crate::bridge::{Bridge, Direction, TenhouBridge};
    let primary = Primary::new(None);
    let calls = primary.calls.clone();
    let mut rig = Rig::new(primary);
    let mut bridge =
        TenhouBridge::new(None, None).with_shared_state(Some(rig.clock.tenhou_state.clone()));
    for frame in [
        r#"{"tag":"GO","type":"1","lobby":"0"}"#,
        r#"{"tag":"REINIT","seed":"0,0,0,3,0,23","ten":"250,250,250,250","oya":"0","hai":"14,18,22,36,37,48,52,57,58,61,89,93,96","kawa0":"1,106,74","kawa1":"79,122,3","kawa2":"72,100,9","kawa3":"107,78,67"}"#,
        r#"{"tag":"T120"}"#,
    ] {
        for captured in bridge
            .parse(Direction::Down, frame.as_bytes())
            .into_captured()
        {
            rig.captured(captured);
        }
    }
    let reply = rig.reply().await;
    assert_eq!(
        reply.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    assert!(
        matches!(
            reply.action,
            MjaiEvent::Dahai {
                tsumogiri: false,
                ..
            }
        ),
        "Primary can discard a held tile, not only the fallback draw"
    );
    let through = calls.lock().unwrap().iter().flatten().count();
    assert_eq!(
        through, 3,
        "one start, one actual snapshot, one constrained real draw"
    );
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .any(|e| matches!(e, MjaiEvent::RestoreKyoku { .. })));
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .any(|e| matches!(e, MjaiEvent::StartKyoku { .. })));
    assert!(
        rig.tracker.snapshot().is_none(),
        "unknown history flags are not advertised as a complete replay"
    );
    let request = reply.request.unwrap();
    // Real input retires that window; repeating the confirmed delivery cannot
    // publish another primary/standby answer for the consumed request.
    for captured in bridge
        .parse(Direction::Down, br#"{"tag":"D14"}"#)
        .into_captured()
    {
        rig.captured(captured);
    }
    assert!(!request.window.unwrap().proof().is_current(&rig.clock));
    rig.handle.feed(rig.last.clone().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(rig.output.try_recv().is_err());
}

#[tokio::test]
async fn invalid_reinit_uses_one_restricted_standby_and_never_invents_a_primary_context() {
    use crate::bridge::{Bridge, Direction, TenhouBridge};
    let primary = Primary::new(None);
    let calls = primary.calls.clone();
    let mut rig = Rig::new(primary);
    let mut bridge =
        TenhouBridge::new(None, None).with_shared_state(Some(rig.clock.tenhou_state.clone()));
    for frame in [
        r#"{"tag":"GO","type":"1","lobby":"0"}"#,
        r#"{"tag":"REINIT","seed":"0,0,0,3,0,23","ten":"250,250,250,250","oya":"0","hai":"14,18,22,36,37,48,52,57,58,61,89,93,96","kawa0":"0,1,2,3,0","kawa1":"79,122,3","kawa2":"72,100,9","kawa3":"107,78,67"}"#,
        r#"{"tag":"T120"}"#,
    ] {
        for captured in bridge
            .parse(Direction::Down, frame.as_bytes())
            .into_captured()
        {
            rig.captured(captured);
        }
    }
    let response = rig.reply().await;
    let control = response.control().unwrap().unwrap();
    assert_eq!(control.source, DecisionSource::NativeStandby);
    assert!(matches!(control.primary, RunnerHealth::Blocked { .. }));
    assert_eq!(
        response.action,
        MjaiEvent::Dahai {
            actor: 0,
            pai: "N".into(),
            tsumogiri: true
        }
    );
    assert!(
        rig.tracker.snapshot().is_none(),
        "missing chronology cannot become a complete rules snapshot"
    );
    assert_eq!(rig.tracker.num_players(), Some(4));
    assert!(rig.monitor.snapshot().sync_error.is_none());
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .all(|e| matches!(e, MjaiEvent::StartGame { .. })),
        "Primary receives no gap marker or fabricated/current-hand events"
    );
    let request = response.request.unwrap();
    for captured in bridge
        .parse(Direction::Down, br#"{"tag":"D120"}"#)
        .into_captured()
    {
        rig.captured(captured);
    }
    assert!(
        !request.window.unwrap().proof().is_current(&rig.clock),
        "actual discard retires the old permission"
    );
    rig.handle.feed(rig.last.clone().unwrap()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        rig.output.try_recv().is_err(),
        "duplicate echo cannot produce another decision"
    );
    let mut next = Rig::new(Primary::new(None)); // BotManager starts a fresh owner at this real boundary
    bridge = bridge.with_shared_state(Some(next.clock.tenhou_state.clone()));
    let parsed = bridge.parse(Direction::Down, br#"{"tag":"INIT","seed":"1,0,0,1,2,8","ten":"250,250,250,250","oya":"1","hai":"14,18,22,36,37,48,52,57,58,61,89,93,96"}"#);
    assert!(matches!(
        parsed.events.as_slice(),
        [MjaiEvent::StartGame { .. }, MjaiEvent::StartKyoku { .. }]
    ));
    for captured in parsed.into_captured() {
        next.captured(captured);
    }
    for captured in bridge
        .parse(Direction::Down, br#"{"tag":"T120"}"#)
        .into_captured()
    {
        next.captured(captured);
    }
    assert_eq!(
        next.reply().await.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
}

#[tokio::test]
async fn real_native_standby_owns_one_window_then_primary_catches_up_on_confirmed_moves() {
    let release = Arc::new(Notify::new());
    let primary = Primary::new(Some(release.clone()));
    let calls = primary.calls.clone();
    let mut rig = Rig::new(primary);
    for event in opening() {
        rig.feed(event);
    }
    let local = rig.reply().await;
    let first = local.control().unwrap().unwrap();
    assert_eq!(first.source, DecisionSource::NativeStandby);
    assert!(matches!(local.action, MjaiEvent::Dahai { actor: 0, .. }));
    let ms = local.meta.as_ref().unwrap()["recovery_timing"]["decision_ms"]
        .as_u64()
        .unwrap();
    assert!(
        (700..1_350).contains(&ms),
        "actual budget/reserve cutoff: {ms} ms"
    );
    // A repeated delivery is idempotent and cannot create another decision.
    rig.handle.feed(rig.last.clone().unwrap()).unwrap();
    rig.feed(local.action.clone());
    for (actor, tile) in [(1, "W"), (2, "C"), (3, "P")] {
        rig.feed(MjaiEvent::Tsumo {
            actor,
            pai: "?".into(),
        });
        rig.feed(MjaiEvent::Dahai {
            actor,
            pai: tile.into(),
            tsumogiri: false,
        });
    }
    release.notify_one();
    rig.caught_up(10).await;
    assert!(
        rig.output.try_recv().is_err(),
        "late primary answer cannot replace standby"
    );
    assert_eq!(
        rig.monitor.snapshot().control.unwrap().source,
        DecisionSource::NativeStandby
    );
    rig.feed(MjaiEvent::Tsumo {
        actor: 0,
        pai: "9s".into(),
    });
    let main = rig.reply().await;
    assert_eq!(
        main.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    assert!(
        matches!(main.action, MjaiEvent::Dahai { ref pai, tsumogiri: true, .. } if pai == "9s")
    );
    let real: Vec<_> = calls.lock().unwrap().iter().flatten().cloned().collect();
    assert_eq!(real.len(), 11);
    assert_eq!(
        real[3], local.action,
        "the primary follows the actual standby move"
    );
    assert!(!real.iter().any(|e| matches!(e, MjaiEvent::Reach { .. })));
}

#[tokio::test]
async fn an_unexecuted_bare_reach_is_never_added_to_either_real_prefix() {
    let mut primary = Primary::new(None);
    primary.bare_reach = true;
    let calls = primary.calls.clone();
    let mut rig = Rig::new(primary);
    for event in opening() {
        rig.feed(event);
    }
    let reply = rig.reply().await;
    assert_eq!(
        reply.control().unwrap().unwrap().source,
        DecisionSource::NativeStandby
    );
    assert!(matches!(reply.action, MjaiEvent::Dahai { .. }));
    assert_eq!(calls.lock().unwrap().iter().flatten().count(), 3);
    assert!(matches!(reply.control().unwrap().unwrap().primary,
        RunnerHealth::Recovering { reason, .. } if reason.contains("rejected by the independent")));
}

#[tokio::test]
async fn real_native_legality_distinguishes_genuine_pass_from_unavailable() {
    let mut rig = Rig::new(Primary::new(None));
    for event in opening() {
        rig.feed(event);
    }
    let discard = rig.reply().await;
    rig.feed(discard.action);
    rig.feed(MjaiEvent::Tsumo {
        actor: 1,
        pai: "?".into(),
    });
    rig.feed(MjaiEvent::Dahai {
        actor: 1,
        pai: "F".into(),
        tsumogiri: false,
    });
    assert_eq!(rig.last.as_ref().unwrap().can_act, Some(true));
    let pass = rig.reply().await;
    assert_eq!(pass.action, MjaiEvent::None);
    assert_eq!(pass.unavailable_reason(), None);
    assert_eq!(
        pass.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    assert_eq!(rig.monitor.snapshot().decision.unwrap().state, "declined");
}

#[tokio::test]
async fn a_closed_real_window_cancels_the_pending_source_without_a_fallback_click() {
    let release = Arc::new(Notify::new());
    let mut rig = Rig::new(Primary::new(Some(release.clone())));
    for event in opening() {
        rig.feed(event);
    }
    // The platform confirms a user's move before the deadline. No source is
    // chosen for the now-closed window, even when the old model answers.
    rig.feed(MjaiEvent::Dahai {
        actor: 0,
        pai: "N".into(),
        tsumogiri: true,
    });
    release.notify_one();
    rig.caught_up(4).await;
    assert!(rig.output.try_recv().is_err());
}

#[tokio::test]
async fn bridge_window_closure_retires_a_request_before_the_next_tracked_frame() {
    let release = Arc::new(Notify::new());
    let mut rig = Rig::new(Primary::new(Some(release.clone())));
    for event in opening() {
        rig.feed(event);
    }
    // Wait until admission, then change the existing bridge-owned window.
    let mut updates = rig.monitor.subscribe();
    while updates.borrow().decision.is_none() {
        updates.changed().await.unwrap();
    }
    let original = rig.monitor.snapshot().context;
    *rig.clock.time_budget.write().unwrap() = None;
    release.notify_one();
    rig.caught_up(3).await;
    assert_eq!(rig.monitor.snapshot().context, original);
    assert!(rig.output.try_recv().is_err());
}

#[tokio::test]
async fn a_real_tracker_gap_retires_both_sources_even_if_the_primary_later_answers() {
    let release = Arc::new(Notify::new());
    let mut rig = Rig::new(Primary::new(Some(release.clone())));
    for event in opening() {
        rig.feed(event);
    }
    rig.monitor.gap("real tracker rejected an event".into());
    release.notify_one();
    rig.caught_up(3).await;
    assert!(rig.output.try_recv().is_err());
    assert!(rig.monitor.snapshot().sync_error.is_some());
}

#[tokio::test]
async fn an_original_request_without_a_bridge_window_cannot_borrow_an_ahead_window() {
    let mut rig = Rig::new(Primary::new(None));
    let b = TimeBudget {
        fixed_ms: 1400,
        add_ms: 0,
        opened_at: Instant::now(),
        source: BudgetSource::DealTile,
    };
    *rig.clock.time_budget.write().unwrap() = Some(b); // already ahead of queued events
    for event in opening() {
        rig.captured(event.into());
    }
    rig.caught_up(3).await;
    assert!(rig.monitor.snapshot().decision.is_none());
    assert!(rig.output.try_recv().is_err());
}

#[tokio::test]
async fn a_queued_original_window_is_never_rebound_to_a_newer_capture_slot() {
    let mut rig = Rig::new(Primary::new(None));
    let old = TimeBudget {
        fixed_ms: 3000,
        add_ms: 0,
        opened_at: Instant::now(),
        source: BudgetSource::DealTile,
    };
    let newer = TimeBudget {
        opened_at: old.opened_at + Duration::from_nanos(1),
        ..old
    };
    *rig.clock.time_budget.write().unwrap() = Some(old);
    for event in (crate::bridge::ParseResult {
        events: opening(),
        parsed: None,
        window: Some(InputWindow::Majsoul(old)),
    })
    .into_captured()
    {
        rig.captured(event);
    }
    // Capture has advanced, but tracker and coordinator still hold the old prefix.
    *rig.clock.time_budget.write().unwrap() = Some(newer);
    rig.caught_up(3).await;
    let control = rig.monitor.snapshot().control.unwrap();
    assert_eq!(control.window, Some(InputWindow::Majsoul(old).proof()));
    assert_ne!(control.window, Some(InputWindow::Majsoul(newer).proof()));
    assert!(rig.output.try_recv().is_err());
}

#[tokio::test]
async fn a_frame_ending_in_dora_and_its_restored_prefix_keep_their_original_window() {
    let mut rig = Rig::new(Primary::new(None));
    let calls = Arc::new(Mutex::new(Vec::<MjaiEvent>::new()));
    let mut events = opening();
    events.push(serde_json::from_value(json!({"type":"dora","dora_marker":"9p"})).unwrap());
    let b = TimeBudget {
        fixed_ms: 3000,
        add_ms: 0,
        opened_at: Instant::now(),
        source: BudgetSource::DealTile,
    };
    for pass in 0..2 {
        let original = TimeBudget {
            opened_at: b.opened_at + Duration::from_nanos(pass),
            ..b
        };
        *rig.clock.time_budget.write().unwrap() = Some(original);
        let parsed = crate::bridge::ParseResult {
            events: events.clone(),
            parsed: None,
            window: Some(InputWindow::Majsoul(original)),
        };
        for event in parsed.into_captured() {
            rig.captured(event);
        }
        let reply = rig.reply().await;
        assert_eq!(
            reply.control().unwrap().unwrap().source,
            DecisionSource::Primary
        );
        assert_eq!(
            reply.request.unwrap().window,
            Some(InputWindow::Majsoul(original))
        );
        assert_eq!(
            reply.request.unwrap().context,
            rig.last.as_ref().unwrap().context.unwrap()
        );
        assert_eq!(reply.meta.as_ref().unwrap()["delivery"]["through"], 4);
        assert!(matches!(
            rig.last.as_ref().unwrap().event,
            MjaiEvent::Dora { .. }
        ));
        calls.lock().unwrap().push(reply.action);
    }
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_post_pon_frame_ending_in_dora_still_opens_its_discard_decision() {
    let mut rig = Rig::new(Primary::new(None));
    let mut events = opening();
    events.extend(serde_json::from_value::<Vec<MjaiEvent>>(json!([
        {"type":"dahai","actor":0,"pai":"N","tsumogiri":true},
        {"type":"tsumo","actor":1,"pai":"?"}, {"type":"dahai","actor":1,"pai":"F","tsumogiri":false},
        {"type":"pon","actor":0,"target":1,"pai":"F","consumed":["F","F"]},
        {"type":"dora","dora_marker":"9p"}
    ])).unwrap());
    let b = TimeBudget {
        fixed_ms: 3000,
        add_ms: 0,
        opened_at: Instant::now(),
        source: BudgetSource::ChiPengGang,
    };
    *rig.clock.time_budget.write().unwrap() = Some(b);
    for event in (crate::bridge::ParseResult {
        events,
        parsed: None,
        window: Some(InputWindow::Majsoul(b)),
    })
    .into_captured()
    {
        rig.captured(event);
    }
    let reply = rig.reply().await;
    assert_eq!(
        reply.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    assert!(matches!(reply.action, MjaiEvent::Dahai { actor: 0, .. }));
    assert_eq!(
        reply.request.unwrap().context,
        rig.last.as_ref().unwrap().context.unwrap()
    );
}

#[test]
fn captured_web3_duplicate_draw_has_both_real_discard_copies_in_the_full_legal_set() {
    use crate::bridge::{Bridge, Direction, TenhouBridge};
    let mut bridge = TenhouBridge::new(None, None);
    let mut standby = standby::LocalStandby::new(3, 4).unwrap();
    let mut seq = 0;
    for frame in
        include_str!("../../../tests/fixtures/tenhou-web3-exhaustive-20261027.jsonl").lines()
    {
        for event in bridge.parse(Direction::Down, frame.as_bytes()).events {
            seq += 1;
            standby.observe(&event).unwrap();
            if seq == 49 {
                let legal = standby.legal();
                for tsumogiri in [false, true] {
                    let reply = BotResponse {
                        request: None,
                        meta: None,
                        action: MjaiEvent::Dahai {
                            actor: 3,
                            pai: "4m".into(),
                            tsumogiri,
                        },
                    };
                    assert!(
                        standby::legal_response(&reply, &legal),
                        "copy={tsumogiri}; {legal:?}"
                    );
                }
                return;
            }
        }
    }
    panic!("real seq49 was not reached");
}

#[test]
fn full_riichi_legality_requires_the_legal_discard_and_its_physical_copy() {
    let mut events = serde_json::to_value(opening()).unwrap();
    events[1]["tehais"][0] =
        json!(["1m", "1m", "1m", "2m", "3m", "4m", "2p", "3p", "4p", "2s", "3s", "4s", "5p"]);
    events[2]["pai"] = json!("9m");
    let mut bot = standby::LocalStandby::new(0, 4).unwrap();
    for event in serde_json::from_value::<Vec<MjaiEvent>>(events).unwrap() {
        bot.observe(&event).unwrap();
    }
    let legal = bot.legal();
    let reach = |pai: &str, tsumogiri| BotResponse {
        request: None,
        action: MjaiEvent::Reach {
            actor: 0,
            pai: Some(pai.into()),
            tsumogiri,
        },
        meta: None,
    };
    assert!(standby::legal_response(&reach("9m", Some(true)), &legal));
    assert!(!standby::legal_response(&reach("9m", Some(false)), &legal));
    assert!(!standby::legal_response(&reach("9m", None), &legal));
    assert!(standby::legal_response(&reach("5p", Some(false)), &legal));
    assert!(!standby::legal_response(&reach("5pr", Some(false)), &legal));
}

#[test]
fn a_winning_reply_proves_the_actual_red_tile_and_target() {
    let mut events = serde_json::to_value(opening()).unwrap();
    events[1]["tehais"][0] =
        json!(["1m", "1m", "1m", "2m", "3m", "4m", "2p", "3p", "4p", "2s", "3s", "4s", "5p"]);
    events[2]["pai"] = json!("5pr");
    let mut bot = standby::LocalStandby::new(0, 4).unwrap();
    for event in serde_json::from_value::<Vec<MjaiEvent>>(events).unwrap() {
        bot.observe(&event).unwrap();
    }
    let legal = bot.legal();
    let reply = |pai: &str, target: u8| {
        serde_json::from_value::<BotResponse>(json!({
            "type":"hora", "actor":0, "target":target, "pai":pai
        }))
        .unwrap()
    };
    assert!(standby::legal_response(&reply("5pr", 0), &legal));
    assert!(!standby::legal_response(&reply("5p", 0), &legal));
    assert!(!standby::legal_response(&reply("5pr", 1), &legal));
    let local = bot.decide().unwrap();
    assert!(standby::legal_response(&local, &legal));
}

#[tokio::test]
async fn transport_restart_replays_the_confirmed_prefix_once() {
    let mut primary = Primary::new(None);
    primary.fail_at = Some(3);
    let calls = primary.calls.clone();
    let mut rig = Rig::new(primary);
    for event in opening() {
        rig.feed(event);
    }
    let reply = rig.reply().await;
    assert_eq!(
        reply.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls[3],
        opening(),
        "restart replays all and only real events"
    );
}

#[test]
fn remaining_budget_deducts_elapsed_time_input_reserve_and_bounded_bank() {
    let now = Instant::now();
    let clock = AutoplayContext::new();
    *clock.time_budget.write().unwrap() = Some(TimeBudget {
        fixed_ms: 6_000,
        add_ms: 10_000,
        opened_at: now - Duration::from_millis(1_000),
        source: BudgetSource::DealTile,
    });
    let mut config = AppConfig::default();
    config.autoplay.delay.safety_margin_ms = 100;
    config.autoplay.input.hover_delay_ms = 20;
    config.autoplay.input.click_hold_ms = 10;
    config.autoplay.input.inter_click_delay_ms = 40;
    let (deadline, reserve, remaining) = inference_deadline(
        clock.time_budget.read().unwrap().map(InputWindow::Majsoul),
        &config,
        now,
    );
    assert_eq!(remaining, 7_500);
    assert_eq!(reserve, 870);
    assert_eq!(deadline.duration_since(now), Duration::from_millis(6_630));
}

#[test]
fn default_fast_input_budget_leaves_inference_time_without_spending_unknown_bank() {
    use crate::autoplay::tenhou_state::{DecisionWindow, TurnClock};
    let now = Instant::now();
    let config = AppConfig::default();
    let window = DecisionWindow {
        ops: 0,
        opened_at: now,
        clock: Some(TurnClock::FAST),
    };
    let (deadline, reserve, remaining) =
        inference_deadline(Some(InputWindow::Tenhou(window)), &config, now);
    assert_eq!((reserve, remaining), (2_350, 3_000));
    assert_eq!(deadline.duration_since(now), Duration::from_millis(650));
    let later = now + Duration::from_millis(200);
    let (deadline, reserve, remaining) =
        inference_deadline(Some(InputWindow::Tenhou(window)), &config, later);
    assert_eq!((reserve, remaining), (2_350, 2_800));
    assert_eq!(deadline.duration_since(later), Duration::from_millis(450));
}

#[tokio::test]
async fn fast_tenhou_with_production_defaults_can_select_the_primary() {
    use crate::autoplay::tenhou_state::{DecisionWindow, TenhouState, TurnClock};
    let mut rig = Rig::with_config(Box::new(Primary::new(None)), AppConfig::default());
    let window = DecisionWindow {
        ops: 0,
        opened_at: Instant::now(),
        clock: Some(TurnClock::FAST),
    };
    *rig.clock.tenhou_state.write().unwrap() = Some(TenhouState {
        window: Some(window),
        ..Default::default()
    });
    for event in opening() {
        rig.captured(crate::event_bus::CapturedEvent {
            event,
            window: Some(InputWindow::Tenhou(window)),
        });
    }
    let reply = rig.reply().await;
    assert_eq!(
        reply.control().unwrap().unwrap().source,
        DecisionSource::Primary
    );
    assert_eq!(
        reply.request.unwrap().window,
        Some(InputWindow::Tenhou(window))
    );
    assert_eq!(
        reply.meta.unwrap()["recovery_timing"]["input_reserve_ms"],
        2_350
    );
}

#[test]
fn local_standby_has_no_cloud_route_and_rejects_wrong_actor_or_red_identity() {
    let mut bot = standby::LocalStandby::new(0, 4).unwrap();
    for event in opening() {
        bot.observe(&event).unwrap();
    }
    let legal = bot.legal();
    let reply = bot.decide().unwrap();
    assert!(standby::legal_response(&reply, &legal));
    let mut wrong = reply.clone();
    if let MjaiEvent::Dahai { actor, .. } = &mut wrong.action {
        *actor = 1;
    }
    assert!(!standby::legal_response(&wrong, &legal));
    let red = BotResponse {
        request: None,
        action: MjaiEvent::Dahai {
            actor: 0,
            pai: "5pr".into(),
            tsumogiri: false,
        },
        meta: None,
    };
    assert!(standby::legal_response(&red, &legal));
    let normal = BotResponse {
        request: None,
        action: MjaiEvent::Dahai {
            actor: 0,
            pai: "5p".into(),
            tsumogiri: false,
        },
        meta: None,
    };
    assert!(!standby::legal_response(&normal, &legal));
}
