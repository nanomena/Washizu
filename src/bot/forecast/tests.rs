use super::*;
use crate::autoplay::pointer::{lean::Spot, PointerController, PointerLayout, TileTarget};
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

pub(crate) fn context(seq: u64) -> DecisionContext {
    DecisionContext {
        game: 1,
        round: 1,
        event_seq: seq,
    }
}

pub(crate) fn receipt(through: u64, session: &str) -> BotResponse {
    BotResponse {
        request: None,
        action: crate::schema::MjaiEvent::None,
        meta: Some(
            json!({"delivery":{"through":through}, "forecast":{"session":session,"through":through,"position":through}}),
        ),
    }
}

pub(crate) async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scripted relay reached its barrier");
}

/// A stdlib-only relay with an observable barrier, not a model/service.
pub(crate) struct Relay {
    pub files: TempDir,
    pub worker: ForecastWorker,
}

impl Relay {
    pub fn new(first_fault: bool) -> Self {
        let files = tempfile::tempdir().unwrap();
        std::fs::write(files.path().join("relay.py"), r#"import json, os, pathlib, sys, time
root = pathlib.Path('.')
(root / 'pid').write_text(str(os.getpid()))
for line in sys.stdin:
    key = json.loads(line)
    with (root / 'requests').open('a') as out:
        out.write(json.dumps(key) + '\n')
    first = not (root / 'entered').exists()
    if first:
        (root / 'entered').touch()
        while not (root / 'release').exists():
            time.sleep(0.01)
    if first and os.environ['FIRST_FAULT'] == '1':
        key['session'] = 'wrong-session'
    print(json.dumps(dict(type='forecast', **key, status='ready', lean=dict(lines=12, options=[dict(discard=True, pai='6s', share=1.0)]))), flush=True)
"#).unwrap();
        let python = std::env::var_os("AKAGI_TEST_PYTHON")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                which::which("python3")
                    .or_else(|_| which::which("python"))
                    .ok()
            })
            .expect("forecast subprocess tests require a Python interpreter");
        let mut command = Command::new(python);
        command
            .current_dir(files.path())
            .args(["-u", "relay.py"])
            .env("FIRST_FAULT", if first_fault { "1" } else { "0" });
        let mut config = AppConfig::default();
        config.monitor.pointer_lean = true;
        config.autoplay.enabled = true;
        let worker = ForecastWorker::spawn(
            Ok(Some(command)),
            Arc::new(RwLock::new(config)),
            crate::event_bus::notify_bus(),
            "scripted".into(),
        );
        Self { files, worker }
    }
    pub fn submit(&self, seq: u64, session: &str) {
        self.worker.input.advance(context(seq), true);
        self.worker
            .input
            .offer(context(seq), &receipt(seq, session), seq);
    }
    pub async fn entered(&self) {
        wait_file(&self.files.path().join("entered")).await;
    }
    pub fn release(&self) {
        std::fs::write(self.files.path().join("release"), "").unwrap();
    }
    pub fn positions(&self) -> Vec<u64> {
        std::fs::read_to_string(self.files.path().join("requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<SnapshotRef>(line).unwrap().position)
            .collect()
    }
    pub async fn result(
        &self,
        rx: &mut tokio::sync::broadcast::Receiver<ForecastLean>,
    ) -> ForecastLean {
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                if let Ok(value) = rx.recv().await {
                    if value.key.owner == self.worker.input.owner {
                        return value;
                    }
                }
            }
        })
        .await
        .expect("current forecast published")
    }
}

#[tokio::test]
async fn optional_snapshot_capture_error_is_reported_without_changing_the_formal_reply() {
    let relay = Relay::new(false);
    let mut notifications = relay.worker.input.notify.subscribe();
    relay.worker.input.advance(context(1), true);
    let mut reply = receipt(1, "session-a");
    reply.action = crate::schema::MjaiEvent::Dahai {
        actor: 0,
        pai: "6s".into(),
        tsumogiri: false,
    };
    let action = reply.action.clone();
    reply.meta.as_mut().unwrap()["forecast_error"] = json!("snapshot capture failed");
    relay.worker.input.observe(context(1), &mut reply, 1, true);
    assert_eq!(reply.action, action);
    assert_eq!(reply.meta.as_ref().unwrap()["delivery"]["through"], 1);
    assert!(reply.unavailable_reason().is_none());
    assert!(reply.meta.as_ref().unwrap().get("forecast_error").is_none());
    assert_eq!(
        notifications.try_recv().unwrap().body.as_deref(),
        Some("snapshot capture failed")
    );
    assert!(!relay.files.path().join("pid").exists());
    relay.worker.close().await;
}

#[test]
fn forecast_wire_requires_exact_identity_and_the_payload_for_its_status() {
    let identity = json!({"session":"opaque-session", "through":3, "position":2});
    let reference: SnapshotRef = serde_json::from_value(identity.clone()).unwrap();
    let mut unknown = identity.clone();
    unknown["extra"] = json!(true);
    assert!(serde_json::from_value::<SnapshotRef>(unknown).is_err());
    let parse = |value: serde_json::Value| {
        WireReply::parse(&serde_json::to_vec(&value).unwrap(), &reference)
    };
    let base = json!({"type":"forecast", "session":"opaque-session", "through":3, "position":2});
    let mut ready = base.clone();
    ready["status"] = json!("ready");
    ready["lean"] = json!({"lines":12, "options":[]});
    assert!(matches!(parse(ready.clone()), Ok(WireReply::Ready(_))));
    let mut obsolete = base.clone();
    obsolete["status"] = json!("obsolete");
    assert!(matches!(parse(obsolete.clone()), Ok(WireReply::Obsolete)));
    let mut unavailable = base;
    unavailable["status"] = json!("unavailable");
    unavailable["reason"] = json!("service offline");
    assert!(matches!(
        parse(unavailable.clone()),
        Ok(WireReply::Unavailable(_))
    ));
    for (valid, field, value) in [
        (ready.clone(), "reason", json!(null)),
        (
            ready.clone(),
            "action",
            json!({"type":"dahai","actor":0,"pai":"6s","tsumogiri":false}),
        ),
        (obsolete.clone(), "lean", json!(null)),
        (obsolete, "reason", json!("stale")),
        (
            unavailable.clone(),
            "lean",
            json!({"lines":12,"options":[]}),
        ),
        (ready.clone(), "through", json!(4)),
        (ready.clone(), "session", json!("other-session")),
        (ready.clone(), "position", json!(3)),
        (ready.clone(), "type", json!("none")),
        (ready.clone(), "unknown", json!(true)),
    ] {
        let mut invalid = valid;
        invalid[field] = value;
        assert!(parse(invalid).is_err(), "invalid field {field} is rejected");
    }
    ready.as_object_mut().unwrap().remove("lean");
    unavailable.as_object_mut().unwrap().remove("reason");
    assert!(parse(ready).is_err());
    assert!(parse(unavailable).is_err());
}

#[tokio::test]
async fn latest_pending_replaces_intermediate_jobs_and_old_stdout_is_drained() {
    let relay = Relay::new(false);
    let mut rx = lean::subscribe();
    relay.submit(1, "session-a");
    relay.entered().await;
    relay.submit(2, "session-a");
    relay.submit(3, "session-a");
    relay.release();
    let result = relay.result(&mut rx).await;
    assert_eq!(
        result.key.context,
        context(3),
        "late first result never published"
    );
    assert_eq!(
        relay.positions(),
        vec![1, 3],
        "one in flight and only the latest pending request"
    );
    assert!(result.is_current(Some(context(3))));
    assert_eq!(result.lean.first(), Some(Spot::Hand("6s".into())));
    relay.worker.close().await;
    assert!(
        !result.is_live(),
        "already queued results lose permission on owner close"
    );
}

#[tokio::test]
async fn obsolete_transport_error_cannot_clear_a_newer_pending_job() {
    let relay = Relay::new(true);
    let mut rx = lean::subscribe();
    relay.submit(1, "session-a");
    relay.entered().await;
    relay.submit(2, "session-b");
    relay.release();
    let result = relay.result(&mut rx).await;
    assert_eq!(result.key.snapshot.session, "session-b");
    assert_eq!(result.key.context, context(2));
    assert_eq!(relay.positions(), vec![1, 2]);
    relay.worker.close().await;
}

#[tokio::test]
async fn reset_revokes_old_session_even_when_the_new_snapshot_has_the_same_counts() {
    let relay = Relay::new(false);
    let mut rx = lean::subscribe();
    relay.submit(1, "before-reset");
    relay.entered().await;
    relay.worker.input.revoke();
    relay.submit(1, "after-reset");
    relay.release();
    let result = relay.result(&mut rx).await;
    assert_eq!(result.key.snapshot.session, "after-reset");
    assert_eq!(relay.positions(), vec![1, 1]);
    relay.worker.close().await;
}

#[tokio::test]
async fn forecast_reference_requires_the_validated_formal_receipt() {
    let relay = Relay::new(false);
    relay.worker.input.advance(context(1), true);
    let mut bad = receipt(1, "session-a");
    bad.meta.as_mut().unwrap()["forecast"]["through"] = json!(0);
    relay.worker.input.offer(context(1), &bad, 1);
    assert!(matches!(
        *relay.worker.input.scope.state.lock().unwrap(),
        OwnerState::Open { key: None, .. }
    ));
    assert!(
        !relay.files.path().join("pid").exists(),
        "invalid reference cannot start optional work"
    );
    relay.submit(1, "session-a");
    relay.entered().await;
    relay.release();
    relay.worker.close().await;
}

#[tokio::test]
async fn a_received_forecast_loses_pointer_permission_on_context_change() {
    let relay = Relay::new(false);
    let mut rx = lean::subscribe();
    relay.submit(1, "session-a");
    relay.entered().await;
    relay.release();
    let result = relay.result(&mut rx).await;
    let mut hand = PointerController::new(Arc::new(crate::autoplay::AutoplayContext::new()));
    hand.take_lean(result.clone());
    assert!(hand.forecast_current(Some(context(1))));
    let rect = crate::autoplay::context::CanvasRect {
        x: 0.0,
        y: 0.0,
        width: 1600.0,
        height: 900.0,
    };
    assert!(hand.set_layout(PointerLayout {
        viewport: rect,
        tiles: vec![TileTarget { tile: "6s".into(), drawn: false,
            bounds: crate::autoplay::context::CanvasRect { x: 500.0, y: 800.0, width: 60.0, height: 80.0 } }],
        next_draw: None,
        rest: crate::autoplay::context::CanvasRect { x: 750.0, y: 700.0, width: 40.0, height: 20.0 },
    }));
    hand.look_at(Spot::Hand("6s".into()));
    assert!(
        hand.motion_due().is_some(),
        "the live forecast queued a look"
    );
    relay.worker.input.advance(context(2), true);
    hand.step().await;
    assert!(
        hand.motion_due().is_none(),
        "queued obsolete movement is cancelled before execution"
    );
    hand.retain_current_forecast(Some(context(2)));
    assert!(!result.is_live());
    assert!(!hand.forecast_current(Some(context(2))));
    hand.look_at(Spot::Hand("6s".into()));
    assert!(
        hand.motion_due().is_none(),
        "stale forecast cannot queue pointer movement"
    );
    assert!(hand.next_glance().is_none());
    relay.worker.close().await;
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn owner_close_reaps_only_its_blocked_relay_and_cancels_pending_work() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let first = Relay::new(false);
    let other = Relay::new(false);
    let mut rx = lean::subscribe();
    first.submit(1, "first");
    other.submit(1, "other");
    first.entered().await;
    other.entered().await;
    first.submit(2, "first");
    let pid = std::fs::read_to_string(first.files.path().join("pid"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let handle = unsafe { OpenProcess(0x00100000, 0, pid) };
    assert!(!handle.is_null());
    drop(first.worker);
    tokio::time::timeout(Duration::from_secs(6), async {
        while unsafe { WaitForSingleObject(handle, 0) } != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropping the owner reaps its process");
    assert_eq!(
        unsafe { WaitForSingleObject(handle, 0) },
        0,
        "close waits for the owned process to exit"
    );
    unsafe {
        CloseHandle(handle);
    }
    let positions: Vec<SnapshotRef> = std::fs::read_to_string(first.files.path().join("requests"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(positions.len(), 1, "pending work was cancelled");
    other.release();
    assert!(
        other.result(&mut rx).await.is_current(Some(context(1))),
        "the other owner's process remains usable"
    );
    other.worker.close().await;
}
