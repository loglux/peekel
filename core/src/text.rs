//! Paragraphs wrapped into a box and drawn on a `Page`: what a slide's text box and a cell of a sheet share.
//!
//! Words are measured by `page::text_width` (a rough table): the wrapping is a guess that is near enough for a preview,
//! and the renderer sets every line with the real font. A word that is much wider than the box (a web address) is broken
//! where the line ends.

use crate::page::{Font, Page, text_width};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A paragraph: pieces of text each in its own font (a `\n` in a piece is a line break), and how it is set.
#[derive(Clone, Debug, Default)]
pub struct Para {
    pub runs: Vec<(String, Font)>,
    pub align: Align,
    /// Indent of the whole paragraph, and of its first line (negative: hanging), in points.
    pub left: f32,
    pub first: f32,
    /// Space before and after, in points.
    pub before: f32,
    pub after: f32,
    /// Line height as a multiple of the usual one (1.0).
    pub line: f32,
    /// A bullet in this font, and how far from the text's left edge it stands (negative: to the left of it; `None`: one
    /// size of the font to the left).
    pub bullet: Option<Font>,
    pub bullet_offset: Option<f32>,
    /// The sign of the bullet (a dot when `None`).
    pub bullet_char: Option<String>,
    /// The size of an empty line (the paragraph has no text).
    pub empty_size: f32,
}

/// A word, the font it is in, and whether a space follows it.
type Word = (String, Font, bool);

/// The words of a paragraph, grouped into units that are not broken inside (pieces with no space between them), with a
/// `None` unit for a line break.
fn units(para: &Para) -> Vec<Option<Vec<Word>>> {
    let mut out: Vec<Option<Vec<Word>>> = Vec::new();
    let mut unit: Vec<Word> = Vec::new();
    for (text, font) in &para.runs {
        for (n, piece) in text.split('\n').enumerate() {
            if n > 0 {
                if !unit.is_empty() {
                    out.push(Some(std::mem::take(&mut unit)));
                }
                out.push(None);
            }
            if piece.starts_with(char::is_whitespace)
                && let Some(last) = unit.last_mut()
            {
                last.2 = true;
            }
            let pieces: Vec<&str> = piece.split_whitespace().collect();
            for (i, word) in pieces.iter().enumerate() {
                let space = i + 1 < pieces.len() || piece.ends_with(char::is_whitespace);
                unit.push(((*word).to_string(), font.clone(), space));
                if space {
                    out.push(Some(std::mem::take(&mut unit)));
                }
            }
            if pieces.is_empty()
                && !piece.is_empty()
                && let Some(last) = unit.last_mut()
            {
                last.2 = true;
                out.push(Some(std::mem::take(&mut unit)));
            }
        }
    }
    if !unit.is_empty() {
        out.push(Some(unit));
    }
    out
}

fn word_width(word: &Word, with_space: bool) -> f32 {
    text_width(&word.0, &word.1)
        + if word.2 && with_space {
            text_width(" ", &word.1)
        } else {
            0.0
        }
}

/// One line: the words on it.
type Line = Vec<Word>;

/// The words of a paragraph broken into lines of at most `width` points (the first line starts `first` in).
fn wrap(para: &Para, width: f32) -> Vec<Line> {
    let room = |line: usize| (width - para.left - if line == 0 { para.first } else { 0.0 }).max(10.0);
    let mut lines: Vec<Line> = vec![Vec::new()];
    let mut used = 0.0f32;
    for unit in units(para) {
        let Some(unit) = unit else {
            lines.push(Vec::new());
            used = 0.0;
            continue;
        };
        // a word that is much wider than the room: broken where the line ends
        let unit = if unit.len() == 1 && word_width(&unit[0], false) > room(1) * 1.2 {
            let (text, font, space) = unit[0].clone();
            let mut chunks: Vec<String> = vec![String::new()];
            for c in text.chars() {
                let current = chunks.last().cloned().unwrap_or_default();
                if !current.is_empty() && text_width(&format!("{current}{c}"), &font) > room(1) {
                    chunks.push(String::new());
                }
                if let Some(last) = chunks.last_mut() {
                    last.push(c);
                }
            }
            let count = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let piece = vec![(chunk, font.clone(), space && i + 1 == count)];
                place(&mut lines, &mut used, piece, &room);
            }
            continue;
        } else {
            unit
        };
        place(&mut lines, &mut used, unit, &room);
    }
    lines
}

/// Puts a unit on the last line, or on a new one when it does not fit.
fn place(lines: &mut Vec<Line>, used: &mut f32, unit: Vec<Word>, room: &dyn Fn(usize) -> f32) {
    let wide: f32 = unit.iter().map(|w| word_width(w, false)).sum();
    if *used + wide > room(lines.len() - 1) && lines.last().is_some_and(|l| !l.is_empty()) {
        lines.push(Vec::new());
        *used = 0.0;
    }
    *used += unit.iter().map(|w| word_width(w, true)).sum::<f32>();
    if let Some(line) = lines.last_mut() {
        line.extend(unit);
    }
}

/// The height of a line of words in points.
fn line_height(line: &[Word], base: f32, factor: f32) -> f32 {
    let size = line.iter().map(|w| w.1.size).fold(base, f32::max);
    size * 1.2 * factor
}

/// Lays `paras` out in a box `width` wide with its top at `y`, drawing on `page` down to `bottom` (a line that would pass
/// it is not drawn). Returns the height the text takes (whole, even where it was cut).
pub fn layout(paras: &[Para], x: f32, y: f32, width: f32, bottom: f32, page: &mut Page) -> f32 {
    let mut at = y;
    for para in paras {
        at += para.before.max(0.0);
        let factor = if para.line > 0.0 { para.line } else { 1.0 };
        let lines = wrap(para, width);
        for (index, line) in lines.iter().enumerate() {
            let height = line_height(line, para.empty_size.max(1.0), factor);
            let size = line.iter().map(|w| w.1.size).fold(para.empty_size.max(1.0), f32::max);
            let baseline = at + height - size * 0.25 * factor;
            let indent = para.left + if index == 0 { para.first } else { 0.0 };
            let room = (width - indent).max(10.0);
            let used: f32 = line.iter().map(|w| word_width(w, false)).sum();
            let start = x
                + indent
                + match para.align {
                    Align::Left => 0.0,
                    Align::Center => ((room - used) / 2.0).max(0.0),
                    Align::Right => (room - used).max(0.0),
                };
            if at + height <= bottom + 0.5 {
                if index == 0
                    && let Some(font) = &para.bullet
                {
                    let bullet_x = x + para.left + para.bullet_offset.unwrap_or(-font.size);
                    page.text(
                        bullet_x,
                        baseline,
                        para.bullet_char.as_deref().unwrap_or("\u{2022}"),
                        font,
                    );
                }
                let mut runs: Vec<(String, Font)> = Vec::new();
                for (text, font, space) in line {
                    if runs.last().is_none_or(|(_, f)| f != font) {
                        runs.push((String::new(), font.clone()));
                    }
                    if let Some((piece, _)) = runs.last_mut() {
                        piece.push_str(text);
                        if *space {
                            piece.push(' ');
                        }
                    }
                }
                if let Some((piece, _)) = runs.last_mut() {
                    let keep = piece.trim_end().len();
                    piece.truncate(keep);
                }
                page.runs(start, baseline, runs);
            }
            at += height;
        }
        at += para.after.max(0.0);
    }
    at - y
}

/// The height `paras` take in a box `width` wide, without drawing anything.
pub fn measure(paras: &[Para], width: f32) -> f32 {
    let mut scratch = Page::new(width.max(1.0), 1.0);
    layout(paras, 0.0, 0.0, width, f32::MAX, &mut scratch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, size: f32) -> Para {
        Para {
            runs: vec![(
                text.to_string(),
                Font {
                    size,
                    ..Font::default()
                },
            )],
            empty_size: size,
            line: 1.0,
            ..Para::default()
        }
    }

    #[test]
    fn words_wrap_at_the_width_and_a_line_break_starts_a_new_line() {
        let long = para(&"word ".repeat(40), 12.0);
        let lines = wrap(&long, 200.0);
        assert!(lines.len() > 3, "{} lines", lines.len());
        assert!(lines.iter().all(|l| !l.is_empty()));
        let broken = para("one\ntwo", 12.0);
        assert_eq!(wrap(&broken, 500.0).len(), 2);
        assert_eq!(wrap(&para("", 12.0), 100.0).len(), 1);
    }

    #[test]
    fn a_word_far_wider_than_the_box_is_broken_and_a_slightly_wide_one_is_not() {
        let wide = para(&"x".repeat(300), 12.0);
        assert!(wrap(&wide, 100.0).len() > 5);
        let width = text_width(
            "Thursday,",
            &Font {
                size: 12.0,
                ..Font::default()
            },
        );
        assert_eq!(
            wrap(&para("Thursday,", 12.0), width * 0.9).len(),
            1,
            "a little wide stays whole"
        );
    }

    #[test]
    fn the_height_grows_with_the_text_and_the_spacing() {
        let one = measure(&[para("one line", 12.0)], 300.0);
        let many = measure(&[para(&"word ".repeat(100), 12.0)], 300.0);
        assert!(many > one * 4.0, "{one} {many}");
        let mut spaced = para("one line", 12.0);
        spaced.before = 10.0;
        spaced.after = 5.0;
        assert!((measure(&[spaced], 300.0) - one - 15.0).abs() < 0.01);
    }
}
