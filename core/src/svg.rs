//! SVG drawn into pixels with `resvg`.
//!
//! A vector picture has no pixel size of its own, and the viewer zooms by shrinking and cropping one raster. So the
//! drawing is made once, large enough to stay sharp at a few steps of zoom, and then handled like any other picture:
//! the longest side is 2048 pixels when the file's own size is smaller, the file's own size up to 4096, and 4096 beyond
//! that (at most 16 megapixels). It is not drawn again at a higher zoom.
//!
//! Not read: pictures inside the SVG (`<image>`; resvg's `raster-images` would bring a second set of decoders), and
//! nothing outside the file is ever opened (the resolvers of `<image>` return nothing). Text is drawn with the fonts installed on the machine; where the font is
//! missing, resvg picks another or draws nothing.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use image::RgbaImage;
use resvg::{tiny_skia, usvg};

use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, read_limited, unreadable};
use crate::xml::escape;

/// A bigger file is refused before it is parsed.
const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// A gzipped one is unpacked to this much at most (usvg would unpack without a limit: a few kilobytes can be gigabytes).
const MAX_UNPACKED: u64 = 64 * 1024 * 1024;
/// A file whose elements are nested deeper than this is refused: the drawing recurses once per level, and a file made
/// for it (1000 levels are enough) used up a thread's stack of 2 MB and ended the whole process.
const MAX_DEPTH: usize = 256;
/// Entities a drawing may declare, the length of one, and the `&` of the whole text when it declares any: all together
/// they cannot expand to more than some tens of megabytes.
const MAX_ENTITIES: usize = 64;
const MAX_ENTITY_VALUE: usize = 4096;
const MAX_REFERENCES: usize = 20_000;
/// A drawing smaller than this is made this large (on its longest side) ...
const SHARP_SIDE: f32 = 2048.0;
/// ... and a larger one is made no larger than this.
const MAX_SIDE: f32 = 4096.0;

/// Whether the name says SVG (plain or gzipped).
pub fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"))
}

/// The first of `candidates` that is an installed family (capitals do not matter), as the family is spelt there.
fn installed(db: &usvg::fontdb::Database, candidates: &[&str]) -> Option<String> {
    candidates.iter().find_map(|wanted| {
        db.faces()
            .flat_map(|face| face.families.iter())
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(name, _)| name.clone())
    })
}

/// The installed fonts, read once: it takes a while and every SVG with text needs them.
///
/// The families that stand for "sans serif", "serif" and "monospace" are Arial, Times New Roman and Courier New until
/// said otherwise, and a machine without them (a Linux one) would draw no text at all: each is set to the first font of
/// a list that is installed.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            if let Some(family) = installed(
                &db,
                &[
                    "Arial",
                    "Segoe UI",
                    "Liberation Sans",
                    "DejaVu Sans",
                    "Noto Sans",
                    "Helvetica",
                    "Ubuntu",
                ],
            ) {
                db.set_sans_serif_family(family);
            }
            if let Some(family) = installed(
                &db,
                &[
                    "Times New Roman",
                    "Liberation Serif",
                    "DejaVu Serif",
                    "Noto Serif",
                    "Georgia",
                ],
            ) {
                db.set_serif_family(family);
            }
            if let Some(family) = installed(
                &db,
                &[
                    "Courier New",
                    "Consolas",
                    "Liberation Mono",
                    "DejaVu Sans Mono",
                    "Noto Sans Mono",
                ],
            ) {
                db.set_monospace_family(family);
            }
            Arc::new(db)
        })
        .clone()
}

/// The family `font-family` falls to when an SVG names none: a sans serif one that is installed.
fn default_family() -> String {
    let db = fonts();
    db.family_name(&usvg::fontdb::Family::SansSerif).to_string()
}

/// The size in pixels a drawing of `width` × `height` units is made at.
fn pixel_size(width: f32, height: f32) -> (u32, u32) {
    let longest = width.max(height);
    let target = if longest < SHARP_SIDE {
        SHARP_SIDE
    } else {
        longest.min(MAX_SIDE)
    };
    let scale = target / longest;
    let side = |units: f32| ((units * scale).round() as u32).max(1);
    (side(width), side(height))
}

/// The text of an SVG that may be gzipped (`.svgz`): unpacked, within `MAX_UNPACKED`.
fn unpack(data: Vec<u8>) -> Result<Vec<u8>, DecodeError> {
    use std::io::Read;
    if !data.starts_with(&[0x1f, 0x8b]) {
        return Ok(data);
    }
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(&data[..])
        .take(MAX_UNPACKED + 1)
        .read_to_end(&mut text)
        .map_err(|_| unreadable("the gzip of the SVG is damaged"))?;
    if text.len() as u64 > MAX_UNPACKED {
        return Err(DecodeError::TooLarge);
    }
    Ok(text)
}

/// Entities a file declares are what a "billion laughs" file is made of (one entity made of many of another, and so on):
/// usvg's parser allows them and has no limit on what they expand to. A few short entities that name no other entity
/// (the namespaces Illustrator wrote for years) are fine; anything else is refused, and so is a file that uses very many.
fn check_entities(data: &[u8]) -> Result<(), DecodeError> {
    const DECLARATION: &[u8] = b"<!ENTITY";
    let find = |from: usize, needle: &[u8]| {
        data.get(from..)
            .and_then(|rest| rest.windows(needle.len()).position(|w| w == needle))
            .map(|p| p + from)
    };
    let refuse = || unreadable("the SVG declares entities that are too long or made of each other");
    let (mut at, mut declared) = (0, 0usize);
    while let Some(start) = find(at, DECLARATION) {
        declared += 1;
        if declared > MAX_ENTITIES {
            return Err(refuse());
        }
        // the value is between the first pair of quotes that follows the name
        let after = start + DECLARATION.len();
        let open = data
            .get(after..)
            .and_then(|rest| rest.iter().position(|&b| b == b'"' || b == b'\''))
            .map(|p| p + after);
        let Some(open) = open else {
            return Ok(());
        };
        let quote = data[open];
        let close = data
            .get(open + 1..)
            .and_then(|rest| rest.iter().position(|&b| b == quote))
            .map(|p| p + open + 1);
        let Some(close) = close else {
            return Err(refuse());
        };
        let value = &data[open + 1..close];
        if value.len() > MAX_ENTITY_VALUE || value.contains(&b'&') {
            return Err(refuse());
        }
        at = close + 1;
    }
    if declared > 0 && data.iter().filter(|&&b| b == b'&').count() > MAX_REFERENCES {
        return Err(refuse());
    }
    Ok(())
}

/// How deep the elements of an XML text are nested, found by reading its tags and not by parsing it: comments, CDATA,
/// declarations and quoted attribute values are stepped over; what a start tag that ends in `/>` opens is closed at once.
fn nesting(data: &[u8]) -> usize {
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
    let (mut depth, mut deepest, mut at) = (0usize, 0usize, 0usize);
    while at < data.len() {
        if data[at] != b'<' {
            at += 1;
            continue;
        }
        let rest = &data[at..];
        let skip = if rest.starts_with(b"<!--") {
            find(rest, b"-->").map(|p| p + 3)
        } else if rest.starts_with(b"<![CDATA[") {
            find(rest, b"]]>").map(|p| p + 3)
        } else if rest.starts_with(b"<?") || rest.starts_with(b"<!") {
            find(rest, b">").map(|p| p + 1)
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            find(rest, b">").map(|p| p + 1)
        } else {
            // a start tag: it ends at the first `>` that is not inside quotes
            let mut quote = 0u8;
            let end = rest.iter().enumerate().skip(1).find_map(|(i, &b)| {
                if quote != 0 {
                    if b == quote {
                        quote = 0;
                    }
                    None
                } else if b == b'"' || b == b'\'' {
                    quote = b;
                    None
                } else {
                    (b == b'>').then_some(i)
                }
            });
            if let Some(i) = end
                && rest[i - 1] != b'/'
            {
                depth += 1;
                deepest = deepest.max(depth);
            }
            end.map(|i| i + 1)
        };
        at += skip.unwrap_or(rest.len());
    }
    deepest
}

/// Draws the SVG in `path`. The background stays transparent.
///
/// Parsing and drawing run on a thread with a large stack, and a panic of the drawing is an error and not the end of
/// the caller.
pub fn render(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = unpack(read_limited(path, MAX_BYTES)?)?;
    let depth = nesting(&data);
    if depth > MAX_DEPTH {
        return Err(unreadable(format!("the SVG is nested too deeply ({depth} levels)")));
    }
    draw_on_thread("svg", "the SVG could not be drawn", || draw(&data))
}

/// Draws SVG text made by the program itself (a card), at its own size and not enlarged.
pub fn render_exact(markup: &str) -> Result<RgbaImage, DecodeError> {
    draw_at(markup.as_bytes(), false)
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    draw_at(data, true)
}

fn draw_at(data: &[u8], enlarge: bool) -> Result<RgbaImage, DecodeError> {
    draw_with(data, enlarge, fonts(), default_family())
}

/// Lines of text set in the font whose file is `font` (and in no other: only that one is known to the drawing), on a
/// transparent area of `width` x `height` pixels, in the colour `ink` (a CSS colour); each line is (size in pixels, text).
pub fn render_in_font(
    font: Vec<u8>,
    width: u32,
    height: u32,
    ink: &str,
    lines: &[(f32, String)],
) -> Result<RgbaImage, DecodeError> {
    let mut db = usvg::fontdb::Database::new();
    db.load_font_data(font);
    let family = db
        .faces()
        .next()
        .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        .ok_or_else(|| unreadable("the font has no family"))?;
    let mut body = String::new();
    let mut y = 0.0f32;
    for (size, text) in lines {
        y += size * 1.1;
        body += &format!(
            r#"<text x="0" y="{y:.1}" font-size="{size:.1}" fill="{ink}" font-family="{}" xml:space="preserve">{}</text>"#,
            escape(&family),
            escape(text)
        );
        y += size * 0.3;
    }
    let markup = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">{body}</svg>"#);
    draw_with(markup.as_bytes(), false, Arc::new(db), family)
}

fn draw_with(
    data: &[u8],
    enlarge: bool,
    fontdb: Arc<usvg::fontdb::Database>,
    font_family: String,
) -> Result<RgbaImage, DecodeError> {
    check_entities(data)?;
    let options = usvg::Options {
        fontdb,
        font_family,
        // a drawing is no reason to open another file (a path, a share that asks for a login) or to parse another drawing
        // that has no limits: no `<image>` is resolved
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(data, &options).map_err(unreadable)?;
    let size = tree.size();
    if !(size.width() > 0.0 && size.height() > 0.0) {
        return Err(DecodeError::Unreadable("the SVG has no size".to_string()));
    }
    let (width, height) = if enlarge {
        pixel_size(size.width(), size.height())
    } else {
        (
            size.width().round().max(1.0) as u32,
            size.height().round().max(1.0) as u32,
        )
    };
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| unreadable("the SVG has no size"))?;
    let transform = tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied colours, the rest of the program straight ones
    let mut straight = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let p = pixel.demultiply();
        straight.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    RgbaImage::from_raw(width, height, straight).ok_or_else(|| unreadable("the SVG was not drawn"))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn draw(svg: &str) -> Result<RgbaImage, DecodeError> {
        let path = std::env::temp_dir().join(format!("peekel-svg-{}-{:x}.svg", std::process::id(), svg.len()));
        std::fs::write(&path, svg).expect("writes the file");
        let result = render(&path);
        let _ = std::fs::remove_file(&path);
        result
    }

    #[test]
    fn entities_that_expand_into_each_other_are_refused_and_a_plain_one_is_not() {
        let bomb = format!(
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "{}"><!ENTITY b "{}">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>{}</text></svg>"#,
            "x".repeat(1000),
            "&a;".repeat(255),
            "&b;".repeat(1000)
        );
        let started = std::time::Instant::now();
        assert!(draw(&bomb).is_err(), "a bomb of entities is refused");
        assert!(started.elapsed().as_secs() < 5, "and quickly");

        let long = format!(
            r#"<!DOCTYPE svg [<!ENTITY a "{}">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&a;</text></svg>"#,
            "x".repeat(100_000)
        );
        assert!(draw(&long).is_err(), "so is an entity that is very long");

        // what Illustrator wrote for years: entities for namespaces
        let plain = r#"<!DOCTYPE svg [<!ENTITY ns_svg "http://www.w3.org/2000/svg">]><svg xmlns="&ns_svg;" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#;
        let image = draw(plain).expect("a plain entity is fine");
        assert!(image.pixels().any(|p| p[0] > 200 && p[3] > 0));
    }

    #[test]
    fn an_image_in_a_drawing_never_reads_another_file() {
        let inner = std::env::temp_dir().join(format!("peekel-svg-inner-{}.svg", std::process::id()));
        std::fs::write(
            &inner,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##,
        )
        .expect("writes the inner file");
        let href = inner.to_string_lossy().replace('\\', "/");
        let outer = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#
        );
        let image = draw(&outer).expect("the outer drawing is fine");
        let _ = std::fs::remove_file(&inner);
        assert!(image.pixels().all(|p| p[3] == 0), "nothing of the other file is drawn");
    }

    #[test]
    fn a_gzipped_drawing_is_drawn_and_a_gzip_that_unpacks_to_too_much_is_refused() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let gzip = |bytes: &[u8]| {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(bytes).expect("writes");
            encoder.finish().expect("finishes")
        };
        let small = gzip(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="5" height="5"/></svg>"#,
        );
        assert!(unpack(small).is_ok());
        // 100 MB of spaces: a few kilobytes gzipped
        let mut bomb = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#.to_vec();
        bomb.extend(std::iter::repeat_n(b' ', 100 * 1024 * 1024));
        bomb.extend_from_slice(b"</svg>");
        let packed = gzip(&bomb);
        assert!(packed.len() < 4 * 1024 * 1024, "{} bytes", packed.len());
        assert!(matches!(unpack(packed), Err(DecodeError::TooLarge)));
        assert!(matches!(
            unpack(vec![0x1f, 0x8b, 0, 1, 2]),
            Err(DecodeError::Unreadable(_))
        ));
        assert_eq!(unpack(b"<svg/>".to_vec()).expect("plain"), b"<svg/>");
    }

    #[test]
    fn the_nesting_of_tags_is_counted_without_being_fooled() {
        assert_eq!(nesting(b""), 0);
        assert_eq!(nesting(b"<a><b/><c></c></a>"), 2);
        assert_eq!(nesting(b"<a><b><c><d/></c></b></a>"), 3);
        assert_eq!(nesting(b"<?xml version='1.0'?><!DOCTYPE svg><svg><g/></svg>"), 1);
        assert_eq!(nesting(b"<a><!-- <b><b><b> --><c/></a>"), 1);
        assert_eq!(nesting(b"<a><![CDATA[ <b><b> ]]></a>"), 1);
        assert_eq!(nesting(br#"<a title="x > <y>" ><b a='/>'/></a>"#), 1);
        assert_eq!(nesting(b"<a><b>"), 2, "tags left open still count");
        assert_eq!(nesting(b"</a></a><b/>"), 0);
    }

    #[test]
    fn a_drawing_nested_too_deep_is_refused_and_one_nested_a_good_deal_is_drawn() {
        let nested = |levels: usize| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}<rect width="5" height="5"/>{}</svg>"#,
                "<g>".repeat(levels),
                "</g>".repeat(levels)
            )
        };
        assert!(draw(&nested(MAX_DEPTH - 2)).is_ok());
        assert!(matches!(draw(&nested(1000)), Err(DecodeError::Unreadable(m)) if m.contains("nested too deeply")));
    }

    #[test]
    fn a_small_drawing_is_made_large_and_keeps_its_proportions() {
        assert_eq!(pixel_size(24.0, 24.0), (2048, 2048));
        assert_eq!(pixel_size(100.0, 50.0), (2048, 1024));
        assert_eq!(
            pixel_size(3000.0, 1000.0),
            (3000, 1000),
            "between the two limits: its own size"
        );
        assert_eq!(pixel_size(8000.0, 4000.0), (4096, 2048));
        assert_eq!(
            pixel_size(1.0, 10_000.0),
            (1, 4096),
            "a thin one keeps at least a pixel"
        );
    }

    #[test]
    fn shapes_land_where_the_drawing_puts_them_and_the_rest_is_transparent() {
        let img = draw(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20" viewBox="0 0 40 20">
                 <rect x="0" y="0" width="20" height="20" fill="#ff0000"/>
                 <rect x="30" y="10" width="10" height="10" fill="#0000ff" fill-opacity="0.5"/>
               </svg>"##,
        )
        .expect("draws");
        assert_eq!(img.dimensions(), (2048, 1024));
        assert_eq!(img.get_pixel(200, 200).0, [255, 0, 0, 255], "red on the left");
        assert_eq!(img.get_pixel(1200, 200).0[3], 0, "nothing in the middle");
        let blue = img.get_pixel(1900, 800).0;
        assert!(
            blue[0] == 0 && blue[1] == 0 && blue[2] >= 250,
            "a half-clear blue keeps its colour (straight alpha): {blue:?}"
        );
        assert!((126..=129).contains(&blue[3]), "alpha {}", blue[3]);
    }

    #[test]
    fn a_drawing_with_a_view_box_and_no_size_is_drawn() {
        let img = draw(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 5"><circle cx="5" cy="2.5" r="2" /></svg>"#,
        )
        .expect("draws");
        assert_eq!(img.dimensions(), (2048, 1024));
        assert_eq!(
            img.get_pixel(1024, 512).0,
            [0, 0, 0, 255],
            "the circle is black by default"
        );
    }

    #[test]
    fn what_is_no_svg_is_an_error_not_a_panic() {
        assert!(matches!(draw("not xml at all"), Err(DecodeError::Unreadable(_))));
        assert!(matches!(
            draw(r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#),
            Err(DecodeError::Unreadable(_))
        ));
        assert!(matches!(
            render(Path::new("there-is-no-such-file.svg")),
            Err(DecodeError::Unreadable(_))
        ));
    }

    #[test]
    fn the_name_tells_svg() {
        for name in ["a.svg", "A.SVG", "b.svgz", "dir.v2/c.Svg"] {
            assert!(is_svg(Path::new(name)), "{name}");
        }
        for name in ["a.png", "svg", "a.svg.txt", "a"] {
            assert!(!is_svg(Path::new(name)), "{name}");
        }
    }
}
