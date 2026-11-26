//! A picture made smaller (or bigger) the way DjVuLibre's pixmap scaler makes it, so that a page made small "as DjVuLibre does"
//! has its pixels (docs/internal/djvu.md, "A smaller page as DjVuLibre makes it"). Written from a reading of DjVuLibre's
//! `GScaler.cpp`; no code was carried over.
//!
//! Along each side on its own: while the picture is more than twice as big as the part of it that is wanted, it is first made half
//! as big by taking the mean of each two by two (or two, or one, at an edge) of its pixels; then each pixel of the result is a mix
//! of two by two pixels of that, the shares in sixteenths. Rows are counted from the bottom, as DjVu counts them.

use crate::iw44::Pixmap;

/// The places along one side.
struct Side {
    /// How many times the side was halved before the mixing.
    shift: u32,
    /// The size of the side after the halving.
    size: usize,
    /// For each pixel of the result, where its centre falls on the halved side, in sixteenths of a pixel (the centre of pixel 0
    /// being 0), not past the centre of the last pixel.
    places: Vec<i32>,
}

impl Side {
    /// A side of `input` pixels made `output` pixels, `numer` pixels of the result standing for `denom` pixels of the input.
    fn new(input: usize, output: usize, numer: usize, denom: usize) -> Self {
        let (mut shift, mut size, mut numer) = (0u32, input, numer as i64);
        let denom = denom as i64;
        while numer + numer < denom {
            shift += 1;
            size = size.div_ceil(2);
            numer <<= 1;
        }
        // the centres stepped along in whole sixteenths, the remainders carried (the first is the centre of the first pixel)
        let length = denom * 16;
        let mut place = (length + numer) / (2 * numer) - 8;
        let mut carried = numer / 2;
        let limit = (size as i64 - 1) * 16;
        let places = (0..output)
            .map(|_| {
                let at = place.min(limit) as i32;
                carried += length;
                place += carried / numer;
                carried %= numer;
                at
            })
            .collect();
        Self { shift, size, places }
    }

    /// The pixel `n` of the halved side, kept on it.
    fn clamp(&self, n: i32) -> usize {
        n.clamp(0, self.size as i32 - 1) as usize
    }
}

/// One mix of two samples: `share` sixteenths of the way from `a` to `b`, to the nearest, a half going up.
fn mix(a: u8, b: u8, share: i32) -> u8 {
    let (a, d) = (i32::from(a), i32::from(b) - i32::from(a));
    (a + ((share * d + 8) >> 4)) as u8
}

/// `input` made `out_w` x `out_h`, `numer_x` pixels of the result standing for `denom_x` of the input across and `numer_y` for
/// `denom_y` down. None when a size is 0.
pub(crate) fn scaled(
    input: &Pixmap,
    out_w: usize,
    out_h: usize,
    (numer_x, denom_x): (usize, usize),
    (numer_y, denom_y): (usize, usize),
) -> Option<Pixmap> {
    if input.width == 0 || input.height == 0 || out_w == 0 || out_h == 0 || numer_x == 0 || numer_y == 0 {
        return None;
    }
    let across = Side::new(input.width, out_w, numer_x, denom_x);
    let down = Side::new(input.height, out_h, numer_y, denom_y);
    // a row of the halved picture, counted from the bottom; the last two are kept, as the rows of the result go up in order
    let halved_row = |row: usize| -> Vec<u8> {
        let mut line = vec![0u8; across.size * 3];
        if across.shift == 0 && down.shift == 0 {
            let top_down = input.height - 1 - row;
            line.copy_from_slice(&input.rgb[top_down * input.width * 3..][..input.width * 3]);
            return line;
        }
        let (rows_from, rows_to) = (row << down.shift, ((row + 1) << down.shift).min(input.height));
        let full = 1u32 << (across.shift + down.shift);
        let div = across.shift + down.shift;
        for (column, out) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let (from, to) = (column << across.shift, ((column + 1) << across.shift).min(input.width));
            let (mut sums, mut count) = ([0u32; 3], 0u32);
            for bottom_up in rows_from..rows_to {
                let top_down = input.height - 1 - bottom_up;
                for pixel in input.rgb[(top_down * input.width + from) * 3..(top_down * input.width + to) * 3]
                    .as_chunks::<3>()
                    .0
                {
                    for (sum, &value) in sums.iter_mut().zip(pixel) {
                        *sum += u32::from(value);
                    }
                    count += 1;
                }
            }
            for (value, sum) in out.iter_mut().zip(sums) {
                *value = if count == full {
                    ((sum + (full >> 1)) >> div) as u8
                } else {
                    ((sum + count / 2) / count.max(1)) as u8
                };
            }
        }
        line
    };
    let mut kept: [(usize, Vec<u8>); 2] = [(usize::MAX, Vec::new()), (usize::MAX, Vec::new())];
    let mut row_of = |row: usize| -> Vec<u8> {
        if let Some((_, line)) = kept.iter().find(|(at, _)| *at == row) {
            return line.clone();
        }
        let line = halved_row(row);
        kept.swap(0, 1);
        kept[1] = (row, line.clone());
        line
    };
    let mut rgb = vec![0u8; out_w * out_h * 3];
    let mut mixed = vec![0u8; across.size * 3];
    for (y, &place) in down.places.iter().enumerate() {
        // the two rows of the halved picture, the lower first, mixed by the share of the place
        let lower_row = down.clamp(place >> 4);
        let upper_row = down.clamp((place >> 4) + 1);
        let (lower, upper) = (row_of(lower_row), row_of(upper_row));
        for ((m, &l), &u) in mixed.iter_mut().zip(&lower).zip(&upper) {
            *m = mix(l, u, place & 15);
        }
        // then along the row, the two columns
        let top_down = out_h - 1 - y;
        let out = &mut rgb[top_down * out_w * 3..][..out_w * 3];
        for (pixel, &place) in out.as_chunks_mut::<3>().0.iter_mut().zip(&across.places) {
            let (left, right) = (across.clamp(place >> 4), across.clamp((place >> 4) + 1));
            for (c, value) in pixel.iter_mut().enumerate() {
                *value = mix(mixed[left * 3 + c], mixed[right * 3 + c], place & 15);
            }
        }
    }
    Some(Pixmap {
        width: out_w,
        height: out_h,
        rgb,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn grey(width: usize, height: usize, value: impl Fn(usize, usize) -> u8) -> Pixmap {
        let mut rgb = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                rgb.extend([value(x, y); 3]);
            }
        }
        Pixmap { width, height, rgb }
    }

    #[test]
    fn a_flat_picture_stays_flat_and_the_size_is_the_one_asked() {
        let flat = grey(37, 23, |_, _| 90);
        for (w, h) in [(37, 23), (12, 8), (5, 3), (1, 1), (60, 40)] {
            let out = scaled(&flat, w, h, (w, 37), (h, 23)).unwrap();
            assert_eq!((out.width, out.height), (w, h));
            assert!(out.rgb.iter().all(|&v| v == 90), "{w} x {h}");
        }
    }

    #[test]
    fn the_same_size_is_a_copy_and_a_half_is_the_mean_of_pairs() {
        let picture = grey(8, 6, |x, y| (x * 30 + y * 7) as u8);
        assert_eq!(scaled(&picture, 8, 6, (8, 8), (6, 6)).unwrap(), picture);
        // at exactly a half the share is eight sixteenths: the mean of two neighbours, a half going up
        let line = grey(8, 1, |x, _| [0, 10, 20, 31, 40, 50, 60, 71][x]);
        let half = scaled(&line, 4, 1, (4, 8), (1, 1)).unwrap();
        let values: Vec<u8> = half.rgb.iter().step_by(3).copied().collect();
        assert_eq!(values, [5, 26, 45, 66]);
    }

    #[test]
    fn more_than_twice_smaller_takes_the_mean_of_blocks_first() {
        // a picture of black and white columns made four times narrower: the blocks of two are grey before the mixing
        let stripes = grey(16, 2, |x, _| if x % 2 == 0 { 0 } else { 255 });
        let out = scaled(&stripes, 4, 2, (4, 16), (2, 2)).unwrap();
        assert!(out.rgb.iter().all(|&v| v == 128), "{:?}", out.rgb);
    }

    #[test]
    fn sizes_of_nothing_give_nothing() {
        let picture = grey(4, 4, |_, _| 1);
        assert!(scaled(&picture, 0, 3, (0, 4), (3, 4)).is_none());
        assert!(scaled(&grey(0, 0, |_, _| 0), 3, 3, (3, 4), (3, 4)).is_none());
    }
}
