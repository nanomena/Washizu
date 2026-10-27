//! Measured site geometry consumed by the common pointer controller.
//! All rectangles use viewport CSS pixels; this module knows no website.

use super::lean::Spot;
use super::Point;
use crate::autoplay::context::CanvasRect;

#[derive(Debug, Clone, PartialEq)]
pub struct TileTarget {
    pub tile: String,
    pub drawn: bool,
    pub bounds: CanvasRect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PointerLayout {
    pub viewport: CanvasRect,
    pub tiles: Vec<TileTarget>,
    /// A measured empty draw slot, never an invented future tile.
    pub next_draw: Option<CanvasRect>,
    /// A site-observed region clear of actionable tiles and controls.
    pub rest: CanvasRect,
}

pub fn center(rect: CanvasRect) -> Point {
    (rect.x + rect.width * 0.5, rect.y + rect.height * 0.5)
}

fn valid(rect: CanvasRect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|v| v.is_finite())
        && rect.width > 0.0
        && rect.height > 0.0
}

impl PointerLayout {
    pub fn valid(&self) -> bool {
        let inside = |bounds: CanvasRect| {
            valid(bounds)
                && self.viewport.contains(bounds.x, bounds.y)
                && self
                    .viewport
                    .contains(bounds.x + bounds.width, bounds.y + bounds.height)
        };
        valid(self.viewport)
            && inside(self.rest)
            && self.next_draw.is_none_or(inside)
            && self
                .tiles
                .iter()
                .all(|tile| !tile.tile.is_empty() && inside(tile.bounds))
    }

    pub fn target(&self, spot: &Spot, pointer: Option<Point>) -> Option<CanvasRect> {
        self.tiles
            .iter()
            .filter(|target| match spot {
                Spot::Hand(tile) => !target.drawn && target.tile == *tile,
                Spot::Draw => target.drawn,
            })
            .min_by(|left, right| {
                let distance = |rect: CanvasRect| {
                    pointer.map_or(0.0, |p| {
                        if rect.contains(p.0, p.1) {
                            0.0
                        } else {
                            super::distance(p, center(rect))
                        }
                    })
                };
                distance(left.bounds).total_cmp(&distance(right.bounds))
            })
            .map(|target| target.bounds)
    }

    pub fn hand_top(&self) -> Option<f64> {
        self.tiles
            .iter()
            .map(|tile| tile.bounds.y)
            .min_by(f64::total_cmp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64, width: f64, height: f64) -> CanvasRect {
        CanvasRect {
            x,
            y,
            width,
            height,
        }
    }
    #[test]
    fn semantic_tiles_keep_red_identity_drawn_copy_and_measured_geometry() {
        let layout = PointerLayout {
            viewport: rect(0.0, 0.0, 900.0, 600.0),
            rest: rect(200.0, 400.0, 100.0, 30.0),
            next_draw: None,
            tiles: vec![
                TileTarget {
                    tile: "5m".into(),
                    drawn: false,
                    bounds: rect(100.0, 500.0, 40.0, 65.0),
                },
                TileTarget {
                    tile: "5mr".into(),
                    drawn: false,
                    bounds: rect(150.0, 500.0, 40.0, 65.0),
                },
                TileTarget {
                    tile: "5m".into(),
                    drawn: true,
                    bounds: rect(250.0, 500.0, 40.0, 65.0),
                },
            ],
        };
        assert!(layout.valid());
        assert_eq!(
            layout
                .target(&Spot::Hand("5m".into()), Some((265.0, 520.0)))
                .unwrap()
                .x,
            100.0
        );
        assert_eq!(
            layout.target(&Spot::Hand("5mr".into()), None).unwrap().x,
            150.0
        );
        assert_eq!(layout.target(&Spot::Draw, None).unwrap().x, 250.0);
        assert!(layout.target(&Spot::Hand("9p".into()), None).is_none());
        let mut changed = layout.clone();
        changed.tiles[0].bounds.x = f64::NAN;
        assert!(!changed.valid());
    }
}
