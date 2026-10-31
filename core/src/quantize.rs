//! Bringing a picture to at most 255 colours, as Sixel's colour registers hold.
//!
//! Ported from BlueEdit (`Sixel.Quantize` and `MedianCut` in `src/Tui/CellPicture.cs`); the output is meant to be
//! byte for byte the same, so the integer arithmetic and the order of the steps are kept as they are there.

use std::collections::HashMap;

/// The palette index of the background: a pixel under half opaque.
pub const BACKGROUND: u8 = 255;

/// The most colours a palette holds; index [`BACKGROUND`] is not one of them.
const MAX_COLOURS: usize = 255;

/// A picture as palette indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexed {
    /// One palette index per pixel, row after row; [`BACKGROUND`] for a transparent pixel.
    pub index: Vec<u8>,
    /// Colours as `0x00RRGGBB`, at most 255.
    pub palette: Vec<u32>,
}

fn is_transparent(argb: u32) -> bool {
    argb >> 24 < 128
}

/// Pixels (`0xAARRGGBB`, `width` to a row) brought to at most 255 colours.
///
/// A picture with that few (a diagram, a chart, a screenshot) keeps them exactly. One with more (a photo) gets a
/// palette made for it (median cut) and the error spread to the next pixels (Floyd–Steinberg), which keeps gradients
/// from banding. A pixel under half opaque is the background; a half-transparent edge is taken as opaque.
pub fn quantize(argb: &[u32], width: usize) -> Indexed {
    if width == 0 {
        return Indexed {
            index: vec![BACKGROUND; argb.len()],
            palette: Vec::new(),
        };
    }
    if let Some(exact) = exact_palette(argb) {
        return exact;
    }
    let mut nearest = median_cut(argb);
    let height = argb.len() / width;
    let mut index = vec![BACKGROUND; argb.len()];
    // the error carried to this row and the next, per channel
    let stride = width + 2;
    let mut err = vec![0i32; 2 * stride * 3];
    let at = |row: usize, x: usize, ch: usize| (row * stride + x) * 3 + ch;
    for y in 0..height {
        for x in 0..width {
            let p = argb[y * width + x];
            if is_transparent(p) {
                continue;
            }
            let channel = |shift: u32, ch: usize| {
                let v = i32::try_from((p >> shift) & 255).unwrap_or(255);
                (v + err[at(0, x + 1, ch)] / 16).clamp(0, 255)
            };
            let (r, g, b) = (channel(16, 0), channel(8, 1), channel(0, 2));
            let bin = usize::try_from((r >> 3 << 10) | (g >> 3 << 5) | (b >> 3)).unwrap_or(0);
            let c = nearest.get(bin);
            index[y * width + x] = u8::try_from(c).unwrap_or(BACKGROUND);
            let chosen = nearest.palette[c];
            let errors = [
                r - i32::try_from(chosen >> 16).unwrap_or(0),
                g - i32::try_from((chosen >> 8) & 255).unwrap_or(0),
                b - i32::try_from(chosen & 255).unwrap_or(0),
            ];
            for (ch, e) in errors.into_iter().enumerate() {
                err[at(0, x + 2, ch)] += e * 7;
                err[at(1, x, ch)] += e * 3;
                err[at(1, x + 1, ch)] += e * 5;
                err[at(1, x + 2, ch)] += e;
            }
        }
        for x in 0..stride {
            for ch in 0..3 {
                err[at(0, x, ch)] = err[at(1, x, ch)];
                err[at(1, x, ch)] = 0;
            }
        }
    }
    Indexed {
        index,
        palette: nearest.palette,
    }
}

/// The picture's own colours in order of first appearance, if there are at most 255.
pub(crate) fn exact_palette(argb: &[u32]) -> Option<Indexed> {
    let mut seen: HashMap<u32, u8> = HashMap::new();
    let mut palette: Vec<u32> = Vec::new();
    for &p in argb {
        if is_transparent(p) {
            continue;
        }
        let rgb = p & 0x00FF_FFFF;
        if seen.contains_key(&rgb) {
            continue;
        }
        if palette.len() == MAX_COLOURS {
            return None;
        }
        seen.insert(rgb, u8::try_from(palette.len()).unwrap_or(BACKGROUND));
        palette.push(rgb);
    }
    let index = argb
        .iter()
        .map(|&p| {
            if is_transparent(p) {
                BACKGROUND
            } else {
                seen.get(&(p & 0x00FF_FFFF)).copied().unwrap_or(BACKGROUND)
            }
        })
        .collect();
    Some(Indexed { index, palette })
}

/// The 5-bit colour bin of a pixel: red, green, blue, five bits each.
fn bin_of(r: u32, g: u32, b: u32) -> usize {
    usize::try_from((r >> 3 << 10) | (g >> 3 << 5) | (b >> 3)).unwrap_or(0)
}

/// Channel `ch` (0 red, 1 green, 2 blue) of a bin, 0..=31.
fn channel(bin: usize, ch: usize) -> i32 {
    i32::try_from((bin >> (10 - 5 * ch)) & 31).unwrap_or(0)
}

/// The palette colour nearest to a bin, worked out when first needed (dithering reaches bins the picture has not).
struct Nearest {
    palette: Vec<u32>,
    table: Vec<Option<usize>>,
}

impl Nearest {
    fn get(&mut self, bin: usize) -> usize {
        if let Some(hit) = self.table[bin] {
            return hit;
        }
        let r = (channel(bin, 0) << 3) + 4;
        let g = (channel(bin, 1) << 3) + 4;
        let b = (channel(bin, 2) << 3) + 4;
        let (mut best, mut best_d) = (0, i32::MAX);
        for (i, &c) in self.palette.iter().enumerate() {
            let dr = r - i32::try_from(c >> 16).unwrap_or(0);
            let dg = g - i32::try_from((c >> 8) & 255).unwrap_or(0);
            let db = b - i32::try_from(c & 255).unwrap_or(0);
            let d = 2 * dr * dr + 4 * dg * dg + 3 * db * db; // the eye: green most, then blue, then red
            if d < best_d {
                (best, best_d) = (i, d);
            }
        }
        self.table[bin] = Some(best);
        best
    }
}

/// A palette of 255 made for the picture (median cut): the colours, 5 bits a channel, are split into boxes, the box
/// with the most pixels across the widest spread halved at its median each time; a box's colour is the mean of its
/// pixels, so a colour that fills a box alone (a chart's bar) is kept exactly.
fn median_cut(argb: &[u32]) -> Nearest {
    Nearest {
        palette: median_cut_palette(argb),
        table: vec![None; 32768],
    }
}

fn median_cut_palette(argb: &[u32]) -> Vec<u32> {
    let mut count = vec![0u64; 32768];
    let mut sum = vec![0u64; 32768 * 3];
    for &p in argb {
        if is_transparent(p) {
            continue;
        }
        let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
        let bin = bin_of(r, g, b);
        count[bin] += 1;
        sum[bin * 3] += u64::from(r);
        sum[bin * 3 + 1] += u64::from(g);
        sum[bin * 3 + 2] += u64::from(b);
    }
    let mut boxes: Vec<Vec<usize>> = vec![(0..32768).filter(|&i| count[i] > 0).collect()];
    while boxes.len() < MAX_COLOURS {
        let (mut pick, mut score, mut axis) = (None, 0u64, 0);
        for (i, bx) in boxes.iter().enumerate() {
            if bx.len() < 2 {
                continue;
            }
            let (a, spread) = widest(bx);
            let pixels: u64 = bx.iter().map(|&bin| count[bin]).sum();
            let s = pixels * u64::try_from(spread + 1).unwrap_or(0);
            if s > score {
                (pick, score, axis) = (Some(i), s, a);
            }
        }
        let Some(pick) = pick else { break };
        let mut sorted = boxes[pick].clone();
        sorted.sort_by_key(|&bin| channel(bin, axis)); // stable, as LINQ's OrderBy
        let half = sorted.iter().map(|&bin| count[bin]).sum::<u64>() / 2;
        let (mut run, mut cut) = (0u64, 1usize);
        while cut < sorted.len() - 1 {
            run += count[sorted[cut - 1]];
            if run >= half {
                break;
            }
            cut += 1;
        }
        boxes[pick] = sorted[..cut].to_vec();
        boxes.push(sorted[cut..].to_vec());
    }
    boxes
        .iter()
        .map(|bx| {
            let (mut n, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
            for &bin in bx {
                n += count[bin];
                r += sum[bin * 3];
                g += sum[bin * 3 + 1];
                b += sum[bin * 3 + 2];
            }
            u32::try_from((r / n) << 16 | (g / n) << 8 | (b / n)).unwrap_or(0)
        })
        .collect()
}

/// The channel with the widest spread over a box, and that spread.
fn widest(bx: &[usize]) -> (usize, i32) {
    let (mut best, mut spread) = (0, -1);
    for ch in 0..3 {
        let (mut lo, mut hi) = (31, 0);
        for &bin in bx {
            let v = channel(bin, ch);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if hi - lo > spread {
            (best, spread) = (ch, hi - lo);
        }
    }
    (best, spread)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPAQUE: u32 = 0xFF00_0000;

    /// `n` distinct opaque colours.
    fn distinct(n: u32) -> Vec<u32> {
        (0..n).map(|i| OPAQUE | ((i * 4099) & 0x00FF_FFFF)).collect()
    }

    #[test]
    fn a_picture_of_few_colours_keeps_them_exactly_in_order_of_appearance() {
        let (red, green, blue) = (OPAQUE | 0xFF0000, OPAQUE | 0x00FF00, OPAQUE | 0x0000FF);
        let got = quantize(&[red, green, red, blue], 2);
        assert_eq!(got.palette, [0xFF0000, 0x00FF00, 0x0000FF]);
        assert_eq!(got.index, [0, 1, 0, 2]);
    }

    #[test]
    fn a_pixel_under_half_opaque_is_the_background_and_not_a_colour() {
        let got = quantize(&[0x7F12_3456, OPAQUE | 0xABCDEF, 0x8012_3456], 3);
        assert_eq!(got.index, [BACKGROUND, 0, 1]);
        assert_eq!(got.palette, [0xABCDEF, 0x123456]);
    }

    #[test]
    fn exactly_255_colours_are_still_exact_and_256_are_not() {
        let exact = quantize(&distinct(255), 255);
        assert_eq!(exact.palette.len(), 255);
        assert!(exact.index.iter().enumerate().all(|(i, &c)| usize::from(c) == i));

        let many = quantize(&distinct(256), 256);
        assert!(many.palette.len() <= 255);
        assert_ne!(
            many.palette,
            distinct(256).iter().map(|c| c & 0x00FF_FFFF).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_photo_stays_within_the_palette_and_every_index_points_into_it() {
        let (w, h) = (64, 48);
        let photo: Vec<u32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                OPAQUE | ((x * 4) << 16) | ((y * 5) << 8) | ((x * y) % 256)
            })
            .collect();
        let got = quantize(&photo, w as usize);
        assert!(got.palette.len() <= 255);
        assert_eq!(got.index.len(), photo.len());
        assert!(got.index.iter().all(|&c| usize::from(c) < got.palette.len()));
        assert_eq!(got, quantize(&photo, w as usize), "same input, same output");
    }

    #[test]
    fn median_cut_keeps_a_flat_colour_exactly_among_a_photos_many() {
        // flat bars of the three primaries over a ramp of 256 greys: more than 255 colours, so not the exact path
        let mut picture: Vec<u32> = (0..256).map(|g| OPAQUE | (g * 0x010101)).collect();
        picture.extend([OPAQUE | 0xFF0000; 40]);
        picture.extend([OPAQUE | 0x00FF00; 40]);
        picture.extend([OPAQUE | 0x0000FF; 40]);
        let got = quantize(&picture, picture.len());
        for primary in [0xFF0000, 0x00FF00, 0x0000FF] {
            assert!(
                got.palette.contains(&primary),
                "{primary:06X} lost: {:06X?}",
                got.palette
            );
        }
        let first_bar = usize::from(got.index[256]);
        assert_eq!(got.palette[first_bar], 0xFF0000);
    }

    #[test]
    fn a_transparent_pixel_stays_the_background_through_dithering() {
        let mut picture = distinct(300);
        picture[7] = 0x0000_0000;
        let got = quantize(&picture, 300);
        assert_eq!(got.index[7], BACKGROUND);
        assert!(got.index.iter().enumerate().all(|(i, &c)| i == 7 || c != BACKGROUND));
    }

    #[test]
    fn nothing_to_quantize_gives_nothing() {
        assert_eq!(
            quantize(&[], 0),
            Indexed {
                index: Vec::new(),
                palette: Vec::new()
            }
        );
        assert_eq!(quantize(&[0x0000_0000], 1).index, [BACKGROUND]);
    }
}
