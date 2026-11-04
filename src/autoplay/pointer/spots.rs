//! Where the pointer goes, other than to a plan's presses.
//!
//! The press points of a plan are [`super::aim`]'s, and the way to them
//! [`super::approach`]'s. Every other point a move ends on is worked out
//! here and nowhere else: the rest by the buttons, the lift off the hand, a
//! look at a tile, a press made again on the same spot, and where the next
//! draw lands (for [`super::rules`]). Each is drawn through
//! [`crate::autoplay::humanize`], which holds every number they draw by.

use super::aim::Reach;
use super::layout::PointerLayout;
use super::Point;
use crate::autoplay::context::CanvasRect;
use crate::autoplay::humanize::{self, Spread};
use rand::Rng;

/// Where the hand waits between presses, in CSS pixels: drawn afresh by the
/// site's buttons (see [`humanize::rest_offset`]) and kept in the part of the
/// table it may wait in. The same on every site; a site only measures where
/// its buttons stand and what is free.
pub fn rest<R: Rng + ?Sized>(
    rect: CanvasRect,
    layout: Option<&PointerLayout>,
    rng: &mut R,
) -> Point {
    let (buttons, area) =
        layout.map_or_else(|| unmeasured(rect), |layout| (layout.buttons, layout.rest));
    let unit = rect.width / 16.0;
    let (x, y) = humanize::rest_offset(rng);
    (
        (buttons.0 + x * unit).clamp(area.x, area.x + area.width),
        (buttons.1 + y * unit).clamp(area.y, area.y + area.height),
    )
}

/// The buttons and the free part of a table with no measured geometry.
fn unmeasured(rect: CanvasRect) -> (Point, CanvasRect) {
    let (buttons, (from, to)) = (humanize::REST_BUTTONS, humanize::REST_AREA);
    let (x, y) = rect.pixel(from.0, from.1);
    let (right, bottom) = rect.pixel(to.0, to.1);
    (
        rect.pixel(buttons.0, buttons.1),
        CanvasRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        },
    )
}

/// Straight up off a hand tile pressed at `from`: above the top of the
/// tiles by [`humanize::LIFT_UP`], drifting [`humanize::LIFT_SIDE`] to
/// either side, in CSS pixels. Clear of the tiles, and so of where the next
/// draw lands.
pub fn lift<R: Rng + ?Sized>(from: Point, top: f64, rect: CanvasRect, rng: &mut R) -> Point {
    let unit = rect.width / 16.0;
    let up = humanize::LIFT_UP.draw(rng);
    let side = humanize::LIFT_SIDE.draw(rng);
    (from.0 + side * unit, top - up * unit)
}

/// On the tile whose middle is `middle` (normalised), as a hand rests on a
/// tile it looks at: off the middle across, high on it rather than low.
pub fn on_tile<R: Rng + ?Sized>(middle: (f64, f64), rect: CanvasRect, rng: &mut R) -> Point {
    rect.pixel(
        middle.0 + humanize::LOOK_OFF_X.draw(rng),
        middle.1 + humanize::on_tile_y(rng),
    )
}

/// A hover point kept inside an actual measured tile of any size.
pub fn inside_tile<R: Rng + ?Sized>(bounds: CanvasRect, rng: &mut R) -> Point {
    let x = (0.5 + humanize::LOOK_OFF_X.draw(rng) * 0.5).clamp(0.15, 0.85);
    let y = (0.25 + humanize::normal(rng) * 0.05).clamp(0.15, 0.45);
    (bounds.x + bounds.width * x, bounds.y + bounds.height * y)
}

/// A press on the same spot again, moved a little as a hand pressing in
/// place moves the mouse: by `x` across and `y` up and down (16:9 units),
/// never off the target whose middle is `middle` and reach `reach`.
pub fn nudge<R: Rng + ?Sized>(
    at: (f64, f64),
    middle: (f64, f64),
    reach: Reach,
    x: Spread,
    y: Spread,
    rng: &mut R,
) -> (f64, f64) {
    (
        (at.0 + x.draw(rng)).clamp(middle.0 - reach.half_x, middle.0 + reach.half_x),
        (at.1 + y.draw(rng)).clamp(middle.1 - reach.half_y, middle.1 + reach.half_y),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::pointer::rules::{refusal, Bound, Sight};
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    const RECT: CanvasRect = CanvasRect {
        x: 0.0,
        y: 0.0,
        width: 1280.0,
        height: 720.0,
    };

    fn layout() -> PointerLayout {
        PointerLayout {
            viewport: RECT,
            tiles: vec![],
            next_draw: None,
            rest: CanvasRect {
                x: 0.0,
                y: 400.0,
                width: 1280.0,
                height: 200.0,
            },
            buttons: (890.0, 540.0),
        }
    }

    /// A rest is drawn afresh each time: mostly near the buttons, and never
    /// out of the part of the table the site measured as free.
    #[test]
    fn a_rest_scatters_by_the_buttons_inside_the_free_part() {
        let mut rng = StdRng::seed_from_u64(61);
        let layout = layout();
        let home = (890.0 - 1.6 * 80.0, 540.0 - 0.7 * 80.0);
        let rests: Vec<Point> = (0..4000)
            .map(|_| rest(RECT, Some(&layout), &mut rng))
            .collect();
        assert!(rests.iter().all(|p| layout.rest.contains(p.0, p.1)));
        let mut far: Vec<f64> = rests
            .iter()
            .map(|p| (p.0 - home.0).hypot(p.1 - home.1))
            .collect();
        far.sort_by(f64::total_cmp);
        assert!(far[2000] < 1.2 * 80.0, "mostly near: {}", far[2000]);
        let mut pixels: Vec<(i64, i64)> = rests.iter().map(|p| (p.0 as i64, p.1 as i64)).collect();
        pixels.sort_unstable();
        pixels.dedup();
        assert!(pixels.len() > 1000, "no favourite pixel: {}", pixels.len());
    }

    /// A table with no measured geometry waits above the hand, off the edge.
    #[test]
    fn a_rest_without_geometry_stays_off_the_hand_and_the_edge() {
        let mut rng = StdRng::seed_from_u64(67);
        let (left, top) = RECT.pixel(1.0, 3.0);
        let (right, bottom) = RECT.pixel(15.0, 7.4);
        for _ in 0..2000 {
            let (x, y) = rest(RECT, None, &mut rng);
            assert!((left..=right).contains(&x) && (top..=bottom).contains(&y));
        }
    }

    /// A lift off the tile just thrown — where the next draw lands — always
    /// ends clear of that spot, above the tiles, and nearly straight up.
    #[test]
    fn a_lift_clears_where_the_next_draw_lands() {
        let mut rng = StdRng::seed_from_u64(43);
        let slot = (1050.0, 660.0);
        let unit = RECT.width / 16.0;
        let top = 610.0;
        let sight = Sight {
            draw_slot: Some(CanvasRect {
                x: 1005.0,
                y: top,
                width: 90.0,
                height: 100.0,
            }),
        };
        for _ in 0..2000 {
            let pressed = (slot.0, slot.1 + humanize::on_tile_y(&mut rng) * unit);
            let to = lift(pressed, top, RECT, &mut rng);
            assert_eq!(refusal(to, Bound::Errand, &sight), None, "{to:?}");
            assert!(to.1 < top, "above the tiles: {to:?}");
            assert!((to.0 - pressed.0).abs() <= humanize::LIFT_SIDE.most * unit + 1e-9);
        }
    }

    /// A look rests on its tile: across within it, high on it.
    #[test]
    fn a_look_rests_on_its_tile() {
        let mut rng = StdRng::seed_from_u64(47);
        let middle = (4.0, 8.0);
        let unit = RECT.width / 16.0;
        let (mx, my) = RECT.pixel(middle.0, middle.1);
        for _ in 0..2000 {
            let (x, y) = on_tile(middle, RECT, &mut rng);
            assert!((x - mx).abs() <= humanize::LOOK_OFF_X.most * unit + 1e-9);
            assert!(y >= my + humanize::TILE_TOP * unit - 1e-9);
            assert!(y <= my + (humanize::TILE_TOP + humanize::TILE_DROP.most) * unit + 1e-9);
        }
    }

    /// A press made again on the same spot stays on its target.
    #[test]
    fn a_nudge_stays_on_its_target() {
        let mut rng = StdRng::seed_from_u64(53);
        let middle = (14.59, 8.37);
        let reach = Reach {
            half_x: 0.45,
            half_y: 0.16,
            from_top: false,
        };
        let wide = Spread { sd: 1.0, most: 3.0 };
        for _ in 0..500 {
            let (x, y) = nudge(middle, middle, reach, wide, wide, &mut rng);
            assert!((x - middle.0).abs() <= reach.half_x + 1e-9);
            assert!((y - middle.1).abs() <= reach.half_y + 1e-9);
        }
    }
}
