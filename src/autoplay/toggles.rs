//! Auto-win scheduling is hand policy, not a UI phase. These due reads and
//! wishes never carry click authority; `ui::Machine` grants that separately.

use super::humanize::{AUTO_WIN_ON, TENPAI_WIN_ON};
use rand::Rng;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    RiichiWin,
    ReadTenpai,
    TenpaiWin,
}

#[derive(Debug, Default)]
pub struct Toggles {
    riichi_at: Option<Instant>,
    read_at: Option<Instant>,
    tenpai_at: Option<Instant>,
    tenpai_on: bool,
}

impl Toggles {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn riichi_accepted<R: Rng + ?Sized>(&mut self, now: Instant, rng: &mut R) {
        self.riichi_at = Some(now + Duration::from_millis(u64::from(AUTO_WIN_ON.draw_ms(rng))));
        self.read_at = None;
        self.tenpai_at = None;
    }
    pub fn discarded(&mut self, now: Instant) {
        self.read_at = Some(now + Duration::from_millis(400));
    }
    pub fn tenpai_read<R: Rng + ?Sized>(&mut self, win: bool, now: Instant, rng: &mut R) {
        self.tenpai_at = match self.tenpai_at {
            _ if !win || self.tenpai_on => None,
            Some(at) => Some(at),
            None => Some(now + Duration::from_millis(u64::from(TENPAI_WIN_ON.draw_ms(rng)))),
        };
    }
    pub fn win_read(&mut self, lit: Option<bool>) {
        self.tenpai_on = lit == Some(true);
    }
    pub fn due(&self) -> Option<Instant> {
        [self.riichi_at, self.read_at, self.tenpai_at]
            .into_iter()
            .flatten()
            .min()
    }
    pub fn next(&mut self, now: Instant) -> Option<Act> {
        for (at, act) in [
            (&mut self.riichi_at, Act::RiichiWin),
            (&mut self.read_at, Act::ReadTenpai),
            (&mut self.tenpai_at, Act::TenpaiWin),
        ] {
            if at.is_some_and(|at| now >= at) {
                *at = None;
                return Some(act);
            }
        }
        None
    }
    pub fn later(&mut self, act: Act, now: Instant, wait: Duration) {
        let slot = match act {
            Act::RiichiWin => &mut self.riichi_at,
            Act::ReadTenpai => &mut self.read_at,
            Act::TenpaiWin => &mut self.tenpai_at,
        };
        *slot = Some(now + wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};
    #[test]
    fn tenpai_changes_cancel_only_pending_wishes_and_reset_cancels_everything() {
        let now = Instant::now();
        let mut rng = StdRng::seed_from_u64(11);
        let mut s = Toggles::default();
        s.discarded(now);
        let read = s.due().unwrap();
        assert_eq!(read, now + Duration::from_millis(400));
        assert_eq!(s.next(read), Some(Act::ReadTenpai));
        s.tenpai_read(true, read, &mut rng);
        let at = s.due().unwrap();
        s.tenpai_read(true, read, &mut rng);
        assert_eq!(s.due(), Some(at));
        s.tenpai_read(false, read, &mut rng);
        assert!(s.due().is_none());
        s.tenpai_read(true, read, &mut rng);
        s.riichi_accepted(read, &mut rng);
        assert_eq!(s.next(s.due().unwrap()), Some(Act::RiichiWin));
        s.tenpai_read(true, read, &mut rng);
        s.win_read(Some(true));
        s.tenpai_read(true, read, &mut rng);
        assert!(s.due().is_none());
        s.riichi_accepted(read, &mut rng);
        s.reset();
        assert!(s.due().is_none());
    }
}
