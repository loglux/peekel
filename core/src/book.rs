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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::RgbaImage;
use peekel_djvu::text::{Rect, Word, locate};

use crate::decode::{ContentsItem, DecodeError};
use crate::guard::unreadable;
use crate::measure::width;
use crate::page::{Font, Page};

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

/// The pictures of a book, decoded once each, within the limits of a book.
#[derive(Debug, Default)]
pub struct Pictures {
    decoded: HashMap<String, Option<Arc<RgbaImage>>>,
    images: usize,
    pixels: u64,
}

impl Pictures {
    /// The picture known as `key` (its id or its path in the book), its bytes given by `bytes` the first time; `None` when it cannot
    /// be read or the limits are met.
    pub fn get(&mut self, key: &str, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<RgbaImage>> {
        if let Some(known) = self.decoded.get(key) {
            return known.clone();
        }
        let image = self.decode(bytes);
        self.decoded.insert(key.to_string(), image.clone());
        image
    }

    fn decode(&mut self, bytes: impl FnOnce() -> Option<Vec<u8>>) -> Option<Arc<RgbaImage>> {
        if self.images >= MAX_IMAGES || self.pixels >= MAX_ALL_PIXELS {
            return None;
        }
        let image = crate::decode::decode_bytes(&bytes()?, MAX_ONE_IMAGE).ok()?;
        let (w, h) = image.dimensions();
        let image = if w.max(h) > SHRINK {
            let scale = f64::from(SHRINK) / f64::from(w.max(h));
            let (nw, nh) = (
                ((f64::from(w) * scale).round() as u32).max(1),
                ((f64::from(h) * scale).round() as u32).max(1),
            );
            crate::resize::triangle(&image, nw, nh)
        } else {
            image
        };
        self.images += 1;
        self.pixels += u64::from(image.width()) * u64::from(image.height());
        Some(Arc::new(image))
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
    /// The first line indented.
    pub indent: bool,
    /// More margin on the left and on the right, in points.
    pub left: f32,
    pub right: f32,
    /// Space above and below, in points.
    pub before: f32,
    pub after: f32,
    /// Kept on one page with the line after it (a heading).
    pub keep: bool,
}

impl Style {
    /// A paragraph of the text.
    pub fn body() -> Self {
        Self {
            scale: 1.0,
            bold: false,
            italic: false,
            align: Align::Justify,
            indent: true,
            left: 0.0,
            right: 0.0,
            before: 0.0,
            after: 0.0,
            keep: false,
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

/// A part of a book, in reading order.
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph {
        spans: Vec<Span>,
        style: Style,
    },
    /// A picture in the text: as wide as it is (a pixel a point) up to the width of the text; `None` when it could not be read.
    Picture(Option<Arc<RgbaImage>>),
    /// The cover: a page of its own, the picture as large as the page.
    Cover(Arc<RgbaImage>),
    /// Space, in points (an empty line).
    Space(f32),
    /// What follows starts a new page (a chapter).
    NewPage,
    /// A line of the contents: the next thing laid out is where it leads.
    Mark {
        depth: usize,
        title: String,
    },
}

/// One page laid out: what is drawn, its words and where they stand (in `UNITS` of a point), and its text.
#[derive(Debug)]
pub struct LaidPage {
    pub page: Page,
    pub words: Vec<Word>,
    pub text: String,
    /// The place of the page in the book: how many words come before it. It does not depend on the size of the text, so the reader's
    /// place is found again when the book is laid out at another size (`page_at`).
    pub place: usize,
}

/// A book laid out into pages.
#[derive(Debug)]
pub struct Laid {
    pub pages: Vec<LaidPage>,
    pub contents: Vec<ContentsItem>,
    /// The size of its pages, in points (`PAGE` made smaller for a larger text).
    pub size: (f32, f32),
}

impl Laid {
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

/// A unit of a line that is not broken: the pieces of a word in their fonts (`emphasis` and the comma after it), its width, and
/// whether a space follows it. A word broken at the end of a line is two units: the head (with the hyphen drawn) and the tail on
/// the next line; both know the whole word, for the search and the text of the page.
#[derive(Default)]
struct Unit {
    pieces: Vec<(String, Font, bool)>,
    width: f32,
    space: bool,
    /// The whole word, when this is a part of one broken at a line's end.
    whole: Option<String>,
    /// The part of a broken word that ends a line.
    head: bool,
    /// The part of a broken word that starts a line.
    tail: bool,
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
        let mut head: Vec<(String, Font, bool)> = Vec::new();
        let mut tail: Vec<(String, Font, bool)> = Vec::new();
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
        let measure = |pieces: &[(String, Font, bool)]| pieces.iter().map(|(t, f, _)| width(t, f)).sum();
        let whole = Some(self.word());
        (
            Unit {
                width: measure(&head),
                pieces: head,
                space: false,
                whole: whole.clone(),
                head: true,
                tail: self.tail,
            },
            Unit {
                width: measure(&tail),
                pieces: tail,
                space: self.space,
                whole,
                head: self.head,
                tail: true,
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
        size: SIZE * style.scale * if span.raised { 0.72 } else { 1.0 },
        bold: style.bold || span.bold,
        // emphasis in an italic paragraph is upright
        italic: style.italic != span.italic,
        underline: false,
        colour: [0, 0, 0],
        family: Some(if span.mono { MONO } else { SERIF }.to_string()),
    }
}

/// The units of a paragraph: its text cut at white space, the pieces of different spans that touch kept together.
fn units_of(spans: &[Span], style: &Style, room: f32) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut current: Vec<(String, Font, bool)> = Vec::new();
    let finish = |current: &mut Vec<(String, Font, bool)>, units: &mut Vec<Unit>, space: bool| {
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
        let mut piece = String::new();
        for c in span.text.chars() {
            // a figure space (U+2007) is a space of a digit's width that does not break: the indent of a line of code
            if c.is_whitespace() && c != '\u{2007}' {
                if !piece.is_empty() {
                    current.push((std::mem::take(&mut piece), font.clone(), span.raised));
                }
                finish(&mut current, &mut units, true);
            } else if !c.is_control() {
                piece.push(c);
            }
        }
        if !piece.is_empty() {
            current.push((piece, font, span.raised));
        }
    }
    finish(&mut current, &mut units, false);
    // a unit wider than the line (a long address) is cut where the line ends
    let mut out = Vec::with_capacity(units.len());
    for unit in units {
        if unit.width <= room {
            out.push(unit);
            continue;
        }
        let space = unit.space;
        let mut part: Vec<(String, Font, bool)> = Vec::new();
        let mut part_width = 0.0;
        for (text, font, raised) in unit.pieces {
            for c in text.chars() {
                let w = width(&c.to_string(), &font);
                if part_width + w > room && part_width > 0.0 {
                    out.push(Unit {
                        pieces: std::mem::take(&mut part),
                        width: part_width,
                        ..Unit::default()
                    });
                    part_width = 0.0;
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
                ..Unit::default()
            });
        }
    }
    out
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
}

impl Builder {
    fn blank(&self) -> LaidPage {
        LaidPage {
            page: Page::new(self.width, self.height),
            words: Vec::new(),
            text: String::new(),
            place: self.words,
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
        if !self.cover {
            // the number of the page, small at the foot
            let number = (self.pages.len() + 1).to_string();
            let font = Font {
                size: SIZE * 0.8,
                colour: GREY,
                family: Some(SERIF.to_string()),
                ..Font::default()
            };
            let w = width(&number, &font);
            done.page
                .text((self.width - w) / 2.0, self.height - self.foot / 2.0, &number, &font);
        }
        self.cover = false;
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

    fn paragraph(&mut self, spans: &[Span], style: &Style) {
        // the insets in points, made smaller together when a large text would leave them most of the line
        let full = self.width - 2.0 * self.side;
        let insets = style.left + style.right;
        let shrink = if insets > full * MOST_INSET {
            full * MOST_INSET / insets
        } else {
            1.0
        };
        let left = self.side + style.left * shrink;
        let room = (full - insets * shrink).max(SIZE * 4.0);
        let size = SIZE * style.scale;
        let indent = if style.indent { INDENT * size } else { 0.0 };
        let units = units_of(spans, style, room - indent);
        if units.is_empty() {
            return;
        }
        // a space of the text's font; a line of code (all monospace) has the monospace one, so its columns stay
        let text = Span {
            mono: spans.iter().all(|s| s.mono),
            ..Span::default()
        };
        let space = width(" ", &font_of(&text, style));
        // the lines: a word that does not fit is broken when the line without it would be stretched much (justified text only)
        let mut lines: Vec<Vec<Unit>> = Vec::new();
        let mut current: Vec<Unit> = Vec::new();
        let mut used = 0.0;
        let mut queue: std::collections::VecDeque<Unit> = units.into();
        while let Some(unit) = queue.pop_front() {
            let line_room = if lines.is_empty() { room - indent } else { room };
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
            if style.align == Align::Justify {
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

        let line_height = size * LEADING;
        if self.y > self.top {
            self.y += style.before;
        }
        // a heading is not left alone at the foot of a page: it goes with the next line
        let needed = lines.len() as f32 * line_height + if style.keep { SIZE * LEADING * 2.0 } else { 0.0 };
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
            let baseline = self.y + size * 1.02;
            let mut words_of_line: Vec<String> = Vec::new();
            for (i, unit) in line.iter().enumerate() {
                let mut px = x;
                for (text, font, raised) in &unit.pieces {
                    let lift = if *raised { size * 0.35 } else { 0.0 };
                    self.page.page.text(px, baseline - lift, text, font);
                    px += width(text, font);
                }
                // the page's text has a broken word once, whole: where its head is, or where its tail starts a page
                let first_of_page = self.page.words.is_empty();
                let word = unit.word();
                if !unit.tail || first_of_page {
                    words_of_line.push(word.clone());
                }
                if !unit.tail {
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

    fn picture(&mut self, image: Option<&Arc<RgbaImage>>) {
        let room = self.width - 2.0 * self.side;
        let tall = self.bottom() - self.top;
        let (w, h) = image.map_or((room / 2.0, room / 4.0), |i| (i.width() as f32, i.height() as f32));
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
            Some(image) => self.page.page.picture(x, self.y, w, h, Arc::clone(image)),
            None => self
                .page
                .page
                .rect(x, self.y, w, h, Some([235, 235, 235]), Some(([170, 170, 170], 0.75))),
        }
        self.y += h + gap;
    }

    fn cover(&mut self, image: &Arc<RgbaImage>) {
        self.new_page();
        if self.full() {
            return;
        }
        self.place_marks();
        let (w, h) = (image.width() as f32, image.height() as f32);
        if w > 0.0 && h > 0.0 {
            let scale = (self.width / w).min(self.height / h);
            let (w, h) = (w * scale, h * scale);
            self.page
                .page
                .picture((self.width - w) / 2.0, (self.height - h) / 2.0, w, h, Arc::clone(image));
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
    let k = percent.clamp(TEXT_SIZES.0, TEXT_SIZES.1) as f32 / 100.0;
    let mut builder = Builder {
        pages: Vec::new(),
        page: LaidPage {
            page: Page::new(PAGE.0 / k, PAGE.1 / k),
            words: Vec::new(),
            text: String::new(),
            place: 0,
        },
        y: MARGIN_TOP / k,
        contents: Vec::new(),
        pending: Vec::new(),
        cover: false,
        limit: limit.min(MAX_PAGES),
        width: PAGE.0 / k,
        height: PAGE.1 / k,
        side: MARGIN_SIDE / k,
        top: MARGIN_TOP / k,
        foot: MARGIN_BOTTOM / k,
        words: 0,
    };
    for block in blocks {
        if builder.full() {
            break;
        }
        match block {
            Block::Paragraph { spans, style } => builder.paragraph(spans, style),
            Block::Picture(image) => builder.picture(image.as_ref()),
            Block::Cover(image) => builder.cover(image),
            Block::Space(points) => {
                if builder.y > builder.top {
                    builder.y += points;
                }
            }
            Block::NewPage => builder.new_page(),
            Block::Mark { depth, title } => builder.pending.push((*depth, title.clone())),
        }
    }
    if !builder.full() {
        builder.new_page();
    }
    if builder.pages.is_empty() {
        let blank = builder.blank();
        builder.pages.push(blank);
    }
    Laid {
        pages: builder.pages,
        contents: builder.contents,
        size: (builder.width, builder.height),
    }
}

/// The book last laid out, by its file (its path, size and time of change) and the size of its text: the viewer asks for its pages one
/// by one, its text and its contents, and the laying out of a long book takes a while.
type Key = (PathBuf, u64, Option<SystemTime>, u32);
static LAID: Mutex<Option<(Key, Arc<Laid>)>> = Mutex::new(None);

fn key(path: &Path) -> Result<Key, DecodeError> {
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    Ok((path.to_path_buf(), meta.len(), meta.modified().ok(), text_size()))
}

/// The book laid out last, if it is this one.
fn cached(key: &Key) -> Option<Arc<Laid>> {
    let last = LAID.lock().ok()?;
    last.as_ref()
        .filter(|(k, _)| k == key)
        .map(|(_, laid)| Arc::clone(laid))
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
    let percent = key.3;
    let laid = crate::guard::draw_on_thread("book", "the book could not be laid out", || {
        Ok(Arc::new(lay_out_at(&read(path)?, usize::MAX, percent)))
    })?;
    if let Ok(mut last) = LAID.lock() {
        *last = Some((key, Arc::clone(&laid)));
    }
    Ok(laid)
}

/// The first page alone (a quick view): only as much of the book is laid out as that page needs.
pub fn first_page<F>(path: &Path, read: F) -> Result<RgbaImage, DecodeError>
where
    F: FnOnce(&Path) -> Result<Vec<Block>, DecodeError> + Send,
{
    let key = key(path)?;
    if let Some(laid) = cached(&key) {
        return laid
            .pages
            .first()
            .map_or(Err(DecodeError::NotAPicture), |p| p.page.render());
    }
    let percent = key.3;
    crate::guard::draw_on_thread("book", "the book could not be laid out", || {
        let laid = lay_out_at(&read(path)?, 1, percent);
        laid.pages
            .first()
            .map_or(Err(DecodeError::NotAPicture), |p| p.page.render())
    })
}

/// Page `page` (from 0; past the end, the last) and the number of pages.
pub fn page(laid: &Laid, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    let count = laid.pages.len();
    let index = page.min(count.saturating_sub(1));
    let image = laid
        .pages
        .get(index)
        .ok_or_else(|| unreadable("the book has no pages"))?
        .page
        .render()?;
    Ok((image, count))
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
        let blocks = vec![Block::Cover(image), text("Words after the cover.")];
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
        let laid = lay_out(&[Block::Cover(cover), text("Chapter one.")], usize::MAX);
        assert_eq!(laid.pages.len(), 2);
        assert!(laid.pages[0].text.is_empty());
        assert!(laid.pages[1].text.starts_with("Chapter one."));
    }
}
