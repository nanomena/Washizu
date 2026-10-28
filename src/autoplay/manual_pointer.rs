//! Physical user-pointer ownership. A focused rectangle is only a candidate:
//! on Windows the actual foreground and hit window must belong to this account.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
enum Phase {
    #[default]
    Outside,
    Moving,
    Resting(Instant),
}

#[derive(Default)]
pub(super) struct ManualPointer {
    phase: Phase,
    pub revision: u64,
    left: Option<(f64, f64)>,
}

impl ManualPointer {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Outside,
            revision: 0,
            left: None,
        }
    }
    pub fn report(
        &mut self,
        moving: bool,
        at: Option<(f64, f64)>,
        owned: bool,
        now: Instant,
        hand_back: Duration,
    ) {
        if !owned || (!moving && at.is_none()) {
            // Leaving the game is not resting over it. Another window must
            // neither retain ownership nor change autoplay's pointer origin.
            self.phase = Phase::Outside;
            self.left = None;
        } else if moving {
            if !matches!(self.phase, Phase::Moving) {
                self.revision += 1;
            }
            self.phase = Phase::Moving;
        } else if matches!(self.phase, Phase::Moving) {
            self.phase = Phase::Resting(now + hand_back);
            self.left = at;
        }
        // Repeated stationary samples never restart the hand-back timer.
    }

    pub fn active(&self, now: Instant) -> bool {
        match self.phase {
            Phase::Outside => false,
            Phase::Moving => true,
            Phase::Resting(until) => now < until,
        }
    }

    pub fn take_left(&mut self) -> Option<(f64, f64)> {
        self.left.take()
    }
}

#[cfg(any(windows, test))]
fn owners_match(expected: usize, foreground: usize, hit: usize) -> bool {
    expected != 0 && foreground == expected && hit == expected
}

pub(super) fn owns_physical_pointer(window: &str) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !window.is_empty() && window.len() <= 16 && window.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid desktop window identity"
    );
    let expected = usize::from_str_radix(window, 16)?;
    anyhow::ensure!(expected != 0, "Missing desktop window identity");
    #[cfg(windows)]
    return Ok(windows::owns(expected));
    // Other desktops retain the shell's focused-game rectangle proof. Native
    // HWND ownership is a Windows contract, never a guessed cross-OS handle.
    #[cfg(not(windows))]
    Ok(true)
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    type Hwnd = *mut c_void;
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(point: *mut Point) -> i32;
        fn GetForegroundWindow() -> Hwnd;
        fn WindowFromPoint(point: Point) -> Hwnd;
        fn GetAncestor(window: Hwnd, flags: u32) -> Hwnd;
    }
    pub fn owns(expected: usize) -> bool {
        // SAFETY: these read-only user32 queries accept opaque HWND values.
        // Point is initialized and lives across GetCursorPos. No input is sent.
        unsafe {
            let mut point = Point { x: 0, y: 0 };
            if GetCursorPos(&mut point) == 0 {
                return false;
            }
            let foreground = GetAncestor(GetForegroundWindow(), 3) as usize;
            let hit = GetAncestor(WindowFromPoint(point), 3) as usize;
            let owner = GetAncestor(expected as Hwnd, 3) as usize;
            super::owners_match(owner, foreground, hit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_window_covering_a_focused_account_cannot_claim_the_pointer() {
        assert!(owners_match(10, 10, 10));
        assert!(
            !owners_match(10, 10, 20),
            "focused account, pointer over another window"
        );
        assert!(!owners_match(10, 20, 10), "background overlapping account");
        assert!(!owners_match(0, 0, 0));
    }
    #[test]
    fn foreign_movement_never_pauses_or_changes_the_pointer_origin() {
        let mut p = ManualPointer::default();
        let now = Instant::now();
        p.report(true, None, false, now, Duration::from_millis(700));
        p.report(
            false,
            Some((20., 30.)),
            false,
            now,
            Duration::from_millis(700),
        );
        assert!(!p.active(now));
        assert_eq!(p.revision, 0);
        assert_eq!(p.take_left(), None);
    }
    #[test]
    fn only_resting_inside_keeps_a_short_hold_and_samples_do_not_extend_it() {
        let mut p = ManualPointer::default();
        let now = Instant::now();
        let wait = Duration::from_millis(700);
        p.report(true, None, true, now, wait);
        p.report(true, None, true, now, wait);
        assert_eq!(p.revision, 1);
        p.report(false, Some((20., 30.)), true, now, wait);
        p.report(
            false,
            Some((20., 30.)),
            true,
            now + Duration::from_millis(600),
            wait,
        );
        assert!(p.active(now + Duration::from_millis(699)));
        assert!(!p.active(now + wait));
        assert_eq!(p.take_left(), Some((20., 30.)));
        assert_eq!(p.take_left(), None);
    }
    #[test]
    fn leaving_the_game_or_switching_accounts_releases_without_a_rest_delay() {
        for foreign in [false, true] {
            let mut p = ManualPointer::default();
            let now = Instant::now();
            let wait = Duration::from_secs(1);
            p.report(true, None, true, now, wait);
            p.report(false, Some((20., 30.)), true, now, wait);
            assert!(p.active(now));
            p.report(
                false,
                if foreign { Some((40., 50.)) } else { None },
                !foreign,
                now,
                wait,
            );
            assert!(!p.active(now));
            assert_eq!(p.take_left(), None);
        }
    }
    #[test]
    fn window_identity_is_explicit_and_malformed_values_fail_closed() {
        for id in ["", "0", "0x123", "not-a-window", "10000000000000000"] {
            assert!(owns_physical_pointer(id).is_err(), "{id}");
        }
    }
}
