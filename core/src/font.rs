//! What a TrueType or OpenType font file says about itself (its names, weight, style, glyphs, version, designer, licence), read
//! with `skrifa` (which is memory safe and was in the tree already: `hayro` brings it), and whether it has the letters of a
//! language, so that a card can show a line of text set in the font.
//!
//! Read: `.ttf`, `.otf` and the first font of a `.ttc`. Not read: WOFF and WOFF2 (compressed), Type 1, bitmap fonts.

use skrifa::attribute::{Attributes, Style};
use skrifa::raw::TableProvider;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};

/// A font's first (or only) face of the file, or `None` when the bytes are no font that can be read.
fn face(data: &[u8]) -> Option<FontRef<'_>> {
    FontRef::from_index(data, 0).ok()
}

/// A name of the font as text, the English one when there is one, and at most `limit` characters.
fn name(font: &FontRef<'_>, id: StringId, limit: usize) -> Option<String> {
    let text: String = font
        .localized_strings(id)
        .english_or_first()?
        .chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect();
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The family of the font as it is called in a `font-family`: the typographic family when there is one, else the family.
pub fn family(data: &[u8]) -> Option<String> {
    let font = face(data)?;
    name(&font, StringId::TYPOGRAPHIC_FAMILY_NAME, 100).or_else(|| name(&font, StringId::FAMILY_NAME, 100))
}

/// Whether the font has a glyph for every one of the letters of `text`.
pub fn has_letters(data: &[u8], text: &str) -> bool {
    let Some(font) = face(data) else {
        return false;
    };
    let map = font.charmap();
    text.chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| map.map(c).is_some())
}

/// The rows (label, value) a font file says about itself.
pub fn properties(data: &[u8]) -> Vec<(String, String)> {
    let Some(font) = face(data) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value {
            rows.push((label.to_string(), value));
        }
    };
    push(
        "Family",
        name(&font, StringId::TYPOGRAPHIC_FAMILY_NAME, 100).or_else(|| name(&font, StringId::FAMILY_NAME, 100)),
    );
    push(
        "Style",
        name(&font, StringId::TYPOGRAPHIC_SUBFAMILY_NAME, 100).or_else(|| name(&font, StringId::SUBFAMILY_NAME, 100)),
    );
    push("Full name", name(&font, StringId::FULL_NAME, 100));
    push("Version", name(&font, StringId::VERSION_STRING, 100));
    let attributes = Attributes::new(&font);
    let weight = attributes.weight.value();
    let slant = match attributes.style {
        Style::Normal => "",
        Style::Italic => ", italic",
        Style::Oblique(_) => ", oblique",
    };
    push("Weight", Some(format!("{}{slant}", weight.round() as u32)));
    push("Glyphs", font.maxp().ok().map(|m| m.num_glyphs().to_string()));
    push("Designer", name(&font, StringId::DESIGNER, 100));
    push("Licence", name(&font, StringId::LICENSE_DESCRIPTION, 200));
    push("Copyright", name(&font, StringId::COPYRIGHT_NOTICE, 200));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_no_font_gives_nothing() {
        assert!(properties(b"").is_empty());
        assert!(properties(b"not a font at all").is_empty());
        assert!(properties(&[0, 1, 0, 0, 0, 1, 0, 0]).is_empty());
        assert_eq!(family(b"nope"), None);
        assert!(!has_letters(b"nope", "a"));
    }

    /// The fonts of Windows (or of a Linux with some), if there is one: the real thing, where there is one to look at.
    fn some_font() -> Option<Vec<u8>> {
        [
            r"C:\Windows\Fonts\arial.ttf",
            r"C:\Windows\Fonts\segoeui.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ]
        .iter()
        .find_map(|p| std::fs::read(p).ok())
    }

    #[test]
    #[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
    fn a_real_font_says_its_family_weight_and_letters() {
        let Some(data) = some_font() else {
            return; // a machine with none of the fonts tried: nothing to look at
        };
        let rows = properties(&data);
        assert!(rows.iter().any(|(l, v)| l == "Family" && !v.is_empty()), "{rows:?}");
        assert!(rows.iter().any(|(l, _)| l == "Glyphs"), "{rows:?}");
        assert!(family(&data).is_some());
        assert!(has_letters(&data, "The quick brown fox"));
        assert!(!has_letters(&data, "\u{10FFFE}"));
        let weight = rows.iter().find(|(l, _)| l == "Weight").expect("a weight");
        assert!(
            weight.1.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "{weight:?}"
        );
    }
}
