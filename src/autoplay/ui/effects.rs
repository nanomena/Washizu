//! A single pending effect owned by the UI machine. This component knows no
//! platform, phase, target, retry policy or transport. Its caller supplies
//! those decisions; it only makes issuance, permission and completion linear.

use std::time::Instant;

/// Opaque one-use identity. Only this machine-owned slot can issue one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    revision: u64,
    serial: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work<I> {
    Idle,
    At(Instant, I),
    Issued(Ticket, I),
    Permitted(Ticket, I),
    Refused(Ticket, I),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Completion<I> {
    pub input: I,
    /// Permission was consumed at the final boundary. A subsequent transport
    /// error still counts as an attempt, but cannot claim a successful click.
    pub permitted: bool,
}

#[derive(Debug)]
pub(crate) struct Effects<I> {
    revision: u64,
    serial: u64,
    work: Work<I>,
}

impl<I> Default for Effects<I> {
    fn default() -> Self {
        Self {
            revision: 0,
            serial: 0,
            work: Work::Idle,
        }
    }
}

impl<I: Copy + PartialEq> Effects<I> {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn revoke(&mut self) {
        self.revision += 1;
        self.cancel();
    }

    pub fn cancel(&mut self) {
        self.work = Work::Idle;
    }

    pub fn idle(&self) -> bool {
        self.work == Work::Idle
    }

    pub fn schedule(&mut self, at: Instant, input: I) {
        self.work = Work::At(at, input);
    }

    pub fn due(&self) -> Option<Instant> {
        match self.work {
            Work::At(at, _) => Some(at),
            _ => None,
        }
    }

    pub fn issue(&mut self, input: I) -> Ticket {
        self.serial += 1;
        let ticket = Ticket {
            revision: self.revision,
            serial: self.serial,
        };
        self.work = Work::Issued(ticket, input);
        ticket
    }

    pub fn tick(&mut self, now: Instant) -> Option<(Ticket, I)> {
        match self.work {
            Work::At(at, input) if now >= at => Some((self.issue(input), input)),
            _ => None,
        }
    }

    pub fn issued(&self, ticket: Ticket) -> Option<I> {
        match self.work {
            Work::Issued(current, input) if current == ticket => Some(input),
            _ => None,
        }
    }

    pub fn input(&self, ticket: Ticket) -> Option<I> {
        match self.work {
            Work::Issued(current, input)
            | Work::Permitted(current, input)
            | Work::Refused(current, input)
                if current == ticket =>
            {
                Some(input)
            }
            _ => None,
        }
    }

    pub fn observed(&mut self, ticket: Ticket, observe: I) -> bool {
        if self.issued(ticket) != Some(observe) {
            return false;
        }
        self.cancel();
        true
    }

    /// Both allowed and refused permissions are spent. No later callback can
    /// re-evaluate the same ticket against a more convenient observation.
    pub fn consume(&mut self, ticket: Ticket, allowed: bool) -> bool {
        let Some(input) = self.issued(ticket) else {
            return false;
        };
        self.work = if allowed {
            Work::Permitted(ticket, input)
        } else {
            Work::Refused(ticket, input)
        };
        allowed
    }

    /// A transport result cannot manufacture permission. Completion retires
    /// the slot before its caller advances a phase or schedules another look.
    pub fn complete(&mut self, ticket: Ticket, sent: bool) -> Option<Completion<I>> {
        let (input, permitted) = match self.work {
            Work::Permitted(current, input) if current == ticket => (input, true),
            Work::Issued(current, input) | Work::Refused(current, input)
                if current == ticket && !sent =>
            {
                (input, false)
            }
            _ => return None,
        };
        self.cancel();
        Some(Completion { input, permitted })
    }

    pub fn retire(&mut self, ticket: Ticket) {
        match self.work {
            Work::Issued(current, _) | Work::Permitted(current, _) | Work::Refused(current, _)
                if current == ticket =>
            {
                self.cancel();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Input {
        Observe,
        Press,
    }

    #[test]
    fn timer_only_issues_once_after_its_due_time() {
        let mut effects = Effects::default();
        let now = Instant::now();
        effects.schedule(now + Duration::from_secs(1), Input::Observe);
        assert!(effects.tick(now).is_none());
        let (ticket, input) = effects.tick(now + Duration::from_secs(1)).unwrap();
        assert_eq!(input, Input::Observe);
        assert!(effects.tick(now + Duration::from_secs(2)).is_none());
        assert!(effects.observed(ticket, Input::Observe));
        assert!(!effects.observed(ticket, Input::Observe));
        assert!(effects.idle());
    }

    #[test]
    fn observation_cannot_consume_a_press_and_refusal_cannot_be_rechecked() {
        let mut effects = Effects::default();
        let ticket = effects.issue(Input::Press);
        assert!(!effects.observed(ticket, Input::Observe));
        assert_eq!(effects.issued(ticket), Some(Input::Press));
        assert!(!effects.consume(ticket, false));
        assert_eq!(effects.input(ticket), Some(Input::Press));
        assert!(!effects.consume(ticket, true));
        assert!(effects.complete(ticket, true).is_none());
        assert_eq!(
            effects.complete(ticket, false),
            Some(Completion {
                input: Input::Press,
                permitted: false
            })
        );
    }

    #[test]
    fn permission_and_completion_are_each_consumed_once() {
        let mut effects = Effects::default();
        let ticket = effects.issue(Input::Press);
        assert!(effects.consume(ticket, true));
        assert!(!effects.consume(ticket, true));
        assert_eq!(
            effects.complete(ticket, true),
            Some(Completion {
                input: Input::Press,
                permitted: true
            })
        );
        let next = effects.issue(Input::Observe);
        assert!(effects.complete(ticket, true).is_none());
        assert!(effects.observed(next, Input::Observe));
    }

    #[test]
    fn transport_failure_after_permission_is_still_an_attempt() {
        let mut effects = Effects::default();
        let ticket = effects.issue(Input::Press);
        assert!(effects.consume(ticket, true));
        assert_eq!(
            effects.complete(ticket, false),
            Some(Completion {
                input: Input::Press,
                permitted: true
            })
        );
        assert!(effects.complete(ticket, false).is_none());
    }

    #[test]
    fn unissued_success_cannot_complete_or_advance_the_effect() {
        let mut effects = Effects::default();
        let ticket = effects.issue(Input::Press);
        assert!(effects.complete(ticket, true).is_none());
        assert_eq!(effects.issued(ticket), Some(Input::Press));
        assert_eq!(
            effects.complete(ticket, false),
            Some(Completion {
                input: Input::Press,
                permitted: false
            })
        );
    }

    #[test]
    fn revoked_and_replaced_callbacks_cannot_change_new_work() {
        let mut effects = Effects::default();
        let old = effects.issue(Input::Press);
        effects.revoke();
        assert_eq!(effects.revision(), 1);
        let at = Instant::now();
        effects.schedule(at, Input::Observe);
        assert!(!effects.consume(old, true));
        assert!(effects.complete(old, false).is_none());
        effects.retire(old);
        assert_eq!(effects.due(), Some(at));
        let (fresh, _) = effects.tick(at).unwrap();
        assert_ne!(fresh, old);
        assert!(!effects.observed(old, Input::Observe));
        assert!(effects.observed(fresh, Input::Observe));
        let first = effects.issue(Input::Press);
        effects.cancel();
        let second = effects.issue(Input::Press);
        assert_ne!(first, second);
        assert!(!effects.consume(first, true));
        assert!(effects.consume(second, true));
        effects.retire(second);
        assert!(effects.idle());
    }
}
