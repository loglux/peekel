//! PEEKEL: a picture shrunk to at most half of its size by an area average, in 16-bit numbers.
//!
//! On 32-bit ARM `pic-scale` has no SIMD (its NEON is for `aarch64` only, as Rust's NEON intrinsics for 32-bit ARM are not
//! stable), and shrinking a magazine page's picture of 2172 x 3072 to 1200 took 275 ms on a 32-bit tablet. This takes 116 ms
//! there in plain loops (the compiler makes NEON of the pass down the rows by itself) and 52 ms with the pass across the row in
//! NEON written by hand (`nightly-neon`); both give the same bytes. Against `pic-scale`'s Hermite the mean difference is under
//! half a level of 255. Measured 27 November 2026 (vendor/README.md).
//!
//! Each output sample is the average of the input samples it covers, weighted by how much of each it covers: at most three per
//! axis while the picture shrinks to no less than half. The rows are averaged first, rounded to bytes, then across.

/// The bits of a weight: the weights of an output sample add up to `1 << BITS`, so a byte times a weight times three fits u16.
const BITS: u32 = 7;
/// The input samples an output sample is made of.
const TAPS: usize = 3;

/// An RGB picture of `width` x `height` (3 bytes a pixel, no padding) shrunk to `new_width` x `new_height`; none when it is not
/// one this can do (larger, or shrunk below half, or smaller than three pixels, or its data not of its size), for the caller to
/// do it another way.
pub fn rgb(
    data: &[u8],
    width: usize,
    height: usize,
    new_width: usize,
    new_height: usize,
) -> Option<Vec<u8>> {
    let fits = |from: usize, to: usize| from >= TAPS && to >= 1 && to <= from && from <= to * 2;
    if !fits(width, new_width) || !fits(height, new_height) || data.len() != width.checked_mul(height)?.checked_mul(3)? {
        return None;
    }
    let (rows, cols) = (taps(height, new_height), taps(width, new_width));
    let line = width * 3;
    let mut mid = vec![0_u8; line * new_height];
    for ((start, weights), out) in rows.iter().zip(mid.chunks_exact_mut(line)) {
        let row = |t: usize| &data[(start + t) * line..][..line];
        down(out, [row(0), row(1), row(2)], *weights);
    }
    let mut out = vec![0_u8; new_width * new_height * 3];
    let across = Across::new(&cols, width);
    for (row, out) in mid.chunks_exact(line).zip(out.chunks_exact_mut(new_width * 3)) {
        across.run(row, out);
    }
    Some(out)
}

/// For each of `to` output samples made from `from` input ones: its first input and the weights of it and the next two, adding
/// up to `1 << BITS`. `from` is at least `TAPS` and at most twice `to`.
fn taps(from: usize, to: usize) -> Vec<(usize, [u8; TAPS])> {
    let step = from as f64 / to as f64;
    let whole = f64::from(1_u32 << BITS);
    (0..to)
        .map(|o| {
            let (a, b) = (o as f64 * step, (o as f64 + 1.0) * step);
            let start = (a as usize).min(from - TAPS);
            let mut weights = [0_u8; TAPS];
            for (t, weight) in weights.iter_mut().enumerate() {
                let i = (start + t) as f64;
                let cover = (b.min(i + 1.0) - a.max(i)).max(0.0) / step;
                *weight = (cover * whole).round() as u8;
            }
            // the rounding may leave the sum a little off: the largest weight takes the difference
            let sum: i32 = weights.iter().map(|&w| i32::from(w)).sum();
            let largest = (0..TAPS).max_by_key(|&t| weights[t]).unwrap_or(0);
            weights[largest] = (i32::from(weights[largest]) + (1 << BITS) - sum).clamp(0, 255) as u8;
            (start, weights)
        })
        .collect()
}

#[inline(always)]
fn blend(samples: [u8; TAPS], weights: [u8; TAPS]) -> u8 {
    let sum = u16::from(samples[0]) * u16::from(weights[0])
        + u16::from(samples[1]) * u16::from(weights[1])
        + u16::from(samples[2]) * u16::from(weights[2]);
    ((sum + (1 << (BITS - 1))) >> BITS) as u8
}

/// One output row from three input rows. Written so that the compiler makes NEON of it by itself (as fast as by hand, measured).
fn down(out: &mut [u8], rows: [&[u8]; TAPS], weights: [u8; TAPS]) {
    for (((o, &a), &b), &c) in out.iter_mut().zip(rows[0]).zip(rows[1]).zip(rows[2]) {
        *o = blend([a, b, c], weights);
    }
}

/// The pass across a row: each output pixel from three input pixels.
struct Across<'a> {
    cols: &'a [(usize, [u8; TAPS])],
    /// Groups of eight output pixels whose 24 input pixels lie together, for NEON; the pixels after them are done one by one.
    #[cfg_attr(not(all(feature = "nightly-neon", target_arch = "arm", target_feature = "neon")), allow(dead_code))]
    groups: Vec<Group>,
    one_by_one_from: usize,
}

/// Eight output pixels made from the 24 input pixels from `first`: for each tap, where in those 24 each output takes it and its
/// weight.
#[cfg_attr(not(all(feature = "nightly-neon", target_arch = "arm", target_feature = "neon")), allow(dead_code))]
struct Group {
    first: usize,
    at: [[u8; 8]; TAPS],
    weights: [[u8; 8]; TAPS],
}

impl<'a> Across<'a> {
    fn new(cols: &'a [(usize, [u8; TAPS])], width: usize) -> Self {
        let mut groups = Vec::new();
        let mut x = 0;
        if cfg!(all(feature = "nightly-neon", target_arch = "arm", target_feature = "neon")) {
            while let Some(eight) = cols.get(x..x + 8) {
                let first = eight[0].0;
                if first + 24 > width || eight.iter().any(|&(start, _)| start + TAPS > first + 24) {
                    break;
                }
                let mut group = Group { first, at: [[0; 8]; TAPS], weights: [[0; 8]; TAPS] };
                for (k, &(start, weights)) in eight.iter().enumerate() {
                    for t in 0..TAPS {
                        group.at[t][k] = (start + t - first) as u8;
                        group.weights[t][k] = weights[t];
                    }
                }
                groups.push(group);
                x += 8;
            }
        }
        Self { cols, groups, one_by_one_from: x }
    }

    fn run(&self, row: &[u8], out: &mut [u8]) {
        #[cfg(all(feature = "nightly-neon", target_arch = "arm", target_feature = "neon"))]
        neon::across(&self.groups, row, out);
        for (x, &(start, weights)) in self.cols.iter().enumerate().skip(self.one_by_one_from) {
            for c in 0..3 {
                let at = |t: usize| row[(start + t) * 3 + c];
                out[x * 3 + c] = blend([at(0), at(1), at(2)], weights);
            }
        }
    }
}

/// NEON by hand for the pass across, which the compiler does not vectorise (85 ms against 23 ms on the tablet). Rust's NEON
/// intrinsics for 32-bit ARM are not stable: this is built only with the `nightly-neon` feature and `RUSTC_BOOTSTRAP=1`.
#[cfg(all(feature = "nightly-neon", target_arch = "arm", target_feature = "neon"))]
#[allow(unsafe_code)]
mod neon {
    use super::{Group, TAPS};
    use core::arch::arm::*;

    /// Eight output pixels at a time: their 24 input pixels split by colour (`vld3`), each tap picked out with a table look-up
    /// (`vtbl3`), multiplied and added in u16, rounded back to bytes and joined again (`vst3`). The same sums and rounding as
    /// `blend`, so the same bytes.
    pub(super) fn across(groups: &[Group], row: &[u8], out: &mut [u8]) {
        for (g, group) in groups.iter().enumerate() {
            let Some(input) = row.get(group.first * 3..(group.first + 24) * 3) else { return };
            let Some(output) = out.get_mut(g * 24..(g + 1) * 24) else { return };
            // SAFETY: `input` is 72 bytes (three loads of 24), `output` 24 (one store of 24), the look-ups read the 8-byte
            // arrays of `group`; NEON is there, as the target is built with it (`target_feature = "neon"`).
            unsafe {
                let p = input.as_ptr();
                let (a, b, c) = (vld3_u8(p), vld3_u8(p.add(24)), vld3_u8(p.add(48)));
                let tables = [uint8x8x3_t(a.0, b.0, c.0), uint8x8x3_t(a.1, b.1, c.1), uint8x8x3_t(a.2, b.2, c.2)];
                let mut colours = [vdup_n_u8(0); 3];
                for (colour, table) in colours.iter_mut().zip(tables) {
                    let mut sum = vdupq_n_u16(0);
                    for t in 0..TAPS {
                        let picked = vtbl3_u8(table, vld1_u8(group.at[t].as_ptr()));
                        sum = vmlal_u8(sum, picked, vld1_u8(group.weights[t].as_ptr()));
                    }
                    *colour = vrshrn_n_u16::<7>(sum);
                }
                vst3_u8(output.as_mut_ptr(), uint8x8x3_t(colours[0], colours[1], colours[2]));
            }
        }
    }
}
