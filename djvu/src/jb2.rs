//! JB2, the bi-level picture coding of DjVu (Appendix 2 of the official DjVu v3 specification): the text of a scanned page is
//! coded as symbols (the shapes of the letters), placed one by one on a white page; many pages share one dictionary of shapes.
//!
//! A stream (an `Sjbz` chunk for a page, a `Djbz` chunk for a shared dictionary) is arithmetic-coded with the Z'-Coder
//! ([`crate::zp`]): a record type, then the fields of that record (spec 11.2.3). Whole numbers are coded by the "multivalue
//! extension" of 11.2.2 to 11.2.5 (a sign, a range from Table 8, then the bits within the range, each decision with a context of
//! its own); the pixels of a shape by a context of 10 neighbouring pixels (direct coding, Figure 2) or of 4 of its own and 7 of
//! a matching shape's (refinement coding, Figure 3). Where the spec is silent the choice made here is said in a comment.
//!
//! Coordinates in the spec run from the lower left corner (column 1, row 1); the page is stored top row first, so the rows are
//! turned over when a shape is put on the page.

use crate::Error;
use crate::zp::ZpDecoder;

/// The largest dimension the integer coding can carry (the last range of Table 8).
const MAX_DIMENSION: i64 = 262_142;

/// The biggest page that is made (as in the other readers of this project: a page of 100 million pixels is already past a big scan).
const MAX_PAGE_PIXELS: usize = 100_000_000;

/// The most pixels that the shapes of one stream may add up to (a hostile stream cannot make an endless library).
const MAX_SHAPE_PIXELS: usize = 256 * 1024 * 1024;

/// The most records in one stream.
const MAX_RECORDS: usize = 8_000_000;

/// A bi-level picture: one byte a pixel, 1 is black, 0 is white; the top row first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Bitmap {
    /// A white bitmap.
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }
    /// The smallest bitmap that holds every black pixel (spec 11.5.10: the empty edge rows and columns are removed before a symbol
    /// is added to the library); an empty bitmap when there is no black pixel.
    fn cropped(&self) -> Self {
        if self.width == 0 {
            return Self::default();
        }
        let rows = || self.pixels.chunks_exact(self.width);
        // the first and the last row that have a black pixel, and the widest span of black pixels in the rows between
        let Some(y0) = rows().position(|row| row.iter().any(|&p| p != 0)) else {
            return Self::default();
        };
        let y1 = self.height - 1 - rows().rev().position(|row| row.iter().any(|&p| p != 0)).unwrap_or(0);
        let (mut x0, mut x1) = (usize::MAX, 0_usize);
        for row in rows().skip(y0).take(y1 - y0 + 1) {
            if let Some(first) = row.iter().position(|&p| p != 0) {
                x0 = x0.min(first);
                x1 = x1.max(row.iter().rposition(|&p| p != 0).unwrap_or(first));
            }
        }
        let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut out = Self::new(w, h);
        for y in 0..h {
            let from = (y0 + y) * self.width + x0;
            out.pixels[y * w..(y + 1) * w].copy_from_slice(&self.pixels[from..from + w]);
        }
        out
    }

    /// Puts the black pixels of `symbol` on this bitmap with its upper left pixel at (left, top); what falls outside is dropped.
    /// Row by row: the part of each row that is on the page is worked out once.
    fn blit(&mut self, symbol: &Self, left: i64, top: i64) {
        // the columns of the symbol that are on the page
        let from = (-left).clamp(0, symbol.width as i64) as usize;
        let to = (self.width as i64 - left).clamp(0, symbol.width as i64) as usize;
        if from >= to || symbol.width == 0 {
            return;
        }
        for (sy, source) in symbol.pixels.chunks_exact(symbol.width).enumerate() {
            let y = top + sy as i64;
            if y < 0 || y >= self.height as i64 {
                continue;
            }
            let start = y as usize * self.width + (left + from as i64) as usize;
            for (page, &pixel) in self.pixels[start..start + (to - from)]
                .iter_mut()
                .zip(&source[from..to])
            {
                *page |= u8::from(pixel != 0);
            }
        }
    }
}

/// A dictionary of shapes shared by the pages of a document (a `Djbz` chunk).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dictionary {
    pub symbols: Vec<Bitmap>,
}

/// The binary contexts of one kind of whole number (spec 11.2.2): the one for the sign, and below it two trees that have nothing in
/// common, one for the numbers below zero and one for the rest (the sign is the root of the decision tree of Figure 1); in each, one
/// context for each range of Table 8 and a tree of them for the bits inside each range (made when first used).
struct NumContext {
    sign: u8,
    branches: [Branch; 2],
}

struct Branch {
    ranges: [u8; 18],
    trees: Vec<Vec<u8>>,
}

impl NumContext {
    fn new() -> Self {
        let branch = || Branch {
            ranges: [0; 18],
            trees: vec![Vec::new(); 17],
        };
        Self {
            sign: 0,
            branches: [branch(), branch()],
        }
    }
}

/// The 16 kinds of whole number of Table 7.
#[derive(Clone, Copy)]
enum Kind {
    RecordType,
    ImageSize,
    MatchIndex,
    SymbolWidth,
    SymbolHeight,
    WidthDifference,
    HeightDifference,
    Column,
    Row,
    SameLineColumn,
    SameLineRow,
    NewLineColumn,
    NewLineRow,
    CommentLength,
    CommentOctet,
    DictionarySize,
}

/// The contexts that the decoding of a stream keeps.
struct Contexts {
    numbers: Vec<NumContext>,
    /// The "offset type" flag (spec 11.5.8) and the "eventual image refinement" flag (11.5.3).
    offset_type: u8,
    refinement_flag: u8,
    direct: Vec<u8>,
    refine: Vec<u8>,
}

impl Contexts {
    fn new() -> Self {
        Self {
            numbers: (0..16).map(|_| NumContext::new()).collect(),
            offset_type: 0,
            refinement_flag: 0,
            direct: vec![0; 1024],
            refine: vec![0; 2048],
        }
    }

    /// The numcoder reset of 11.4.10.2: every context used for coding numbers is cleared.
    fn reset_numbers(&mut self) {
        for n in &mut self.numbers {
            *n = NumContext::new();
        }
    }
}

/// A whole number in `low..=high` (spec 11.2.5): the sign (unless the limits settle it), the range of Table 8 that `v` is in, the
/// bits of `v` inside it most significant first, and `n = v` or `n = -v - 1`. A decision that the limits settle is not coded and is
/// taken as settled (spec 11.2.5.4).
fn decode_num(zp: &mut ZpDecoder<'_>, ctx: &mut NumContext, low: i64, high: i64) -> i64 {
    // phase 1: the sign (0 means below zero)
    let positive = if low < 0 && high >= 0 {
        zp.decode_bit(&mut ctx.sign)
    } else {
        high >= 0
    };
    // the values `v` can take: n >= 0 gives v = n, n < 0 gives v = -n - 1
    let (vmin, vmax) = if positive {
        (low.max(0), high)
    } else {
        ((-high - 1).max(0), -low - 1)
    };
    let branch = &mut ctx.branches[usize::from(positive)];
    // phase 2: which range of Table 8 (0, 1-2, 3-6, 7-14, ...) holds v
    let mut found = None;
    for j in 0..=17_usize {
        let (lo, hi) = if j == 0 {
            (0, 0)
        } else {
            ((1_i64 << j) - 1, (1_i64 << (j + 1)) - 2)
        };
        if hi < vmin {
            continue; // v cannot be in a range below its smallest value: "not in this range" is settled
        }
        let inside = if vmax <= hi {
            true // the last range v can be in: "in this range" is settled
        } else {
            // spec 11.2.5.2 says a 1 means "v is in this range"; in real streams a 1 means "v is further on" (found by decoding
            // the pages of real books), so a 0 is what says "in this range"
            !zp.decode_bit(&mut branch.ranges[j])
        };
        if inside {
            found = Some((j, lo, hi));
            break;
        }
    }
    let (j, lo, hi) = found.unwrap_or((17, (1 << 17) - 1, (1 << 18) - 2));
    // phase 3: the bits of v - lo, most significant first, down a complete binary tree of 2^j leaves
    let v = if j == 0 {
        0
    } else {
        let size = 1_i64 << j;
        let allowed_from = vmin.max(lo) - lo;
        let allowed_to = vmax.min(hi) - lo;
        let tree = &mut branch.trees[j - 1];
        if tree.is_empty() {
            tree.resize(usize::try_from(size - 1).unwrap_or(0), 0);
        }
        let (mut node, mut start, mut width) = (1_usize, 0_i64, size);
        while width > 1 {
            let half = width / 2;
            let bit = if allowed_from > start + half - 1 {
                true // the lower half holds none of the values that are allowed
            } else if allowed_to < start + half {
                false // the upper half holds none
            } else {
                zp.decode_bit(&mut tree[node - 1])
            };
            if bit {
                start += half;
            }
            node = node * 2 + usize::from(bit);
            width = half;
        }
        lo + start
    };
    // phase 4
    if positive { v } else { -v - 1 }
}

/// Where the shapes of the current text line are, in the coordinates of the spec (columns from 1 at the left, rows from 1 at the
/// bottom), for the relative placing of 11.5.8.
struct Line {
    first_left: i64,
    first_bottom: i64,
    previous_right: i64,
    bottoms: Vec<i64>,
}

/// One stream being decoded.
struct Decoder<'a, 'd> {
    zp: ZpDecoder<'a>,
    ctx: Contexts,
    /// The shapes of the shared dictionary that this stream inherits (a prefix of it), then the ones it adds.
    base: &'d [Bitmap],
    own: Vec<Bitmap>,
    shape_pixels: usize,
    page: Bitmap,
    /// Whether the shapes are drawn on `page` as they are placed (not when only the list of blits is wanted).
    paint: bool,
    /// When the blits are wanted (for the colours of a page): where each shape was put, and the shapes that are not in the library.
    blits: Option<Vec<Blit>>,
    loose: Vec<Bitmap>,
    line: Line,
}

impl<'a, 'd> Decoder<'a, 'd> {
    fn num(&mut self, kind: Kind, low: i64, high: i64) -> i64 {
        decode_num(&mut self.zp, &mut self.ctx.numbers[kind as usize], low, high)
    }

    fn library_len(&self) -> usize {
        self.base.len() + self.own.len()
    }

    fn symbol(&self, index: usize) -> Option<&Bitmap> {
        if index < self.base.len() {
            self.base.get(index)
        } else {
            self.own.get(index - self.base.len())
        }
    }

    fn charge(&mut self, width: usize, height: usize) -> Result<(), Error> {
        self.shape_pixels = self.shape_pixels.saturating_add(width.saturating_mul(height));
        if self.shape_pixels > MAX_SHAPE_PIXELS {
            return Err(Error::BadJb2("the shapes add up to more than the limit"));
        }
        Ok(())
    }

    /// A size field pair: width then height, each `0..=MAX_DIMENSION`.
    fn absolute_size(&mut self) -> Result<(usize, usize), Error> {
        let w = self.num(Kind::SymbolWidth, 0, MAX_DIMENSION);
        let h = self.num(Kind::SymbolHeight, 0, MAX_DIMENSION);
        let (w, h) = (usize::try_from(w).unwrap_or(0), usize::try_from(h).unwrap_or(0));
        self.charge(w, h)?;
        Ok((w, h))
    }

    /// A shape by direct coding (spec 11.5.9): every pixel with the 10 neighbours of Figure 2 as its context: row -2: -1, 0, +1;
    /// row -1: -2 to +2; the row itself: -2, -1 (the first neighbour is the highest bit of the context). The shape is made in a
    /// frame with two empty rows above and empty columns on both sides, so that no pixel needs a test of the edge, and the three
    /// parts of the context slide along the rows from one pixel to the next.
    fn direct_bitmap(&mut self, width: usize, height: usize) -> Bitmap {
        let stride = width + 6;
        let mut frame = vec![0u8; (height + 2) * stride];
        for y in 0..height {
            let (above, rest) = frame.split_at_mut((y + 2) * stride);
            let two_up = &above[y * stride..(y + 1) * stride];
            let one_up = &above[(y + 1) * stride..];
            let row = &mut rest[..stride];
            // the pixel x of a row is at x + 2
            self.zp
                .decode_direct_row(&mut self.ctx.direct, row, two_up, one_up, width);
        }
        let mut bitmap = Bitmap::new(width, height);
        for (y, line) in bitmap.pixels.chunks_exact_mut(width.max(1)).enumerate().take(height) {
            line.copy_from_slice(&frame[(y + 2) * stride + 2..][..width]);
        }
        bitmap
    }

    /// A shape by refinement of the library shape `matching` (spec 11.5.10): 4 pixels of the shape itself and 7 of the matching
    /// one (Figure 3), the two aligned at the centres of their rectangles (the left of the two middle columns, the lower of the
    /// two middle rows when a side is even). The shape is made in a frame like that of a direct shape, and the three rows of the
    /// matching shape that a row looks at are laid out against it once for the row.
    fn refined_bitmap(&mut self, width: usize, height: usize, matching: &Bitmap) -> Bitmap {
        let stride = width + 6;
        let mut frame = vec![0u8; (height + 1) * stride];
        let shift_x = (matching.width as i64 - 1) / 2 - (width as i64 - 1) / 2;
        let shift_y = (matching.height as i64) / 2 - (height as i64) / 2;
        // the matching shape's row `row`, as seen from the columns -1 to width + 1 of the shape that is made (white where the
        // matching shape is not)
        let aligned = |row: i64, out: &mut Vec<u8>| {
            out.clear();
            out.resize(width + 3, 0);
            if row < 0 || row >= matching.height as i64 || matching.width == 0 {
                return;
            }
            // the entries i with 0 <= i - 1 + shift_x < matching.width are the ones the matching shape has
            let first = (1 - shift_x).clamp(0, width as i64 + 3) as usize;
            let last = (matching.width as i64 + 1 - shift_x).clamp(0, width as i64 + 3) as usize;
            if first < last {
                let at = row as usize * matching.width + (first as i64 - 1 + shift_x) as usize;
                out[first..last].copy_from_slice(&matching.pixels[at..at + (last - first)]);
            }
        };
        let (mut up, mut mid, mut down) = (Vec::new(), Vec::new(), Vec::new());
        for y in 0..height {
            let (above, rest) = frame.split_at_mut((y + 1) * stride);
            let one_up = &above[y * stride..];
            let row = &mut rest[..stride];
            aligned(y as i64 + shift_y - 1, &mut up);
            aligned(y as i64 + shift_y, &mut mid);
            aligned(y as i64 + shift_y + 1, &mut down);
            let mut left = 0_usize;
            for x in 0..width {
                // pixels 1 to 4: (-1,-1) (0,-1) (1,-1) (-1,0) of the shape being made
                let own =
                    usize::from(one_up[x + 1]) << 2 | usize::from(one_up[x + 2]) << 1 | usize::from(one_up[x + 3]);
                // pixels 5 to 11 of the matching shape: (0,-1) (-1,0) (0,0) (1,0) (-1,1) (0,1) (1,1) round the aligned place
                let m_up = usize::from(up[x + 1]);
                let m_mid = usize::from(mid[x]) << 2 | usize::from(mid[x + 1]) << 1 | usize::from(mid[x + 2]);
                let m_down = usize::from(down[x]) << 2 | usize::from(down[x + 1]) << 1 | usize::from(down[x + 2]);
                let index = own << 8 | left << 7 | m_up << 6 | m_mid << 3 | m_down;
                let bit = self.zp.decode_bit(&mut self.ctx.refine[index]);
                row[x + 2] = u8::from(bit);
                left = usize::from(bit);
            }
        }
        let mut bitmap = Bitmap::new(width, height);
        for (y, line) in bitmap.pixels.chunks_exact_mut(width.max(1)).enumerate().take(height) {
            line.copy_from_slice(&frame[(y + 1) * stride + 2..][..width]);
        }
        bitmap
    }

    /// The place of a shape of this size that is given relative to another (spec 11.5.8): its left column and its bottom row.
    fn relative_place(&mut self, width: usize, height: usize) -> (i64, i64) {
        let (width, height) = (width as i64, height as i64);
        let first = self.zp.decode_bit(&mut self.ctx.offset_type);
        if first {
            // relative to the first shape of the line, which this one then becomes the first of a new line
            let dx = self.num(Kind::NewLineColumn, -MAX_DIMENSION - 1, MAX_DIMENSION);
            let dy = self.num(Kind::NewLineRow, -MAX_DIMENSION - 1, MAX_DIMENSION);
            let left = self.line.first_left + dx;
            let top = self.line.first_bottom + dy;
            let bottom = top - height + 1;
            self.line = Line {
                first_left: left,
                first_bottom: bottom,
                previous_right: left + width - 1,
                bottoms: vec![bottom],
            };
            (left, bottom)
        } else {
            // relative to the previous shape of the line and the baseline of the line
            let dx = self.num(Kind::SameLineColumn, -MAX_DIMENSION - 1, MAX_DIMENSION);
            let dy = self.num(Kind::SameLineRow, -MAX_DIMENSION - 1, MAX_DIMENSION);
            let left = self.line.previous_right + dx;
            let baseline = match self.line.bottoms.as_slice() {
                [.., a, b, c] => {
                    let mut three = [*a, *b, *c];
                    three.sort_unstable();
                    three[1] // the median of the bottoms of the last three shapes
                }
                _ => self.line.first_bottom,
            };
            let bottom = baseline + dy;
            self.line.previous_right = left + width - 1;
            self.line.bottoms.push(bottom);
            (left, bottom)
        }
    }

    /// Puts a shape on the page with its lower left pixel at column `left`, row `bottom` (spec coordinates). `library` is the number
    /// of the shape in the library when it is one of its shapes, as that is how the blit is kept.
    fn place(&mut self, shape: &Bitmap, left: i64, bottom: i64, library: Option<usize>) {
        let top_row = bottom + shape.height as i64 - 1;
        let top = self.page.height as i64 - top_row;
        if self.paint {
            self.page.blit(shape, left - 1, top);
        }
        if self.blits.is_some() {
            let number = library.unwrap_or_else(|| {
                self.loose.push(shape.clone());
                usize::MAX - (self.loose.len() - 1) // a number counted from the end: the library is not all known yet
            });
            if let Some(blits) = self.blits.as_mut() {
                blits.push(Blit {
                    left: left - 1,
                    top,
                    shape: number,
                });
            }
        }
    }

    /// The record loop (spec 11.2.3, 11.4). `dictionary` is true for a shared dictionary, which may hold only a few record types
    /// (11.4.10.3).
    fn records(&mut self, dictionary: bool) -> Result<(), Error> {
        for _ in 0..MAX_RECORDS {
            if self.zp.is_exhausted() {
                return Err(Error::BadJb2("the data ends before the end-of-data record"));
            }
            let record = self.num(Kind::RecordType, 0, 11);
            if dictionary && !matches!(record, 2 | 5 | 9 | 10 | 11) {
                return Err(Error::BadJb2("a record that a shared dictionary may not hold"));
            }
            match record {
                1..=3 => {
                    let (w, h) = self.absolute_size()?;
                    let shape = self.direct_bitmap(w, h);
                    if record != 2 {
                        let (left, bottom) = self.relative_place(w, h);
                        self.place(&shape, left, bottom, None);
                    }
                    if record != 3 {
                        self.own.push(shape.cropped());
                    }
                }
                4..=6 => {
                    let len = self.library_len();
                    if len == 0 {
                        return Err(Error::BadJb2("a refinement of a shape in an empty library"));
                    }
                    let index = usize::try_from(self.num(Kind::MatchIndex, 0, len as i64 - 1)).unwrap_or(0);
                    let dw = self.num(Kind::WidthDifference, -MAX_DIMENSION - 1, MAX_DIMENSION);
                    let dh = self.num(Kind::HeightDifference, -MAX_DIMENSION - 1, MAX_DIMENSION);
                    let matching = self
                        .symbol(index)
                        .cloned()
                        .ok_or(Error::BadJb2("a matching shape that is not there"))?;
                    let (w, h) = (matching.width as i64 + dw, matching.height as i64 + dh);
                    if !(0..=MAX_DIMENSION).contains(&w) || !(0..=MAX_DIMENSION).contains(&h) {
                        return Err(Error::BadJb2("a refined shape with an impossible size"));
                    }
                    let (w, h) = (w as usize, h as usize);
                    self.charge(w, h)?;
                    let shape = self.refined_bitmap(w, h, &matching);
                    if record != 5 {
                        let (left, bottom) = self.relative_place(w, h);
                        self.place(&shape, left, bottom, None);
                    }
                    if record != 6 {
                        self.own.push(shape.cropped());
                    }
                }
                7 => {
                    let len = self.library_len();
                    if len == 0 {
                        return Err(Error::BadJb2("a copy of a shape in an empty library"));
                    }
                    let index = usize::try_from(self.num(Kind::MatchIndex, 0, len as i64 - 1)).unwrap_or(0);
                    let shape = self
                        .symbol(index)
                        .cloned()
                        .ok_or(Error::BadJb2("a matching shape that is not there"))?;
                    let (left, bottom) = self.relative_place(shape.width, shape.height);
                    self.place(&shape, left, bottom, Some(index));
                }
                8 => {
                    // data that is no symbol: a shape coded directly, at an absolute place, kept out of the library
                    let (w, h) = self.absolute_size()?;
                    let shape = self.direct_bitmap(w, h);
                    let left = self.num(Kind::Column, 1, self.page.width as i64);
                    let top = self.num(Kind::Row, 1, self.page.height as i64);
                    self.place(&shape, left, top - h as i64 + 1, None);
                }
                9 => self.ctx.reset_numbers(),
                10 => {
                    let length = self.num(Kind::CommentLength, 0, MAX_DIMENSION);
                    for _ in 0..length {
                        self.num(Kind::CommentOctet, 0, 255);
                    }
                }
                11 => return Ok(()),
                _ => return Err(Error::BadJb2("a record that does not exist")),
            }
        }
        Err(Error::BadJb2("more records than the limit"))
    }
}

/// Reads the head of a stream and makes the decoder: an optional "required dictionary" record with the number of shapes taken
/// from the shared dictionary (spec 11.4.10.1), then the start of data with the size of the picture.
fn start<'a, 'd>(data: &'a [u8], shared: Option<&'d Dictionary>, dictionary: bool) -> Result<Decoder<'a, 'd>, Error> {
    let mut decoder = Decoder {
        zp: ZpDecoder::new(data),
        ctx: Contexts::new(),
        base: &[],
        own: Vec::new(),
        shape_pixels: 0,
        page: Bitmap::default(),
        paint: true,
        blits: None,
        loose: Vec::new(),
        line: Line {
            first_left: 0, // the spec says column 1; real streams are coded from one column further left (found by comparing pages)
            first_bottom: 0,
            previous_right: 0,
            bottoms: Vec::new(),
        },
    };
    let mut record = decoder.num(Kind::RecordType, 0, 11);
    if record == 9 {
        let size = usize::try_from(decoder.num(Kind::DictionarySize, 0, MAX_DIMENSION)).unwrap_or(0);
        if size > 0 {
            let shared = shared.ok_or(Error::BadJb2("the stream needs a shared dictionary and none is given"))?;
            decoder.base = shared.symbols.get(..size).ok_or(Error::BadJb2(
                "the stream wants more shapes than the shared dictionary has",
            ))?;
        }
        record = decoder.num(Kind::RecordType, 0, 11);
    }
    if record != 0 {
        return Err(Error::BadJb2("no start-of-data record"));
    }
    let width = decoder.num(Kind::ImageSize, 0, MAX_DIMENSION);
    let height = decoder.num(Kind::ImageSize, 0, MAX_DIMENSION);
    if decoder.zp.decode_bit(&mut decoder.ctx.refinement_flag) {
        return Err(Error::BadJb2(
            "image refinement data is announced, which the spec says is never used",
        ));
    }
    let (width, height) = (
        usize::try_from(width).unwrap_or(0),
        usize::try_from(height).unwrap_or(0),
    );
    if !dictionary {
        if width.saturating_mul(height) > MAX_PAGE_PIXELS {
            return Err(Error::BadJb2("a page bigger than the limit"));
        }
        decoder.page = Bitmap::new(width, height);
        // the first shape is coded as if relative to a first shape whose lower left pixel is the upper left of the page (11.5.8)
        decoder.line = Line {
            first_left: 0, // the spec says column 1; real streams are coded from one column further left (found by comparing pages)
            first_bottom: height as i64,
            previous_right: 0,
            bottoms: vec![height as i64],
        };
    }
    Ok(decoder)
}

/// Decodes the `Sjbz` chunk of a page: the picture of its text (black on white). `shared` is the dictionary that the page includes,
/// when it does.
///
/// # Errors
/// [`Error::BadJb2`] for a stream that is cut, uses what the spec does not allow, or goes past a limit.
pub fn decode_image(data: &[u8], shared: Option<&Dictionary>) -> Result<Bitmap, Error> {
    let mut decoder = start(data, shared, false)?;
    decoder.records(false)?;
    Ok(decoder.page)
}

/// One shape put on a page: its upper left pixel at column `left`, row `top` (the top row is 0), and which of the shapes of the
/// [`PageShapes`] it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blit {
    pub left: i64,
    pub top: i64,
    pub shape: usize,
}

/// A page with how it was put together: the picture of its text, the shapes put on it in order, and the shapes themselves.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageShapes {
    pub mask: Bitmap,
    pub blits: Vec<Blit>,
    /// The shapes of the library (those of the shared dictionary it inherits, then its own), then those that were drawn and not
    /// kept; but the first `inherited` of them are not here when the page was made by [`decode_shapes`] (they are the first shapes of
    /// the dictionary that the caller has).
    pub shapes: Vec<Bitmap>,
    pub inherited: usize,
}

impl PageShapes {
    /// The shape that a blit names, from this page or from the dictionary it was made with.
    #[must_use]
    pub fn shape<'a>(&'a self, dictionary: Option<&'a Dictionary>, number: usize) -> Option<&'a Bitmap> {
        match number.checked_sub(self.inherited) {
            Some(own) => self.shapes.get(own),
            None => dictionary.and_then(|d| d.symbols.get(number)),
        }
    }
}

/// As [`decode_image`], and the blits as well: what is needed to give each shape its own colour (the `FGbz` chunk does).
///
/// # Errors
/// As [`decode_image`].
pub fn decode_page(data: &[u8], shared: Option<&Dictionary>) -> Result<PageShapes, Error> {
    let mut decoder = start(data, shared, false)?;
    decoder.blits = Some(Vec::new());
    decoder.records(false)?;
    let library = decoder.library_len();
    let mut blits = decoder.blits.take().unwrap_or_default();
    for blit in &mut blits {
        if blit.shape > usize::MAX / 2 {
            blit.shape = library + (usize::MAX - blit.shape); // a shape that is not in the library: after it
        }
    }
    let mut shapes = decoder.base.to_vec();
    shapes.append(&mut decoder.own);
    shapes.append(&mut decoder.loose);
    Ok(PageShapes {
        mask: decoder.page,
        blits,
        shapes,
        inherited: 0,
    })
}

/// As [`decode_page`], without drawing the page: the mask is empty (it has the size of the page and no pixels), and only the blits
/// and the shapes are made. For a picture that is made small, which counts the shapes' pixels in its own way and has no use for a
/// bitmap of the whole page.
///
/// # Errors
/// As [`decode_image`].
pub fn decode_shapes(data: &[u8], shared: Option<&Dictionary>) -> Result<PageShapes, Error> {
    let mut decoder = start(data, shared, false)?;
    decoder.paint = false;
    decoder.page.pixels = Vec::new();
    decoder.blits = Some(Vec::new());
    decoder.records(false)?;
    let library = decoder.library_len();
    let mut blits = decoder.blits.take().unwrap_or_default();
    for blit in &mut blits {
        if blit.shape > usize::MAX / 2 {
            blit.shape = library + (usize::MAX - blit.shape);
        }
    }
    let inherited = decoder.base.len();
    let mut shapes = std::mem::take(&mut decoder.own);
    shapes.append(&mut decoder.loose);
    Ok(PageShapes {
        mask: decoder.page,
        blits,
        shapes,
        inherited,
    })
}

/// Decodes a `Djbz` chunk: the shapes of a shared dictionary, those it inherits from another one first (spec 11.4.10.3).
///
/// # Errors
/// As [`decode_image`].
pub fn decode_dictionary(data: &[u8], inherited: Option<&Dictionary>) -> Result<Dictionary, Error> {
    let mut decoder = start(data, inherited, true)?;
    decoder.records(true)?;
    let mut symbols = decoder.base.to_vec();
    symbols.append(&mut decoder.own);
    Ok(Dictionary { symbols })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // Streams made by the JB2 encoder of djvu-rs (MIT) from the pages that `page` below rebuilds: a decoder written from the spec
    // reading what another program wrote.
    // LETTERS: a page of 170 x 96, 63 bytes
    const LETTERS: [u8; 63] = [
        0x80, 0xd4, 0xc1, 0x79, 0xe3, 0xaf, 0xd3, 0x4a, 0xd0, 0x47, 0x08, 0x8d, 0x46, 0xeb, 0x59, 0x5f, 0xc7, 0x51,
        0x69, 0x70, 0x34, 0x2c, 0xf6, 0xab, 0x15, 0xf6, 0x18, 0x54, 0x4e, 0xad, 0x38, 0x86, 0x8d, 0x27, 0x0c, 0x16,
        0x5d, 0x41, 0xc9, 0x8e, 0xfd, 0x37, 0x4d, 0x0c, 0x5f, 0xc1, 0x17, 0x01, 0xe7, 0x5a, 0xa8, 0x11, 0x49, 0x4c,
        0x29, 0xe2, 0x07, 0x72, 0x9e, 0x7c, 0x9a, 0x5b, 0x69,
    ];
    // BLOCK_AND_DOTS: a page of 70 x 60, 89 bytes
    const BLOCK_AND_DOTS: [u8; 89] = [
        0x81, 0xe2, 0x9e, 0xa1, 0x91, 0x90, 0x30, 0x3f, 0x96, 0x47, 0xa2, 0x91, 0xf4, 0x17, 0x15, 0xa5, 0xe9, 0x8d,
        0x63, 0xf2, 0xc5, 0x57, 0xb9, 0x9f, 0x1d, 0x64, 0xba, 0x92, 0x6c, 0x95, 0xd8, 0xd5, 0xe0, 0x3f, 0xac, 0xb1,
        0xcf, 0xca, 0xca, 0x1d, 0x24, 0x54, 0x54, 0x7b, 0xf3, 0xa4, 0x98, 0xd8, 0x05, 0xfe, 0xb0, 0x49, 0x4d, 0x40,
        0x12, 0x51, 0x32, 0x31, 0x26, 0x4c, 0xf1, 0xaa, 0x43, 0xb3, 0xf0, 0x0b, 0x24, 0x33, 0x22, 0x04, 0x4b, 0xea,
        0x99, 0xac, 0x33, 0xf5, 0x29, 0x90, 0x27, 0x7a, 0x19, 0x6c, 0x3b, 0x9c, 0x95, 0xda, 0x37, 0xda, 0x71,
    ];
    // EMPTY_PAGE: a page of 50 x 40, 4 bytes
    const EMPTY_PAGE: [u8; 4] = [0x82, 0xcc, 0x02, 0x7f];
    // ONE_DOT: a page of 9 x 7, 5 bytes
    const ONE_DOT: [u8; 5] = [0x8d, 0xb5, 0x05, 0x03, 0x4d];

    /// Five letter-like shapes (the same function made the input of LETTERS): the dots of each, relative to its upper left corner.
    fn glyph(kind: usize) -> Vec<(usize, usize)> {
        let mut dots = Vec::new();
        match kind % 5 {
            0 => {
                for y in 0..10 {
                    for x in 0..8 {
                        if !(2..8).contains(&y) || !(2..6).contains(&x) {
                            dots.push((x, y));
                        }
                    }
                }
            }
            1 => {
                for i in 0..9 {
                    dots.push((4, i));
                    dots.push((i, 4));
                }
            }
            2 => {
                for y in 0..10 {
                    dots.push((0, y));
                    dots.push((1, y));
                }
                for x in 0..7 {
                    dots.push((x, 8));
                    dots.push((x, 9));
                }
            }
            3 => {
                for i in 0..9 {
                    dots.push((i, i));
                    if i + 1 < 9 {
                        dots.push((i + 1, i));
                    }
                }
            }
            _ => {
                for x in 0..9 {
                    dots.push((x, 0));
                    dots.push((x, 1));
                }
                for y in 0..10 {
                    dots.push((4, y));
                }
            }
        }
        dots
    }

    fn noise(len: usize, mut seed: u64) -> Vec<u64> {
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed
            })
            .collect()
    }

    /// The pages that the vectors were made from, by the same functions as the generator.
    fn page(kind: usize) -> (usize, usize, Vec<u8>) {
        match kind {
            0 => {
                let (w, h) = (170, 96);
                let mut px = vec![0u8; w * h];
                for line in 0..3 {
                    for i in 0..6 {
                        let x0 = 6 + i * 26 + (i * 7 + line * 3) % 4;
                        let y0 = 8 + line * 30 + i % 2;
                        for (dx, dy) in glyph(i * 2 + line) {
                            px[(y0 + dy) * w + x0 + dx] = 1;
                        }
                    }
                }
                (w, h, px)
            }
            1 => {
                let (w, h) = (70, 60);
                let mut px = vec![0u8; w * h];
                for y in 10..40 {
                    for x in 8..30 {
                        px[y * w + x] = 1;
                    }
                }
                for i in 0..50 {
                    px[i * w + i + 5] = 1;
                }
                for (k, r) in noise(60, 0x1234_5678_9ABC_DEF1).into_iter().enumerate() {
                    px[(r as usize >> 8) % (w * h)] = (k % 2) as u8 | 1;
                }
                (w, h, px)
            }
            2 => (50, 40, vec![0; 50 * 40]),
            _ => {
                let mut px = vec![0u8; 9 * 7];
                px[3 * 9 + 4] = 1;
                (9, 7, px)
            }
        }
    }

    #[test]
    fn pages_that_another_encoder_wrote_come_back_as_the_pages_it_was_given() {
        for (name, kind, stream) in [
            ("letters", 0, &LETTERS[..]),
            ("a block, a diagonal and dots", 1, &BLOCK_AND_DOTS[..]),
            ("an empty page", 2, &EMPTY_PAGE[..]),
            ("one dot", 3, &ONE_DOT[..]),
        ] {
            let (w, h, pixels) = page(kind);
            let decoded = decode_image(stream, None).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!((decoded.width, decoded.height), (w, h), "{name}: the size");
            assert!(decoded.pixels == pixels, "{name}: the pixels differ");
        }
    }

    #[test]
    fn a_whole_number_is_always_within_its_limits_whatever_the_data() {
        for (low, high) in [
            (0, 11),
            (0, 262_142),
            (-262_143, 262_142),
            (1, 1),
            (3, 9),
            (-5, -1),
            (-1, 0),
            (7, 7),
            (0, 0),
            (-100, 100),
            (1, 4000),
        ] {
            for seed in 1..60_u64 {
                let data: Vec<u8> = noise(64, seed * 0x9E37).into_iter().map(|v| (v >> 16) as u8).collect();
                let mut zp = ZpDecoder::new(&data);
                let mut ctx = NumContext::new();
                for _ in 0..200 {
                    let n = decode_num(&mut zp, &mut ctx, low, high);
                    assert!((low..=high).contains(&n), "{n} is not in {low}..={high}");
                }
            }
        }
    }

    #[test]
    fn a_number_that_the_limits_settle_costs_no_bits() {
        // low == high: nothing is decoded, whatever the data
        let mut zp = ZpDecoder::new(&[0x12, 0x34, 0x56]);
        let before = zp.registers();
        assert_eq!(decode_num(&mut zp, &mut NumContext::new(), 7, 7), 7);
        assert_eq!(zp.registers(), before);
    }

    #[test]
    fn data_of_any_kind_is_an_error_or_a_picture_and_never_a_panic() {
        for seed in 1..80_u64 {
            for len in [0_usize, 1, 2, 5, 40, 300, 3000] {
                let data: Vec<u8> = noise(len, seed * 0x2545_F491)
                    .into_iter()
                    .map(|v| (v >> 20) as u8)
                    .collect();
                let _ = decode_image(&data, None);
                let _ = decode_dictionary(&data, None);
                let _ = decode_image(&data, Some(&Dictionary::default()));
            }
        }
        // the streams above, cut at every length and with each byte changed in turn
        for stream in [&LETTERS[..], &BLOCK_AND_DOTS[..]] {
            for cut in 0..stream.len() {
                let _ = decode_image(&stream[..cut], None);
            }
            for at in 0..stream.len() {
                let mut changed = stream.to_vec();
                changed[at] ^= 0xFF;
                let _ = decode_image(&changed, None);
                changed[at] ^= 0xFF ^ 0x01;
                let _ = decode_image(&changed, None);
            }
        }
    }

    #[test]
    fn a_stream_that_needs_a_dictionary_without_one_is_an_error_and_a_dictionary_stream_is_not_a_page() {
        // the vectors name no shared dictionary, so a dictionary given to them is ignored
        let with = decode_image(&LETTERS, Some(&Dictionary::default()));
        assert!(with.is_ok());
        // a page is not a dictionary: its records are not all allowed in one
        assert!(decode_dictionary(&LETTERS, None).is_err());
    }

    #[test]
    fn cropping_removes_empty_edges_and_blitting_puts_only_the_black_pixels() {
        let mut b = Bitmap::new(5, 4);
        b.pixels[4 + 2 * 5 - 5] = 1; // (4, 1)
        b.pixels[2 * 5 + 1] = 1; // (1, 2)
        let c = b.cropped();
        assert_eq!((c.width, c.height), (4, 2), "from x 1 to 4 and y 1 to 2");
        assert_eq!(c.pixels, vec![0, 0, 0, 1, 1, 0, 0, 0]);
        assert_eq!(
            Bitmap::new(3, 3).cropped(),
            Bitmap::default(),
            "an empty shape has no size"
        );
        let mut page = Bitmap::new(6, 6);
        page.blit(&c, 1, 1);
        assert_eq!(page.pixels.iter().filter(|&&p| p == 1).count(), 2);
        page.blit(&c, 5, 5); // mostly off the page: the part that is on it only
        assert_eq!(page.pixels[5 * 6 + 5], 0, "the corner of the shape is white");
    }
}
