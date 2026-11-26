//! The column of a book's body text: in a textbook whose pages keep a wide margin for notes, icons and headings (often outside, so it
//! changes sides from page to page), the part of a page the body's lines keep to, so that a reader can show that part larger. Given on
//! request only, for a book the reader chose; nothing here crops a page by itself. A PDF with text only: a scan has none to judge by.
//!
//! How it is found (`docs/internal/formats.md` "The column of a book's body text" has what was measured):
//! - pages spread over the book are read ([`SAMPLE`] at most); the words of each are gathered into pieces of lines, a row of words split
//!   where a gap is wider than two of the row's heights (a note in the margin beside a line of the body);
//! - the body's lines are the long ones ([`LONG_CHARS`] letters, [`LONG_WIDTH`] of the page); a note, an icon's text, a heading in the
//!   margin are too short or too rare to count;
//! - each page with enough long lines has its own left edge (the edge most of its lines share). The pages fall into one layout, or two
//!   whose edges are far apart (the margin outside, on the left of one page and the right of the next). Which layout a page has is
//!   told by its own lines, not by its number: a book where pages were taken out or put in does not keep to odd and even;
//! - a layout's column: the edge most of its lines share on the left and on the right, the top and bottom of its lines, a little room
//!   round them ([`PAD`]) and more below ([`BELOW`]: what hangs under the last line); a reader may tune it larger or smaller;
//! - no column is given when the text is about as wide as the page, when nothing stands beside the column (cropping the margins does as
//!   much), and when the pages' text does not keep to one column (two columns of text, a magazine's free layout, two pages scanned as one).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::content::PageBox;
use crate::decode::DecodeError;
use crate::pdf_text::PageText;

/// How many pages, spread over the book, are read to find the column.
const SAMPLE: usize = 80;
/// A piece of a line with at least this many letters, and at least [`LONG_WIDTH`] of the page wide, is a line of the body.
const LONG_CHARS: usize = 25;
const LONG_WIDTH: f64 = 0.35;
/// The width of the window in which most lines' edges are looked for, a part of the page's width.
const CROWD: f64 = 0.03;
/// How far from the column's edge a line may start or end and still keep to it (an indent, a ragged right), a part of the width.
const SLACK: f64 = 0.04;
/// Two layouts are told apart when their left edges are at least this far apart, and each has at least [`LAYOUT_SHARE`] of the pages.
const LAYOUT_GAP: f64 = 0.06;
const LAYOUT_SHARE: f64 = 0.15;
/// A column wider than this part of the page is not worth showing larger.
const MOST_WIDTH: f64 = 0.75;
/// Cropping the margins keeps the box of all the text: the column is worth it when that box is this many times wider.
const LEAST_GAIN: f64 = 1.15;
/// A page keeps to the column when this part of its letters is in it, and the book when this part of its pages do.
const PAGE_FIT: f64 = 0.8;
const BOOK_FIT: f64 = 0.75;
/// The most of the pages, a part, that may keep to no column (exercises in two columns, tables) and the book still have one.
const MOST_ODD: f64 = 0.35;
/// A page is judged by its letters when it has this many.
const PAGE_LETTERS: usize = 300;
/// The part of the pages whose lines the column's top and bottom hold whole (the rest: a page with a line set lower than all others).
const PAGES_HELD: f64 = 0.95;
/// The room left round the column, a part of the page's width (and of its height, the half of it).
const PAD: f64 = 0.015;
/// More room below the column's lowest lines, a part of the page's height: what hangs under a line (a fraction's denominator, a
/// subscript, a formula set lower) on a page the pages read to find the column did not show. Calculus for Dummies: the lowest words of
/// 17 pages in 377 were below the column without it, of 2 with it.
const BELOW: f64 = 0.02;
/// The most a reader's tuning moves each side of the column, a part of the page.
const MOST_TUNE: f64 = 0.1;
/// How many books' columns are kept.
const KEEP: usize = 8;

/// Why a book has no column to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoColumn {
    /// Not a PDF, or a PDF with no text (a scan), or too little of it to judge by.
    NoText,
    /// The text is about as wide as the page.
    AsWide,
    /// Nothing stands beside the column: cropping the margins does as much.
    NothingBeside,
    /// The pages' text does not keep to one column.
    NotOneColumn,
}

/// The column of a book: one layout, or two (the margin on one side, then the other).
#[derive(Debug, Clone, PartialEq)]
pub struct Columns {
    layouts: Vec<Edges>,
    /// The layout most pages of each side (even, odd) have: for a page whose own lines are too few to tell.
    by_side: [usize; 2],
}

/// Edges as parts of the page's width and height (0 to 1).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Edges {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

/// A piece of a line, in parts of the page's width and height.
#[derive(Debug, Clone, Copy)]
struct Piece {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    letters: usize,
}

impl Piece {
    fn long(&self) -> bool {
        self.letters >= LONG_CHARS && self.right - self.left >= LONG_WIDTH
    }
}

/// The column of the book at `path`, found once and kept while the file stays as it is.
pub fn columns(path: &Path) -> Result<Result<Columns, NoColumn>, DecodeError> {
    static KEPT: Mutex<Vec<(Key, Result<Columns, NoColumn>)>> = Mutex::new(Vec::new());
    let key = Key::of(path);
    if let Some(key) = &key
        && let Ok(kept) = KEPT.lock()
        && let Some((_, found)) = kept.iter().find(|(k, _)| k == key)
    {
        return Ok(found.clone());
    }
    let found = find(path)?;
    if let Some(key) = key
        && let Ok(mut kept) = KEPT.lock()
    {
        kept.retain(|(k, _)| k != &key);
        kept.insert(0, (key, found.clone()));
        kept.truncate(KEEP);
    }
    Ok(found)
}

/// The column of page `page` (counted from 0) of the book at `path`, in the pixels of the page as `decode::decode_page` draws it. When
/// `shape` (a width to a height) is given, the box is made that shape where the page has room for it: wider from the margin's side
/// first (the margin seen as far as the screen allows), or taller; within the page. `tune` is the reader's own adjustment: a part of the
/// page's width (on the left and right) and height (above and below) added on every side, taken in when below 0 (at most
/// [`MOST_TUNE`]; the column never narrower or lower than a tenth of the page), before the box is made the screen's shape.
pub fn column_box(
    path: &Path,
    page: usize,
    shape: Option<f64>,
    tune: f64,
) -> Result<Result<PageBox, NoColumn>, DecodeError> {
    let book = match columns(path)? {
        Ok(book) => book,
        Err(why) => return Ok(Err(why)),
    };
    let Some(size) = crate::decode::page_size(path, page) else {
        return Ok(Err(NoColumn::NoText));
    };
    let ((width, height), _) = size?;
    // the page's own lines tell its layout; a page with too few of them goes by its side
    let layout = if book.layouts.len() > 1 {
        let own = crate::pdf_text::page_words(path, page)
            .ok()
            .and_then(|(text, size)| book.layout_of(&pieces(&text, size)));
        own.unwrap_or(book.by_side[page % 2])
    } else {
        0
    };
    let Some(edges) = book.layouts.get(layout).or(book.layouts.first()) else {
        return Ok(Err(NoColumn::NoText));
    };
    let edges = tuned(*edges, tune);
    let edges = match shape {
        Some(shape) if shape.is_finite() && shape > 0.0 => {
            shaped(edges, f64::from(width) / f64::from(height.max(1)), shape)
        }
        _ => edges,
    };
    Ok(Ok(to_box(edges, (width, height))))
}

impl Columns {
    /// The layout whose column most of these pieces' letters keep to; `None` when there are too few long lines to tell.
    fn layout_of(&self, pieces: &[Piece]) -> Option<usize> {
        if pieces.iter().filter(|p| p.long()).count() < 3 {
            return None;
        }
        let within = |e: &Edges| {
            pieces
                .iter()
                .filter(|p| p.long() && p.left >= e.left - SLACK && p.right <= e.right + SLACK)
                .map(|p| p.letters)
                .sum::<usize>()
        };
        self.layouts
            .iter()
            .enumerate()
            .max_by_key(|(_, e)| within(e))
            .map(|(i, _)| i)
    }
}

/// The column found from the pages spread over the book.
fn find(path: &Path) -> Result<Result<Columns, NoColumn>, DecodeError> {
    if !crate::pdf::is_pdf(path) {
        return Ok(Err(NoColumn::NoText));
    }
    let Some(size) = crate::decode::page_size(path, 0) else {
        return Ok(Err(NoColumn::NoText));
    };
    let (_, count) = size?;
    // an odd step, so that pages of both sides are read
    let step = (count / SAMPLE).max(1) | 1;
    let wanted: Vec<usize> = (0..count).step_by(step).take(SAMPLE).collect();
    let read = crate::pdf_text::pages_words(path, &wanted)?;
    let pages: Vec<(usize, Vec<Piece>)> = read
        .iter()
        .map(|(number, text, size)| (*number, pieces(text, *size)))
        .collect();
    Ok(judge(&pages))
}

/// The column of the pages `pages` (each its number and its pieces of lines), or why there is none.
fn judge(pages: &[(usize, Vec<Piece>)]) -> Result<Columns, NoColumn> {
    let all_long: usize = pages.iter().map(|(_, p)| p.iter().filter(|p| p.long()).count()).sum();
    if all_long < 20 {
        return Err(NoColumn::NoText);
    }
    // each page with enough long lines: its own left edge
    let mut own: Vec<(usize, f64)> = pages
        .iter()
        .filter_map(|(number, pieces)| {
            let lefts: Vec<f64> = pieces.iter().filter(|p| p.long()).map(|p| p.left).collect();
            (lefts.len() >= 3).then(|| (*number, crowd(lefts, true)))
        })
        .collect();
    own.sort_by(|a, b| a.1.total_cmp(&b.1));
    // one layout, or two split at the widest gap between the pages' edges that leaves enough pages on each side (a few odd pages at
    // either end, a title or a table, make wider gaps of their own)
    let mut split = None;
    let least = ((own.len() as f64 * LAYOUT_SHARE).ceil() as usize).max(2);
    if own.len() >= 2 * least {
        let (at, gap) = own
            .windows(2)
            .enumerate()
            .map(|(i, w)| (i + 1, w[1].1 - w[0].1))
            .filter(|(at, _)| *at >= least && own.len() - at >= least)
            .fold((0, 0.0), |best, now| if now.1 > best.1 { now } else { best });
        if gap >= LAYOUT_GAP
            && let Some((_, edge)) = own.get(at)
        {
            split = Some(edge - gap / 2.0);
        }
    }
    let layout_of_page = |number: usize| -> usize {
        match (split, own.iter().find(|(n, _)| *n == number)) {
            (Some(at), Some((_, left))) if *left > at => 1,
            _ => 0,
        }
    };
    let groups = if split.is_some() { 2 } else { 1 };
    // each layout's column; a layout of a few pages that keep to no column (exercises in two columns, a table) is let go, and the
    // book judged by the other, whose column those pages then have
    let mut layouts = Vec::new();
    // how many pages of each side (even, odd) each layout kept has
    let mut sides: Vec<[usize; 2]> = Vec::new();
    let mut failed = None;
    for group in 0..groups {
        let members: Vec<(usize, &Vec<Piece>)> = pages
            .iter()
            .filter(|(number, _)| own.iter().any(|(n, _)| n == number) && layout_of_page(*number) == group)
            .map(|(n, p)| (*n, p))
            .collect();
        let only: Vec<&Vec<Piece>> = members.iter().map(|(_, p)| *p).collect();
        match column_of(&only) {
            Ok(edges) => {
                let mut count = [0usize; 2];
                for (number, _) in &members {
                    count[number % 2] += 1;
                }
                sides.push(count);
                layouts.push(edges);
            }
            Err(why) => failed = Some((why, members.len())),
        }
    }
    if let Some((why, count)) = failed
        && (layouts.is_empty() || count as f64 > own.len() as f64 * MOST_ODD)
    {
        return Err(why);
    }
    // the box of all the text, as cropping the margins would keep it (its rarest 1% left out), against the column
    let every: Vec<&Piece> = pages.iter().flat_map(|(_, p)| p.iter()).collect();
    let all_left = percentile(every.iter().map(|p| p.left).collect(), 0.01);
    let all_right = percentile(every.iter().map(|p| p.right).collect(), 0.99);
    let widest = layouts.iter().map(|e| e.right - e.left).fold(0.0, f64::max);
    if widest > MOST_WIDTH + 2.0 * PAD {
        return Err(NoColumn::AsWide);
    }
    if (all_right - all_left) < widest * LEAST_GAIN {
        return Err(NoColumn::NothingBeside);
    }
    let side = |s: usize| match (sides.first(), sides.get(1)) {
        (Some(first), Some(second)) if second[s] > first[s] => 1,
        _ => 0,
    };
    Ok(Columns {
        layouts,
        by_side: [side(0), side(1)],
    })
}

/// The column of one layout's pages: the edges most of their long lines share, the top and bottom of those lines, with room round them.
fn column_of(pages: &[&Vec<Piece>]) -> Result<Edges, NoColumn> {
    let long: Vec<&Piece> = pages.iter().flat_map(|p| p.iter()).filter(|p| p.long()).collect();
    if long.len() < 10 {
        return Err(NoColumn::NoText);
    }
    let left = crowd(long.iter().map(|p| p.left).collect(), true);
    let right = crowd(long.iter().map(|p| p.right).collect(), false);
    if right <= left {
        return Err(NoColumn::NotOneColumn);
    }
    // a second column of text: many long lines starting well inside the first
    let second = long.iter().filter(|p| p.left > left + 0.3).count();
    let aligned = long.iter().filter(|p| (p.left - left).abs() <= SLACK).count();
    if second * 4 > long.len() || aligned * 2 < long.len() {
        return Err(NoColumn::NotOneColumn);
    }
    // most pages' text keeps to the column
    let judged: Vec<(usize, usize)> = pages
        .iter()
        .map(|p| {
            let all: usize = p.iter().map(|p| p.letters).sum();
            let within: usize = p
                .iter()
                .filter(|p| p.left >= left - SLACK && p.right <= right + SLACK)
                .map(|p| p.letters)
                .sum();
            (within, all)
        })
        .filter(|(_, all)| *all >= PAGE_LETTERS)
        .collect();
    let fitting = judged
        .iter()
        .filter(|(within, all)| *within as f64 >= *all as f64 * PAGE_FIT)
        .count();
    if judged.is_empty() || (fitting as f64) < judged.len() as f64 * BOOK_FIT {
        return Err(NoColumn::NotOneColumn);
    }
    // the top and bottom: each page's highest and lowest line of the column (a short last line of a paragraph too), then the edge
    // that holds them on all but the rarest pages, one for the whole book (the scale does not change from page to page)
    let (mut tops, mut bottoms) = (Vec::new(), Vec::new());
    for page in pages {
        let inside = page.iter().filter(|p| {
            let middle = (p.left + p.right) / 2.0;
            middle >= left && middle <= right
        });
        let (high, low) = inside.fold((f64::MAX, f64::MIN), |(high, low), p| {
            (high.min(p.top), low.max(p.bottom))
        });
        if high <= low {
            tops.push(high);
            bottoms.push(low);
        }
    }
    let top = percentile(tops, 1.0 - PAGES_HELD);
    let bottom = percentile(bottoms, PAGES_HELD);
    Ok(Edges {
        left: (left - PAD).max(0.0),
        top: (top - PAD / 2.0).max(0.0),
        right: (right + PAD).min(1.0),
        bottom: (bottom + PAD / 2.0 + BELOW).min(1.0),
    })
}

/// The words of a page gathered into pieces of lines, in parts of the page (`size`, in the pixels of the words).
fn pieces(text: &PageText, size: (u32, u32)) -> Vec<Piece> {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let mut words: Vec<_> = text
        .words
        .iter()
        .filter(|w| w.rect.bottom > w.rect.top && w.rect.right > w.rect.left)
        .collect();
    words.sort_by_key(|w| (w.rect.top + w.rect.bottom) / 2);
    // rows: a word whose middle is within half the row's height of the row's middle
    let mut rows: Vec<Vec<&peekel_djvu::text::Word>> = Vec::new();
    for word in words {
        let middle = (word.rect.top + word.rect.bottom) / 2;
        let height = word.rect.bottom - word.rect.top;
        if let Some(row) = rows.last_mut() {
            let top = row.iter().map(|x| x.rect.top).min().unwrap_or(word.rect.top);
            let bottom = row.iter().map(|x| x.rect.bottom).max().unwrap_or(word.rect.bottom);
            if (middle - (top + bottom) / 2).abs() * 2 <= height.min(bottom - top).max(1) {
                row.push(word);
                continue;
            }
        }
        rows.push(vec![word]);
    }
    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by_key(|w| w.rect.left);
        let mut heights: Vec<i32> = row.iter().map(|w| w.rect.bottom - w.rect.top).collect();
        heights.sort_unstable();
        let height = heights.get(heights.len() / 2).copied().unwrap_or(1).max(1);
        let mut current: Option<(i32, i32, i32, i32, usize)> = None;
        for word in row {
            let letters = word.text.chars().count();
            let r = word.rect;
            match current.as_mut() {
                Some(c) if r.left - c.1 <= height * 2 => {
                    c.1 = c.1.max(r.right);
                    c.2 = c.2.min(r.top);
                    c.3 = c.3.max(r.bottom);
                    c.4 += letters + 1;
                }
                _ => {
                    out.extend(current.take());
                    current = Some((r.left, r.right, r.top, r.bottom, letters));
                }
            }
        }
        out.extend(current);
    }
    out.into_iter()
        .map(|(left, right, top, bottom, letters)| Piece {
            left: f64::from(left) / w,
            right: f64::from(right) / w,
            top: f64::from(top) / h,
            bottom: f64::from(bottom) / h,
            letters,
        })
        .collect()
}

/// The edge most of `values` share: the window of [`CROWD`] with the most of them, then the outermost of those (the low one for a
/// left edge, the high one for a right).
fn crowd(mut values: Vec<f64>, low: bool) -> f64 {
    values.sort_by(f64::total_cmp);
    let (mut best, mut at, mut from) = (0, 0, 0);
    for i in 0..values.len() {
        while values[i] - values[from] > CROWD {
            from += 1;
        }
        if i + 1 - from > best {
            best = i + 1 - from;
            at = i;
        }
    }
    let Some(&high) = values.get(at) else {
        return if low { 0.0 } else { 1.0 };
    };
    let inside: Vec<f64> = values
        .iter()
        .copied()
        .filter(|v| *v >= high - CROWD && *v <= high)
        .collect();
    percentile(inside, if low { 0.05 } else { 0.95 })
}

fn percentile(mut values: Vec<f64>, part: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * part).round() as usize]
}

/// `edges` with `tune` of the page added on every side (taken in when below 0), within the page; as they were when that would leave
/// them narrower or lower than a tenth of the page.
fn tuned(edges: Edges, tune: f64) -> Edges {
    if !tune.is_finite() {
        return edges;
    }
    let tune = tune.clamp(-MOST_TUNE, MOST_TUNE);
    let out = Edges {
        left: (edges.left - tune).max(0.0),
        top: (edges.top - tune).max(0.0),
        right: (edges.right + tune).min(1.0),
        bottom: (edges.bottom + tune).min(1.0),
    };
    if out.right - out.left < 0.1 || out.bottom - out.top < 0.1 {
        edges
    } else {
        out
    }
}

/// `edges` of a page of shape `page` (its width to its height) made `shape` where the page has room: wider from the side with more
/// room first (the margin's), or taller from both sides alike.
fn shaped(edges: Edges, page: f64, shape: f64) -> Edges {
    let (w, h) = ((edges.right - edges.left) * page, edges.bottom - edges.top);
    if h <= 0.0 || w <= 0.0 {
        return edges;
    }
    let mut out = edges;
    if w / h < shape {
        // wider, in parts of the page's width: from the side with more room (the margin's) first, then the other
        let want = ((h * shape - w) / page).max(0.0);
        let (room_left, room_right) = (edges.left, 1.0 - edges.right);
        let (left, right) = if room_left >= room_right {
            let left = want.min(room_left);
            (left, (want - left).min(room_right))
        } else {
            let right = want.min(room_right);
            ((want - right).min(room_left), right)
        };
        out.left -= left;
        out.right += right;
    } else {
        // taller: from the top and the bottom alike, then from whichever has room left
        let want = (w / shape - h).max(0.0);
        let (room_top, room_bottom) = (edges.top, 1.0 - edges.bottom);
        let up = (want / 2.0).min(room_top);
        let down = (want / 2.0).min(room_bottom);
        let rest = want - up - down;
        let up_more = rest.min(room_top - up);
        let down_more = (rest - up_more).min(room_bottom - down);
        out.top -= up + up_more;
        out.bottom += down + down_more;
    }
    out
}

/// `edges` in the pixels of a page of `width` x `height`.
fn to_box(edges: Edges, (width, height): (u32, u32)) -> PageBox {
    let (w, h) = (f64::from(width), f64::from(height));
    let left = (edges.left * w).floor().clamp(0.0, w);
    let top = (edges.top * h).floor().clamp(0.0, h);
    let right = (edges.right * w).ceil().clamp(left, w);
    let bottom = (edges.bottom * h).ceil().clamp(top, h);
    PageBox {
        x: left as u32,
        y: top as u32,
        width: ((right - left) as u32).max(1),
        height: ((bottom - top) as u32).max(1),
    }
}

/// A file as it is now: a file written again is looked at again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl Key {
    fn of(path: &Path) -> Option<Key> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Key {
            path: path.to_path_buf(),
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}
