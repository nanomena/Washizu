//! Reading a few pixels of the game, for what the page does not say.
//!
//! Mahjong Soul draws everything into one canvas and keeps its interface's
//! state inside its engine, out of reach of the page's script. Some of that
//! state shows only as colour: whether one of the client's own toggles (auto
//! sort, auto win, no calls, auto tsumogiri) is on is whether its glyph is
//! green, and whether a hand's result is ready to confirm is whether its
//! confirm button, blue with gold words, has been drawn (the game's own
//! screens after its last result have a yellow one in the same place, and
//! the last of them a "play again" beside it, blue with gold words too). A
//! screenshot of the page reads them, cropped here to the box that matters.
//!
//! The screenshot is of the whole view, as the stream's are: a clipped one
//! has Chromium change the page's viewport for the moment it takes, which
//! the game shows as a flash.

use crate::autoplay::context::CanvasRect;
use crate::autoplay::majsoul::coords;
use anyhow::{anyhow, Context, Result};
use chromiumoxide::cdp::browser_protocol::page::{
    CaptureScreenshotFormat, CaptureScreenshotParams,
};
use chromiumoxide::page::Page;
use std::io::Read;

/// What a toggle's glyph shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// Green: the toggle is on.
    Lit,
    /// The light grey-blue of a toggle that is off.
    Unlit,
    /// Neither: the strip is folded away, or something covers it.
    Absent,
}

/// What a look at the screen found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sight {
    /// A hand's result's confirm button, bottom right: blue with gold words.
    Button,
    /// The yellow confirm of the game's own screens after its last result,
    /// in the same place: its standings, its rank, then the room's rewards.
    Standings,
    /// "Play again", left of the yellow confirm on the last of the game's
    /// own screens.
    Rematch,
    /// What "play again" asks: match another game of the room? Its yellow
    /// confirm left of a blue cancel, over a veiled table.
    RematchAsk,
    /// No button: the result is still being drawn, or it has gone.
    Nothing,
    /// The screen could not be read.
    Unread,
}

/// What a hand's result shows in `image`, a screenshot of the whole view at
/// `scale` device pixels a CSS pixel, the game's canvas at `rect`.
pub fn result_sight(image: &Image, scale: f64, rect: CanvasRect) -> Sight {
    let (left, top, right, bottom) = coords::CONFIRM_FACE;
    let (x0, y0) = rect.pixel(left, top);
    let (x1, y1) = rect.pixel(right, bottom);
    let face = image.crop(x0 * scale, y0 * scale, (x1 - x0) * scale, (y1 - y0) * scale);
    if is_confirm(&face) {
        Sight::Button
    } else if is_game_confirm(&face) {
        Sight::Standings
    } else {
        Sight::Nothing
    }
}

/// What the game's own screens after its last result show in `image`, as
/// [`result_sight`] takes it: the question "play again" asks, else "play
/// again" where it shows (the last of them), else the yellow confirm of each,
/// else a hand's blue confirm (the last result still up), else nothing yet.
pub fn end_sight(image: &Image, scale: f64, rect: CanvasRect) -> Sight {
    let face = |(left, top, right, bottom): (f64, f64, f64, f64)| {
        let (x0, y0) = rect.pixel(left, top);
        let (x1, y1) = rect.pixel(right, bottom);
        image.crop(x0 * scale, y0 * scale, (x1 - x0) * scale, (y1 - y0) * scale)
    };
    let confirm = face(coords::CONFIRM_FACE);
    if is_game_confirm(&face(coords::REMATCH_ASK_CONFIRM_FACE))
        && is_blue_face(&face(coords::REMATCH_ASK_CANCEL_FACE))
    {
        Sight::RematchAsk
    } else if is_confirm(&face(coords::REMATCH_FACE)) && is_game_confirm(&confirm) {
        Sight::Rematch
    } else if is_game_confirm(&confirm) {
        Sight::Standings
    } else if is_confirm(&confirm) {
        Sight::Button
    } else {
        Sight::Nothing
    }
}

/// The share of `pixels` that `f` holds for.
fn share(pixels: &[[u8; 3]], f: impl Fn(i32, i32, i32) -> bool) -> f64 {
    let count = pixels
        .iter()
        .filter(|&&[r, g, b]| f(i32::from(r), i32::from(g), i32::from(b)))
        .count();
    count as f64 / pixels.len().max(1) as f64
}

/// Whether a box holds a result's confirm button: enough of it the button's
/// blue-violet face, and some of it the gold of its words ("(3) 确认"). The
/// table, the dark veil over it while a result is drawn, and the words that
/// replace the button once it is pressed are neither.
pub fn is_confirm(pixels: &[[u8; 3]]) -> bool {
    let face = share(pixels, |r, g, b| {
        b >= 100 && b - r >= 45 && b - g >= 35 && r >= 35
    });
    let words = share(pixels, |r, g, b| {
        r >= 170 && (110..=215).contains(&g) && b <= 150 && r - b >= 70
    });
    face >= 0.25 && words >= 0.03
}

/// Whether most of a box is the blue-violet face of the client's blue
/// buttons, words or not: the cancel of "play again"'s question has pale ones.
fn is_blue_face(pixels: &[[u8; 3]]) -> bool {
    share(pixels, |r, g, b| {
        b >= 100 && b - r >= 45 && b - g >= 35 && r >= 35
    }) >= 0.5
}

/// Whether a box holds the yellow confirm of the game's standings or rank
/// screen: most of it the button's yellow, some of it the dark red of its
/// words ("确认"). The lobby's gold icons, which come after, are not.
pub fn is_game_confirm(pixels: &[[u8; 3]]) -> bool {
    let face = share(pixels, |r, g, b| {
        r >= 200 && g >= 150 && b <= 160 && r - b >= 70 && g - b >= 40
    });
    let words = share(pixels, |r, g, b| {
        (70..=170).contains(&r) && g <= 90 && b <= 90 && r - g >= 30
    });
    face >= 0.4 && words >= 0.03
}

/// What the glyph in the CSS-pixel box `(x, y, w, h)` of the page shows.
pub async fn glyph(page: &Page, x: f64, y: f64, w: f64, h: f64) -> Result<Glyph> {
    let (image, scale) = shot(page).await?;
    Ok(classify(&image.crop(
        x * scale,
        y * scale,
        w * scale,
        h * scale,
    )))
}

/// A screenshot of the whole view, and how many of its pixels make a CSS
/// pixel.
pub async fn shot(page: &Page) -> Result<(Image, f64)> {
    // The view's width in CSS pixels: the screenshot is in device pixels.
    let css_width = page
        .evaluate("window.innerWidth")
        .await
        .context("CDP evaluate innerWidth")?
        .value()
        .and_then(serde_json::Value::as_f64)
        .filter(|w| *w > 0.0)
        .ok_or_else(|| anyhow!("screenshot: the view has no width"))?;
    let params = CaptureScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .optimize_for_speed(true)
        .build();
    let shot = page
        .execute(params)
        .await
        .context("CDP captureScreenshot")?;
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD
        .decode(AsRef::<str>::as_ref(&shot.result.data))
        .context("screenshot: base64")?;
    let image = decode_png(&png).ok_or_else(|| anyhow!("screenshot: not a PNG this reads"))?;
    let scale = image.width as f64 / css_width;
    Ok((image, scale))
}

/// A decoded screenshot: its RGB pixels, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[u8; 3]>,
}

impl Image {
    /// The pixels of the box `(x, y, w, h)`, in this image's pixels, clipped
    /// to the image: at least one pixel wide and high where it overlaps.
    pub fn crop(&self, x: f64, y: f64, w: f64, h: f64) -> Vec<[u8; 3]> {
        let span = |at: f64, len: f64, most: usize| {
            let start = (at.max(0.0).floor() as usize).min(most);
            let end = ((at + len).ceil().max(0.0) as usize)
                .max(start + 1)
                .min(most);
            start..end
        };
        let (columns, rows) = (span(x, w, self.width), span(y, h, self.height));
        rows.flat_map(|row| {
            let line = &self.pixels[row * self.width..(row + 1) * self.width];
            line[columns.clone()].iter().copied()
        })
        .collect()
    }
}

/// A glyph is lit when enough of its box is the toggles' green, unlit when
/// enough is their grey-blue, and absent otherwise.
pub fn classify(pixels: &[[u8; 3]]) -> Glyph {
    if pixels.is_empty() {
        return Glyph::Absent;
    }
    let green = |&[r, g, b]: &[u8; 3]| g > 140 && i32::from(g) - i32::from(r.max(b)) > 70;
    let grey = |&[r, g, b]: &[u8; 3]| {
        let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
        lo > 110 && hi - lo < 60 && i32::from(g) - i32::from(r.max(b)) < 20
    };
    let share = |f: &dyn Fn(&[u8; 3]) -> bool| {
        pixels.iter().filter(|p| f(p)).count() as f64 / pixels.len() as f64
    };
    if share(&green) >= 0.08 {
        Glyph::Lit
    } else if share(&grey) >= 0.08 {
        Glyph::Unlit
    } else {
        Glyph::Absent
    }
}

/// An 8-bit, non-interlaced RGB or RGBA PNG, as Chromium writes its
/// screenshots; `None` for anything else.
pub fn decode_png(png: &[u8]) -> Option<Image> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 8 || png[..8] != SIGNATURE {
        return None;
    }
    let (mut width, mut height, mut channels) = (0usize, 0usize, 0usize);
    let mut data = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().ok()?) as usize;
        let kind = &png[at + 4..at + 8];
        let body = png.get(at + 8..at + 8 + len)?;
        match kind {
            b"IHDR" => {
                width = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as usize;
                height = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
                let (depth, colour, interlace) = (*body.get(8)?, *body.get(9)?, *body.get(12)?);
                channels = match (depth, colour, interlace) {
                    (8, 2, 0) => 3,
                    (8, 6, 0) => 4,
                    _ => return None,
                };
            }
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 || height == 0 || channels == 0 {
        return None;
    }
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(data.as_slice())
        .read_to_end(&mut raw)
        .ok()?;
    let stride = width * channels;
    if raw.len() < height * (stride + 1) {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    let mut previous = vec![0u8; stride];
    for row in 0..height {
        let line = &raw[row * (stride + 1)..(row + 1) * (stride + 1)];
        let (filter, line) = (line[0], &line[1..]);
        let mut current = vec![0u8; stride];
        for i in 0..stride {
            let left = if i >= channels {
                current[i - channels]
            } else {
                0
            };
            let up = previous[i];
            let corner = if i >= channels {
                previous[i - channels]
            } else {
                0
            };
            let predicted = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, corner),
                _ => return None,
            };
            current[i] = line[i].wrapping_add(predicted);
        }
        out.extend(current.chunks_exact(channels).map(|p| [p[0], p[1], p[2]]));
        previous = current;
    }
    Some(Image {
        width,
        height,
        pixels: out,
    })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i16::from(a), i16::from(b), i16::from(c));
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A PNG of `pixels` (RGB, one row each of `width`), filtered with `filter`.
    fn png(width: usize, rows: &[Vec<[u8; 3]>], filter: u8) -> Vec<u8> {
        let mut raw = Vec::new();
        let mut previous = vec![0u8; width * 3];
        for row in rows {
            let line: Vec<u8> = row.iter().flatten().copied().collect();
            raw.push(filter);
            for i in 0..line.len() {
                let left = if i >= 3 { line[i - 3] } else { 0 };
                let predicted = match filter {
                    0 => 0,
                    1 => left,
                    2 => previous[i],
                    _ => unreachable!(),
                };
                raw.push(line[i].wrapping_sub(predicted));
            }
            previous = line;
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&raw).unwrap();
        let idat = z.finish().unwrap();
        let chunk = |kind: &[u8], body: &[u8]| {
            let mut c = (body.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(body);
            c.extend_from_slice(&[0, 0, 0, 0]); // the CRC is not checked
            c
        };
        let mut ihdr = (width as u32).to_be_bytes().to_vec();
        ihdr.extend_from_slice(&(rows.len() as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        out.extend(chunk(b"IHDR", &ihdr));
        out.extend(chunk(b"IDAT", &idat));
        out.extend(chunk(b"IEND", &[]));
        out
    }

    /// Pixels come back as they went in, whichever filter the rows use.
    #[test]
    fn a_png_reads_back_its_pixels() {
        let rows = vec![
            vec![[10, 200, 30], [40, 50, 60]],
            vec![[255, 0, 128], [7, 8, 9]],
        ];
        for filter in [0, 1, 2] {
            let read = decode_png(&png(2, &rows, filter)).expect("decoded");
            assert_eq!((read.width, read.height), (2, 2));
            assert_eq!(read.pixels, rows.concat(), "filter {filter}");
        }
        assert!(decode_png(b"not a png").is_none());
    }

    /// A box of the whole view is cut out of it, clipped to its edges, and
    /// never comes out empty where it overlaps.
    #[test]
    fn a_box_is_cut_out_of_the_whole_view() {
        let width = 4;
        let pixels: Vec<[u8; 3]> = (0..12u8).map(|i| [i, 0, 0]).collect();
        let image = Image {
            width,
            height: 3,
            pixels,
        };
        let red = |cut: Vec<[u8; 3]>| cut.iter().map(|p| p[0]).collect::<Vec<_>>();
        assert_eq!(red(image.crop(1.0, 1.0, 2.0, 2.0)), [5, 6, 9, 10]);
        assert_eq!(
            red(image.crop(3.0, 0.0, 5.0, 1.0)),
            [3],
            "clipped at the edge"
        );
        assert_eq!(
            red(image.crop(1.2, 0.0, 0.3, 0.3)),
            [1],
            "a sliver is a pixel"
        );
        assert!(
            image.crop(9.0, 9.0, 2.0, 2.0).is_empty(),
            "outside the view"
        );
    }

    /// The toggles' green reads lit, their grey-blue unlit, the dark strip
    /// with nothing on it absent. The colours are the ones measured on the
    /// client: a lit glyph about (60, 230, 60), an unlit one about (150, 158, 170).
    #[test]
    fn a_glyph_reads_lit_unlit_or_absent() {
        let back = [20u8, 24, 52];
        let with = |glyph: [u8; 3]| -> Vec<[u8; 3]> {
            (0..100)
                .map(|i| if i % 4 == 0 { glyph } else { back })
                .collect()
        };
        assert_eq!(classify(&with([60, 230, 60])), Glyph::Lit);
        assert_eq!(classify(&with([150, 158, 170])), Glyph::Unlit);
        assert_eq!(classify(&vec![back; 100]), Glyph::Absent);
        assert_eq!(classify(&[]), Glyph::Absent);
    }

    /// A box of `parts` (colour, share in hundredths), in that order, a
    /// hundred pixels for each share point.
    fn mixed(parts: &[([u8; 3], usize)]) -> Vec<[u8; 3]> {
        parts
            .iter()
            .flat_map(|&(colour, share)| std::iter::repeat_n(colour, share * 100))
            .collect()
    }

    /// The confirm button reads as one by its blue face and gold words; the
    /// table (even as bright as it is while a hand is dealt), the dark veil
    /// over it while a result is drawn, and the white words left once it is
    /// pressed do not. The colours are the ones measured on the client: the
    /// face about (53, 69, 124), the words (221, 174, 121).
    #[test]
    fn the_confirm_button_reads_by_its_face_and_words() {
        let button = mixed(&[
            ([53, 69, 124], 44),
            ([221, 174, 121], 7),
            ([47, 58, 88], 49),
        ]);
        assert!(is_confirm(&button));
        let dealt = mixed(&[([45, 65, 123], 39), ([30, 53, 95], 61)]);
        assert!(!is_confirm(&dealt), "a bright table has no gold");
        assert!(!is_confirm(&mixed(&[([30, 50, 87], 100)])), "the table");
        assert!(!is_confirm(&mixed(&[([6, 10, 19], 100)])), "the veil");
        let pressed = mixed(&[([17, 26, 44], 85), ([235, 235, 235], 15)]);
        assert!(!is_confirm(&pressed), "the words once pressed");
        assert!(!is_confirm(&[]));
    }

    /// The game's yellow confirm reads as one by its yellow face and dark red
    /// words, and not as a hand's; the lobby's gold icons are neither. The
    /// colours are the ones measured on the client: the face about
    /// (241, 206, 107), the words (110, 51, 51); in the lobby a little of
    /// both over pink.
    #[test]
    fn the_game_s_yellow_confirm_reads_by_its_face_and_words() {
        let yellow = mixed(&[
            ([241, 206, 107], 79),
            ([110, 51, 51], 8),
            ([116, 92, 52], 13),
        ]);
        assert!(is_game_confirm(&yellow));
        assert!(!is_confirm(&yellow));
        let button = mixed(&[
            ([53, 69, 124], 44),
            ([221, 174, 121], 7),
            ([47, 58, 88], 49),
        ]);
        assert!(
            !is_game_confirm(&button),
            "a hand's button is not the game's"
        );
        let lobby = mixed(&[
            ([216, 187, 124], 11),
            ([132, 55, 45], 10),
            ([172, 90, 100], 79),
        ]);
        assert!(!is_game_confirm(&lobby) && !is_confirm(&lobby), "the lobby");
        assert!(
            !is_game_confirm(&mixed(&[([30, 50, 87], 100)])),
            "the table"
        );
        assert!(!is_game_confirm(&[]));
    }

    /// The button is looked for where the canvas has it, at the screenshot's scale.
    #[test]
    fn a_result_is_seen_by_its_button_where_the_canvas_has_it() {
        // A canvas of 160x90 CSS pixels at (20, 10), shot at two device pixels a CSS pixel.
        let rect = CanvasRect {
            x: 20.0,
            y: 10.0,
            width: 160.0,
            height: 90.0,
        };
        let (width, height) = (400, 220);
        let table = Image {
            width,
            height,
            pixels: vec![[30, 50, 87]; width * height],
        };
        assert_eq!(result_sight(&table, 2.0, rect), Sight::Nothing);
        let mut shown = table.clone();
        let (left, top, right, bottom) = coords::CONFIRM_FACE;
        let (x0, y0) = rect.pixel(left, top);
        let (x1, y1) = rect.pixel(right, bottom);
        for y in (y0 * 2.0) as usize..(y1 * 2.0) as usize {
            for x in (x0 * 2.0) as usize..(x1 * 2.0) as usize {
                // Gold words on every tenth pixel of the face.
                shown.pixels[y * width + x] = if (x + y) % 10 == 0 {
                    [221, 174, 121]
                } else {
                    [53, 69, 124]
                };
            }
        }
        assert_eq!(result_sight(&shown, 2.0, rect), Sight::Button);
        assert_eq!(
            result_sight(&shown, 1.0, rect),
            Sight::Nothing,
            "at the wrong scale it is elsewhere"
        );
        let mut standings = table.clone();
        for y in (y0 * 2.0) as usize..(y1 * 2.0) as usize {
            for x in (x0 * 2.0) as usize..(x1 * 2.0) as usize {
                standings.pixels[y * width + x] = if (x + y) % 10 == 0 {
                    [110, 51, 51]
                } else {
                    [241, 206, 107]
                };
            }
        }
        assert_eq!(result_sight(&standings, 2.0, rect), Sight::Standings);
    }

    /// The game's own screens are read by their buttons: "play again" only
    /// beside the yellow confirm, the yellow confirm alone, a hand's blue
    /// confirm, or nothing. A blue box where "play again" would be, with no
    /// yellow confirm beside it, is not "play again".
    #[test]
    fn the_end_screens_are_seen_by_their_buttons() {
        let rect = CanvasRect {
            x: 20.0,
            y: 10.0,
            width: 160.0,
            height: 90.0,
        };
        let (width, height) = (400, 220);
        let table = Image {
            width,
            height,
            pixels: vec![[30, 50, 87]; width * height],
        };
        let paint = |image: &mut Image,
                     (left, top, right, bottom): (f64, f64, f64, f64),
                     face: [u8; 3],
                     words: [u8; 3]| {
            let (x0, y0) = rect.pixel(left, top);
            let (x1, y1) = rect.pixel(right, bottom);
            for y in (y0 * 2.0) as usize..(y1 * 2.0) as usize {
                for x in (x0 * 2.0) as usize..(x1 * 2.0) as usize {
                    image.pixels[y * width + x] = if (x + y) % 10 == 0 { words } else { face };
                }
            }
        };
        let (blue, gold) = ([48, 80, 144], [224, 176, 112]);
        let (yellow, red) = ([241, 206, 107], [110, 51, 51]);
        assert_eq!(end_sight(&table, 2.0, rect), Sight::Nothing);
        let mut standings = table.clone();
        paint(&mut standings, coords::CONFIRM_FACE, yellow, red);
        assert_eq!(end_sight(&standings, 2.0, rect), Sight::Standings);
        let mut last = standings.clone();
        paint(&mut last, coords::REMATCH_FACE, blue, gold);
        assert_eq!(end_sight(&last, 2.0, rect), Sight::Rematch);
        let mut alone = table.clone();
        paint(&mut alone, coords::REMATCH_FACE, blue, gold);
        assert_eq!(end_sight(&alone, 2.0, rect), Sight::Nothing);
        let mut result = table.clone();
        paint(
            &mut result,
            coords::CONFIRM_FACE,
            [53, 69, 124],
            [221, 174, 121],
        );
        assert_eq!(end_sight(&result, 2.0, rect), Sight::Button);
        // The question over a veiled table: its yellow confirm beside its blue cancel.
        let mut ask = table.clone();
        paint(&mut ask, coords::REMATCH_ASK_CONFIRM_FACE, yellow, red);
        paint(
            &mut ask,
            coords::REMATCH_ASK_CANCEL_FACE,
            [57, 86, 144],
            [216, 192, 144],
        );
        assert_eq!(end_sight(&ask, 2.0, rect), Sight::RematchAsk);
        let mut half = table.clone();
        paint(&mut half, coords::REMATCH_ASK_CONFIRM_FACE, yellow, red);
        assert_eq!(
            end_sight(&half, 2.0, rect),
            Sight::Nothing,
            "a yellow box alone is not the question"
        );
        // A hand's result never reads as "play again".
        assert_eq!(result_sight(&last, 2.0, rect), Sight::Standings);
    }
}
