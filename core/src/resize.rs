//! A picture made smaller or larger with the triangle (bilinear) filter: the same arithmetic as `image::imageops::resize` with
//! `FilterType::Triangle` (the same weights, in `f32`, summed in the same order, the same rounding), so the pixels come out the same;
//! but each pass goes along the rows of memory and on several threads. `image`'s second pass goes down the columns on one thread: a
//! photograph made to fit a window of 1770 x 1180 took about 100 ms there (`docs/internal/reading-formats.md`).

use image::RgbaImage;

/// Below this many output pixels the threads cost more than they save.
const PARALLEL_FROM: usize = 1 << 16;

/// For each output pixel along one side, the first input pixel it is made from and the weights of it and of those after it.
struct Taps {
    first: Vec<usize>,
    /// Where each output pixel's weights begin in `weights`; the last entry is the end.
    starts: Vec<usize>,
    weights: Vec<f32>,
}

impl Taps {
    fn new(from: u32, to: u32) -> Self {
        let ratio = from as f32 / to as f32;
        let sratio = if ratio < 1.0 { 1.0 } else { ratio };
        let support = sratio; // the triangle reaches 1 pixel either side, widened when shrinking
        let mut taps = Taps {
            first: Vec::with_capacity(to as usize),
            starts: Vec::with_capacity(to as usize + 1),
            weights: Vec::new(),
        };
        for out in 0..to {
            let centre = (out as f32 + 0.5) * ratio;
            let left = ((centre - support).floor() as i64).clamp(0, i64::from(from) - 1);
            let right = ((centre + support).ceil() as i64).clamp(left + 1, i64::from(from));
            let centre = centre - 0.5;
            let start = taps.weights.len();
            taps.starts.push(start);
            let mut sum = 0.0;
            for i in left..right {
                let x = (i as f32 - centre) / sratio;
                let w = if x.abs() < 1.0 { 1.0 - x.abs() } else { 0.0 };
                taps.weights.push(w);
                sum += w;
            }
            for w in &mut taps.weights[start..] {
                *w /= sum;
            }
            taps.first.push(left as usize);
        }
        taps.starts.push(taps.weights.len());
        taps
    }

    fn of(&self, out: usize) -> (usize, &[f32]) {
        (self.first[out], &self.weights[self.starts[out]..self.starts[out + 1]])
    }
}

/// How many threads for `pixels` of output.
fn threads_for(pixels: usize) -> usize {
    if pixels < PARALLEL_FROM {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
    }
}

/// Runs `fill(first_row, rows)` over `out` cut into bands of whole rows of `row_len` items, on several threads.
fn by_bands<T: Send>(out: &mut [T], row_len: usize, fill: impl Fn(usize, &mut [T]) + Sync) {
    let rows = out.len().checked_div(row_len).unwrap_or(0);
    if rows == 0 {
        return;
    }
    let per = rows.div_ceil(threads_for(out.len() / 4));
    let fill = &fill;
    std::thread::scope(|scope| {
        for (n, band) in out.chunks_mut(per * row_len).enumerate() {
            scope.spawn(move || fill(n * per, band));
        }
    });
}

/// `src` made `width` x `height` with the triangle filter.
pub fn triangle(src: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 || width == 0 || height == 0 {
        return RgbaImage::new(width, height);
    }
    if (width, height) == (sw, sh) {
        return src.clone();
    }
    let (sw, w, h) = (sw as usize, width as usize, height as usize);
    let raw = src.as_raw();

    // down the columns first, into numbers that are not rounded yet: `sw` x `h`
    let rows = Taps::new(sh, height);
    let mut between = vec![0f32; sw * h * 4];
    by_bands(&mut between, sw * 4, |first, band| {
        for (r, row) in band.chunks_mut(sw * 4).enumerate() {
            let (top, weights) = rows.of(first + r);
            for (k, &weight) in weights.iter().enumerate() {
                let line = &raw[(top + k) * sw * 4..(top + k + 1) * sw * 4];
                for (t, &v) in row.iter_mut().zip(line) {
                    *t += f32::from(v) * weight;
                }
            }
        }
    });

    // then along the rows
    let columns = Taps::new(sw as u32, width);
    let mut out = vec![0u8; w * h * 4];
    by_bands(&mut out, w * 4, |first, band| {
        for (r, row) in band.chunks_mut(w * 4).enumerate() {
            let line = &between[(first + r) * sw * 4..(first + r + 1) * sw * 4];
            for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (left, weights) = columns.of(x);
                let mut t = [0f32; 4];
                for (k, &weight) in weights.iter().enumerate() {
                    let at = (left + k) * 4;
                    for c in 0..4 {
                        t[c] += line[at + c] * weight;
                    }
                }
                for c in 0..4 {
                    pixel[c] = t[c].clamp(0.0, 255.0).round() as u8;
                }
            }
        }
    });
    RgbaImage::from_raw(width, height, out).unwrap_or_else(|| RgbaImage::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use image::imageops::{self, FilterType};

    fn noise(width: u32, height: u32) -> RgbaImage {
        let mut seed = 12345u32;
        RgbaImage::from_fn(width, height, |_, _| {
            let mut next = || {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as u8
            };
            Rgba([next(), next(), next(), next()])
        })
    }

    #[test]
    fn the_pixels_are_those_of_image_s_own_triangle_filter() {
        let src = noise(613, 411);
        for (w, h) in [
            (300, 200),
            (612, 410),
            (97, 389),
            (1000, 700),
            (613, 100),
            (1, 1),
            (2000, 1500),
        ] {
            let ours = triangle(&src, w, h);
            let theirs = imageops::resize(&src, w, h, FilterType::Triangle);
            assert_eq!(ours.dimensions(), theirs.dimensions());
            let differ = ours.pixels().zip(theirs.pixels()).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "{w} x {h}: {differ} pixels differ");
        }
    }

    #[test]
    fn an_empty_picture_or_size_gives_an_empty_picture() {
        assert_eq!(triangle(&RgbaImage::new(0, 0), 10, 10).dimensions(), (10, 10));
        assert_eq!(triangle(&noise(5, 5), 0, 3).dimensions(), (0, 3));
    }
}
