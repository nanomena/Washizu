//! The sole selected-action lifetime. Sites supply observations, the pointer
//! supplies physical input, and this owner alone advances or retries a turn.

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
    Expired,
    Target,
    UnknownClock,
    Unconfirmed,
    DispatchUnknown,
    InvalidPlan,
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
    Preparing,
    WaitingReady,
    WaitingPacing,
    Permitted,
    Dispatching,
    WaitingCommand,
    WaitingDeclaration,
    Finished(Outcome),
}

pub enum Effect {
    Observe,
    Pace {
        clock: TurnClockFacts,
    },
    WaitUntil(Instant),
    Press {
        permit: Permit,
        target: InputTarget,
        intent: PressIntent,
        retry: u32,
    },
    Finished {
        outcome: Outcome,
        reload: bool,
    },
}

struct Run {
    id: u64,
    binding: Binding,
    original: DecisionContext,
    action: MjaiEvent,
    plan: TurnPlan,
    settings: Settings,
    phase: Phase,
    index: usize,
    retries: u32,
    permit: Option<Permit>,
    target: Option<InputTarget>,
    kind: TargetKind,
    hand: Option<Vec<String>>,
    native_window: Option<String>,
    may_rebind: bool,
    revealed: Option<(TargetIdentity, Instant)>,
    deadline: Instant,
    verify_at: Instant,
    ready_at: Instant,
    pacing_at: Option<Instant>,
    declaration: bool,
    committed: bool,
    reload: bool,
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
            phase: Phase::Preparing,
            index: 0,
            retries: 0,
            permit: None,
            target: None,
            kind: TargetKind::Commit,
            hand: None,
            native_window: None,
            may_rebind: false,
            revealed: None,
            deadline: now + Duration::from_millis(u64::from(settings.ready_ms)),
            verify_at: now,
            ready_at: now,
            pacing_at: None,
            declaration: false,
            committed: false,
            reload: false,
        });
        true
    }
    pub fn phase(&self) -> Option<Phase> {
        self.run.as_ref().map(|run| run.phase)
    }
    pub fn input_ticket(&self) -> Option<InputTicket> {
        self.run.as_ref().map(|run| run.binding.input)
    }
    pub fn intent(&self) -> Option<PressIntent> {
        self.run
            .as_ref()
            .and_then(|run| run.plan.presses.get(run.index).cloned())
    }
    pub fn requirement(&self) -> InputRequirement {
        self.run
            .as_ref()
            .map_or(InputRequirement::NonDiscard, |run| {
                if matches!(run.action, MjaiEvent::Reach { .. }) && !run.declaration {
                    InputRequirement::Declaration
                } else if run
                    .plan
                    .presses
                    .get(run.index)
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
            if !matches!(run.phase, Phase::Finished(_)) {
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
        if run.phase != Phase::WaitingPacing || run.pacing_at.is_some() {
            return false;
        }
        run.pacing_at = Some(until.min(run.deadline));
        run.phase = Phase::WaitingReady;
        true
    }

    pub fn next(
        &mut self,
        facts: &Facts,
        observation: Option<&SiteObservation>,
        now: Instant,
    ) -> Effect {
        let Some(run) = &mut self.run else {
            return Effect::Finished {
                outcome: Outcome::Cancelled(CancelReason::Context),
                reload: false,
            };
        };
        if let Phase::Finished(outcome) = run.phase {
            return Effect::Finished {
                outcome,
                reload: run.reload,
            };
        }
        if !facts.enabled || facts.manual || facts.owner != run.binding.owner {
            run.finish(Outcome::Cancelled(CancelReason::Ownership));
            return run.effect();
        }
        // A server echo is independent evidence. Never label mouse dispatch as accepted.
        if run.committed
            && run.index + 1 == run.plan.presses.len()
            && facts.latest.as_ref().is_some_and(|(context, action)| {
                same_round(*context, run.binding.context)
                    && context.event_seq > run.binding.context.event_seq
                    && confirms(&run.action, action)
            })
        {
            run.finish(Outcome::ServerConfirmed);
            self.dead_clicks = 0;
            return run.effect();
        }
        if now >= run.deadline {
            run.finish(Outcome::Cancelled(CancelReason::Expired));
            return run.effect();
        }
        if run.phase == Phase::WaitingDeclaration {
            if run.accept_declaration(facts, observation) {
                run.advance(now);
                // This observation answered the declaration intent. Resolve
                // the next physical target only with the next intent.
                return Effect::Observe;
            } else if facts.context == Some(run.binding.context) {
                if facts.input != run.binding.input && !facts.command {
                    run.finish(Outcome::Cancelled(CancelReason::Context));
                    return run.effect();
                }
                if now >= run.verify_at {
                    run.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
                    return run.effect();
                }
                return Effect::Observe;
            } else {
                run.finish(Outcome::Cancelled(CancelReason::Context));
                return run.effect();
            }
        }
        if facts.context != Some(run.binding.context) {
            run.finish(Outcome::Cancelled(CancelReason::Context));
            return run.effect();
        }
        if facts.window.as_deref() != Some(run.binding.window.as_str()) {
            run.finish(Outcome::Cancelled(CancelReason::Window));
            return run.effect();
        }
        if run.committed && facts.command {
            run.finish(Outcome::ClientAccepted);
            self.dead_clicks = 0;
            return run.effect();
        }
        if run.phase == Phase::WaitingCommand {
            if facts.input != run.binding.input {
                run.finish(Outcome::Cancelled(CancelReason::Context));
                return run.effect();
            }
            if now < run.verify_at {
                return Effect::Observe;
            }
            if run.retries >= run.settings.retries {
                self.dead_clicks = self.dead_clicks.saturating_add(1);
                run.reload =
                    run.settings.reload_after > 0 && self.dead_clicks >= run.settings.reload_after;
                if run.reload {
                    self.dead_clicks = 0;
                }
                run.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
                return run.effect();
            }
            run.retries += 1;
            run.phase = Phase::WaitingReady;
        } else if facts.input != run.binding.input {
            run.finish(Outcome::Cancelled(CancelReason::Context));
            return run.effect();
        }
        if matches!(
            run.phase,
            Phase::WaitingPacing | Phase::Permitted | Phase::Dispatching
        ) {
            return Effect::Observe;
        }
        if now < run.ready_at {
            return Effect::WaitUntil(run.ready_at.min(run.deadline));
        }
        let Some(observation) = observation else {
            return Effect::Observe;
        };
        if !observation.ready {
            return Effect::Observe;
        }
        let Some(target) = observation.target.as_ref() else {
            return Effect::Observe;
        };
        if let Some((previous, until)) = &run.revealed {
            if observation.target_kind == TargetKind::Reveal && previous == &target.identity {
                if now < *until {
                    return Effect::Observe;
                }
                if run.retries >= run.settings.retries {
                    run.finish(Outcome::Cancelled(CancelReason::Unconfirmed));
                    return run.effect();
                }
                run.retries += 1;
            } else {
                run.revealed = None;
            }
        }
        if !run.bind(target) {
            run.finish(Outcome::Cancelled(CancelReason::Target));
            return run.effect();
        }
        if run.pacing_at.is_none() {
            let Some(clock) = observation.clock else {
                run.finish(Outcome::Cancelled(CancelReason::UnknownClock));
                return run.effect();
            };
            run.deadline = clock.started_at
                + Duration::from_millis(
                    u64::from(clock.fixed_ms) + u64::from(clock.bank_at_start_ms.unwrap_or(0)),
                );
            if now >= run.deadline {
                run.finish(Outcome::Cancelled(CancelReason::Expired));
                return run.effect();
            }
            run.phase = Phase::WaitingPacing;
            return Effect::Pace { clock };
        }
        if let Some(until) = run.pacing_at.filter(|until| now < *until) {
            return Effect::WaitUntil(until);
        }
        self.serial += 1;
        let permit = Permit {
            run: run.id,
            serial: self.serial,
        };
        run.permit = Some(permit);
        run.kind = observation.target_kind;
        run.phase = Phase::Permitted;
        Effect::Press {
            permit,
            target: target.clone(),
            intent: run.plan.presses[run.index].clone(),
            retry: run.retries,
        }
    }

    /// The pointer invokes this after travel/hover and a fresh read-only probe.
    /// Rejected and accepted permits are both spent exactly once.
    pub fn consume(
        &mut self,
        permit: Permit,
        facts: &Facts,
        observation: &SiteObservation,
        point: (f64, f64),
        remaining_ms: u32,
        now: Instant,
    ) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        if run.phase != Phase::Permitted || run.permit != Some(permit) {
            return false;
        }
        let current = facts.enabled
            && !facts.manual
            && facts.owner == run.binding.owner
            && facts.context == Some(run.binding.context)
            && facts.window.as_deref() == Some(&run.binding.window)
            && facts.input == run.binding.input
            && now + Duration::from_millis(u64::from(remaining_ms)) < run.deadline
            && observation.ready
            && observation.target_kind == run.kind
            && observation.target == run.target
            && observation.target.as_ref().is_some_and(|target| {
                target.bounds.contains(point.0, point.1)
                    && target.viewport.contains(point.0, point.1)
            });
        if current {
            run.phase = Phase::Dispatching;
        } else {
            run.finish(Outcome::Cancelled(CancelReason::Target));
        }
        current
    }

    pub fn dispatched(&mut self, permit: Permit, sent: bool, now: Instant) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        if run.phase == Phase::Permitted && run.permit == Some(permit) && !sent {
            run.finish(Outcome::Cancelled(CancelReason::DispatchUnknown));
            return false;
        }
        if run.id != permit.run || run.phase != Phase::Dispatching || run.permit != Some(permit) {
            return false;
        }
        run.permit = None;
        if !sent {
            run.finish(Outcome::Cancelled(CancelReason::DispatchUnknown));
            return false;
        }
        run.committed |= run.kind == TargetKind::Commit
            && run.plan.presses[run.index].confirmation == Confirmation::ClientCommand;
        run.verify_at = (now + Duration::from_millis(u64::from(run.settings.verify_ms.max(1))))
            .min(run.deadline);
        if run.kind == TargetKind::Reveal {
            run.revealed = run
                .target
                .take()
                .map(|target| (target.identity, run.verify_at));
            run.may_rebind = true;
            run.ready_at = now + Duration::from_millis(u64::from(run.settings.gap_ms));
            run.phase = Phase::WaitingReady;
        } else {
            match run.plan.presses[run.index].confirmation {
                Confirmation::Dispatch => run.advance(now),
                Confirmation::ClientCommand => run.phase = Phase::WaitingCommand,
                Confirmation::DeclarationEcho => run.phase = Phase::WaitingDeclaration,
            }
        }
        true
    }
}

impl Run {
    fn finish(&mut self, outcome: Outcome) {
        self.phase = Phase::Finished(outcome);
        self.permit = None;
    }
    fn effect(&self) -> Effect {
        let Phase::Finished(outcome) = self.phase else {
            return Effect::Observe;
        };
        Effect::Finished {
            outcome,
            reload: self.reload,
        }
    }
    fn advance(&mut self, now: Instant) {
        self.index += 1;
        self.retries = 0;
        self.target = None;
        self.permit = None;
        self.may_rebind = true;
        self.phase = Phase::WaitingReady;
        self.ready_at = now + Duration::from_millis(u64::from(self.settings.gap_ms));
    }
    fn bind(&mut self, target: &InputTarget) -> bool {
        if target.identity.window.is_empty()
            || target.identity.control.is_empty()
            || target.identity.hand.is_empty()
            || !valid_rect(target.bounds)
            || !valid_rect(target.viewport)
            || !target.viewport.contains(target.bounds.x, target.bounds.y)
            || !target.viewport.contains(
                target.bounds.x + target.bounds.width,
                target.bounds.y + target.bounds.height,
            )
        {
            return false;
        }
        if let Some(hand) = &self.hand {
            if hand != &target.identity.hand {
                return false;
            }
        } else {
            self.hand = Some(target.identity.hand.clone());
        }
        if let Some(window) = &self.native_window {
            if window != &target.identity.window && !self.may_rebind {
                return false;
            }
        }
        if let Some(previous) = &self.target {
            if previous != target {
                return false;
            }
        }
        self.native_window = Some(target.identity.window.clone());
        self.may_rebind = false;
        self.target = Some(target.clone());
        true
    }
    fn accept_declaration(&mut self, facts: &Facts, observation: Option<&SiteObservation>) -> bool {
        let Some(declaration) =
            observation.and_then(|observation| observation.continuation.as_ref())
        else {
            return false;
        };
        let Some((context, action)) = &facts.latest else {
            return false;
        };
        let exact = same_round(*context, self.original)
            && Some(context.event_seq) == self.original.event_seq.checked_add(1)
            && facts.context == Some(*context)
            && facts.window.as_deref() == Some(declaration.window.as_str())
            && declaration.window != self.binding.window
            && matches!((&self.action, action), (MjaiEvent::Reach { actor: selected, .. }, MjaiEvent::Reach { actor, .. }) if selected == actor)
            && self.hand.as_ref() == Some(&declaration.hand);
        if exact {
            self.declaration = true;
            self.binding.context = *context;
            self.binding.window = declaration.window.clone();
            self.binding.input = facts.input;
        }
        exact
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
fn valid_plan(action: &MjaiEvent, plan: &TurnPlan) -> bool {
    !plan.presses.is_empty() && plan.presses.last().is_some_and(|last| last.confirmation == Confirmation::ClientCommand)
        && plan.presses.iter().enumerate().all(|(index, intent)| {
            let same = intent.action == *action || (intent.role == PressRole::Discard && matches!((action, &intent.action),
                (MjaiEvent::Reach { actor:a, pai:Some(pai), tsumogiri:Some(tsumo) }, MjaiEvent::Dahai { actor:b, pai:actual, tsumogiri }) if a == b && pai == actual && tsumo == tsumogiri));
            same && (index + 1 == plan.presses.len() || intent.confirmation != Confirmation::ClientCommand)
        })
}
