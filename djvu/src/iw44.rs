//! IW44, the wavelet picture of DjVu (spec section 10): the background and foreground of a page, and the pictures of a photo page.
//!
//! A picture is one or three planes (luminance, and blue and red chrominance), each cut into blocks of 32 x 32 wavelet
//! coefficients. The chunks (`BG44`, `FG44`, `PM44`, `BM44`) carry slices; a slice refines one band of coefficients of every
//! block, by a finer step each time round. The planes are rebuilt by the inverse wavelet transform and put together to RGB.
//!
//! Written from the spec. Where the spec is not enough (the values of the steps, the refinement, the edges of the transform) the
//! differences found against the streams of another encoder and the pictures of DjVuLibre's `ddjvu` are in docs/internal/djvu.md.

use crate::Error;
use crate::zp::ZpDecoder;

/// The largest picture read, in pixels. A scanned page at 600 dpi is about 62 million; a background is a third of that in each
/// direction.
pub const MAX_PIXELS: usize = 64 * 1024 * 1024;

/// The most slices that one picture may spend: real encoders write about a hundred, and every slice costs a pass over all blocks.
const MAX_SLICES: usize = 512;

/// The bands: the first bucket of each, and one past the last. A bucket is 16 coefficients.
const BAND_FIRST_BUCKET: [usize; 11] = [0, 1, 2, 3, 4, 8, 12, 16, 32, 48, 64];

/// The first steps of the 16 coefficients of band 0, and of bands 1 to 9 (index 0 is not used). Values are in 1/64 of a grey level,
/// scaled by 2^10. Every slice halves the step of its band.
const STEP_BAND0: [u32; 16] = [
    0x4000, 0x8000, 0x8000, 0x10000, 0x10000, 0x10000, 0x10000, 0x10000, 0x10000, 0x10000, 0x10000, 0x10000, 0x20000,
    0x20000, 0x20000, 0x20000,
];
const STEP_BANDS: [u32; 10] = [
    0, 0x20000, 0x20000, 0x40000, 0x40000, 0x40000, 0x80000, 0x40000, 0x40000, 0x80000,
];

/// A step this big or bigger (or zero) is not used: the slice has nothing for that band yet (or any more).
const STEP_LIMIT: u32 = 0x8000;

// the flags of a bucket and of a band of a block (the OR of what their coefficients are)
const ACTIVE: u8 = 2; // not zero: it is refined
const NEW: u8 = 4; // becomes active in this slice
const UNK: u8 = 8; // zero, and may become active

/// A picture, three bytes (red, green, blue) for each pixel, the top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixmap {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

/// One plane of coefficients and the state of its decoder.
#[derive(Debug, Clone)]
struct Plane {
    block_cols: usize,
    block_rows: usize,
    /// 1024 coefficients for each block, in the order of the spec (the index is the number of a coefficient in its block).
    coefs: Vec<i16>,
    /// For each bucket of each block (64 to a block) which of its 16 coefficients are not zero, one bit each.
    masks: Vec<u16>,
    step_band0: [u32; 16],
    step_bands: [u32; 10],
    /// The band that the next slice is for.
    band: usize,
    ctx_bucket: u8,
    ctx_coef: [u8; 80],
    ctx_activate: [u8; 16],
    ctx_increase: u8,
}

impl Plane {
    fn new(width: usize, height: usize) -> Self {
        let block_cols = width.div_ceil(32);
        let block_rows = height.div_ceil(32);
        Self {
            block_cols,
            block_rows,
            coefs: vec![0; block_cols * block_rows * 1024],
            masks: vec![0; block_cols * block_rows * 64],
            step_band0: STEP_BAND0,
            step_bands: STEP_BANDS,
            band: 0,
            ctx_bucket: 0,
            ctx_coef: [0; 80],
            ctx_activate: [0; 16],
            ctx_increase: 0,
        }
    }

    /// One slice: the current band of every block, then the step of the band is halved and the next band is the current one.
    fn slice(&mut self, zp: &mut ZpDecoder<'_>) {
        let band = self.band;
        // the coefficients whose step can be used: for band 0 those whose own step is in range, for the others all or none
        let usable: u16 = if band == 0 {
            self.step_band0
                .iter()
                .enumerate()
                .filter(|&(_, &step)| step > 0 && step < STEP_LIMIT)
                .fold(0, |mask, (j, _)| mask | 1 << j)
        } else {
            let step = self.step_bands[band];
            if step > 0 && step < STEP_LIMIT { 0xFFFF } else { 0 }
        };
        if usable != 0 {
            let blocks = self.block_cols * self.block_rows;
            for block in 0..blocks {
                self.block(zp, block, band, usable);
            }
        }
        // the end of the slice
        self.step_bands[band] >>= 1;
        if band == 0 {
            for step in &mut self.step_band0 {
                *step >>= 1;
            }
        }
        self.band = if band == 9 { 0 } else { band + 1 };
    }

    /// The four passes of one band of one block (spec 10.5.3 to 10.5.6). `usable` has a bit for each coefficient of a bucket whose
    /// step can be used. Which coefficients are not zero is kept as a mask of 16 bits for each bucket, so that nothing has to be
    /// looked at to know it.
    fn block(&mut self, zp: &mut ZpDecoder<'_>, block: usize, band: usize, usable: u16) {
        let first = BAND_FIRST_BUCKET[band];
        let buckets = BAND_FIRST_BUCKET[band + 1] - first;
        let coefs = &mut self.coefs[block * 1024..(block + 1) * 1024];
        let masks = &mut self.masks[block * 64..(block + 1) * 64];

        // the state at the start of the slice: the coefficients that are not zero are active, the zero ones may become active
        let mut active = [0u16; 16];
        let mut unknown = [0u16; 16];
        let mut bucket_flags = [0u8; 16];
        let mut band_flags = 0u8;
        for b in 0..buckets {
            let mask = masks[first + b];
            active[b] = mask & usable;
            unknown[b] = !mask & usable;
            let or = if active[b] != 0 { ACTIVE } else { 0 } | if unknown[b] != 0 { UNK } else { 0 };
            bucket_flags[b] = or;
            band_flags |= or;
        }

        // pass 1: does the band of this block hold anything new? A band of one to four buckets always does; a band of sixteen
        // buckets says so, if it has nothing active and something that may become
        let mut new = buckets < 16 || band_flags & ACTIVE != 0;
        if !new && band_flags & UNK != 0 {
            new = zp.decode_bit(&mut self.ctx_bucket);
        }

        if new {
            // pass 2: which buckets hold something new
            let mut any = false;
            for (b, flag) in bucket_flags.iter_mut().enumerate().take(buckets) {
                if *flag & UNK == 0 {
                    continue;
                }
                let mut n = 0usize;
                if band != 0 {
                    // how many of the four coefficients of the band below that are above this bucket are not zero
                    let below = first + b;
                    n = ((masks[below / 4] >> (4 * (below % 4))) & 0xF).count_ones() as usize;
                    if n == 4 {
                        n = 3;
                    }
                }
                if band_flags & ACTIVE != 0 {
                    n |= 4;
                }
                if zp.decode_bit(&mut self.ctx_coef[band * 8 + n]) {
                    *flag |= NEW;
                    any = true;
                }
            }

            // pass 3: which coefficients of those buckets become active
            if any {
                for b in 0..buckets {
                    if bucket_flags[b] & NEW == 0 {
                        continue;
                    }
                    let cells = &mut coefs[(first + b) * 16..(first + b + 1) * 16];
                    let base = if bucket_flags[b] & ACTIVE != 0 { 8 } else { 0 };
                    let mut left = unknown[b].count_ones() as usize;
                    for (j, cell) in cells.iter_mut().enumerate() {
                        if unknown[b] >> j & 1 == 0 {
                            continue;
                        }
                        if zp.decode_bit(&mut self.ctx_activate[base + left.min(7)]) {
                            let negative = zp.decode_pass();
                            left = 0;
                            let step = if band == 0 {
                                self.step_band0[j]
                            } else {
                                self.step_bands[band]
                            } as i32;
                            let value = step + (step >> 1) - (step >> 3);
                            *cell = (if negative { -value } else { value }) as i16;
                            masks[first + b] |= 1 << j;
                        }
                        left = left.saturating_sub(1);
                    }
                }
            }
        }

        // pass 4: the coefficients that were active before this slice get one more bit
        if band_flags & ACTIVE != 0 {
            for b in 0..buckets {
                let cells = &mut coefs[(first + b) * 16..(first + b + 1) * 16];
                let mut todo = active[b];
                while todo != 0 {
                    let j = todo.trailing_zeros() as usize;
                    todo &= todo - 1;
                    let cell = &mut cells[j];
                    let step = (if band == 0 {
                        self.step_band0[j]
                    } else {
                        self.step_bands[band]
                    }) as i32;
                    let mut magnitude = i32::from(*cell).abs();
                    let up = if magnitude <= 3 * step {
                        let bit = zp.decode_bit(&mut self.ctx_increase);
                        magnitude += step >> 2;
                        bit
                    } else {
                        zp.decode_pass()
                    };
                    if up {
                        magnitude += step >> 1;
                    } else {
                        magnitude += -step + (step >> 1);
                    }
                    *cell = (if *cell < 0 { -magnitude } else { magnitude }) as i16;
                    if *cell == 0 {
                        masks[first + b] &= !(1 << j);
                    }
                }
            }
        }
    }

    /// The plane as grey levels with 6 fractional bits, rows from the bottom, `block_cols * 32` wide: the coefficients put in
    /// place and the inverse wavelet transform done.
    fn reconstruct(&self, width: usize, height: usize) -> (Vec<i16>, usize) {
        let stride = self.block_cols * 32;
        let rows = self.block_rows * 32;
        let mut plane = vec![0i16; stride * rows];
        for block_row in 0..self.block_rows {
            for block_col in 0..self.block_cols {
                let block = &self.coefs[(block_row * self.block_cols + block_col) * 1024..][..1024];
                if block.iter().all(|&c| c == 0) {
                    continue;
                }
                let origin = block_row * 32 * stride + block_col * 32;
                for (&c, &place) in block.iter().zip(PLACE.iter()) {
                    plane[origin + usize::from(place >> 5) * stride + usize::from(place & 31)] = c;
                }
            }
        }
        inverse_wavelet(&mut plane, stride, width, height);
        (plane, stride)
    }
}

/// Where the coefficient `i` of a block goes in its 32 x 32 tile, as `row * 32 + column`: the bits of the index, the odd ones for the
/// row and the even ones for the column, the highest first (spec 10.3).
const PLACE: [u16; 1024] = {
    let mut table = [0u16; 1024];
    let mut i = 0;
    while i < 1024 {
        let row =
            ((i >> 1) & 1) << 4 | ((i >> 3) & 1) << 3 | ((i >> 5) & 1) << 2 | ((i >> 7) & 1) << 1 | ((i >> 9) & 1);
        let col = (i & 1) << 4 | ((i >> 2) & 1) << 3 | ((i >> 4) & 1) << 2 | ((i >> 6) & 1) << 1 | ((i >> 8) & 1);
        table[i] = (row * 32 + col) as u16;
        i += 1;
    }
    table
};

/// A sample is kept in 16 bits after every step, as DjVuLibre keeps it: a value that does not fit wraps round.
fn wrap(v: i32) -> i32 {
    i32::from(v as i16)
}

/// One line of the inverse transform (spec 10.6), the even samples in `even` and the odd ones in `odd` (the sample `k` of the line
/// is `even[k / 2]` or `odd[k / 2]`): first every even sample is lifted by the odd ones near it, then every odd sample is predicted
/// from the even ones near it. The samples that are not there count as 0 in the first, and the ends have their own forms. Away from
/// the ends the steps are plain sums over shifted slices, which the compiler does several at a time.
fn filter_halves(even: &mut [i32], odd: &mut [i32], along_rows: bool) {
    let (ne, no) = (even.len(), odd.len());
    if ne == 0 {
        return;
    }
    let last = ne + no - 1;
    let at = |even: &[i32], odd: &[i32], i: usize| if i.is_multiple_of(2) { even[i / 2] } else { odd[i / 2] };
    // the first step: for the sample k = 2j the neighbours are odd[j - 1], odd[j], odd[j - 2] and odd[j + 1]
    let lift_at_end = |even: &mut [i32], odd: &[i32], j: usize| {
        let k = 2 * j;
        let p1 = if k >= 1 { odd[j - 1] } else { 0 };
        let p3 = if k >= 3 { odd[j - 2] } else { 0 };
        // DjVuLibre keeps the sample it had for k + 1 when k + 3 is past the end, for the third and the fifth sample, along the
        // rows only (down the columns it takes 0, as the spec says); the last sample has that for its own k + 1
        let n1 = if k < last {
            odd[j]
        } else if along_rows && (k == 4 || k == 6) {
            odd[j - 1]
        } else {
            0
        };
        let n3 = if k + 3 <= last {
            odd[j + 1]
        } else if along_rows && (k == 2 || k == 4) {
            n1
        } else {
            0
        };
        even[j] = wrap(even[j] - ((9 * (p1 + n1) - (p3 + n3) + 16) >> 5));
    };
    let (from, to) = if last >= 7 { (2, (last - 3) / 2 + 1) } else { (ne, ne) };
    for j in 0..from {
        lift_at_end(even, odd, j);
    }
    if from < to {
        let n = to - from;
        let (b3, b1, f1, f3) = (
            &odd[from - 2..][..n],
            &odd[from - 1..][..n],
            &odd[from..][..n],
            &odd[from + 1..][..n],
        );
        for ((((e, &a), &b), &c), &d) in even[from..to].iter_mut().zip(b3).zip(b1).zip(f1).zip(f3) {
            *e = wrap(*e - ((9 * (b + c) - (a + d) + 16) >> 5));
        }
    }
    for j in to..ne {
        lift_at_end(even, odd, j);
    }
    // the second step: for the sample k = 2j + 1 the neighbours are even[j], even[j + 1], even[j - 1] and even[j + 2]
    let predict_at_end = |even: &[i32], odd: &mut [i32], j: usize| {
        let k = 2 * j + 1;
        let before = at(even, odd, k - 1);
        odd[j] = if k < last {
            wrap(odd[j] + ((before + even[j + 1] + 1) >> 1))
        } else {
            wrap(odd[j] + before)
        };
    };
    let (from, to) = if last >= 6 { (1, (last - 4) / 2 + 1) } else { (no, no) };
    for j in 0..from {
        predict_at_end(even, odd, j);
    }
    if from < to {
        let n = to - from;
        let (b3, b1, f1, f3) = (
            &even[from - 1..][..n],
            &even[from..][..n],
            &even[from + 1..][..n],
            &even[from + 2..][..n],
        );
        for ((((o, &a), &b), &c), &d) in odd[from..to].iter_mut().zip(b3).zip(b1).zip(f1).zip(f3) {
            *o = wrap(*o + ((9 * (b + c) - (a + d) + 8) >> 4));
        }
    }
    for j in to..no {
        predict_at_end(even, odd, j);
    }
}

/// The first step for one sample down a column: it is lifted by the odd samples near it.
#[inline(always)]
fn lift_even(c: i16, p1: i16, n1: i16, p3: i16, n3: i16) -> i16 {
    let (p1, n1, p3, n3) = (i32::from(p1), i32::from(n1), i32::from(p3), i32::from(n3));
    (i32::from(c) - ((9 * (p1 + n1) - (p3 + n3) + 16) >> 5)) as i16
}

/// The second step for one sample down a column, away from the edge: it is predicted from the even samples near it.
#[inline(always)]
fn predict_odd(c: i16, p1: i16, n1: i16, p3: i16, n3: i16) -> i16 {
    let (p1, n1, p3, n3) = (i32::from(p1), i32::from(n1), i32::from(p3), i32::from(n3));
    (i32::from(c) + ((9 * (p1 + n1) - (p3 + n3) + 8) >> 4)) as i16
}

/// Next to the edge the second step has simpler forms.
#[inline(always)]
fn predict_near_edge(c: i16, before: i16, next: i16) -> i16 {
    (i32::from(c) + ((i32::from(before) + i32::from(next) + 1) >> 1)) as i16
}

/// `out[j] = f(row[j * s], ..)` over the samples of the rows that are `s` apart; the rows of a whole picture (`s` is 1) are read as
/// slices, which the compiler can do several at a time.
fn per_column<const N: usize>(out: &mut [i16], rows: [&[i16]; N], s: usize, f: impl Fn([i16; N]) -> i16) {
    if s == 1 {
        for (j, o) in out.iter_mut().enumerate() {
            *o = f(std::array::from_fn(|r| rows[r][j]));
        }
    } else {
        for (j, o) in out.iter_mut().enumerate() {
            *o = f(std::array::from_fn(|r| rows[r][j * s]));
        }
    }
}

/// What `filter_line` does to every column `x` that is a multiple of `s`, all the columns at once: the line is the rows `0, s, 2s ..`,
/// and the steps go over whole rows, so the plane is read along its rows and not down its columns. The samples that are not there
/// count as 0 in the first step, as in `filter_line` with `along_rows` false.
fn filter_columns(plane: &mut [i16], stride: usize, width: usize, height: usize, s: usize) {
    let lines = height.div_ceil(s);
    if lines == 0 || width == 0 {
        return;
    }
    let last = lines - 1;
    let count = width.div_ceil(s);
    let zeros = vec![0i16; width];
    let mut fresh = vec![0i16; count];
    let row_of = |k: usize| k * s * stride;
    // the first step: every even row is lifted by the odd rows near it
    for k in (0..=last).step_by(2) {
        {
            let at = |row: Option<usize>| -> &[i16] { row.map_or(&zeros[..], |r| &plane[row_of(r)..][..width]) };
            let rows = [
                &plane[row_of(k)..][..width],
                at(k.checked_sub(1)),
                at((k < last).then_some(k + 1)),
                at(k.checked_sub(3)),
                at((k + 3 <= last).then_some(k + 3)),
            ];
            per_column(&mut fresh, rows, s, |[c, p1, n1, p3, n3]| lift_even(c, p1, n1, p3, n3));
        }
        for (cell, &v) in plane[row_of(k)..][..width].iter_mut().step_by(s).zip(&fresh) {
            *cell = v;
        }
    }
    // the second step: every odd row is predicted from the even rows near it
    for k in (1..=last).step_by(2) {
        {
            let row = |r: usize| &plane[row_of(r)..][..width];
            if k < last && k >= 3 && k + 3 <= last {
                let rows = [row(k), row(k - 1), row(k + 1), row(k - 3), row(k + 3)];
                per_column(&mut fresh, rows, s, |[c, p1, n1, p3, n3]| {
                    predict_odd(c, p1, n1, p3, n3)
                });
            } else if k < last {
                per_column(&mut fresh, [row(k), row(k - 1), row(k + 1)], s, |[c, a, b]| {
                    predict_near_edge(c, a, b)
                });
            } else {
                per_column(&mut fresh, [row(k), row(k - 1)], s, |[c, a]| {
                    (i32::from(c) + i32::from(a)) as i16
                });
            }
        }
        for (cell, &v) in plane[row_of(k)..][..width].iter_mut().step_by(s).zip(&fresh) {
            *cell = v;
        }
    }
}

/// The inverse wavelet transform of a plane `width` x `height`, stored in rows of `stride`: for the distances 16, 8, 4, 2, 1 the
/// samples that are that far apart, down the columns and then along the rows.
fn inverse_wavelet(plane: &mut [i16], stride: usize, width: usize, height: usize) {
    let (mut even, mut odd) = (Vec::new(), Vec::new());
    let mut s = 16;
    while s >= 1 {
        filter_columns(plane, stride, width, height, s);
        for y in (0..height).step_by(s) {
            let row = &mut plane[y * stride..y * stride + width];
            even.clear();
            odd.clear();
            if s == 1 {
                // the samples side by side: the pairs come apart without a stride
                let (pairs, tail) = row.as_chunks::<2>();
                even.extend(pairs.iter().map(|p| i32::from(p[0])));
                odd.extend(pairs.iter().map(|p| i32::from(p[1])));
                even.extend(tail.iter().map(|&v| i32::from(v)));
                filter_halves(&mut even, &mut odd, true);
                let (pairs, tail) = row.as_chunks_mut::<2>();
                for (p, (&e, &o)) in pairs.iter_mut().zip(even.iter().zip(&odd)) {
                    p[0] = e as i16;
                    p[1] = o as i16;
                }
                if let (Some(cell), Some(&e)) = (tail.first_mut(), even.last()) {
                    *cell = e as i16;
                }
                continue;
            }
            even.extend(row.iter().step_by(2 * s).map(|&v| i32::from(v)));
            odd.extend(row.iter().skip(s).step_by(2 * s).map(|&v| i32::from(v)));
            filter_halves(&mut even, &mut odd, true);
            for (cell, &v) in row.iter_mut().step_by(2 * s).zip(&even) {
                *cell = v as i16;
            }
            for (cell, &v) in row.iter_mut().skip(s).step_by(2 * s).zip(&odd) {
                *cell = v as i16;
            }
        }
        s /= 2;
    }
}

/// A picture being read: the chunks of one layer, in order. The size and the kind (grey or colour) come with the first.
#[derive(Debug, Clone, Default)]
pub struct Iw44 {
    width: usize,
    height: usize,
    colour: bool,
    /// The slices before the chrominance starts.
    delay: usize,
    /// Slices read so far.
    slices: usize,
    /// The number the next chunk must have.
    serial: u32,
    planes: Vec<Plane>,
}

impl Iw44 {
    pub fn new() -> Self {
        Self::default()
    }

    /// The size of the picture, once the first chunk was read.
    pub fn size(&self) -> Option<(usize, usize)> {
        (!self.planes.is_empty()).then_some((self.width, self.height))
    }

    /// Reads one chunk (`BG44`, `FG44`, `PM44`, `BM44`): the first one carries the size and the kind, all of them slices.
    pub fn add_chunk(&mut self, data: &[u8]) -> Result<(), Error> {
        let (&serial, rest) = data.split_first().ok_or(Error::BadIw44("a chunk without a number"))?;
        if u32::from(serial) != self.serial & 0xFF || (self.planes.is_empty() != (serial == 0)) {
            return Err(Error::BadIw44("chunks out of order"));
        }
        let (&slices, rest) = rest
            .split_first()
            .ok_or(Error::BadIw44("a chunk without a count of slices"))?;
        let rest = if serial == 0 {
            if rest.len() < 7 {
                return Err(Error::BadIw44("a first chunk too short for its header"));
            }
            let width = usize::from(u16::from_be_bytes([rest[2], rest[3]]));
            let height = usize::from(u16::from_be_bytes([rest[4], rest[5]]));
            if width == 0 || height == 0 {
                return Err(Error::BadIw44("a picture without a size"));
            }
            if width * height > MAX_PIXELS {
                return Err(Error::BadIw44("a picture too big"));
            }
            self.width = width;
            self.height = height;
            self.colour = rest[0] & 0x80 == 0;
            self.delay = if rest[1] >= 2 { usize::from(rest[6] & 0x7F) } else { 0 };
            self.slices = 0;
            let count = if self.colour { 3 } else { 1 };
            self.planes = (0..count).map(|_| Plane::new(width, height)).collect();
            &rest[7..]
        } else {
            rest
        };
        if self.slices + usize::from(slices) > MAX_SLICES {
            return Err(Error::BadIw44("too many slices"));
        }
        let mut zp = ZpDecoder::new(rest);
        for _ in 0..slices {
            self.slices += 1;
            self.planes[0].slice(&mut zp);
            if self.colour && self.slices > self.delay {
                self.planes[1].slice(&mut zp);
                self.planes[2].slice(&mut zp);
            }
        }
        self.serial += 1;
        Ok(())
    }

    /// The picture as RGB, from what was read so far.
    pub fn pixmap(&self) -> Result<Pixmap, Error> {
        let (width, height) = self.size().ok_or(Error::BadIw44("no first chunk"))?;
        // the three planes of a colour picture do not depend on each other: a thread each, when the picture is big enough to pay for
        // it; a plane whose thread could not be made, or came to grief, is done here
        let planes: Vec<(Vec<i16>, usize)> = if self.planes.len() > 1 && width * height >= 1 << 16 {
            std::thread::scope(|scope| {
                let started: Vec<_> = self
                    .planes
                    .iter()
                    .map(|p| std::thread::Builder::new().spawn_scoped(scope, move || p.reconstruct(width, height)))
                    .collect();
                started
                    .into_iter()
                    .zip(&self.planes)
                    .map(|(thread, p)| match thread.map(|t| t.join()) {
                        Ok(Ok(done)) => done,
                        _ => p.reconstruct(width, height),
                    })
                    .collect()
            })
        } else {
            self.planes.iter().map(|p| p.reconstruct(width, height)).collect()
        };
        // a grey level with 6 fractional bits, rounded to a whole one
        let level = |v: i16| ((i32::from(v) + 32) >> 6).clamp(-128, 127);
        let mut rgb = vec![0u8; width * height * 3];
        for (top_row, out) in rgb.chunks_exact_mut((width * 3).max(1)).enumerate().take(height) {
            let row = height - 1 - top_row; // the rows are stored from the bottom
            let line = |plane: usize| &planes[plane].0[row * planes[plane].1..][..width];
            if self.colour {
                for ((pixel, &y), (&cb, &cr)) in out
                    .as_chunks_mut::<3>()
                    .0
                    .iter_mut()
                    .zip(line(0))
                    .zip(line(1).iter().zip(line(2)))
                {
                    let (y, cb, cr) = (level(y), level(cb), level(cr));
                    let t2 = cr + (cr >> 1);
                    let t3 = y + 128 - (cb >> 2);
                    pixel[0] = (y + 128 + t2).clamp(0, 255) as u8;
                    pixel[1] = (t3 - (t2 >> 1)).clamp(0, 255) as u8;
                    pixel[2] = (t3 + (cb << 1)).clamp(0, 255) as u8;
                }
            } else {
                for (pixel, &y) in out.as_chunks_mut::<3>().0.iter_mut().zip(line(0)) {
                    pixel.fill((127 - level(y)) as u8);
                }
            }
        }
        Ok(Pixmap { width, height, rgb })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // Streams made by another encoder (the IW44 encoder of djvu-rs, MIT) from synthetic pictures, and what DjVuLibre's ddjvu made of
    // them. Our decoder, written from the spec, must give the same pixels.
    const GRAY_9X17_B_0: [u8; 23] = [
        0x00, 0x0a, 0x81, 0x02, 0x00, 0x09, 0x00, 0x11, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_1: [u8; 16] = [
        0x01, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_2: [u8; 16] = [
        0x02, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_3: [u8; 16] = [
        0x03, 0x0a, 0x12, 0x3f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_4: [u8; 16] = [
        0x04, 0x0a, 0x66, 0x77, 0xf8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_5: [u8; 16] = [
        0x05, 0x0a, 0x52, 0xa1, 0x06, 0xd6, 0x50, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_6: [u8; 16] = [
        0x06, 0x0a, 0xd1, 0x2b, 0xfc, 0x6f, 0xc4, 0x03, 0xbb, 0x85, 0x8f, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const GRAY_9X17_B_7: [u8; 10] = [0x07, 0x04, 0x09, 0x94, 0x14, 0x36, 0x71, 0xff, 0xff, 0xff];
    const GRAY_9X17_B_PIXELS: [u8; 153] = [
        99, 105, 111, 122, 124, 102, 72, 46, 21, 95, 101, 107, 116, 119, 99, 73, 51, 29, 91, 97, 102, 111, 114, 97, 74,
        55, 37, 89, 95, 101, 109, 112, 97, 75, 51, 27, 83, 88, 93, 101, 104, 94, 76, 49, 23, 62, 65, 68, 72, 75, 72,
        75, 105, 135, 38, 39, 40, 40, 43, 58, 74, 78, 81, 18, 17, 16, 11, 14, 41, 73, 98, 122, 11, 9, 7, 0, 3, 33, 73,
        110, 147, 38, 37, 36, 32, 33, 51, 75, 97, 120, 75, 76, 77, 78, 78, 78, 78, 78, 77, 112, 115, 118, 124, 123,
        105, 81, 61, 41, 136, 140, 143, 152, 151, 122, 83, 50, 17, 118, 121, 123, 129, 129, 110, 84, 56, 28, 86, 87,
        88, 89, 90, 89, 84, 69, 54, 62, 61, 60, 58, 59, 66, 85, 129, 174, 37, 35, 33, 27, 29, 55, 85, 102, 118,
    ];
    const COLOUR_5X3_B_0: [u8; 23] = [
        0x00, 0x0a, 0x01, 0x02, 0x00, 0x05, 0x00, 0x03, 0x8a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_1: [u8; 16] = [
        0x01, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_2: [u8; 16] = [
        0x02, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_3: [u8; 16] = [
        0x03, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_4: [u8; 16] = [
        0x04, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_5: [u8; 16] = [
        0x05, 0x0a, 0x37, 0x64, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_6: [u8; 16] = [
        0x06, 0x0a, 0xcb, 0x1e, 0x17, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_5X3_B_7: [u8; 10] = [0x07, 0x04, 0xe3, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    const COLOUR_5X3_B_PIXELS: [u8; 45] = [
        107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107,
        135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135, 160, 107, 135,
        160,
    ];
    const COLOUR_40X27_A_0: [u8; 23] = [
        0x00, 0x0a, 0x01, 0x02, 0x00, 0x28, 0x00, 0x1b, 0x80, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_40X27_A_1: [u8; 16] = [
        0x01, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    const COLOUR_40X27_A_2: [u8; 16] = [
        0x02, 0x0a, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    // the colour picture 40 x 27 is checked by the FNV-1a hash of what ddjvu made of it
    const COLOUR_40X27_A_HASH: u64 = 0x2a79_e010_7b3d_e345;

    fn fnv(data: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    fn decode(chunks: &[&[u8]]) -> Pixmap {
        let mut image = Iw44::new();
        for chunk in chunks {
            image.add_chunk(chunk).unwrap();
        }
        image.pixmap().unwrap()
    }

    fn noise(len: usize, mut seed: u64) -> Vec<u8> {
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            })
            .collect()
    }

    fn gray_stream() -> Vec<&'static [u8]> {
        vec![
            &GRAY_9X17_B_0,
            &GRAY_9X17_B_1,
            &GRAY_9X17_B_2,
            &GRAY_9X17_B_3,
            &GRAY_9X17_B_4,
            &GRAY_9X17_B_5,
            &GRAY_9X17_B_6,
            &GRAY_9X17_B_7,
        ]
    }

    #[test]
    fn pictures_that_another_encoder_wrote_come_out_as_ddjvu_makes_them() {
        // 74 slices in 8 chunks; the rows of 9 samples need the exception of DjVuLibre in the transform along rows
        let gray = decode(&gray_stream());
        assert_eq!((gray.width, gray.height), (9, 17));
        let levels: Vec<u8> = gray.rgb.chunks(3).map(|p| p[0]).collect();
        assert_eq!(levels, GRAY_9X17_B_PIXELS);
        assert!(
            gray.rgb.chunks(3).all(|p| p[0] == p[1] && p[1] == p[2]),
            "a grey picture has no colour"
        );

        // colour, the chrominance starting after 10 slices
        let colour = decode(&[
            &COLOUR_5X3_B_0,
            &COLOUR_5X3_B_1,
            &COLOUR_5X3_B_2,
            &COLOUR_5X3_B_3,
            &COLOUR_5X3_B_4,
            &COLOUR_5X3_B_5,
            &COLOUR_5X3_B_6,
            &COLOUR_5X3_B_7,
        ]);
        assert_eq!((colour.width, colour.height), (5, 3));
        assert_eq!(colour.rgb, COLOUR_5X3_B_PIXELS);

        let more = decode(&[&COLOUR_40X27_A_0, &COLOUR_40X27_A_1, &COLOUR_40X27_A_2]);
        assert_eq!((more.width, more.height), (40, 27));
        assert_eq!(fnv(&more.rgb), COLOUR_40X27_A_HASH);
    }

    #[test]
    fn a_picture_with_nothing_in_it_is_mid_grey_and_a_flat_one_stays_flat() {
        // no coefficient at all: the grey level 0 is 127 in a grey picture
        let mut image = Iw44::new();
        image.add_chunk(&[0, 0, 0x81, 2, 0, 20, 0, 10, 0]).unwrap();
        let pixmap = image.pixmap().unwrap();
        assert_eq!((pixmap.width, pixmap.height), (20, 10));
        assert!(pixmap.rgb.iter().all(|&v| v == 127));

        // only the first coefficient of the one block: the picture is one level, whatever the size
        for (w, h) in [(20, 10), (32, 32), (1, 1), (5, 30), (31, 17)] {
            let mut plane = Plane::new(w, h);
            plane.coefs[0] = 64 * 20;
            let (rebuilt, stride) = plane.reconstruct(w, h);
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(rebuilt[y * stride + x], 64 * 20, "{w} x {h} at {x}, {y}");
                }
            }
        }
    }

    /// The line filter on one line in order, by the halves.
    fn filter_line(line: &mut [i32], along_rows: bool) {
        let mut even: Vec<i32> = line.iter().step_by(2).copied().collect();
        let mut odd: Vec<i32> = line.iter().skip(1).step_by(2).copied().collect();
        filter_halves(&mut even, &mut odd, along_rows);
        for (k, v) in line.iter_mut().enumerate() {
            *v = if k % 2 == 0 { even[k / 2] } else { odd[k / 2] };
        }
    }

    /// The filter as it was first written, one line in order with the neighbours kept in variables: the reference for the halves.
    fn reference_line(line: &mut [i32], along_rows: bool) {
        let n = line.len();
        if n == 0 {
            return;
        }
        let last = n - 1;
        // the first step, near the ends of the line (k < 4 or k + 3 > last) and away from them
        let lift_at_end = |line: &mut [i32], k: usize| {
            let p1 = if k >= 1 { line[k - 1] } else { 0 };
            let p3 = if k >= 3 { line[k - 3] } else { 0 };
            // DjVuLibre keeps the sample it had for k + 1 when k + 3 is past the end, for the third and the fifth sample, along the
            // rows only (down the columns it takes 0, as the spec says); the last sample has that for its own k + 1
            let n1 = if k < last {
                line[k + 1]
            } else if along_rows && (k == 4 || k == 6) {
                line[k - 1]
            } else {
                0
            };
            let n3 = if k + 3 <= last {
                line[k + 3]
            } else if along_rows && (k == 2 || k == 4) {
                n1
            } else {
                0
            };
            line[k] = wrap(line[k] - ((9 * (p1 + n1) - (p3 + n3) + 16) >> 5));
        };
        let (mut k, inner_end) = (0, if last >= 7 { (last - 3) & !1 } else { 0 });
        while k <= last && (inner_end == 0 || k < 4) {
            lift_at_end(line, k);
            k += 2;
        }
        if inner_end != 0 {
            for k in (4..=inner_end).step_by(2) {
                line[k] = wrap(line[k] - ((9 * (line[k - 1] + line[k + 1]) - (line[k - 3] + line[k + 3]) + 16) >> 5));
            }
            for k in (inner_end + 2..=last).step_by(2) {
                lift_at_end(line, k);
            }
        }
        // the second step: the same, the ends having their own forms
        let predict_at_end = |line: &mut [i32], k: usize| {
            let before = line[k - 1];
            if k < last {
                line[k] = wrap(line[k] + ((before + line[k + 1] + 1) >> 1));
            } else {
                line[k] = wrap(line[k] + before);
            }
        };
        if last >= 6 {
            predict_at_end(line, 1);
            let inner_end = (last - 3) | 1;
            let inner_end = if inner_end > last - 3 { inner_end - 2 } else { inner_end };
            for k in (3..=inner_end).step_by(2) {
                line[k] = wrap(line[k] + ((9 * (line[k - 1] + line[k + 1]) - (line[k - 3] + line[k + 3]) + 8) >> 4));
            }
            for k in (inner_end + 2..=last).step_by(2) {
                predict_at_end(line, k);
            }
        } else {
            for k in (1..=last).step_by(2) {
                predict_at_end(line, k);
            }
        }
    }

    #[test]
    fn the_halves_give_what_the_line_in_order_gave() {
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as i32 % 65536 - 32768
        };
        for n in 0..90 {
            for along_rows in [false, true] {
                for _ in 0..20 {
                    let line: Vec<i32> = (0..n).map(|_| next()).collect();
                    let (mut a, mut b) = (line.clone(), line);
                    reference_line(&mut a, along_rows);
                    filter_line(&mut b, along_rows);
                    assert_eq!(a, b, "{n} {along_rows}");
                }
            }
        }
    }

    #[test]
    fn the_filter_of_a_line_has_no_trouble_with_short_ones_and_keeps_a_flat_line_flat() {
        for n in 0..40 {
            for along_rows in [false, true] {
                // only the even samples carry the picture; the odd ones are the detail, zero here
                let mut line: Vec<i32> = (0..n).map(|k| if k % 2 == 1 { 0 } else { 5000 }).collect();
                filter_line(&mut line, along_rows);
                if n >= 9 {
                    // the ends of a line have fewer neighbours to lift by, so they move a little
                    assert!(
                        line.iter().all(|&v| (v - 5000).abs() < 1500),
                        "{n} {along_rows}: {line:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn chunks_must_come_in_order_and_say_what_they_are() {
        let first = [0, 0, 0x81, 2, 0, 8, 0, 8, 0];
        let mut image = Iw44::new();
        assert!(image.add_chunk(&[]).is_err());
        assert!(image.add_chunk(&[0]).is_err(), "no count of slices");
        assert!(image.add_chunk(&[1, 0]).is_err(), "a later chunk before the first");
        assert!(image.add_chunk(&first[..6]).is_err(), "a header that is cut short");
        assert!(image.add_chunk(&[0, 0, 0x81, 2, 0, 0, 0, 8, 0]).is_err(), "no width");
        assert!(
            image.add_chunk(&[0, 0, 0x81, 2, 0xFF, 0xFF, 0xFF, 0xFF, 0]).is_err(),
            "more pixels than the limit"
        );
        assert!(image.pixmap().is_err(), "nothing was read");
        image.add_chunk(&first).unwrap();
        assert!(image.add_chunk(&first).is_err(), "a second first chunk");
        assert!(image.add_chunk(&[5, 0]).is_err(), "a chunk that skips a number");
        image.add_chunk(&[1, 0]).unwrap();
        assert_eq!(image.size(), Some((8, 8)));

        // the slices are limited for the whole picture, whatever the chunks
        let mut image = Iw44::new();
        image.add_chunk(&first).unwrap();
        let mut error = None;
        for serial in 1..=4 {
            if let Err(e) = image.add_chunk(&[serial, 255, 0xFF, 0xFF]) {
                error = Some(e);
                break;
            }
        }
        assert_eq!(error, Some(Error::BadIw44("too many slices")));
    }

    #[test]
    fn data_of_any_kind_is_an_error_or_a_picture_and_never_a_panic() {
        for seed in 1..40_u64 {
            for len in [9_usize, 10, 12, 40, 300, 2000] {
                let mut data = noise(len, seed * 0x9E37);
                data[0] = 0;
                data[2] &= 0x81; // either kind
                data[4] = 0;
                data[5] = 1 + data[5] % 90;
                data[6] = 0;
                data[7] = 1 + data[7] % 90;
                data[1] = 1 + data[1] % 40;
                let mut image = Iw44::new();
                if image.add_chunk(&data).is_ok() {
                    let _ = image.pixmap();
                    let mut more = noise(len / 2 + 2, seed);
                    more[0] = 1;
                    let _ = image.add_chunk(&more);
                    let _ = image.pixmap();
                }
            }
        }

        // a real stream cut at every length and with each byte changed in turn
        let stream = gray_stream();
        for cut in 0..stream.len() {
            let mut image = Iw44::new();
            for chunk in &stream[..cut] {
                let _ = image.add_chunk(chunk);
            }
            let _ = image.pixmap();
        }
        for (i, chunk) in stream.iter().enumerate() {
            for at in 0..chunk.len() {
                let mut changed = chunk.to_vec();
                changed[at] ^= 0xFF;
                let mut image = Iw44::new();
                for (j, other) in stream.iter().enumerate() {
                    let _ = image.add_chunk(if i == j { &changed } else { other });
                }
                let _ = image.pixmap();
            }
        }
    }
}
