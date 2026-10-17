//! Every random draw autoplay makes about how its hand moves and waits.
//!
//! Nothing else in autoplay draws a number for a pause, a hold, a travel
//! time, a curve, a place or a choice of shape: it asks here, by name. One
//! table, so that two parts of the code cannot drift into two ideas of how a
//! hand behaves, and each shape is tuned in one place. Each kind of draw has
//! the shape people's own have:
//!
//! - **Times** are log-normal ([`Tail`], [`Scaled`]): mostly near their median,
//!   now and then a good deal longer, never one fixed rhythm. Bounds keep a
//!   tail from running away.
//! - **Waits for a moment** that could come at any time ([`Hazard`]) are
//!   exponential: no moment likelier than another, so the longer the chance
//!   lasts, the likelier it was taken.
//! - **Places** are normal about where they aim ([`Spread`]): mostly near, now
//!   and then further, never past a bound that would put them on something
//!   else. Where the hand waits between presses has a tail of its own
//!   ([`rest_spot`]): mostly near the buttons, now and then across the table.
//! - **Choices** are chances ([`chance`], [`one_of`], [`fraction`]).
//!
//! The think time before a decision is the delay model's to shape
//! (`autoplay::delay`, and the script it runs); it draws through the same
//! primitives ([`log_normal`], [`uniform_ms`]).

use rand::Rng;
use rand_distr::{Distribution, LogNormal, Normal};
use std::sync::OnceLock;

/// A log-normal draw about `median`, `spread` the deviation of its log, held
/// within `low..=high`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tail {
    pub median: f64,
    pub spread: f64,
    pub low: f64,
    pub high: f64,
}

impl Tail {
    pub fn draw<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
        if self.median <= 0.0 {
            return 0.0;
        }
        let value =
            LogNormal::new(self.median.ln(), self.spread).map_or(self.median, |d| d.sample(rng));
        value.clamp(self.low.min(self.high), self.high.max(self.low))
    }

    pub fn draw_ms<R: Rng + ?Sized>(&self, rng: &mut R) -> u32 {
        self.draw(rng).round() as u32
    }
}

/// A log-normal draw about a time the caller gives (one configured, or worked
/// out): within `low..=high` times it, and never under `floor` ms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scaled {
    pub spread: f64,
    pub low: f64,
    pub floor: f64,
    pub high: f64,
}

impl Scaled {
    pub fn draw_ms<R: Rng + ?Sized>(&self, base_ms: u32, rng: &mut R) -> u32 {
        let base = f64::from(base_ms);
        if base <= 0.0 {
            return 0;
        }
        let low = (base * self.low).max(self.floor).min(base);
        Tail {
            median: base,
            spread: self.spread,
            low,
            high: base * self.high,
        }
        .draw_ms(rng)
    }
}

/// A normal draw about zero, `sd` its deviation, never past `most` either way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spread {
    pub sd: f64,
    pub most: f64,
}

impl Spread {
    pub fn draw<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
        Normal::new(0.0, self.sd)
            .map_or(0.0, |d| d.sample(rng))
            .clamp(-self.most, self.most)
    }
}

/// A distance down from a point, never up: any length as likely to be where
/// it stops as the next (exponential, mean `mean`), never past `most`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drop {
    pub mean: f64,
    pub most: f64,
}

impl Drop {
    pub fn draw<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
        // Drawn again past `most` rather than piled up on it.
        for _ in 0..8 {
            let u: f64 = rng.random();
            let down = -self.mean * (1.0 - u).max(f64::MIN_POSITIVE).ln();
            if down <= self.most {
                return down;
            }
        }
        self.most
    }
}

/// A wait in which every moment is as likely as any other to be the one it
/// ends: at least `floor` ms, then exponential with mean `mean` ms, never
/// past `most`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hazard {
    pub floor: f64,
    pub mean: f64,
    pub most: f64,
}

impl Hazard {
    pub fn draw_ms<R: Rng + ?Sized>(&self, rng: &mut R) -> u32 {
        let u: f64 = rng.random();
        let wait = self.floor - self.mean * (1.0 - u).max(f64::MIN_POSITIVE).ln();
        wait.min(self.most).round() as u32
    }
}

// ---- Times ----

/// Over the target before pressing it, about the configured hover.
pub const HOVER: Scaled = Scaled {
    spread: 0.3,
    low: 0.6,
    floor: 80.0,
    high: 3.0,
};
/// The button held down, about the configured hold.
pub const HOLD: Scaled = Scaled {
    spread: 0.3,
    low: 0.5,
    floor: 40.0,
    high: 3.0,
};
/// From one press of a plan to the hand setting off for the next: the client
/// answers the first (the tiles a riichi may throw, a call's choices) and a
/// person sees it before moving. Never shorter than the configured gap
/// ([`BETWEEN_PRESSES`]).
pub const SEE_NEXT: Tail = Tail {
    median: 420.0,
    spread: 0.3,
    low: 250.0,
    high: 1200.0,
};
/// From one press of a plan to the hand setting off for the next, the
/// configured gap being `configured_ms`: about that, and never shorter than
/// it takes to see what the press brought up ([`SEE_NEXT`]).
pub fn next_press_gap<R: Rng + ?Sized>(configured_ms: u32, rng: &mut R) -> u32 {
    BETWEEN_PRESSES
        .draw_ms(configured_ms, rng)
        .max(SEE_NEXT.draw_ms(rng))
}
/// Between the presses of one plan, about the configured gap.
pub const BETWEEN_PRESSES: Scaled = Scaled {
    spread: 0.35,
    low: 0.6,
    floor: 0.0,
    high: 3.0,
};
/// How long a move takes, about what its length calls for: mostly within a
/// fifth either way, now and then nearly twice.
pub const TRAVEL: Scaled = Scaled {
    spread: 0.2,
    low: 0.7,
    floor: 0.0,
    high: 1.9,
};
/// How long a hand is still before it turns sharply: it never reverses or
/// cuts a corner at speed, whatever planned the next move.
pub const TURN_STOP: Tail = Tail {
    median: 280.0,
    spread: 0.3,
    low: 180.0,
    high: 600.0,
};
/// After throwing the tile it drew, as it meant to, a hand expecting to do
/// the same next turn lifts off the hand rather than going to the buttons:
/// how far above the top of the tiles, in 16:9 units — clear of them, so it
/// never rests where the next draw lands before the tile is there, and
/// nearer the hand than a rest by the buttons ...
pub const LIFT_UP: Tail = Tail {
    median: 1.2,
    spread: 0.3,
    low: 0.8,
    high: 2.4,
};
/// ... and how far to either side it drifts on the way: straight up, all but.
pub const LIFT_SIDE: Spread = Spread { sd: 0.1, most: 0.3 };
/// The longest the hand lingers on the tile it threw before it lifts: up
/// well before the next draw can land under it, however quick the turns
/// between.
pub const LIFT_LINGER_MOST_MS: u32 = 900;
/// A move shorter than this, in CSS pixels, is a nudge: made in one step,
/// with no way to turn from.
pub const NUDGE_PX: f64 = 6.0;
/// How hard a hand brakes when a press cuts its move short, in CSS pixels
/// per millisecond squared: from a brisk 1 px/ms to a stop in a tenth of a
/// second ...
pub const BRAKE_DECELERATION: f64 = 0.01;
/// ... never braking longer than this ...
pub const BRAKE_MOST_MS: f64 = 180.0;
/// ... and below this speed, in px/ms, as good as still already.
pub const STILL_SPEED: f64 = 0.05;
/// On what it pressed, before leaving it.
pub const LINGER: Tail = Tail {
    median: 550.0,
    spread: 0.6,
    low: 120.0,
    high: 2500.0,
};
/// From the seat's own move to its eye turning to the next turn.
pub const SETTLE: Tail = Tail {
    median: 2300.0,
    spread: 0.4,
    low: 1000.0,
    high: 8000.0,
};
/// From the frame that opens a decision — our draw, a discard we may call —
/// to the hand being able to move for it: the client's animation of the tile
/// coming in, and a person seeing what it is.
pub const SEE: Tail = Tail {
    median: 800.0,
    spread: 0.3,
    low: 500.0,
    high: 2200.0,
};
/// From the frame that opens the dealer's first turn — a new hand being
/// dealt — to the hand laid out, before [`SEE`] starts: Majsoul dropped
/// presses 2.4 s in and took them from 2.7 s.
pub const DEAL_MS: u32 = 2700;
/// From a changed mind to the pointer moving.
pub const REACTION: Tail = Tail {
    median: 650.0,
    spread: 0.35,
    low: 300.0,
    high: 2500.0,
};
/// Between looks at the next turn; its median moves with how sure the bot
/// is (see `pointer::lean::wait_ms`).
pub const GLANCE: Tail = Tail {
    median: 1700.0,
    spread: 0.45,
    low: 600.0,
    high: 6000.0,
};
/// After the user's own mouse rests, before autoplay has the pointer back:
/// half a second to a second, most often near the short end.
pub const HAND_BACK: Tail = Tail {
    median: 680.0,
    spread: 0.22,
    low: 500.0,
    high: 1000.0,
};
/// On a tile looked at on the way to a press, as a share of the rest the
/// delay model gave.
pub const LOOK_REST: Tail = Tail {
    median: 1.0,
    spread: 0.4,
    low: 0.5,
    high: 2.5,
};
/// On the tile pressed, before pressing it, as a share of that rest.
pub const SETTLE_REST: Tail = Tail {
    median: 0.55,
    spread: 0.5,
    low: 0.2,
    high: 2.0,
};
/// Added to the think when the hand presses another tile than the one it
/// was looking at between turns: a changed mind weighs the two before it
/// presses, as a torn hand does.
pub const CHANGED_MIND: Tail = Tail {
    median: 700.0,
    spread: 0.35,
    low: 350.0,
    high: 1500.0,
};
/// A hand weighing two tiles: how much longer each stop is than the one
/// before it, the wait before it moves the first.
pub const LONGER: Tail = Tail {
    median: 1.5,
    spread: 0.2,
    low: 1.15,
    high: 2.5,
};

/// Between two steps of a move: about a display frame, never quite even, as
/// a real mouse's events come.
pub fn frame_ms<R: Rng + ?Sized>(rng: &mut R) -> u64 {
    rng.random_range(11..=17)
}

// ---- Between decisions ----

/// From a hand's result arriving to the first look for its confirm button,
/// in ms: the client plays the win's or the draw's cut-in first.
pub const RESULT_LOOK_FIRST_MS: u64 = 1500;
/// Between two looks for the confirm button while it has not shown, in ms.
pub const RESULT_LOOK_EVERY_MS: u64 = 250;
/// From a confirm button showing to the first burst on it: a person sees it,
/// then any moment as likely as the next. The button counts down from three
/// and the client confirms by itself at nought, so the reaction, the move
/// there and the hover all fit in about three seconds.
pub const CONFIRM_SEEN: Hazard = Hazard {
    floor: 250.0,
    mean: 400.0,
    most: 1_300.0,
};
/// After a win of the seat's own, how many times longer the hand looks at
/// a confirm button before pressing it ([`CONFIRM_SEEN`]), never past
/// [`CONFIRM_SEEN_MOST_MS`]: a player looks at their own hand's score.
pub const OUR_BUTTON_LINGER: f64 = 1.5;
/// The longest the hand looks at a confirm button before setting off for it, in ms.
pub const CONFIRM_SEEN_MOST_MS: f64 = 1_600.0;
/// From a hand's result arriving to the hand starting to press its way
/// through it, when the screen cannot be read to see its button: a look at
/// the result, then any moment as likely as the next.
pub const RESULT_START: Hazard = Hazard {
    floor: 2100.0,
    mean: 3000.0,
    most: 30_000.0,
};
/// After a win of the seat's own, how many times longer the hand lets the
/// result show before it starts pressing through it, when it cannot see the
/// button ([`RESULT_START`]): a player looks at their own hand's score.
pub const OUR_WIN_LINGER: f64 = 2.5;
/// Between one burst of presses on the result and the next: a third of a
/// second on average, never under 120 ms.
pub const RESULT_PAUSE: Hazard = Hazard {
    floor: 120.0,
    mean: 200.0,
    most: 7_000.0,
};
/// The fewest and the most presses a burst has.
pub const BURST_LEAST: u32 = 2;
pub const BURST_MOST: u32 = 7;
/// Between the presses of a burst, from one's release to the next one's
/// press: a hand clicking as fast as it does.
pub const BURST_GAP: Tail = Tail {
    median: 150.0,
    spread: 0.35,
    low: 70.0,
    high: 600.0,
};
/// What a press of a burst holds the button about, in ms, before [`HOLD`]
/// draws it: quick, and with no hover first, the pointer already on it.
pub const TAP_HOLD_MS: u32 = 70;
/// How far, in the 16:9 unit, the pointer drifts between the presses of a
/// burst, sideways and up and down: a pixel or two, a hand clicking fast not
/// moving the mouse. A press pushes the mouse more along the hand than across
/// it, so mostly up and down.
pub const TAP_DRIFT_X: Spread = Spread {
    sd: 0.005,
    most: 0.02,
};
pub const TAP_DRIFT_Y: Spread = Spread {
    sd: 0.016,
    most: 0.05,
};
/// From one burst on the same button to the next, the hand still on it: it
/// stays where it was, moved a little more than between two presses, again
/// mostly up and down.
pub const STAY_DRIFT_X: Spread = Spread {
    sd: 0.012,
    most: 0.04,
};
pub const STAY_DRIFT_Y: Spread = Spread {
    sd: 0.035,
    most: 0.10,
};
/// From our riichi being accepted to the hand switching the client's
/// auto-win on: any moment as likely as the next, so an early riichi, with
/// longer to go, is the likelier to have it switched on.
pub const AUTO_WIN_ON: Hazard = Hazard {
    floor: 2000.0,
    mean: 25_000.0,
    most: 600_000.0,
};

/// From a game's end to the first look for its own screens, in ms, when its
/// last result is not being looked at: the last result plays first.
pub const END_LOOK_FIRST_MS: u64 = 1500;
/// Between two looks for the game's own screens while none is in sight, in
/// ms. They stay until pressed, so there is no hurry.
pub const END_LOOK_EVERY_MS: u64 = 400;
/// From one of the game's own screens after its end showing — the
/// standings, the rank, the room's rewards with "play again" — to the press
/// on it: a person reads it first, then any moment as likely as the next.
/// About two and a half seconds on average; nothing counts down there.
pub const END_SEEN: Hazard = Hazard {
    floor: 1500.0,
    mean: 1200.0,
    most: 8_000.0,
};
/// From a press on one of those screens to the look for the next: the next
/// is drawn first.
pub const END_PAUSE: Hazard = Hazard {
    floor: 400.0,
    mean: 300.0,
    most: 3_000.0,
};

/// How many presses a burst has: any number from [`BURST_LEAST`] to
/// [`BURST_MOST`], each as likely.
pub fn burst_len<R: Rng + ?Sized>(rng: &mut R) -> u32 {
    BURST_LEAST + one_of((BURST_MOST - BURST_LEAST + 1) as usize, rng) as u32
}

// ---- Who holds the pointer ----

/// How long a press waits for the user's hand to leave the mouse before it
/// goes ahead anyway: a turn's clock does not wait.
pub const USER_WAIT_MS: u64 = 1500;
/// How long the unhurried moves are put off at a time while the user's hand
/// is on the mouse.
pub const USER_PUT_OFF_MS: u64 = 300;
/// How long a press between decisions is put off at a time while the user's
/// hand is on the mouse, or our own decision is open.
pub const ACT_PUT_OFF_MS: u64 = 1200;
/// How long the client takes to draw a toggle's new state after a press,
/// before it is read again.
pub const TOGGLE_REDRAW_MS: u64 = 400;

// ---- Shapes of an approach ----

/// With no runner-up to look at, how often a press is settled on before it is made.
pub const SETTLE_CHANCE: f64 = 0.7;
/// With one within reach, how often the hand only settles on its choice ...
pub const ONLY_SETTLE: f64 = 0.3;
/// ... and how often it rests on the runner-up first; otherwise it goes
/// over to the runner-up and comes back.
pub const LOOK_FIRST: f64 = 0.42;

// ---- Places ----

/// How far a leg bows from straight.
pub const BEND: Spread = Spread {
    sd: 0.06,
    most: 0.18,
};
/// Off the middle of a tile looked at, across, in units of the canvas.
pub const LOOK_OFF_X: Spread = Spread {
    sd: 0.08,
    most: 0.2,
};
/// Where on a hand tile's height a hand puts the pointer, to press it or to
/// look at it: from just inside the top of its face (some 1.13 high about its
/// middle), and down from there, on average at the top quarter — mostly in
/// the upper half, never in the bottom fifth. In units of the canvas, down
/// from the middle positive.
pub const TILE_TOP: f64 = -0.50;
pub const TILE_DROP: Drop = Drop {
    mean: 0.22,
    most: 0.80,
};

/// Where on a hand tile's height the pointer goes, as an offset from its
/// middle: [`TILE_TOP`], then [`TILE_DROP`] down.
pub fn on_tile_y<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    TILE_TOP + TILE_DROP.draw(rng)
}
/// A press's scatter in its target, as a share of the target's reach: along
/// the way the pointer came, across it, and with no way known.
pub const AIM_ALONG: f64 = 0.38;
pub const AIM_ACROSS: f64 = 0.27;
pub const AIM_ANYWAY: f64 = 0.33;

/// This session's lean in a target, as a share of its reach: a little to
/// either side, and a little low rather than high. Drawn once.
pub fn session_lean() -> (f64, f64) {
    static LEAN: OnceLock<(f64, f64)> = OnceLock::new();
    *LEAN.get_or_init(|| {
        let mut rng = rand::rng();
        (rng.random_range(-0.12..0.12), rng.random_range(-0.04..0.16))
    })
}

/// Where the hand waits between presses, as a normalised 16:9 point. Mostly
/// near one place, a little up and to the left of the buttons; the further
/// from it, the rarer, now and then across the table. Never over the hand,
/// which would leave a tile lifted, and never by the edge of the game.
pub fn rest_spot<R: Rng + ?Sized>(rng: &mut R) -> (f64, f64) {
    const NEAR: Tail = Tail {
        median: 0.9,
        spread: 0.75,
        low: 0.0,
        high: 7.5,
    };
    let (bx, by) = crate::autoplay::majsoul::coords::ACTIONS[0];
    let home = (bx - 1.6, by - 0.7);
    let reach = NEAR.draw(rng);
    // Mostly to the left and a little up, the way a hand drifts off the buttons.
    let angle = std::f64::consts::PI * (1.1 + Spread { sd: 0.3, most: 0.9 }.draw(rng));
    let (x, y) = (
        home.0 + reach * angle.cos(),
        home.1 + reach * angle.sin() * 0.45,
    );
    (x.clamp(1.0, 15.0), y.clamp(3.0, 7.4))
}

/// A unit normal draw.
pub fn normal<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    Normal::new(0.0, 1.0).map_or(0.0, |d| d.sample(rng))
}

// ---- Choices ----

pub fn chance<R: Rng + ?Sized>(p: f64, rng: &mut R) -> bool {
    rng.random_bool(p.clamp(0.0, 1.0))
}

/// One of `n` (n > 0), each as likely.
pub fn one_of<R: Rng + ?Sized>(n: usize, rng: &mut R) -> usize {
    rng.random_range(0..n)
}

/// A number in `0..1`, for weighing choices.
pub fn fraction<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    rng.random()
}

// ---- For the delay model ----

/// A log-normal of `mu` and `sigma`, when they make one.
pub fn log_normal(mu: f64, sigma: f64) -> Option<LogNormal<f64>> {
    LogNormal::new(mu, sigma).ok()
}

/// An even draw in `lo..=hi` ms.
pub fn uniform_ms<R: Rng + ?Sized>(lo: u32, hi: u32, rng: &mut R) -> u32 {
    if hi <= lo {
        lo
    } else {
        rng.random_range(lo..=hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    /// Sorted draws, for their median and tail.
    fn draws(n: usize, mut f: impl FnMut(&mut StdRng) -> u32) -> Vec<u32> {
        let mut rng = StdRng::seed_from_u64(42);
        let mut all: Vec<u32> = (0..n).map(|_| f(&mut rng)).collect();
        all.sort_unstable();
        all
    }

    /// Every time is long-tailed: near its median mostly, the slowest
    /// twentieth well beyond it, all within its bounds.
    #[test]
    fn every_time_is_long_tailed_within_its_bounds() {
        for (name, tail) in [
            ("linger", LINGER),
            ("settle", SETTLE),
            ("reaction", REACTION),
            ("glance", GLANCE),
        ] {
            let all = draws(4000, |rng| tail.draw_ms(rng));
            assert!(
                all.iter()
                    .all(|&ms| f64::from(ms) >= tail.low && f64::from(ms) <= tail.high),
                "{name}"
            );
            let (median, slow) = (f64::from(all[2000]), f64::from(all[3800]));
            assert!(
                (median / tail.median - 1.0).abs() < 0.08,
                "{name} median {median}"
            );
            assert!(
                slow > median * 1.6,
                "{name}: the slowest twentieth from {slow}"
            );
        }
    }

    /// A time about a configured one keeps it as its median and runs long now and then.
    #[test]
    fn a_configured_time_is_its_median_with_a_tail() {
        let hover = draws(4000, |rng| HOVER.draw_ms(200, rng));
        assert!(hover.iter().all(|&ms| (120..=600).contains(&ms)));
        assert!((190..=210).contains(&hover[2000]), "median {}", hover[2000]);
        assert!(hover[3800] > 300, "tail {}", hover[3800]);
        let travel = draws(4000, |rng| TRAVEL.draw_ms(300, rng));
        assert!(travel.iter().all(|&ms| (210..=570).contains(&ms)));
        assert!(travel[3960] > 450, "tail {}", travel[3960]);
        assert_eq!(HOLD.draw_ms(0, &mut StdRng::seed_from_u64(1)), 0);
    }

    /// The hand-back stays within half a second to a second, as the user chose,
    /// most often near the short end.
    #[test]
    fn the_hand_back_is_half_a_second_to_a_second_mostly_short() {
        let all = draws(4000, |rng| HAND_BACK.draw_ms(rng));
        assert!(all.iter().all(|&ms| (500..=1000).contains(&ms)));
        assert!((640..=720).contains(&all[2000]), "median {}", all[2000]);
        assert!(all[3800] > 900, "now and then near the long end");
    }

    /// Where the hand waits: mostly near its home, the further the rarer,
    /// now and then far; never over the hand or by the edge.
    #[test]
    fn the_rest_spot_is_mostly_near_home_and_sometimes_far() {
        let mut rng = StdRng::seed_from_u64(7);
        let (bx, by) = crate::autoplay::majsoul::coords::ACTIONS[0];
        let home = (bx - 1.6, by - 0.7);
        let mut far: Vec<f64> = (0..4000)
            .map(|_| {
                let (x, y) = rest_spot(&mut rng);
                assert!(
                    (1.0..=15.0).contains(&x) && (3.0..=7.4).contains(&y),
                    "{x} {y}"
                );
                ((x - home.0).powi(2) + (y - home.1).powi(2)).sqrt()
            })
            .collect();
        far.sort_by(f64::total_cmp);
        assert!(far[2000] < 1.2, "mostly near: median {}", far[2000]);
        assert!(far[3900] > 2.5, "and now and then far: {}", far[3900]);
    }

    /// A wait for any moment: never before its floor nor past its most, its
    /// mean the floor and the mean, and no moment likelier than another —
    /// having lasted a while, it is as likely to go on as at the start.
    #[test]
    fn a_wait_for_any_moment_has_no_likelier_moment() {
        let wait = Hazard {
            floor: 1000.0,
            mean: 4000.0,
            most: 60_000.0,
        };
        let all = draws(20_000, |rng| wait.draw_ms(rng));
        assert!(all.iter().all(|&ms| (1000..=60_000).contains(&ms)));
        let mean = all.iter().map(|&ms| f64::from(ms)).sum::<f64>() / all.len() as f64;
        assert!((mean / 5000.0 - 1.0).abs() < 0.05, "mean {mean}");
        // Past the floor, lasting another 4 s is as likely from 1 s as from 9 s.
        let past = |from: u32| {
            let left: Vec<_> = all.iter().filter(|&&ms| ms >= from).collect();
            left.iter().filter(|&&&ms| ms >= from + 4000).count() as f64 / left.len() as f64
        };
        let (early, late) = (past(1000), past(9000));
        assert!((early - late).abs() < 0.04, "{early} {late}");
        assert!((early - (-1.0f64).exp()).abs() < 0.03, "{early}");
    }

    /// A burst is two presses to seven, each count as likely.
    #[test]
    fn a_burst_is_two_presses_to_seven_each_as_likely() {
        let all = draws(30_000, burst_len);
        assert!(all.iter().all(|&n| (2..=7).contains(&n)));
        for n in 2..=7 {
            let share = all.iter().filter(|&&m| m == n).count() as f64 / all.len() as f64;
            assert!((share - 1.0 / 6.0).abs() < 0.015, "{n}: {share}");
        }
    }

    /// A place's spread stays within its bound and is mostly small.
    #[test]
    fn a_spread_is_mostly_small_and_never_past_its_bound() {
        let mut rng = StdRng::seed_from_u64(9);
        let all: Vec<f64> = (0..4000).map(|_| BEND.draw(&mut rng)).collect();
        assert!(all.iter().all(|b| b.abs() <= BEND.most));
        let small = all.iter().filter(|b| b.abs() < BEND.sd).count();
        assert!(
            small > 2400,
            "about two in three within a deviation: {small}"
        );
    }
}
