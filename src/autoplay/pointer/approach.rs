//! The pointer's way to a press.
//!
//! The way is made inside the thinking time the delay model chose: the pause
//! that leads the plan gives up exactly what the way takes, so the press lands
//! when it would have without it. Most presses are settled on first — the hand
//! arrives early and rests on its target — and a hand torn between two
//! discards may rest on the runner-up before its choice, each stop longer
//! than the one before.

use super::{distance, travel_ms, Leg, Point};
use crate::autoplay::delay::probs::Candidate;
use crate::autoplay::humanize::{
    self, LONGER, LOOK_FIRST, LOOK_REST, ONLY_SETTLE, SETTLE_CHANCE, SETTLE_REST,
};
use rand::Rng;

/// How long the pointer rests on a tile it looks at, unless the delay script
/// says otherwise (`look_ms` in its answer). A settle on the press before it
/// is made is measured against it too.
pub const LOOK_MS: u32 = 800;

/// How far apart, in 16:9 units, two tiles may sit for the pointer to go from
/// one to the other: three tiles, and the gap before the drawn one. Any
/// further and the look is a sweep across the rack, and nobody weighs two
/// tiles by waving between the ends of their hand.
pub const LOOK_REACH: f64 = 2.7;

/// The pointer's way to a press: wait `pause_ms`, then walk `legs`. The last
/// leg ends on the press point; one that dwells there settles on the target
/// before pressing it, one that does not presses as it arrives — the press's
/// own hover follows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Approach {
    pub pause_ms: u32,
    pub legs: Vec<Leg>,
}

impl Approach {
    /// Everything the approach takes, pause included.
    pub fn total_ms(&self) -> u32 {
        self.pause_ms
            + self
                .legs
                .iter()
                .map(|l| l.travel_ms + l.dwell_ms)
                .sum::<u32>()
    }

    /// Whether the way rests on the runner-up before the press.
    pub fn looks_elsewhere(&self) -> bool {
        self.legs.len() > 1
    }
}

/// Plan the pointer's way from `from` to a press at `press`, to take exactly
/// `think_ms`.
///
/// With a `look` at a runner-up that sits within reach of the press, the
/// pointer settles on the press alone; or rests on the runner-up first; or
/// rests on the press, goes over to the runner-up and comes back — the think
/// spread over the wait before it moves and its stops, each longer than the
/// one before (see [`weighed`]). With none, it mostly settles on the press,
/// for a while drawn afresh against `rest_ms`, and otherwise presses as it
/// arrives. Whatever does not fit in `think_ms` is dropped, the look before
/// the settle and the settle before the travel: a short think gets a plain
/// approach, and a shorter one gets a quicker hand.
pub fn approach<R: Rng + ?Sized>(
    think_ms: u32,
    from: Point,
    press: Point,
    look: Option<Point>,
    rest_ms: u32,
    unit: f64,
    rng: &mut R,
) -> Approach {
    let near = look.filter(|&at| distance(press, at) <= LOOK_REACH * unit);
    // The stops to make on the way, the fullest first: each gives way to the
    // next when the think cannot hold it.
    let shapes: Vec<Vec<Point>> = match near {
        Some(at) => {
            let roll = humanize::fraction(rng);
            if roll < ONLY_SETTLE {
                vec![vec![]]
            } else if roll < ONLY_SETTLE + LOOK_FIRST {
                vec![vec![at], vec![]]
            } else {
                vec![vec![press, at], vec![at], vec![]]
            }
        }
        None if humanize::chance(SETTLE_CHANCE, rng) => vec![vec![]],
        None => Vec::new(),
    };
    for stops in &shapes {
        if let Some(found) = with_stops(think_ms, from, press, stops, rest_ms, unit, false, rng) {
            return found;
        }
    }
    plain(think_ms, from, press, unit, rng)
}

/// The way to a press when the hand, resting at `from` on a tile it was
/// looking at between turns, presses another at `press`: a changed mind,
/// which moves as a torn hand does. With the old tile within reach of the
/// new, it always weighs the two — stays on the old a while before going
/// over, or goes over, back, and over again — each stop longer than the one
/// before, the new tile's the longest. Further apart it does not sweep
/// between them: it waits where it is, then settles on the new tile for
/// longer than it waited. `think_ms` should already hold the time a changed
/// mind takes (`humanize::CHANGED_MIND`).
pub fn changed_mind<R: Rng + ?Sized>(
    think_ms: u32,
    from: Point,
    press: Point,
    rest_ms: u32,
    unit: f64,
    rng: &mut R,
) -> Approach {
    let shapes: Vec<Vec<Point>> = if distance(press, from) <= LOOK_REACH * unit {
        // As a torn hand's looks, without the plain settle.
        if humanize::fraction(rng) < LOOK_FIRST / (1.0 - ONLY_SETTLE) {
            vec![vec![from], vec![]]
        } else {
            vec![vec![press, from], vec![from], vec![]]
        }
    } else {
        vec![vec![]]
    };
    for stops in &shapes {
        if let Some(found) = with_stops(think_ms, from, press, stops, rest_ms, unit, true, rng) {
            return found;
        }
    }
    plain(think_ms, from, press, unit, rng)
}

/// The plainest way: wait, then travel to the press and press on arrival,
/// a quicker hand when the think cannot hold even the travel.
fn plain<R: Rng + ?Sized>(
    think_ms: u32,
    from: Point,
    press: Point,
    unit: f64,
    rng: &mut R,
) -> Approach {
    let travel = humanize::TRAVEL
        .draw_ms(travel_ms(distance(from, press), unit), rng)
        .min(think_ms);
    Approach {
        pause_ms: think_ms - travel,
        legs: vec![Leg {
            to: press,
            travel_ms: travel,
            dwell_ms: 0,
            bend: humanize::BEND.draw(rng),
        }],
    }
}

/// An approach that rests on each of `stops` on its way to `press`, and
/// settles on `press` before pressing it; `None` when `think_ms` cannot hold
/// that. With stops, or with `grow`, the think is spread over the wait and
/// the stops each longer than the one before ([`weighed`]); otherwise the
/// settle is drawn against `rest_ms` and the wait has the rest.
#[allow(clippy::too_many_arguments)]
fn with_stops<R: Rng + ?Sized>(
    think_ms: u32,
    from: Point,
    press: Point,
    stops: &[Point],
    rest_ms: u32,
    unit: f64,
    grow: bool,
    rng: &mut R,
) -> Option<Approach> {
    let mut legs = Vec::with_capacity(stops.len() + 1);
    let mut at = from;
    for &stop in stops.iter().chain(std::iter::once(&press)) {
        legs.push(Leg {
            to: stop,
            travel_ms: humanize::TRAVEL.draw_ms(travel_ms(distance(at, stop), unit), rng),
            dwell_ms: 0,
            bend: humanize::BEND.draw(rng),
        });
        at = stop;
    }
    let travel: u32 = legs.iter().map(|l| l.travel_ms).sum();
    let spare = think_ms.checked_sub(travel)?;
    let rest = f64::from(rest_ms);
    if grow || !stops.is_empty() {
        return weighed(spare, legs, rest, rng);
    }
    if f64::from(spare) < rest * SETTLE_REST.low {
        return None;
    }
    // A rest on the tile pressed, no more than what is left once the travel
    // is paid for; the wait before the hand moves has the rest.
    let settle = &mut legs[0];
    settle.dwell_ms = ((rest * SETTLE_REST.draw(rng)) as u32).min(spare);
    Some(Approach {
        pause_ms: spare - settle.dwell_ms,
        legs,
    })
}

/// A hand weighing two tiles spends its think, once the travel is paid for,
/// on the wait before it moves and the stops it makes, each longer than the
/// one before by [`LONGER`]: it looks longer every time it comes back, and
/// longest at the tile it then presses. A longer think is longer stops, not
/// a longer wait. `None` when the first stop would be too short to read a
/// tile in (`LOOK_REST.low` of `rest`).
fn weighed<R: Rng + ?Sized>(
    spare: u32,
    mut legs: Vec<Leg>,
    rest: f64,
    rng: &mut R,
) -> Option<Approach> {
    let mut shares = Vec::with_capacity(legs.len() + 1);
    let mut share = 1.0;
    for _ in 0..=legs.len() {
        shares.push(share);
        share *= LONGER.draw(rng);
    }
    let unit = f64::from(spare) / shares.iter().sum::<f64>();
    if unit * shares[1] < rest * LOOK_REST.low {
        return None;
    }
    for (leg, share) in legs.iter_mut().zip(&shares[1..]) {
        leg.dwell_ms = (unit * share) as u32;
    }
    let rested: u32 = legs.iter().map(|l| l.dwell_ms).sum();
    Some(Approach {
        pause_ms: spare - rested,
        legs,
    })
}

/// The tile to glance at before discarding `chosen`: the other of the bot's
/// two likeliest candidates, when both are discards and of different tiles.
///
/// Anything else is not a hesitation the pointer can show. A call or a
/// declaration is a button, not a place in the hand; and the same tile from
/// the hand or from the draw is one tile, however the bot counts it.
pub fn runner_up<'a>(candidates: &'a [Candidate], chosen: &str) -> Option<&'a str> {
    let [first, second, ..] = candidates else {
        return None;
    };
    let tile = |c: &'a Candidate| match (c.kind.as_deref(), c.pais.as_slice()) {
        (Some("dahai"), [pai]) => Some(pai.as_str()),
        _ => None,
    };
    let (a, b) = (tile(first)?, tile(second)?);
    match (a == chosen, b == chosen) {
        (true, false) => Some(b),
        (false, true) => Some(a),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const UNIT: f64 = 100.0;
    const PRESS: Point = (1170.0, 836.0);
    /// Two tiles along from the press.
    const NEAR: Point = (1012.0, 836.0);
    /// The other end of the hand.
    const FAR: Point = (300.0, 836.0);

    fn discard(pai: &str, prob: f64) -> Candidate {
        Candidate {
            kind: Some("dahai".into()),
            pais: vec![pai.into()],
            prob,
        }
    }

    fn median(mut values: Vec<u32>) -> u32 {
        values.sort_unstable();
        values[values.len() / 2]
    }

    /// The press lands when it would have anyway: whatever is planned, the
    /// approach takes exactly the think it was given.
    #[test]
    fn an_approach_takes_exactly_the_think_it_is_given() {
        let mut rng = StdRng::seed_from_u64(7);
        for think in [0, 40, 250, 700, 1_000, 1_800, 3_500, 9_000, 60_000] {
            for look in [None, Some(NEAR), Some(FAR)] {
                for _ in 0..200 {
                    let from = (
                        rng.random_range(200.0..1300.0),
                        rng.random_range(660.0..750.0),
                    );
                    let plan = approach(think, from, PRESS, look, LOOK_MS, UNIT, &mut rng);
                    assert_eq!(plan.total_ms(), think, "{plan:?}");
                    assert_eq!(plan.legs.last().unwrap().to, PRESS);
                }
            }
        }
    }

    #[test]
    fn a_torn_hand_settles_on_its_choice_or_looks_at_the_runner_up_first() {
        let mut rng = StdRng::seed_from_u64(11);
        let mut shapes = std::collections::BTreeSet::new();
        for _ in 0..400 {
            let plan = approach(
                20_000,
                (700.0, 700.0),
                PRESS,
                Some(NEAR),
                LOOK_MS,
                UNIT,
                &mut rng,
            );
            let stops: Vec<Point> = plan.legs.iter().map(|l| l.to).collect();
            assert!(
                stops == vec![PRESS]
                    || stops == vec![NEAR, PRESS]
                    || stops == vec![PRESS, NEAR, PRESS],
                "{stops:?}"
            );
            assert_eq!(plan.looks_elsewhere(), stops.len() > 1);
            if !plan.looks_elsewhere() {
                assert!((160..=1_600).contains(&plan.legs[0].dwell_ms), "{plan:?}");
            }
            shapes.insert(stops.len());
        }
        assert_eq!(shapes.len(), 3, "all three shapes should occur");
    }

    #[test]
    fn a_press_is_mostly_settled_on_and_now_and_then_made_on_arrival() {
        let mut rng = StdRng::seed_from_u64(19);
        let plans: Vec<Approach> = (0..1_000)
            .map(|_| approach(9_000, (700.0, 700.0), PRESS, None, LOOK_MS, UNIT, &mut rng))
            .collect();
        assert!(plans.iter().all(|plan| plan.legs.len() == 1));
        let settles: Vec<u32> = plans
            .iter()
            .map(|plan| plan.legs[0].dwell_ms)
            .filter(|&dwell| dwell > 0)
            .collect();
        assert!(
            (600..=800).contains(&settles.len()),
            "{} settled",
            settles.len()
        );
        // Rests are drawn afresh, long-tailed: mostly near the median, some
        // twice as long or more.
        let middle = median(settles.clone());
        assert!((360..=520).contains(&middle), "median {middle}");
        assert!(settles.iter().filter(|&&dwell| dwell >= 2 * middle).count() > 20);
        let distinct: std::collections::BTreeSet<u32> = settles.iter().copied().collect();
        assert!(distinct.len() > settles.len() / 2);
    }

    /// A hand weighing two tiles looks longer each time: the wait before it
    /// moves is the shortest, every stop longer than the one before, the one
    /// on the tile it presses the longest.
    #[test]
    fn a_torn_hand_looks_longer_each_time() {
        let mut rng = StdRng::seed_from_u64(23);
        let mut torn = 0;
        for think in [3_000, 5_000, 9_000, 20_000] {
            for _ in 0..300 {
                let plan = approach(
                    think,
                    (700.0, 700.0),
                    PRESS,
                    Some(NEAR),
                    LOOK_MS,
                    UNIT,
                    &mut rng,
                );
                if !plan.looks_elsewhere() {
                    continue;
                }
                torn += 1;
                let mut last = plan.pause_ms;
                for leg in &plan.legs {
                    assert!(leg.dwell_ms > last, "{plan:?}");
                    last = leg.dwell_ms;
                }
                assert!(plan.legs[0].dwell_ms >= (LOOK_MS as f64 * LOOK_REST.low) as u32);
            }
        }
        assert!(torn > 500, "{torn}");
    }

    /// A changed mind next to the old tile always weighs the two, each stop
    /// longer than the one before; far from it, it waits less than it then
    /// settles on the new tile.
    #[test]
    fn a_changed_mind_weighs_the_old_tile_against_the_new() {
        let mut rng = StdRng::seed_from_u64(29);
        let mut shapes = std::collections::BTreeSet::new();
        for _ in 0..300 {
            let plan = changed_mind(6_000, NEAR, PRESS, LOOK_MS, UNIT, &mut rng);
            assert_eq!(plan.total_ms(), 6_000);
            assert!(plan.looks_elsewhere(), "{plan:?}");
            let stops: Vec<Point> = plan.legs.iter().map(|l| l.to).collect();
            assert!(
                stops == vec![NEAR, PRESS] || stops == vec![PRESS, NEAR, PRESS],
                "{stops:?}"
            );
            let mut last = plan.pause_ms;
            for leg in &plan.legs {
                assert!(leg.dwell_ms > last, "{plan:?}");
                last = leg.dwell_ms;
            }
            shapes.insert(stops.len());
        }
        assert_eq!(shapes.len(), 2, "both ways of weighing occur");
        for _ in 0..100 {
            let plan = changed_mind(3_500, FAR, PRESS, LOOK_MS, UNIT, &mut rng);
            assert_eq!(plan.total_ms(), 3_500);
            assert_eq!(plan.legs.len(), 1, "no sweep across the rack");
            assert!(plan.legs[0].dwell_ms > plan.pause_ms, "{plan:?}");
        }
    }

    /// A longer think is longer stops, not a longer wait before moving.
    #[test]
    fn a_longer_think_is_longer_stops() {
        let mut rng = StdRng::seed_from_u64(13);
        let mut first_look = |think: u32| -> u32 {
            let dwells = (0..400)
                .filter_map(|_| {
                    let plan = approach(
                        think,
                        (700.0, 700.0),
                        PRESS,
                        Some(NEAR),
                        LOOK_MS,
                        UNIT,
                        &mut rng,
                    );
                    plan.looks_elsewhere().then(|| plan.legs[0].dwell_ms)
                })
                .collect();
            median(dwells)
        };
        let short = first_look(4_000);
        let long = first_look(8_000);
        assert!(long > short * 3 / 2, "{short} vs {long}");
    }

    #[test]
    fn tiles_far_apart_are_not_looked_between() {
        let mut rng = StdRng::seed_from_u64(17);
        for _ in 0..100 {
            let plan = approach(
                9_000,
                (700.0, 700.0),
                PRESS,
                Some(FAR),
                LOOK_MS,
                UNIT,
                &mut rng,
            );
            assert_eq!(plan.legs.len(), 1, "{plan:?}");
            assert_eq!(plan.legs[0].to, PRESS);
        }
    }

    #[test]
    fn a_think_too_short_to_rest_gets_a_plain_approach() {
        let mut rng = StdRng::seed_from_u64(3);
        for _ in 0..100 {
            // The travel takes most of it: no look fits, and at most a short settle.
            let plan = approach(
                480,
                (700.0, 700.0),
                PRESS,
                Some(NEAR),
                LOOK_MS,
                UNIT,
                &mut rng,
            );
            assert_eq!(plan.legs.len(), 1);
            assert_eq!(plan.total_ms(), 480);
        }
        // And one too short even to travel gets a quicker hand, not a later press.
        let plan = approach(60, (200.0, 700.0), PRESS, None, LOOK_MS, UNIT, &mut rng);
        assert_eq!(
            (plan.pause_ms, plan.legs[0].travel_ms, plan.legs[0].dwell_ms),
            (0, 60, 0)
        );
    }

    #[test]
    fn the_runner_up_is_the_other_of_two_different_discards() {
        let two = [discard("5p", 0.48), discard("1m", 0.41), discard("E", 0.05)];
        assert_eq!(runner_up(&two, "5p"), Some("1m"));
        assert_eq!(runner_up(&two, "1m"), Some("5p"));
        // The choice is neither of the two likeliest: nothing to show.
        assert_eq!(runner_up(&two, "E"), None);
        // A red five and a plain one sit in different places.
        assert_eq!(
            runner_up(&[discard("5pr", 0.5), discard("5p", 0.4)], "5p"),
            Some("5pr")
        );
    }

    #[test]
    fn nothing_to_glance_at_between_one_tile_or_a_button() {
        // The same tile, from the hand and from the draw.
        assert_eq!(
            runner_up(&[discard("5p", 0.5), discard("5p", 0.4)], "5p"),
            None
        );
        // A declaration is a button.
        let reach = Candidate {
            kind: Some("reach".into()),
            pais: vec![],
            prob: 0.4,
        };
        assert_eq!(runner_up(&[discard("5p", 0.5), reach], "5p"), None);
        // A bot that does not say what its candidates are.
        let unnamed = Candidate {
            kind: None,
            pais: vec!["1m".into()],
            prob: 0.4,
        };
        assert_eq!(runner_up(&[discard("5p", 0.5), unnamed], "5p"), None);
        assert_eq!(runner_up(&[discard("5p", 1.0)], "5p"), None);
    }
}
