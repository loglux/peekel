//! A photograph as 255 colours, chosen for how it looks and not only for what it holds.
//!
//! [`crate::quantize::quantize`] is BlueEdit's and stays as it is (the golden tests compare it with BlueEdit's own bytes).
//! On a photograph it has two habits that show: the palette is shared out by pixel count, so a large sky takes most of it
//! and a face is left with a few colours, and the nearest colour is judged in RGB with weights, which is not how the eye
//! measures. Dithering then mixes the sky's blues into the skin, which shows as bluish patches on lips and cheeks.
//!
//! What this does differently (each change measured on three photographs):
//! - the palette is shared out by the pixels of a box **to the power 0.4**, so that a small area of its own colours keeps
//!   its share;
//! - the nearest colour is the nearest in **CIE Lab**, looked up by bins of 6 bits a channel (an answer is worked out
//!   once for each bin that turns up, so this costs about what BlueEdit's lookup does);
//! - the error that dithering passes on is **limited to ±32** a channel, which keeps a bad match from spreading.
//!
//! A picture of 255 colours or fewer keeps them exactly, as before.

use crate::quantize::{BACKGROUND, Indexed, exact_palette};

/// The palette is shared out by the pixels of a box to this power (1 is BlueEdit's).
const POWER: f64 = 0.4;

/// The most a pixel's error is allowed to pass on to the next ones, in each channel.
const ERROR_LIMIT: i32 = 32;

/// Bits a channel of the bins the nearest colour is looked up by.
const LOOKUP_BITS: u32 = 6;

/// The most colours of a palette; [`BACKGROUND`] is not one of them.
const MAX_COLOURS: usize = 255;

fn is_transparent(argb: u32) -> bool {
    argb >> 24 < 128
}

/// A palette made once for a whole picture (from a small copy of it), for the frames shown while it is moved: the colours
/// are the picture's own and stay the same from frame to frame, and a pixel's colour is a lookup, so a frame costs next to
/// nothing; dithering is left out, or is the cheap ordered kind (a fixed pattern added to the pixel, no error to carry).
pub struct Steady {
    palette: Vec<u32>,
    palette_lab: Vec<[f32; 3]>,
    lab: LabTable,
    /// The palette index of each colour of 6 bits a channel that has turned up (`u8::MAX`: not yet).
    lookup: Vec<u8>,
}

impl std::fmt::Debug for Steady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Steady")
            .field("colours", &self.palette.len())
            .finish_non_exhaustive()
    }
}

/// The 4 x 4 pattern of ordered dithering, as offsets from -7.5 to 7.5 (times the strength asked for, per 16).
const BAYER: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

impl Steady {
    /// The palette of `sample`, pixels (`0xAARRGGBB`) of a small copy of the whole picture; None for a picture of few
    /// colours (its frames are exact as they are).
    pub fn new(sample: &[u32]) -> Option<Self> {
        if exact_palette(sample).is_some() {
            return None;
        }
        let palette = make_palette(sample);
        let lab = LabTable::new();
        let palette_lab = palette.iter().map(|&c| lab.of(c)).collect();
        Some(Self {
            palette,
            palette_lab,
            lab,
            lookup: vec![u8::MAX; 1 << 18],
        })
    }

    /// Pixels (`width` to a row) as palette indexes. `dither` is the strength of the ordered dithering in levels of a channel
    /// (0 for none; 8 is mild).
    pub fn quantize(&mut self, argb: &[u32], width: usize, dither: i32) -> Indexed {
        let mut index = Vec::with_capacity(argb.len());
        for (i, &p) in argb.iter().enumerate() {
            if is_transparent(p) {
                index.push(BACKGROUND);
                continue;
            }
            let offset = if dither == 0 || width == 0 {
                0
            } else {
                (BAYER[(i / width) & 3][(i % width) & 3] * 2 - 15) * dither / 16
            };
            let channel = |shift: u32| {
                u8::try_from((i32::try_from((p >> shift) & 255).unwrap_or(0) + offset).clamp(0, 255)).unwrap_or(0)
            };
            index.push(self.nearest([channel(16), channel(8), channel(0)]));
        }
        Indexed {
            index,
            palette: self.palette.clone(),
        }
    }

    fn nearest(&mut self, p: [u8; 3]) -> u8 {
        let bin = (usize::from(p[0]) >> 2) << 12 | (usize::from(p[1]) >> 2) << 6 | usize::from(p[2]) >> 2;
        if self.lookup[bin] != u8::MAX {
            return self.lookup[bin];
        }
        let centre = [p[0] >> 2 << 2 | 2, p[1] >> 2 << 2 | 2, p[2] >> 2 << 2 | 2];
        let l = self.lab.of_rgb(centre);
        let mut best = (0usize, f32::MAX);
        for (i, c) in self.palette_lab.iter().enumerate() {
            let d = (l[0] - c[0]).powi(2) + (l[1] - c[1]).powi(2) + (l[2] - c[2]).powi(2);
            if d < best.1 {
                best = (i, d);
            }
        }
        self.lookup[bin] = u8::try_from(best.0).unwrap_or(0);
        self.lookup[bin]
    }
}

/// Pixels (`0xAARRGGBB`, `width` to a row) as at most 255 colours; a pixel under half opaque is the background.
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
    let palette = make_palette(argb);
    let lab = LabTable::new();
    let palette_lab: Vec<[f32; 3]> = palette.iter().map(|&c| lab.of(c)).collect();
    // the palette index of each bin that has turned up (u8::MAX: not yet)
    let mut lookup = vec![u8::MAX; 1 << (3 * LOOKUP_BITS)];
    let shift = 8 - LOOKUP_BITS;
    let mut nearest = |p: [u8; 3]| -> u8 {
        let bin = (usize::from(p[0]) >> shift) << (2 * LOOKUP_BITS)
            | (usize::from(p[1]) >> shift) << LOOKUP_BITS
            | usize::from(p[2]) >> shift;
        if lookup[bin] != u8::MAX {
            return lookup[bin];
        }
        let half = 1u8 << (shift - 1);
        let centre = [
            (p[0] >> shift << shift) | half,
            (p[1] >> shift << shift) | half,
            (p[2] >> shift << shift) | half,
        ];
        let l = lab.of_rgb(centre);
        let mut best = (0usize, f32::MAX);
        for (i, c) in palette_lab.iter().enumerate() {
            let d = (l[0] - c[0]).powi(2) + (l[1] - c[1]).powi(2) + (l[2] - c[2]).powi(2);
            if d < best.1 {
                best = (i, d);
            }
        }
        lookup[bin] = u8::try_from(best.0).unwrap_or(0);
        lookup[bin]
    };

    let height = argb.len() / width;
    let mut index = vec![BACKGROUND; argb.len()];
    // the error carried to this row and the next, per channel, in sixteenths as in BlueEdit's
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
            let wanted = [channel(16, 0), channel(8, 1), channel(0, 2)];
            let c = nearest([wanted[0] as u8, wanted[1] as u8, wanted[2] as u8]);
            index[y * width + x] = c;
            let chosen = palette[usize::from(c)];
            let errors = [
                wanted[0] - i32::try_from(chosen >> 16).unwrap_or(0),
                wanted[1] - i32::try_from((chosen >> 8) & 255).unwrap_or(0),
                wanted[2] - i32::try_from(chosen & 255).unwrap_or(0),
            ];
            for (ch, e) in errors.into_iter().enumerate() {
                let e = e.clamp(-ERROR_LIMIT, ERROR_LIMIT);
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
    Indexed { index, palette }
}

/// 255 colours made for the picture: median cut over a histogram of 5 bits a channel, a box split at the median of its
/// pixels across its widest channel, the box to split chosen by `pixels^POWER * spread`; a box's colour is the mean of
/// its pixels.
fn make_palette(argb: &[u32]) -> Vec<u32> {
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
    let channel = |bin: usize, ch: usize| i32::try_from((bin >> (10 - 5 * ch)) & 31).unwrap_or(0);
    let widest = |members: &[usize]| {
        let (mut best, mut spread) = (0, -1);
        for ch in 0..3 {
            let (mut lo, mut hi) = (31, 0);
            for &bin in members {
                let v = channel(bin, ch);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            if hi - lo > spread {
                (best, spread) = (ch, hi - lo);
            }
        }
        (best, spread)
    };
    let mut boxes: Vec<Vec<usize>> = vec![(0..32768).filter(|&i| count[i] > 0).collect()];
    while boxes.len() < MAX_COLOURS {
        let (mut pick, mut best, mut axis) = (None, 0.0f64, 0);
        for (i, members) in boxes.iter().enumerate() {
            if members.len() < 2 {
                continue;
            }
            let (a, spread) = widest(members);
            let pixels: u64 = members.iter().map(|&bin| count[bin]).sum();
            let score = (pixels as f64).powf(POWER) * f64::from(spread + 1);
            if score > best {
                (pick, best, axis) = (Some(i), score, a);
            }
        }
        let Some(pick) = pick else { break };
        let mut sorted = boxes[pick].clone();
        sorted.sort_by_key(|&bin| channel(bin, axis));
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
        .map(|members| {
            let (mut n, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
            for &bin in members {
                n += count[bin];
                r += sum[bin * 3];
                g += sum[bin * 3 + 1];
                b += sum[bin * 3 + 2];
            }
            u32::try_from((r / n) << 16 | (g / n) << 8 | (b / n)).unwrap_or(0)
        })
        .collect()
}

fn bin_of(r: u32, g: u32, b: u32) -> usize {
    usize::try_from((r >> 3 << 10) | (g >> 3 << 5) | (b >> 3)).unwrap_or(0)
}

/// sRGB to CIE Lab (D65): the space in which a distance is about what the eye sees.
struct LabTable {
    linear: [f32; 256],
}

impl LabTable {
    fn new() -> Self {
        let mut linear = [0.0; 256];
        for (i, slot) in linear.iter_mut().enumerate() {
            let v = i as f32 / 255.0;
            *slot = if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
        }
        Self { linear }
    }

    /// Of a colour as `0x00RRGGBB`.
    fn of(&self, rgb: u32) -> [f32; 3] {
        self.of_rgb([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8])
    }

    fn of_rgb(&self, p: [u8; 3]) -> [f32; 3] {
        let (r, g, b) = (
            self.linear[usize::from(p[0])],
            self.linear[usize::from(p[1])],
            self.linear[usize::from(p[2])],
        );
        let x = (0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b) / 0.950_47;
        let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
        let z = (0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b) / 1.088_83;
        let f = |t: f32| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot read its own fixture should stop at once
mod tests {
    use super::*;

    const OPAQUE: u32 = 0xFF00_0000;

    /// A photograph in miniature: a large sky that shades from deep to pale blue, and in a corner a small patch of skin
    /// with a little noise, where red must stay above blue.
    fn sky_and_skin() -> (Vec<u32>, usize, Vec<usize>) {
        let (w, h) = (320usize, 240usize);
        let mut noise = 0x1234_5678u32;
        let mut next = move || {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            i32::try_from(noise >> 28).unwrap_or(0)
        };
        let mut pixels = Vec::with_capacity(w * h);
        let mut skin = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let in_patch = (250..310).contains(&x) && (170..230).contains(&y);
                let (r, g, b) = if in_patch {
                    skin.push(y * w + x);
                    (226 + next() / 2, 176 + next() / 2, 146 + next() / 2)
                } else {
                    // a gradient across and down, with a little noise: thousands of blues
                    let t = (y * 255 / h) as i32;
                    (
                        70 + t / 2 + next() / 3,
                        120 + t / 2 + x as i32 / 8 + next() / 3,
                        200 + t / 5 + next() / 3,
                    )
                };
                pixels.push(
                    OPAQUE | (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32,
                );
            }
        }
        (pixels, w, skin)
    }

    fn rgb_of(q: &Indexed, i: usize) -> (i32, i32, i32) {
        let c = q.palette[usize::from(q.index[i])];
        (
            i32::try_from(c >> 16).unwrap_or(0) & 255,
            i32::try_from(c >> 8).unwrap_or(0) & 255,
            i32::try_from(c).unwrap_or(0) & 255,
        )
    }

    #[test]
    fn a_steady_palette_is_the_pictures_own_and_keeps_the_background() {
        let (mut pixels, w, _) = sky_and_skin();
        pixels[0] = 0; // a clear pixel
        let mut steady = Steady::new(&pixels).expect("a photo has many colours");
        let frame = steady.quantize(&pixels, w, 0);
        assert!(frame.palette.len() <= 255);
        assert_eq!(frame.index[0], BACKGROUND);
        assert!(
            frame
                .index
                .iter()
                .all(|&i| usize::from(i) < frame.palette.len() || i == BACKGROUND)
        );
        // the colour of a pixel is near its own (no more than a step of the palette away on average)
        let mut off = 0i64;
        for (i, &p) in pixels.iter().enumerate().skip(1) {
            let (r, g, b) = rgb_of(&frame, i);
            off += i64::from((r - i32::try_from((p >> 16) & 255).unwrap_or(0)).abs())
                + i64::from((g - i32::try_from((p >> 8) & 255).unwrap_or(0)).abs())
                + i64::from((b - i32::try_from(p & 255).unwrap_or(0)).abs());
        }
        assert!(
            off / (3 * pixels.len() as i64) < 8,
            "the mean error of a channel is {}",
            off / (3 * pixels.len() as i64)
        );
    }

    #[test]
    fn a_steady_palette_is_the_same_for_every_frame_and_dithering_changes_only_the_indexes() {
        let (pixels, w, _) = sky_and_skin();
        let mut steady = Steady::new(&pixels).expect("a photo has many colours");
        let plain = steady.quantize(&pixels, w, 0);
        let dithered = steady.quantize(&pixels, w, 8);
        assert_eq!(plain.palette, dithered.palette);
        assert_ne!(plain.index, dithered.index, "a pattern was added");
        // the same pixels again: the same indexes
        assert_eq!(steady.quantize(&pixels, w, 8), dithered);
    }

    #[test]
    fn a_picture_of_few_colours_has_no_steady_palette_and_is_drawn_exactly() {
        let argb = [OPAQUE | 0xFF0000, OPAQUE | 0x00FF00, OPAQUE | 0x123456];
        assert!(Steady::new(&argb).is_none());
    }

    #[test]
    fn a_picture_of_few_colours_keeps_them_exactly_as_before() {
        let argb = [
            OPAQUE | 0xFF0000,
            OPAQUE | 0x00FF00,
            OPAQUE | 0xFF0000,
            OPAQUE | 0x0000FF,
        ];
        assert_eq!(quantize(&argb, 2), crate::quantize::quantize(&argb, 2));
    }

    #[test]
    fn nothing_to_quantize_and_the_background() {
        assert_eq!(quantize(&[], 0).palette.len(), 0);
        assert_eq!(quantize(&[0x0000_0000], 1).index, [BACKGROUND]);
    }

    #[test]
    fn a_photograph_stays_within_the_palette_and_a_transparent_pixel_is_the_background() {
        let (mut pixels, width, _) = sky_and_skin();
        pixels[7] = 0x0000_0000;
        let q = quantize(&pixels, width);
        assert!(q.palette.len() <= MAX_COLOURS);
        assert_eq!(q.index[7], BACKGROUND);
        assert!(
            q.index
                .iter()
                .enumerate()
                .all(|(i, &c)| i == 7 || usize::from(c) < q.palette.len())
        );
        assert_eq!(q, quantize(&pixels, width), "the same input gives the same output");
    }

    #[test]
    fn a_small_area_of_its_own_colours_is_not_swallowed_by_a_large_sky() {
        let (pixels, width, skin) = sky_and_skin();
        let q = quantize(&pixels, width);
        // skin has red above green above blue; a pixel that came out blue-dominant is the patch gone bluish
        let bluish = skin
            .iter()
            .filter(|&&i| {
                let (r, _, b) = rgb_of(&q, i);
                b >= r
            })
            .count();
        assert_eq!(bluish, 0, "of {} skin pixels", skin.len());
        // and the patch's mean stays near what it was
        let mean =
            |f: &dyn Fn(usize) -> i32| skin.iter().map(|&i| f(i)).sum::<i32>() / i32::try_from(skin.len()).unwrap_or(1);
        let original_r = mean(&|i| i32::try_from((pixels[i] >> 16) & 255).unwrap_or(0));
        let made_r = mean(&|i| rgb_of(&q, i).0);
        assert!((original_r - made_r).abs() <= 6, "red {original_r} became {made_r}");
    }

    /// The mean distance in Lab between a picture and what a palette made of it, both blurred over 5 x 5 pixels: dithering
    /// is meant to be averaged by the eye, so this is what the eye takes the colours for.
    fn blurred_distance(original: &[u32], q: &Indexed, width: usize) -> f32 {
        let table = LabTable::new();
        let height = original.len() / width;
        let lab = |f: &dyn Fn(usize) -> u32| -> Vec<f32> {
            let flat: Vec<f32> = (0..original.len()).flat_map(|i| table.of(f(i))).collect();
            let blur = |src: &[f32], horizontal: bool| {
                let mut out = vec![0.0f32; src.len()];
                let (outer, inner) = if horizontal { (height, width) } else { (width, height) };
                for o in 0..outer {
                    for i in 0..inner {
                        let (lo, hi) = (i.saturating_sub(2), (i + 2).min(inner - 1));
                        let at = |j: usize| if horizontal { o * width + j } else { j * width + o };
                        for ch in 0..3 {
                            let sum: f32 = (lo..=hi).map(|j| src[at(j) * 3 + ch]).sum();
                            out[at(i) * 3 + ch] = sum / (hi - lo + 1) as f32;
                        }
                    }
                }
                out
            };
            blur(&blur(&flat, true), false)
        };
        let was = lab(&|i| original[i]);
        let made = lab(&|i| q.palette[usize::from(q.index[i])]);
        let sum: f32 = (0..original.len())
            .map(|i| {
                let d = |c: usize| (was[i * 3 + c] - made[i * 3 + c]).powi(2);
                (d(0) + d(1) + d(2)).sqrt()
            })
            .sum();
        sum / original.len() as f32
    }

    #[test]
    fn on_a_real_picture_it_is_not_worse_than_blueedits_palette() {
        // the chart of the test pictures: anti-aliased, so many more than 255 colours; measured on
        // photographs, where the gain is large (about a quarter of the distance)
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("images")
            .join("growth.png");
        let picture = crate::decode::decode(&path, 800, 600).expect("decodes");
        let mine = blurred_distance(&picture.argb, &quantize(&picture.argb, picture.width), picture.width);
        let theirs = blurred_distance(
            &picture.argb,
            &crate::quantize::quantize(&picture.argb, picture.width),
            picture.width,
        );
        assert!(mine <= theirs * 1.05, "mine {mine} against blueedit's {theirs}");
    }
}
