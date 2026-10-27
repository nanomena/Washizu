//! Where in its target a press lands.
//!
//! A plan names a target by its middle: a tile's, a button's. Pressed there
//! every time, the same few pixels take every press of a session, which no
//! hand does. A press lands instead somewhere in the target's safe middle (its
//! [`Reach`]), scattered the way a hand's endpoints scatter:
//!
//! - normally, about the middle, and never outside the reach, so a press never
//!   strays onto the neighbour;
//! - longer along the way the pointer came than across it, as an aimed
//!   movement ends;
//! - with a small lean of its own, drawn once a session: a person tends to the
//!   same side of a target;
//! - on a hand tile, high rather than low: on average at the top quarter of
//!   its face, from just inside its top and down from there
//!   (`humanize::on_tile_y`), as a hand reaching down to a tile stops on it.
//!
//! A press that is made again, because the first did not register, lands at a
//! fresh point of the same target ([`again`]).

use super::Point;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::humanize::{
    self, AIM_ACROSS as ACROSS, AIM_ALONG as ALONG, AIM_ANYWAY as ANYWAY,
};
use rand::Rng;

/// The part of a target a press may land on: half its width and half its
/// height, in units of the canvas, about its middle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    pub half_x: f64,
    pub half_y: f64,
    /// A hand tile: pressed from the top of its face down rather than about
    /// its middle (`humanize::on_tile_y`). Across, as any target.
    pub from_top: bool,
}

/// Aim within one measured target; the common executor supplies each target
/// only after its current website identity and readiness have been observed.
pub fn target<R: Rng + ?Sized>(bounds: CanvasRect, from: Option<Point>, rng: &mut R) -> Point {
    let middle = super::layout::center(bounds);
    let way = from.map(|p| (middle.0 - p.0, middle.1 - p.1));
    let reach = Reach {
        half_x: bounds.width * 0.4,
        half_y: bounds.height * 0.4,
        from_top: false,
    };
    land(middle, way, reach, humanize::session_lean(), rng)
}

/// A fresh point in the target whose middle is `middle`, for a press made again.
pub fn again<R: Rng + ?Sized>(middle: Point, reach: Reach, rng: &mut R) -> Point {
    land(middle, None, reach, humanize::session_lean(), rng)
}

/// A point within `reach` of `middle`, scattered along `way` when it is known.
fn land<R: Rng + ?Sized>(
    middle: Point,
    way: Option<Point>,
    reach: Reach,
    lean: (f64, f64),
    rng: &mut R,
) -> Point {
    let (hx, hy) = (reach.half_x.max(1e-6), reach.half_y.max(1e-6));
    if reach.from_top {
        // A tile is come at from above, the buttons' side: across is sideways.
        let x = (lean.0 * hx + humanize::normal(rng) * ACROSS * hx).clamp(-0.95 * hx, 0.95 * hx);
        return (middle.0 + x, middle.1 + humanize::on_tile_y(rng));
    }
    let base = (lean.0 * hx, lean.1 * hy);
    // The way as a direction, and how far the reach goes along it and across it.
    let along = way
        .map(|(dx, dy)| (dx, dy, (dx * dx + dy * dy).sqrt()))
        .filter(|&(_, _, n)| n > 1e-9)
        .map(|(dx, dy, n)| (dx / n, dy / n));
    let within = |o: (f64, f64)| (o.0 / hx).powi(2) + (o.1 / hy).powi(2) <= 1.0;
    let mut offset = (0.0, 0.0);
    for _ in 0..8 {
        let (u, v) = (humanize::normal(rng), humanize::normal(rng));
        offset = match along {
            Some((dx, dy)) => {
                // The ellipse's radius along a direction and across it.
                let radius = |x: f64, y: f64| 1.0 / ((x / hx).powi(2) + (y / hy).powi(2)).sqrt();
                let (r_along, r_across) = (radius(dx, dy), radius(-dy, dx));
                let (a, c) = (u * ALONG * r_along, v * ACROSS * r_across);
                (base.0 + a * dx - c * dy, base.1 + a * dy + c * dx)
            }
            None => (base.0 + u * ANYWAY * hx, base.1 + v * ANYWAY * hy),
        };
        if within(offset) {
            break;
        }
    }
    if !within(offset) {
        // Rarely far out: brought back just inside.
        let k = 0.95 / ((offset.0 / hx).powi(2) + (offset.1 / hy).powi(2)).sqrt();
        offset = (offset.0 * k, offset.1 * k);
    }
    (middle.0 + offset.0, middle.1 + offset.1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const TILE: Reach = Reach {
        half_x: 0.24,
        half_y: 0.36,
        from_top: false,
    };

    fn inside(p: Point, middle: Point, reach: Reach) -> bool {
        ((p.0 - middle.0) / reach.half_x).powi(2) + ((p.1 - middle.1) / reach.half_y).powi(2)
            <= 1.0 + 1e-9
    }

    /// Every press stays within its target's reach, wherever it comes from.
    #[test]
    fn a_press_never_leaves_its_target() {
        let mut rng = StdRng::seed_from_u64(1);
        let middle = (6.0, 8.3);
        for i in 0..2000 {
            let way = [None, Some((1.0, 0.0)), Some((0.0, -1.0)), Some((0.7, 0.7))][i % 4];
            let p = land(middle, way, TILE, (0.12, 0.16), &mut rng);
            assert!(inside(p, middle, TILE), "{p:?}");
        }
    }

    /// Presses spread about the middle, not on one point, and leaning the
    /// session's way on average.
    #[test]
    fn presses_spread_about_the_middle_with_the_sessions_lean() {
        let mut rng = StdRng::seed_from_u64(2);
        let middle = (6.0, 8.3);
        let points: Vec<Point> = (0..3000)
            .map(|_| land(middle, None, TILE, (0.1, 0.1), &mut rng))
            .collect();
        let mean = points
            .iter()
            .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
        let mean = (mean.0 / points.len() as f64, mean.1 / points.len() as f64);
        assert!(
            (mean.0 - (6.0 + 0.024)).abs() < 0.01 && (mean.1 - (8.3 + 0.036)).abs() < 0.012,
            "{mean:?}"
        );
        let mut xs: Vec<i64> = points.iter().map(|p| (p.0 * 1000.0) as i64).collect();
        xs.sort_unstable();
        xs.dedup();
        assert!(xs.len() > 300, "not a few points but many");
    }

    /// A press coming from the side scatters more along its way than across.
    #[test]
    fn presses_scatter_more_along_the_way_they_came() {
        let mut rng = StdRng::seed_from_u64(3);
        let middle = (6.0, 6.0);
        let round = Reach {
            half_x: 0.3,
            half_y: 0.3,
            from_top: false,
        };
        let points: Vec<Point> = (0..4000)
            .map(|_| land(middle, Some((1.0, 0.0)), round, (0.0, 0.0), &mut rng))
            .collect();
        let var = |f: fn(&Point) -> f64| {
            let m = points.iter().map(f).sum::<f64>() / points.len() as f64;
            points.iter().map(|p| (f(p) - m).powi(2)).sum::<f64>() / points.len() as f64
        };
        let (along, across) = (var(|p| p.0), var(|p| p.1));
        assert!(along > across * 1.5, "along {along} across {across}");
    }

    /// A hand tile is pressed on average at the top quarter of its face, down
    /// from just inside its top: never above that, mostly in the upper half,
    /// never in the bottom fifth, and across within the tile.
    #[test]
    fn a_tile_is_pressed_high_rather_than_low() {
        let mut rng = StdRng::seed_from_u64(5);
        let middle = (6.0, 8.3625);
        let tile = Reach {
            from_top: true,
            ..TILE
        };
        let points: Vec<Point> = (0..4000)
            .map(|_| land(middle, Some((0.0, 1.0)), tile, (0.1, 0.1), &mut rng))
            .collect();
        let top = middle.1 + humanize::TILE_TOP;
        let bottom = middle.1 + humanize::TILE_TOP + humanize::TILE_DROP.most;
        assert!(points
            .iter()
            .all(|p| p.1 >= top - 1e-9 && p.1 <= bottom + 1e-9));
        assert!(points.iter().all(|p| (p.0 - middle.0).abs() <= tile.half_x));
        let high = points.iter().filter(|p| p.1 < middle.1).count();
        assert!(high > 3200, "mostly the upper half: {high} of 4000");
        assert!(bottom < middle.1 + 0.35, "never in the bottom fifth");
        // On average at the top quarter of a face some 1.13 high.
        let mean = points.iter().map(|p| p.1).sum::<f64>() / points.len() as f64;
        assert!((mean - (middle.1 - 1.13 / 4.0)).abs() < 0.03, "{mean}");
    }

    #[test]
    fn measured_targets_keep_every_press_inside_on_both_geometry_scales() {
        let mut rng = StdRng::seed_from_u64(4);
        for bounds in [
            CanvasRect {
                x: 100.0,
                y: 200.0,
                width: 18.0,
                height: 27.0,
            },
            CanvasRect {
                x: 600.0,
                y: 500.0,
                width: 90.0,
                height: 120.0,
            },
        ] {
            for from in [None, Some((0.0, 0.0)), Some((1000.0, 700.0))] {
                for _ in 0..300 {
                    let point = target(bounds, from, &mut rng);
                    assert!(bounds.contains(point.0, point.1));
                }
            }
        }
    }
}
