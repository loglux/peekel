//! What the readers of text documents (docx, odt) share in laying a paragraph out: the pieces of its content (`Atom`), the break
//! into lines, the alignment and the drawing of a line. A preview, not the layout of Word: the widths are a rough table
//! (`page::text_width`), so a line may be longer or shorter than in the program that made the file.

use std::sync::Arc;

use image::RgbaImage;

use crate::page::{Font, Page, text_width};

/// A tab goes to the next multiple of this many points from the start of the line.
const TAB: f32 = 36.0;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// How a paragraph is set: all in points, already kept within sane limits by the reader of the format.
pub struct Layout {
    pub align: Align,
    /// Space above and below.
    pub before: f32,
    pub after: f32,
    /// The height of a line as a multiple of the usual one.
    pub line_factor: f32,
    /// The indent of the whole paragraph, and of its first line against it (negative: hanging).
    pub left: f32,
    pub first: f32,
    /// A bullet in the hanging indent of the first line.
    pub bullet: bool,
    /// The font of the paragraph (its size sets the height of an empty line and of the bullet).
    pub font: Font,
}

/// A piece of a paragraph's content.
pub enum Atom {
    /// A word and whether a space follows it.
    Word {
        text: String,
        font: Font,
        space: bool,
    },
    Picture {
        image: Option<Arc<RgbaImage>>,
        width: f32,
        height: f32,
    },
    Tab,
    Break,
    PageBreak,
}

/// One paragraph: wrapped into lines and drawn from `y`; `y` is moved below it. False when the page is full (or a page break came).
pub fn paragraph(atoms: Vec<Atom>, set: &Layout, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
    let (line_factor, left, first, align, numbered) = (set.line_factor, set.left, set.first, set.align, set.bullet);
    *y += set.before;

    // the units that cannot be broken inside: words joined with no space between them, a picture, a tab
    let mut lines: Vec<Vec<Atom>> = vec![Vec::new()];
    let mut line_width = 0.0f32;
    let mut line_index = 0usize;
    let mut unit: Vec<Atom> = Vec::new();
    let mut page_break = false;
    let available = |line: usize| (width - left - if line == 0 { first } else { 0.0 }).max(20.0);
    let too_long = |a: &Atom| atom_width(a, false) > available(1) * 1.2;
    let flush_unit =
        |unit: &mut Vec<Atom>, lines: &mut Vec<Vec<Atom>>, line_width: &mut f32, line_index: &mut usize| {
            if unit.is_empty() {
                return;
            }
            let w: f32 = unit.iter().map(|a| atom_width(a, false)).sum();
            if *line_width + w > available(*line_index) && lines.last().is_some_and(|l| !l.is_empty()) {
                // the space that ended the last word of the line is not part of its width
                lines.push(Vec::new());
                *line_width = 0.0;
                *line_index += 1;
            }
            *line_width += unit.iter().map(|a| atom_width(a, true)).sum::<f32>();
            if let Some(line) = lines.last_mut() {
                line.append(unit);
            }
        };
    for atom in atoms {
        match atom {
            Atom::Break => {
                flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
                lines.push(Vec::new());
                line_width = 0.0;
                line_index += 1;
            }
            Atom::PageBreak => {
                flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
                page_break = true;
                break;
            }
            Atom::Tab | Atom::Picture { .. } => {
                flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
                unit.push(atom);
                flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
            }
            Atom::Word { .. } if too_long(&atom) => {
                // a word wider than a line (a web address) is broken where the line ends, as Word does
                if let Atom::Word { text, font, space } = atom {
                    let limit = available(1);
                    let mut chunks: Vec<String> = vec![String::new()];
                    for c in text.chars() {
                        let current = chunks.last().cloned().unwrap_or_default();
                        if !current.is_empty() && text_width(&format!("{current}{c}"), &font) > limit {
                            chunks.push(String::new());
                        }
                        if let Some(last) = chunks.last_mut() {
                            last.push(c);
                        }
                    }
                    let count = chunks.len();
                    for (i, chunk) in chunks.into_iter().enumerate() {
                        unit.push(Atom::Word {
                            text: chunk,
                            font: font.clone(),
                            space: space && i + 1 == count,
                        });
                        flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
                    }
                }
            }
            Atom::Word { space, .. } => {
                unit.push(atom);
                if space {
                    flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);
                }
            }
        }
    }
    flush_unit(&mut unit, &mut lines, &mut line_width, &mut line_index);

    let marker = numbered.then(|| Font {
        bold: false,
        ..set.font.clone()
    });
    let mut fits = true;
    let mut first_on_page = true;
    for (index, line) in lines.iter().enumerate() {
        // the height of the line: its biggest text or picture
        let size = line
            .iter()
            .filter_map(|a| match a {
                Atom::Word { font, .. } => Some(font.size),
                _ => None,
            })
            .fold(set.font.size, f32::max);
        let tallest = line
            .iter()
            .filter_map(|a| match a {
                Atom::Picture { height, .. } => Some(*height),
                _ => None,
            })
            .fold(0.0, f32::max);
        let ascent = (size * 0.95).max(tallest);
        let height = (ascent + size * 0.25) * line_factor;
        if *y + height > bottom && !(first_on_page && *y <= bottom && out.is_empty()) {
            fits = false;
            break;
        }
        first_on_page = false;
        let baseline = *y + height - size * 0.25 * line_factor;
        let start = x + left + if index == 0 { first } else { 0.0 };
        let used: f32 = line.iter().map(|a| atom_width(a, false)).sum::<f32>();
        let room = available(index);
        let mut cursor = match align {
            Align::Left => start,
            Align::Center => start + ((room - used) / 2.0).max(0.0),
            Align::Right => start + (room - used).max(0.0),
        };
        if index == 0
            && let Some(font) = &marker
        {
            out.text(start - 12.0_f32.max(-first), baseline, "\u{2022}", font);
        }
        draw_line(line, &mut cursor, baseline, start, room, out);
        *y += height;
    }
    *y += set.after;
    fits && !page_break
}

/// The atoms of one line: the words between two tabs or pictures are one line of text made of pieces in their own
/// fonts, which the renderer sets with the real widths (the widths here are only a guess, for wrapping).
fn draw_line(line: &[Atom], cursor: &mut f32, baseline: f32, start: f32, room: f32, out: &mut Page) {
    let mut runs: Vec<(String, Font)> = Vec::new();
    let mut from = *cursor;
    let flush = |runs: &mut Vec<(String, Font)>, from: f32, out: &mut Page| {
        if let Some((text, _)) = runs.last_mut() {
            let trimmed = text.trim_end().len();
            text.truncate(trimmed);
        }
        out.runs(from, baseline, std::mem::take(runs));
    };
    for atom in line {
        match atom {
            Atom::Word { text, font, space } => {
                if runs.last().is_none_or(|(_, f)| f != font) {
                    runs.push((String::new(), font.clone()));
                }
                if let Some((piece, _)) = runs.last_mut() {
                    piece.push_str(text);
                    if *space {
                        piece.push(' ');
                    }
                }
                *cursor += atom_width(atom, true);
            }
            Atom::Tab => {
                flush(&mut runs, from, out);
                let rel = *cursor - start;
                *cursor = start + ((rel / TAB).floor() + 1.0) * TAB;
                from = *cursor;
            }
            Atom::Picture { image, width, height } => {
                flush(&mut runs, from, out);
                // wider than the line: shrunk to it, in proportion
                let shrink = (room / width).min(1.0);
                let (w, h) = (width * shrink, height * shrink);
                match image {
                    Some(image) => out.picture(*cursor, baseline - h, w, h, Arc::clone(image)),
                    None => out.rect(
                        *cursor,
                        baseline - h,
                        w,
                        h,
                        Some([235, 235, 235]),
                        Some(([170, 170, 170], 0.75)),
                    ),
                }
                *cursor += w;
                from = *cursor;
            }
            Atom::Break | Atom::PageBreak => {}
        }
    }
    flush(&mut runs, from, out);
}

/// The width of an atom; `with_space` counts the space after a word.
pub fn atom_width(atom: &Atom, with_space: bool) -> f32 {
    match atom {
        Atom::Word { text, font, space } => {
            text_width(text, font)
                + if *space && with_space {
                    text_width(" ", font)
                } else {
                    0.0
                }
        }
        Atom::Picture { width, .. } => *width,
        Atom::Tab => 0.0,
        Atom::Break | Atom::PageBreak => 0.0,
    }
}

/// The words of a piece of text: a space after each, and one before the first when the text starts with white space.
pub fn words(text: &str, font: &Font, atoms: &mut Vec<Atom>) {
    let starts_with_space = text.starts_with(char::is_whitespace);
    let ends_with_space = text.ends_with(char::is_whitespace);
    if starts_with_space && let Some(Atom::Word { space, .. }) = atoms.last_mut() {
        *space = true;
    }
    let pieces: Vec<&str> = text.split_whitespace().collect();
    for (i, piece) in pieces.iter().enumerate() {
        atoms.push(Atom::Word {
            text: (*piece).to_string(),
            font: font.clone(),
            space: i + 1 < pieces.len() || ends_with_space,
        });
    }
    // a text of nothing but white space is a space between what is around it
    if pieces.is_empty()
        && !text.is_empty()
        && let Some(Atom::Word { space, .. }) = atoms.last_mut()
    {
        *space = true;
    }
}
