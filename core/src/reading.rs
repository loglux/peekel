//! The text of a page for a reader's own tools (Peekel TV, #235): the word at a point, for a translation on a long press, and the plain
//! text of one page, for reading aloud. From what the search already reads: the words a book whose text flows was laid out in (FB2,
//! EPUB, HTML), the words of a PDF's text, the zones of a DjVu page's text layer. A scan with no text has none.

use std::path::Path;

use peekel_djvu::text::Word;

use crate::content::PageBox;
use crate::decode::DecodeError;
use crate::djvu::PageWords;

/// A point this far from a word (in parts of the word's height) still takes it: a finger is not a pointer.
const NEAR: f64 = 0.5;

/// The word at (`x`, `y`) on page `page` (counted from 0; past the end, the last) as drawn at `size` pixels (the picture `decode_page`
/// or `decode_page_within` gave: any scale of the page), and its box in those pixels. The word as the page has it, without the marks
/// round it (quotes, a comma after it); a word broken at a line's end is given whole, with the box of the part pointed at. The word the
/// point is in, else the nearest within half a line; `None` when there is none, for a page with no text and for any other file.
///
/// A book whose text flows is the one laid out at the text size and page shape set now (`book::set_text_size`, `set_page_shape`), as
/// its pages are drawn.
pub fn word_at(
    path: &Path,
    page: usize,
    size: (u32, u32),
    x: u32,
    y: u32,
) -> Result<Option<(String, PageBox)>, DecodeError> {
    let Some((words, units)) = page_words(path, page)? else {
        return Ok(None);
    };
    Ok(nearest(&words, units, size, (x, y)))
}

/// The text of page `page` (counted from 0; past the end, the last) in reading order, for reading aloud: the lines of a paragraph
/// joined by a space and a word broken at a line's end joined again; a line break between paragraphs where the file marks them (a book
/// whose text flows always; a DjVu text layer where it says so, on about one page in seven of the books measured; a PDF never, its lines
/// are not told apart from its paragraphs). `None` for a page with no text and for any other file.
pub fn page_text(path: &Path, page: usize) -> Result<Option<String>, DecodeError> {
    if let Some(laid) = laid(path)? {
        let index = page.min(laid.pages.len().saturating_sub(1));
        return Ok(laid
            .pages
            .get(index)
            .map(|p| p.text.trim_end().to_string())
            .filter(|t| !t.trim().is_empty()));
    }
    if crate::djvu::is_djvu(path) {
        return Ok(crate::djvu::page_text(path, page)?
            .map(|t| readable(&t, &['\u{1f}', '\u{1d}', '\u{0b}']))
            .filter(|t| !t.is_empty()));
    }
    if crate::pdf::is_pdf(path) {
        let (text, _) = crate::pdf_text::page_words(path, page)?;
        return Ok(Some(readable(&text.text, &[])).filter(|t| !t.is_empty()));
    }
    Ok(None)
}

/// The layout of a book whose text flows, `None` for any other file.
fn laid(path: &Path) -> Result<Option<std::sync::Arc<crate::book::Laid>>, DecodeError> {
    use crate::book;
    Ok(Some(if crate::fb2::is_fb2(path) {
        book::laid(path, crate::fb2::blocks)?
    } else if crate::epub::is_epub(path) {
        book::laid(path, crate::epub::blocks)?
    } else if crate::html::is_html(path) {
        book::laid(path, crate::html::blocks)?
    } else if crate::docx::is_docx(path) {
        book::laid(path, crate::docx::blocks)?
    } else {
        return Ok(None);
    }))
}

/// The words of a page and the size of the page they are counted in.
fn page_words(path: &Path, page: usize) -> Result<Option<PageWords>, DecodeError> {
    if let Some(laid) = laid(path)? {
        let index = page.min(laid.pages.len().saturating_sub(1));
        return Ok(laid.pages.get(index).map(|p| (p.words.clone(), laid.units())));
    }
    if crate::djvu::is_djvu(path) {
        return crate::djvu::page_words(path, page);
    }
    if crate::pdf::is_pdf(path) {
        let (text, size) = crate::pdf_text::page_words(path, page)?;
        return Ok(Some((text.words, size)));
    }
    Ok(None)
}

/// The words of page `page` from the one at `from` to the one at `to` (points of a picture of `size`, as for [`word_at`]; either may
/// come first), as a phrase, and their boxes in those pixels, one for each line they take: what a finger drawn over a passage picks. The
/// word at `from` is found as [`word_at`] finds it (none there: `None`); the one at `to` is the nearest at any distance, so that a finger
/// taken into a margin or between lines ends the phrase at the word nearest to it. The phrase is the words in the page's order, lines
/// joined by a space and a word broken at a line's end made whole ([`readable`]), without the marks before its first word or after its
/// last (a full stop, a question or an exclamation stays). `None` as [`word_at`] gives it.
pub fn words_between(
    path: &Path,
    page: usize,
    size: (u32, u32),
    from: (u32, u32),
    to: (u32, u32),
) -> Result<Option<(String, Vec<PageBox>)>, DecodeError> {
    let Some((words, units)) = page_words(path, page)? else {
        return Ok(None);
    };
    Ok(between(&words, units, size, from, to))
}

/// [`words_between`] over the words of a page counted in `units`.
fn between(
    words: &[Word],
    units: (u32, u32),
    size: (u32, u32),
    from: (u32, u32),
    to: (u32, u32),
) -> Option<(String, Vec<PageBox>)> {
    let first = nearest_index(words, units, size, from, true)?;
    let last = nearest_index(words, units, size, to, false)?;
    let (first, last) = (first.min(last), first.max(last));
    let chosen: Vec<&Word> = words.get(first..=last)?.iter().filter(|w| one_word(w)).collect();
    // the lines: a word that does not stand beside the one before it (not level with it, or back at the left) begins one
    let mut lines: Vec<Vec<&Word>> = Vec::new();
    for word in chosen {
        match lines.last_mut() {
            Some(line) if line.last().is_some_and(|before| beside(before, word)) => line.push(word),
            _ => lines.push(vec![word]),
        }
    }
    let text = lines
        .iter()
        .map(|line| line.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n");
    let text = readable(&text, &[]);
    let text = text
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '.' | '!' | '?' | '…'));
    if text.is_empty() {
        return None;
    }
    let scale = scales(units, size)?;
    let boxes = lines
        .iter()
        .filter_map(|line| {
            let rect = line.iter().map(|w| w.rect).reduce(|a, b| peekel_djvu::text::Rect {
                left: a.left.min(b.left),
                top: a.top.min(b.top),
                right: a.right.max(b.right),
                bottom: a.bottom.max(b.bottom),
            })?;
            Some(box_of(rect, scale, size))
        })
        .collect();
    Some((text.to_string(), boxes))
}

/// Whether `word` stands on the same line as `before`, after it: their heights overlap by half the smaller, and it is not left of it.
fn beside(before: &Word, word: &Word) -> bool {
    let (a, b) = (before.rect, word.rect);
    let overlap = a.bottom.min(b.bottom) - a.top.max(b.top);
    let smaller = (a.bottom - a.top).min(b.bottom - b.top).max(1);
    overlap * 2 >= smaller && b.left >= a.left
}

/// The scale from a page counted in `units` to a picture of `size`; `None` when either is empty.
fn scales(units: (u32, u32), size: (u32, u32)) -> Option<(f64, f64)> {
    if units.0 == 0 || units.1 == 0 || size.0 == 0 || size.1 == 0 {
        return None;
    }
    Some((
        f64::from(size.0) / f64::from(units.0),
        f64::from(size.1) / f64::from(units.1),
    ))
}

/// `rect` of a page, at `scale`, as a box of a picture of `size` (kept within it, at least a pixel each way).
fn box_of(rect: peekel_djvu::text::Rect, scale: (f64, f64), size: (u32, u32)) -> PageBox {
    let edge = |v: i32, scale: f64, limit: u32| ((f64::from(v) * scale).round().max(0.0) as u32).min(limit);
    let (left, top) = (edge(rect.left, scale.0, size.0), edge(rect.top, scale.1, size.1));
    let (right, bottom) = (edge(rect.right, scale.0, size.0), edge(rect.bottom, scale.1, size.1));
    PageBox {
        x: left,
        y: top,
        width: right.saturating_sub(left).max(1),
        height: bottom.saturating_sub(top).max(1),
    }
}

/// Whether `word` is one word: not a mark alone (a dash), not a line (a zone of a DjVu text layer whose zones go no deeper).
fn one_word(word: &Word) -> bool {
    let text = bare(&word.text);
    !text.is_empty() && !text.contains(char::is_whitespace)
}

/// Which of `words` (counted in a page of `units`) is nearest to `point` (in the pixels of a picture of `size`): within half a line of
/// it when `within`, else at any distance.
fn nearest_index(
    words: &[Word],
    units: (u32, u32),
    size: (u32, u32),
    point: (u32, u32),
    within: bool,
) -> Option<usize> {
    let (sx, sy) = scales(units, size)?;
    let (px, py) = (f64::from(point.0) / sx, f64::from(point.1) / sy);
    let (index, _) = words
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let r = w.rect;
            let (left, top, right, bottom) = (
                f64::from(r.left),
                f64::from(r.top),
                f64::from(r.right),
                f64::from(r.bottom),
            );
            let away_x = (left - px).max(px - right).max(0.0);
            let away_y = (top - py).max(py - bottom).max(0.0);
            let reach = (bottom - top).max(1.0) * NEAR;
            let away = away_x.hypot(away_y);
            ((!within || away <= reach) && one_word(w)).then_some((i, away))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    Some(index)
}

/// The word nearest to `point` (in the pixels of a picture of `size`) among `words` (counted in a page of `units`), within half a line
/// of it, and its box in those pixels.
fn nearest(words: &[Word], units: (u32, u32), size: (u32, u32), point: (u32, u32)) -> Option<(String, PageBox)> {
    let index = nearest_index(words, units, size, point, true)?;
    let word = words.get(index)?;
    Some((
        bare(&word.text).to_string(),
        box_of(word.rect, scales(units, size)?, size),
    ))
}

/// A word without the marks round it: what is not a letter or a digit at its ends (`«Слово,»` is `Слово`; `пол-года` stays).
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

/// Text for reading aloud from lines: the lines joined by a space, a word broken at a line's end (`сло-` and a line that starts with a
/// small letter) joined again; each of `paragraph` (the marks of a DjVu text layer) a line break; spaces made one, control characters out.
fn readable(text: &str, paragraph: &[char]) -> String {
    let mut out = String::new();
    for block in text.split(|c| paragraph.contains(&c)) {
        let mut joined = String::new();
        for line in block.split('\n') {
            let line: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if line.is_empty() {
                continue;
            }
            // the head of a word broken at the line's end, without its hyphen
            let head = joined
                .strip_suffix('-')
                .and_then(|before| before.rsplit(' ').next())
                .filter(|head| head.chars().last().is_some_and(char::is_alphabetic))
                .map(str::to_string);
            let first = line.split(' ').next().unwrap_or_default();
            match head {
                // the line starts with the whole word again (a text layer of FineReader's does so): the head goes
                Some(head)
                    if first.chars().count() > head.chars().count()
                        && first.to_lowercase().starts_with(&head.to_lowercase()) =>
                {
                    joined.truncate(joined.len() - head.len() - 1);
                    if !joined.is_empty() && !joined.ends_with(' ') {
                        joined.push(' ');
                    }
                }
                // the tail of the word
                Some(_) if first.chars().next().is_some_and(char::is_lowercase) => {
                    joined.pop();
                }
                _ if !joined.is_empty() => joined.push(' '),
                _ => {}
            }
            joined.push_str(&line);
        }
        if !joined.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&joined);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that finds no word where it must should stop at once
mod tests {
    use super::*;
    use peekel_djvu::text::Rect;

    fn word(text: &str, left: i32, top: i32, right: i32, bottom: i32) -> Word {
        Word {
            text: text.to_string(),
            rect: Rect {
                left,
                top,
                right,
                bottom,
            },
        }
    }

    #[test]
    fn the_word_pointed_at_or_the_nearest_within_half_a_line() {
        let words = [word("«Hello,", 10, 10, 50, 30), word("world»", 60, 10, 100, 30)];
        // a page of 200 x 100 drawn at twice its size
        let at = |x, y| nearest(&words, (200, 100), (400, 200), (x, y));
        let (text, b) = at(40, 40).expect("a word");
        assert_eq!(text, "Hello");
        assert_eq!(
            b,
            PageBox {
                x: 20,
                y: 20,
                width: 80,
                height: 40
            }
        );
        assert_eq!(at(150, 40).map(|w| w.0).as_deref(), Some("world"));
        // in the gap between them: the nearer
        assert_eq!(at(108, 40).map(|w| w.0).as_deref(), Some("Hello"));
        assert_eq!(at(112, 40).map(|w| w.0).as_deref(), Some("world"));
        // half a line below still; further, nothing
        assert_eq!(at(40, 78).map(|w| w.0).as_deref(), Some("Hello"));
        assert_eq!(at(40, 90), None);
        assert_eq!(at(390, 190), None);
        // a dash alone is no word, nor a line
        assert_eq!(nearest(&[word("—", 0, 0, 10, 10)], (20, 20), (20, 20), (5, 5)), None);
        assert_eq!(
            nearest(&[word("a line of words", 0, 0, 10, 10)], (20, 20), (20, 20), (5, 5)),
            None
        );
        assert_eq!(nearest(&words, (0, 0), (400, 200), (40, 40)), None);
    }

    #[test]
    fn a_phrase_from_one_word_to_another_with_a_box_for_each_line() {
        // two lines of a page of 200 x 100, the first word of the second the end of a word broken at the line's end
        let words = [
            word("«The", 10, 10, 40, 20),
            word("time", 45, 10, 70, 20),
            word("trav-", 75, 10, 100, 20),
            word("eller,", 10, 30, 40, 40),
            word("said", 45, 30, 70, 40),
            word("he.»", 75, 30, 100, 40),
        ];
        // drawn at twice its size
        let at = |from, to| between(&words, (200, 100), (400, 200), from, to);
        let (text, boxes) = at((30, 30), (190, 70)).expect("a phrase");
        assert_eq!(text, "The time traveller, said he.");
        assert_eq!(
            boxes,
            [
                PageBox {
                    x: 20,
                    y: 20,
                    width: 180,
                    height: 20
                },
                PageBox {
                    x: 20,
                    y: 60,
                    width: 180,
                    height: 20
                }
            ]
        );
        // dragged backwards, the same; one word alone, without its marks
        assert_eq!(
            at((190, 70), (30, 30)).map(|p| p.0).as_deref(),
            Some("The time traveller, said he.")
        );
        assert_eq!(at((110, 30), (110, 30)).map(|p| p.0).as_deref(), Some("time"));
        // the end taken into the margin far below: the nearest word still ends it; the start must be at a word
        assert_eq!(
            at((110, 30), (110, 199)).map(|p| p.0).as_deref(),
            Some("time traveller, said")
        );
        assert_eq!(at((110, 120), (190, 70)), None);
        assert_eq!(between(&words, (0, 0), (400, 200), (30, 30), (190, 70)), None);
    }

    #[test]
    fn lines_are_joined_and_a_broken_word_made_whole() {
        assert_eq!(readable("Это сло- \nво и ещё\nстрока.", &[]), "Это слово и ещё строка.");
        // the whole word again after its head: the head goes
        assert_eq!(readable("решение нера-\nнеравенства E", &[]), "решение неравенства E");
        assert_eq!(
            readable("Соответ-\nСоответствующие точки", &[]),
            "Соответствующие точки"
        );
        assert_eq!(readable("Нера-\nнеравенства", &[]), "неравенства");
        // a hyphen before a capital, or after a digit, stays
        assert_eq!(readable("Северо-\nЗапад", &[]), "Северо- Запад");
        assert_eq!(readable("1941-\nгод", &[]), "1941- год");
        // the marks of paragraphs, regions and columns, and the NUL that ends a DjVu page
        assert_eq!(
            readable(
                "One\nline.\u{1f}Two\u{1d}\u{0b}Three\0",
                &['\u{1f}', '\u{1d}', '\u{0b}']
            ),
            "One line.\nTwo\nThree"
        );
        assert_eq!(readable(" \n\t\n", &[]), "");
    }
}
