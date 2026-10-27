//! Tenhou Web observations and transitions of the same `ui::Machine`.
//! The injected client reports facts and executes a permitted control; it
//! never chooses a phase, target, retry or recovery path.

use super::{
    Client, Effect, Environment, Input, Machine, State, Suspension, Ticket, WaitingReason,
    END_LIMIT, LOOK, PRESS_LIMIT, RESULT_LIMIT,
};
use crate::autoplay::humanize;
use crate::autoplay::tenhou::client::Dispatch;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ClientTag {
    #[serde(rename = "web3")]
    Web3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PopupTag {
    #[serde(rename = "TAIKYOKU")]
    Start,
    #[serde(rename = "AGARI")]
    Win,
    #[serde(rename = "RYUUKYOKU")]
    Draw,
    #[serde(rename = "OWARI")]
    Final,
    #[serde(rename = "NINTEI")]
    RankNotice,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Popup {
    pub identity: String,
    pub tag: PopupTag,
    pub enabled: bool,
    pub closing: bool,
    pub queued: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub identity: String,
    pub mode: u32,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lobby {
    pub identity: String,
    pub lobby: u32,
    pub offers: Vec<Offer>,
    pub reservation_requested: bool,
}

/// Required fields have no guessed defaults. A missing/invalid producer is
/// reported as unavailable by the executor, never as a convenient lobby.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Web3Observation {
    pub client: ClientTag,
    #[serde(deserialize_with = "required_nullable")]
    pub popup: Option<Popup>,
    #[serde(deserialize_with = "required_nullable")]
    pub lobby: Option<Lobby>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    Popup {
        identity: String,
        tag: PopupTag,
        queued: u32,
    },
    Reserve {
        identity: String,
        lobby_identity: String,
        lobby: u32,
        mode: u32,
    },
}

impl Control {
    pub fn dispatch_key(&self) -> (&'static str, &str) {
        match self {
            Self::Popup { identity, .. } => ("popup", identity),
            Self::Reserve { identity, .. } => ("join", identity),
        }
    }

    fn identity(&self) -> &str {
        match self {
            Self::Popup { identity, .. } | Self::Reserve { identity, .. } => identity,
        }
    }
}

/// Only a real game's `MatchInfo::Tenhou` may establish this target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableSpec {
    pub lobby: u32,
    pub mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    WaitingStart,
    Starting,
    Table,
    RoundResult,
    FinalResult,
    WaitingLobby,
    ReservationPending,
    Stopped(WaitingReason),
}

#[derive(Debug, Clone)]
struct Candidate {
    control: Control,
    observation: Web3Observation,
    ready_at: Instant,
}

#[derive(Debug, Clone)]
pub(super) struct Run {
    pub stage: Stage,
    player: Option<u8>,
    target: Option<TableSpec>,
    candidate: Option<Candidate>,
    spent: HashSet<String>,
    until: Option<Instant>,
    attempts: u32,
}

impl Default for Run {
    fn default() -> Self {
        Self {
            stage: Stage::WaitingStart,
            player: None,
            target: None,
            candidate: None,
            spent: HashSet::new(),
            until: None,
            attempts: 0,
        }
    }
}

impl Run {
    fn stop(&mut self, reason: WaitingReason) {
        self.stage = Stage::Stopped(reason);
        self.candidate = None;
        self.until = None;
    }

    fn expire(&mut self, now: Instant) {
        if self.until.is_some_and(|until| now >= until) {
            self.stop(WaitingReason::EndTimeout);
        }
    }

    fn allowed(&self, control: &Control, env: Environment) -> bool {
        if self.player.is_none() {
            return false;
        }
        match control {
            Control::Popup {
                tag: PopupTag::Start,
                ..
            } => env.settings.quick_confirm && self.stage == Stage::Starting,
            Control::Popup {
                tag: PopupTag::Win | PopupTag::Draw,
                ..
            } => env.settings.quick_confirm && self.stage == Stage::RoundResult,
            Control::Popup {
                tag: PopupTag::Final,
                ..
            } => env.settings.auto_continue && self.stage == Stage::FinalResult,
            Control::Popup {
                tag: PopupTag::RankNotice,
                ..
            } => {
                env.settings.auto_continue
                    && self.stage == Stage::WaitingLobby
                    && self.target.is_some()
            }
            Control::Reserve { lobby, mode, .. } => {
                env.settings.auto_continue
                    && self.stage == Stage::WaitingLobby
                    && self.target
                        == Some(TableSpec {
                            lobby: *lobby,
                            mode: *mode,
                        })
            }
            _ => false,
        }
    }

    fn control(&mut self, observation: &Web3Observation, now: Instant) -> Option<Control> {
        if let Some(popup) = &observation.popup {
            // A popup plus a usable lobby is contradictory, not permission
            // to prefer one observer or choose whichever button is enabled.
            if observation.lobby.is_some() || popup.identity.is_empty() {
                return None;
            }
            if self.spent.contains(&popup.identity) {
                return None;
            }
            let stage = match popup.tag {
                PopupTag::Start if matches!(self.stage, Stage::WaitingStart | Stage::Starting) => {
                    Stage::Starting
                }
                PopupTag::Win | PopupTag::Draw
                    if matches!(
                        self.stage,
                        Stage::WaitingStart | Stage::Starting | Stage::Table | Stage::RoundResult
                    ) =>
                {
                    Stage::RoundResult
                }
                PopupTag::Final
                    if matches!(
                        self.stage,
                        Stage::WaitingStart
                            | Stage::Starting
                            | Stage::Table
                            | Stage::RoundResult
                            | Stage::FinalResult
                    ) =>
                {
                    Stage::FinalResult
                }
                // The real client can queue a rank certificate after OWARI.
                // It belongs to the completed match's return-to-lobby path,
                // never to login/startup or a live table.
                PopupTag::RankNotice if self.stage == Stage::WaitingLobby => Stage::WaitingLobby,
                _ => return None,
            };
            if self.stage != stage {
                self.until = Some(
                    now + if stage == Stage::FinalResult {
                        END_LIMIT
                    } else {
                        RESULT_LIMIT
                    },
                );
                self.attempts = 0;
            }
            self.stage = stage;
            if !popup.enabled || popup.closing {
                return None;
            }
            return Some(Control::Popup {
                identity: popup.identity.clone(),
                tag: popup.tag,
                queued: popup.queued,
            });
        }
        let Some(lobby) = &observation.lobby else {
            return None;
        };
        if lobby.identity.is_empty() {
            return None;
        }
        if lobby.reservation_requested {
            self.stage = Stage::ReservationPending;
            self.until = None;
            return None;
        }
        if self.stage != Stage::WaitingLobby {
            if matches!(
                self.stage,
                Stage::Table | Stage::RoundResult | Stage::FinalResult
            ) {
                self.stop(WaitingReason::Lobby);
            }
            return None;
        }
        let Some(target) = self.target else {
            self.stop(WaitingReason::NoKnownTable);
            return None;
        };
        if lobby.lobby != target.lobby {
            self.stop(WaitingReason::TargetUnavailable);
            return None;
        }
        let offers: Vec<_> = lobby
            .offers
            .iter()
            .filter(|offer| offer.mode == target.mode)
            .collect();
        let [offer] = offers.as_slice() else {
            self.stop(WaitingReason::TargetUnavailable);
            return None;
        };
        if !offer.enabled || offer.identity.is_empty() || self.spent.contains(&offer.identity) {
            self.stop(WaitingReason::TargetUnavailable);
            return None;
        }
        Some(Control::Reserve {
            identity: offer.identity.clone(),
            lobby_identity: lobby.identity.clone(),
            lobby: target.lobby,
            mode: target.mode,
        })
    }
}

impl Machine {
    pub(super) fn synchronize_web(
        &mut self,
        env: Environment,
        why: Option<Suspension>,
        now: Instant,
    ) {
        let session_changed = self.env.is_some_and(|old| {
            old.client != env.client
                || old.generation.session != env.generation.session
                || old.generation.table != env.generation.table
        });
        let ownership_changed = self
            .env
            .is_some_and(|old| old.generation != env.generation || old.settings != env.settings)
            || self.suspension != why;
        if ownership_changed || session_changed {
            self.revoke();
        }
        if session_changed || !matches!(self.state, State::Web(_)) {
            self.state = State::Web(Run::default());
        }
        let State::Web(run) = &mut self.state else {
            unreachable!()
        };
        if ownership_changed {
            run.candidate = None;
            if matches!(run.stage, Stage::WaitingLobby | Stage::ReservationPending) {
                run.stop(WaitingReason::OwnershipChanged);
            }
        }
        run.expire(now);
        self.suspension = why;
        self.env = Some(env);
        if why.is_none() && !matches!(run.stage, Stage::Stopped(_)) && self.effects.idle() {
            self.effects.schedule(now + LOOK, Input::WebObserve);
        }
    }

    pub fn web_game_started(
        &mut self,
        player: Option<u8>,
        target: Option<TableSpec>,
        now: Instant,
    ) {
        if self.client() != Client::TenhouWeb {
            return;
        }
        let player = player.filter(|seat| *seat < 4);
        self.revoke();
        self.state = State::Web(Run {
            stage: Stage::Starting,
            player,
            target: target.filter(|_| player.is_some()),
            until: Some(now + RESULT_LIMIT),
            ..Run::default()
        });
        self.effects.schedule(now + LOOK, Input::WebObserve);
    }

    pub(super) fn web_table_started(&mut self) {
        self.revoke();
        if let State::Web(run) = &mut self.state {
            run.stage = Stage::Table;
            run.candidate = None;
            run.until = None;
            run.attempts = 0;
        }
    }

    pub(super) fn web_tick(&mut self, now: Instant) -> Option<Effect> {
        let State::Web(run) = &mut self.state else {
            return None;
        };
        run.expire(now);
        if matches!(run.stage, Stage::Stopped(_)) {
            self.effects.cancel();
            return None;
        }
        let (ticket, input) = self.effects.tick(now)?;
        match input {
            Input::WebObserve => Some(Effect::ObserveWeb(ticket)),
            Input::WebControl => run.candidate.as_ref().map(|candidate| Effect::InvokeWeb {
                ticket,
                control: candidate.control.clone(),
            }),
            _ => unreachable!("Web route cannot inherit a pixel/decision effect"),
        }
    }

    /// This real client phase drives confirmations before the bridge's
    /// multi-ron boundary can publish Hora. No MJAI event is manufactured.
    pub fn web_observed<R: Rng + ?Sized>(
        &mut self,
        ticket: Ticket,
        observation: Option<Web3Observation>,
        now: Instant,
        rng: &mut R,
    ) {
        if self.client() != Client::TenhouWeb || !self.effects.observed(ticket, Input::WebObserve) {
            return;
        }
        let Some(env) = self.env else { return };
        let State::Web(run) = &mut self.state else {
            return;
        };
        run.expire(now);
        if matches!(run.stage, Stage::Stopped(_)) {
            return;
        }
        let Some(observation) = observation else {
            run.stop(WaitingReason::ObservationUnavailable);
            return;
        };
        let control = run
            .control(&observation, now)
            .filter(|control| run.allowed(control, env));
        if let Some(control) = control {
            let ready_at = match &run.candidate {
                Some(old) if old.control == control && old.observation == observation => {
                    old.ready_at
                }
                _ => {
                    let delay = match &control {
                        Control::Popup {
                            tag: PopupTag::Start | PopupTag::Win | PopupTag::Draw,
                            ..
                        } => humanize::CONFIRM_SEEN.draw_ms(rng),
                        _ => humanize::END_SEEN.draw_ms(rng),
                    };
                    now + Duration::from_millis(u64::from(delay))
                }
            };
            run.candidate = Some(Candidate {
                control,
                observation,
                ready_at,
            });
            if now >= ready_at {
                self.effects.schedule(now, Input::WebControl);
            } else {
                self.effects
                    .schedule(ready_at.min(now + LOOK), Input::WebObserve);
            }
        } else {
            run.candidate = None;
            if !matches!(run.stage, Stage::Stopped(_)) {
                self.effects.schedule(now + LOOK, Input::WebObserve)
            }
        }
    }

    /// The executor re-reads this page under its read lease, synchronizes
    /// ownership, consumes once, then atomically rechecks the same witness
    /// inside the client's invoke. Either refusal permanently spends ticket.
    pub fn consume_web(
        &mut self,
        ticket: Ticket,
        control: &Control,
        observation: Option<&Web3Observation>,
    ) -> bool {
        if self.suspension.is_some() || self.effects.issued(ticket) != Some(Input::WebControl) {
            return false;
        }
        let allowed = match (&self.state, self.env, observation) {
            (State::Web(run), Some(env), Some(observation)) => {
                run.candidate.as_ref().is_some_and(|candidate| {
                    candidate.control == *control
                        && candidate.observation == *observation
                        && run.allowed(&candidate.control, env)
                        && !run.spent.contains(candidate.control.identity())
                })
            }
            _ => false,
        };
        self.effects.consume(ticket, allowed)
    }

    pub fn web_done(&mut self, ticket: Ticket, result: Dispatch, now: Instant) {
        if self.effects.input(ticket) != Some(Input::WebControl) {
            return;
        }
        let State::Web(run) = &mut self.state else {
            return;
        };
        let Some(completion) = self.effects.complete(ticket, result == Dispatch::Sent) else {
            return;
        };
        let candidate = run.candidate.take();
        if completion.permitted {
            run.attempts += 1;
            match result {
                Dispatch::Sent => {
                    let Some(candidate) = candidate else { return };
                    run.spent.insert(candidate.control.identity().to_owned());
                    match candidate.control {
                        Control::Popup {
                            tag: PopupTag::Final,
                            ..
                        } => {
                            run.stage = Stage::WaitingLobby;
                            run.until = Some(now + END_LIMIT);
                        }
                        Control::Reserve { .. } => {
                            run.stage = Stage::ReservationPending;
                            run.until = None;
                        }
                        _ => {}
                    }
                }
                Dispatch::Changed => {} // no invocation; a fresh observation may acquire a new ticket
                Dispatch::Unavailable => run.stop(WaitingReason::DispatchUnavailable),
            }
            if run.attempts >= PRESS_LIMIT && !matches!(run.stage, Stage::ReservationPending) {
                run.stop(WaitingReason::PressLimit);
            }
        }
        if !matches!(run.stage, Stage::Stopped(_)) {
            self.effects.schedule(now + LOOK, Input::WebObserve)
        }
    }
}
