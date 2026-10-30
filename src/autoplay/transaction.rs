//! The sole selected-action lifetime. Sites supply observations, the pointer
//! supplies physical input, and this owner alone advances or retries a turn.
//!
//! Each [`State`] carries the data it needs. A physical control's identity is
//! bound once two consecutive reads show it at rest; its geometry is read
//! afresh every time and only the final probe before mouse-down judges it.

#[cfg(test)]
mod tests;

use super::platform::{
    Confirmation, InputTarget, PressIntent, PressRole, SiteObservation, TargetIdentity, TargetKind,
    TurnClockFacts, TurnPlan,
};
use super::verify::InputTicket;
use crate::monitor::DecisionContext;
use crate::schema::MjaiEvent;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    pub session: u64,
    pub table: u64,
    pub settings: u64,
    pub manual: u64,
}

impl Owner {
    fn same_scope(self, other: Self) -> bool {
        self.session == other.session
            && self.table == other.table
            && self.settings == other.settings
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub context: DecisionContext,
    pub owner: Owner,
    pub window: String,
    pub input: InputTicket,
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub retries: u32,
    pub verify_ms: u32,
    pub gap_ms: u32,
    pub ready_ms: u32,
    pub reload_after: u32,
}

pub struct Facts {
    pub context: Option<DecisionContext>,
    pub owner: Owner,
    pub window: Option<String>,
    pub enabled: bool,
    pub manual: bool,
    pub input: InputTicket,
    /// This is actual uplink evidence relative to input_ticket(), never a mouse result.
    pub command: bool,
    pub latest: Option<(DecisionContext, MjaiEvent)>,
}

/// What the site showed for this call.
#[derive(Clone, Copy)]
pub enum Observed<'a> {
    /// No read was made: only ownership, time and manual input are checked.
    Skipped,
    /// The site could not be read. The caller logs why.
    Failed,
    Seen(&'a SiteObservation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRequirement {
    Any,
    NonDiscard,
    Declaration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    Replaced,
    Ownership,
    Context,
    Window,
    /// Other client input arrived after the request.
    Input,
    Expired,
    /// The deadline passed before the current step's control was ever seen at rest.
    TargetUnavailable,
    /// The site showed a different physical control, hand or native window.
    TargetChanged,
    /// The site reported geometry or identity no physical control can have.
    InvalidTarget,
    UnknownClock,
    Unconfirmed,
    DispatchUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    ClientAccepted,
    ServerConfirmed,
    Cancelled(CancelReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permit {
    run: u64,
    serial: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Finding the current step's control at rest, then waiting for pacing.
    Aiming,
    /// A single-use permit is out; the pointer travels and probes.
    Pressing,
    Dispatching,
    AwaitingCommand,
    /// A local menu was opened; waiting for the next control to appear.
    AwaitingReveal,
    /// The mouse sequence has not yet produced a declaration receipt.
    AwaitingDeclaration,
    /// The client sent the declaration (or its exact server echo arrived).
    /// Wait for the echoed continuation within the original turn deadline.
    Continuation,
    PausedManual,
    Finished(Outcome),
}

pub enum Effect {
    Observe,
    YieldManual,
    Pace {
        clock: TurnClockFacts,
        target: InputTarget,
    },
    WaitUntil(Instant),
    Press {
        permit: Permit,
        target: InputTarget,
        intent: Box<PressIntent>,
        attempt: u32,
    },
    Finished {
        outcome: Outcome,
        reload: bool,
    },
}

#[derive(Debug, Clone)]
struct Aim {
    /// Bound when two consecutive reads agree; fixed until the next dispatch.
    control: Option<TargetIdentity>,
    /// After a dispatch the next control may live in another native window.
    rebind: bool,
    not_before: Instant,
    last: Option<(TargetKind, InputTarget)>,
}

impl Aim {
    fn fresh(not_before: Instant, rebind: bool) -> Box<Self> {
        Box::new(Self {
            control: None,
            rebind,
            not_before,
            last: None,
        })
    }
}

#[derive(Debug, Clone)]
enum Awaiting {
    Command,
    Reveal(TargetIdentity),
    Declaration,
}

#[derive(Debug, Clone)]
enum Resume {
    Aim {
        rebind: bool,
    },
    Reveal {
        revealed: TargetIdentity,
        verify_at: Instant,
    },
}

#[derive(Debug, Clone)]
enum State {
    Aiming(Box<Aim>),
    Pressing {
        aim: Box<Aim>,
        kind: TargetKind,
        permit: Permit,
    },
    Dispatching {
        kind: TargetKind,
        identity: TargetIdentity,
        permit: Permit,
    },
    Awaiting {
        what: Awaiting,
        verify_at: Instant,
        not_before: Instant,
    },
    Continuation,
    Paused(Resume),
    Finished(Outcome),
}

/// The pacing policy is asked once per selected action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pace {
    Unrequested,
    Requested,
    Until(Instant),
}

#[derive(Clone, Copy)]
enum Validity {
    Valid,
    Manual,
    Invalid(CancelReason),
}

struct Run {
    id: u64,
    binding: Binding,
    original: DecisionContext,
    action: MjaiEvent,
    plan: TurnPlan,
    settings: Settings,
    step: usize,
    attempts: u32,
    /// Every control of this run belongs to this hand and native window.
    hand: Option<Vec<String>>,
    native: Option<String>,
    clock: Option<TurnClockFacts>,
    deadline: Instant,
    pace: Pace,
    /// The final command press has physically been dispatched at least once.
    committed: bool,
    reload: bool,
    state: State,
}

#[derive(Default)]
pub struct TurnController {
    run: Option<Run>,
    consumed: VecDeque<(DecisionContext, u64, u64, String)>,
    serial: u64,
    dead_clicks: u32,
}

impl TurnController {
    pub fn begin(
        &mut self,
        binding: Binding,
        action: MjaiEvent,
        plan: TurnPlan,
        settings: Settings,
        now: Instant,
    ) -> bool {
        let key = (
            binding.context,
            binding.owner.session,
            binding.owner.table,
            binding.window.clone(),
        );
        if self.consumed.contains(&key) || binding.window.is_empty() || !valid_plan(&action, &plan)
        {
            return false;
        }
        self.cancel(CancelReason::Replaced);
        self.consumed.push_back(key);
        if self.consumed.len() > 256 {
            self.consumed.pop_front();
        }
        self.serial += 1;
        self.run = Some(Run {
            id: self.serial,
            original: binding.context,
            binding,
            action,
            plan,
            settings,
            step: 0,
            attempts: 0,
            hand: None,
            native: None,
            clock: None,
            deadline: now + Duration::from_millis(u64::from(settings.ready_ms)),
            pace: Pace::Unrequested,
            committed: false,
            reload: false,
            state: State::Aiming(Aim::fresh(now, false)),
        });
        true
    }
    pub fn phase(&self) -> Option<Phase> {
        self.run.as_ref().map(|run| match &run.state {
            State::Aiming(_) => Phase::Aiming,
            State::Pressing { .. } => Phase::Pressing,
            State::Dispatching { .. } => Phase::Dispatching,
            State::Awaiting { what, .. } => match what {
                Awaiting::Command => Phase::AwaitingCommand,
                Awaiting::Reveal(_) => Phase::AwaitingReveal,
                Awaiting::Declaration => Phase::AwaitingDeclaration,
            },
            State::Continuation => Phase::Continuation,
            State::Paused(_) => Phase::PausedManual,
            State::Finished(outcome) => Phase::Finished(*outcome),
        })
    }
    pub fn input_ticket(&self) -> Option<InputTicket> {
        self.run.as_ref().map(|run| run.binding.input)
    }
    pub fn intent(&self) -> Option<PressIntent> {
        self.run
            .as_ref()
            .and_then(|run| run.plan.presses.get(run.step).cloned())
    }
    pub fn requirement(&self) -> InputRequirement {
        self.run
            .as_ref()
            .map_or(InputRequirement::NonDiscard, |run| {
                if matches!(run.action, MjaiEvent::Reach { .. }) && !run.declared() {
                    InputRequirement::Declaration
                } else if run
                    .plan
                    .presses
                    .get(run.step)
                    .is_some_and(|intent| intent.role == PressRole::Discard)
                {
                    InputRequirement::Any
                } else {
                    InputRequirement::NonDiscard
                }
            })
    }
    pub fn cancel(&mut self, reason: CancelReason) {
        if let Some(run) = &mut self.run {
            if !matches!(run.state, State::Finished(_)) {
                run.finish(Outcome::Cancelled(reason));
            }
        }
    }
    pub fn new_game(&mut self) {
        self.run = None;
        self.consumed.clear();
        self.dead_clicks = 0;
    }

    /// Called once after the common pacing policy has chosen an absolute time.
    pub fn paced(&mut self, until: Instant) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        if !matches!(run.state, State::Aiming(_)) || run.pace != Pace::Requested {
            return false;
        }
        run.pace = Pace::Until(until.min(run.deadline));
        true
    }

    pub fn next(&mut self, facts: &Facts, observed: Observed<'_>, now: Instant) -> Effect {
        let Some(run) = &mut self.run else {
            return Effect::Finished {
                outcome: Outcome::Cancelled(CancelReason::Context),
                reload: false,
            };
        };
        if let State::Finished(_) = run.state {
            return run.effect();
        }
        if !facts.enabled || !facts.owner.same_scope(run.binding.owner) {
            return run.finish(Outcome::Cancelled(CancelReason::Ownership));
        }
        if let Observed::Seen(observation) = observed {
            run.anchor_clock(facts, observation);
        }
        // A server echo is independent evidence. Never label mouse dispatch as accepted.
        if run.committed
            && run.step + 1 == run.plan.presses.len()
            && facts.latest.as_ref().is_some_and(|(context, action)| {
                same_round(*context, run.binding.context)
                    && context.event_seq > run.binding.context.event_seq
                    && confirms(&run.action, action)
            })
        {
            self.dead_clicks = 0;
            return run.finish(Outcome::ServerConfirmed);
        }
        if now >= run.deadline {
            let never_seen = matches!(&run.state, State::Aiming(aim) if aim.control.is_none());
            return run.finish(Outcome::Cancelled(if never_seen {
                CancelReason::TargetUnavailable
            } else {
                CancelReason::Expired
            }));
        }
        if matches!(
            run.state,
            State::Awaiting {
                what: Awaiting::Declaration,
                ..
            } | State::Continuation
        ) {
            return run.declaration(facts, observed, now);
        }
        // Our own command can close the context (a win ends the round). A
        // context replaced by another live window is a different matter.
        if run.committed && facts.command && facts.context.is_none() {
            self.dead_clicks = 0;
            return run.finish(Outcome::ClientAccepted);
        }
        let validity = run.validity(facts, now);
        match validity {
            Validity::Invalid(CancelReason::Input) if run.committed && facts.command => {
                self.dead_clicks = 0;
                return run.finish(Outcome::ClientAccepted);
            }
            Validity::Invalid(reason) => return run.finish(Outcome::Cancelled(reason)),
            Validity::Valid | Validity::Manual => {}
        }
        if let State::Awaiting {
            what: Awaiting::Command,
            verify_at,
            ..
        } = run.state
        {
            if now < verify_at {
                return Effect::Observe;
            }
            if run.attempts >= run.settings.retries {
                self.dead_clicks = self.dead_clicks.saturating_add(1);
                run.reload =
                    run.settings.reload_after > 0 && self.dead_clicks >= run.settings.reload_after;
                if run.reload {
                    self.dead_clicks = 0;
                }
                return run.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
            }
            // Any control the site matches to this step may be pressed again:
            // a closed candidate menu brings back its opening button.
            run.attempts += 1;
            run.state = State::Aiming(Aim::fresh(now, false));
            tracing::info!(context=?run.binding.context, attempt=run.attempts, "autoplay: input unconfirmed; retrying within the original turn deadline");
        }
        if let State::Dispatching { .. } = run.state {
            return Effect::Observe;
        }
        if let State::Paused(resume) = &run.state {
            if facts.manual {
                return Effect::YieldManual;
            }
            // Only the manual revision may change. The real context, window,
            // input ticket, session/settings and original deadline survived
            // the checks above; the next target is observed and bound anew.
            run.binding.owner.manual = facts.owner.manual;
            run.state = match resume.clone() {
                Resume::Aim { rebind } => State::Aiming(Aim::fresh(now, rebind)),
                Resume::Reveal {
                    revealed,
                    verify_at,
                } => State::Awaiting {
                    what: Awaiting::Reveal(revealed),
                    verify_at,
                    not_before: now,
                },
            };
            tracing::info!(context=?run.binding.context, "autoplay: selected turn resumed after manual input; resolving a fresh target");
        } else if let Validity::Manual = validity {
            run.pause();
            return Effect::YieldManual;
        }
        if let State::Pressing { .. } = run.state {
            return Effect::Observe;
        }
        if let State::Awaiting {
            what: Awaiting::Reveal(revealed),
            verify_at,
            not_before,
        } = &run.state
        {
            if now < *not_before {
                return Effect::WaitUntil((*not_before).min(run.deadline));
            }
            let Observed::Seen(observation) = observed else {
                return Effect::Observe;
            };
            let Some(target) = observation.target.as_ref().filter(|_| observation.ready) else {
                return Effect::Observe;
            };
            if observation.target_kind == TargetKind::Reveal && target.identity == *revealed {
                if now < *verify_at {
                    return Effect::Observe;
                }
                if run.attempts >= run.settings.retries {
                    return run.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
                }
                run.attempts += 1;
                run.state = State::Aiming(Box::new(Aim {
                    control: Some(revealed.clone()),
                    rebind: false,
                    not_before: now,
                    last: None,
                }));
            } else {
                // The opened menu shows the next control, possibly in a new window.
                run.state = State::Aiming(Aim::fresh(now, true));
            }
        }
        run.aim(observed, now, &mut self.serial)
    }

    /// The pointer invokes this after travel/hover and a fresh read-only probe.
    /// `None` means the probe failed. A permit is spent whatever the answer;
    /// a moved or briefly missing control only sends the turn back to aiming.
    pub fn consume(
        &mut self,
        permit: Permit,
        facts: &Facts,
        observation: Option<&SiteObservation>,
        point: (f64, f64),
        remaining_ms: u32,
        now: Instant,
    ) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        let State::Pressing {
            aim,
            kind,
            permit: held,
        } = &run.state
        else {
            return false;
        };
        if *held != permit {
            return false;
        }
        let (aim, kind) = (aim.clone(), *kind);
        match run.validity(facts, now) {
            Validity::Invalid(reason) => {
                run.finish(Outcome::Cancelled(reason));
                return false;
            }
            Validity::Manual => {
                run.pause();
                return false;
            }
            Validity::Valid => {}
        }
        if now + Duration::from_millis(u64::from(remaining_ms)) >= run.deadline {
            run.finish(Outcome::Cancelled(CancelReason::Expired));
            return false;
        }
        let target = observation
            .filter(|observation| observation.ready && observation.target_kind == kind)
            .and_then(|observation| observation.target.as_ref());
        match target {
            Some(target) if aim.control.as_ref() != Some(&target.identity) => {
                run.finish(Outcome::Cancelled(CancelReason::TargetChanged));
                false
            }
            Some(target)
                if valid_target(target)
                    && target.hit.contains(point.0, point.1)
                    && target.viewport.contains(point.0, point.1) =>
            {
                run.state = State::Dispatching {
                    kind,
                    identity: target.identity.clone(),
                    permit,
                };
                true
            }
            _ => {
                tracing::info!(context=?run.binding.context, "autoplay: control moved or unseen before mouse-down; aiming again");
                run.state = State::Aiming(Box::new(Aim { last: None, ..*aim }));
                false
            }
        }
    }

    /// Travel has no mouse-down. A manual interruption can yield the same
    /// selected action; an unexplained physical failure remains terminal.
    pub fn approach_failed(&mut self, facts: &Facts, now: Instant) {
        let Some(run) = &mut self.run else { return };
        if !matches!(run.state, State::Aiming(_)) {
            return;
        }
        if let Validity::Manual = run.validity(facts, now) {
            run.pause();
        } else {
            run.finish(Outcome::Cancelled(CancelReason::DispatchUnknown));
        }
    }

    pub fn dispatched(&mut self, permit: Permit, sent: bool, facts: &Facts, now: Instant) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        match &run.state {
            State::Pressing { permit: held, .. } if *held == permit && !sent => {
                // The final probe was never reached.
                if let Validity::Manual = run.validity(facts, now) {
                    run.pause();
                } else {
                    run.finish(Outcome::Cancelled(CancelReason::DispatchUnknown));
                }
                false
            }
            State::Dispatching {
                kind,
                identity,
                permit: held,
            } if *held == permit => {
                let (kind, identity) = (*kind, identity.clone());
                if !sent {
                    run.finish(Outcome::Cancelled(CancelReason::DispatchUnknown));
                    return false;
                }
                let confirmation = run.plan.presses[run.step].confirmation;
                run.committed |=
                    kind == TargetKind::Commit && confirmation == Confirmation::ClientCommand;
                // Zero disables automatic verification/retry. It does not turn a
                // dispatched mouse sequence into a fabricated command receipt.
                let verify_at = if run.settings.verify_ms == 0 {
                    run.deadline
                } else {
                    (now + Duration::from_millis(u64::from(run.settings.verify_ms)))
                        .min(run.deadline)
                };
                let (what, not_before) = match (kind, confirmation) {
                    (TargetKind::Reveal, _) => (
                        Awaiting::Reveal(identity),
                        now + Duration::from_millis(u64::from(run.settings.gap_ms)),
                    ),
                    (_, Confirmation::Dispatch) => {
                        run.advance(now);
                        return true;
                    }
                    (_, Confirmation::ClientCommand) => (Awaiting::Command, now),
                    (_, Confirmation::DeclarationEcho) => (Awaiting::Declaration, now),
                };
                run.state = State::Awaiting {
                    what,
                    verify_at,
                    not_before,
                };
                true
            }
            _ => false,
        }
    }
}

impl Run {
    fn validity(&self, facts: &Facts, now: Instant) -> Validity {
        if !facts.enabled || !facts.owner.same_scope(self.binding.owner) {
            Validity::Invalid(CancelReason::Ownership)
        } else if facts.context != Some(self.binding.context) {
            Validity::Invalid(CancelReason::Context)
        } else if facts.window.as_deref() != Some(self.binding.window.as_str()) {
            Validity::Invalid(CancelReason::Window)
        } else if facts.input != self.binding.input {
            Validity::Invalid(CancelReason::Input)
        } else if now >= self.deadline {
            Validity::Invalid(CancelReason::Expired)
        } else if facts.manual || facts.owner.manual != self.binding.owner.manual {
            Validity::Manual
        } else {
            Validity::Valid
        }
    }

    /// Readiness and manual input may precede target binding. Anchor the
    /// real protocol clock once, rather than expiring a paused turn by the
    /// provisional readiness timeout or restarting its budget on resume.
    fn anchor_clock(&mut self, facts: &Facts, observation: &SiteObservation) {
        if self.clock.is_some()
            || facts.context != Some(self.binding.context)
            || facts.window.as_deref() != Some(self.binding.window.as_str())
            || facts.input != self.binding.input
        {
            return;
        }
        if let Some(clock) = observation.clock {
            self.deadline = clock.started_at
                + Duration::from_millis(
                    u64::from(clock.fixed_ms) + u64::from(clock.bank_at_start_ms.unwrap_or(0)),
                );
            self.clock = Some(clock);
        }
    }

    fn aim(&mut self, observed: Observed<'_>, now: Instant, serial: &mut u64) -> Effect {
        let State::Aiming(aim) = &mut self.state else {
            return Effect::Observe;
        };
        if now < aim.not_before {
            return Effect::WaitUntil(aim.not_before.min(self.deadline));
        }
        let observation = match observed {
            Observed::Skipped => return Effect::Observe,
            Observed::Failed => {
                aim.last = None;
                return Effect::Observe;
            }
            Observed::Seen(observation) => observation,
        };
        let Some(target) = observation.target.as_ref().filter(|_| observation.ready) else {
            aim.last = None;
            return Effect::Observe;
        };
        if !valid_target(target) {
            return self.finish(Outcome::Cancelled(CancelReason::InvalidTarget));
        }
        let changed = self
            .hand
            .as_ref()
            .is_some_and(|hand| hand != &target.identity.hand)
            || (!aim.rebind
                && self
                    .native
                    .as_ref()
                    .is_some_and(|window| window != &target.identity.window))
            || aim
                .control
                .as_ref()
                .is_some_and(|control| control != &target.identity);
        if changed {
            return self.finish(Outcome::Cancelled(CancelReason::TargetChanged));
        }
        let at_rest = aim.last.as_ref().is_some_and(|(kind, last)| {
            *kind == observation.target_kind
                && last.identity == target.identity
                && same_place(last, target)
        });
        aim.last = Some((observation.target_kind, target.clone()));
        if !at_rest {
            return Effect::Observe;
        }
        if aim.control.is_none() {
            tracing::info!(control=%target.identity.control, bounds=?target.bounds, hit=?target.hit, "autoplay: control at rest");
            aim.control = Some(target.identity.clone());
            aim.rebind = false;
            self.hand = Some(target.identity.hand.clone());
            self.native = Some(target.identity.window.clone());
        }
        match self.pace {
            Pace::Unrequested => {
                let Some(clock) = self.clock else {
                    return self.finish(Outcome::Cancelled(CancelReason::UnknownClock));
                };
                self.pace = Pace::Requested;
                return Effect::Pace {
                    clock,
                    target: target.clone(),
                };
            }
            Pace::Requested => return Effect::Observe,
            Pace::Until(until) if now < until => return Effect::WaitUntil(until),
            Pace::Until(_) => {}
        }
        *serial += 1;
        let permit = Permit {
            run: self.id,
            serial: *serial,
        };
        let aim = aim.clone();
        self.state = State::Pressing {
            aim,
            kind: observation.target_kind,
            permit,
        };
        Effect::Press {
            permit,
            target: target.clone(),
            intent: Box::new(self.plan.presses[self.step].clone()),
            attempt: self.attempts,
        }
    }

    fn declaration(&mut self, facts: &Facts, observed: Observed<'_>, now: Instant) -> Effect {
        if !facts.input.declaration_compatible(self.binding.input) {
            return self.finish(Outcome::Cancelled(CancelReason::Context));
        }
        let observation = match observed {
            Observed::Seen(observation) => Some(observation),
            Observed::Skipped | Observed::Failed => None,
        };
        if self.accept_declaration(facts, observation) {
            self.advance(now);
            // This observation answered the declaration intent. Resolve
            // the next physical target only with the next intent.
            return Effect::Observe;
        }
        if facts.context != Some(self.binding.context) && !self.declaration_echo(facts) {
            return self.finish(Outcome::Cancelled(CancelReason::Context));
        }
        // Capture, tracker and the read-only browser probe advance
        // independently. The exact own echo can arrive after this
        // iteration's probe. Keep waiting for its fresh hand/window
        // proof. The click verification timeout applies only before
        // a real declaration receipt: it is not a server RTT limit.
        // Never press the old target or restart the turn clock.
        if let State::Awaiting { verify_at, .. } = self.state {
            if facts.input.reach_since(self.binding.input) || self.declaration_echo(facts) {
                self.state = State::Continuation;
                tracing::info!(context=?self.binding.context, "autoplay: declaration receipt observed; awaiting exact echoed discard continuation within original turn deadline");
            } else if now >= verify_at {
                return self.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
            }
        }
        Effect::Observe
    }

    fn accept_declaration(&mut self, facts: &Facts, observation: Option<&SiteObservation>) -> bool {
        let Some(declaration) =
            observation.and_then(|observation| observation.continuation.as_ref())
        else {
            return false;
        };
        let Some((context, _)) = &facts.latest else {
            return false;
        };
        let exact = self.declaration_echo(facts)
            && facts.input.declaration_compatible(self.binding.input)
            && facts.window.as_deref() == Some(declaration.window.as_str())
            && declaration.window != self.binding.window
            && self.hand.as_ref() == Some(&declaration.hand);
        if exact {
            self.binding.context = *context;
            self.binding.window = declaration.window.clone();
            self.binding.input = facts.input;
        }
        exact
    }

    fn declaration_echo(&self, facts: &Facts) -> bool {
        let Some((context, action)) = &facts.latest else {
            return false;
        };
        same_round(*context, self.original)
            && Some(context.event_seq) == self.original.event_seq.checked_add(1)
            && facts.context == Some(*context)
            && matches!((&self.action, action), (MjaiEvent::Reach { actor: selected, .. }, MjaiEvent::Reach { actor, .. }) if selected == actor)
    }

    /// A declaration was accepted once a declaration-echo step lies behind.
    fn declared(&self) -> bool {
        self.plan.presses[..self.step]
            .iter()
            .any(|press| press.confirmation == Confirmation::DeclarationEcho)
    }

    fn advance(&mut self, now: Instant) {
        self.step += 1;
        self.attempts = 0;
        self.state = State::Aiming(Aim::fresh(
            now + Duration::from_millis(u64::from(self.settings.gap_ms)),
            true,
        ));
    }

    fn pause(&mut self) {
        let resume = match &self.state {
            State::Aiming(aim) => Resume::Aim { rebind: aim.rebind },
            State::Awaiting {
                what: Awaiting::Reveal(revealed),
                verify_at,
                ..
            } => Resume::Reveal {
                revealed: revealed.clone(),
                verify_at: *verify_at,
            },
            _ => Resume::Aim { rebind: false },
        };
        if self.pace == Pace::Requested {
            self.pace = Pace::Unrequested;
        }
        tracing::info!(context=?self.binding.context, "autoplay: selected turn paused for manual input; old permission revoked");
        self.state = State::Paused(resume);
    }

    fn finish(&mut self, outcome: Outcome) -> Effect {
        self.state = State::Finished(outcome);
        self.effect()
    }

    fn effect(&self) -> Effect {
        let State::Finished(outcome) = self.state else {
            return Effect::Observe;
        };
        Effect::Finished {
            outcome,
            reload: self.reload,
        }
    }
}

fn same_round(a: DecisionContext, b: DecisionContext) -> bool {
    a.game == b.game && a.round == b.round
}
fn confirms(selected: &MjaiEvent, event: &MjaiEvent) -> bool {
    match (selected, event) {
        (
            MjaiEvent::Reach {
                actor: a,
                pai: Some(pai),
                tsumogiri: Some(tsumo),
            },
            MjaiEvent::Dahai {
                actor: b,
                pai: actual,
                tsumogiri,
            },
        ) => a == b && pai == actual && tsumo == tsumogiri,
        _ => selected == event,
    }
}
fn valid_rect(rect: super::context::CanvasRect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}
fn encloses(outer: super::context::CanvasRect, inner: super::context::CanvasRect) -> bool {
    outer.contains(inner.x, inner.y)
        && outer.contains(inner.x + inner.width, inner.y + inner.height)
}
fn valid_target(target: &InputTarget) -> bool {
    !target.identity.window.is_empty()
        && !target.identity.control.is_empty()
        && !target.identity.hand.is_empty()
        && valid_rect(target.bounds)
        && valid_rect(target.hit)
        && valid_rect(target.viewport)
        && encloses(target.hit, target.bounds)
        && encloses(target.viewport, target.hit)
}
/// Two reads of one control agree within a pixel: antialiased edges may
/// differ by a device pixel, an entering or moving control by far more.
fn same_place(a: &InputTarget, b: &InputTarget) -> bool {
    let near = |a: super::context::CanvasRect, b: super::context::CanvasRect| {
        (a.x - b.x).abs() <= 1.
            && (a.y - b.y).abs() <= 1.
            && (a.x + a.width - b.x - b.width).abs() <= 1.
            && (a.y + a.height - b.y - b.height).abs() <= 1.
    };
    a.viewport == b.viewport && near(a.bounds, b.bounds) && near(a.hit, b.hit)
}
fn valid_plan(action: &MjaiEvent, plan: &TurnPlan) -> bool {
    !plan.presses.is_empty() && plan.presses.last().is_some_and(|last| last.confirmation == Confirmation::ClientCommand)
        && plan.presses.iter().enumerate().all(|(index, intent)| {
            let same = intent.action == *action || (intent.role == PressRole::Discard && matches!((action, &intent.action),
                (MjaiEvent::Reach { actor:a, pai:Some(pai), tsumogiri:Some(tsumo) }, MjaiEvent::Dahai { actor:b, pai:actual, tsumogiri }) if a == b && pai == actual && tsumo == tsumogiri));
            same && (index + 1 == plan.presses.len() || intent.confirmation != Confirmation::ClientCommand)
        })
}
