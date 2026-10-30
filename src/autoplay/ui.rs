//! Platform UI phase and input authority. See `UI_FSM.md`.
//! All times and observations are supplied by the caller. This module never
//! sleeps, captures, clicks, selects a bot, or reconstructs model context.

mod effects;
pub mod tenhou_web;
#[cfg(test)]
mod tests;

use effects::Effects;
pub use effects::Ticket;

use super::budget::{BudgetSource, TimeBudget};
use super::humanize::{self, ASK_SEEN, CONFIRM_SEEN, END_SEEN};
use super::pixels::{Glyph, Sight};
use super::verify::{InputTicket, LobbyObservation, LobbyPhase};
use rand::Rng;
use std::time::{Duration, Instant};

const RESULT_LIMIT: Duration = Duration::from_secs(30);
const LAST_RESULT_LIMIT: Duration = Duration::from_secs(45);
const END_LIMIT: Duration = Duration::from_secs(180);
/// TAIKYOKU to INIT takes about 12 s while every seat is connected.
const START_LIMIT: Duration = Duration::from_secs(60);
/// A reconnect outlasts a proxy's failover (health checks run every 120 s).
const RECONNECT_LIMIT: Duration = Duration::from_secs(300);
/// Observed own queues reach about 13 minutes; past this an own reservation
/// is presumed lost with its socket and the client is reloaded once.
const RESERVATION_LIMIT: Duration = Duration::from_secs(15 * 60);
const RETRY_WAIT: Duration = Duration::from_secs(3);
const PRESS_LIMIT: u32 = 8;
const LOOK: Duration = Duration::from_millis(humanize::END_LOOK_EVERY_MS);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub quick_confirm: bool,
    pub auto_continue: bool,
    pub auto_win: bool,
}

/// Session/config/capture, protocol reset, and manual ownership are distinct
/// facts. None of them identifies the selected model or its health.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Generation {
    pub session: u64,
    pub table: u64,
    pub manual: u64,
    pub settings: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Majsoul,
    TenhouWeb,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablePhase {
    Waiting,
    OwnDecision,
    Claim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub opened_at: Instant,
    pub phase: TablePhase,
    pub expires_at: Instant,
}

impl From<TimeBudget> for Window {
    fn from(b: TimeBudget) -> Self {
        let phase = match b.source {
            BudgetSource::NewRound | BudgetSource::DealTile | BudgetSource::ChiPengGang => {
                TablePhase::OwnDecision
            }
            BudgetSource::DiscardTile | BudgetSource::AnGangAddGang | BudgetSource::BaBei => {
                TablePhase::Claim
            }
        };
        Self {
            opened_at: b.opened_at,
            phase,
            expires_at: b.opened_at
                + Duration::from_millis(u64::from(b.fixed_ms) + u64::from(b.add_ms)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suspension {
    Disabled,
    Manual,
    CaptureUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    WaitingStart,
    Table(TablePhase),
    Unknown,
    Result,
    Continue,
    Reward,
    Rematch,
    RematchAsk,
    Matching,
    Suspended(Suspension),
    Web(tenhou_web::Stage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReason {
    NoGame,
    SessionChanged,
    Lobby,
    EndTimeout,
    ReconnectTimeout,
    PressLimit,
    ClientReload,
    NoKnownTable,
    TargetUnavailable,
    OwnershipChanged,
    ObservationUnavailable,
    DispatchUnavailable,
}

#[derive(Debug, Clone, Copy)]
pub struct Environment {
    pub client: Client,
    pub context: Option<crate::monitor::DecisionContext>,
    pub settings: Settings,
    pub generation: Generation,
    pub manual: bool,
    pub page_available: bool,
    pub window: Option<Window>,
    pub confirmed: u64,
    pub lobby: LobbyObservation,
    pub input: InputTicket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Round {
        confirmed: u64,
        ours: bool,
    },
    LastResult {
        confirmed: u64,
        ours: bool,
        until: Instant,
    },
    Summary,
    Continued,
    RematchSent,
    QuestionSent,
}

#[derive(Debug, Clone, Copy)]
struct Screens {
    route: Route,
    until: Instant,
    presses: u32,
    seen: Option<(Sight, Instant)>,
    retry: Option<(Sight, Instant)>,
}

impl Screens {
    fn retired(self) -> State {
        match self.route {
            Route::Round { .. } => State::Table(TablePhase::Waiting),
            _ if self.presses >= PRESS_LIMIT => State::WaitingStart(WaitingReason::PressLimit),
            _ => State::WaitingStart(WaitingReason::EndTimeout),
        }
    }
}

#[derive(Debug, Clone)]
enum State {
    WaitingStart(WaitingReason),
    Table(TablePhase),
    Screens(Screens),
    Matching,
    Web(tenhou_web::Run),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    Observe,
    Screen(Sight),
    AutoWin,
    WebObserve,
    WebControl,
    WebReload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Observe(Ticket),
    Press {
        ticket: Ticket,
        sight: Sight,
    },
    ObserveWeb(Ticket),
    PressWeb {
        ticket: Ticket,
        control: tenhou_web::Control,
    },
    /// Reload the Tenhou client so an own stalled match can be resumed.
    ReloadWeb(Ticket),
}

/// Final capture facts, read after hover. `table_visible` is positive pixel
/// evidence (the existing table toggle strip), never `Nothing` alone.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub stable_geometry: bool,
    pub sight: Sight,
    pub table_visible: bool,
    pub auto_win: Glyph,
}

#[derive(Debug)]
pub struct Machine {
    state: State,
    suspension: Option<Suspension>,
    env: Option<Environment>,
    effects: Effects<Input>,
}

impl Default for Machine {
    fn default() -> Self {
        Self {
            state: State::WaitingStart(WaitingReason::NoGame),
            suspension: None,
            env: None,
            effects: Effects::default(),
        }
    }
}

impl Machine {
    pub fn phase(&self) -> Phase {
        if let Some(why) = self.suspension {
            return Phase::Suspended(why);
        }
        match self.state {
            State::Web(ref run) => Phase::Web(run.stage),
            State::WaitingStart(_) => Phase::WaitingStart,
            State::Table(p) => Phase::Table(p),
            State::Matching => Phase::Matching,
            State::Screens(s) => match s.seen.map(|s| s.0) {
                Some(Sight::Button) => Phase::Result,
                Some(Sight::Continue) => Phase::Continue,
                Some(Sight::Reward) => Phase::Reward,
                Some(Sight::Rematch) => Phase::Rematch,
                Some(Sight::RematchAsk) => Phase::RematchAsk,
                _ => Phase::Unknown,
            },
        }
    }

    fn revoke(&mut self) {
        self.effects.revoke();
    }

    pub fn waiting_reason(&self) -> Option<WaitingReason> {
        match self.state {
            State::WaitingStart(reason) => Some(reason),
            State::Web(ref run) => match run.stage {
                tenhou_web::Stage::Stopped(reason) => Some(reason),
                _ => None,
            },
            _ => None,
        }
    }

    fn ticket(&mut self, input: Input) -> Ticket {
        self.effects.issue(input)
    }

    pub fn client(&self) -> Client {
        self.env.map_or(Client::Unsupported, |env| env.client)
    }

    /// Revoke old callbacks on every ownership change, including an off/on
    /// or manual interval that completed while an executor was awaiting CDP.
    pub fn synchronize(&mut self, env: Environment, now: Instant) {
        let session_changed = self.env.is_some_and(|old| {
            old.client != env.client
                || old.generation.session != env.generation.session
                || old.generation.table != env.generation.table
        });
        let changed = self
            .env
            .is_some_and(|old| old.generation != env.generation || old.settings != env.settings);
        let lobby_changed = self.env.is_none_or(|old| old.lobby != env.lobby);
        let why = if !env.settings.enabled {
            Some(Suspension::Disabled)
        } else if !env.page_available {
            Some(Suspension::CaptureUnavailable)
        } else if env.manual {
            Some(Suspension::Manual)
        } else {
            None
        };
        if env.client == Client::TenhouWeb {
            self.synchronize_web(env, why, now);
            return;
        }
        if changed || session_changed || self.suspension != why {
            self.revoke();
            if let State::Screens(s) = &mut self.state {
                s.seen = None;
                s.retry = None;
                // Manual/profile/capture ownership cannot inherit an old
                // rematch question. Result identity and deadlines survive.
                if !matches!(s.route, Route::Round { .. } | Route::LastResult { .. }) {
                    s.route = Route::Summary;
                }
            }
        }
        self.suspension = why;
        if session_changed {
            // New profile/tab/capture/restore/round: old screen ownership is
            // retired, not transplanted to a coincidentally similar screen.
            // A real start/restore event reenters the table explicitly.
            self.state = State::WaitingStart(WaitingReason::SessionChanged);
        }
        if lobby_changed && env.lobby.phase != LobbyPhase::Unknown {
            self.revoke();
            self.state = match env.lobby.phase {
                LobbyPhase::Matching => State::Matching,
                LobbyPhase::WaitingStart => State::WaitingStart(WaitingReason::Lobby),
                LobbyPhase::Unknown => unreachable!(),
            };
        }
        let moved = self.env.is_some_and(|old| old.window != env.window);
        self.env = Some(env);
        if let State::Table(_) = self.state {
            if moved {
                self.revoke();
            }
            self.state = State::Table(
                env.window
                    .filter(|w| now < w.expires_at)
                    .map_or(TablePhase::Waiting, |w| w.phase),
            );
        }
        if let State::Screens(s) = &mut self.state {
            if let Route::Round { confirmed, .. } = s.route {
                if confirmed != env.confirmed {
                    self.revoke();
                    self.state = State::Table(TablePhase::Waiting);
                    return;
                }
            }
            if now >= s.until || s.presses >= PRESS_LIMIT {
                let retired = s.retired();
                self.revoke();
                self.state = retired;
            } else if self.suspension.is_none() && self.effects.idle() {
                self.effects.schedule(now + LOOK, Input::Observe);
            }
        }
    }

    /// Called for authentic game/round starts, including protocol restore.
    pub fn table_started(&mut self, window: Option<Window>) {
        if self.client() == Client::TenhouWeb {
            self.web_table_started();
            return;
        }
        self.revoke();
        self.state = State::Table(window.map_or(TablePhase::Waiting, |w| w.phase));
    }

    pub fn game_started(&mut self) {
        self.table_started(None);
    }

    pub fn result(&mut self, confirmed: u64, ours: bool, now: Instant) {
        if self.client() == Client::TenhouWeb {
            return;
        }
        if let State::Screens(s) = &mut self.state {
            if let Route::Round { ours: own, .. } | Route::LastResult { ours: own, .. } =
                &mut s.route
            {
                *own |= ours; // double ron is one screen, not a new plan
                return;
            }
            return; // delayed result cannot resurrect an ending
        }
        if !matches!(self.state, State::Table(_)) {
            return;
        }
        self.revoke();
        self.state = State::Screens(Screens {
            route: Route::Round { confirmed, ours },
            until: now + RESULT_LIMIT,
            presses: 0,
            seen: None,
            retry: None,
        });
        self.effects.schedule(
            now + Duration::from_millis(humanize::RESULT_LOOK_FIRST_MS),
            Input::Observe,
        );
    }

    pub fn game_ended(&mut self, now: Instant) {
        if self.client() == Client::TenhouWeb {
            if let State::Web(run) = &mut self.state {
                run.end_resume();
            }
            return;
        }
        let Some(env) = self.env else {
            return;
        };
        let route = match self.state {
            State::Screens(Screens {
                route: Route::Round { confirmed, ours },
                ..
            }) => Route::LastResult {
                confirmed,
                ours,
                until: now + LAST_RESULT_LIMIT,
            },
            State::Table(_) => Route::LastResult {
                confirmed: env.confirmed,
                ours: false,
                until: now + LAST_RESULT_LIMIT,
            },
            _ => return, // duplicate, delayed or unowned end cannot restart
        };
        self.revoke();
        self.state = State::Screens(Screens {
            route,
            until: now + END_LIMIT,
            presses: 0,
            seen: None,
            retry: None,
        });
        self.effects.schedule(
            now + Duration::from_millis(humanize::END_LOOK_FIRST_MS),
            Input::Observe,
        );
    }

    pub fn due(&self) -> Option<Instant> {
        if self.suspension.is_some() {
            return None;
        }
        self.effects.due()
    }

    pub fn tick(&mut self, now: Instant) -> Option<Effect> {
        if self.suspension.is_some() {
            return None;
        }
        if matches!(self.state, State::Web(_)) {
            return self.web_tick(now);
        }
        if let State::Screens(s) = self.state {
            if now >= s.until || s.presses >= PRESS_LIMIT {
                self.revoke();
                self.state = s.retired();
                return None;
            }
        }
        let (t, input) = self.effects.tick(now)?;
        match input {
            Input::Observe => Some(Effect::Observe(t)),
            Input::Screen(sight) => Some(Effect::Press { ticket: t, sight }),
            _ => unreachable!("only screen work is clock driven"),
        }
    }

    /// A callback can only complete the specific observation it was issued.
    pub fn observed<R: Rng + ?Sized>(
        &mut self,
        ticket: Ticket,
        sight: Sight,
        now: Instant,
        rng: &mut R,
    ) {
        if self.client() != Client::Majsoul {
            return;
        }
        if !self.effects.observed(ticket, Input::Observe) {
            return;
        }
        let State::Screens(mut s) = self.state else {
            return;
        };
        let env = self.env.expect("observation requires an environment");
        if let Route::LastResult { until, .. } = s.route {
            if now >= until || matches!(sight, Sight::Continue | Sight::Rematch) {
                s.route = Route::Summary;
            }
        }
        let allowed = match s.route {
            Route::Round { confirmed, .. } => {
                env.settings.quick_confirm && env.confirmed == confirmed && sight == Sight::Button
            }
            Route::LastResult { .. } => env.settings.quick_confirm && sight == Sight::Button,
            Route::Summary => {
                env.settings.auto_continue && matches!(sight, Sight::Continue | Sight::Rematch)
            }
            Route::Continued => {
                env.settings.auto_continue
                    && matches!(sight, Sight::Continue | Sight::Reward | Sight::Rematch)
            }
            Route::RematchSent => {
                env.settings.auto_continue && matches!(sight, Sight::Rematch | Sight::RematchAsk)
            }
            Route::QuestionSent => env.settings.auto_continue && sight == Sight::RematchAsk,
        };
        if !allowed || s.retry.is_some_and(|(old, at)| old == sight && now < at) {
            s.seen = None;
            self.effects.schedule(now + LOOK, Input::Observe);
        } else {
            let read_at = match s.seen {
                Some((old, at)) if old == sight => at,
                _ => {
                    let ms = match sight {
                        Sight::Button => {
                            let ours = matches!(
                                s.route,
                                Route::Round { ours: true, .. }
                                    | Route::LastResult { ours: true, .. }
                            );
                            (f64::from(CONFIRM_SEEN.draw_ms(rng))
                                * if ours {
                                    humanize::OUR_BUTTON_LINGER
                                } else {
                                    1.0
                                })
                            .min(humanize::CONFIRM_SEEN_MOST_MS) as u64
                        }
                        Sight::RematchAsk => u64::from(ASK_SEEN.draw_ms(rng)),
                        _ => u64::from(END_SEEN.draw_ms(rng)),
                    };
                    now + Duration::from_millis(ms)
                }
            };
            s.seen = Some((sight, read_at));
            if now >= read_at {
                self.effects.schedule(now, Input::Screen(sight));
            } else {
                self.effects
                    .schedule(read_at.min(now + LOOK), Input::Observe);
            }
        }
        self.state = State::Screens(s);
    }

    /// Every input passes this same boundary, after the final capture. A
    /// ticket can produce at most one mouse-down, including duplicate calls.
    pub fn consume(&mut self, ticket: Ticket, frame: Frame) -> bool {
        if self.suspension.is_some() {
            return false;
        }
        let Some(input) = self.effects.issued(ticket) else {
            return false;
        };
        let allowed = frame.stable_geometry && self.env.is_some_and(|env| match input {
            Input::WebObserve | Input::WebControl | Input::WebReload => false,
            Input::Observe => false,
            Input::Screen(sight) => {
                frame.sight == sight
                    && matches!(self.state, State::Screens(s) if s.seen.is_some_and(|seen| seen.0 == sight))
            }
            Input::AutoWin => {
                env.settings.auto_win
                    && env.window.is_none()
                    && frame.table_visible
                    && frame.sight == Sight::Nothing
                    && frame.auto_win == Glyph::Unlit
            }
        });
        self.effects.consume(ticket, allowed)
    }

    pub fn screen_done<R: Rng + ?Sized>(
        &mut self,
        ticket: Ticket,
        pressed: bool,
        now: Instant,
        rng: &mut R,
    ) {
        let Some(Input::Screen(_)) = self.effects.input(ticket) else {
            return;
        };
        let State::Screens(mut s) = self.state else {
            return;
        };
        let Some(completion) = self.effects.complete(ticket, pressed) else {
            return;
        };
        let Input::Screen(sight) = completion.input else {
            unreachable!("screen completion admitted above");
        };
        if completion.permitted {
            s.presses += 1;
            if pressed {
                s.route = match sight {
                    Sight::Continue => Route::Continued,
                    Sight::Rematch => Route::RematchSent,
                    Sight::RematchAsk => Route::QuestionSent,
                    _ => s.route,
                };
            }
            s.retry = Some((sight, now + RETRY_WAIT));
        }
        s.seen = None;
        self.state = State::Screens(s);
        self.effects.schedule(
            now + if pressed {
                Duration::from_millis(u64::from(humanize::END_PAUSE.draw_ms(rng)))
            } else {
                LOOK
            },
            Input::Observe,
        );
    }

    pub fn auto_win(&mut self) -> Option<Ticket> {
        let env = self.env?;
        (self.suspension.is_none()
            && env.settings.auto_win
            && env.window.is_none()
            && matches!(self.state, State::Table(TablePhase::Waiting))
            && self.effects.idle())
        .then(|| self.ticket(Input::AutoWin))
    }

    pub fn toggle_done(&mut self, ticket: Ticket) {
        self.effects.retire(ticket);
    }
}
