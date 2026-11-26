//! The box of a page's content: the part of a PDF or DjVu page (a scan, as a rule) that has print on it, so that a reader can draw that
//! part larger and leave the white margins out. Given on request only; nothing here crops a page.
//!
//! The page is drawn small (about [`LOOK`] pixels on its longest side) and its print found there:
//! - the paper is the median brightness of the page; print is what is much darker than that ([`INK`]);
//! - a dark band along an edge (the scanner's shadow, the edge of the next page) is not print: rows or columns at an edge that are more
//!   than half dark, and a long thin mark touching an edge;
//! - specks (dust, a hole of the binding) are not print: the print is gathered in cells of [`CELL`] pixels, the cells that touch make a
//!   mark, and a mark with less than [`MIN_INK`] dark pixels is left out;
//! - a running head and a page number are a line at the top or the bottom set apart from the text by a wider gap than the lines of the
//!   text, and no taller than they are (a page number at the bottom: also narrow). The box is given with them or without them, as the
//!   reader asks ([`BoxOptions::heads`]).
//!
//! [`shared_content_box`] gives one box for the pages of the same side (odd or even) round a page: the median of each edge of up to five
//! such pages, then widened to take in all of this page, so that a page's print is never cut and the scale does not jump from page to
//! page. `docs/internal/formats.md` "The box of a scanned page's content" has the measurements.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use image::RgbaImage;

use crate::decode::DecodeError;

/// The longest side, in pixels, of the page as it is drawn to find its print.
const LOOK: usize = 1000;
/// Print is darker than this part of the paper's brightness.
const INK: f64 = 0.6;
/// A page whose paper is darker than this (of 255) is all print: its box is the whole page.
const DARK_PAPER: u8 = 96;
/// The side of a cell, in pixels of the page drawn at [`LOOK`].
const CELL: usize = 4;
/// A mark with fewer dark pixels than this is a speck.
const MIN_INK: u32 = 12;
/// A mark of at most this many cells that is more than [`SPECK_FILL`] % print is a speck.
const SPECK_CELLS: usize = 9;
const SPECK_FILL: u64 = 30;
/// A straight run of print in the outer part of the page longer than this part of its side is a line of the scan, not print.
const LONG_RUN: f64 = 0.08;
/// A mark that lies all within this part of a side from the edge is not print.
const RIM: f64 = 0.025;
/// The widest band of long lines down the page that is taken for the fold of a spread, a part of the page's width.
const FOLD_WIDTH: f64 = 0.05;
/// How far in from an edge (a part of the side) a dark band is looked for.
const EDGE: f64 = 0.1;
/// How many found boxes are kept, so that the neighbours of the next page are not looked at again.
const KEEP: usize = 64;

/// A part of a page, in the pixels of the page as `decode::decode_page` draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// What the box of a page takes in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxOptions {
    /// The room left round the print, a part of the page's shorter side (0.02 is 2 %); 0 to 0.25.
    pub padding: f64,
    /// Whether a running head at the top and a page number at the bottom are in the box.
    pub heads: bool,
}

impl Default for BoxOptions {
    fn default() -> Self {
        Self {
            padding: 0.02,
            heads: true,
        }
    }
}

/// Edges as parts of the page's width and height (0 to 1), whatever size it was drawn at.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Edges {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Edges {
    const WHOLE: Edges = Edges {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    fn union(self, other: Edges) -> Edges {
        Edges {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// The print found on a page: with its running head and page number, and without them (the same when it has none).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Found {
    all: Edges,
    body: Edges,
}

impl Found {
    fn edges(&self, heads: bool) -> Edges {
        if heads { self.all } else { self.body }
    }
}

/// The box of the print on page `page` (counted from 0; a number past the end is the last) of a PDF or DjVu file, in the pixels of the
/// page as `decode::decode_page` draws it, with the room `options` asks for round it. `None` for a page with nothing on it and for any
/// other file.
pub fn content_box(path: &Path, page: usize, options: BoxOptions) -> Result<Option<PageBox>, DecodeError> {
    let Some(size) = crate::decode::page_size(path, page) else {
        return Ok(None);
    };
    let ((width, height), count) = size?;
    let page = page.min(count.saturating_sub(1));
    Ok(found(path, page, (width, height))?
        .map(|found| to_box(found.edges(options.heads), (width, height), options.padding)))
}

/// One box for the pages of the same side as page `page` round it (`page` and up to two before and two after it, two apart): the median
/// of each of their edges, widened to take in all the print of `page` itself; in the pixels of `page` as `decode::decode_page` draws it,
/// with the room `options` asks for. `None` when `page` has nothing on it, and for any file but a PDF or DjVu file.
pub fn shared_content_box(path: &Path, page: usize, options: BoxOptions) -> Result<Option<PageBox>, DecodeError> {
    let Some(size) = crate::decode::page_size(path, page) else {
        return Ok(None);
    };
    let ((width, height), count) = size?;
    let page = page.min(count.saturating_sub(1));
    let Some(own) = found(path, page, (width, height))? else {
        return Ok(None);
    };
    let mut all = vec![own.edges(options.heads)];
    for other in [
        page.checked_sub(4),
        page.checked_sub(2),
        page.checked_add(2),
        page.checked_add(4),
    ]
    .into_iter()
    .flatten()
    .filter(|&other| other < count)
    {
        let other_size = match crate::decode::page_size(path, other) {
            Some(Ok((size, _))) => size,
            // a neighbour that cannot be read is left out: the page's own box still stands
            _ => continue,
        };
        if let Ok(Some(found)) = found(path, other, other_size) {
            all.push(found.edges(options.heads));
        }
    }
    let median = |edge: fn(&Edges) -> f64| {
        let mut values: Vec<f64> = all.iter().map(edge).collect();
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    let shared = Edges {
        left: median(|e| e.left),
        top: median(|e| e.top),
        right: median(|e| e.right),
        bottom: median(|e| e.bottom),
    }
    .union(own.edges(options.heads));
    Ok(Some(to_box(shared, (width, height), options.padding)))
}

/// The print of page `page`, kept for the next asks.
fn found(path: &Path, page: usize, (width, height): (u32, u32)) -> Result<Option<Found>, DecodeError> {
    static KEPT: Mutex<Vec<(Key, Option<Found>)>> = Mutex::new(Vec::new());
    let key = Key::of(path, page);
    if let Some(key) = &key
        && let Ok(kept) = KEPT.lock()
        && let Some((_, found)) = kept.iter().find(|(k, _)| k == key)
    {
        return Ok(*found);
    }
    let longest = width.max(height).max(1);
    let scale = (LOOK as f64 / f64::from(longest)).min(1.0);
    // a DjVu page is looked at from the first chunk of its background: the margins are the same within a pixel or two at this size
    // and it is two or three times less work on a scan (docs/internal/djvu.md, "The margins from a first glance")
    let (image, _) = if crate::djvu::is_djvu(path) {
        crate::djvu::render_page_glance(path, page, scale)?
    } else {
        crate::decode::render_page_scaled(path, page, scale)?
    };
    let found = find(&crate::decode::fit(image, LOOK, LOOK));
    if let Some(key) = key
        && let Ok(mut kept) = KEPT.lock()
    {
        kept.retain(|(k, _)| k != &key);
        kept.insert(0, (key, found));
        kept.truncate(KEEP);
    }
    Ok(found)
}

/// A page of a file as it is now: a file written again is looked at again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    page: usize,
}

impl Key {
    fn of(path: &Path, page: usize) -> Option<Key> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Key {
            path: path.to_path_buf(),
            len: meta.len(),
            modified: meta.modified().ok(),
            page,
        })
    }
}

/// `edges` in the pixels of a page of `width` x `height`, with `padding` (a part of the shorter side) round them, within the page.
fn to_box(edges: Edges, (width, height): (u32, u32), padding: f64) -> PageBox {
    let pad = padding.clamp(0.0, 0.25) * f64::from(width.min(height));
    let (w, h) = (f64::from(width), f64::from(height));
    let left = (edges.left * w - pad).floor().clamp(0.0, w);
    let top = (edges.top * h - pad).floor().clamp(0.0, h);
    let right = (edges.right * w + pad).ceil().clamp(left, w);
    let bottom = (edges.bottom * h + pad).ceil().clamp(top, h);
    PageBox {
        x: left as u32,
        y: top as u32,
        width: ((right - left) as u32)
            .max(1)
            .min(width.saturating_sub(left as u32).max(1)),
        height: ((bottom - top) as u32)
            .max(1)
            .min(height.saturating_sub(top as u32).max(1)),
    }
}

/// A mark: touching cells with print.
#[derive(Debug, Clone, Copy)]
struct Mark {
    /// In cells: the first and one past the last column and row.
    left: usize,
    top: usize,
    right: usize,
    bottom: usize,
    ink: u32,
}

/// The print on a page drawn small; `None` when there is none.
fn find(image: &RgbaImage) -> Option<Found> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w == 0 || h == 0 {
        return None;
    }
    let lum: Vec<u8> = image
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            let y = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
            // a pixel that lets the background through is over white paper
            ((y * u32::from(a) + 255 * (255 - u32::from(a))) / 255) as u8
        })
        .collect();
    let mut histogram = [0usize; 256];
    for &y in &lum {
        histogram[usize::from(y)] += 1;
    }
    let paper = {
        let mut seen = 0;
        let mut median = 255u8;
        for (value, &n) in histogram.iter().enumerate() {
            seen += n;
            if seen * 2 >= lum.len() {
                median = value as u8;
                break;
            }
        }
        median
    };
    if paper < DARK_PAPER {
        return Some(Found {
            all: Edges::WHOLE,
            body: Edges::WHOLE,
        });
    }
    let limit = (f64::from(paper) * INK) as u8;
    let mut ink: Vec<bool> = lum.iter().map(|&y| y < limit).collect();
    clear_edge_bands(&mut ink, w, h);

    let marks = marks(&ink, w, h);
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let kept: Vec<Mark> = marks
        .into_iter()
        .filter(|m| m.ink >= MIN_INK && !speck(m) && !edge_line(m, cw, ch) && !at_rim(m, cw, ch))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let edges_of = |marks: &[Mark]| -> Option<Edges> {
        let left = marks.iter().map(|m| m.left).min()?;
        let top = marks.iter().map(|m| m.top).min()?;
        let right = marks.iter().map(|m| m.right).max()?;
        let bottom = marks.iter().map(|m| m.bottom).max()?;
        Some(Edges {
            left: (left * CELL) as f64 / w as f64,
            top: (top * CELL) as f64 / h as f64,
            right: ((right * CELL).min(w)) as f64 / w as f64,
            bottom: ((bottom * CELL).min(h)) as f64 / h as f64,
        })
    };
    let all = edges_of(&kept)?;
    let body = without_heads(&kept, cw, ch)
        .and_then(|body| edges_of(&body))
        .unwrap_or(all);
    Some(Found { all, body })
}

/// Clears the rows and columns at an edge, within [`EDGE`] of it, that are more than half print: the dark band of a scan's edge.
fn clear_edge_bands(ink: &mut [bool], w: usize, h: usize) {
    let row = |ink: &[bool], y: usize| ink[y * w..(y + 1) * w].iter().filter(|&&d| d).count();
    let column = |ink: &[bool], x: usize| (0..h).filter(|&y| ink[y * w + x]).count();
    let (reach_y, reach_x) = (((h as f64) * EDGE) as usize, ((w as f64) * EDGE) as usize);
    let mut rows = Vec::new();
    for y in (0..reach_y).take_while(|&y| row(ink, y) * 2 > w) {
        rows.push(y);
    }
    for y in (h - reach_y..h).rev().take_while(|&y| row(ink, y) * 2 > w) {
        rows.push(y);
    }
    let mut columns = Vec::new();
    for x in (0..reach_x).take_while(|&x| column(ink, x) * 2 > h) {
        columns.push(x);
    }
    for x in (w - reach_x..w).rev().take_while(|&x| column(ink, x) * 2 > h) {
        columns.push(x);
    }
    for y in rows {
        ink[y * w..(y + 1) * w].fill(false);
    }
    for x in columns {
        for y in 0..h {
            ink[y * w + x] = false;
        }
    }
    clear_edge_runs(ink, w, h);
    clear_folds(ink, w, h);
}

/// Clears the fold of a spread and any other narrow band of long lines down the page: a few neighbouring columns (together less than
/// [`FOLD_WIDTH`] of the page) each with a run of print more than a quarter of the page long. Such a band touches the ends of the lines
/// on both sides of it and would join them into one mark from top to bottom. A dark picture is wider than that and stays.
fn clear_folds(ink: &mut [bool], w: usize, h: usize) {
    let long = h / 4;
    let longest = |ink: &[bool], x: usize| {
        let (mut best, mut run) = (0, 0);
        for y in 0..h {
            run = if ink[y * w + x] { run + 1 } else { 0 };
            best = best.max(run);
        }
        best
    };
    let lined: Vec<bool> = (0..w).map(|x| longest(ink, x) > long).collect();
    let narrow = ((w as f64) * FOLD_WIDTH) as usize;
    let mut x = 0;
    while x < w {
        if !lined[x] {
            x += 1;
            continue;
        }
        let start = x;
        while x < w && lined[x] {
            x += 1;
        }
        if x - start > narrow {
            continue;
        }
        // the band and 1 % of the page on each side of it (the soft edge of a shadow, the fold's broken ends), from the top to the
        // bottom: a letter cut there loses a sliver, which does not move the box
        let pad = (w / 100).max(2);
        for column in start.saturating_sub(pad)..(x + pad).min(w) {
            for y in 0..h {
                ink[y * w + column] = false;
            }
        }
    }
}

/// Clears, in the outer part of the page ([`EDGE`] of a side), the straight runs of print along that side longer than [`LONG_RUN`] of
/// it: the line of a scan's frame or of the paper's edge, which would otherwise join the text next to it into one mark. No letter is
/// that long; a rule under a running head is, and goes with it (the head's words stay).
fn clear_edge_runs(ink: &mut [bool], w: usize, h: usize) {
    let (reach_x, reach_y) = (((w as f64) * EDGE) as usize, ((h as f64) * EDGE) as usize);
    let (long_x, long_y) = (((w as f64) * LONG_RUN) as usize, ((h as f64) * LONG_RUN) as usize);
    // down the columns at the left and the right
    for x in (0..reach_x).chain(w - reach_x..w) {
        let mut y = 0;
        while y < h {
            if !ink[y * w + x] {
                y += 1;
                continue;
            }
            let start = y;
            while y < h && ink[y * w + x] {
                y += 1;
            }
            if y - start >= long_y {
                (start..y).for_each(|yy| ink[yy * w + x] = false);
            }
        }
    }
    // along the rows at the top and the bottom
    for y in (0..reach_y).chain(h - reach_y..h) {
        let row = &mut ink[y * w..(y + 1) * w];
        let mut x = 0;
        while x < w {
            if !row[x] {
                x += 1;
                continue;
            }
            let start = x;
            while x < w && row[x] {
                x += 1;
            }
            if x - start >= long_x {
                row[start..x].fill(false);
            }
        }
    }
}

/// The marks of the print: cells of [`CELL`] pixels with any print, joined to the cells round them.
fn marks(ink: &[bool], w: usize, h: usize) -> Vec<Mark> {
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![0u32; cw * ch];
    for y in 0..h {
        for x in 0..w {
            if ink[y * w + x] {
                cells[(y / CELL) * cw + x / CELL] += 1;
            }
        }
    }
    let mut seen = vec![false; cw * ch];
    let mut marks = Vec::new();
    let mut stack = Vec::new();
    for start in 0..cells.len() {
        if cells[start] == 0 || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut mark = Mark {
            left: usize::MAX,
            top: usize::MAX,
            right: 0,
            bottom: 0,
            ink: 0,
        };
        while let Some(cell) = stack.pop() {
            let (cx, cy) = (cell % cw, cell / cw);
            mark.left = mark.left.min(cx);
            mark.top = mark.top.min(cy);
            mark.right = mark.right.max(cx + 1);
            mark.bottom = mark.bottom.max(cy + 1);
            mark.ink = mark.ink.saturating_add(cells[cell]);
            for ny in cy.saturating_sub(1)..(cy + 2).min(ch) {
                for nx in cx.saturating_sub(1)..(cx + 2).min(cw) {
                    let next = ny * cw + nx;
                    if cells[next] > 0 && !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        marks.push(mark);
    }
    marks
}

/// A small solid blot: a speck of dust or a hole, not a letter (whose strokes leave most of its cells' area white).
fn speck(m: &Mark) -> bool {
    let cells = (m.right - m.left) * (m.bottom - m.top);
    cells <= SPECK_CELLS && u64::from(m.ink) * 100 > (cells * CELL * CELL) as u64 * SPECK_FILL
}

/// A mark all within the rim of the page ([`RIM`] of a side): the broken edge of the paper, a dark corner, a hole; print so close to
/// the edge, when there is any, is cut by the scan already, and the box's room takes most of it in again.
fn at_rim(m: &Mark, cw: usize, ch: usize) -> bool {
    let (rim_x, rim_y) = (((cw as f64) * RIM).ceil() as usize, ((ch as f64) * RIM).ceil() as usize);
    m.right <= rim_x || m.left >= cw.saturating_sub(rim_x) || m.bottom <= rim_y || m.top >= ch.saturating_sub(rim_y)
}

/// A long thin mark: in the outer part of the page ([`EDGE`] of a side) and along it, at least a quarter of the side long (a shadow,
/// the edge of the paper, a line of the scan); anywhere, at least a third of the page long (the fold between the pages of a spread). A
/// rule between columns of text goes too, which leaves the box as it is, the text round it being in it.
fn edge_line(m: &Mark, cw: usize, ch: usize) -> bool {
    let (width, height) = (m.right - m.left, m.bottom - m.top);
    let (reach_x, reach_y) = (
        ((cw as f64) * EDGE).ceil() as usize,
        ((ch as f64) * EDGE).ceil() as usize,
    );
    let at_side = m.right <= reach_x || m.left >= cw.saturating_sub(reach_x);
    let at_end = m.bottom <= reach_y || m.top >= ch.saturating_sub(reach_y);
    let (thin_across, thin_down) = (width * 100 < cw * 3, height * 100 < ch * 3);
    let down = thin_across && (height * 3 > ch || (at_side && height * 4 > ch));
    let across = thin_down && (width * 3 > cw || (at_end && width * 4 > cw));
    down || across
}

/// The marks without a running head at the top and a page number at the bottom; `None` when the page has neither.
fn without_heads(marks: &[Mark], cw: usize, ch: usize) -> Option<Vec<Mark>> {
    // the lines: runs of rows (of cells) that some mark covers
    let mut covered = vec![false; ch];
    for m in marks {
        covered[m.top..m.bottom].fill(true);
    }
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut y = 0;
    while y < ch {
        if covered[y] {
            let start = y;
            while y < ch && covered[y] {
                y += 1;
            }
            lines.push((start, y));
        } else {
            y += 1;
        }
    }
    if lines.len() < 4 {
        return None;
    }
    let inner = &lines[1..lines.len() - 1];
    let middle = |mut values: Vec<usize>| {
        values.sort_unstable();
        values[values.len() / 2]
    };
    let line_height = middle(inner.iter().map(|(a, b)| b - a).collect());
    // at least two inner lines, so at least one gap between them
    let gap = middle(inner.windows(2).map(|p| p[1].0 - p[0].1).collect());
    // set apart: a gap twice the text's and at least 1.2 % of the page; no taller than a line and a third of the text
    let apart = |space: usize| space * 1000 >= ch * 12 && space >= gap * 2;
    let thin = |(a, b): (usize, usize)| (b - a) * 3 <= line_height * 4;
    let (first, last) = (lines[0], lines[lines.len() - 1]);
    let head = thin(first) && apart(lines[1].0 - first.1);
    let foot = thin(last) && apart(last.0 - lines[lines.len() - 2].1) && {
        // a page number is narrow; a line of a footnote is not
        let in_foot = marks.iter().filter(|m| m.top >= last.0);
        let left = in_foot.clone().map(|m| m.left).min().unwrap_or(0);
        let right = in_foot.map(|m| m.right).max().unwrap_or(cw);
        (right - left) * 10 <= cw * 4
    };
    if !head && !foot {
        return None;
    }
    let body: Vec<Mark> = marks
        .iter()
        .filter(|m| !(head && m.bottom <= first.1) && !(foot && m.top >= last.0))
        .copied()
        .collect();
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that finds no print where it drew some should stop at once
mod tests {
    use super::*;
    use image::Rgba;

    const PAPER: Rgba<u8> = Rgba([235, 230, 220, 255]);
    const PRINT: Rgba<u8> = Rgba([30, 30, 30, 255]);

    fn page(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, PAPER)
    }

    fn fill(image: &mut RgbaImage, (x, y, w, h): (u32, u32, u32, u32), colour: Rgba<u8>) {
        for yy in y..y + h {
            for xx in x..x + w {
                image.put_pixel(xx, yy, colour);
            }
        }
    }

    /// Lines of "words" (blocks with spaces between them) from `top`, `count` lines `pitch` apart, from `left` to `right`.
    fn text(image: &mut RgbaImage, left: u32, right: u32, top: u32, count: u32, pitch: u32, height: u32) {
        for line in 0..count {
            let mut x = left;
            while x + 30 <= right {
                fill(image, (x, top + line * pitch, 30, height), PRINT);
                x += 40;
            }
        }
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.012
    }

    #[test]
    fn a_blank_page_has_no_box() {
        assert_eq!(find(&page(700, 1000)), None);
    }

    #[test]
    fn the_box_holds_the_text_and_leaves_the_margins_and_specks_out() {
        let mut image = page(700, 1000);
        text(&mut image, 100, 600, 150, 30, 24, 12);
        // dust in the margins
        fill(&mut image, (20, 40, 2, 2), PRINT);
        fill(&mut image, (660, 900, 3, 2), PRINT);
        let found = find(&image).expect("print");
        assert_eq!(found.all, found.body);
        let e = found.all;
        assert!(near(e.left, 100.0 / 700.0) && near(e.right, 570.0 / 700.0), "{e:?}");
        assert!(
            near(e.top, 0.15) && near(e.bottom, (150.0 + 29.0 * 24.0 + 12.0) / 1000.0),
            "{e:?}"
        );
    }

    #[test]
    fn a_dark_band_and_a_shadow_at_the_edges_are_not_print() {
        let mut image = page(700, 1000);
        text(&mut image, 100, 600, 150, 30, 24, 12);
        // the edge of the next page down the left side, and a thin shadow along the bottom that does not reach the corners
        fill(&mut image, (0, 0, 25, 1000), PRINT);
        fill(&mut image, (80, 992, 560, 8), PRINT);
        let e = find(&image).expect("print").all;
        assert!(near(e.left, 100.0 / 700.0), "{e:?}");
        assert!(e.bottom < 0.9, "{e:?}");
    }

    #[test]
    fn a_frame_line_of_the_scan_close_to_the_text_is_not_print() {
        let mut image = page(700, 1000);
        text(&mut image, 64, 600, 150, 30, 24, 12);
        // a line down the left, two pixels from the words, which would join them into one mark, and one across the top
        fill(&mut image, (60, 20, 2, 960), PRINT);
        fill(&mut image, (40, 30, 620, 2), PRINT);
        let e = find(&image).expect("print").all;
        assert!(near(e.left, 64.0 / 700.0) && near(e.top, 0.15), "{e:?}");
    }

    #[test]
    fn the_fold_of_a_spread_touching_the_lines_does_not_stretch_the_box_to_the_ends_of_the_page() {
        let mut image = page(1000, 700);
        text(&mut image, 100, 480, 100, 20, 24, 12);
        text(&mut image, 520, 900, 100, 20, 24, 12);
        // the dark fold from the top to the bottom, touching the ends of the lines on both sides
        fill(&mut image, (478, 0, 44, 700), PRINT);
        let e = find(&image).expect("print").all;
        assert!(
            near(e.top, 100.0 / 700.0) && near(e.bottom, (100.0 + 19.0 * 24.0 + 12.0) / 700.0),
            "{e:?}"
        );
        assert!(near(e.left, 0.1), "{e:?}");
    }

    #[test]
    fn a_running_head_and_a_page_number_are_told_apart_from_the_text() {
        let mut image = page(700, 1000);
        // a head of small type, the text, a number below
        text(&mut image, 100, 600, 60, 1, 24, 10);
        text(&mut image, 100, 600, 150, 30, 24, 12);
        fill(&mut image, (330, 930, 30, 12), PRINT);
        let found = find(&image).expect("print");
        assert!(
            near(found.all.top, 0.06) && near(found.all.bottom, 0.942),
            "{:?}",
            found.all
        );
        assert!(
            near(found.body.top, 0.15) && near(found.body.bottom, (150.0 + 29.0 * 24.0 + 12.0) / 1000.0),
            "{:?}",
            found.body
        );
    }

    #[test]
    fn a_title_larger_than_the_text_and_a_footnote_stay_in_the_body() {
        let mut image = page(700, 1000);
        // a chapter's title in large type set apart above the text, a one-line footnote as wide as the text below it
        text(&mut image, 200, 500, 60, 1, 24, 30);
        text(&mut image, 100, 600, 150, 30, 24, 12);
        text(&mut image, 100, 600, 930, 1, 24, 10);
        let found = find(&image).expect("print");
        assert_eq!(found.all, found.body);
    }

    #[test]
    fn a_dark_page_is_all_print() {
        let image = RgbaImage::from_pixel(300, 400, Rgba([20, 20, 20, 255]));
        assert_eq!(find(&image).map(|f| f.all), Some(Edges::WHOLE));
    }

    #[test]
    fn the_box_in_pixels_has_its_room_and_stays_within_the_page() {
        let edges = Edges {
            left: 0.1,
            top: 0.2,
            right: 0.9,
            bottom: 0.8,
        };
        assert_eq!(
            to_box(edges, (1000, 2000), 0.0),
            PageBox {
                x: 100,
                y: 400,
                width: 800,
                height: 1200
            }
        );
        assert_eq!(
            to_box(edges, (1000, 2000), 0.02),
            PageBox {
                x: 80,
                y: 380,
                width: 840,
                height: 1240
            }
        );
        assert_eq!(
            to_box(Edges::WHOLE, (1000, 2000), 0.25),
            PageBox {
                x: 0,
                y: 0,
                width: 1000,
                height: 2000
            }
        );
    }
}
