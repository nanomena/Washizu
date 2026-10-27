//! What the pointer may do: where it may not go now, and when it may set off.
//!
//! Every move the pointer makes passes these checks at the one place moves
//! are sent ([`super::PointerController`]), whoever planned the move: the way to a press,
//! a plan's next press, a press made again, a press between decisions, the
//! rest after a press, the lift after a tsumogiri, the looks between turns.
//! A rule added here holds for all of them, and no rule is checked anywhere
//! else — a rule kept by the code that plans one kind of move is a rule
//! another kind of move can break.
//!
//! - [`refusal`]: where the pointer may not go now — where the next draw
//!   lands, while that tile is not on screen, unless it goes there to press
//!   what is there.
//! - [`turn_wait`]: when it may set off — a sharp turn only from a stop.
//!
//! Who holds the pointer is the hand's own business, not a rule about a
//! move: the user's hand on the mouse puts everything of ours off, and a
//! press cuts the unhurried moves short.

use super::Point;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::humanize;
use std::time::{Duration, Instant};

/// What the pointer knows of the table, for the rules.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sight {
    /// Where the next draw lands, in CSS pixels, and the canvas's 16:9 unit
    /// there in pixels, while that tile is not on screen. `None` once it is,
    /// and before the hand at rest has been seen.
    pub draw_slot: Option<CanvasRect>,
}

/// Why a move may not be made now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It ends where the next draw lands, before that tile is there: no
    /// hand goes to a tile it has not seen, however sure it is of throwing
    /// it.
    DrawNotThere,
}

/// What a move is for, as far as the rules go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// To a press: a decision's, or one between decisions. A press answers
    /// what is on screen — a decision comes for a frame that showed its tile
    /// — so it is never kept off where the draw lands: the bot's answer to
    /// our draw may come before the draw's own event does.
    Press,
    /// An unhurried move, a rest, a lift or a look: it goes only where the
    /// rules allow now.
    Errand,
}

/// Why a move ending at `to`, made for `bound`, may not be made now, if it
/// may not.
pub fn refusal(to: Point, bound: Bound, sight: &Sight) -> Option<Refusal> {
    if bound == Bound::Press {
        return None;
    }
    let slot = sight.draw_slot?;
    slot.contains(to.0, to.1).then_some(Refusal::DrawNotThere)
}

/// How long a move from `from` to `to` waits before it sets off, when it
/// turns sharply — by more than 60 degrees — from the last move (`last`: its
/// direction and when it ended): long enough that the hand has been still
/// [`humanize::TURN_STOP`] since. A gentle bend, a turn after such a stop
/// anyway (a press's hover and hold count), or a move of a few pixels waits
/// nothing.
pub fn turn_wait<R: rand::Rng + ?Sized>(
    last: Option<(Point, Instant)>,
    from: Point,
    to: Point,
    now: Instant,
    rng: &mut R,
) -> Option<Duration> {
    let (was, ended) = last?;
    let way = direction(from, to)?;
    if was.0 * way.0 + was.1 * way.1 >= 0.5 {
        return None;
    }
    let stop = Duration::from_millis(u64::from(humanize::TURN_STOP.draw_ms(rng)));
    stop.checked_sub(now.saturating_duration_since(ended))
        .filter(|wait| !wait.is_zero())
}

/// Which way a move from `from` to `to` goes, as a unit vector; `None` for a
/// move of a few pixels, which has no way worth turning from.
pub fn direction(from: Point, to: Point) -> Option<Point> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = (dx * dx + dy * dy).sqrt();
    (length >= humanize::NUDGE_PX).then(|| (dx / length, dy / length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const UNIT: f64 = 80.0;

    /// Where the next draw lands is refused while the tile is not there,
    /// and only there: the hand's own last tile, the lift above the slot
    /// and anything once the tile is on screen are not.
    #[test]
    fn where_the_draw_lands_is_refused_until_it_is_there() {
        let slot = (1000.0, 670.0);
        let waiting = Sight {
            draw_slot: Some(CanvasRect {
                x: 956.0,
                y: 610.0,
                width: 88.0,
                height: 120.0,
            }),
        };
        let errand = Bound::Errand;
        for to in [slot, (1030.0, 690.0), (970.0, 640.0)] {
            assert_eq!(
                refusal(to, errand, &waiting),
                Some(Refusal::DrawNotThere),
                "{to:?}"
            );
        }
        // The hand's last tile, a tile and the draw's gap to the left.
        assert_eq!(
            refusal((1000.0 - 1.04 * UNIT, 670.0), errand, &waiting),
            None
        );
        // Lifted clear of the tiles above the slot.
        assert_eq!(
            refusal((1000.0, 670.0 - 1.4 * UNIT), errand, &waiting),
            None
        );
        // The tile on screen: our turn.
        assert_eq!(refusal(slot, errand, &Sight::default()), None);
        // A press answers what is on screen, whatever the events say yet.
        assert_eq!(refusal(slot, Bound::Press, &waiting), None);
    }

    /// A move that reverses, or turns a corner, right after the last one
    /// waits for a stop; one that goes on the same way, or turns after a
    /// stop long enough, or barely moves, does not.
    #[test]
    fn a_sharp_turn_waits_for_a_stop_and_nothing_else_does() {
        let mut rng = StdRng::seed_from_u64(41);
        let ended = Instant::now();
        let right = Some(((1.0, 0.0), ended));
        let just_after = ended + Duration::from_millis(20);
        let from = (500.0, 500.0);
        // Back the way it came, and a right angle: a stop first.
        for to in [(300.0, 500.0), (500.0, 700.0)] {
            let wait = turn_wait(right, from, to, just_after, &mut rng).expect("a stop");
            let least = humanize::TURN_STOP.low as u64 - 20;
            assert!(wait >= Duration::from_millis(least), "{wait:?}");
            assert!(wait <= Duration::from_millis(humanize::TURN_STOP.high as u64));
        }
        // On the same way, or bending gently: no stop.
        assert_eq!(
            turn_wait(right, from, (700.0, 520.0), just_after, &mut rng),
            None
        );
        assert_eq!(
            turn_wait(right, from, (650.0, 620.0), just_after, &mut rng),
            None
        );
        // Still long enough already, or a nudge of a pixel or two: no stop.
        let later = ended + Duration::from_millis(humanize::TURN_STOP.high as u64);
        assert_eq!(
            turn_wait(right, from, (300.0, 500.0), later, &mut rng),
            None
        );
        assert_eq!(
            turn_wait(right, from, (497.0, 501.0), just_after, &mut rng),
            None
        );
        // Nothing moved before: nothing to turn from.
        assert_eq!(
            turn_wait(None, from, (300.0, 500.0), just_after, &mut rng),
            None
        );
    }
}
