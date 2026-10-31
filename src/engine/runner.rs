//! One game seat's decision connection to the shared engine.
//!
//! The runner keeps the game's whole real event stream and the position the
//! engine has committed through. Every batch extends the stream; the runner
//! sends only the suffix the engine has not committed, so a reconnect — to the
//! same engine or to a restarted one — replays the full real prefix from 0 on
//! a fresh session. A reply that comes too late keeps the connection: the
//! engine still answers it, and that answer is read and dropped (its toasts
//! kept) before the next request, so a late answer can never be taken for a
//! later request's. Each reply's receipt must name exactly the requested
//! range; `meta.delivery.through` tells the coordinator what was committed.

use super::{EndpointFile, Package, PROTOCOL};
use crate::bot::{BotResponse, BotRunner};
use crate::event_bus::NotifyBus;
use crate::schema::{MjaiEvent, Notification, NotifyLevel};
use anyhow::{bail, Context as _, Result};
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tracing::warn;

/// How long one answer is waited for. A slow answer is still read and
/// dropped before the next request.
pub const ANSWER_WAIT: Duration = Duration::from_secs(20);
/// How long a connection attempt may take.
const CONNECT_WAIT: Duration = Duration::from_secs(1);
const UNAVAILABLE: &str = "The engine is unavailable.";

/// What a game seat tells the engine when it greets it.
#[derive(Debug, Clone)]
pub struct Greeting {
    pub account: String,
    pub seat: u8,
    /// Whether forecast snapshots are wanted on this session.
    pub lean: bool,
    /// The public table facts observed before the game.
    pub game: Option<Value>,
}

struct Connection {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// What has arrived past the last complete line. A read cut short by
    /// the answer wait leaves its bytes here for the next read.
    line: Vec<u8>,
    token: String,
}

pub struct EngineRunner {
    endpoint: EndpointFile,
    name: String,
    display: String,
    greeting: Greeting,
    notify: NotifyBus,
    wait: Duration,
    connection: Option<Connection>,
    /// Every real event of this game, as sent.
    events: Vec<Value>,
    /// Events the engine has committed on this connection.
    cursor: usize,
    /// The exact range of each request awaiting its reply (`None` for a
    /// greeting).
    owed: VecDeque<Option<(usize, usize)>>,
    /// The unreachable toast was shown; shown once until a connection works.
    told: bool,
}

impl EngineRunner {
    pub fn new(package: &Package, greeting: Greeting, notify: NotifyBus) -> Self {
        Self {
            endpoint: package.endpoint(),
            name: package.name.clone(),
            display: package.display.clone(),
            greeting,
            notify,
            wait: ANSWER_WAIT,
            connection: None,
            events: Vec::new(),
            cursor: 0,
            owed: VecDeque::new(),
            told: false,
        }
    }

    /// Override the answer wait. For tests.
    pub fn set_wait(&mut self, wait: Duration) {
        self.wait = wait;
    }

    fn toast(&self, notification: Notification) {
        let _ = self
            .notify
            .send(notification.id(format!("engine-{}", self.name)));
    }

    fn close(&mut self) {
        self.connection = None;
        self.owed.clear();
        self.cursor = 0;
    }

    fn unavailable(&self, reason: &str) -> BotResponse {
        self.respond(json!({"type": "none", "meta": {"unavailable": reason}}))
            .expect("an unavailable answer is a valid response")
    }

    /// The reply as the coordinator reads it: with the committed prefix.
    fn respond(&self, mut reply: Value) -> Result<BotResponse> {
        let object = reply
            .as_object_mut()
            .context("the engine's reply is not an object")?;
        let meta = object
            .entry("meta")
            .or_insert_with(|| Value::Object(Map::new()));
        let meta = meta
            .as_object_mut()
            .context("the engine's reply meta is not an object")?;
        meta.insert("delivery".into(), json!({ "through": self.cursor }));
        // The card goes by the package's own name.
        if let Some(title) = meta.get_mut("show").and_then(|show| show.get_mut("title")) {
            *title = Value::String(self.display.clone());
        }
        serde_json::from_value(reply).context("the engine's reply is not an mjai reaction")
    }

    fn relay_notices(&self, reply: &mut Value) {
        let Some(notices) = reply.as_object_mut().and_then(|o| o.remove("notify")) else {
            return;
        };
        for notice in notices.as_array().into_iter().flatten() {
            let level = notice
                .get("level")
                .cloned()
                .and_then(|level| serde_json::from_value::<NotifyLevel>(level).ok())
                .unwrap_or(NotifyLevel::Info);
            let mut notification = Notification::info(
                notice
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.display)
                    .to_owned(),
            );
            notification.level = level;
            if let Some(body) = notice.get("body").and_then(Value::as_str) {
                notification = notification.body(body);
            }
            let id = notice
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("engine-{}", self.name));
            let _ = self.notify.send(notification.id(id));
        }
    }

    async fn send(&mut self, request: &Value, span: Option<(usize, usize)>) -> Result<()> {
        let connection = self.connection.as_mut().context("not connected")?;
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        connection.writer.write_all(&line).await?;
        connection.writer.flush().await?;
        self.owed.push_back(span);
        Ok(())
    }

    /// The next line of the engine's, before `deadline`. A read cut short
    /// keeps what arrived and its owed range.
    async fn read(&mut self, deadline: Instant) -> Result<ReadOutcome> {
        let connection = self.connection.as_mut().context("not connected")?;
        let read = tokio::time::timeout_at(
            deadline,
            connection.reader.read_until(b'\n', &mut connection.line),
        )
        .await;
        match read {
            Err(_) => return Ok(ReadOutcome::Late),
            Ok(Err(error)) => return Err(error).context("read the engine's reply"),
            Ok(Ok(0)) => bail!("the engine closed the connection"),
            Ok(Ok(_)) if !connection.line.ends_with(b"\n") => {
                bail!("the engine closed the connection mid-reply")
            }
            Ok(Ok(_)) => {}
        }
        let line = std::mem::take(&mut connection.line);
        let span = self
            .owed
            .pop_front()
            .context("the engine replied to nothing")?;
        let mut reply: Value =
            serde_json::from_slice(&line).context("the engine's reply is not JSON")?;
        if let Some((start, end)) = span {
            let receipt = reply
                .as_object_mut()
                .and_then(|o| o.remove("receipt"))
                .context("the engine's reply carries no receipt")?;
            if receipt.get("start").and_then(Value::as_u64) != Some(start as u64)
                || receipt.get("end").and_then(Value::as_u64) != Some(end as u64)
            {
                bail!("the engine replied to a different event range");
            }
            let through = receipt
                .get("through")
                .and_then(Value::as_u64)
                .filter(|through| (start as u64..=end as u64).contains(through))
                .context("the engine returned an invalid committed position")?
                as usize;
            let unavailable = reply.pointer("/meta/unavailable").is_some();
            if through != end && !unavailable {
                bail!("an executable reply did not commit the whole batch");
            }
            self.cursor = through;
        }
        Ok(ReadOutcome::Line(reply))
    }

    /// Connect and greet. Without an engine this says so once and leaves no
    /// connection.
    async fn open(&mut self, deadline: Instant) -> Result<()> {
        let endpoint = self.endpoint.read()?;
        let address = format!("{}:{}", endpoint.host, endpoint.port);
        let connect_by = deadline.min(Instant::now() + CONNECT_WAIT);
        let stream = tokio::time::timeout_at(connect_by, TcpStream::connect(&address))
            .await
            .map_err(|_| anyhow::anyhow!("connecting to {address} timed out"))?
            .with_context(|| format!("connect to {address}"))?;
        let _ = stream.set_nodelay(true);
        let (reader, writer) = stream.into_split();
        self.connection = Some(Connection {
            reader: BufReader::new(reader),
            writer,
            line: Vec::new(),
            token: endpoint.token.clone(),
        });
        self.owed.clear();
        self.cursor = 0;
        let mut hello = json!({
            "type": "hello",
            "protocol": PROTOCOL,
            "token": endpoint.token,
            "account": self.greeting.account,
            "seat": self.greeting.seat,
            "lean": self.greeting.lean,
        });
        if let Some(game) = &self.greeting.game {
            hello["game"] = game.clone();
        }
        self.send(&hello, None).await?;
        let mut ready = match self.read(deadline).await? {
            ReadOutcome::Line(reply) => reply,
            ReadOutcome::Late => bail!("the engine did not answer the greeting in time"),
        };
        self.relay_notices(&mut ready);
        let token = &self.connection.as_ref().expect("connected").token;
        if ready.get("type").and_then(Value::as_str) != Some("ready") {
            let reason = ready
                .pointer("/meta/unavailable")
                .and_then(Value::as_str)
                .unwrap_or("the engine refused the greeting");
            bail!("{}", reason.replace(token.as_str(), "[token]"));
        }
        if ready.get("token").and_then(Value::as_str) != Some(token.as_str()) {
            bail!("the endpoint is answered by a different engine instance");
        }
        self.toast(Notification::success(format!("{} connected", self.display)));
        self.told = false;
        Ok(())
    }

    /// Read the replies to requests already answered `none`, so the next
    /// reply read is the next request's. What they say to do is too late;
    /// what they tell the user is not.
    async fn skip_late_replies(&mut self, deadline: Instant) -> Result<()> {
        while !self.owed.is_empty() {
            match self.read(deadline).await? {
                ReadOutcome::Line(mut reply) => self.relay_notices(&mut reply),
                ReadOutcome::Late => bail!("no reply to two requests in a row"),
            }
        }
        self.toast(
            Notification::success(format!("{} answering again", self.display))
                .body("A late reply was skipped; the game it follows is intact."),
        );
        Ok(())
    }

    async fn ask(&mut self, deadline: Instant) -> Result<ReadOutcome> {
        if !self.owed.is_empty() {
            self.skip_late_replies(deadline).await?;
        }
        if self.connection.is_none() {
            if let Err(error) = self.open(deadline).await {
                self.close();
                if !self.told {
                    self.toast(
                        Notification::error(format!("{} not reachable", self.display))
                            .body(format!(
                                "{error:#}. Washizu runs the engine while an account uses it; its state says why it is not up."
                            ))
                            .sticky(),
                    );
                    self.told = true;
                }
                return Ok(ReadOutcome::Late);
            }
        }
        let (start, end) = (self.cursor, self.events.len());
        let request = json!({
            "type": "events",
            "start": start,
            "events": &self.events[start..end],
        });
        self.send(&request, Some((start, end))).await?;
        self.read(deadline).await
    }
}

enum ReadOutcome {
    Line(Value),
    /// No answer in time; the connection and its owed ranges stay.
    Late,
}

#[async_trait]
impl BotRunner for EngineRunner {
    async fn react(&mut self, events: &[MjaiEvent]) -> Result<BotResponse> {
        let deadline = Instant::now() + self.wait;
        for event in events {
            self.events.push(serde_json::to_value(event)?);
        }
        let ends = events
            .iter()
            .any(|e| matches!(e, MjaiEvent::EndGame { .. }));
        let response = match self.ask(deadline).await {
            Ok(ReadOutcome::Line(mut reply)) => {
                self.relay_notices(&mut reply);
                match self.respond(reply) {
                    Ok(response) => response,
                    Err(error) => {
                        warn!(engine = %self.name, %error, "engine reply rejected");
                        self.close();
                        self.unavailable(UNAVAILABLE)
                    }
                }
            }
            Ok(ReadOutcome::Late) if self.connection.is_none() => self.unavailable(UNAVAILABLE),
            Ok(ReadOutcome::Late) => {
                // The engine has the batch and will still answer it. The
                // connection stays, and with it the game it has followed.
                self.toast(
                    Notification::error(format!("{} answered too late", self.display))
                        .body("That reply is skipped before the next request."),
                );
                self.unavailable(UNAVAILABLE)
            }
            Err(error) => {
                self.close();
                self.toast(
                    Notification::error(format!("{} connection lost", self.display))
                        .body(format!("{error:#}; reconnecting at the next request.")),
                );
                self.unavailable(UNAVAILABLE)
            }
        };
        if ends {
            self.close();
        }
        Ok(response)
    }

    /// A new game, or a replay of the whole confirmed prefix on a fresh
    /// session.
    async fn reset(&mut self) -> Result<()> {
        self.close();
        self.events.clear();
        self.told = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
