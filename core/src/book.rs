//! A book whose text flows (FB2; EPUB later): its reader turns it into blocks (paragraphs of styled pieces, pictures, the places of the
//! chapters), and they are laid out here into pages of one fixed size, as a printed book is. The size does not follow the window: the
//! viewer scales a page as it scales one of a PDF, so the number of a page stays the same whatever the window, and a spread, the place
//! where the reader stopped, the contents and the search work as they do for PDF and DjVu.
//!
//! The text is set justified with an indent, in a serif font; the words are measured in the font they are drawn in (`measure`), and
//! each word is drawn where the line puts it. A Russian word that does not fit is broken (`hyphen`) when the line would otherwise be
//! stretched much; a line that would have to be stretched too much still is left ragged (an English one, which is not broken).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::RgbaImage;
use peekel_djvu::text::{Rect, Word, locate};

use crate::decode::{ContentsItem, DecodeError};
use crate::guard::unreadable;
use crate::measure::width;
use crate::page::{Font, Packed, Page, Picture};

/// The page, in points: 6 x 9 inches, a common size of a printed book.
pub const PAGE: (f32, f32) = (432.0, 648.0);
const MARGIN_SIDE: f32 = 44.0;
const MARGIN_TOP: f32 = 46.0;
const MARGIN_BOTTOM: f32 = 54.0;
/// The size of the text of the book, and the height of its line as a multiple of the size.
pub const SIZE: f32 = 11.5;
pub const LEADING: f32 = 1.38;
/// The text of the book, in this order of families: a serif one on every system (`fontset` adds the free ones of the same widths).
const SERIF: &str = "Georgia, Times New Roman, serif";
const MONO: &str = "Consolas, Courier New, monospace";
const SANS: &str = "Arial, Helvetica, sans-serif";
/// The indent of the first line of a paragraph, in units of the size.
const INDENT: f32 = 1.6;
/// A line is stretched to the width only while a space grows to at most this many times its own width.
const MOST_STRETCH: f32 = 4.0;
/// A word that does not fit is broken (`hyphen`) when, without it, the spaces of the line would grow to more than this many times
/// their width.
const HYPHENATE_WHEN: f32 = 1.4;
/// The words of a page are counted in these units of a point, for the search's rectangles.
const UNITS: f32 = 4.0;
/// A book of more pages is cut there.
const MAX_PAGES: usize = 50_000;
/// The size of the text the reader may choose, in per cent of the book's own (`set_text_size`), and its step.
pub const TEXT_SIZES: (u32, u32) = (70, 200);
pub const TEXT_SIZE_STEP: u32 = 10;
/// The insets of a paragraph (an epigraph's) take at most this share of the width of the text, so that a large text keeps room.
const MOST_INSET: f32 = 0.4;
const GREY: [u8; 3] = [110, 110, 110];

/// Pictures decoded at most for one book, one picture's pixels, and the pixels of all of them once shrunk (`SHRINK`).
const MAX_IMAGES: usize = 300;
const MAX_ONE_IMAGE: u64 = 40_000_000;
const MAX_ALL_PIXELS: u64 = 120_000_000;
/// A picture is kept no larger than this on its longest side: about the pixels of a page as the viewer draws it.
const SHRINK: u32 = 1600;

/// The cover of a book from the bytes of its picture (`decode::cover`), as it is, within the pixels of one picture of a book.
pub(crate) fn cover_picture(bytes: &[u8]) -> Option<RgbaImage> {
    crate::decode::decode_bytes(bytes, MAX_ONE_IMAGE).ok()
}

/// The pictures of a book, read once each, within the limits of a book. Each is kept packed, as its file (`page::Packed`): its header
/// is read for its size, and its pixels are decoded when a page with it is drawn. Decoded, a textbook's pictures took 396 MB of its
/// layout (Peekel TV, #235); a tablet with 2 GB ran out of memory with two such books kept.
#[derive(Debug, Default)]
pub struct Pictures {
    read: HashMap<String, Option<Picture>>,
    images: usize,
    pixels: u64,
}

impl Pictures {
    /// The picture known as `key` (its id or its path in the book), its bytes given by `bytes` the first time; `None` when it cannot
    /// be read or the limits are met.
    pub fn get(&mut self, key: &str, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Option<Picture> {
        if let Some(known) = self.read.get(key) {
            return known.clone();
        }
        let image = self.read(bytes);
        self.read.insert(key.to_string(), image.clone());
        image
    }

    fn read(&mut self, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Option<Picture> {
        if self.images >= MAX_IMAGES || self.pixels >= MAX_ALL_PIXELS {
            return None;
        }
        let packed = Packed::new(bytes()?, MAX_ONE_IMAGE, SHRINK)?;
        let (w, h) = packed.dimensions();
        self.images += 1;
        self.pixels += u64::from(w) * u64::from(h);
        Some(Picture::Packed(Arc::new(packed)))
    }
}

/// A piece of the text of a paragraph in one style.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    /// Small and raised: a note's number.
    pub raised: bool,
    /// In a sans-serif face (a book's style sheet asks for one).
    pub sans: bool,
    /// Small capitals: its small letters drawn as capitals a size smaller.
    pub small_caps: bool,
    /// Its size against the paragraph's, when it is not the same (a word a style sheet makes larger or smaller).
    pub scale: Option<f32>,
    /// Small and lowered: a subscript.
    pub lowered: bool,
    pub underline: bool,
    /// Struck through.
    pub strike: bool,
    /// The colour of its letters, when the book gives one; black else.
    pub colour: Option<[u8; 3]>,
}

/// How far a piece of text is lifted from the line: 1 up (a note's number, a superscript), -1 down (a subscript), 0 not.
type Lift = i8;

impl Span {
    fn lift(&self) -> Lift {
        if self.raised {
            1
        } else if self.lowered {
            -1
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Align {
    #[default]
    Justify,
    Left,
    Center,
    Right,
}

/// How a paragraph is set.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// The size, as a multiple of the book's.
    pub scale: f32,
    pub bold: bool,
    pub italic: bool,
    pub align: Align,
    /// Set left only because the book says so or says nothing (a paragraph of an EPUB or a page of HTML): justified all the same when
    /// the reader asks for it (`set_justify`).
    pub loose: bool,
    /// The first line indented: by `indent_by` points when the book gives it, else by the core's `INDENT`.
    pub indent: bool,
    pub indent_by: Option<f32>,
    /// A paragraph its book sets with no indent and no space above it after another one, so that nothing shows where it begins: given
    /// the core's indent while `set_mark_paragraphs` is on, as Readium CSS makes a paragraph's indent a reader's setting.
    pub unmarked: bool,
    /// More margin on the left and on the right, in points.
    pub left: f32,
    pub right: f32,
    /// Space above and below, in points.
    pub before: f32,
    pub after: f32,
    /// Kept on one page with the line after it (a heading).
    pub keep: bool,
    /// A line of a block of code (`pre`, a listing a style sheet sets in a monospace font): drawn on a backdrop when that is asked for
    /// (`set_code_backdrop`), with the colour the book gives it (`shade`) or the core's own.
    pub code: bool,
    pub shade: Option<[u8; 3]>,
}

impl Style {
    /// A paragraph of the text.
    pub fn body() -> Self {
        Self {
            scale: 1.0,
            bold: false,
            italic: false,
            align: Align::Justify,
            loose: false,
            indent: true,
            indent_by: None,
            unmarked: false,
            left: 0.0,
            right: 0.0,
            before: 0.0,
            after: 0.0,
            keep: false,
            code: false,
            shade: None,
        }
    }

    /// A heading of this depth (0 the title of a part or the book, 1 a chapter, deeper smaller).
    pub fn heading(depth: usize) -> Self {
        let scale = match depth {
            0 => 1.6,
            1 => 1.35,
            2 => 1.18,
            _ => 1.05,
        };
        Self {
            scale,
            bold: true,
            align: Align::Center,
            indent: false,
            before: SIZE * 0.6,
            after: SIZE * 0.6,
            keep: true,
            ..Self::body()
        }
    }

    /// A line of a poem, an epigraph and the like: left, not indented, set in from the left by `left` points.
    pub fn verse(left: f32) -> Self {
        Self {
            align: Align::Left,
            indent: false,
            left,
            ..Self::body()
        }
    }
}

/// A table of a book (`<table>` of an EPUB, an FB2, a page of HTML): its rows in order, set in from the left by `left` points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Row>,
    pub left: f32,
}

/// A row of a table; `head` for one of its header (`<thead>`, or a row of header cells only), drawn again at the top of a page the
/// table goes on to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub head: bool,
}

/// A cell of a table: its text (a `\n` in it starts a new line: a paragraph or a `<br>` of the cell), how many columns and rows it
/// spans (`colspan`, `rowspan`; at least 1), whether it is a header cell (`<th>`: centred), and whether it is set bold (a header
/// cell, or one whose text the book sets bold).
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub columns: usize,
    pub rows: usize,
    pub head: bool,
    pub bold: bool,
}

/// A part of a book, in reading order.
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph {
        spans: Vec<Span>,
        style: Style,
    },
    /// A picture in the text: as wide as it is (a pixel a point) up to the width of the text; `None` when it could not be read.
    Picture(Option<Picture>),
    /// The cover: a page of its own, the picture as large as the page.
    Cover(Picture),
    /// A table, laid out as a grid (`Builder::table`).
    Table(Table),
    /// Space, in points (an empty line).
    Space(f32),
    /// A line across the text (`hr` of a book with style sheets), set in by `left` and `right` points.
    Rule {
        left: f32,
        right: f32,
    },
    /// What follows starts a new page (a chapter).
    NewPage,
    /// A page break a book's style sheet asks for inside a part (`page-break-after: always`): a new page, but not after a short stretch
    /// of text while `set_mark_paragraphs` is on (`SHORT_BREAK`). Not where a part begins.
    Break,
    /// A line of the contents: the next thing laid out is where it leads.
    Mark {
        depth: usize,
        title: String,
    },
}

/// One page laid out: what is drawn, its words and where they stand (in `UNITS` of a point), and its text.
#[derive(Debug, Clone)]
pub struct LaidPage {
    pub page: Page,
    pub words: Vec<Word>,
    pub text: String,
    /// The place of the page in the book: how many words come before it. It does not depend on the size of the text, so the reader's
    /// place is found again when the book is laid out at another size (`page_at`).
    pub place: usize,
    /// Whether its number is drawn at its foot (not on a cover). The number is drawn when the page is, from where the page is in the
    /// book: a part laid out alone (`Growing`) does not know it yet.
    pub numbered: bool,
}

/// A book laid out into pages.
#[derive(Debug, Clone)]
pub struct Laid {
    pub pages: Vec<LaidPage>,
    pub contents: Vec<ContentsItem>,
    /// The size of its pages, in points (`PAGE` made smaller for a larger text).
    pub size: (f32, f32),
    /// The margin at the foot of its pages, in points, where the number is drawn.
    pub foot: f32,
    /// The first page of each part of the book (`parts`: what follows a `Block::NewPage`), in order.
    pub parts: Vec<usize>,
}

impl Laid {
    /// About how many bytes the layout holds (`Page::weight`, the words and the text of the pages; a picture once).
    pub fn weight(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        self.pages
            .iter()
            .map(|p| {
                std::mem::size_of::<LaidPage>()
                    + p.page.weight(&mut seen)
                    + p.words
                        .iter()
                        .map(|w| std::mem::size_of::<Word>() + w.text.len())
                        .sum::<usize>()
                    + p.text.len()
            })
            .sum()
    }

    /// The size the rectangles of the words are counted in.
    pub fn units(&self) -> (u32, u32) {
        ((self.size.0 * UNITS) as u32, (self.size.1 * UNITS) as u32)
    }
}

/// The size of the text the books are laid out at, in per cent of their own: the viewer's choice, for every book (the quick view of
/// the plugin never changes it). A larger text is laid out on a page that is as much smaller, with margins as much smaller: the page is
/// drawn at the same size on the screen, so its text is larger there, and fewer words go on it.
static TEXT_SIZE: AtomicU32 = AtomicU32::new(100);

/// Sets the size of the text (in per cent, kept within `TEXT_SIZES`); the next book or page asked for is laid out at it.
pub fn set_text_size(percent: u32) {
    TEXT_SIZE.store(percent.clamp(TEXT_SIZES.0, TEXT_SIZES.1), Ordering::Relaxed);
}

/// The size of the text, in per cent.
pub fn text_size() -> u32 {
    TEXT_SIZE.load(Ordering::Relaxed)
}

/// The shape of the page the books are laid out on, width and height in any units (the pixels of a screen, or of one column of it):
/// `0` for the book's own shape, `PAGE` (6 x 9). Set by a program that shows a page of a shape of its own (an Android TV in landscape,
/// a phone upright); the terminal viewer never sets it.
static PAGE_SHAPE: AtomicU64 = AtomicU64::new(0);

/// Whether a block of code is drawn on a backdrop, with a little room above and below it (Peekel TV, #235, item 9: many books give their
/// code nothing that sets it off from the text). Off unless a program asks for it (the terminal viewer and the plugin never do).
static CODE_BACKDROP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The backdrop of a block of code a book gives no colour of its own: a little darker than the white page, so that a page a reader
/// darkens for the night (it turns the page's colours round) gets a backdrop a little lighter than its black.
const CODE_SHADE: [u8; 3] = [238, 238, 238];
/// The room round the lines of a block of code on its backdrop, in parts of the size of the text: above and below it, and at its sides.
const CODE_ROOM: f32 = 0.5;
const CODE_SIDE: f32 = 0.4;

/// Sets whether a block of code is drawn on a backdrop (`CODE_BACKDROP`); the next book or page asked for is laid out so.
pub fn set_code_backdrop(on: bool) {
    CODE_BACKDROP.store(on, Ordering::Relaxed);
}

/// Whether a block of code is drawn on a backdrop.
pub fn code_backdrop() -> bool {
    CODE_BACKDROP.load(Ordering::Relaxed)
}

/// Whether the paragraphs of a book whose style sheets set them left (or say nothing) are justified and hyphenated all the same (the
/// author, 24 November 2026: a setting, on by default, the look of the books before their style sheets were read). Off, the book's own
/// alignment. Paragraphs the book centres or sets right stay so either way, as Readium CSS keeps them (`docs/internal/css-cascade.md`).
static JUSTIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Sets whether paragraphs are justified whatever the book's style sheets say (`JUSTIFY`); the next book or page asked for is laid out
/// so.
pub fn set_justify(on: bool) {
    JUSTIFY.store(on, Ordering::Relaxed);
}

/// Whether paragraphs are justified whatever the book's style sheets say.
pub fn justify() -> bool {
    JUSTIFY.load(Ordering::Relaxed)
}

/// Whether the edges of blocks are shown where a book's style sheets leave them unseen: a paragraph with nothing to show where it
/// begins (no indent and no space above it, just after another such one: a book made carelessly) is given the core's indent
/// (`Style::unmarked`), the text after a table at least `ROOM_AFTER_TABLE` below it (Peekel TV, #235), and a page break the book asks
/// for after a short stretch of text not made (`SHORT_BREAK`). The author, 24 November 2026: a setting of its own, on by default.
/// Off, the book's look as its style sheets give it.
static MARK_PARAGRAPHS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Sets whether a paragraph with nothing to show where it begins is given the core's indent (`MARK_PARAGRAPHS`); the next book or
/// page asked for is laid out so.
pub fn set_mark_paragraphs(on: bool) {
    MARK_PARAGRAPHS.store(on, Ordering::Relaxed);
}

/// Whether a paragraph with nothing to show where it begins is given the core's indent.
pub fn mark_paragraphs() -> bool {
    MARK_PARAGRAPHS.load(Ordering::Relaxed)
}

/// Whether the size most of a book's text is set in (its style sheets may make it 120 % or 84 % of the usual) is taken as the size
/// the reader chose, all the book's sizes kept in proportion to it (`body_scale`): the author, 24 November 2026, a setting of its own,
/// on by default. Off, the sizes as the book's sheets give them.
static NORMALIZE_TEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Sets whether the size most of a book's text is set in is taken as the reader's (`NORMALIZE_TEXT`); the next book or page asked for
/// is laid out so.
pub fn set_normalize_text(on: bool) {
    NORMALIZE_TEXT.store(on, Ordering::Relaxed);
}

/// Whether the size most of a book's text is set in is taken as the reader's.
pub fn normalize_text() -> bool {
    NORMALIZE_TEXT.load(Ordering::Relaxed)
}

/// The settings of the look a layout is made with, beside the size of the text and the shape of the page: blocks of code on a backdrop
/// (`set_code_backdrop`), paragraphs justified (`set_justify`), paragraphs with nothing to show where they begin indented
/// (`set_mark_paragraphs`), the size of most of the text the reader's (`set_normalize_text`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Look {
    backdrop: bool,
    justify: bool,
    mark: bool,
    normalize: bool,
}

fn look() -> Look {
    Look {
        backdrop: code_backdrop(),
        justify: justify(),
        mark: mark_paragraphs(),
        normalize: normalize_text(),
    }
}

/// The size most of the text of `blocks` is set in, of the book's size: that of the paragraphs (not code) with the most letters,
/// among sizes taken to a hundredth; 1 for a book with none. Kept within half and twice, so that a book of odd sizes stays readable.
fn body_scale(blocks: &[Block]) -> f32 {
    let mut letters: HashMap<u32, usize> = HashMap::new();
    for block in blocks {
        if let Block::Paragraph { spans, style } = block
            && !style.code
        {
            let count: usize = spans.iter().map(|s| s.text.chars().count()).sum();
            *letters.entry((style.scale * 100.0).round() as u32).or_default() += count;
        }
    }
    letters
        .into_iter()
        .max_by_key(|(scale, count)| (*count, *scale))
        .map_or(1.0, |(scale, _)| (scale as f32 / 100.0).clamp(0.5, 2.0))
}

/// What the sizes of `blocks` are multiplied by in a layout of `look`: the size of most of their text made the reader's, when asked.
fn size_factor(blocks: &[Block], look: Look) -> f32 {
    if look.normalize { 1.0 / body_scale(blocks) } else { 1.0 }
}

/// The height of a page against its width is kept within these: a page of a shape more extreme is laid out at the nearest of them.
pub const PAGE_SHAPES: (f32, f32) = (0.5, 2.5);

/// Sets the shape of the page (`None`: the book's own); the next book or page asked for is laid out on it. The width of the lines stays
/// that of `PAGE` (a line as long as in a printed book, at any size of the text); the height follows the shape, within `PAGE_SHAPES`.
/// A place in the text (`place_of`, `page_at`) is kept across a change of shape as across a change of the size of the text.
pub fn set_page_shape(shape: Option<(u32, u32)>) {
    PAGE_SHAPE.store(packed(shape), Ordering::Relaxed);
}

/// The shape of the page set by [`set_page_shape`], `None` for the book's own.
pub fn page_shape() -> Option<(u32, u32)> {
    unpacked(PAGE_SHAPE.load(Ordering::Relaxed))
}

/// A shape in one number (width above, height below); `0` for none, and for a side of 0.
fn packed(shape: Option<(u32, u32)>) -> u64 {
    match shape {
        Some((w, h)) if w > 0 && h > 0 => (u64::from(w) << 32) | u64::from(h),
        _ => 0,
    }
}

fn unpacked(packed: u64) -> Option<(u32, u32)> {
    (packed != 0).then_some(((packed >> 32) as u32, packed as u32))
}

/// The page, in points at the book's own size of text, for a shape (`None`: `PAGE`).
pub fn page_for(shape: Option<(u32, u32)>) -> (f32, f32) {
    match shape {
        Some((w, h)) if w > 0 && h > 0 => {
            let tall = (h as f32 / w as f32).clamp(PAGE_SHAPES.0, PAGE_SHAPES.1);
            (PAGE.0, PAGE.0 * tall)
        }
        _ => PAGE,
    }
}

/// A unit of a line that is not broken: the pieces of a word in their fonts (`emphasis` and the comma after it), its width, and
/// whether a space follows it. A word broken at the end of a line is two units: the head (with the hyphen drawn) and the tail on
/// the next line; both know the whole word, for the search and the text of the page.
#[derive(Default)]
struct Unit {
    pieces: Vec<(String, Font, Lift)>,
    width: f32,
    space: bool,
    /// The whole word, when this is a part of one broken at a line's end.
    whole: Option<String>,
    /// The part of a broken word that ends a line.
    head: bool,
    /// The part of a broken word that starts a line.
    tail: bool,
    /// A later piece of a unit wider than the line, cut where the line ends (`units_of`): not a word of its own in the count of words,
    /// so that the count is that of the text (`words_in`) and not of the width of a line.
    more: bool,
}

impl Unit {
    fn text(&self) -> String {
        self.pieces.iter().map(|(t, _, _)| t.as_str()).collect()
    }

    /// The word, whole if this is a part of one.
    fn word(&self) -> String {
        self.whole.clone().unwrap_or_else(|| self.text())
    }

    /// The unit broken before char `at` of its text: the head (a hyphen drawn after it, unless the break is after a hyphen of the
    /// word) and the tail.
    fn split(&self, at: usize) -> (Unit, Unit) {
        let mut head: Vec<(String, Font, Lift)> = Vec::new();
        let mut tail: Vec<(String, Font, Lift)> = Vec::new();
        let mut seen = 0;
        for (text, font, raised) in &self.pieces {
            let count = text.chars().count();
            if seen + count <= at {
                head.push((text.clone(), font.clone(), *raised));
            } else if seen >= at {
                tail.push((text.clone(), font.clone(), *raised));
            } else {
                let cut = at - seen;
                head.push((text.chars().take(cut).collect(), font.clone(), *raised));
                tail.push((text.chars().skip(cut).collect(), font.clone(), *raised));
            }
            seen += count;
        }
        if let Some((text, _, _)) = head.last_mut()
            && !text.ends_with('-')
        {
            text.push('-');
        }
        let measure = |pieces: &[(String, Font, Lift)]| pieces.iter().map(|(t, f, _)| width(t, f)).sum();
        let whole = Some(self.word());
        (
            Unit {
                width: measure(&head),
                pieces: head,
                space: false,
                whole: whole.clone(),
                head: true,
                tail: self.tail,
                more: self.more,
            },
            Unit {
                width: measure(&tail),
                pieces: tail,
                space: self.space,
                whole,
                head: self.head,
                tail: true,
                more: self.more,
            },
        )
    }

    /// The unit broken at the last place where its head fits in `room` points, if there is one.
    fn hyphenate(&self, room: f32) -> Option<(Unit, Unit)> {
        crate::hyphen::points(&self.text())
            .into_iter()
            .rev()
            .map(|at| self.split(at))
            .find(|(head, _)| head.width <= room)
    }
}

/// The font of a span in a paragraph of this style.
fn font_of(span: &Span, style: &Style) -> Font {
    Font {
        size: SIZE * style.scale * span.scale.unwrap_or(1.0) * if span.raised || span.lowered { 0.72 } else { 1.0 },
        bold: style.bold || span.bold,
        // emphasis in an italic paragraph is upright
        italic: style.italic != span.italic,
        underline: span.underline,
        strike: span.strike,
        colour: span.colour.unwrap_or([0, 0, 0]),
        family: Some(
            if span.mono {
                MONO
            } else if span.sans {
                SANS
            } else {
                SERIF
            }
            .to_string(),
        ),
    }
}

/// The size small capitals are drawn at, of the text's.
const SMALL_CAPS: f32 = 0.78;

/// The least room between a table's last line and the text after it, while the reader's look is asked for (`set_mark_paragraphs`): a
/// book may give 4 px, which on a screen leaves the text touching the table. Not after a picture, whose caption stays by it.
const ROOM_AFTER_TABLE: f32 = SIZE;

/// The height a line across the text (`Block::Rule`) takes, the line in its middle.
const RULE_ROOM: f32 = SIZE * 0.5;

/// The share of a page's height below which the text since the last page break (or the start of the part) is too short to end its
/// page at the next break the book asks for, while `set_mark_paragraphs` is on: a book converted from FB2 ends every section with a
/// page break, its notes too, and gave each note of three lines a page of its own (110 such pages in one book).
const SHORT_BREAK: f32 = 1.0 / 3.0;

/// The units of a paragraph: its text cut at white space, the pieces of different spans that touch kept together.
fn units_of(spans: &[Span], style: &Style, room: f32) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut current: Vec<(String, Font, Lift)> = Vec::new();
    let finish = |current: &mut Vec<(String, Font, Lift)>, units: &mut Vec<Unit>, space: bool| {
        if current.is_empty() {
            if space && let Some(last) = units.last_mut() {
                last.space = true;
            }
            return;
        }
        let pieces = std::mem::take(current);
        let width = pieces.iter().map(|(t, f, _)| width(t, f)).sum();
        units.push(Unit {
            pieces,
            width,
            space,
            ..Unit::default()
        });
    };
    for span in spans {
        let font = font_of(span, style);
        // small capitals: a small letter is drawn as a capital in a smaller font, so a word may be pieces of two fonts
        let small = Font {
            size: font.size * SMALL_CAPS,
            ..font.clone()
        };
        let mut piece = String::new();
        let mut piece_small = false;
        for c in span.text.chars() {
            // a figure space (U+2007) is a space of a digit's width that does not break: the indent of a line of code
            if c.is_whitespace() && c != '\u{2007}' {
                if !piece.is_empty() {
                    let f = if piece_small { &small } else { &font };
                    current.push((std::mem::take(&mut piece), f.clone(), span.lift()));
                }
                finish(&mut current, &mut units, true);
            } else if !c.is_control() {
                let is_small = span.small_caps && c.is_lowercase();
                if is_small != piece_small && !piece.is_empty() {
                    let f = if piece_small { &small } else { &font };
                    current.push((std::mem::take(&mut piece), f.clone(), span.lift()));
                }
                piece_small = is_small;
                if is_small {
                    piece.extend(c.to_uppercase());
                } else {
                    piece.push(c);
                }
            }
        }
        if !piece.is_empty() {
            current.push((piece, if piece_small { small } else { font }, span.lift()));
        }
    }
    finish(&mut current, &mut units, false);
    // a unit wider than the line (a long address) is cut where the line ends
    let mut out = Vec::with_capacity(units.len());
    for unit in units {
        cut_unit(unit, room, &mut out);
    }
    out
}

/// `unit` as it is when it is no wider than `room`, else cut where `room` ends into pieces (the later ones not counted as words).
fn cut_unit(unit: Unit, room: f32, out: &mut Vec<Unit>) {
    if unit.width <= room {
        out.push(unit);
        return;
    }
    let space = unit.space;
    let mut part: Vec<(String, Font, Lift)> = Vec::new();
    let mut part_width = 0.0;
    let mut more = false;
    for (text, font, raised) in unit.pieces {
        for c in text.chars() {
            let w = width(&c.to_string(), &font);
            if part_width + w > room && part_width > 0.0 {
                out.push(Unit {
                    pieces: std::mem::take(&mut part),
                    width: part_width,
                    more,
                    ..Unit::default()
                });
                part_width = 0.0;
                more = true;
            }
            match part.last_mut() {
                Some((t, f, r)) if *f == font && *r == raised => t.push(c),
                _ => part.push((c.to_string(), font.clone(), raised)),
            }
            part_width += w;
        }
    }
    if !part.is_empty() {
        out.push(Unit {
            pieces: part,
            width: part_width,
            space,
            more,
            ..Unit::default()
        });
    }
}

/// `unit` made to fit `room`: as it is when it does; else broken with a hyphen where the language allows (`Unit::hyphenate`), as
/// often as it takes, and what still does not fit cut (`cut_unit`). For a word in a table's cell.
fn fit_unit(unit: Unit, room: f32, out: &mut Vec<Unit>) {
    let mut rest = unit;
    while rest.width > room {
        match rest.hyphenate(room) {
            Some((head, tail)) => {
                out.push(head);
                rest = tail;
            }
            None => break,
        }
    }
    cut_unit(rest, room, out);
}

/// The room inside a table's cell at its sides and above and below its text, in points, the same at any size the table is made to
/// (Peekel TV, #235: at 0.3 em made smaller with the table, the text all but touched the lines); the lines between the cells; the bar
/// between the cells of a row set as a line; the smallest a table is made to fit the width (of its size); the most columns read.
const TABLE_PAD: f32 = SIZE * 0.4;
const TABLE_VPAD: f32 = SIZE * 0.2;
const TABLE_LINE: [u8; 3] = [150, 150, 150];
const TABLE_BAR: &str = "\u{2502}";
const TABLE_SMALLEST: f32 = 0.7;
/// The size of a table's text (of the body's), as books set their tables: fewer lines wrapped (Peekel TV, #235).
const TABLE_TEXT: f32 = 0.9;
/// The most width a column takes for its longest word, in points at the table's size (8 em of its text): a longer word is broken in
/// its cell.
const TABLE_WORD: f32 = SIZE * TABLE_TEXT * 8.0;
/// The shade behind a header cell and the line under a table's header rows, which set the header apart more than bold (Peekel TV,
/// #235; a reader's night look darkens the page, the shade with it).
const TABLE_HEAD_SHADE: [u8; 3] = [240, 240, 240];
const TABLE_HEAD_LINE: f32 = 1.2;
const MAX_COLUMNS: usize = 64;

/// A table laid out: the height of each row, the left edge of each column from the table's (and the right edge of the last), its
/// cells with their lines, how many rows at its top are its header, and the size it is set at (of the text's).
struct Grid {
    rows: Vec<f32>,
    lefts: Vec<f32>,
    cells: Vec<GridCell>,
    head: usize,
    scale: f32,
}

struct GridCell {
    row: usize,
    column: usize,
    rows: usize,
    columns: usize,
    lines: Vec<Vec<Unit>>,
    /// A header cell, or a cell of the header rows: shaded.
    head: bool,
    font: Font,
    place: Place,
}

/// Where a cell's lines stand across it: at its left; centred (a header cell); or a number right-aligned in a block as wide as the
/// widest number of its column, the block centred in the cell (a column of one-digit numbers comes out centred, numbers of several
/// lengths aligned on their last digits).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Place {
    Left,
    Centre,
    Number(f32),
}

/// Whether a cell's text is a number (`Some(true)`: digits with a sign, a decimal point or comma, spaces between groups, a per cent),
/// a dash that stands for none (`Some(false)`), or neither. An address (two points and more) or a date is no number.
fn number_like(text: &str) -> Option<bool> {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 16 {
        return None;
    }
    if t.chars().all(|c| matches!(c, '-' | '\u{2013}' | '\u{2014}')) {
        return Some(false);
    }
    let body = t
        .strip_prefix(|c: char| matches!(c, '-' | '+' | '\u{2212}' | '\u{b1}'))
        .unwrap_or(t);
    let body = body.strip_suffix('%').unwrap_or(body).trim_end();
    let number = body.starts_with(|c: char| c.is_ascii_digit())
        && body.ends_with(|c: char| c.is_ascii_digit())
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\u{a0}' | '\u{202f}'))
        && body.matches('.').count() <= 1;
    number.then_some(true)
}

/// The width of a line of units, `space` between those a space follows.
fn natural(line: &[Unit], space: f32) -> f32 {
    line.iter().map(|u| u.width).sum::<f32>()
        + line[..line.len().saturating_sub(1)].iter().filter(|u| u.space).count() as f32 * space
}

impl Grid {
    /// The end of the group of rows that starts at `row`: the rows a cell that starts in it spans, so that the group is kept on one page.
    fn group_end(&self, row: usize) -> usize {
        let mut end = row + 1;
        loop {
            let further = self
                .cells
                .iter()
                .filter(|c| c.row >= row && c.row < end)
                .map(|c| c.row + c.rows)
                .max()
                .unwrap_or(end);
            if further <= end {
                return end.min(self.rows.len());
            }
            end = further;
        }
    }
}

/// The style of a cell's text at `scale` of the text's size: bold when the cell is, a header cell centred.
fn cell_style(cell: &Cell, scale: f32) -> Style {
    Style {
        scale,
        bold: cell.bold,
        align: if cell.head { Align::Center } else { Align::Left },
        indent: false,
        ..Style::body()
    }
}

/// The paragraphs of a cell's text (its lines), each as units of `style`; a word wider than `room` broken with a hyphen where its
/// language allows, else cut (`fit_unit`).
fn cell_units(text: &str, style: &Style, room: f32) -> Vec<Vec<Unit>> {
    text.split('\n')
        .map(|paragraph| {
            let span = Span {
                text: paragraph.to_string(),
                ..Span::default()
            };
            let mut out = Vec::new();
            for unit in units_of(std::slice::from_ref(&span), style, f32::MAX) {
                fit_unit(unit, room, &mut out);
            }
            out
        })
        .filter(|units| !units.is_empty())
        .collect()
}

/// A table laid out as a grid within `room` points of width: each column as wide as its longest word at least, and as its longest
/// line at most, the room between shared out as the columns want it; a table whose longest words do not fit is made smaller, down to
/// `TABLE_SMALLEST` of its size. `None` when it still does not fit, or a group of rows a cell spans is taller than `page`.
fn grid(table: &Table, room: f32, page: f32) -> Option<Grid> {
    let count = table.rows.len();
    // where each cell stands: a cell takes the first column of its row a cell above does not span into
    let mut taken: Vec<Vec<bool>> = vec![Vec::new(); count];
    let mut placed: Vec<(usize, usize, usize, usize, &Cell)> = Vec::new();
    let mut columns = 0;
    for (r, row) in table.rows.iter().enumerate() {
        let mut c = 0;
        for cell in &row.cells {
            while taken[r].get(c) == Some(&true) {
                c += 1;
            }
            if c >= MAX_COLUMNS {
                break;
            }
            let spans = (cell.rows.clamp(1, count - r), cell.columns.clamp(1, MAX_COLUMNS - c));
            for line in &mut taken[r..r + spans.0] {
                if line.len() < c + spans.1 {
                    line.resize(c + spans.1, false);
                }
                line[c..c + spans.1].fill(true);
            }
            placed.push((r, c, spans.0, spans.1, cell));
            c += spans.1;
            columns = columns.max(c);
        }
    }
    if columns == 0 {
        return None;
    }
    // the widths at the table's own size: the longest word of a column (as it is, and no longer than `TABLE_WORD`), and its longest
    // line; the room inside a cell does not change with the table's size
    let pad = 2.0 * TABLE_PAD;
    let (mut whole, mut least, mut most) = (vec![0.0f32; columns], vec![0.0f32; columns], vec![0.0f32; columns]);
    let mut wants: Vec<(usize, usize, f32, f32, f32)> = placed
        .iter()
        .map(|&(_, c, _, cols, cell)| {
            let style = cell_style(cell, TABLE_TEXT);
            let space = width(" ", &font_of(&Span::default(), &style));
            let paragraphs = cell_units(&cell.text, &style, f32::MAX);
            let longest_word = paragraphs.iter().flatten().map(|u| u.width).fold(0.0, f32::max);
            let longest_line = paragraphs
                .iter()
                .map(|units| {
                    units.iter().map(|u| u.width).sum::<f32>()
                        + units[..units.len().saturating_sub(1)]
                            .iter()
                            .filter(|u| u.space)
                            .count() as f32
                            * space
                })
                .fold(0.0, f32::max);
            (c, cols, longest_word, longest_word.min(TABLE_WORD), longest_line)
        })
        .collect();
    // the cells of one column first, then those that span more, which widen their columns as much as each
    wants.sort_by_key(|&(_, cols, _, _, _)| cols);
    for (c, cols, word, short, line) in wants {
        let share = |widths: &mut [f32], want: f32| {
            let have: f32 = widths[c..c + cols].iter().sum::<f32>() + (cols - 1) as f32 * pad;
            if want > have {
                for w in &mut widths[c..c + cols] {
                    *w += (want - have) / cols as f32;
                }
            }
        };
        share(&mut whole, word);
        share(&mut least, short);
        share(&mut most, line.max(word));
    }
    for ((most, whole), least) in most.iter_mut().zip(&mut whole).zip(&least) {
        *whole = whole.max(*least);
        *most = most.max(*whole);
    }
    let sum = |widths: &[f32]| widths.iter().sum::<f32>();
    let free = room - columns as f32 * pad;
    // the room past the least shared out as the columns want it, to the most: a column whose text wraps gets it, one that is already
    // as wide as its longest line gets none
    let between = |least: &[f32]| -> Vec<f32> {
        let k = (free - sum(least)) / (sum(&most) - sum(least)).max(f32::EPSILON);
        least.iter().zip(&most).map(|(l, m)| l + (m - l) * k.min(1.0)).collect()
    };
    let (scale, widths): (f32, Vec<f32>) = if sum(&most) <= free {
        (TABLE_TEXT, most.clone())
    } else if sum(&whole) <= free {
        // every word whole
        (TABLE_TEXT, between(&whole))
    } else if sum(&least) <= free {
        // a word longer than `TABLE_WORD` broken in its cell (with a hyphen where its language allows)
        (TABLE_TEXT, between(&least))
    } else {
        let k = free / sum(&least);
        if TABLE_TEXT * k < TABLE_SMALLEST {
            return None;
        }
        (TABLE_TEXT * k, least.iter().map(|l| l * k).collect())
    };
    // the header: its rows marked (`<thead>`, or header cells only), else a first row whose every cell is set bold (a book that sets
    // its header in `<td><b>`), when rows follow it
    let marked = table.rows.iter().take_while(|r| r.head).count();
    let head = if marked > 0 {
        marked
    } else {
        usize::from(table.rows.first().is_some_and(|r| {
            r.cells.iter().any(|c| !c.text.trim().is_empty())
                && r.cells.iter().all(|c| c.bold || c.text.trim().is_empty())
        }))
    };
    let head = if head == count { 0 } else { head };
    // a column whose cells (of one column, not header cells) are numbers, or dashes for none with a number among them
    let numeric: Vec<bool> = (0..columns)
        .map(|c| {
            let kinds: Vec<Option<bool>> = placed
                .iter()
                .filter(|&&(r, at, _, cols, cell)| {
                    r >= head && at == c && cols == 1 && !cell.head && !cell.text.trim().is_empty()
                })
                .map(|&(_, _, _, _, cell)| number_like(&cell.text))
                .collect();
            kinds.iter().all(Option::is_some) && kinds.contains(&Some(true))
        })
        .collect();
    let mut lefts = vec![0.0f32];
    for w in &widths {
        let last = lefts.last().copied().unwrap_or(0.0);
        lefts.push(last + w + pad);
    }
    // the cells' lines, and the rows as high as their highest cells
    let line_height = SIZE * scale * LEADING;
    let least_row = line_height + 2.0 * TABLE_VPAD;
    let mut rows = vec![least_row; count];
    let mut cells: Vec<GridCell> = placed
        .into_iter()
        .map(|(row, column, rows, columns, cell)| {
            let style = cell_style(cell, scale);
            let font = font_of(&Span::default(), &style);
            // half a point more: a column as wide as its longest word, worked out in parts, may come out a hair narrower than it
            let inner = (lefts[column + columns] - lefts[column] - pad).max(1.0) + 0.5;
            let space = width(" ", &font);
            let lines = cell_units(&cell.text, &style, inner)
                .into_iter()
                .flat_map(|units| break_lines(units, (inner, inner), false, space))
                .collect::<Vec<_>>();
            let place = if cell.head {
                Place::Centre
            } else if columns == 1 && numeric[column] && lines.len() == 1 && number_like(&cell.text).is_some() {
                // the block's width is known when the column's numbers are all laid out (below)
                Place::Number(0.0)
            } else {
                Place::Left
            };
            GridCell {
                row,
                column,
                rows,
                columns,
                lines,
                head: cell.head || row < head,
                font,
                place,
            }
        })
        .collect();
    let mut blocks = vec![0.0f32; columns];
    for cell in cells.iter().filter(|c| matches!(c.place, Place::Number(_))) {
        let space = width(" ", &cell.font);
        let line = cell.lines.first().map_or(0.0, |l| natural(l, space));
        blocks[cell.column] = blocks[cell.column].max(line);
    }
    for cell in &mut cells {
        if let Place::Number(block) = &mut cell.place {
            *block = blocks[cell.column];
        }
    }
    cells.sort_by_key(|c| c.rows);
    for cell in &cells {
        let height = cell.lines.len().max(1) as f32 * line_height + 2.0 * TABLE_VPAD;
        let have: f32 = rows[cell.row..cell.row + cell.rows].iter().sum();
        if height > have {
            rows[cell.row + cell.rows - 1] += height - have;
        }
    }
    cells.sort_by_key(|c| (c.row, c.column));
    let grid = Grid {
        rows,
        lefts,
        cells,
        head,
        scale,
    };
    // a group of rows taller than a page cannot be kept on one
    let mut row = 0;
    while row < count {
        let end = grid.group_end(row);
        if grid.rows[row..end].iter().sum::<f32>() > page {
            return None;
        }
        row = end;
    }
    Some(grid)
}

/// `units` broken into lines of `room` points (`first`, the room of the first line), `space` between units a space follows. A
/// justified paragraph (`justify`) breaks a word that does not fit when the line without it would be stretched much.
fn break_lines(units: Vec<Unit>, (first, room): (f32, f32), justify: bool, space: f32) -> Vec<Vec<Unit>> {
    let mut lines: Vec<Vec<Unit>> = Vec::new();
    let mut current: Vec<Unit> = Vec::new();
    let mut used = 0.0;
    let mut queue: std::collections::VecDeque<Unit> = units.into();
    while let Some(unit) = queue.pop_front() {
        let line_room = if lines.is_empty() { first } else { room };
        let gap = if current.last().is_some_and(|u| u.space) {
            space
        } else {
            0.0
        };
        if current.is_empty() || used + gap + unit.width <= line_room {
            used += gap + unit.width;
            current.push(unit);
            continue;
        }
        if justify {
            let gaps = current[..current.len() - 1].iter().filter(|u| u.space).count();
            let loose = gaps == 0 || (line_room - used) / gaps as f32 > space * (HYPHENATE_WHEN - 1.0);
            if loose && let Some((head, tail)) = unit.hyphenate(line_room - used - gap) {
                current.push(head);
                lines.push(std::mem::take(&mut current));
                queue.push_front(tail);
                used = 0.0;
                continue;
            }
        }
        lines.push(std::mem::take(&mut current));
        used = unit.width;
        current.push(unit);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// How many words `blocks` have in the count of the layout (`Builder::words`, the places of pages) without laying them out: the units
/// of `units_of` before any is cut or broken, so that the place a part of a book starts at is known before it is laid out (`Growing`).
fn words_in(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph { spans, .. } => words_of(spans.iter().map(|s| s.text.as_str())),
            // each cell once (its header drawn again on a page the table goes on to, and the bars of a row set as a line, are not
            // words of the text)
            Block::Table(table) => table
                .rows
                .iter()
                .flat_map(|r| &r.cells)
                .map(|c| words_of(std::iter::once(c.text.as_str())))
                .sum(),
            _ => 0,
        })
        .sum()
}

/// The words of pieces of text that follow each other without a break (the spans of a paragraph), as `units_of` makes units of them.
fn words_of<'t>(pieces: impl Iterator<Item = &'t str>) -> usize {
    let (mut count, mut in_word) = (0usize, false);
    for piece in pieces {
        for c in piece.chars() {
            if c.is_whitespace() && c != '\u{2007}' {
                in_word = false;
            } else if !c.is_control() && !in_word {
                in_word = true;
                count += 1;
            }
        }
    }
    count
}

/// What the laying out has made so far.
struct Builder {
    pages: Vec<LaidPage>,
    page: LaidPage,
    y: f32,
    contents: Vec<ContentsItem>,
    /// Marks met whose page is the page of the next thing laid out.
    pending: Vec<(usize, String)>,
    /// The current page is the cover (no number on it).
    cover: bool,
    limit: usize,
    /// The page and its margins, in points, for the size of the text.
    width: f32,
    height: f32,
    side: f32,
    top: f32,
    foot: f32,
    /// The words laid out so far (a broken word once).
    words: usize,
    /// Whether blocks of code are drawn on a backdrop (`set_code_backdrop`), and the backdrop of the one being laid out: its colour and
    /// its left edge and width.
    backdrop: bool,
    code: Option<([u8; 3], f32, f32)>,
    /// The backdrop drawn for that block on this page: where it is among the page's items, and its foot.
    backdrop_box: Option<(usize, f32)>,
    /// Whether a paragraph set left only by its book is justified all the same (`set_justify`).
    justify: bool,
    /// Whether a paragraph with nothing to show where it begins gets the core's indent (`set_mark_paragraphs`).
    mark: bool,
    /// The room a paragraph kept with what follows it (a heading, a caption) needs below it: two lines, or the start of a table.
    follow: f32,
    /// What the sizes of the paragraphs are multiplied by (`size_factor`).
    factor: f32,
    /// Where the last table ended, while nothing but space has come after it (for the room after it, `ROOM_AFTER_TABLE`).
    after_table: Option<f32>,
    /// Where the text since the last page break the book asked for (or the start of the part) began: the page and the height.
    broke_at: (usize, f32),
}

impl Builder {
    /// The left edge of the lines of a paragraph of `style` and their room, its insets made smaller together when a large text would
    /// leave them most of the line.
    fn text_span(&self, style: &Style) -> (f32, f32) {
        let full = self.width - 2.0 * self.side;
        let insets = style.left + style.right;
        let shrink = if insets > full * MOST_INSET {
            full * MOST_INSET / insets
        } else {
            1.0
        };
        (
            self.side + style.left * shrink,
            (full - insets * shrink).max(SIZE * 4.0),
        )
    }

    /// The backdrop of a block of code from `top` down to `bottom` on the current page.
    fn shade(&mut self, top: f32, bottom: f32) {
        if let Some((colour, x, w)) = self.code
            && bottom > top
        {
            // one backdrop for the block on this page, grown line by line (a box a line left hairlines of the page between them)
            if let Some((index, foot)) = self.backdrop_box
                && (top - foot).abs() < 0.01
                && self.page.page.grow_rect(index, foot, bottom)
            {
                self.backdrop_box = Some((index, bottom));
                return;
            }
            self.page.page.rect(x, top, w, bottom - top, Some(colour), None);
            self.backdrop_box = self.page.page.last_item().map(|index| (index, bottom));
        }
    }

    /// A page break the book asks for (`Block::Break`): a new page, unless the text since the last one is short and on this page
    /// while `set_mark_paragraphs` is on (`SHORT_BREAK`); then the text goes on, and the next break is measured from here.
    fn page_break(&mut self) {
        let (page, y) = self.broke_at;
        let short = page == self.pages.len() && self.y - y < (self.bottom() - self.top) * SHORT_BREAK;
        if self.mark && short {
            self.broke_at = (self.pages.len(), self.y);
            return;
        }
        self.new_page();
        self.broke_at = (self.pages.len(), self.y);
    }

    /// A line across the text, set in by `left` and `right` points (`hr`), not at the top of a page.
    fn rule(&mut self, left: f32, right: f32) {
        if self.y <= self.top || self.full() {
            return;
        }
        let style = Style {
            left,
            right,
            ..Style::body()
        };
        let (x, width) = self.text_span(&style);
        if self.y + RULE_ROOM > self.bottom() {
            return;
        }
        let y = self.y + RULE_ROOM / 2.0;
        self.page.page.line((x, y), (x + width, y), TABLE_LINE, 0.75);
        self.y += RULE_ROOM;
    }

    /// The backdrop a paragraph of `style` is drawn on: a line of code's while blocks of code are drawn on one (its book's colour or
    /// the core's), the background a book gives a block of text (`background-color` of a box set apart).
    fn backdrop_of(&self, style: &Style) -> Option<[u8; 3]> {
        match (style.code, self.backdrop) {
            (true, true) => Some(style.shade.unwrap_or(CODE_SHADE)),
            (true, false) => None,
            (false, _) => style.shade,
        }
    }

    /// What comes next is (or is not) on a backdrop (a block of code, a box of text with a background): a block begins with room
    /// above it on its backdrop, and ends with room below it; the next with another colour or edges is a block of its own.
    fn code_or_not(&mut self, style: Option<&Style>) {
        let room = SIZE * CODE_ROOM;
        let wanted = style.and_then(|s| {
            let colour = self.backdrop_of(s)?;
            let (left, width) = self.text_span(s);
            let pad = SIZE * CODE_SIDE;
            Some((colour, left - pad, width + 2.0 * pad))
        });
        let end = |builder: &mut Self| {
            if builder.y > builder.top && builder.y + room <= builder.bottom() {
                builder.shade(builder.y, builder.y + room);
                builder.y += room;
            }
            builder.code = None;
        };
        match (wanted, self.code) {
            (Some(wanted), Some(now)) if wanted == now => {}
            (Some(wanted), now) => {
                if now.is_some() {
                    end(self);
                }
                self.code = Some(wanted);
                self.backdrop_box = None;
                if self.y > self.top && self.y + room < self.bottom() {
                    self.shade(self.y, self.y + room);
                    self.y += room;
                }
            }
            (None, Some(_)) => end(self),
            (None, None) => {}
        }
    }
    fn blank(&self) -> LaidPage {
        LaidPage {
            page: Page::new(self.width, self.height),
            words: Vec::new(),
            text: String::new(),
            place: self.words,
            numbered: true,
        }
    }

    fn full(&self) -> bool {
        self.pages.len() >= self.limit
    }

    /// Starts a new page, unless the current one is empty.
    fn new_page(&mut self) {
        if self.page.page.is_empty() {
            // nothing on it yet: it starts where the book is now
            self.page.place = self.words;
            return;
        }
        let blank = self.blank();
        let mut done = std::mem::replace(&mut self.page, blank);
        // its number is drawn with it (`drawn`), from where it is in the book
        done.numbered = !self.cover;
        self.backdrop_box = None;
        self.cover = false;
        self.after_table = None;
        self.pages.push(done);
        self.y = self.top;
    }

    /// The marks waiting lead to the current page.
    fn place_marks(&mut self) {
        let page = self.pages.len();
        for (depth, title) in self.pending.drain(..) {
            self.contents.push(ContentsItem {
                depth,
                title,
                page: Some(page),
            });
        }
    }

    fn bottom(&self) -> f32 {
        self.height - self.foot
    }

    /// The indent of the first line of a paragraph of `style`, in points: what the book gives (a negative one, a hanging indent, no
    /// further left than the page's margin), else the core's.
    fn indent_of(&self, style: &Style) -> f32 {
        let (left, room) = self.text_span(style);
        match (style.indent, style.indent_by) {
            (false, _) if style.unmarked && self.mark => INDENT * SIZE * style.scale,
            (false, _) => 0.0,
            (true, Some(points)) => points.clamp(self.side - left, room * 0.5),
            (true, None) => INDENT * SIZE * style.scale,
        }
    }

    fn paragraph(&mut self, spans: &[Span], style: &Style) {
        // room after a table, when the book gives less (Peekel TV, #235: the textbook's 4 px left the text touching the table's lines)
        if let Some(end) = self.after_table.take()
            && self.mark
            && self.y > self.top
        {
            self.y = self.y.max(end + ROOM_AFTER_TABLE);
        }
        // the book's sizes made the reader's (`size_factor`)
        let sized;
        let style = if (self.factor - 1.0).abs() > 0.001 {
            sized = Style {
                scale: style.scale * self.factor,
                indent_by: style.indent_by.map(|i| i * self.factor),
                ..style.clone()
            };
            &sized
        } else {
            style
        };
        let (_, room) = self.text_span(style);
        let indent = self.indent_of(style);
        let units = units_of(spans, style, room - indent);
        self.paragraph_of(units, spans.iter().all(|s| s.mono), style);
    }

    /// A paragraph of units already made (`units_of`; a table's row set as a line, `table`); `mono` when all of it is monospace.
    fn paragraph_of(&mut self, units: Vec<Unit>, mono: bool, style: &Style) {
        let justified;
        let style = if self.justify && style.loose && style.align == Align::Left {
            justified = Style {
                align: Align::Justify,
                ..style.clone()
            };
            &justified
        } else {
            style
        };
        let (left, room) = self.text_span(style);
        let size = SIZE * style.scale;
        let indent = self.indent_of(style);
        if units.is_empty() {
            return;
        }
        // a space of the text's font; a line of code (all monospace) has the monospace one, so its columns stay
        let text = Span {
            mono,
            ..Span::default()
        };
        let space = width(" ", &font_of(&text, style));
        let lines = break_lines(units, (room - indent, room), style.align == Align::Justify, space);

        let line_height = size * LEADING;
        if self.y > self.top {
            self.y += style.before;
        }
        // a heading is not left alone at the foot of a page: it goes with the next line
        let needed = lines.len() as f32 * line_height + if style.keep { self.follow } else { 0.0 };
        if style.keep && self.y + needed > self.bottom() && !self.page.page.is_empty() {
            self.new_page();
        }
        let count = lines.len();
        for (index, line) in lines.iter().enumerate() {
            if self.full() {
                return;
            }
            if self.y + line_height > self.bottom() && !self.page.page.is_empty() {
                self.new_page();
                if self.full() {
                    return;
                }
            }
            self.place_marks();
            let line_room = if index == 0 { room - indent } else { room };
            let start_x = left + if index == 0 { indent } else { 0.0 };
            let gaps = line[..line.len().saturating_sub(1)].iter().filter(|u| u.space).count();
            let natural: f32 = line.iter().map(|u| u.width).sum::<f32>() + gaps as f32 * space;
            let last = index + 1 == count;
            let mut gap = space;
            let mut x = start_x;
            match style.align {
                Align::Justify if !last && gaps > 0 => {
                    let stretched = space + (line_room - natural) / gaps as f32;
                    if stretched <= space * MOST_STRETCH {
                        gap = stretched.max(space);
                    }
                }
                Align::Center => x += ((line_room - natural) / 2.0).max(0.0),
                Align::Right => x += (line_room - natural).max(0.0),
                _ => {}
            }
            // a line of code on its backdrop, drawn under its text
            self.shade(self.y, self.y + line_height);
            let words_of_line = self.put_line(line, (x, self.y + size * 1.02), gap, size);
            if !words_of_line.is_empty() {
                if !self.page.text.is_empty() && !self.page.text.ends_with('\n') {
                    self.page.text.push(' ');
                }
                self.page.text.push_str(&words_of_line.join(" "));
            }
            self.y += line_height;
        }
        self.page.text.push('\n');
        self.y += style.after;
    }

    /// A table laid out as a grid (`grid`); one that does not fit the width of the text even at `TABLE_SMALLEST` of its size, or has a
    /// row taller than a page, is set a row a line, its cells parted by a bar, as before.
    /// The height of the start of a table as it will be laid out: its header and its first group of rows (no more than half a page);
    /// two lines for one set as text or a row a line.
    fn table_start(&self, table: &Table) -> f32 {
        let lines = SIZE * LEADING * 2.0;
        let (_, room) = self.text_span(&Style::verse(table.left));
        let page = self.bottom() - self.top;
        let Some(grid) = grid(table, room, page) else {
            return lines;
        };
        let mut end = grid.group_end(0);
        while end < grid.head {
            end = grid.group_end(end);
        }
        if end < grid.rows.len() && end == grid.head {
            end = grid.group_end(end);
        }
        grid.rows[..end].iter().sum::<f32>().clamp(lines, page / 2.0)
    }

    fn table(&mut self, table: &Table) {
        self.after_table = None;
        let style = Style::verse(table.left);
        let (left, room) = self.text_span(&style);
        // a table of one column is a book's way to set a block apart (an epigraph and its author), not a grid: its lines as text
        if table
            .rows
            .iter()
            .all(|r| r.cells.iter().map(|c| c.columns.max(1)).sum::<usize>() <= 1)
        {
            for cell in table.rows.iter().flat_map(|r| &r.cells) {
                for line in cell.text.split('\n') {
                    let span = Span {
                        text: line.to_string(),
                        bold: cell.bold,
                        ..Span::default()
                    };
                    self.paragraph(std::slice::from_ref(&span), &style);
                }
            }
            return;
        }
        match grid(table, room, self.bottom() - self.top) {
            Some(grid) => self.put_grid(&grid, left),
            None => {
                for row in &table.rows {
                    let font = font_of(&Span::default(), &style);
                    let mut units: Vec<Unit> = Vec::new();
                    for (i, cell) in row.cells.iter().enumerate() {
                        if i > 0 {
                            if let Some(last) = units.last_mut() {
                                last.space = true;
                            }
                            // the bar is no word of the text
                            units.push(Unit {
                                width: width(TABLE_BAR, &font),
                                pieces: vec![(TABLE_BAR.to_string(), font.clone(), 0)],
                                space: true,
                                more: true,
                                ..Unit::default()
                            });
                        }
                        let span = Span {
                            text: cell.text.replace('\n', " "),
                            ..Span::default()
                        };
                        units.extend(units_of(std::slice::from_ref(&span), &style, room));
                    }
                    self.paragraph_of(units, false, &style);
                }
            }
        }
    }

    /// The rows of a grid on the pages: a group of rows a cell spans is kept on one page; a page the table goes on to starts with its
    /// header rows again (drawn, not counted again among the words).
    fn put_grid(&mut self, grid: &Grid, left: f32) {
        let mut row = 0;
        let mut first = true;
        while row < grid.rows.len() {
            let end = grid.group_end(row);
            let height: f32 = grid.rows[row..end].iter().sum();
            if self.full() {
                return;
            }
            if self.y + height > self.bottom() && !self.page.page.is_empty() {
                self.new_page();
                if self.full() {
                    return;
                }
                // the header again, when it is not the rows that come next
                if row >= grid.head
                    && grid.head > 0
                    && self.y + grid.rows[..grid.head].iter().sum::<f32>() + height <= self.bottom()
                {
                    self.put_rows(grid, left, 0..grid.head, false);
                }
            }
            if first {
                self.place_marks();
                first = false;
            }
            self.put_rows(grid, left, row..end, true);
            row = end;
        }
        self.after_table = Some(self.y);
        self.y += SIZE * 0.4;
    }

    /// Rows `rows` of a grid at the current height: each cell's box and its lines; the words counted when `count` (not for a header
    /// drawn again).
    fn put_rows(&mut self, grid: &Grid, left: f32, rows: std::ops::Range<usize>, count: bool) {
        let top = self.y;
        let tops: Vec<f32> = grid
            .rows
            .iter()
            .scan(0.0, |y, h| {
                let at = *y;
                *y += h;
                Some(at)
            })
            .collect();
        let origin = tops.get(rows.start).copied().unwrap_or(0.0);
        let mut text_of_rows: Vec<Vec<String>> = vec![Vec::new(); rows.len()];
        for cell in grid.cells.iter().filter(|c| rows.contains(&c.row)) {
            let x = left + grid.lefts[cell.column];
            let y = top + tops[cell.row] - origin;
            let w = grid.lefts[cell.column + cell.columns] - grid.lefts[cell.column];
            let h: f32 = grid.rows[cell.row..cell.row + cell.rows].iter().sum();
            let shade = cell.head.then_some(TABLE_HEAD_SHADE);
            self.page.page.rect(x, y, w, h, shade, Some((TABLE_LINE, 0.5)));
            let size = SIZE * grid.scale;
            let line_height = size * LEADING;
            let space = width(" ", &cell.font);
            for (i, line) in cell.lines.iter().enumerate() {
                let natural = natural(line, space);
                let inner = w - 2.0 * TABLE_PAD;
                let shift = match cell.place {
                    Place::Left => 0.0,
                    Place::Centre => ((inner - natural) / 2.0).max(0.0),
                    Place::Number(block) => ((inner - block) / 2.0).max(0.0) + (block - natural).max(0.0),
                };
                let baseline = y + TABLE_VPAD + i as f32 * line_height + size * 1.02;
                let words_before = self.words;
                let words = self.put_line(line, (x + TABLE_PAD + shift, baseline), space, size);
                if !count {
                    self.words = words_before;
                }
                text_of_rows[cell.row - rows.start].extend(words);
            }
        }
        // a heavier line under the header rows (here, or drawn again at the top of a page)
        if grid.head > 0 && rows.contains(&(grid.head - 1)) {
            let y = top + tops[grid.head - 1] + grid.rows[grid.head - 1] - origin;
            let right = left + grid.lefts.last().copied().unwrap_or(0.0);
            self.page.page.line((left, y), (right, y), TABLE_LINE, TABLE_HEAD_LINE);
        }
        // the page's text: a row a line
        for words in text_of_rows.into_iter().filter(|w| !w.is_empty()) {
            if !self.page.text.is_empty() && !self.page.text.ends_with('\n') {
                self.page.text.push('\n');
            }
            self.page.text.push_str(&words.join(" "));
        }
        self.page.text.push('\n');
        self.y = top + grid.rows[rows].iter().sum::<f32>();
    }

    /// Draws a line of units from `x` on `baseline`, `gap` between the units a space follows, at the size `size` (for a raised piece
    /// and the boxes of the words); keeps each word and its box, and counts the words. The words for the page's text: a broken word
    /// once, whole, where its head is (or where its tail starts a page).
    fn put_line(&mut self, line: &[Unit], (mut x, baseline): (f32, f32), gap: f32, size: f32) -> Vec<String> {
        let mut words_of_line: Vec<String> = Vec::new();
        for (i, unit) in line.iter().enumerate() {
            let mut px = x;
            for (text, font, raised) in &unit.pieces {
                let lift = match *raised {
                    1 => size * 0.35,
                    -1 => -size * 0.12,
                    _ => 0.0,
                };
                self.page.page.text(px, baseline - lift, text, font);
                px += width(text, font);
            }
            let first_of_page = self.page.words.is_empty();
            let word = unit.word();
            if !unit.tail || first_of_page {
                words_of_line.push(word.clone());
            }
            if !unit.tail && !unit.more {
                self.words += 1;
            }
            self.page.words.push(Word {
                text: word,
                rect: Rect {
                    left: (x * UNITS) as i32,
                    top: ((baseline - size * 0.82) * UNITS) as i32,
                    right: ((x + unit.width) * UNITS).ceil() as i32,
                    bottom: ((baseline + size * 0.24) * UNITS).ceil() as i32,
                },
            });
            x += unit.width;
            if unit.space && i + 1 < line.len() {
                x += gap;
            }
        }
        words_of_line
    }

    fn picture(&mut self, image: Option<&Picture>) {
        self.after_table = None;
        let room = self.width - 2.0 * self.side;
        let tall = self.bottom() - self.top;
        let (w, h) = image.map_or((room / 2.0, room / 4.0), |i| {
            let (w, h) = i.dimensions();
            (w as f32, h as f32)
        });
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let shrink = (room / w).min(tall / h).min(1.0);
        let (w, h) = (w * shrink, h * shrink);
        let gap = SIZE * 0.5;
        if self.y + gap + h > self.bottom() && !self.page.page.is_empty() {
            self.new_page();
        }
        if self.full() {
            return;
        }
        self.place_marks();
        if self.y > self.top {
            self.y += gap;
        }
        let x = self.side + (room - w) / 2.0;
        match image {
            Some(image) => self.page.page.picture(x, self.y, w, h, image.clone()),
            None => self
                .page
                .page
                .rect(x, self.y, w, h, Some([235, 235, 235]), Some(([170, 170, 170], 0.75))),
        }
        self.y += h + gap;
    }

    fn cover(&mut self, image: &Picture) {
        self.new_page();
        if self.full() {
            return;
        }
        self.place_marks();
        let (w, h) = image.dimensions();
        let (w, h) = (w as f32, h as f32);
        if w > 0.0 && h > 0.0 {
            let scale = (self.width / w).min(self.height / h);
            let (w, h) = (w * scale, h * scale);
            self.page
                .page
                .picture((self.width - w) / 2.0, (self.height - h) / 2.0, w, h, image.clone());
            self.cover = true;
        }
        self.new_page();
    }
}

/// Lays `blocks` out into pages at the book's own size of text; stops after `limit` pages (the first page alone for a quick view).
pub fn lay_out(blocks: &[Block], limit: usize) -> Laid {
    lay_out_at(blocks, limit, 100)
}

/// The same with the text at `percent` of its size (`set_text_size`).
pub fn lay_out_at(blocks: &[Block], limit: usize, percent: u32) -> Laid {
    lay_out_on(blocks, limit, percent, PAGE)
}

/// The same on a page of `page` points at the book's own size of text (`page_for`), made smaller for a larger text as `PAGE` is.
///
/// The book is laid out part by part (`parts`: each starts a page, so its pages do not depend on those before it), and the parts are
/// put together (`joined`): the same pages as one pass over the whole book, and what lets a part be laid out alone first (`Growing`).
pub fn lay_out_on(blocks: &[Block], limit: usize, percent: u32, page: (f32, f32)) -> Laid {
    lay_out_with(blocks, limit, (percent, page, look()))
}

/// The same with whether blocks of code are drawn on a backdrop given (that of the key a book is kept by).
fn lay_out_with(blocks: &[Block], limit: usize, (percent, page, look): (u32, (f32, f32), Look)) -> Laid {
    let limit = limit.min(MAX_PAGES);
    let factor = size_factor(blocks, look);
    let mut laid = Vec::new();
    let mut pages = 0;
    for part in parts(blocks) {
        if pages >= limit {
            break;
        }
        let one = lay_out_part(blocks, &part, limit - pages, (percent, page, look), factor);
        pages += one.pages.len();
        laid.push(one);
    }
    joined(laid, words_in(blocks), percent, page)
}

/// A part of a book: its blocks (those after a `Block::NewPage`, up to the next), the marks of the contents that wait for its first
/// page (those at the end of the part before, which lead to what is laid out next), and the place it starts at.
#[derive(Debug, Clone, PartialEq)]
struct Part {
    blocks: std::ops::Range<usize>,
    carried: Vec<(usize, String)>,
    first_word: usize,
}

/// The parts of a book, in order; a stretch with nothing but marks gives none (its marks wait for the next part).
fn parts(blocks: &[Block]) -> Vec<Part> {
    let mut parts = Vec::new();
    let (mut carried, mut words) = (Vec::new(), 0usize);
    let mut close = |range: std::ops::Range<usize>, carried: &mut Vec<(usize, String)>, words: &mut usize| {
        let mut end = range.end;
        while end > range.start && matches!(blocks[end - 1], Block::Mark { .. }) {
            end -= 1;
        }
        let trailing: Vec<(usize, String)> = blocks[end..range.end]
            .iter()
            .filter_map(|b| match b {
                Block::Mark { depth, title } => Some((*depth, title.clone())),
                _ => None,
            })
            .collect();
        if end > range.start {
            parts.push(Part {
                blocks: range.start..end,
                carried: std::mem::take(carried),
                first_word: *words,
            });
            *words += words_in(&blocks[range.start..end]);
        }
        carried.extend(trailing);
    };
    let mut start = 0;
    for (i, block) in blocks.iter().enumerate() {
        if matches!(block, Block::NewPage) {
            close(start..i, &mut carried, &mut words);
            start = i + 1;
        }
    }
    close(start..blocks.len(), &mut carried, &mut words);
    parts
}

/// One part laid out alone, on pages of its own counted from 0, its words counted from the place it starts at; at most `limit` pages.
fn lay_out_part(
    blocks: &[Block],
    part: &Part,
    limit: usize,
    (percent, page, look): (u32, (f32, f32), Look),
    factor: f32,
) -> Laid {
    let k = percent.clamp(TEXT_SIZES.0, TEXT_SIZES.1) as f32 / 100.0;
    let mut builder = Builder {
        pages: Vec::new(),
        page: LaidPage {
            page: Page::new(page.0 / k, page.1 / k),
            words: Vec::new(),
            text: String::new(),
            place: part.first_word,
            numbered: true,
        },
        y: MARGIN_TOP / k,
        contents: Vec::new(),
        pending: part.carried.clone(),
        cover: false,
        limit: limit.min(MAX_PAGES),
        width: page.0 / k,
        height: page.1 / k,
        side: MARGIN_SIDE / k,
        top: MARGIN_TOP / k,
        foot: MARGIN_BOTTOM / k,
        words: part.first_word,
        backdrop: look.backdrop,
        justify: look.justify,
        mark: look.mark,
        factor,
        after_table: None,
        broke_at: (0, MARGIN_TOP / k),
        code: None,
        backdrop_box: None,
        follow: SIZE * LEADING * 2.0,
    };
    let part_blocks = blocks.get(part.blocks.clone()).unwrap_or_default();
    for (index, block) in part_blocks.iter().enumerate() {
        if builder.full() {
            break;
        }
        // what a paragraph kept with what follows it needs below it: two lines of text, or the header and first row of a table
        if let Block::Paragraph { style, .. } = block
            && style.keep
        {
            let next = part_blocks[index + 1..]
                .iter()
                .find(|b| !matches!(b, Block::Space(_) | Block::Mark { .. }));
            builder.follow = match next {
                Some(Block::Table(table)) => builder.table_start(table),
                _ => SIZE * LEADING * 2.0,
            };
        }
        // a space and a mark of the contents keep a block of code going (an empty line of it)
        match block {
            Block::Paragraph { style, .. } => builder.code_or_not(Some(style)),
            Block::Space(_) | Block::Mark { .. } => {}
            _ => builder.code_or_not(None),
        }
        match block {
            Block::Paragraph { spans, style } => builder.paragraph(spans, style),
            Block::Picture(image) => builder.picture(image.as_ref()),
            Block::Cover(image) => builder.cover(image),
            Block::Table(table) => builder.table(table),
            Block::Space(points) => {
                if builder.y > builder.top {
                    let to = (builder.y + points).min(builder.bottom());
                    builder.shade(builder.y, to);
                    builder.y += points;
                }
            }
            Block::Rule { left, right } => builder.rule(*left, *right),
            Block::NewPage => builder.new_page(),
            Block::Break => builder.page_break(),
            Block::Mark { depth, title } => builder.pending.push((*depth, title.clone())),
        }
    }
    if !builder.full() {
        builder.new_page();
    }
    Laid {
        pages: builder.pages,
        contents: builder.contents,
        size: (builder.width, builder.height),
        foot: builder.foot,
        parts: vec![0],
    }
}

/// The parts laid out put together into the book: their pages in order, the pages of the contents counted in the book; a book with no
/// page has one blank page, at the place of its end (`words`).
fn joined(parts: Vec<Laid>, words: usize, percent: u32, page: (f32, f32)) -> Laid {
    let k = percent.clamp(TEXT_SIZES.0, TEXT_SIZES.1) as f32 / 100.0;
    let mut book = Laid {
        pages: Vec::new(),
        contents: Vec::new(),
        size: (page.0 / k, page.1 / k),
        foot: MARGIN_BOTTOM / k,
        parts: Vec::new(),
    };
    for part in parts {
        let base = book.pages.len();
        book.parts.push(base);
        book.contents.extend(part.contents.into_iter().map(|item| ContentsItem {
            page: item.page.map(|p| p + base),
            ..item
        }));
        book.pages.extend(part.pages);
    }
    book.pages.truncate(MAX_PAGES);
    if book.pages.is_empty() {
        book.pages.push(LaidPage {
            page: Page::new(book.size.0, book.size.1),
            words: Vec::new(),
            text: String::new(),
            place: words,
            numbered: false,
        });
    }
    book
}

/// Page `index` of `laid` as it is drawn: with its number (`number`, from 0) at its foot when it has one, small and grey.
fn drawn(laid: &Laid, index: usize, number: Option<usize>) -> Result<Page, DecodeError> {
    let page = laid
        .pages
        .get(index)
        .ok_or_else(|| unreadable("the book has no pages"))?;
    let mut drawn = page.page.clone();
    if let Some(number) = number.filter(|_| page.numbered) {
        let number = (number + 1).to_string();
        let font = Font {
            size: SIZE * 0.8,
            colour: GREY,
            family: Some(SERIF.to_string()),
            ..Font::default()
        };
        let w = width(&number, &font);
        drawn.text((laid.size.0 - w) / 2.0, laid.size.1 - laid.foot / 2.0, &number, &font);
    }
    Ok(drawn)
}

/// The books laid out last, by their file (its path, size and time of change), the size of the text and the shape of the page: the
/// viewer asks for the pages one by one, the text and the contents, and the laying out of a long book takes a while (25 s for a book
/// of 1562 pages on a tablet, Peekel TV, #235). More than one is kept, so that a cover drawn at another shape, or a step of the text
/// size and back, does not throw away the book being read: the last `KEEP_LAID`, the newest first, as long as they weigh no more
/// than `KEEP_LAID_BYTES` together (`Laid::weight`); the newest is kept whatever it weighs.
type Key = (PathBuf, u64, Option<SystemTime>, u32, Option<(u32, u32)>, Look);
static LAID: Mutex<Vec<(Key, Arc<Laid>, usize)>> = Mutex::new(Vec::new());
const KEEP_LAID: usize = 3;
const KEEP_LAID_BYTES: usize = 256 * 1024 * 1024;

/// Keeps `laid` as the newest layout, dropping the oldest ones past the limits.
fn keep(key: Key, laid: &Arc<Laid>) {
    let weight = laid.weight();
    let Ok(mut kept) = LAID.lock() else {
        return;
    };
    kept.retain(|(k, _, _)| *k != key);
    kept.insert(0, (key, Arc::clone(laid), weight));
    let mut total = 0;
    let mut count = 0;
    kept.retain(|(_, _, w)| {
        total += w;
        count += 1;
        count == 1 || (count <= KEEP_LAID && total <= KEEP_LAID_BYTES)
    });
}

/// Lets go of the layouts kept (`laid`): for a reader told that memory is low, or before it tries a page again after "out of memory"
/// (Peekel TV, #235). A layout still in use by a caller lives until it is dropped there; the next page asked for lays its book out anew.
pub fn forget_laid() {
    if let Ok(mut kept) = LAID.lock() {
        kept.clear();
    }
    if let Ok(mut growing) = GROWING.lock() {
        *growing = None;
    }
    crate::page::forget_decoded();
}

/// A book being laid out part by part (`open_at`): its blocks, its parts, and those laid out so far. One book at a time, the one
/// opened last; `laid` lays out the rest of it and keeps it whole as any other.
struct Growing {
    key: Key,
    blocks: Arc<Vec<Block>>,
    parts: Vec<Part>,
    laid: Vec<Option<Arc<Laid>>>,
    words: usize,
    /// What the sizes of its paragraphs are multiplied by (`size_factor`).
    factor: f32,
}
static GROWING: Mutex<Option<Growing>> = Mutex::new(None);

/// A page of a book whose text flows, named by its part and its page in the part: what `open_at` and `turn_to` give, valid while the
/// size of the text and the shape of the page stay (else the reader opens the book again at the place it keeps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    part: usize,
    /// The page in the part; `usize::MAX`, its last.
    page: usize,
}

/// A page drawn for a reader that opens a book at its place before the whole of it is laid out (Peekel TV, #235, item 5.1).
#[derive(Debug)]
pub struct Shown {
    pub image: RgbaImage,
    /// Where the page starts in the text (`place_of`): what the reader keeps to open the book there again.
    pub place: usize,
    pub spot: Spot,
    /// The pages before and after it; `None` at the start and the end of the book.
    pub previous: Option<Spot>,
    pub next: Option<Spot>,
    /// Its number in the book from 0, and the number of pages, once the parts they need are laid out.
    pub number: Option<usize>,
    pub count: Option<usize>,
}

/// A book as far as it is laid out: whole (kept), or growing.
enum Book {
    Whole(Arc<Laid>),
    Growing,
}

/// The book at `path` for `open_at` and `turn_to`: the whole layout when it is kept, else the growing one (read now when it is not
/// the one growing).
fn book_of<F>(path: &Path, read: F) -> Result<(Key, Book), DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let key = key(path)?;
    if let Some(laid) = cached(&key) {
        return Ok((key, Book::Whole(laid)));
    }
    if GROWING
        .lock()
        .ok()
        .is_some_and(|g| g.as_ref().is_some_and(|g| g.key == key))
    {
        return Ok((key, Book::Growing));
    }
    let blocks = crate::guard::draw_on_thread("book", "the book could not be read", || read(path))?;
    let parts = parts(&blocks);
    let growing = Growing {
        key: key.clone(),
        words: words_in(&blocks),
        factor: size_factor(&blocks, key.5),
        blocks: Arc::new(blocks),
        laid: vec![None; parts.len()],
        parts,
    };
    if let Ok(mut slot) = GROWING.lock() {
        *slot = Some(growing);
    }
    Ok((key, Book::Growing))
}

/// Part `part` of the growing book of `key`, laid out now when it is not yet; `None` when that book is no longer the one growing
/// (laid out whole meanwhile, or another opened) or has no such part.
fn growing_part(key: &Key, part: usize) -> Result<Option<Arc<Laid>>, DecodeError> {
    let (blocks, which, factor) = {
        let Ok(growing) = GROWING.lock() else {
            return Ok(None);
        };
        let Some(g) = growing.as_ref().filter(|g| g.key == *key) else {
            return Ok(None);
        };
        let Some(which) = g.parts.get(part) else {
            return Ok(None);
        };
        if let Some(Some(laid)) = g.laid.get(part) {
            return Ok(Some(Arc::clone(laid)));
        }
        (Arc::clone(&g.blocks), which.clone(), g.factor)
    };
    let (percent, page) = (key.3, page_for(key.4));
    let laid = crate::guard::draw_on_thread("book", "the book could not be laid out", || {
        Ok(Arc::new(lay_out_part(
            &blocks,
            &which,
            MAX_PAGES,
            (percent, page, key.5),
            factor,
        )))
    })?;
    if let Ok(mut growing) = GROWING.lock()
        && let Some(g) = growing.as_mut().filter(|g| g.key == *key)
        && let Some(slot) = g.laid.get_mut(part)
    {
        slot.get_or_insert_with(|| Arc::clone(&laid));
    }
    Ok(Some(laid))
}

/// The layout the pages of a part are in, and their range there.
type PartPages = (Arc<Laid>, std::ops::Range<usize>);

/// The pages of part `part` of `book`: the layout they are in and their range there; `None` past the parts.
fn part_pages(key: &Key, book: &Book, part: usize) -> Result<Option<PartPages>, DecodeError> {
    match book {
        Book::Whole(laid) => Ok(laid.parts.get(part).map(|&start| {
            let end = laid.parts.get(part + 1).copied().unwrap_or(laid.pages.len());
            (Arc::clone(laid), start..end)
        })),
        Book::Growing => Ok(growing_part(key, part)?.map(|laid| {
            let pages = laid.pages.len();
            (laid, 0..pages)
        })),
    }
}

/// How many parts `book` has.
fn part_count(key: &Key, book: &Book) -> usize {
    match book {
        Book::Whole(laid) => laid.parts.len(),
        Book::Growing => GROWING
            .lock()
            .ok()
            .and_then(|g| g.as_ref().filter(|g| g.key == *key).map(|g| g.parts.len()))
            .unwrap_or(0),
    }
}

/// The number of the first page of part `part` in the book and the number of pages, as far as they are known: the pages of the parts
/// before it, of all of them.
fn numbers(key: &Key, book: &Book, part: usize) -> (Option<usize>, Option<usize>) {
    match book {
        Book::Whole(laid) => (laid.parts.get(part).copied(), Some(laid.pages.len())),
        Book::Growing => {
            let Ok(growing) = GROWING.lock() else {
                return (None, None);
            };
            let Some(g) = growing.as_ref().filter(|g| g.key == *key) else {
                return (None, None);
            };
            let count = |laid: &[Option<Arc<Laid>>]| {
                laid.iter()
                    .map(|l| l.as_ref().map(|l| l.pages.len()))
                    .sum::<Option<usize>>()
            };
            (
                count(&g.laid[..part.min(g.laid.len())]),
                count(&g.laid).map(|c| c.max(1)),
            )
        }
    }
}

/// The page that `spot` names, found in the parts next to it when its own has no page (a part of marks and space), drawn for the box.
fn show(key: &Key, book: &Book, spot: Spot, max_width: usize, max_height: usize) -> Result<Shown, DecodeError> {
    let key = key.clone();
    let parts = part_count(&key, book);
    if parts == 0 {
        // a book with no part: its one blank page, as `laid` gives it
        let laid = Arc::new(joined(Vec::new(), 0, key.3, page_for(key.4)));
        return shown_page(
            &laid,
            0,
            Spot { part: 0, page: 0 },
            None,
            None,
            (None, Some(1)),
            (max_width, max_height),
        );
    }
    // a spot past the end is the last page; a part with no page passes on to the next (or, for its last page, the one before)
    let mut part = spot.part.min(parts - 1);
    let mut last = spot.page == usize::MAX || spot.part >= parts;
    let mut index = spot.page;
    loop {
        let (laid, range) =
            part_pages(&key, book, part)?.ok_or_else(|| unreadable("the book could not be laid out"))?;
        if !range.is_empty() {
            let local = if last {
                range.len() - 1
            } else {
                index.min(range.len() - 1)
            };
            let previous = if local > 0 {
                Some(Spot { part, page: local - 1 })
            } else {
                (part > 0).then(|| Spot {
                    part: part - 1,
                    page: usize::MAX,
                })
            };
            let next = if local + 1 < range.len() {
                Some(Spot { part, page: local + 1 })
            } else {
                (part + 1 < parts).then_some(Spot {
                    part: part + 1,
                    page: 0,
                })
            };
            let (first, count) = numbers(&key, book, part);
            return shown_page(
                &laid,
                range.start + local,
                Spot { part, page: local },
                previous,
                next,
                (first.map(|f| f + local), count),
                (max_width, max_height),
            );
        }
        match (last, part) {
            (true, 0) => {
                last = false;
                index = 0;
            }
            (true, _) => part -= 1,
            (false, p) if p + 1 < parts => {
                part += 1;
                index = 0;
            }
            (false, _) => {
                last = true;
                if part == 0 {
                    let laid = Arc::new(joined(Vec::new(), 0, key.3, page_for(key.4)));
                    return shown_page(
                        &laid,
                        0,
                        Spot { part: 0, page: 0 },
                        None,
                        None,
                        (None, Some(1)),
                        (max_width, max_height),
                    );
                }
                part -= 1;
            }
        }
    }
}

fn shown_page(
    laid: &Laid,
    index: usize,
    spot: Spot,
    previous: Option<Spot>,
    next: Option<Spot>,
    (number, count): (Option<usize>, Option<usize>),
    (max_width, max_height): (usize, usize),
) -> Result<Shown, DecodeError> {
    let page = laid
        .pages
        .get(index)
        .ok_or_else(|| unreadable("the book has no pages"))?;
    Ok(Shown {
        image: drawn(laid, index, number)?.render_within(max_width, max_height)?,
        place: page.place,
        spot,
        previous,
        next,
        number,
        count,
    })
}

/// The page of the book at `path` that place `place` (`place_of`, kept by the reader) is on, drawn for the box `max_width` x
/// `max_height`, laying out only the part of the book it is in when the book is not laid out yet (a chapter: what follows a
/// `Block::NewPage`); the other parts are laid out when they are turned to, or all of them by anything that needs the whole book
/// (`laid`: the number of pages, the contents, a search), which a reader may call on a thread of its own meanwhile. A place past the
/// end is the last page.
pub fn open_at<F>(path: &Path, read: F, place: usize, max_width: usize, max_height: usize) -> Result<Shown, DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let (key, book) = book_of(path, read)?;
    let spot = match &book {
        Book::Whole(laid) => {
            let index = page_at(laid, place);
            let part = laid.parts.partition_point(|&start| start <= index).saturating_sub(1);
            Spot {
                part,
                page: index - laid.parts.get(part).copied().unwrap_or(0),
            }
        }
        Book::Growing => {
            let firsts: Vec<usize> = GROWING
                .lock()
                .ok()
                .and_then(|g| {
                    g.as_ref()
                        .filter(|g| g.key == key)
                        .map(|g| g.parts.iter().map(|p| p.first_word).collect())
                })
                .unwrap_or_default();
            // the first part that starts there (a part of a picture comes before the text that follows it), else the last before
            let first = firsts.partition_point(|&w| w < place);
            let part = match firsts.get(first) {
                Some(&w) if w == place => first,
                _ => first.saturating_sub(1),
            };
            match part_pages(&key, &book, part)? {
                Some((laid, range)) => {
                    let pages = &laid.pages[range];
                    let first = pages.partition_point(|p| p.place < place);
                    let page = match pages.get(first) {
                        Some(p) if p.place == place => first,
                        _ => first.saturating_sub(1),
                    };
                    Spot { part, page }
                }
                None => Spot { part, page: 0 },
            }
        }
    };
    show(&key, &book, spot, max_width, max_height)
}

/// The page `spot` names (the `previous` or `next` of a page shown), drawn for the box; see [`open_at`].
pub fn turn_to<F>(path: &Path, read: F, spot: Spot, max_width: usize, max_height: usize) -> Result<Shown, DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let (key, book) = book_of(path, read)?;
    show(&key, &book, spot, max_width, max_height)
}

/// The rest of the growing book of `key` laid out and put together with what is laid out of it; `None` when it is not the one growing.
fn finish_growing(key: &Key) -> Result<Option<Laid>, DecodeError> {
    let parts = {
        let Ok(growing) = GROWING.lock() else {
            return Ok(None);
        };
        match growing.as_ref().filter(|g| g.key == *key) {
            Some(g) => g.parts.len(),
            None => return Ok(None),
        }
    };
    let mut pages = 0usize;
    for part in 0..parts {
        if pages >= MAX_PAGES {
            break;
        }
        match growing_part(key, part)? {
            Some(one) => pages += one.pages.len(),
            None => return Ok(None),
        }
    }
    // taken out, so that each part is moved into the whole and not copied (unless a page of it is being drawn just now)
    let growing = GROWING.lock().ok().and_then(|mut g| {
        if g.as_ref().is_some_and(|g| g.key == *key) {
            g.take()
        } else {
            None
        }
    });
    let Some(growing) = growing else {
        return Ok(None);
    };
    let laid: Vec<Laid> = growing.laid.into_iter().flatten().map(Arc::unwrap_or_clone).collect();
    Ok(Some(joined(laid, growing.words, key.3, page_for(key.4))))
}

fn key(path: &Path) -> Result<Key, DecodeError> {
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    Ok((
        path.to_path_buf(),
        meta.len(),
        meta.modified().ok(),
        text_size(),
        page_shape(),
        look(),
    ))
}

/// The layout kept for this key, made the newest.
fn cached(key: &Key) -> Option<Arc<Laid>> {
    let mut kept = LAID.lock().ok()?;
    let index = kept.iter().position(|(k, _, _)| k == key)?;
    let entry = kept.remove(index);
    let laid = Arc::clone(&entry.1);
    kept.insert(0, entry);
    Some(laid)
}

/// The book at `path` laid out whole, read by `read` (the reader of its format) when it is not the one laid out last. On a thread with
/// a large stack, so that a file made to hurt is an error and not the end of the caller.
pub fn laid<F>(path: &Path, read: F) -> Result<Arc<Laid>, DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let key = key(path)?;
    if let Some(laid) = cached(&key) {
        return Ok(laid);
    }
    // the book opened at its place (`open_at`): what is laid out of it is used, the rest laid out now
    if let Some(laid) = finish_growing(&key)? {
        let laid = Arc::new(laid);
        keep(key, &laid);
        return Ok(laid);
    }
    let (percent, page) = (key.3, page_for(key.4));
    let laid = crate::guard::draw_on_thread("book", "the book could not be laid out", || {
        Ok(Arc::new(lay_out_with(&read(path)?, usize::MAX, (percent, page, key.5))))
    })?;
    keep(key, &laid);
    Ok(laid)
}

/// The first page alone (a quick view): only as much of the book is laid out as that page needs.
pub fn first_page<F>(path: &Path, read: F) -> Result<RgbaImage, DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let key = key(path)?;
    if let Some(laid) = cached(&key) {
        return drawn(&laid, 0, Some(0)).map_err(|_| DecodeError::NotAPicture)?.render();
    }
    let (percent, page) = (key.3, page_for(key.4));
    crate::guard::draw_on_thread("book", "the book could not be laid out", || {
        let laid = lay_out_with(&read(path)?, 1, (percent, page, key.5));
        drawn(&laid, 0, Some(0)).map_err(|_| DecodeError::NotAPicture)?.render()
    })
}

/// Page `page` (from 0; past the end, the last) and the number of pages.
pub fn page(laid: &Laid, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    let count = laid.pages.len();
    Ok((laid_page(laid, page)?.render()?, count))
}

/// As [`page`], but drawn at the size at which it fits the box `max_width` x `max_height` pixels (`Page::render_within`): for a
/// program that shows the page on a screen of its own (Peekel TV), smaller for a column of a TV, larger for an upright phone.
pub fn page_within(
    laid: &Laid,
    page: usize,
    max_width: usize,
    max_height: usize,
) -> Result<(RgbaImage, usize), DecodeError> {
    let count = laid.pages.len();
    Ok((laid_page(laid, page)?.render_within(max_width, max_height)?, count))
}

/// Page `page` of the whole book (past the end, the last) as it is drawn, with its number.
fn laid_page(laid: &Laid, page: usize) -> Result<Page, DecodeError> {
    let index = page.min(laid.pages.len().saturating_sub(1));
    drawn(laid, index, Some(index))
}

/// The text of every page, for a search; `None` when the book has no text at all.
pub fn texts(laid: &Laid) -> Option<Vec<String>> {
    let texts: Vec<String> = laid.pages.iter().map(|p| p.text.clone()).collect();
    texts.iter().any(|t| !t.trim().is_empty()).then_some(texts)
}

/// The contents, `None` when the book has none.
pub fn contents(laid: &Laid) -> Option<Vec<ContentsItem>> {
    (!laid.contents.is_empty()).then(|| laid.contents.clone())
}

/// Marks on `image` (page `page` as [`page`] drew it) the words that `needle` is in; how many.
pub fn mark_words(laid: &Laid, page: usize, needle: &str, image: &mut RgbaImage) -> usize {
    laid.pages.get(page).map_or(0, |p| {
        crate::djvu::paint_found(&(locate(&p.words, needle), laid.units()), image)
    })
}

/// The place of page `page` in the book (`LaidPage::place`; past the end, that of the last).
pub fn place_of(laid: &Laid, page: usize) -> usize {
    laid.pages
        .get(page.min(laid.pages.len().saturating_sub(1)))
        .map_or(0, |p| p.place)
}

/// The page that place `place` is on: the first that starts there (a cover, a page of a picture, come before the text that follows
/// them), else the last that starts before it.
pub fn page_at(laid: &Laid, place: usize) -> usize {
    let first = laid.pages.partition_point(|p| p.place < place);
    match laid.pages.get(first) {
        Some(p) if p.place == place => first,
        _ => first.saturating_sub(1),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn text(t: &str) -> Block {
        Block::Paragraph {
            spans: vec![Span {
                text: t.to_string(),
                ..Span::default()
            }],
            style: Style::body(),
        }
    }

    fn cell(text: &str) -> Cell {
        Cell {
            text: text.to_string(),
            columns: 1,
            rows: 1,
            head: false,
            bold: false,
        }
    }

    fn row(cells: Vec<Cell>) -> Row {
        Row { cells, head: false }
    }

    #[test]
    fn a_table_is_a_grid_its_columns_as_wide_as_they_need() {
        let mut head = row(vec![
            Cell {
                head: true,
                bold: true,
                ..cell("Name")
            },
            Cell {
                head: true,
                bold: true,
                ..cell("What it is")
            },
        ]);
        head.head = true;
        let table = Table {
            rows: vec![head, row(vec![cell("ab"), cell(&"a long description ".repeat(12))])],
            left: 0.0,
        };
        let room = PAGE.0 - 2.0 * MARGIN_SIDE;
        let grid = grid(&table, room, 1000.0).expect("fits");
        assert_eq!(grid.lefts.len(), 3);
        // the whole room, the long column the wider; the long cell wrapped, its row higher
        assert!(
            (grid.lefts[2] - room).abs() < 0.01 && grid.lefts[1] < room / 3.0,
            "{:?}",
            grid.lefts
        );
        assert!(
            grid.cells[3].lines.len() > 2 && grid.rows[1] > grid.rows[0] * 2.0,
            "{:?}",
            grid.rows
        );
        assert_eq!((grid.head, grid.scale), (1, TABLE_TEXT));
        // a table of short cells is no wider than it needs
        let small = Table {
            rows: vec![row(vec![cell("a"), cell("b")])],
            left: 0.0,
        };
        assert!(grid_of(&small, room).lefts[2] < room / 4.0);
    }

    fn grid_of(table: &Table, room: f32) -> Grid {
        grid(table, room, 1000.0).expect("fits")
    }

    #[test]
    fn a_long_word_in_a_cell_is_whole_when_the_table_has_room_and_hyphenated_when_not_cut() {
        // the routing table of Peekel TV's report: "маршрутизато" / "ра" was cut while the last column was half empty
        let bold = |t: &str| Cell {
            bold: true,
            head: true,
            ..cell(t)
        };
        let mut head = row(vec![
            bold("Номер сети"),
            bold("Адрес следующего маршрутизатора"),
            bold("Порт"),
            bold("Расстояние"),
        ]);
        head.head = true;
        let table = Table {
            rows: vec![
                head,
                row(vec![cell("202.101.15.0"), cell("202.101.16.1"), cell("1"), cell("2")]),
            ],
            left: 0.0,
        };
        let pieces = |grid: &Grid| -> Vec<(String, bool, bool)> {
            grid.cells
                .iter()
                .flat_map(|c| c.lines.iter().flatten())
                .map(|u| (u.text(), u.more, u.head))
                .collect()
        };
        // the width of a page at the book's text size: every word whole, nothing cut
        let wide = grid_of(&table, PAGE.0 - 2.0 * MARGIN_SIDE);
        assert!(
            pieces(&wide).iter().all(|(_, more, head)| !more && !head),
            "{:?}",
            pieces(&wide)
        );
        assert!(pieces(&wide).iter().any(|(t, _, _)| t == "маршрутизатора"));
        // at 120 % (the tablet's) the word is too long for any share of the width: broken with a hyphen, nothing cut
        let tablet = grid_of(&table, (PAGE.0 - 2.0 * MARGIN_SIDE) / 1.2);
        assert!(
            pieces(&tablet).iter().all(|(_, more, _)| !more),
            "{:?}",
            pieces(&tablet)
        );
        // a word longer than `TABLE_WORD` in any font (a machine without the fonts measures by the rough table, and there a table is
        // made smaller before that header word is broken): in a room too small for it, found by narrowing, it is broken with a
        // hyphen, as in the text, not cut
        let long = Table {
            rows: vec![row(vec![cell("сеть"), cell("превысокомногорассмотрительствующий")])],
            left: 0.0,
        };
        let narrow = (100..=344)
            .rev()
            .step_by(4)
            .filter_map(|room| grid(&long, room as f32, 1000.0))
            .find(|g| pieces(g).iter().any(|(_, more, head)| *more || *head))
            .expect("a room where the word is broken");
        let broken: Vec<(String, bool, bool)> = pieces(&narrow)
            .into_iter()
            .filter(|(_, more, head)| *more || *head)
            .collect();
        assert!(
            broken.iter().all(|(t, more, head)| !more && *head && t.ends_with('-')),
            "{broken:?}"
        );
    }

    #[test]
    fn numbers_stand_in_a_block_centred_in_their_column_and_the_header_is_shaded_with_a_line_under_it() {
        assert_eq!(
            [
                "1",
                "-2,5",
                "12 345",
                "3.14",
                "50 %",
                "\u{2014}",
                "202.101.15.0",
                "01.02.2020",
                "v1",
                "1-2",
                ""
            ]
            .map(number_like),
            [
                Some(true),
                Some(true),
                Some(true),
                Some(true),
                Some(true),
                Some(false),
                None,
                None,
                None,
                None,
                None
            ]
        );
        let mut head = row(vec![
            Cell {
                head: true,
                bold: true,
                ..cell("Номер сети")
            },
            Cell {
                head: true,
                bold: true,
                ..cell("Расстояние")
            },
            Cell {
                head: true,
                bold: true,
                ..cell("Год")
            },
        ]);
        head.head = true;
        let table = Table {
            rows: vec![
                head,
                row(vec![cell("202.101.15.0"), cell("1"), cell("1998")]),
                row(vec![cell("202.101.16.0"), cell("\u{2014}"), cell("12")]),
            ],
            left: 0.0,
        };
        let grid = grid_of(&table, PAGE.0 - 2.0 * MARGIN_SIDE);
        let places: Vec<Place> = grid.cells.iter().map(|c| c.place).collect();
        assert_eq!(places[..3], [Place::Centre; 3]);
        // addresses to the left; one digit and a dash in one block; years of two lengths in a block as wide as the longer
        assert_eq!((places[3], places[6]), (Place::Left, Place::Left));
        let block = |i: usize| match places[i] {
            Place::Number(b) => b,
            other => panic!("{other:?}"),
        };
        assert!(
            block(4) == block(7) && block(5) == block(8) && block(5) > block(4),
            "{places:?}"
        );
        // on the page: a shade behind each header cell, a line under the header
        let laid = lay_out(&[Block::Table(table.clone())], usize::MAX);
        assert_eq!(laid.pages[0].page.filled(TABLE_HEAD_SHADE), 3);
        assert_eq!(laid.pages[0].page.lines_of(TABLE_HEAD_LINE), 1);
        // a header the book sets as a first row of bold cells (`<td><b>`): a header too, shaded, its cells where the book puts them
        let mut bold_row = table.clone();
        for cell in &mut bold_row.rows[0].cells {
            cell.head = false;
        }
        bold_row.rows[0].head = false;
        let grid = grid_of(&bold_row, PAGE.0 - 2.0 * MARGIN_SIDE);
        assert_eq!(grid.head, 1);
        assert!(grid.cells[..3].iter().all(|c| c.head && c.place == Place::Left));
        assert!(matches!(grid.cells[4].place, Place::Number(_)));
        let laid = lay_out(&[Block::Table(bold_row.clone())], usize::MAX);
        assert_eq!(laid.pages[0].page.filled(TABLE_HEAD_SHADE), 3);
        // not when a cell of that row is not bold
        bold_row.rows[0].cells[1].bold = false;
        assert_eq!(grid_of(&bold_row, PAGE.0 - 2.0 * MARGIN_SIDE).head, 0);
    }

    #[test]
    fn cells_that_span_columns_and_rows_take_their_places() {
        let wide = Cell {
            columns: 2,
            ..cell("both")
        };
        let tall = Cell {
            rows: 2,
            ..cell("tall")
        };
        let table = Table {
            rows: vec![
                row(vec![tall, wide]),
                row(vec![cell("x"), cell("y")]),
                row(vec![cell("p"), cell("q"), cell("r")]),
            ],
            left: 0.0,
        };
        let grid = grid_of(&table, 300.0);
        let at: Vec<(usize, usize, usize, usize)> = grid
            .cells
            .iter()
            .map(|c| (c.row, c.column, c.rows, c.columns))
            .collect();
        assert_eq!(
            at,
            [
                (0, 0, 2, 1),
                (0, 1, 1, 2),
                (1, 1, 1, 1),
                (1, 2, 1, 1),
                (2, 0, 1, 1),
                (2, 1, 1, 1),
                (2, 2, 1, 1)
            ]
        );
        // the two rows the tall cell spans are kept together
        assert_eq!((grid.group_end(0), grid.group_end(2)), (2, 3));
    }

    #[test]
    fn a_wide_table_is_made_smaller_and_a_word_too_long_for_its_column_is_cut() {
        let words = |n: usize| (0..n).map(|i| cell(&format!("Column{i}"))).collect::<Vec<_>>();
        let room = PAGE.0 - 2.0 * MARGIN_SIDE;
        // too wide at its size, not at 0.7 of it
        let mut columns = 1;
        let smaller = loop {
            let table = Table {
                rows: vec![row(words(columns))],
                left: 0.0,
            };
            let grid = grid_of(&table, room);
            if grid.scale < TABLE_TEXT {
                break grid;
            }
            columns += 1;
        };
        assert!(smaller.scale >= TABLE_SMALLEST && (smaller.lefts[columns] - room).abs() < 0.01);
        // past that, none (the table is set a row a line)
        let table = Table {
            rows: vec![row(words(columns * 3))],
            left: 0.0,
        };
        assert!(grid(&table, room, 1000.0).is_none());
        // an address longer than `TABLE_WORD` does not take the room of the others: it is cut in its cell
        let table = Table {
            rows: vec![row(vec![cell("short"), cell(&"www.example.org/".repeat(6))])],
            left: 0.0,
        };
        let grid = grid_of(&table, room);
        assert_eq!(grid.scale, TABLE_TEXT);
        assert!(grid.cells[1].lines.len() > 1 && grid.cells[1].lines.iter().all(|l| l.len() == 1));
    }

    #[test]
    fn a_long_table_goes_on_to_the_next_page_with_its_header_and_its_words_are_counted_once() {
        let mut head = row(vec![
            Cell {
                head: true,
                bold: true,
                ..cell("Key")
            },
            Cell {
                head: true,
                bold: true,
                ..cell("Value")
            },
        ]);
        head.head = true;
        let mut rows = vec![head];
        rows.extend((0..80).map(|i| row(vec![cell(&format!("k{i}")), cell(&format!("v{i} more"))])));
        let blocks = vec![text("Before."), Block::Table(Table { rows, left: 0.0 }), text("After.")];
        let laid = lay_out(&blocks, usize::MAX);
        assert!(laid.pages.len() > 2, "{} pages", laid.pages.len());
        // each page of it starts with the header; every row once, in order
        for page in &laid.pages[1..laid.pages.len() - 1] {
            assert!(
                page.text.starts_with("Key Value\n"),
                "{:?}",
                &page.text[..20.min(page.text.len())]
            );
        }
        let all: String = laid.pages.iter().map(|p| p.text.as_str()).collect();
        for i in 0..80 {
            assert_eq!(all.matches(&format!("k{i} v{i} more\n")).count(), 1, "row {i}");
        }
        // the places: the words of the text, the header once (as `words_in` counts them before the laying out)
        let last = laid.pages.last().expect("pages");
        assert!(last.text.contains("After."));
        assert_eq!(words_in(&blocks), 1 + 2 + 80 * 3 + 1);
        let parts = parts(&blocks);
        assert_eq!(parts.len(), 1);
        assert!(laid.pages.windows(2).all(|p| p[0].place <= p[1].place));
        // and a page that starts inside the table starts at the place of its first row of the text, not of the header drawn again
        let second = &laid.pages[1];
        let first_key = second
            .text
            .lines()
            .nth(1)
            .and_then(|l| l.split(' ').next())
            .unwrap_or("");
        let row: usize = first_key.trim_start_matches('k').parse().expect("a row");
        assert_eq!(second.place, 1 + 2 + row * 3);
    }

    #[test]
    fn a_table_of_one_column_is_set_as_text() {
        let table = Table {
            rows: vec![
                row(vec![cell("A saying,\nin two lines.")]),
                row(vec![cell("Its author.")]),
            ],
            left: 0.0,
        };
        let laid = lay_out(&[Block::Table(table)], usize::MAX);
        assert_eq!(laid.pages[0].text, "A saying,\nin two lines.\nIts author.\n");
        assert_eq!(laid.pages.len(), 1);
    }

    #[test]
    fn the_size_most_of_a_books_text_is_set_in_is_made_the_readers_only_while_asked() {
        let paragraph = |text: &str, scale: f32| Block::Paragraph {
            spans: vec![Span {
                text: text.to_string(),
                ..Span::default()
            }],
            style: Style { scale, ..Style::body() },
        };
        // a book that sets its text at 120 %, its headings larger, a note smaller (Wikisource)
        let blocks = vec![
            paragraph("A Heading", 2.0),
            paragraph(&"text of the book ".repeat(40), 1.2),
            paragraph(&"more text ".repeat(40), 1.2),
            paragraph("a note", 0.9),
        ];
        assert!((body_scale(&blocks) - 1.2).abs() < 0.001);
        let look = |normalize: bool| Look {
            backdrop: false,
            justify: true,
            mark: true,
            normalize,
        };
        let height = |normalize: bool| -> i32 {
            let laid = lay_out_with(&blocks, usize::MAX, (100, PAGE, look(normalize)));
            let word = laid.pages[0].words.iter().find(|w| w.text == "text").expect("a word");
            word.rect.bottom - word.rect.top
        };
        // the text at the reader's size while asked, at the book's 120 % when not; FB2's sizes (1) are left as they are
        let ratio = height(false) as f32 / height(true) as f32;
        assert!((ratio - 1.2).abs() < 0.03, "{ratio}");
        assert!((size_factor(&[paragraph("plain", 1.0)], look(true)) - 1.0).abs() < 0.001);
    }

    #[test]
    fn the_text_after_a_table_has_room_below_it_only_while_asked_and_a_book_giving_more_keeps_its_own() {
        let table = Block::Table(Table {
            rows: vec![Row {
                cells: vec![
                    Cell {
                        text: "a".to_string(),
                        columns: 1,
                        rows: 1,
                        head: false,
                        bold: false,
                    },
                    Cell {
                        text: "b".to_string(),
                        columns: 1,
                        rows: 1,
                        head: false,
                        bold: false,
                    },
                ],
                head: false,
            }],
            left: 0.0,
        });
        // the room from the table's last line to the top of the next word, with the book's space after the table (4 px)
        let room = |space: f32, mark: bool| -> f32 {
            let blocks = vec![text("Before."), table.clone(), Block::Space(space), text("After.")];
            let look = Look {
                backdrop: false,
                justify: true,
                mark,
                normalize: true,
            };
            let laid = lay_out_with(&blocks, usize::MAX, (100, PAGE, look));
            let words = &laid.pages[0].words;
            let b = words.iter().find(|w| w.text == "b").expect("the cell");
            let after = words.iter().find(|w| w.text == "After.").expect("the text");
            (after.rect.top - b.rect.bottom) as f32 / UNITS
        };
        let (asked, strict) = (room(3.0, true), room(3.0, false));
        assert!(asked > strict + SIZE * 0.3, "{asked} {strict}");
        // a book that gives more than the least keeps its own room
        assert!((room(3.0 * SIZE, true) - room(3.0 * SIZE, false)).abs() < 0.01);
    }

    #[test]
    fn a_box_of_text_is_drawn_on_its_background_and_a_rule_across_the_text() {
        let shaded = |text: &str, shade: Option<[u8; 3]>| Block::Paragraph {
            spans: vec![Span {
                text: text.to_string(),
                ..Span::default()
            }],
            style: Style { shade, ..Style::body() },
        };
        let blocks = vec![
            shaded("Before.", None),
            shaded("One.", Some([230, 230, 250])),
            shaded("Two.", Some([230, 230, 250])),
            Block::Rule { left: 0.0, right: 0.0 },
            shaded("After.", None),
        ];
        // the backdrop of code off: a book's background of a box is drawn all the same, one panel for its two paragraphs
        let look = Look {
            backdrop: false,
            justify: true,
            mark: true,
            normalize: true,
        };
        let laid = lay_out_with(&blocks, usize::MAX, (100, PAGE, look));
        assert_eq!(laid.pages[0].page.filled([230, 230, 250]), 1);
        assert_eq!(laid.pages[0].page.lines_of(0.75), 1);
    }

    #[test]
    fn small_capitals_are_capitals_a_size_smaller_and_a_word_keeps_its_place() {
        let spans = [Span {
            text: "Tolstoy wrote".to_string(),
            small_caps: true,
            ..Span::default()
        }];
        let units = units_of(&spans, &Style::body(), 1000.0);
        assert_eq!(units.len(), 2);
        let pieces: Vec<(&str, f32)> = units[0].pieces.iter().map(|(t, f, _)| (t.as_str(), f.size)).collect();
        assert_eq!(pieces, [("T", SIZE), ("OLSTOY", SIZE * SMALL_CAPS)]);
        assert_eq!(units[1].text(), "WROTE");
        // a sans face and a size of its own
        let font = font_of(
            &Span {
                sans: true,
                scale: Some(0.8),
                ..Span::default()
            },
            &Style::body(),
        );
        assert_eq!((font.family.as_deref(), font.size), (Some(SANS), SIZE * 0.8));
    }

    #[test]
    fn a_paragraph_a_book_sets_left_is_justified_only_while_the_setting_asks_and_its_indent_is_the_books() {
        let words = "the quick brown fox jumps over a lazy dog and runs ".repeat(12);
        let paragraph = |align: Align, loose: bool| Block::Paragraph {
            spans: vec![Span {
                text: words.clone(),
                ..Span::default()
            }],
            style: Style {
                align,
                loose,
                indent_by: Some(3.0 * SIZE),
                ..Style::body()
            },
        };
        // the right edge of the first line, and the left edge of the first word
        let edges = |block: Block, justify: bool| -> (i32, i32) {
            let laid = lay_out_with(
                &[block],
                usize::MAX,
                (
                    100,
                    PAGE,
                    Look {
                        backdrop: false,
                        justify,
                        mark: true,
                        normalize: true,
                    },
                ),
            );
            let page = &laid.pages[0].words;
            let first_line: Vec<&Word> = page.iter().filter(|w| w.rect.top == page[0].rect.top).collect();
            (first_line.last().map_or(0, |w| w.rect.right), page[0].rect.left)
        };
        let (justified, indented) = edges(paragraph(Align::Left, true), true);
        let (ragged, _) = edges(paragraph(Align::Left, true), false);
        let (kept, _) = edges(paragraph(Align::Left, false), true);
        let full = ((PAGE.0 - MARGIN_SIDE) * UNITS) as i32;
        assert!(
            (full - justified).abs() < (UNITS * 2.0) as i32,
            "{justified} against {full}"
        );
        assert!(ragged < justified && kept == ragged, "{ragged} {kept} {justified}");
        // the book's indent of 3em, not the core's 1.6em
        assert_eq!(indented, ((MARGIN_SIDE + 3.0 * SIZE) * UNITS) as i32);
        // a paragraph with nothing to show where it begins gets the core's indent, only while its own setting asks (whatever the
        // setting of justifying says)
        let unmarked = |mark: bool| -> i32 {
            let block = Block::Paragraph {
                spans: vec![Span {
                    text: words.clone(),
                    ..Span::default()
                }],
                style: Style {
                    align: Align::Left,
                    loose: true,
                    indent: false,
                    unmarked: true,
                    ..Style::body()
                },
            };
            let laid = lay_out_with(
                &[block],
                usize::MAX,
                (
                    100,
                    PAGE,
                    Look {
                        backdrop: false,
                        justify: false,
                        mark,
                        normalize: true,
                    },
                ),
            );
            laid.pages[0].words[0].rect.left
        };
        assert_eq!(unmarked(true), ((MARGIN_SIDE + INDENT * SIZE) * UNITS) as i32);
        assert_eq!(unmarked(false), (MARGIN_SIDE * UNITS) as i32);
    }

    #[test]
    #[allow(clippy::expect_used)] // a page that cannot be drawn should stop the test at once
    fn a_block_of_code_is_drawn_on_a_backdrop_only_when_asked() {
        let code = |t: &str, shade: Option<[u8; 3]>| Block::Paragraph {
            spans: vec![Span {
                text: t.to_string(),
                mono: true,
                ..Span::default()
            }],
            style: Style {
                align: Align::Left,
                indent: false,
                code: true,
                shade,
                ..Style::body()
            },
        };
        let blocks = vec![
            text("Some words before the code."),
            code("let x = 1;", None),
            Block::Space(SIZE * LEADING),
            code("let y = 2;", None),
            text("Some words after it."),
            code("print(x)", Some([200, 220, 255])),
        ];
        let draw = |backdrop: bool| {
            let laid = lay_out_with(
                &blocks,
                usize::MAX,
                (
                    100,
                    PAGE,
                    Look {
                        backdrop,
                        justify: true,
                        mark: true,
                        normalize: true,
                    },
                ),
            );
            let words: Vec<(String, Rect)> = laid.pages[0].words.iter().map(|w| (w.text.clone(), w.rect)).collect();
            (
                drawn(&laid, 0, Some(0)).expect("a page").render().expect("drawn"),
                words,
            )
        };
        // one backdrop for a block, not a box a line (whose edges left hairlines of the page between the lines on a tablet): the first
        // block, its two lines and the empty one between them, is one box; the last, of the book's colour, another
        let laid = lay_out_with(
            &blocks,
            usize::MAX,
            (
                100,
                PAGE,
                Look {
                    backdrop: true,
                    justify: true,
                    mark: true,
                    normalize: true,
                },
            ),
        );
        assert_eq!(laid.pages[0].page.filled(CODE_SHADE), 1);
        assert_eq!(laid.pages[0].page.filled([200, 220, 255]), 1);
        let ((off, off_words), (on, on_words)) = (draw(false), draw(true));
        let count = |image: &RgbaImage, colour: [u8; 3]| image.pixels().filter(|p| p.0[..3] == colour).count();
        // off: no backdrop at all (as before); on: the core's shade behind the first block, the book's own behind the last
        assert_eq!((count(&off, CODE_SHADE), count(&off, [200, 220, 255])), (0, 0));
        assert!(count(&on, CODE_SHADE) > 5000, "{}", count(&on, CODE_SHADE));
        assert!(count(&on, [200, 220, 255]) > 1000, "{}", count(&on, [200, 220, 255]));
        // the same words; the first stays where it is, the code and what follows it come down by the room above the block
        assert_eq!(off_words.len(), on_words.len());
        assert_eq!(off_words[0], on_words[0]);
        let at = |words: &[(String, Rect)], text: &str| {
            words.iter().find(|(t, _)| t == text).map(|(_, r)| r.top).unwrap_or(0)
        };
        assert!(at(&on_words, "let") > at(&off_words, "let"));
        assert!(at(&on_words, "after") > at(&off_words, "after"));
        // the empty line in the block is on the backdrop too: one shaded run down from the first line to the second
        let column = (MARGIN_SIDE * 2048.0 / PAGE.1) as u32 + 4;
        let (from, to) = (
            (at(&on_words, "let") as f32 / UNITS * 2048.0 / PAGE.1) as u32,
            (at(&on_words, "y") as f32 / UNITS * 2048.0 / PAGE.1) as u32,
        );
        assert!(
            (from..to).all(|y| on.get_pixel(column, y).0[..3] != [255, 255, 255]),
            "{from}..{to}"
        );
    }

    #[test]
    fn a_long_text_fills_pages_and_every_word_is_on_one() {
        let paragraph = "The quick brown fox jumps over the lazy dog again and again. ".repeat(30);
        let blocks: Vec<Block> = (0..12).map(|_| text(&paragraph)).collect();
        let laid = lay_out(&blocks, usize::MAX);
        assert!(laid.pages.len() > 3, "{} pages", laid.pages.len());
        let words: usize = laid.pages.iter().map(|p| p.words.len()).sum();
        assert_eq!(words, 12 * 30 * 12);
        // every word within the text's width, and below the top margin
        let (left, right) = (
            (MARGIN_SIDE * UNITS) as i32 - 1,
            ((PAGE.0 - MARGIN_SIDE) * UNITS) as i32 + 2,
        );
        for page in &laid.pages {
            for word in &page.words {
                assert!(word.rect.left >= left && word.rect.right <= right, "{word:?}");
                assert!(word.rect.bottom <= (PAGE.1 * UNITS) as i32);
            }
        }
    }

    #[test]
    fn a_page_of_another_shape_keeps_the_width_of_the_lines_and_takes_its_height_from_the_shape() {
        // the book's own shape, a column of a TV in landscape (8:9), a whole TV screen (16:9), a very tall phone
        assert_eq!(page_for(None), PAGE);
        assert_eq!(page_for(Some((0, 5))), PAGE, "a shape with no width is none");
        assert_eq!(page_for(Some((960, 1080))), (PAGE.0, PAGE.0 * 1.125));
        assert_eq!(page_for(Some((1920, 1080))), (PAGE.0, PAGE.0 * 0.5625));
        assert_eq!(
            page_for(Some((100, 1000))),
            (PAGE.0, PAGE.0 * PAGE_SHAPES.1),
            "kept within the shapes"
        );
        assert_eq!(page_for(Some((1000, 100))), (PAGE.0, PAGE.0 * PAGE_SHAPES.0));
        let paragraph = "The quick brown fox jumps over the lazy dog again and again. ".repeat(30);
        let blocks: Vec<Block> = (0..12).map(|_| text(&paragraph)).collect();
        let own = lay_out(&blocks, usize::MAX);
        let wide = lay_out_on(&blocks, usize::MAX, 100, page_for(Some((1920, 1080))));
        assert_eq!(wide.size, (PAGE.0, PAGE.0 * 0.5625));
        // the same words on more, shorter pages, each within the page
        let words = |laid: &Laid| laid.pages.iter().map(|p| p.words.len()).sum::<usize>();
        assert_eq!(words(&wide), words(&own));
        assert!(
            wide.pages.len() > own.pages.len() * 2,
            "{} against {}",
            wide.pages.len(),
            own.pages.len()
        );
        let (right, bottom) = (
            ((PAGE.0 - MARGIN_SIDE) * UNITS) as i32 + 2,
            (wide.size.1 * UNITS) as i32,
        );
        assert!(
            wide.pages
                .iter()
                .flat_map(|p| &p.words)
                .all(|w| w.rect.right <= right && w.rect.bottom <= bottom)
        );
        // a place found again on the other shape, as for another size of the text
        for page in &own.pages {
            let there = page_at(&wide, page.place);
            assert!(wide.pages[there].place <= page.place);
            assert!(wide.pages.get(there + 1).is_none_or(|next| next.place > page.place));
        }
        // a larger text on that shape: the page smaller as PAGE is
        let large = lay_out_on(&blocks, 1, 200, page_for(Some((1920, 1080))));
        assert_eq!(large.size, (PAGE.0 / 2.0, PAGE.0 * 0.5625 / 2.0));
    }

    #[test]
    fn the_shape_of_the_page_is_kept_and_none_is_the_books_own() {
        // not set here: the books laid out by other tests at the same time read it
        assert_eq!(unpacked(packed(Some((1920, 1080)))), Some((1920, 1080)));
        assert_eq!(unpacked(packed(Some((u32::MAX, 1)))), Some((u32::MAX, 1)));
        assert_eq!(unpacked(packed(Some((0, 1080)))), None);
        assert_eq!(unpacked(packed(None)), None);
        assert_eq!(page_shape(), None, "the book's own unless a program sets it");
    }

    #[test]
    fn a_larger_text_is_laid_out_on_a_smaller_page_into_more_pages_and_the_place_of_a_page_is_found_again() {
        let paragraph = "The quick brown fox jumps over the lazy dog again and again. ".repeat(30);
        let blocks: Vec<Block> = (0..12).map(|_| text(&paragraph)).collect();
        let own = lay_out(&blocks, usize::MAX);
        let large = lay_out_at(&blocks, usize::MAX, 150);
        assert_eq!(own.size, PAGE);
        assert!((large.size.0 - PAGE.0 / 1.5).abs() < 0.01 && (large.size.1 - PAGE.1 / 1.5).abs() < 0.01);
        assert_eq!(
            large.units(),
            ((PAGE.0 / 1.5 * UNITS) as u32, (PAGE.1 / 1.5 * UNITS) as u32)
        );
        assert!(
            large.pages.len() > own.pages.len() * 2,
            "{} pages against {}",
            large.pages.len(),
            own.pages.len()
        );
        // the same words, each within the smaller page's margins
        let words = |laid: &Laid| laid.pages.iter().map(|p| p.words.len()).sum::<usize>();
        assert_eq!(words(&large), words(&own));
        let right = ((large.size.0 - MARGIN_SIDE / 1.5) * UNITS) as i32 + 2;
        assert!(large.pages.iter().flat_map(|p| &p.words).all(|w| w.rect.right <= right));
        // a place is the number of words before the page: each page found again at its own size, and at the other size the page that
        // has its first word
        for (index, page) in own.pages.iter().enumerate() {
            assert_eq!(page_at(&own, place_of(&own, index)), index);
            let there = page_at(&large, page.place);
            let first = &large.pages[there];
            assert!(first.place <= page.place, "page {index}");
            assert!(
                large.pages.get(there + 1).is_none_or(|next| next.place > page.place),
                "page {index}"
            );
        }
        assert_eq!(own.pages[0].place, 0);
        assert_eq!(
            page_at(&large, usize::MAX),
            large.pages.len() - 1,
            "past the end: the last page"
        );
        // sizes out of the range are taken as its ends
        assert_eq!(
            lay_out_at(&blocks, 1, 1000).size,
            lay_out_at(&blocks, 1, TEXT_SIZES.1).size
        );
    }

    #[test]
    fn a_page_without_words_comes_before_the_text_that_follows_it_when_a_place_is_found() {
        let image = Arc::new(RgbaImage::new(40, 60));
        let blocks = vec![Block::Cover(image.into()), text("Words after the cover.")];
        let laid = lay_out(&blocks, usize::MAX);
        assert_eq!(laid.pages.len(), 2);
        assert_eq!((laid.pages[0].place, laid.pages[1].place), (0, 0));
        assert_eq!(page_at(&laid, 0), 0, "the cover, not the text after it");
        assert_eq!(page_at(&laid, 2), 1);
    }

    #[test]
    fn the_insets_of_an_epigraph_leave_room_for_a_large_text() {
        let epigraph = Block::Paragraph {
            spans: vec![Span {
                text: "Words of an epigraph that is set in to the right. ".repeat(4),
                ..Span::default()
            }],
            style: Style::verse(PAGE.0 * 0.3),
        };
        let large = lay_out_at(&[epigraph], usize::MAX, TEXT_SIZES.1);
        let k = TEXT_SIZES.1 as f32 / 100.0;
        let full = (PAGE.0 - 2.0 * MARGIN_SIDE) / k;
        let lefts = large.pages[0].words.iter().map(|w| w.rect.left).min().unwrap_or(0);
        let start = ((MARGIN_SIDE / k + full * MOST_INSET) * UNITS) as i32;
        assert!(lefts <= start + 1, "{lefts} against {start}: the inset made smaller");
    }

    #[test]
    fn a_justified_line_ends_at_the_right_margin_and_the_last_line_does_not() {
        let laid = lay_out(&[text(&"word ".repeat(200))], usize::MAX);
        let words = &laid.pages[0].words;
        let right = ((PAGE.0 - MARGIN_SIDE) * UNITS) as i32;
        // the last word of the first line: the one before the first that starts a new line
        let first_line_top = words[0].rect.top;
        let end = words
            .iter()
            .take_while(|w| w.rect.top == first_line_top)
            .last()
            .map(|w| w.rect.right);
        assert!(end.is_some_and(|e| (e - right).abs() <= 2), "{end:?} against {right}");
        let last = words.last().map(|w| w.rect.right).unwrap_or(0);
        assert!(last < right - 40, "the last line is not stretched: {last}");
    }

    #[test]
    fn a_mark_leads_to_the_page_of_what_follows_it_and_a_chapter_starts_a_page() {
        let filler = "Some words to fill the page with text. ".repeat(40);
        let blocks = vec![
            Block::Mark {
                depth: 0,
                title: "One".to_string(),
            },
            text(&filler),
            Block::NewPage,
            Block::Mark {
                depth: 0,
                title: "Two".to_string(),
            },
            text("The second chapter."),
        ];
        let laid = lay_out(&blocks, usize::MAX);
        let pages: Vec<Option<usize>> = laid.contents.iter().map(|c| c.page).collect();
        assert_eq!(pages[0], Some(0));
        let two = pages[1].unwrap_or(0);
        assert!(two >= 1);
        assert!(laid.pages[two].text.starts_with("The second chapter."));
        assert_eq!(laid.pages.len(), two + 1);
    }

    #[test]
    fn a_heading_at_the_foot_of_a_page_goes_with_its_text_to_the_next() {
        // fill a page to its last line or two, then a heading
        let mut blocks: Vec<Block> = Vec::new();
        for _ in 0..200 {
            blocks.push(Block::Paragraph {
                spans: vec![Span {
                    text: "Line".to_string(),
                    ..Span::default()
                }],
                style: Style::verse(0.0),
            });
        }
        let laid = lay_out(&blocks, usize::MAX);
        let per_page = laid.pages[0].words.len();
        let mut blocks: Vec<Block> = blocks.into_iter().take(per_page - 1).collect();
        blocks.push(Block::Paragraph {
            spans: vec![Span {
                text: "Heading".to_string(),
                ..Span::default()
            }],
            style: Style::heading(1),
        });
        blocks.push(text("After the heading."));
        let laid = lay_out(&blocks, usize::MAX);
        assert_eq!(laid.pages.len(), 2);
        assert!(laid.pages[1].text.starts_with("Heading"), "{:?}", laid.pages[1].text);
    }

    #[test]
    fn the_words_found_are_marked_and_a_word_too_long_for_a_line_is_cut() {
        let long = "x".repeat(400);
        let laid = lay_out(&[text(&format!("find me {long}"))], usize::MAX);
        assert!(laid.pages[0].words.len() > 3);
        let mut image = RgbaImage::from_pixel(864, 1296, image::Rgba([255, 255, 255, 255]));
        assert_eq!(mark_words(&laid, 0, "me", &mut image), 1);
        assert_eq!(mark_words(&laid, 0, "absent", &mut image), 0);
    }

    #[test]
    fn a_russian_word_is_broken_at_a_line_end_and_found_whole() {
        let laid = lay_out(
            &[text(
                &"Капитанская дочка Пугачёва переправлялась через широкую реку. ".repeat(20),
            )],
            usize::MAX,
        );
        let words = &laid.pages[0].words;
        // some line ends with a broken word: its two parts are two words on the page, each knowing the whole word
        let broken = words.windows(2).filter(|w| w[0].text == w[1].text).count();
        assert!(broken > 0, "no word broken");
        // the text of the page has each word once
        let text = &laid.pages[0].text;
        assert_eq!(text.matches("Капитанская").count(), 20, "{text}");
        assert!(!text.contains('-'));
        let mut image = RgbaImage::from_pixel(864, 1296, image::Rgba([255, 255, 255, 255]));
        let needle = peekel_djvu::text::normalise("переправлялась");
        assert!(mark_words(&laid, 0, &needle, &mut image) >= 20);
    }

    #[test]
    fn only_the_first_page_is_laid_out_when_one_is_asked_for() {
        let blocks: Vec<Block> = (0..50).map(|_| text(&"many words here ".repeat(60))).collect();
        assert_eq!(lay_out(&blocks, 1).pages.len(), 1);
    }

    #[test]
    fn a_cover_is_a_page_of_its_own_with_no_number() {
        let cover = Arc::new(RgbaImage::from_pixel(60, 90, image::Rgba([200, 0, 0, 255])));
        let laid = lay_out(&[Block::Cover(cover.into()), text("Chapter one.")], usize::MAX);
        assert_eq!(laid.pages.len(), 2);
        assert!(laid.pages[0].text.is_empty());
        assert!(laid.pages[1].text.starts_with("Chapter one."));
    }
}
