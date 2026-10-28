//! Versioned, read-only observations for the monitoring workspace.
//! No UI subscriber can start a bot or mutate game state through this cache.

use crate::bot::{BotResponse, ServiceStatus};
use crate::game_state::{GameStateSnapshot, MahgenView, MeldSnapshot};
use crate::schema::{
    AccountLevels, BotStatus, CaptureStatus, MjaiEvent, Notification, NotifyLevel,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The name the window goes by: the active bot's display name, as its
/// manifest gives it, or Akagi's own when no bot is selected.
pub fn brand(config: &crate::config::AppConfig) -> String {
    let root = crate::util::resolve_dir(std::path::Path::new(&config.bot.dir));
    crate::bot::BotRegistry::scan(&root)
        .ok()
        .and_then(|registry| registry.find(config.bot.active_for(4)).cloned())
        .map(|bot| {
            bot.manifest
                .and_then(|manifest| manifest.bot.display)
                .unwrap_or(bot.name)
        })
        .unwrap_or_else(|| "Washizu".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContext {
    pub game: u64,
    pub round: u64,
    pub event_seq: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorEvent {
    pub seq: u64,
    pub ts_ms: i64,
    pub event: MjaiEvent,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    /// Which decision this is, for as long as the process runs: what a reply
    /// is matched to. Never shown.
    pub id: u64,
    /// Its place among the decisions of its game, from one. What the monitor
    /// numbers it by when the bot's reply gives no position of its own
    /// (`meta.show.position`).
    pub number: u64,
    pub context: DecisionContext,
    pub started_ms: i64,
    pub completed_ms: Option<i64>,
    pub reaction_ms: Option<u64>,
    pub bot: String,
    pub trigger: MjaiEvent,
    /// thinking / awaiting / confirmed / expired / failed / unavailable / declined
    pub state: String,
    pub response: Option<BotResponse>,
    pub error: Option<String>,
    pub confirmed_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicHand {
    pub seat: u8,
    pub tiles: Vec<String>,
    /// The seat's open melds as the round ended. Kept with the hand because
    /// the table that holds them is dealt again at the next round.
    pub melds: Vec<MeldSnapshot>,
    pub winning_tile: Option<String>,
    pub target: Option<u8>,
    pub ura_markers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoundResult {
    pub kind: String,
    pub hands: Vec<PublicHand>,
    pub scores: Option<Vec<i32>>,
    pub deltas: Option<Vec<i32>>,
}

/// One thing a bot or its service wants the user to know, shown as it is
/// written: the monitor does not interpret it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub name: String,
    pub value: String,
    /// A longer explanation, shown on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The `facts` list of a reply's `meta` or of a service's `ready` line.
/// Anything that is not a well-formed fact is left out.
pub fn facts_of(value: Option<&serde_json::Value>) -> Vec<Fact> {
    value
        .and_then(|v| v.as_array())
        .map(|facts| {
            facts
                .iter()
                .filter_map(|fact| serde_json::from_value(fact.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// One hand of the current game, kept after the next is dealt: which hand it
/// was and how it ended. Its events stay on this side and are asked for by
/// `round` ([`MonitorHandle::hand_events`]): the snapshot is sent whole on
/// every change, and a game's worth of events is not something to send twice.
#[derive(Debug, Clone, Serialize)]
pub struct HandSummary {
    /// Its place among the hands dealt this game, from one.
    pub round: u64,
    pub bakaze: String,
    pub kyoku: u8,
    pub honba: u8,
    /// `None` while it is being played.
    pub result: Option<RoundResult>,
    /// How many events it has had.
    pub events: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorSnapshot {
    pub control: Option<crate::bot::control::DecisionControl>,
    /// Optional pointer prediction; never a decision source or primary health.
    pub forecast: crate::bot::forecast::ForecastStatus,
    pub revision: u64,
    pub context: DecisionContext,
    pub names: Vec<String>,
    pub red_fives: Option<bool>,
    pub game: Option<GameStateSnapshot>,
    pub view: Option<MahgenView>,
    pub round_ended: bool,
    pub game_ended: bool,
    pub capture: CaptureStatus,
    pub bot: BotStatus,
    /// unknown / connected / error; independent of the Bot process state.
    pub service: String,
    pub service_error: Option<String>,
    /// The process behind `service` when the app runs it: whether it is up,
    /// not whether the bot has reached it.
    pub server: ServiceStatus,
    pub sync_error: Option<String>,
    /// Last unresolved sync fault. Unlike the current hand's `sync_error`,
    /// this survives deals/reconnects until a fresh actionable model reply.
    pub sync_fault: Option<String>,
    /// The name the window goes by: the active bot's display name.
    pub brand: String,
    /// What the bot's latest reply said about itself (`meta.facts`).
    pub facts: Vec<Fact>,
    pub autoplay_enabled: bool,
    pub decision: Option<Decision>,
    pub previous_decision: Option<Decision>,
    pub result: Option<RoundResult>,
    pub previous_result: Option<RoundResult>,
    /// The hands of this game in the order they were dealt, the one being
    /// played last. Emptied when another game starts.
    pub hands: Vec<HandSummary>,
    /// The events of the hand being played.
    pub events: VecDeque<MonitorEvent>,
    pub last_event_ms: Option<i64>,
    /// The player's own ranks as the platform last said them; kept across
    /// games. `None` until it has said them.
    pub account: Option<AccountLevels>,
}

impl Default for MonitorSnapshot {
    fn default() -> Self {
        Self {
            control: None,
            forecast: crate::bot::forecast::status(),
            revision: 0,
            context: DecisionContext {
                game: 0,
                round: 0,
                event_seq: 0,
            },
            names: vec![],
            red_fives: None,
            game: None,
            view: None,
            round_ended: true,
            game_ended: true,
            capture: CaptureStatus::Stopped,
            bot: BotStatus::Idle,
            service: "unknown".into(),
            service_error: None,
            server: ServiceStatus::None,
            sync_error: None,
            sync_fault: None,
            brand: "Washizu".into(),
            facts: vec![],
            autoplay_enabled: false,
            decision: None,
            previous_decision: None,
            result: None,
            previous_result: None,
            hands: vec![],
            events: VecDeque::new(),
            last_event_ms: None,
            account: None,
        }
    }
}

struct Inner {
    snapshot: MonitorSnapshot,
    next_decision: u64,
    /// Decisions begun in the current game.
    game_decisions: u64,
    /// What the provider calls the current game, when it says: the same
    /// name on a second `start_game` is a reconnection, not another game.
    game_id: Option<u64>,
    /// The events of each hand in `snapshot.hands`, in the same order.
    hand_events: Vec<Vec<MonitorEvent>>,
}

#[derive(Clone)]
pub struct MonitorHandle {
    inner: Arc<Mutex<Inner>>,
    updates: watch::Sender<MonitorSnapshot>,
}

impl Default for MonitorHandle {
    fn default() -> Self {
        let snapshot = MonitorSnapshot::default();
        let (updates, _) = watch::channel(snapshot.clone());
        Self {
            inner: Arc::new(Mutex::new(Inner {
                snapshot,
                next_decision: 0,
                game_decisions: 0,
                game_id: None,
                hand_events: vec![],
            })),
            updates,
        }
    }
}

impl MonitorHandle {
    pub fn snapshot(&self) -> MonitorSnapshot {
        self.updates.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<MonitorSnapshot> {
        self.updates.subscribe()
    }

    fn update<T>(&self, change: impl FnOnce(&mut Inner) -> T) -> T {
        let mut inner = self.inner.lock().expect("monitor lock poisoned");
        let result = change(&mut inner);
        inner.snapshot.revision += 1;
        self.updates.send_replace(inner.snapshot.clone());
        result
    }

    /// Called by the tracker before it publishes the corresponding TrackedEvent.
    pub fn game_event(
        &self,
        event: &MjaiEvent,
        game: Option<GameStateSnapshot>,
        error: Option<String>,
    ) -> DecisionContext {
        self.update(|inner| {
            if let MjaiEvent::StartGame { game_meta, .. } = event {
                inner.game_decisions = 0;
                // A game that is rejoined keeps the hands it has played.
                let id = game_meta.as_ref().and_then(|meta| meta.game_id);
                if id.is_none() || id != inner.game_id {
                    inner.snapshot.hands.clear();
                    inner.hand_events.clear();
                }
                inner.game_id = id;
            }
            let s = &mut inner.snapshot;
            s.context.event_seq += 1;
            let now = chrono::Local::now().timestamp_millis();
            if let MjaiEvent::StartGame {
                names, aka_flag, ..
            } = event
            {
                s.context.game += 1;
                s.control = None;
                s.context.round = 0;
                s.names = names.clone();
                s.red_fives = *aka_flag;
                s.decision = None;
                s.previous_decision = None;
                s.result = None;
                s.previous_result = None;
                s.events.clear();
                s.game_ended = false;
                s.round_ended = true;
                s.sync_error = None;
                s.service = "unknown".into();
                s.service_error = None;
                s.facts.clear();
            }
            if matches!(event, MjaiEvent::StartKyoku { .. }) {
                expire(&mut s.decision);
                s.previous_decision = s.decision.take().or(s.previous_decision.take());
                s.previous_result = s.result.take().or(s.previous_result.take());
                s.context.round += 1;
                s.round_ended = false;
                s.events.clear();
                s.sync_error = None;
            }
            if let MjaiEvent::StartKyoku {
                bakaze,
                kyoku,
                honba,
                ..
            } = event
            {
                // A hand dealt again before it ended — a rejoined game hands
                // back the one in play — takes the place of its first deal.
                let again = s.hands.last().is_some_and(|hand| {
                    hand.result.is_none()
                        && hand.bakaze == *bakaze
                        && hand.kyoku == *kyoku
                        && hand.honba == *honba
                });
                if again {
                    if let Some(events) = inner.hand_events.last_mut() {
                        events.clear();
                    }
                } else {
                    s.hands.push(HandSummary {
                        round: s.hands.len() as u64 + 1,
                        bakaze: bakaze.clone(),
                        kyoku: *kyoku,
                        honba: *honba,
                        result: None,
                        events: 0,
                    });
                    inner.hand_events.push(vec![]);
                }
            }
            let mut game = game;
            // The monitor is a public view even when an offline replay contains
            // omniscient engine state. Public reveals have their own provenance.
            if let Some(g) = &mut game {
                for player in &mut g.players {
                    if Some(player.seat) != g.our_seat {
                        player.tehai.fill("?".into());
                        player.drawn_tile = None;
                    }
                }
            }
            s.view = game.as_ref().map(MahgenView::from_snapshot);
            s.game = game;
            if let Some(error) = error {
                s.sync_error = Some(error.clone());
                s.sync_fault = Some(error);
                expire(&mut s.decision);
            }
            match event {
                MjaiEvent::Hora {
                    actor,
                    target,
                    tehai,
                    pai,
                    scores,
                    deltas,
                    ura_markers,
                    ..
                } => {
                    let melds = melds_of(s.game.as_ref(), *actor);
                    let result = s.result.get_or_insert_with(|| RoundResult {
                        kind: "hora".into(),
                        hands: vec![],
                        scores: None,
                        deltas: None,
                    });
                    if let Some(tiles) = tehai.as_ref().filter(|t| public_tiles(t)) {
                        if !result.hands.iter().any(|h| h.seat == *actor) {
                            result.hands.push(PublicHand {
                                seat: *actor,
                                tiles: tiles.clone(),
                                melds,
                                winning_tile: pai.clone(),
                                target: Some(*target),
                                ura_markers: ura_markers.clone().unwrap_or_default(),
                            });
                        }
                    }
                    if scores.is_some() {
                        result.scores = scores.clone();
                    }
                    if deltas.is_some() {
                        result.deltas = deltas.clone();
                    }
                    s.round_ended = true;
                }
                MjaiEvent::Ryukyoku {
                    tehais,
                    scores,
                    deltas,
                    reason,
                } => {
                    let hands = tehais
                        .as_ref()
                        .map(|hands| {
                            hands
                                .iter()
                                .enumerate()
                                .filter_map(|(seat, hand)| {
                                    hand.as_ref().filter(|t| public_tiles(t)).map(|tiles| {
                                        PublicHand {
                                            seat: seat as u8,
                                            tiles: tiles.clone(),
                                            melds: melds_of(s.game.as_ref(), seat as u8),
                                            winning_tile: None,
                                            target: None,
                                            ura_markers: vec![],
                                        }
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    s.result = Some(RoundResult {
                        kind: reason.clone().unwrap_or_else(|| "draw".into()),
                        hands,
                        scores: scores.clone(),
                        deltas: deltas.clone(),
                    });
                    s.round_ended = true;
                }
                MjaiEvent::EndKyoku => s.round_ended = true,
                MjaiEvent::EndGame { .. } => {
                    s.game_ended = true;
                    s.round_ended = true;
                }
                _ => {}
            }
            // Settlement scores are authoritative. Do not reapply aggregate
            // deltas on each winner in a multi-ron sequence.
            if let (Some(game), Some(scores)) = (
                &mut s.game,
                s.result.as_ref().and_then(|r| r.scores.as_ref()),
            ) {
                for player in &mut game.players {
                    if let Some(score) = scores.get(player.seat as usize) {
                        player.score = *score;
                    }
                }
            }
            s.last_event_ms = Some(now);
            let mut visible_event = event.clone();
            if let MjaiEvent::StartKyoku { tehais, .. } = &mut visible_event {
                let seat = s.game.as_ref().and_then(|g| g.our_seat);
                for (i, tiles) in tehais.iter_mut().enumerate() {
                    if Some(i as u8) != seat {
                        tiles.fill("?".into());
                    }
                }
            }
            if let MjaiEvent::Tsumo { actor, pai } = &mut visible_event {
                if Some(*actor) != s.game.as_ref().and_then(|g| g.our_seat) {
                    *pai = "?".into();
                }
            }
            let seen = MonitorEvent {
                seq: s.context.event_seq,
                ts_ms: now,
                event: visible_event,
            };
            // The hand's own record: everything from its deal on, and how it
            // ended once it has. A rejoined game's second `start_game` belongs
            // to no hand.
            let rejoined = matches!(event, MjaiEvent::StartGame { .. });
            if let (false, Some(hand), Some(events)) =
                (rejoined, s.hands.last_mut(), inner.hand_events.last_mut())
            {
                events.push(seen.clone());
                hand.events = events.len();
                if s.result.is_some() {
                    hand.result.clone_from(&s.result);
                }
            }
            s.events.push_back(seen);
            // A whole hand, calls and all, stays under this.
            while s.events.len() > 240 {
                s.events.pop_front();
            }
            confirm(s);
            if s.round_ended {
                expire(&mut s.decision);
            }
            s.context
        })
    }

    /// The events of one hand of the current game, by its `round` in
    /// [`MonitorSnapshot::hands`]. `None` for a hand this game has not had.
    pub fn hand_events(&self, round: u64) -> Option<Vec<MonitorEvent>> {
        let inner = self.inner.lock().expect("monitor lock poisoned");
        let index = usize::try_from(round.checked_sub(1)?).ok()?;
        inner.hand_events.get(index).cloned()
    }

    pub fn begin(&self, context: DecisionContext, bot: &str, trigger: &MjaiEvent) -> Option<u64> {
        self.update(|inner| {
            let s = &mut inner.snapshot;
            if !same_round(context, s.context) || s.round_ended || s.sync_error.is_some() {
                return None;
            }
            inner.next_decision += 1;
            inner.game_decisions += 1;
            expire(&mut s.decision);
            s.previous_decision = s.decision.take().or(s.previous_decision.take());
            s.decision = Some(Decision {
                id: inner.next_decision,
                number: inner.game_decisions,
                context,
                bot: bot.into(),
                trigger: trigger.clone(),
                started_ms: chrono::Local::now().timestamp_millis(),
                completed_ms: None,
                reaction_ms: None,
                state: "thinking".into(),
                response: None,
                error: None,
                confirmed_seq: None,
            });
            Some(inner.next_decision)
        })
    }

    pub fn complete(&self, id: u64, response: &BotResponse, reaction_ms: u64) {
        self.update(|inner| {
            let s = &mut inner.snapshot;
            let Some(d) = &mut s.decision else {
                return;
            };
            if d.id != id || !same_round(d.context, s.context) {
                return;
            }
            d.completed_ms = Some(chrono::Local::now().timestamp_millis());
            d.reaction_ms = Some(reaction_ms);
            d.response = Some(response.clone());
            d.error = response.unavailable_reason().map(String::from);
            d.state = if s.round_ended || s.sync_error.is_some() {
                "expired"
            } else if response.unavailable_reason().is_some() {
                "unavailable"
            } else if matches!(response.action, MjaiEvent::None) {
                if response
                    .meta
                    .as_ref()
                    .is_some_and(|m| m.get("show").is_some())
                    || response.control().ok().flatten().is_some_and(|c| {
                        c.source != crate::bot::control::DecisionSource::Unavailable
                    })
                {
                    "declined"
                } else {
                    "unavailable"
                }
            } else {
                "awaiting"
            }
            .into();
            if let Some(meta) = &response.meta {
                // A reply that says nothing about the bot leaves what the last one said.
                if meta.get("facts").is_some() {
                    s.facts = facts_of(meta.get("facts"));
                }
                if meta.get("show").is_some()
                    && response
                        .control()
                        .ok()
                        .flatten()
                        .is_none_or(|c| c.source == crate::bot::control::DecisionSource::Primary)
                {
                    s.service = "connected".into();
                    s.service_error = None;
                }
            }
            if matches!(d.state.as_str(), "awaiting" | "declined")
                && s.service != "error"
                && response
                    .control()
                    .ok()
                    .flatten()
                    .is_none_or(|c| c.source == crate::bot::control::DecisionSource::Primary)
            {
                s.sync_fault = None;
            }
            confirm(s);
        });
    }

    pub fn fail(&self, id: u64, error: String) {
        self.update(|inner| {
            if let Some(d) = &mut inner.snapshot.decision {
                if d.id == id {
                    d.state = "failed".into();
                    d.error = Some(error);
                }
            }
        });
    }

    pub fn set_control(&self, control: crate::bot::control::DecisionControl) {
        self.update(|inner| {
            inner.snapshot.control = Some(control);
        });
    }

    pub fn set_forecast(&self, status: crate::bot::forecast::ForecastStatus) {
        self.update(|inner| inner.snapshot.forecast = status);
    }

    pub fn set_runner_health(
        &self,
        primary: crate::bot::control::RunnerHealth,
        standby: crate::bot::control::RunnerHealth,
    ) {
        self.update(|inner| {
            if let Some(control) = &mut inner.snapshot.control {
                control.primary = primary;
                control.standby = standby;
            }
        });
    }

    pub fn gap(&self, detail: String) {
        self.update(|i| {
            i.snapshot.sync_error = Some(detail.clone());
            i.snapshot.sync_fault = Some(detail);
            expire(&mut i.snapshot.decision);
        });
    }
    pub fn set_capture(&self, capture: CaptureStatus) {
        self.update(|i| {
            if !matches!(capture, CaptureStatus::Running { .. }) {
                expire(&mut i.snapshot.decision);
            }
            i.snapshot.capture = capture;
        });
    }
    pub fn set_bot(&self, bot: BotStatus) {
        self.update(|i| {
            if matches!(
                bot,
                BotStatus::Error { .. } | BotStatus::Stopped { .. } | BotStatus::Idle
            ) {
                expire(&mut i.snapshot.decision);
            }
            i.snapshot.bot = bot;
        });
    }
    pub fn set_autoplay(&self, enabled: bool) {
        self.update(|i| i.snapshot.autoplay_enabled = enabled);
    }
    pub fn set_server(&self, server: ServiceStatus) {
        self.update(|i| i.snapshot.server = server);
    }
    pub fn set_brand(&self, brand: String) {
        self.update(|i| i.snapshot.brand = brand);
    }
    /// The player's own ranks, as the platform has just said them.
    pub fn set_account(&self, levels: AccountLevels) {
        self.update(|i| i.snapshot.account = Some(levels));
    }

    pub fn notification(&self, notice: &Notification) {
        let Some(id) = notice.id.as_deref() else {
            return;
        };
        let bot = match &self.snapshot().bot {
            BotStatus::Ready { bot, .. }
            | BotStatus::Loading { bot, .. }
            | BotStatus::Error { bot, .. }
            | BotStatus::Stopped { bot } => bot.clone(),
            _ => String::new(),
        };
        let suffix = id.strip_prefix(&format!("{bot}-"));
        let Some(suffix) = suffix.filter(|_| !bot.is_empty()) else {
            return;
        };
        self.update(|i| {
            let s = &mut i.snapshot;
            let fault = matches!(notice.level, NotifyLevel::Error | NotifyLevel::Warn);
            // In coordinated mode typed worker health owns primary failure.
            // A primary stderr notice must not expire a valid standby action
            // or become a fault in the independent real-event tracker.
            if s.control.is_some() && ["server", "desync", "unsupported", "error"].contains(&suffix)
            {
                if suffix == "server" {
                    s.service = if fault { "error" } else { "connected" }.into();
                }
                s.service_error =
                    fault.then(|| notice.body.clone().unwrap_or_else(|| notice.title.clone()));
                return;
            }
            if suffix == "server" {
                s.service = if fault { "error" } else { "connected" }.into();
                s.service_error =
                    fault.then(|| notice.body.clone().unwrap_or_else(|| notice.title.clone()));
                if fault {
                    expire(&mut s.decision);
                }
            } else if fault && ["desync", "unsupported", "error"].contains(&suffix) {
                let detail = notice.body.clone().unwrap_or_else(|| notice.title.clone());
                s.sync_error = Some(detail.clone());
                s.sync_fault = Some(detail);
                expire(&mut s.decision);
            }
        });
    }
}

/// A seat's open melds in `game`, which are public.
fn melds_of(game: Option<&GameStateSnapshot>, seat: u8) -> Vec<MeldSnapshot> {
    game.and_then(|g| g.players.iter().find(|p| p.seat == seat))
        .map(|p| p.melds.clone())
        .unwrap_or_default()
}

fn public_tiles(tiles: &[String]) -> bool {
    !tiles.is_empty() && tiles.iter().all(|t| t != "?" && !t.is_empty())
}
fn same_round(a: DecisionContext, b: DecisionContext) -> bool {
    a.game == b.game && a.round == b.round
}
fn expire(decision: &mut Option<Decision>) {
    if let Some(d) = decision {
        if d.state == "thinking" || d.state == "awaiting" {
            d.state = "expired".into();
        }
    }
}

fn confirm(s: &mut MonitorSnapshot) {
    let Some(d) = &mut s.decision else {
        return;
    };
    if d.state != "awaiting" || !same_round(d.context, s.context) {
        return;
    }
    let Some(response) = &d.response else {
        return;
    };
    let events: Vec<_> = s
        .events
        .iter()
        .filter(|e| e.seq > d.context.event_seq)
        .collect();
    let actor = match &response.action {
        MjaiEvent::Reach { actor, .. } => Some(*actor),
        action => action_actor(action).or(s.game.as_ref().and_then(|g| g.our_seat)),
    };
    let actual = events
        .iter()
        .find(|e| action_actor(&e.event) == actor && actor.is_some());
    if let MjaiEvent::Reach { actor, pai, .. } = &response.action {
        let declared = events
            .iter()
            .any(|e| matches!(&e.event, MjaiEvent::Reach { actor: a, .. } if a == actor));
        if declared {
            if let Some(e) = actual.filter(|e| matches!(&e.event, MjaiEvent::Dahai { actor: a, pai: p, .. } if a == actor && pai.as_ref().is_none_or(|expected| expected == p))) {
                d.state = "confirmed".into(); d.confirmed_seq = Some(e.seq);
            }
        }
    } else if let Some(e) = actual.filter(|e| action_matches(&response.action, &e.event)) {
        d.state = "confirmed".into();
        d.confirmed_seq = Some(e.seq);
    }
    // A real own action different from the plan invalidates the suggestion.
    if d.state == "awaiting" && actual.is_some() {
        d.state = "expired".into();
        d.error = Some("The game confirmed a different action.".into());
    }
}

// Declarations alone do not commit a move. Use the first actual own move
// after the trigger, so a matching tile on a later turn cannot confirm it.
fn action_actor(event: &MjaiEvent) -> Option<u8> {
    match event {
        MjaiEvent::Dahai { actor, .. }
        | MjaiEvent::Chi { actor, .. }
        | MjaiEvent::Pon { actor, .. }
        | MjaiEvent::Daiminkan { actor, .. }
        | MjaiEvent::Ankan { actor, .. }
        | MjaiEvent::Kakan { actor, .. }
        | MjaiEvent::Kita { actor, .. }
        | MjaiEvent::Hora { actor, .. } => Some(*actor),
        _ => None,
    }
}

fn action_matches(a: &MjaiEvent, b: &MjaiEvent) -> bool {
    match (a, b) {
        (
            MjaiEvent::Dahai {
                actor: a, pai: p, ..
            },
            MjaiEvent::Dahai {
                actor: b, pai: q, ..
            },
        ) => a == b && p == q,
        (
            MjaiEvent::Hora {
                actor: a,
                target: t,
                ..
            },
            MjaiEvent::Hora {
                actor: b,
                target: u,
                ..
            },
        ) => a == b && t == u,
        (
            MjaiEvent::Chi { .. }
            | MjaiEvent::Pon { .. }
            | MjaiEvent::Daiminkan { .. }
            | MjaiEvent::Ankan { .. }
            | MjaiEvent::Kakan { .. }
            | MjaiEvent::Kita { .. },
            _,
        ) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(json: &str) -> MjaiEvent {
        serde_json::from_str(json).unwrap()
    }
    fn hand(m: &MonitorHandle) -> DecisionContext {
        m.game_event(
            &event(r#"{"type":"start_game","id":0,"names":["a","b","c","d"],"aka_flag":true}"#),
            None,
            None,
        );
        m.game_event(&event(r#"{"type":"start_kyoku","bakaze":"E","dora_marker":"1m","kyoku":1,"honba":0,"kyotaku":0,"oya":0,"scores":[25000,25000,25000,25000],"tehais":[[],[],[],[]]}"#), None, None)
    }
    fn response(json: &str) -> BotResponse {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn forecast_updates_change_only_their_field_and_snapshot_revision() {
        use crate::bot::control::{DecisionControl, DecisionSource, RunnerHealth};
        use crate::bot::forecast::{ForecastPhase, ForecastStatus};
        for source in [DecisionSource::Primary, DecisionSource::NativeStandby] {
            let m = MonitorHandle::default();
            let context = hand(&m);
            m.set_bot(BotStatus::Ready {
                bot: "demo".into(),
                actor_id: 0,
            });
            let control = DecisionControl {
                context,
                window: None,
                source,
                primary: RunnerHealth::Ready { through: 2 },
                standby: RunnerHealth::Ready { through: 2 },
            };
            m.set_control(control.clone());
            let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
            let mut reply = response(r#"{"type":"dahai","actor":0,"pai":"9m","tsumogiri":true}"#);
            reply.set_control(control);
            m.complete(id, &reply, 10);
            let before = serde_json::to_value(m.snapshot()).unwrap();
            for phase in [
                ForecastPhase::Idle,
                ForecastPhase::Disabled,
                ForecastPhase::Unsupported {
                    reason: "no forecast protocol".into(),
                },
                ForecastPhase::Waiting,
                ForecastPhase::Running {
                    context,
                    through: 2,
                    position: 1,
                },
                ForecastPhase::Ready {
                    context,
                    through: 2,
                    position: 1,
                },
                ForecastPhase::Unavailable {
                    reason: "forecast pipe closed".into(),
                },
            ] {
                let status = ForecastStatus { owner: 9, phase };
                let revision = m.snapshot().revision;
                m.set_forecast(status.clone());
                let mut after = serde_json::to_value(m.snapshot()).unwrap();
                assert_eq!(after["forecast"], serde_json::to_value(status).unwrap());
                assert_eq!(after["revision"], revision + 1);
                after["forecast"] = before["forecast"].clone();
                after["revision"] = before["revision"].clone();
                assert_eq!(after, before);
            }
        }
    }

    #[test]
    fn primary_notices_do_not_expire_standby_but_a_real_tracker_gap_does() {
        use crate::bot::control::{DecisionControl, DecisionSource, RunnerHealth};
        let m = MonitorHandle::default();
        let context = hand(&m);
        m.set_bot(BotStatus::Ready {
            bot: "demo".into(),
            actor_id: 0,
        });
        let control = DecisionControl {
            context,
            window: None,
            source: DecisionSource::NativeStandby,
            primary: RunnerHealth::Recovering {
                through: 2,
                reason: "pipe lost".into(),
            },
            standby: RunnerHealth::Ready { through: 3 },
        };
        m.set_control(control.clone());
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        let mut reply = response(r#"{"type":"dahai","actor":0,"pai":"9m","tsumogiri":true}"#);
        reply.set_control(control);
        m.complete(id, &reply, 10);
        m.notification(
            &Notification::warn("primary down")
                .body("pipe lost")
                .id("demo-server"),
        );
        assert_eq!(m.snapshot().decision.unwrap().state, "awaiting");
        assert!(m.snapshot().sync_error.is_none());
        assert_eq!(m.snapshot().service_error.as_deref(), Some("pipe lost"));
        assert_eq!(m.snapshot().service, "error");
        m.set_runner_health(
            RunnerHealth::Ready { through: 3 },
            RunnerHealth::Ready { through: 3 },
        );
        assert_eq!(
            m.snapshot().control.unwrap().source,
            DecisionSource::NativeStandby
        );
        m.gap("missing real event".into());
        assert_eq!(m.snapshot().decision.unwrap().state, "expired");
    }
    fn deal(m: &MonitorHandle, kyoku: u8, honba: u8) -> DecisionContext {
        let dealt = format!(
            r#"{{"type":"start_kyoku","bakaze":"E","dora_marker":"1m","kyoku":{kyoku},"honba":{honba},"kyotaku":0,"oya":0,"scores":[25000,25000,25000,25000],"tehais":[[],[],[],[]]}}"#
        );
        m.game_event(&event(&dealt), None, None)
    }
    fn play(m: &MonitorHandle, events: &[&str]) {
        for json in events {
            m.game_event(&event(json), None, None);
        }
    }
    /// The game a provider names, started (or rejoined).
    fn start_named(m: &MonitorHandle, game_id: u64) {
        let start = MjaiEvent::StartGame {
            names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kyoku_first: None,
            aka_flag: Some(true),
            id: Some(0),
            num_players: 4,
            game_meta: Some(crate::schema::GameMeta {
                game_id: Some(game_id),
                ..Default::default()
            }),
        };
        m.game_event(&start, None, None);
    }
    /// A table in which seat 1 has called a pon of white dragons.
    fn table_with_a_pon() -> GameStateSnapshot {
        let player = |seat: u8| {
            serde_json::json!({
                "seat": seat, "tehai": [], "river": [], "score": 25000,
                "riichi_declared": false, "riichi_stage": false, "double_riichi": false,
                "riichi_declaration_index": null,
                "melds": if seat == 1 {
                    serde_json::json!([{ "kind": "pon", "tiles": ["P", "P", "P"], "from_who": 0, "called_tile": "P" }])
                } else {
                    serde_json::json!([])
                },
            })
        };
        serde_json::from_value(serde_json::json!({
            "bakaze": "E", "kyoku": 1, "honba": 0, "kyotaku": 0, "oya": 0,
            "current_player": 1, "turn_count": 9, "phase": "wait_act", "is_done": true,
            "players": [player(0), player(1), player(2), player(3)],
            "dora_markers": ["1m"], "our_seat": 0,
        }))
        .unwrap()
    }

    /// A revealed hand keeps its open melds after the next round is dealt,
    /// when the table they were read from holds that round instead.
    #[test]
    fn a_revealed_hand_keeps_its_melds_into_the_next_round() {
        let m = MonitorHandle::default();
        hand(&m);
        m.game_event(
            &event(r#"{"type":"hora","actor":1,"target":1,"pai":"5m","tehai":["2m","3m","4m","5m","6m","7m","1p","1p","9s","9s"],"deltas":[-2600,7800,-2600,-2600]}"#),
            Some(table_with_a_pon()),
            None,
        );
        play(&m, &[r#"{"type":"end_kyoku"}"#]);
        deal(&m, 2, 0);

        let s = m.snapshot();
        let result = s.hands[0].result.as_ref().expect("the first hand ended");
        let melds: Vec<_> = result.hands[0]
            .melds
            .iter()
            .map(|m| (m.kind.clone(), m.tiles.clone()))
            .collect();
        assert_eq!(
            melds,
            [(crate::game_state::MeldKind::Pon, vec!["P".to_string(); 3])]
        );
    }

    /// The player's own ranks are not a game's: a new game keeps them, and
    /// what the platform says next replaces them.
    #[test]
    fn our_ranks_outlast_a_game() {
        let m = MonitorHandle::default();
        assert!(m.snapshot().account.is_none());
        let first = AccountLevels {
            platform: crate::schema::AccountPlatform::Majsoul,
            level: Some(crate::schema::Level {
                id: 10203,
                score: 761,
            }),
            level3: None,
            at: chrono::Utc::now(),
        };
        m.set_account(first.clone());
        hand(&m);
        assert_eq!(m.snapshot().account, Some(first));
        let next = AccountLevels {
            platform: crate::schema::AccountPlatform::Majsoul,
            level: Some(crate::schema::Level {
                id: 10203,
                score: 800,
            }),
            level3: Some(crate::schema::Level {
                id: 20101,
                score: 0,
            }),
            at: chrono::Utc::now(),
        };
        m.set_account(next.clone());
        assert_eq!(m.snapshot().account, Some(next));
    }

    /// A game's hands stay: how each ended in the snapshot, its events on
    /// request.
    #[test]
    fn the_hands_of_a_game_are_kept_until_another_game_starts() {
        let m = MonitorHandle::default();
        hand(&m);
        play(
            &m,
            &[
                r#"{"type":"tsumo","actor":0,"pai":"5m"}"#,
                r#"{"type":"dahai","actor":0,"pai":"5m","tsumogiri":true}"#,
                r#"{"type":"ryukyoku","deltas":[0,0,0,0]}"#,
                r#"{"type":"end_kyoku"}"#,
            ],
        );
        deal(&m, 1, 1);
        play(&m, &[r#"{"type":"tsumo","actor":0,"pai":"9p"}"#]);

        let s = m.snapshot();
        let hands: Vec<_> = s
            .hands
            .iter()
            .map(|h| (h.round, h.kyoku, h.honba, h.result.is_some(), h.events))
            .collect();
        assert_eq!(hands, [(1, 1, 0, true, 5), (2, 1, 1, false, 2)]);
        // The hand in play is in the snapshot; an earlier one is asked for.
        assert_eq!(s.events.len(), 2);
        let first = m.hand_events(1).unwrap();
        assert_eq!(first.len(), 5);
        assert!(matches!(first[0].event, MjaiEvent::StartKyoku { .. }));
        assert!(matches!(first[4].event, MjaiEvent::EndKyoku));
        assert!(m.hand_events(0).is_none() && m.hand_events(3).is_none());

        // Another game: nothing of the last one is left.
        hand(&m);
        assert_eq!(m.snapshot().hands.len(), 1);
        assert_eq!(m.hand_events(1).unwrap().len(), 1);
    }

    /// Rejoining a game starts it again on the wire and deals the hand in
    /// play again. The hands already played stay, and the hand in play is
    /// one hand, not two.
    #[test]
    fn a_rejoined_game_keeps_its_hands() {
        let m = MonitorHandle::default();
        start_named(&m, 7);
        deal(&m, 1, 0);
        play(
            &m,
            &[
                r#"{"type":"ryukyoku","deltas":[0,0,0,0]}"#,
                r#"{"type":"end_kyoku"}"#,
            ],
        );
        deal(&m, 1, 1);
        play(&m, &[r#"{"type":"tsumo","actor":0,"pai":"9p"}"#]);

        start_named(&m, 7);
        deal(&m, 1, 1);
        let hands: Vec<_> = m
            .snapshot()
            .hands
            .iter()
            .map(|h| (h.round, h.honba, h.result.is_some(), h.events))
            .collect();
        assert_eq!(hands, [(1, 0, true, 3), (2, 1, false, 1)]);

        // A different game is a different game.
        start_named(&m, 8);
        assert!(m.snapshot().hands.is_empty());
    }

    #[test]
    fn decisions_are_numbered_from_one_in_every_game() {
        let m = MonitorHandle::default();
        let number = |m: &MonitorHandle| m.snapshot().decision.map(|d| (d.id, d.number));
        let context = hand(&m);
        m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.begin(context, "demo", &MjaiEvent::None).unwrap();
        assert_eq!(number(&m), Some((2, 2)));

        // A new round of the same game goes on counting.
        let context = m.game_event(&event(r#"{"type":"start_kyoku","bakaze":"E","dora_marker":"1m","kyoku":2,"honba":0,"kyotaku":0,"oya":1,"scores":[25000,25000,25000,25000],"tehais":[[],[],[],[]]}"#), None, None);
        m.begin(context, "demo", &MjaiEvent::None).unwrap();
        assert_eq!(number(&m), Some((3, 3)));

        // A new game starts again from one; what a reply is matched to does not.
        let context = hand(&m);
        m.begin(context, "demo", &MjaiEvent::None).unwrap();
        assert_eq!(number(&m), Some((4, 1)));
    }
    #[test]
    fn stale_round_reply_is_not_current() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        hand(&m);
        m.complete(
            id,
            &response(r#"{"type":"dahai","actor":0,"pai":"5mr","tsumogiri":false}"#),
            140,
        );
        assert!(m.snapshot().decision.is_none());
    }
    #[test]
    fn what_a_bot_says_about_itself_is_kept_as_written_until_it_says_otherwise() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(
                r#"{"type":"dahai","actor":0,"pai":"1m","tsumogiri":false,"meta":{"facts":[{"name":"Depth","value":"42 plies","note":"Counted from the deal."},{"value":"nameless"}]}}"#,
            ),
            140,
        );
        let facts = m.snapshot().facts;
        assert_eq!(facts.len(), 1, "a fact is a name and a value");
        assert_eq!(
            (facts[0].name.as_str(), facts[0].value.as_str()),
            ("Depth", "42 plies")
        );
        assert_eq!(facts[0].note.as_deref(), Some("Counted from the deal."));

        // A reply that says nothing about the bot leaves the last word standing.
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"none","meta":{"show":{"items":[]}}}"#),
            90,
        );
        assert_eq!(m.snapshot().facts.len(), 1);
    }
    #[test]
    fn red_identity_is_required_and_early_wire_echo_confirms() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.game_event(
            &event(r#"{"type":"dahai","actor":0,"pai":"5mr","tsumogiri":false}"#),
            None,
            None,
        );
        m.complete(
            id,
            &response(r#"{"type":"dahai","actor":0,"pai":"5mr","tsumogiri":false}"#),
            140,
        );
        assert_eq!(m.snapshot().decision.unwrap().state, "confirmed");
        assert!(!action_matches(
            &event(r#"{"type":"dahai","actor":0,"pai":"5m","tsumogiri":false}"#),
            &event(r#"{"type":"dahai","actor":0,"pai":"5mr","tsumogiri":false}"#)
        ));
    }
    #[test]
    fn discard_without_reach_never_confirms_riichi() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"reach","actor":0,"pai":"3p"}"#),
            80,
        );
        m.game_event(
            &event(r#"{"type":"dahai","actor":0,"pai":"3p","tsumogiri":false}"#),
            None,
            None,
        );
        assert_ne!(m.snapshot().decision.unwrap().state, "confirmed");
    }
    #[test]
    fn partial_draw_and_multi_ron_keep_only_explicit_reveals() {
        let m = MonitorHandle::default();
        hand(&m);
        m.game_event(&event(r#"{"type":"ryukyoku","tehais":[null,["5pr","P"],[],null],"reason":"exhaustive","scores":[24000,28000,24000,24000]}"#), None, None);
        let result = m.snapshot().result.unwrap();
        assert_eq!(result.hands.len(), 1);
        assert_eq!(result.hands[0].tiles[0], "5pr");
        hand(&m);
        for seat in [1, 2] {
            m.game_event(&event(&format!(r#"{{"type":"hora","actor":{seat},"target":0,"tehai":["P"],"pai":"5mr","deltas":[-8000,4000,4000,0]}}"#)), None, None);
        }
        let result = m.snapshot().result.unwrap();
        assert_eq!(result.hands.len(), 2);
        assert_eq!(result.deltas.unwrap()[0], -8000);
        assert_eq!(result.hands[0].tiles.len(), 1);
    }
    #[test]
    fn none_on_service_failure_cannot_show_a_fresh_success() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        m.set_bot(BotStatus::Ready {
            bot: "demo".into(),
            actor_id: 0,
        });
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.notification(&Notification::error("unreachable").id("demo-server"));
        m.complete(id, &response(r#"{"type":"none"}"#), 4000);
        assert_eq!(m.snapshot().service, "error");
        assert_eq!(m.snapshot().decision.unwrap().state, "unavailable");
    }

    #[test]
    fn sync_fault_survives_deals_until_an_actionable_reply_confirms_recovery() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        m.set_bot(BotStatus::Ready {
            bot: "demo".into(),
            actor_id: 0,
        });
        m.notification(
            &Notification::warn("lost this hand")
                .body("bad discard")
                .id("demo-desync"),
        );
        assert!(m.begin(context, "demo", &MjaiEvent::None).is_none());
        let context = deal(&m, 2, 0);
        assert!(m.snapshot().sync_error.is_none());
        assert_eq!(m.snapshot().sync_fault.as_deref(), Some("bad discard"));
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(id, &response(r#"{"type":"none"}"#), 10);
        assert!(m.snapshot().sync_fault.is_some());
        // A reconnect/new start_game is not proof of restored inference either.
        let context = hand(&m);
        assert!(m.snapshot().sync_fault.is_some());
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"dahai","actor":0,"pai":"9m","tsumogiri":true}"#),
            10,
        );
        assert!(m.snapshot().sync_fault.is_none());
        m.gap("another lost hand".into());
        let context = deal(&m, 3, 0);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"none","meta":{"unavailable":"still lost"}}"#),
            10,
        );
        assert!(m.snapshot().sync_fault.is_some());
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"none","meta":{"show":{"items":[]}}}"#),
            10,
        );
        assert!(
            m.snapshot().sync_fault.is_none(),
            "an actual model PASS also confirms recovery"
        );
    }
    #[test]
    fn notices_require_the_current_bot_namespace() {
        let m = MonitorHandle::default();
        hand(&m);
        m.set_bot(BotStatus::Ready {
            bot: "demo".into(),
            actor_id: 0,
        });
        m.notification(&Notification::error("Socket closed").id("demo-server"));
        assert_eq!(m.snapshot().service, "error");
        m.set_bot(BotStatus::Ready {
            bot: "another-bot".into(),
            actor_id: 0,
        });
        m.notification(&Notification::info("Connected").id("demo-server"));
        assert_ne!(m.snapshot().service, "connected");
    }
    #[test]
    fn later_matching_discard_cannot_confirm_an_old_suggestion() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        for tile in ["1m", "5mr"] {
            m.game_event(
                &event(&format!(
                    r#"{{"type":"dahai","actor":0,"pai":"{tile}","tsumogiri":false}}"#
                )),
                None,
                None,
            );
        }
        m.complete(
            id,
            &response(r#"{"type":"dahai","actor":0,"pai":"5mr","tsumogiri":false}"#),
            400,
        );
        assert_eq!(m.snapshot().decision.unwrap().state, "expired");
    }
    #[test]
    fn riichi_needs_both_declaration_and_the_selected_red_discard() {
        let m = MonitorHandle::default();
        let context = hand(&m);
        let id = m.begin(context, "demo", &MjaiEvent::None).unwrap();
        m.complete(
            id,
            &response(r#"{"type":"reach","actor":0,"pai":"5pr"}"#),
            100,
        );
        m.game_event(&event(r#"{"type":"reach","actor":0}"#), None, None);
        assert_eq!(m.snapshot().decision.as_ref().unwrap().state, "awaiting");
        m.game_event(
            &event(r#"{"type":"dahai","actor":0,"pai":"5pr","tsumogiri":false}"#),
            None,
            None,
        );
        assert_eq!(m.snapshot().decision.unwrap().state, "confirmed");
    }
}
