//! Optional display prediction has a separate engine connection, owner and
//! latest-only mailbox. It cannot publish an action or delay primary event
//! delivery.

#[cfg(test)]
pub(crate) mod tests;

use crate::autoplay::pointer::lean::{self, Lean};
use crate::bot::BotResponse;
use crate::config::AppConfig;
use crate::engine::EndpointFile;
use crate::event_bus::NotifyBus;
use crate::monitor::DecisionContext;
use crate::schema::Notification;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{watch, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRef {
    pub session: String,
    pub through: u64,
    pub position: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastKey {
    pub owner: u64,
    pub context: DecisionContext,
    pub snapshot: SnapshotRef,
}

#[derive(Debug)]
enum OwnerState {
    Open {
        context: Option<DecisionContext>,
        requested: bool,
        key: Option<ForecastKey>,
    },
    Closed,
}

#[derive(Debug)]
struct Scope {
    state: Mutex<OwnerState>,
}

impl Scope {
    fn current(&self, key: &ForecastKey) -> bool {
        matches!(&*self.state.lock().unwrap(), OwnerState::Open { requested: true, key: Some(current), .. } if current == key)
    }
}

/// This internal result is never deserialized from a bot or sent as a
/// BotResponse. Queued results lose permission when their owner/context ends.
#[derive(Debug, Clone)]
pub struct ForecastLean {
    pub key: ForecastKey,
    pub lean: Lean,
    scope: Weak<Scope>,
}

impl ForecastLean {
    pub fn is_current(&self, context: Option<DecisionContext>) -> bool {
        context == Some(self.key.context)
            && self.scope.upgrade().is_some_and(|s| s.current(&self.key))
    }
    pub fn is_live(&self) -> bool {
        self.scope.upgrade().is_some_and(|s| s.current(&self.key))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ForecastPhase {
    /// No game owns a forecast worker. This is not the saved preference.
    Idle,
    Disabled,
    Unsupported {
        reason: String,
    },
    Waiting,
    Running {
        context: DecisionContext,
        through: u64,
        position: u64,
    },
    Ready {
        context: DecisionContext,
        through: u64,
        position: u64,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForecastStatus {
    pub owner: u64,
    #[serde(flatten)]
    pub phase: ForecastPhase,
}

fn statuses() -> &'static watch::Sender<ForecastStatus> {
    static STATUS: OnceLock<watch::Sender<ForecastStatus>> = OnceLock::new();
    STATUS.get_or_init(|| {
        watch::channel(ForecastStatus {
            owner: 0,
            phase: ForecastPhase::Idle,
        })
        .0
    })
}

pub fn subscribe_status() -> watch::Receiver<ForecastStatus> {
    statuses().subscribe()
}
pub fn status() -> ForecastStatus {
    statuses().borrow().clone()
}

fn set_status(owner: u64, phase: ForecastPhase) {
    statuses().send_if_modified(|old| {
        if old.owner > owner || (old.owner == owner && old.phase == phase) {
            return false;
        }
        *old = ForecastStatus { owner, phase };
        true
    });
}

#[derive(Clone)]
pub struct ForecastInput {
    owner: u64,
    scope: Arc<Scope>,
    jobs: watch::Sender<Option<ForecastKey>>,
    unsupported: Option<String>,
    notify: NotifyBus,
    name: String,
}

impl ForecastInput {
    pub fn advance(&self, context: DecisionContext, requested: bool) {
        let mut state = self.scope.state.lock().unwrap();
        if let OwnerState::Open {
            context: old,
            requested: wanted,
            ..
        } = &*state
        {
            if *old == Some(context) && *wanted == requested {
                return;
            }
            *state = OwnerState::Open {
                context: Some(context),
                requested,
                key: None,
            };
            self.jobs.send_replace(None);
            set_status(self.owner, self.idle_phase(requested));
        }
    }
    fn idle_phase(&self, requested: bool) -> ForecastPhase {
        if !requested {
            ForecastPhase::Disabled
        } else if let Some(reason) = &self.unsupported {
            ForecastPhase::Unsupported {
                reason: reason.clone(),
            }
        } else {
            ForecastPhase::Waiting
        }
    }
    /// Formal restart invalidates references but never retries forecast as a
    /// formal request. A new primary session must supply a new reference.
    pub fn revoke(&self) {
        let mut state = self.scope.state.lock().unwrap();
        if let OwnerState::Open { key, requested, .. } = &mut *state {
            *key = None;
            self.jobs.send_replace(None);
            set_status(self.owner, self.idle_phase(*requested));
        }
    }
    /// Optional snapshot capture failure is consumed only after the formal
    /// receipt was validated. It never changes the formal action or health.
    pub fn observe(
        &self,
        context: DecisionContext,
        reply: &mut BotResponse,
        expected: u64,
        decide: bool,
    ) {
        let failure = reply
            .meta
            .as_mut()
            .and_then(|meta| meta.as_object_mut())
            .and_then(|meta| meta.remove("forecast_error"));
        if let Some(failure) = failure {
            let reason = failure
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("invalid forecast snapshot error")
                .to_owned();
            let mut state = self.scope.state.lock().unwrap();
            if let OwnerState::Open {
                context: Some(current),
                requested: true,
                key,
            } = &mut *state
            {
                if *current == context {
                    *key = None;
                    self.jobs.send_replace(None);
                    set_status(
                        self.owner,
                        ForecastPhase::Unavailable {
                            reason: reason.clone(),
                        },
                    );
                    drop(state);
                    self.notify_unavailable(reason);
                }
            }
            return;
        }
        if !decide {
            self.offer(context, reply, expected);
        }
    }

    fn offer(&self, context: DecisionContext, reply: &BotResponse, expected: u64) {
        if self.unsupported.is_some() {
            return;
        }
        let parsed = (|| -> Result<Option<SnapshotRef>> {
            let Some(raw) = reply.meta.as_ref().and_then(|m| m.get("forecast")) else {
                return Ok(None);
            };
            let reference: SnapshotRef = serde_json::from_value(raw.clone())
                .map_err(|_| anyhow::anyhow!("invalid forecast reference"))?;
            let acknowledged = reply
                .meta
                .as_ref()
                .and_then(|m| m.get("delivery"))
                .and_then(|m| m.get("through"))
                .and_then(|n| n.as_u64());
            if reference.session.is_empty()
                || reference.through != expected
                || acknowledged != Some(expected)
            {
                bail!("forecast reference did not match the acknowledged real event prefix");
            }
            Ok(Some(reference))
        })();
        let mut state = self.scope.state.lock().unwrap();
        let OwnerState::Open {
            context: Some(current),
            requested: true,
            key,
        } = &mut *state
        else {
            return;
        };
        if *current != context {
            return;
        }
        match parsed {
            Ok(Some(snapshot)) => {
                let next = ForecastKey {
                    owner: self.owner,
                    context,
                    snapshot,
                };
                if key.as_ref() != Some(&next) {
                    *key = Some(next.clone());
                    self.jobs.send_replace(Some(next));
                }
            }
            Ok(None) => {}
            Err(error) => {
                *key = None;
                self.jobs.send_replace(None);
                let reason = format!("{error:#}");
                set_status(
                    self.owner,
                    ForecastPhase::Unavailable {
                        reason: reason.clone(),
                    },
                );
                drop(state);
                self.notify_unavailable(reason);
            }
        }
    }
    fn notify_unavailable(&self, reason: String) {
        tracing::warn!(owner = self.owner, %reason, "optional forecast unavailable");
        let _ = self.notify.send(
            Notification::warn("预推理暂不可用")
                .body(reason)
                .id(format!("{}-forecast", self.name)),
        );
    }

    fn phase_for_key(&self, expected: &ForecastKey, phase: ForecastPhase, clear: bool) -> bool {
        let mut state = self.scope.state.lock().unwrap();
        let OwnerState::Open {
            requested: true,
            key: Some(current),
            ..
        } = &*state
        else {
            return false;
        };
        if current != expected {
            return false;
        }
        if clear {
            if let OwnerState::Open { key, .. } = &mut *state {
                *key = None;
            }
            self.jobs.send_replace(None);
        }
        set_status(self.owner, phase);
        true
    }

    fn revoke_key(&self, key: &ForecastKey) {
        self.phase_for_key(key, self.idle_phase(true), true);
    }

    fn fail_key(&self, key: &ForecastKey, reason: String) {
        if self.phase_for_key(
            key,
            ForecastPhase::Unavailable {
                reason: reason.clone(),
            },
            true,
        ) {
            self.notify_unavailable(reason);
        }
    }

    fn publish(&self, key: &ForecastKey, payload: Lean) {
        let state = self.scope.state.lock().unwrap();
        if !matches!(&*state, OwnerState::Open { requested: true, key: Some(current), .. } if current == key)
        {
            return;
        }
        lean::publish(ForecastLean {
            key: key.clone(),
            lean: payload,
            scope: Arc::downgrade(&self.scope),
        });
        set_status(
            self.owner,
            ForecastPhase::Ready {
                context: key.context,
                through: key.snapshot.through,
                position: key.snapshot.position,
            },
        );
    }
}

pub struct ForecastWorker {
    pub input: ForecastInput,
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ForecastWorker {
    /// `endpoint` is the engine whose snapshots this owner may ask about;
    /// any other bot has no forecast connection.
    pub fn spawn(
        endpoint: Option<EndpointFile>,
        config: Arc<RwLock<AppConfig>>,
        notify: NotifyBus,
        name: String,
    ) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let owner = NEXT.fetch_add(1, Ordering::Relaxed);
        let unsupported = endpoint
            .is_none()
            .then(|| "当前 Bot 不支持独立预推理通道。".to_owned());
        let (jobs, receiver) = watch::channel(None);
        let (stop, stopping) = watch::channel(false);
        let input = ForecastInput {
            owner,
            scope: Arc::new(Scope {
                state: Mutex::new(OwnerState::Open {
                    context: None,
                    requested: false,
                    key: None,
                }),
            }),
            jobs,
            unsupported,
            notify,
            name,
        };
        let task = endpoint
            .map(|endpoint| tokio::spawn(run(endpoint, input.clone(), receiver, stopping, config)));
        Self { input, stop, task }
    }
    pub fn stop(&self) {
        let mut state = self.input.scope.state.lock().unwrap();
        *state = OwnerState::Closed;
        self.input.jobs.send_replace(None);
        self.stop.send_replace(true);
        set_status(self.input.owner, ForecastPhase::Idle);
    }
    pub async fn close(mut self) {
        self.stop();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ForecastWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

enum WireReply {
    Ready(Lean),
    Obsolete,
    Unavailable(String),
}

impl WireReply {
    fn parse(line: &[u8], expected: &SnapshotRef) -> Result<Self> {
        let mut fields: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(line).map_err(|_| anyhow::anyhow!("invalid forecast result"))?;
        if fields
            .remove("type")
            .as_ref()
            .and_then(|value| value.as_str())
            != Some("forecast")
        {
            bail!("forecast result omitted its forecast type");
        }
        let status = fields.remove("status");
        let result = match status.as_ref().and_then(|value| value.as_str()) {
            Some("ready") => Self::Ready(
                serde_json::from_value(
                    fields
                        .remove("lean")
                        .context("forecast ready result omitted its lean payload")?,
                )
                .map_err(|_| anyhow::anyhow!("invalid forecast lean payload"))?,
            ),
            Some("obsolete") => Self::Obsolete,
            Some("unavailable") => {
                let reason = fields.remove("reason");
                let reason = reason
                    .as_ref()
                    .and_then(|value| value.as_str())
                    .filter(|reason| !reason.is_empty())
                    .context("forecast unavailable result omitted its reason")?;
                Self::Unavailable(reason.to_owned())
            }
            _ => bail!("forecast result omitted a valid status"),
        };
        // After the exact status payload is consumed, only identity may
        // remain. This rejects unknown fields, actions and fields belonging
        // to another status, including explicitly supplied null values.
        let reference: SnapshotRef = serde_json::from_value(serde_json::Value::Object(fields))
            .map_err(|_| {
                anyhow::anyhow!(
                    "forecast result contained an invalid identity or unexpected fields"
                )
            })?;
        if &reference != expected {
            bail!("forecast result identity did not match its original request");
        }
        Ok(result)
    }
}

/// One forecast connection to the engine, opened when first needed and
/// dropped on any transport error. Its own requests carry the endpoint's
/// token; an engine restart publishes a new one.
struct Socket {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    token: String,
    line: Vec<u8>,
}

impl Socket {
    async fn connect(endpoint: &EndpointFile) -> Result<Self> {
        let published = endpoint.read()?;
        let address = format!("{}:{}", published.host, published.port);
        let stream = tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(&address))
            .await
            .map_err(|_| anyhow::anyhow!("connecting to the engine timed out"))?
            .with_context(|| format!("connect to the engine at {address}"))?;
        let _ = stream.set_nodelay(true);
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            token: published.token,
            line: vec![],
        })
    }
    async fn ask(&mut self, key: &ForecastKey) -> Result<WireReply> {
        let request = serde_json::json!({
            "type": "forecast",
            "token": self.token,
            "session": key.snapshot.session,
            "through": key.snapshot.through,
            "position": key.snapshot.position,
        });
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await?;
        self.writer.flush().await?;
        self.line.clear();
        if self.reader.read_until(b'\n', &mut self.line).await? == 0 {
            bail!("the engine closed the forecast connection without a result");
        }
        WireReply::parse(&self.line, &key.snapshot)
    }
}

async fn run(
    endpoint: EndpointFile,
    input: ForecastInput,
    mut jobs: watch::Receiver<Option<ForecastKey>>,
    mut stop: watch::Receiver<bool>,
    config: Arc<RwLock<AppConfig>>,
) {
    let mut socket: Option<Socket> = None;
    loop {
        tokio::select! {
            biased;
            _ = super::lifecycle::cancelled(&mut stop) => break,
            changed = jobs.changed() => if changed.is_err() { break; },
        }
        let job = jobs.borrow_and_update().clone(); // release watch read lease before every await
        let Some(key) = job else {
            continue;
        };
        if !input.scope.current(&key) {
            continue;
        }
        let enabled = {
            let cfg = config.read().await;
            cfg.monitor.pointer_lean && cfg.autoplay.enabled
        };
        if !enabled {
            input.phase_for_key(&key, ForecastPhase::Disabled, true);
            continue;
        }
        if !input.phase_for_key(
            &key,
            ForecastPhase::Running {
                context: key.context,
                through: key.snapshot.through,
                position: key.snapshot.position,
            },
            false,
        ) {
            continue;
        }
        if socket.is_none() {
            let connected = tokio::select! {
                biased;
                _ = super::lifecycle::cancelled(&mut stop) => break,
                connected = Socket::connect(&endpoint) => connected,
            };
            match connected {
                Ok(connected) => socket = Some(connected),
                Err(error) => {
                    input.fail_key(&key, format!("{error:#}"));
                    continue;
                }
            }
        }
        let result = tokio::select! {
            biased;
            _ = super::lifecycle::cancelled(&mut stop) => break,
            result = tokio::time::timeout(Duration::from_secs(30), socket.as_mut().unwrap().ask(&key)) => {
                result.map_err(|_| anyhow::anyhow!("optional forecast connection exceeded its transport watchdog")).and_then(|r| r)
            }
        };
        match result {
            Err(error) => {
                socket = None;
                input.fail_key(&key, format!("{error:#}"));
            }
            Ok(reply) => {
                let enabled = {
                    let cfg = config.read().await;
                    cfg.monitor.pointer_lean && cfg.autoplay.enabled
                };
                if !enabled {
                    input.phase_for_key(&key, ForecastPhase::Disabled, true);
                    continue;
                }
                if !input.scope.current(&key) {
                    continue;
                }
                match reply {
                    WireReply::Ready(payload) => input.publish(&key, payload),
                    WireReply::Obsolete => input.revoke_key(&key),
                    WireReply::Unavailable(reason) => input.fail_key(&key, reason),
                }
            }
        }
    }
    drop(socket);
}
