//! The width of a piece of text in the font it is drawn in, for a page whose lines must end where the drawing ends them (a book, set
//! justified): the advance widths of the letters in the font file the renderer picks for the family (`svg::fonts`, the same
//! database), read with `skrifa`. Kerning and shaping are not counted, so a word may come out a little wider than drawn; across a word
//! that is a fraction of a point. A letter the font has not (the renderer takes another font for it) and a family that is not installed
//! are measured by the rough table of `page::text_width`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use resvg::usvg::fontdb::{Database, Family, ID, Query, Stretch, Style, Weight};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, MetadataProvider};

use crate::page::{Font, text_width};

/// A font file read for measuring: its bytes, the face in it, and the widths of the letters met so far, in units of the size.
struct Face {
    data: Arc<Vec<u8>>,
    index: u32,
    widths: HashMap<char, Option<f32>>,
}

/// The faces by the font asked for (the family list, bold, italic); `None` for one the database has no face for.
type Faces = HashMap<(String, bool, bool), Option<Face>>;
static FACES: Mutex<Option<Faces>> = Mutex::new(None);

/// The width of `text` in `font`, in points.
pub fn width(text: &str, font: &Font) -> f32 {
    let key = (crate::page::family(font.family.as_deref()), font.bold, font.italic);
    let Ok(mut faces) = FACES.lock() else {
        return text_width(text, font);
    };
    let faces = faces.get_or_insert_with(HashMap::new);
    let face = faces
        .entry(key)
        .or_insert_with_key(|(families, bold, italic)| load(&crate::svg::fonts(), families, *bold, *italic));
    let Some(face) = face else {
        return text_width(text, font);
    };
    let mut units = 0.0f32;
    let data = Arc::clone(&face.data);
    let mut font_ref: Option<Option<FontRef<'_>>> = None;
    for c in text.chars() {
        let known = match face.widths.get(&c) {
            Some(known) => *known,
            None => {
                let parsed = font_ref.get_or_insert_with(|| FontRef::from_index(&data, face.index).ok());
                let measured = parsed.as_ref().and_then(|f| advance(f, c));
                face.widths.insert(c, measured);
                measured
            }
        };
        units += match known {
            Some(units) => units,
            None => {
                // a letter of another font: the rough table, at the size
                let plain = Font {
                    bold: false,
                    ..font.clone()
                };
                text_width(&c.to_string(), &plain) / font.size.max(0.01)
            }
        };
    }
    units * font.size
}

/// The advance of `c` in units of the size, or `None` when the font has no letter for it.
fn advance(font: &FontRef<'_>, c: char) -> Option<f32> {
    let glyph = font.charmap().map(c)?;
    let units_per_em = f32::from(font.metrics(Size::unscaled(), LocationRef::default()).units_per_em);
    let advance = font
        .glyph_metrics(Size::unscaled(), LocationRef::default())
        .advance_width(glyph)?;
    (units_per_em > 0.0).then(|| advance / units_per_em)
}

/// The face the renderer takes for a family list (`Georgia, Liberation Serif, serif`), read into memory.
fn load(db: &Database, families: &str, bold: bool, italic: bool) -> Option<Face> {
    let id = face(db, families, bold, italic)?;
    let (data, index) = db.with_face_data(id, |data, index| (data.to_vec(), index))?;
    Some(Face {
        data: Arc::new(data),
        index,
        widths: HashMap::new(),
    })
}

/// The face the renderer takes for a family list: the first family of it that `db` has, else the serif family, as `usvg`'s own choice
/// of a font does (`default_font_selector`). Without that last step a list none of whose families is installed (the core's monospace
/// list on Android, Peekel TV, #235) was measured by the rough table and drawn in the serif face, and its words ran into each other.
fn face(db: &Database, families: &str, bold: bool, italic: bool) -> Option<ID> {
    let mut names: Vec<Family<'_>> = families
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|name| match name.to_ascii_lowercase().as_str() {
            "serif" => Family::Serif,
            "sans-serif" => Family::SansSerif,
            "monospace" => Family::Monospace,
            _ => Family::Name(name),
        })
        .collect();
    names.push(Family::Serif);
    db.query(&Query {
        families: &names,
        weight: if bold { Weight::BOLD } else { Weight::NORMAL },
        stretch: Stretch::Normal,
        style: if italic { Style::Italic } else { Style::Normal },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_measured_and_wider_letters_make_it_wider() {
        let font = Font {
            size: 12.0,
            family: Some("Times New Roman".to_string()),
            ..Font::default()
        };
        let narrow = width("iiii", &font);
        let wide = width("MMMM", &font);
        assert!(narrow > 0.0 && wide > narrow * 2.0, "{narrow} {wide}");
        assert_eq!(width("", &font), 0.0);
        // twice the size, twice the width
        let big = Font {
            size: 24.0,
            ..font.clone()
        };
        assert!((width("Word", &big) - 2.0 * width("Word", &font)).abs() < 0.01);
    }

    /// A database of one face: the serif face of this machine's fonts, alone, as on a system whose fonts have no monospace face the
    /// core knows by name (Android's); `None` on a machine with no serif face at all.
    fn serif_only() -> Option<(Database, Vec<u8>)> {
        let all = crate::svg::fonts();
        let id = all.query(&Query {
            families: &[Family::Serif],
            ..Query::default()
        })?;
        let data = all.with_face_data(id, |data, _| data.to_vec())?;
        let mut db = Database::new();
        db.load_font_data(data.clone());
        crate::svg::set_generic_families(&mut db);
        Some((db, data))
    }

    #[test]
    fn a_family_list_with_nothing_installed_is_measured_in_the_serif_face_it_is_drawn_in() {
        let Some((db, data)) = serif_only() else {
            return;
        };
        // the core's list for a line of code, as a page names it
        let mono = crate::page::family(Some("Consolas, Courier New, monospace"));
        for (bold, italic) in [(false, false), (true, false), (false, true)] {
            let face = load(&db, &mono, bold, italic).map(|f| f.data.as_ref().clone());
            assert_eq!(face.as_ref(), Some(&data), "{mono} {bold} {italic}");
        }
    }

    #[test]
    fn a_family_that_is_not_installed_falls_to_one_that_is() {
        let font = Font {
            family: Some("No Such Family At All".to_string()),
            ..Font::default()
        };
        assert!(width("Hello", &font) > 0.0);
    }
}
