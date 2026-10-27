//! Where the pointer is, and how it moves.
//!
//! A press needs the pointer on its target and nothing more, and for a long
//! time that was all the pointer did: it appeared on a tile, pressed, and
//! stayed there until the next press. On the table that reads as a tile
//! which lifts with nothing reaching for it, and one which stays lifted for
//! the rest of the round because the pointer never left.
//!
//! This module plans the rest of the pointer's life, all of it visible
//! through what the client draws under a pointer:
//!
//! - [`approach`] — the way to a press: it travels there instead of
//!   appearing, mostly settles on its target before pressing it, and may
//!   first rest on the runner-up when the bot was torn between two discards;
//! - [`leave`] — once the press has registered the pointer leaves the hand
//!   for where it waits, by the buttons (`humanize::rest_spot`);
//! - [`lean`] — between the seat's turns it looks, at its own pace, at the
//!   tiles the bot expects to throw next;
//! - [`aim`] — where in its target a press lands: scattered in the target's
//!   middle, never on the same few pixels;
//! - [`spots`] — where every other move ends: the rest by the buttons, the
//!   lift off the hand, a look at a tile, a press made again in place;
//! - [`rules`] — where the pointer may not go now and when it may set off,
//!   checked for every move and nowhere else;
//! - [`queue`] — the moves that can wait and be cut short, the rest after a
//!   press and the looks, queued by priority; a press brakes them;
//! - [`hand`] — the pointer's state over a game, and the one place every
//!   move and every press is sent to the page from.
//!
//! Three things are kept in one place each, so that no kind of move can
//! slip past what another kind obeys: where a move ends ([`aim`] for
//! presses, [`spots`] for the rest), whether and when it may go ([`rules`]),
//! and every time and distance drawn (`humanize`). [`hand`] alone sends
//! them, through `cdp_input`; everything else is arithmetic on points and
//! milliseconds. The approach
//! is made *inside* the thinking time the delay model chose, never after it:
//! the press lands at the instant it would have without any of this, so no
//! budget, floor or cap is affected.

pub mod aim;
pub mod approach;
pub mod hand;
pub mod layout;
pub mod lean;
pub mod queue;
pub mod rules;
pub mod spots;

pub use approach::{approach, changed_mind, runner_up, Approach, LOOK_MS, LOOK_REACH};
pub use hand::{ApproachInput, PointerController, Touch};
pub use layout::{PointerLayout, TileTarget};

use rand::Rng;

/// A point in CSS pixels.
pub type Point = (f64, f64);

/// One stretch of the pointer's way: travel to `to`, then stay for `dwell_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct Leg {
    pub to: Point,
    pub travel_ms: u32,
    pub dwell_ms: u32,
    /// How far the path leans off the straight line, as a fraction of its
    /// length. Signed: which side it leans to.
    pub bend: f64,
}

/// How long a hand takes to move the pointer `distance` pixels, on a canvas
/// whose 16:9 unit is `unit` pixels: as an aimed mouse movement takes (Fitts),
/// growing with the log of the distance in targets of about a tile's width. A
/// neighbouring tile takes some 0.3 s, the far side of the table some 0.7 s:
/// a long way is quicker per unit, never a flick.
pub fn travel_ms(distance: f64, unit: f64) -> u32 {
    if !(distance.is_finite() && unit.is_finite()) || unit <= 0.0 || distance < 1.0 {
        return 0;
    }
    (100.0 + 140.0 * (1.0 + distance / unit / 0.45).log2()).min(1100.0) as u32
}

/// About how long the pointer takes from one press of a plan to the next
/// ([`PointerController::reach`]): a button's way down to the hand, some four units.
pub fn next_press_ms() -> u32 {
    travel_ms(4.0, 1.0)
}

fn distance(a: Point, b: Point) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// The point `t` of the way (`0..=1` in time) along a leg from `from` to
/// `to`: a shallow arc, started and ended gently.
pub fn along(from: Point, to: Point, bend: f64, t: f64) -> Point {
    let t = t.clamp(0.0, 1.0);
    // Minimum-jerk easing: no speed and no acceleration at either end.
    let s = t * t * t * (10.0 - 15.0 * t + 6.0 * t * t);
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    // The arc's control point: the midpoint, pushed sideways.
    let control = (
        (from.0 + to.0) / 2.0 - dy * bend,
        (from.1 + to.1) / 2.0 + dx * bend,
    );
    let a = (1.0 - s) * (1.0 - s);
    let b = 2.0 * (1.0 - s) * s;
    let c = s * s;
    (
        a * from.0 + b * control.0 + c * to.0,
        a * from.1 + b * control.1 + c * to.1,
    )
}

/// The pointer leaving a press for `to`: a wait first — a hand does not
/// spring off the tile it has just let go, and how long it stays varies a
/// great deal: mostly about half a second, now and then at once, now and
/// then a second or two — given as the leg's dwell, which here comes
/// *before* the travel.
pub fn leave<R: Rng + ?Sized>(from: Point, to: Point, unit: f64, rng: &mut R) -> Leg {
    use crate::autoplay::humanize;
    Leg {
        to,
        travel_ms: humanize::TRAVEL.draw_ms(travel_ms(distance(from, to), unit), rng),
        dwell_ms: humanize::LINGER.draw_ms(rng),
        bend: humanize::BEND.draw(rng),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const UNIT: f64 = 100.0;

    #[test]
    fn a_leg_starts_and_ends_on_its_points_whatever_its_bend() {
        let (from, to) = ((120.0, 840.0), (910.0, 655.0));
        for bend in [-0.12, 0.0, 0.12] {
            assert_eq!(along(from, to, bend, 0.0), from);
            let end = along(from, to, bend, 1.0);
            assert!((end.0 - to.0).abs() < 1e-9 && (end.1 - to.1).abs() < 1e-9);
        }
    }

    #[test]
    fn a_leg_never_strays_further_than_its_bend() {
        let (from, to) = ((100.0, 800.0), (900.0, 800.0));
        for i in 0..=100 {
            let (x, y) = along(from, to, 0.12, f64::from(i) / 100.0);
            assert!((100.0..=900.0).contains(&x), "x left the span: {x}");
            // A quadratic arc peaks at half its control point's offset.
            assert!(
                (y - 800.0).abs() <= 800.0 * 0.12 / 2.0 + 1e-9,
                "y strayed: {y}"
            );
        }
    }

    #[test]
    fn a_leg_only_moves_forward() {
        let (from, to) = ((100.0, 800.0), (900.0, 700.0));
        let mut last = from.0;
        for i in 1..=200 {
            let (x, _) = along(from, to, 0.0, f64::from(i) / 200.0);
            assert!(x >= last, "went back at step {i}");
            last = x;
        }
    }

    #[test]
    fn travel_time_grows_with_distance_and_is_bounded() {
        assert_eq!(travel_ms(0.0, UNIT), 0);
        let near = travel_ms(79.0, UNIT);
        let far = travel_ms(900.0, UNIT);
        assert!(near > 250 && near < far, "{near} {far}");
        assert!(
            far < 800,
            "the far side of the table is not a slow crawl: {far}"
        );
        assert!(travel_ms(100_000.0, UNIT) <= 1100);
        assert_eq!(travel_ms(500.0, 0.0), 0);
    }

    #[test]
    fn a_hand_stays_on_its_press_for_a_varying_while_before_it_leaves() {
        let mut rng = StdRng::seed_from_u64(9);
        let mut waits: Vec<u32> = (0..600)
            .map(|_| leave((0.0, 0.0), (100.0, 0.0), UNIT, &mut rng).dwell_ms)
            .collect();
        waits.sort_unstable();
        assert!(waits.iter().all(|w| (120..=2500).contains(w)));
        let median = waits[waits.len() / 2];
        assert!((420..=700).contains(&median), "median {median}");
        // Now and then at once, now and then a second or more.
        assert!(
            waits[30] < 250 && waits[waits.len() - 30] > 1_100,
            "{:?}",
            (waits[30], waits[waits.len() - 30])
        );
    }
}
