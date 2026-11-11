//! The text of a PDF, for a search in the viewer: the letters `hayro` draws, their Unicode where the font tells it (a `ToUnicode` map,
//! the encoding, the names of the glyphs), and where they stand, gathered into words.
//!
//! A PDF has no words, only letters put at places: a space may be a letter or only a gap. So a word ends at a letter that is a space, at a
//! gap wider than a fifth of the size of the letters, and where the line changes; the lines are told apart by the baseline. The place
//! of a letter is its origin and its advance (the font's width, else half the size) across, and the ascent and descent of a usual font
//! (0.8 and 0.2 of the size) up and down, in the pixels of the page drawn at a scale of one (the origin at the top left, as `hayro`
//! draws it), so that a word is marked on the picture as the words of a DjVu book are (`djvu::mark_words`).
//!
//! A file made to hurt: the file is read with a size limit, the work runs on a thread of its own on which a panic is an error and which
//! is given up after a time, and the letters of a page and of the whole book are counted. Not read: the text of a scan (it has none; that
//! would need recognition), letters whose font tells no Unicode, text drawn as paths.

use std::path::Path;
use std::time::Duration;

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache,
    InterpreterSettings, SoftMask, TransformExt, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{BezPath, Point};
use image::RgbaImage;
use peekel_djvu::text::{Rect, Word, locate};

use crate::decode::DecodeError;
use crate::guard::unreadable;
use crate::pdf::{MAX_BYTES, on_thread};

/// The most letters read of one page (a page of small print has some thousands).
const MAX_LETTERS: usize = 100_000;
/// The most letters read of the whole file (a long book has some millions).
const MAX_BOOK: usize = 20_000_000;
/// How long the text of a whole file may take to read, and of one page.
const BOOK_TIME: Duration = Duration::from_secs(60);
const PAGE_TIME: Duration = Duration::from_secs(10);
/// The units of a glyph to an em (PDF fonts, `hayro` too).
const EM: f64 = 1000.0;

/// A letter and its box, in the pixels of the page at a scale of one.
#[derive(Debug, Clone)]
struct Letter {
    text: String,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    /// The baseline and the size, to tell the lines apart.
    base: f64,
    size: f64,
}

/// What `hayro` is told to draw, kept as letters; everything else is let go.
#[derive(Debug, Default)]
struct Letters {
    letters: Vec<Letter>,
}

impl Device<'_> for Letters {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'_>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'_>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'_, '_>, _: ImageDrawProps<'_>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, '_>, props: DrawProps<'_>, _: &DrawMode) {
        for glyph in run.glyphs() {
            if self.letters.len() >= MAX_LETTERS {
                return;
            }
            let Some(text) = glyph.as_unicode().map(|u| match u {
                BfString::Char(c) => c.to_string(),
                BfString::String(s) => s,
            }) else {
                continue;
            };
            let t = props.transform * glyph.transform();
            let origin = t * Point::ZERO;
            let size = (t * Point::new(0.0, EM) - origin).hypot();
            if !(origin.x.is_finite() && origin.y.is_finite() && size.is_finite() && size > 0.0) {
                continue;
            }
            let advance = match &**glyph {
                Glyph::Outline(outline) => outline.advance_width().map(f64::from),
                Glyph::Type3(_) => None,
            }
            .filter(|a| a.is_finite() && *a > 0.0)
            .unwrap_or(EM / 2.0);
            let corners = [
                t * Point::new(0.0, 0.8 * EM),
                t * Point::new(advance, 0.8 * EM),
                t * Point::new(0.0, -0.2 * EM),
                t * Point::new(advance, -0.2 * EM),
            ];
            let (xs, ys) = (corners.map(|p| p.x), corners.map(|p| p.y));
            self.letters.push(Letter {
                text,
                left: xs.iter().copied().fold(f64::INFINITY, f64::min),
                right: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                top: ys.iter().copied().fold(f64::INFINITY, f64::min),
                bottom: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                base: origin.y,
                size,
            });
        }
    }
}

/// The text of a page: the words in the order they are drawn, joined by a space (a new line where the line changes), and each word with
/// its rectangle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageText {
    pub text: String,
    pub words: Vec<Word>,
}

/// The letters of a page gathered into words.
fn words_of(letters: &[Letter]) -> PageText {
    let mut out = PageText::default();
    // the word being made, and whether a line ended before it began
    let mut word: Option<(String, Letter, bool)> = None;
    let mut last: Option<&Letter> = None;
    let mut line_ended = false;
    let finish = |word: &mut Option<(String, Letter, bool)>, out: &mut PageText| {
        if let Some((text, b, after_line)) = word.take() {
            if !out.text.is_empty() {
                out.text.push(if after_line { '\n' } else { ' ' });
            }
            out.text.push_str(&text);
            let px = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
            out.words.push(Word {
                text,
                rect: Rect {
                    left: px(b.left),
                    top: px(b.top),
                    right: px(b.right),
                    bottom: px(b.bottom),
                },
            });
        }
    };
    for letter in letters {
        let new_line = last.is_some_and(|l| (letter.base - l.base).abs() > 0.5 * l.size.max(letter.size));
        let gap = last
            .is_some_and(|l| letter.left - l.right > 0.2 * l.size.max(letter.size) || letter.right < l.left - l.size);
        line_ended |= new_line;
        if letter.text.chars().all(char::is_whitespace) {
            finish(&mut word, &mut out);
            last = Some(letter);
            continue;
        }
        if new_line || gap {
            finish(&mut word, &mut out);
        }
        match &mut word {
            Some((text, b, _)) => {
                text.push_str(&letter.text);
                b.left = b.left.min(letter.left);
                b.right = b.right.max(letter.right);
                b.top = b.top.min(letter.top);
                b.bottom = b.bottom.max(letter.bottom);
            }
            None => {
                word = Some((letter.text.clone(), letter.clone(), line_ended));
                line_ended = false;
            }
        }
        last = Some(letter);
    }
    finish(&mut word, &mut out);
    out
}

/// The letters of a page, in the pixels of the page at a scale of one.
fn letters_of(page: &Page<'_>, pdf: &Pdf) -> Vec<Letter> {
    let (width, height) = page.render_dimensions();
    let cache = InterpreterCache::new();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        hayro::kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        &cache,
        pdf.xref(),
        InterpreterSettings::default(),
    );
    let mut letters = Letters::default();
    interpret_page(page, &mut context, &mut letters);
    letters.letters
}

fn read(path: &Path) -> Result<Vec<u8>, DecodeError> {
    if std::fs::metadata(path).map_err(unreadable)?.len() > MAX_BYTES {
        return Err(DecodeError::TooLarge);
    }
    std::fs::read(path).map_err(unreadable)
}

fn open(data: Vec<u8>) -> Result<Pdf, DecodeError> {
    Pdf::new(data).map_err(|e| unreadable(format!("the PDF cannot be read ({e:?})")))
}

/// The text of every page (an empty string for a page with none), or `None` when no page has any (a scan). For a search in the book.
pub fn page_texts(path: &Path) -> Result<Option<Vec<String>>, DecodeError> {
    let data = read(path)?;
    on_thread(BOOK_TIME, move || {
        let pdf = open(data)?;
        let mut read = 0usize;
        let texts: Vec<String> = pdf
            .pages()
            .iter()
            .map(|page| {
                if read >= MAX_BOOK {
                    return String::new();
                }
                let letters = letters_of(page, &pdf);
                read += letters.len();
                words_of(&letters).text
            })
            .collect();
        Ok(texts.iter().any(|t| !t.is_empty()).then_some(texts))
    })?
}

/// The text and words of page `page` (counted from 0), and the size of the page in pixels at a scale of one.
pub fn page_words(path: &Path, page: usize) -> Result<(PageText, (u32, u32)), DecodeError> {
    let data = read(path)?;
    on_thread(PAGE_TIME, move || {
        let pdf = open(data)?;
        let pages = pdf.pages();
        let page = pages.get(page).ok_or_else(|| unreadable("no such page"))?;
        let (width, height) = page.render_dimensions();
        let size = (width.max(0.0) as u32, height.max(0.0) as u32);
        Ok((words_of(&letters_of(page, &pdf)), size))
    })?
}

/// Marks on `image` (page `page` as `pdf::render_page` drew it) the words that `needle` (made by `djvu::normalise`) is in; how many.
pub fn mark_words(path: &Path, page: usize, needle: &str, image: &mut RgbaImage) -> Result<usize, DecodeError> {
    let (text, size) = page_words(path, page)?;
    Ok(crate::djvu::paint_found(&(locate(&text.words, needle), size), image))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot read its own fixture should stop at once
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("pdf")
            .join(name)
    }

    #[test]
    fn the_words_of_a_page_and_where_they_stand() {
        let (text, size) = page_words(&fixture("text.pdf"), 0).expect("reads");
        assert_eq!(text.text, "Hello PDF");
        assert_eq!(size, (200, 100));
        let words: Vec<&str> = text.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, ["Hello", "PDF"]);
        // 18 points at x 80, the baseline 40 points from the bottom of a page 100 high: 60 from the top
        let hello = text.words[0].rect;
        assert_eq!(hello.left, 80);
        assert!(
            (44..=47).contains(&hello.top) && (63..=65).contains(&hello.bottom),
            "{hello:?}"
        );
        assert!(text.words[1].rect.left > hello.right, "{:?}", text.words);
    }

    #[test]
    fn every_page_of_a_book_and_a_scan_has_none() {
        let texts = page_texts(&fixture("three-pages.pdf"))
            .expect("reads")
            .expect("has text");
        assert_eq!(texts, ["Page 1", "Page 2", "Page 3"]);
        assert_eq!(page_texts(&fixture("scan.pdf")).expect("reads"), None);
    }

    #[test]
    fn a_word_found_is_marked_yellow() {
        let path = fixture("text.pdf");
        let (mut image, _) = crate::pdf::render_page(&path, 0).expect("draws");
        let before = image.clone();
        let marked = mark_words(&path, 0, "pdf", &mut image).expect("marks");
        assert_eq!(marked, 1);
        assert_ne!(image, before);
        assert_eq!(mark_words(&path, 0, "absent", &mut image.clone()).expect("marks"), 0);
    }

    #[test]
    fn letters_without_a_gap_are_one_word_and_a_new_line_is_a_new_word() {
        let letter = |text: &str, left: f64, base: f64| Letter {
            text: text.to_string(),
            left,
            right: left + 5.0,
            top: base - 8.0,
            bottom: base + 2.0,
            base,
            size: 10.0,
        };
        let letters = [
            letter("a", 0.0, 10.0),
            letter("b", 5.0, 10.0),
            letter("c", 13.0, 10.0),
            letter("d", 0.0, 30.0),
        ];
        let text = words_of(&letters);
        assert_eq!(text.text, "ab c\nd");
        assert_eq!(
            text.words[0].rect,
            Rect {
                left: 0,
                top: 2,
                right: 10,
                bottom: 12
            }
        );
    }

    #[test]
    fn a_damaged_file_is_an_error_and_not_a_panic() {
        let dir = std::env::temp_dir().join("peekel-pdf-text-test");
        let _ = std::fs::create_dir_all(&dir);
        let bytes = std::fs::read(fixture("text.pdf")).expect("reads");
        for cut in [0, 10, 100, 300, bytes.len() - 20] {
            let path = dir.join(format!("cut-{cut}.pdf"));
            std::fs::write(&path, &bytes[..cut]).expect("writes");
            let _ = page_texts(&path);
            let _ = page_words(&path, 0);
            let _ = std::fs::remove_file(&path);
        }
    }
}
