//! Read-only geometry of the client's rendered meld choices.
//!
//! Protocol/legal actions identify the chosen combination. Pixels establish
//! whether its candidate row is actually drawn and where its tile faces are.
//! No phase, wait, retry or mouse effect is owned here.
use crate::autoplay::{pixels::Image, CanvasRect};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CandidateRow {
    pub faces: Vec<CanvasRect>,
    pub per_choice: usize,
}

impl CandidateRow {
    /// A single tile's safe interior, rather than the gap between two tiles
    /// or the old row's guessed center. The shared pointer scatters inside it.
    pub fn target(&self, index: usize) -> Option<CanvasRect> {
        self.faces.get(index.checked_mul(self.per_choice)?).copied()
    }
}

/// Connected tile faces in the lower-table candidate band. A row must have
/// the exact expected number of groups and a consistent visible layout.
/// Hidden/partial/covered choices supply no target.
pub(super) fn read(
    image: &Image,
    scale: f64,
    viewport: CanvasRect,
    choices: usize,
) -> Option<CandidateRow> {
    if !(2..=6).contains(&choices)
        || !scale.is_finite()
        || scale <= 0.
        || ![viewport.x, viewport.y, viewport.width, viewport.height]
            .iter()
            .all(|v| v.is_finite())
        || viewport.width <= 0.
        || viewport.height <= 0.
        || viewport.x < 0.
        || viewport.y < 0.
        || (viewport.x + viewport.width) * scale > image.width as f64 + 1.
        || (viewport.y + viewport.height) * scale > image.height as f64 + 1.
    {
        return None;
    }
    let unit = viewport.height * scale / 9.;
    let (left, top) = viewport.pixel(1.5, 5.35);
    let (right, bottom) = viewport.pixel(14.5, 7.15);
    let x0 = (left * scale).floor() as usize;
    let y0 = (top * scale).floor() as usize;
    let x1 = ((right * scale).ceil() as usize).min(image.width);
    let y1 = ((bottom * scale).ceil() as usize).min(image.height);
    let (width, height) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    if width == 0 || height == 0 {
        return None;
    }
    let mut seen = vec![false; width * height];
    let bright = |index: usize| {
        let [r, g, b] = image.pixels[(y0 + index / width) * image.width + x0 + index % width];
        r >= 180 && g >= 175 && b >= 145 && r.max(g).max(b) - r.min(g).min(b) <= 65
    };
    let mut faces = Vec::new();
    let mut todo = Vec::new();
    for start in 0..seen.len() {
        if seen[start] || !bright(start) {
            continue;
        }
        seen[start] = true;
        todo.clear();
        todo.push(start);
        let (mut min_x, mut max_x, mut min_y, mut max_y) =
            (start % width, start % width, start / width, start / width);
        let mut count = 0;
        while let Some(index) = todo.pop() {
            count += 1;
            let (x, y) = (index % width, index / width);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            for next in [
                x.checked_sub(1).map(|_| index - 1),
                (x + 1 < width).then_some(index + 1),
                y.checked_sub(1).map(|_| index - width),
                (y + 1 < height).then_some(index + width),
            ]
            .into_iter()
            .flatten()
            {
                if !seen[next] && bright(next) {
                    seen[next] = true;
                    todo.push(next);
                }
            }
        }
        let (w, h) = ((max_x - min_x + 1) as f64, (max_y - min_y + 1) as f64);
        if (0.38..=1.05).contains(&(w / unit))
            && (0.65..=1.45).contains(&(h / unit))
            && (0.30..=1.0).contains(&(w / h))
            && count as f64 / (w * h) >= 0.48
        {
            faces.push(CanvasRect {
                x: (x0 + min_x) as f64 / scale,
                y: (y0 + min_y) as f64 / scale,
                width: w / scale,
                height: h / scale,
            });
        }
    }
    if faces.len() % choices != 0 || !(2..=3).contains(&(faces.len() / choices)) {
        return None;
    }
    let per_choice = faces.len() / choices;
    faces.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut widths: Vec<_> = faces.iter().map(|f| f.width).collect();
    let mut heights: Vec<_> = faces.iter().map(|f| f.height).collect();
    widths.sort_by(f64::total_cmp);
    heights.sort_by(f64::total_cmp);
    let (w, h) = (widths[widths.len() / 2], heights[heights.len() / 2]);
    // Hover may lift the selected group. Other groups still establish the
    // row's resting baseline; the safe interior also fits the lifted tile.
    let row_y = faces.iter().map(|f| f.y).max_by(f64::total_cmp)?;
    if faces.iter().any(|f| {
        (f.width - w).abs() > w * 0.18
            || (f.height - h).abs() > h * 0.18
            || (f.y - row_y).abs() > h * 0.24
    }) {
        return None;
    }
    for (index, pair) in faces.windows(2).enumerate() {
        let gap = pair[1].x - (pair[0].x + pair[0].width);
        if gap < -w * 0.08 || gap > w * 1.3 {
            return None;
        }
        if (index + 1) % per_choice != 0 && gap > w * 0.24 {
            return None;
        }
    }
    Some(CandidateRow { faces, per_choice })
}

/// The active meld button's coloured face/glyph within its protocol-mapped
/// neighbourhood. Its visible geometry, rather than the neighbourhood's
/// centre, supplies the target. Grey cancel text and blue table cloth fail.
pub(super) fn action(
    image: &Image,
    scale: f64,
    viewport: CanvasRect,
    near: (f64, f64),
) -> Option<CanvasRect> {
    if !scale.is_finite()
        || scale <= 0.
        || viewport.width <= 0.
        || viewport.height <= 0.
        || ![
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            near.0,
            near.1,
        ]
        .iter()
        .all(|v| v.is_finite())
    {
        return None;
    }
    let (x0, y0) = viewport.pixel(near.0 - 1.0, near.1 - 0.48);
    let (x1, y1) = viewport.pixel(near.0 + 0.9, near.1 + 0.35);
    if x0 < 0. || y0 < 0. || x1 * scale > image.width as f64 || y1 * scale > image.height as f64 {
        return None;
    }
    let (mut left, mut top, mut right, mut bottom, mut count) = (usize::MAX, usize::MAX, 0, 0, 0);
    for y in (y0 * scale) as usize..(y1 * scale) as usize {
        for x in (x0 * scale) as usize..(x1 * scale) as usize {
            let [r, g, b] = image.pixels[y * image.width + x];
            if g >= 150
                && i32::from(g) - i32::from(r) >= 35
                && (b >= 170 || i32::from(g) - i32::from(b) >= 35)
            {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
                count += 1;
            }
        }
    }
    if count < (20. * scale * scale) as usize {
        return None;
    }
    let width = (right - left + 1) as f64 / scale;
    let height = (bottom - top + 1) as f64 / scale;
    let unit = viewport.height / 9.;
    if !(0.28..=1.9).contains(&(width / unit)) || !(0.20..=0.83).contains(&(height / unit)) {
        return None;
    }
    Some(CanvasRect {
        x: left as f64 / scale,
        y: top as f64 / scale,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autoplay::majsoul::coords;

    fn native(bytes: &[u8]) -> (Image, CanvasRect) {
        let image = crate::autoplay::pixels::decode_png(bytes).unwrap();
        let rect = CanvasRect {
            x: 0.,
            y: 0.,
            width: image.width as f64,
            height: image.height as f64,
        };
        (image, rect)
    }

    #[test]
    fn actual_button_geometry_tracks_canvas_offset_and_device_scale() {
        for png in [
            include_bytes!("../fixtures/melds/action-chi.png").as_slice(),
            include_bytes!("../fixtures/melds/action-pon.png").as_slice(),
        ] {
            let (source, original) = native(png);
            let baseline = action(&source, 1., original, coords::ACTIONS[1]).unwrap();
            for downsample in [1, 2] {
                let (w, h) = (source.width / downsample, source.height / downsample);
                let (left, top) = (31, 19);
                let mut image = Image {
                    width: w + left + 7,
                    height: h + top + 11,
                    pixels: vec![[0, 0, 0]; (w + left + 7) * (h + top + 11)],
                };
                for y in 0..h {
                    for x in 0..w {
                        image.pixels[(y + top) * image.width + x + left] =
                            source.pixels[y * downsample * source.width + x * downsample];
                    }
                }
                for dpr in [1., 2.] {
                    let viewport = CanvasRect {
                        x: left as f64 / dpr,
                        y: top as f64 / dpr,
                        width: w as f64 / dpr,
                        height: h as f64 / dpr,
                    };
                    let face = action(&image, dpr, viewport, coords::ACTIONS[1]).unwrap();
                    let factor = downsample as f64 * dpr;
                    assert!(((face.x - viewport.x) * factor - baseline.x).abs() <= 2.);
                    assert!(((face.y - viewport.y) * factor - baseline.y).abs() <= 2.);
                    assert!((face.width * factor - baseline.width).abs() <= 2.);
                    assert!((face.height * factor - baseline.height).abs() <= 2.);
                }
            }
        }
    }

    #[test]
    fn native_active_meld_buttons_supply_geometry_but_never_a_candidate_row() {
        for png in [
            include_bytes!("../fixtures/melds/action-chi.png").as_slice(),
            include_bytes!("../fixtures/melds/action-pon.png").as_slice(),
        ] {
            let (image, rect) = native(png);
            let face = action(&image, 1., rect, coords::ACTIONS[1]).expect("real active button");
            assert!(face.width > 30. && face.height > 30.);
            for choices in 2..=6 {
                assert!(read(&image, 1., rect, choices).is_none());
            }
        }
    }

    #[test]
    fn table_and_native_hand_hint_never_become_meld_choices() {
        for png in [
            include_bytes!("../fixtures/melds/no-choice.png").as_slice(),
            include_bytes!("../fixtures/melds/hand-hint.png").as_slice(),
        ] {
            let (image, rect) = native(png);
            assert!(action(&image, 1., rect, coords::ACTIONS[1]).is_none());
            for choices in 2..=6 {
                assert!(read(&image, 1., rect, choices).is_none());
            }
        }
    }
}
