//! SVG drawn into pixels with `resvg`.
//!
//! A vector picture has no pixel size of its own, and the viewer zooms by shrinking and cropping one raster. So the
//! drawing is made once, large enough to stay sharp at a few steps of zoom, and then handled like any other picture:
//! the longest side is 2048 pixels when the file's own size is smaller, the file's own size up to 4096, and 4096 beyond
//! that (at most 16 megapixels). The viewer keeps the parsed drawing ([`Drawing`]) and draws the part shown again when that
//! picture would be enlarged. A box with no zoom (the quick view of the plugin) gets the drawing made at the size of the box
//! instead ([`render_within`]).
//!
//! A picture inside the SVG (`<image>`) is drawn when it is stored in the file itself (a `data:` URL) and is a PNG, a JPEG, a GIF
//! or a WebP, within the limits below. Not read: another SVG inside an SVG (it would be parsed with none of our limits), and
//! nothing outside the file is ever opened (a path or an address in `<image>` gives nothing). Text is drawn with the fonts
//! installed on the machine; where the font is missing, resvg picks another or draws nothing.

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
/// A picture inside a drawing may have this many pixels (a photograph of 12 megapixels is fine), and all the pictures of one
/// drawing together this many: resvg decodes each one to 4 bytes a pixel when it draws.
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_IMAGES_TOTAL: u64 = 48_000_000;
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

/// The families that stand for "sans serif", "serif" and "monospace", the first that is installed: Windows', Linux's, then Android's
/// (its `/system/fonts`, which the reader gives as a folder of fonts: `fontdb` reads no system fonts there, nor the aliases of its
/// `fonts.xml`, Peekel TV, #235).
const SANS_SERIF: &[&str] = &[
    "Arial",
    "Segoe UI",
    "Liberation Sans",
    "DejaVu Sans",
    "Noto Sans",
    "Helvetica",
    "Ubuntu",
    "Roboto",
    "Droid Sans",
];
const SERIF: &[&str] = &[
    "Times New Roman",
    "Liberation Serif",
    "DejaVu Serif",
    "Noto Serif",
    "Georgia",
    "Droid Serif",
];
const MONOSPACE: &[&str] = &[
    "Courier New",
    "Consolas",
    "Liberation Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Droid Sans Mono",
    "Roboto Mono",
    "Cutive Mono",
];

/// Sets the families that stand for "sans serif", "serif" and "monospace" to the first of [`SANS_SERIF`], [`SERIF`] and [`MONOSPACE`]
/// that is in `db`; one with none of its list is left as `fontdb` has it.
pub(crate) fn set_generic_families(db: &mut usvg::fontdb::Database) {
    if let Some(family) = installed(db, SANS_SERIF) {
        db.set_sans_serif_family(family);
    }
    if let Some(family) = installed(db, SERIF) {
        db.set_serif_family(family);
    }
    if let Some(family) = installed(db, MONOSPACE) {
        db.set_monospace_family(family);
    }
}

/// The installed fonts, read once: it takes a while and every SVG with text needs them.
///
/// The families that stand for "sans serif", "serif" and "monospace" are Arial, Times New Roman and Courier New until
/// said otherwise, and a machine without them (a Linux one, an Android one) would draw no text in them: each is set to the
/// first font of a list that is installed (`set_generic_families`).
pub(crate) fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            // the user's own fonts first, then those the program was given, then the system's (`fontset`)
            for dir in crate::fontset::dirs() {
                db.load_fonts_dir(dir);
            }
            db.load_system_fonts();
            set_generic_families(&mut db);
            Arc::new(db)
        })
        .clone()
}

/// The family `font-family` falls to when an SVG names none: a sans serif one that is installed.
fn default_family() -> String {
    let db = fonts();
    db.family_name(&usvg::fontdb::Family::SansSerif).to_string()
}

/// How large a drawing is made.
#[derive(Clone, Copy)]
enum Size {
    /// Its own size, not enlarged (a card the program made).
    Own,
    /// Large enough to stay sharp at a few steps of zoom ([`pixel_size`]).
    Sharp,
    /// As large as fits in a box of this many pixels, enlarged or made smaller (the box of a quick view, which has no zoom).
    Within(usize, usize),
}

/// The size in pixels a drawing of `width` × `height` units has in a box of `max_width` × `max_height`, its proportions kept; the
/// sides rounded as [`crate::decode::fit`] rounds them, so that the picture fits the box as it is and is not scaled again; the
/// longest side at most `MAX_SIDE`.
fn size_within(width: f32, height: f32, max_width: usize, max_height: usize) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = (max_width as f64 / w)
        .min(max_height as f64 / h)
        .min(f64::from(MAX_SIDE) / w.max(h));
    let side = |units: f64| ((units * scale).round_ties_even() as u32).max(1);
    (side(w), side(h))
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

/// The size in pixels `tree` is drawn at for `size_of`, made smaller if a picture inside it would be enlarged
/// ([`picture_scale_limit`]).
fn drawn_size(tree: &usvg::Tree, size_of: Size) -> (u32, u32) {
    let size = tree.size();
    let (width, height) = match size_of {
        Size::Sharp => pixel_size(size.width(), size.height()),
        Size::Within(max_width, max_height) => size_within(size.width(), size.height(), max_width, max_height),
        Size::Own => {
            return (
                size.width().round().max(1.0) as u32,
                size.height().round().max(1.0) as u32,
            );
        }
    };
    let most = picture_scale_limit(tree);
    if (width as f32 / size.width()).max(height as f32 / size.height()) <= most {
        return (width, height);
    }
    let side = |units: f32| ((units * most).round_ties_even() as u32).max(1);
    (side(size.width()), side(size.height()))
}

/// The largest scale (pixels for a unit of the drawing) at which no picture inside `tree` (a PNG, a JPEG, a GIF, a WebP) that
/// covers a quarter of it or more is enlarged; never less than 1, the drawing's own size, which is what resvg's own program
/// draws by default. A JPEG of 64 x 64 that was most of a drawing of 80 units was blown up with it to a box of 900 px, blurred
/// and blocky; a picture by itself is not enlarged either (`decode::fit`). A small picture in a drawing of shapes (an icon in a
/// diagram) does not count: the shapes stay as large as the box. Infinite when there is no such picture.
fn picture_scale_limit(tree: &usvg::Tree) -> f32 {
    fn visit(group: &usvg::Group, area: f32, limit: &mut f32) {
        for node in group.children() {
            match node {
                usvg::Node::Group(inner) => visit(inner, area, limit),
                usvg::Node::Image(image) => {
                    let covered = image.abs_bounding_box();
                    if !image.is_visible()
                        || matches!(image.kind(), usvg::ImageKind::SVG(_))
                        || covered.width() * covered.height() < area / 4.0
                    {
                        continue;
                    }
                    // the transform takes a pixel of the picture to units of the drawing
                    let (sx, sy) = image.abs_transform().get_scale();
                    let units = sx.max(sy);
                    if units > 0.0 {
                        *limit = limit.min(1.0 / units);
                    }
                }
                _ => {}
            }
        }
    }
    let size = tree.size();
    let mut limit = f32::INFINITY;
    visit(tree.root(), size.width() * size.height(), &mut limit);
    limit.max(1.0)
}

/// The text of an SVG that may be gzipped (`.svgz`): unpacked, within `MAX_UNPACKED`.
fn unpack(data: Vec<u8>) -> Result<Vec<u8>, DecodeError> {
    unpack_within(data, MAX_UNPACKED)
}

/// As [`unpack`], within `limit` bytes.
fn unpack_within(data: Vec<u8>, limit: u64) -> Result<Vec<u8>, DecodeError> {
    use std::io::Read;
    if !data.starts_with(&[0x1f, 0x8b]) {
        return Ok(data);
    }
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(&data[..])
        .take(limit + 1)
        .read_to_end(&mut text)
        .map_err(|_| unreadable("the gzip of the SVG is damaged"))?;
    if text.len() as u64 > limit {
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
    render_sized(path, Size::Sharp)
}

/// Draws the SVG in `path` as large as fits in `max_width` × `max_height` pixels (enlarged or made smaller, its proportions
/// kept): for a box that is shown as it is (the quick view of the plugin), so the drawing is made once at the size shown and
/// not made large and then shrunk.
pub fn render_within(path: &Path, max_width: usize, max_height: usize) -> Result<RgbaImage, DecodeError> {
    if max_width == 0 || max_height == 0 {
        return Err(DecodeError::NoRoom);
    }
    render_sized(path, Size::Within(max_width, max_height))
}

fn render_sized(path: &Path, size: Size) -> Result<RgbaImage, DecodeError> {
    let data = read_checked(path)?;
    draw_on_thread("svg", "the SVG could not be drawn", || {
        draw_with(&data, size, fonts(), default_family())
    })
}

/// The text of the SVG in `path`, unpacked and within the limits that are judged before parsing (size, nesting).
fn read_checked(path: &Path) -> Result<Vec<u8>, DecodeError> {
    let data = unpack(read_limited(path, MAX_BYTES)?)?;
    let depth = nesting(&data);
    if depth > MAX_DEPTH {
        return Err(unreadable(format!("the SVG is nested too deeply ({depth} levels)")));
    }
    Ok(data)
}

/// An SVG parsed once and kept, to be drawn again at any zoom: the viewer draws it at about 2048 px as a picture ([`Drawing::render`])
/// and, when that picture would be enlarged on the screen, draws the part of it the window shows at the scale of the window
/// ([`Drawing::render_part`]), so that it stays sharp.
pub struct Drawing {
    tree: usvg::Tree,
    skipped: Skipped,
}

/// What a drawing holds that was not drawn, counted while it was parsed (each `<image>` that gave nothing). The rest of what resvg
/// leaves out (an element it does not support, a font that is not there) is not counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// Pictures and drawings given by a path or an address: never opened.
    pub linked: usize,
    /// Drawings stored inside it refused by the limits, or stored inside a drawing inside (one level only).
    pub drawings: usize,
    /// Pictures stored inside it over the limits, or of a kind that is not read (not a PNG, a JPEG, a GIF or a WebP).
    pub pictures: usize,
}

impl Skipped {
    /// Whether everything was drawn.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What was not drawn, one part for each kind, short: "1 linked file", "2 drawings inside", "1 picture".
    pub fn parts(&self) -> Vec<String> {
        let count =
            |n: usize, one: &str, many: &str| (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }));
        [
            count(self.linked, "linked file", "linked files"),
            count(self.drawings, "drawing inside", "drawings inside"),
            count(self.pictures, "picture", "pictures"),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// One line for the screen, when something was not drawn: "not drawn: 1 linked file, 1 picture".
    pub fn note(&self) -> Option<String> {
        (!self.is_empty()).then(|| format!("not drawn: {}", self.parts().join(", ")))
    }
}

impl std::fmt::Debug for Drawing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let size = self.tree.size();
        write!(f, "Drawing({} x {})", size.width(), size.height())
    }
}

impl Drawing {
    /// Reads and parses the SVG in `path`, with the same limits as [`render`].
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let data = read_checked(path)?;
        draw_on_thread("svg", "the SVG could not be read", || {
            parse(&data, fonts(), default_family()).map(|(tree, skipped)| Self { tree, skipped })
        })
    }

    /// What the drawing holds that was not drawn.
    pub fn skipped(&self) -> Skipped {
        self.skipped
    }

    /// The drawing as large as fits in `max_width` × `max_height` pixels, as [`render_within`] makes it.
    pub fn render_within(&self, max_width: usize, max_height: usize) -> Result<RgbaImage, DecodeError> {
        if max_width == 0 || max_height == 0 {
            return Err(DecodeError::NoRoom);
        }
        let size = self.tree.size();
        let (width, height) = drawn_size(&self.tree, Size::Within(max_width, max_height));
        draw_on_thread("svg", "the SVG could not be drawn", || {
            draw_tree(
                &self.tree,
                (width, height),
                tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height()),
            )
        })
    }

    /// The drawing at about 2048 px, as [`render`] makes it.
    pub fn render(&self) -> Result<RgbaImage, DecodeError> {
        let size = self.tree.size();
        let (width, height) = drawn_size(&self.tree, Size::Sharp);
        draw_on_thread("svg", "the SVG could not be drawn", || {
            draw_tree(
                &self.tree,
                (width, height),
                tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height()),
            )
        })
    }

    /// As [`Drawing::render`], in `count` bands (1: at once, as before the bands), for comparing the two (`core/examples/svg_bands.rs`).
    #[doc(hidden)]
    pub fn render_in(&self, count: usize) -> Result<RgbaImage, DecodeError> {
        let size = self.tree.size();
        let (width, height) = drawn_size(&self.tree, Size::Sharp);
        let transform = tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
        draw_on_thread("svg", "the SVG could not be drawn", || {
            draw_in_bands(&self.tree, (width, height), transform, count)
        })
    }

    /// Whether the drawing has a filter (and is drawn at once, not in bands).
    #[doc(hidden)]
    pub fn has_filters(&self) -> bool {
        has_filters(self.tree.root())
    }

    /// The part of the drawing that the window shows, at the scale of the window: `picture` is the size of the picture [`Drawing::render`]
    /// made, `(x, y)` the corner of the part in its pixels, `scale` the pixels of the window for each of its pixels, `out` the size of
    /// the window's part in pixels. The same place and size as that picture cut and enlarged, but drawn from the shapes.
    pub fn render_part(
        &self,
        picture: (u32, u32),
        corner: (f64, f64),
        scale: f64,
        out: (u32, u32),
    ) -> Result<RgbaImage, DecodeError> {
        let size = self.tree.size();
        if out.0 == 0 || out.1 == 0 || u64::from(out.0) * u64::from(out.1) > MAX_PART_PIXELS {
            return Err(DecodeError::TooLarge);
        }
        // a unit of the drawing is this many pixels of the picture, each of them `scale` pixels of the window
        let sx = f64::from(picture.0) / f64::from(size.width()) * scale;
        let sy = f64::from(picture.1) / f64::from(size.height()) * scale;
        let transform = tiny_skia::Transform::from_row(
            sx as f32,
            0.0,
            0.0,
            sy as f32,
            (-corner.0 * scale) as f32,
            (-corner.1 * scale) as f32,
        );
        draw_on_thread("svg", "the SVG could not be drawn", || {
            draw_tree(&self.tree, out, transform)
        })
    }
}

/// The most pixels a part drawn for the window may have (a window of 8K is 33 million).
const MAX_PART_PIXELS: u64 = 40_000_000;

/// Draws SVG text made by the program itself (a card), at its own size and not enlarged.
pub fn render_exact(markup: &str) -> Result<RgbaImage, DecodeError> {
    draw_with(markup.as_bytes(), Size::Own, fonts(), default_family())
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
    draw_with(markup.as_bytes(), Size::Own, Arc::new(db), family)
}

/// What the pictures and drawings stored in one drawing may take, all of them together.
struct Budget {
    /// Pixels of the pictures, up to `MAX_IMAGES_TOTAL`.
    pixels: AtomicU64,
    /// Bytes of the drawings inside it, unpacked, up to `MAX_UNPACKED` (as much as one file may unpack to).
    text: AtomicU64,
    /// The levels of tags a drawing inside may have: what is left of `MAX_DEPTH` after the outer drawing's own, since it is parsed
    /// while the outer one is (on the same stack).
    depth: usize,
    /// What was not drawn ([`Skipped`]).
    linked: AtomicUsize,
    drawings: AtomicUsize,
    pictures: AtomicUsize,
}

/// The resolver of the pictures that a drawing stores in itself (`data:` URLs): a PNG, a JPEG, a GIF or a WebP, told by its own
/// first bytes and not by the name the drawing gives it, not above the pixel limits (read from the header, before anything is
/// decoded). Anything else is taken for a drawing when `drawings` is true ([`inner_drawing`]); otherwise it gives nothing.
fn embedded_pictures(budget: Arc<Budget>, drawings: bool) -> usvg::ImageHrefDataResolverFn<'static> {
    Box::new(move |_mime, data, options| {
        let reader = image::ImageReader::new(std::io::Cursor::new(&data[..]))
            .with_guessed_format()
            .ok()?;
        let Some(format) = reader.format() else {
            // not a picture: a drawing, or nothing that is read
            let drawing = looks_like_drawing(&data);
            let found = if drawing && drawings {
                inner_drawing(&data, options, &budget)
            } else {
                None
            };
            if found.is_none() {
                let counter = if drawing { &budget.drawings } else { &budget.pictures };
                counter.fetch_add(1, Ordering::Relaxed);
            }
            return found;
        };
        let found = reader.into_dimensions().ok().and_then(|(width, height)| {
            let pixels = u64::from(width) * u64::from(height);
            if pixels > MAX_IMAGE_PIXELS
                || budget.pixels.fetch_add(pixels, Ordering::Relaxed) + pixels > MAX_IMAGES_TOTAL
            {
                return None;
            }
            match format {
                image::ImageFormat::Png => Some(usvg::ImageKind::PNG(data)),
                image::ImageFormat::Jpeg => Some(usvg::ImageKind::JPEG(data)),
                image::ImageFormat::Gif => Some(usvg::ImageKind::GIF(data)),
                image::ImageFormat::WebP => Some(usvg::ImageKind::WEBP(data)),
                _ => None,
            }
        });
        if found.is_none() {
            budget.pictures.fetch_add(1, Ordering::Relaxed);
        }
        found
    })
}

/// Whether bytes stored in a drawing are a drawing: gzipped, or the tag `<svg` near the start.
fn looks_like_drawing(data: &[u8]) -> bool {
    data.starts_with(&[0x1f, 0x8b]) || data[..data.len().min(4096)].windows(4).any(|w| w == b"<svg")
}

/// The resolver of the pictures and drawings a drawing names by a path or an address: nothing is opened, each one is counted.
fn linked(budget: &Arc<Budget>) -> usvg::ImageHrefStringResolverFn<'static> {
    let budget = Arc::clone(budget);
    Box::new(move |_, _| {
        budget.linked.fetch_add(1, Ordering::Relaxed);
        None
    })
}

/// A drawing stored in a drawing (`<image>` with an SVG in a `data:` URL, gzipped or not), parsed with the limits of a file:
/// unpacked within what is left of the budget, its tags nested no deeper than the budget's depth, its entities checked; its own
/// pictures resolved, but not a drawing inside it (one level only), and never a path or an address.
fn inner_drawing(data: &[u8], options: &usvg::Options, budget: &Arc<Budget>) -> Option<usvg::ImageKind> {
    let left = MAX_UNPACKED.saturating_sub(budget.text.load(Ordering::Relaxed));
    let text = unpack_within(data.to_vec(), left).ok()?;
    let length = text.len() as u64;
    if budget.text.fetch_add(length, Ordering::Relaxed) + length > MAX_UNPACKED
        || nesting(&text) > budget.depth
        || check_entities(&text).is_err()
    {
        return None;
    }
    let inner = usvg::Options {
        fontdb: Arc::clone(&options.fontdb),
        font_family: options.font_family.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: embedded_pictures(Arc::clone(budget), false),
            resolve_string: linked(budget),
        },
        ..usvg::Options::default()
    };
    usvg::Tree::from_data(&text, &inner).ok().map(usvg::ImageKind::SVG)
}

/// The parsed drawing, with the entities checked first and only what is stored in it resolved; and what was not.
fn parse(
    data: &[u8],
    fontdb: Arc<usvg::fontdb::Database>,
    font_family: String,
) -> Result<(usvg::Tree, Skipped), DecodeError> {
    check_entities(data)?;
    let budget = Arc::new(Budget {
        pixels: AtomicU64::new(0),
        text: AtomicU64::new(0),
        depth: MAX_DEPTH.saturating_sub(nesting(data)),
        linked: AtomicUsize::new(0),
        drawings: AtomicUsize::new(0),
        pictures: AtomicUsize::new(0),
    });
    let options = usvg::Options {
        fontdb,
        font_family,
        // a drawing is no reason to open another file (a path, a share that asks for a login): only the pictures and the
        // drawings stored in the file are resolved, each within the limits
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: embedded_pictures(Arc::clone(&budget), true),
            resolve_string: linked(&budget),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(data, &options).map_err(unreadable)?;
    let size = tree.size();
    if !(size.width() > 0.0 && size.height() > 0.0) {
        return Err(DecodeError::Unreadable("the SVG has no size".to_string()));
    }
    let skipped = Skipped {
        linked: budget.linked.load(Ordering::Relaxed),
        drawings: budget.drawings.load(Ordering::Relaxed),
        pictures: budget.pictures.load(Ordering::Relaxed),
    };
    Ok((tree, skipped))
}

fn draw_with(
    data: &[u8],
    size_of: Size,
    fontdb: Arc<usvg::fontdb::Database>,
    font_family: String,
) -> Result<RgbaImage, DecodeError> {
    let (tree, _) = parse(data, fontdb, font_family)?;
    let size = tree.size();
    let (width, height) = drawn_size(&tree, size_of);
    let transform = tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    draw_tree(&tree, (width, height), transform)
}

/// `tree` drawn with `transform` on a clear area of `size` pixels.
///
/// A drawing of many rows is drawn in bands, one per core, at the same time ([`bands`]): each band is the whole drawing moved up and
/// cut by the band's edges, so the pixels are those of one drawing. Not a drawing with filters: a filter works on the area its
/// element covers within what is drawn, so a blur cut at a band's edge would leave a seam.
fn draw_tree(tree: &usvg::Tree, size: (u32, u32), transform: tiny_skia::Transform) -> Result<RgbaImage, DecodeError> {
    let count = if has_filters(tree.root()) {
        1
    } else {
        bands(size.1, std::thread::available_parallelism().map_or(1, usize::from))
    };
    draw_in_bands(tree, size, transform, count)
}

/// `tree` drawn in `count` bands at the same time (1: at once).
fn draw_in_bands(
    tree: &usvg::Tree,
    size: (u32, u32),
    transform: tiny_skia::Transform,
    count: usize,
) -> Result<RgbaImage, DecodeError> {
    let (width, height) = size;
    if count <= 1 {
        return draw_band(tree, width, (0, height), transform);
    }
    let step = height.div_ceil(count as u32);
    let parts: Vec<(u32, u32)> = (0..count as u32)
        .map(|i| (i * step, ((i + 1) * step).min(height)))
        .filter(|(top, bottom)| top < bottom)
        .collect();
    let drawn: Vec<Result<RgbaImage, DecodeError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = parts
            .iter()
            .map(|&rows| {
                std::thread::Builder::new()
                    .name("svg band".to_string())
                    .stack_size(crate::guard::STACK)
                    .spawn_scoped(scope, move || draw_band(tree, width, rows, transform))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| match handle {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|_| Err(unreadable("the SVG could not be drawn"))),
                Err(e) => Err(unreadable(e)),
            })
            .collect()
    });
    let mut whole = Vec::with_capacity(width as usize * height as usize * 4);
    for band in drawn {
        whole.extend_from_slice(band?.as_raw());
    }
    RgbaImage::from_raw(width, height, whole).ok_or_else(|| unreadable("the SVG was not drawn"))
}

/// How many bands a drawing of `height` rows is drawn in with `cores` cores: one per core, each at least `MIN_BAND` rows.
fn bands(height: u32, cores: usize) -> usize {
    cores.min((height / MIN_BAND) as usize).max(1)
}

/// The fewest rows a band has: below that the work of going through the drawing once more costs more than it saves.
const MIN_BAND: u32 = 128;

/// Whether `group` or anything in it (its masks and clipping paths too) has a filter.
fn has_filters(group: &usvg::Group) -> bool {
    !group.filters().is_empty()
        || group.mask().is_some_and(|mask| has_filters(mask.root()))
        || group.clip_path().is_some_and(|clip| has_filters(clip.root()))
        || group.children().iter().any(|node| match node {
            usvg::Node::Group(inner) => has_filters(inner),
            _ => false,
        })
}

/// The rows a band is drawn beyond each of its edges and then dropped: where a shape is cut by the edge of the area drawn its edge
/// pixels come out a little otherwise (measured: the pixels that differ from one drawing gathered in the rows by a band's edge), so
/// the cut is put outside the rows kept.
const BAND_MARGIN: u32 = 32;

/// The rows `top..bottom` of `tree` drawn with `transform` in an area `width` pixels wide, in straight colours.
fn draw_band(
    tree: &usvg::Tree,
    width: u32,
    (top, bottom): (u32, u32),
    transform: tiny_skia::Transform,
) -> Result<RgbaImage, DecodeError> {
    // the whole drawing has no margin outside it: the first band starts at its top edge, as one drawing does; the last band is
    // drawn past the bottom, which is clear
    let from = top.saturating_sub(BAND_MARGIN);
    let height = bottom + BAND_MARGIN - from;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| unreadable("the SVG has no size"))?;
    resvg::render(
        tree,
        transform.post_translate(0.0, -(from as f32)),
        &mut pixmap.as_mut(),
    );
    // the rows kept; tiny-skia keeps premultiplied colours, the rest of the program straight ones
    let row = width as usize;
    let kept = &pixmap.pixels()[(top - from) as usize * row..(bottom - from) as usize * row];
    let mut straight = Vec::with_capacity(kept.len() * 4);
    for pixel in kept {
        let p = pixel.demultiply();
        straight.extend_from_slice(&[p.red(), p.green(), p.blue(), p.alpha()]);
    }
    RgbaImage::from_raw(width, bottom - top, straight).ok_or_else(|| unreadable("the SVG was not drawn"))
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
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5) * crate::slack(),
            "and quickly"
        );

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

    /// `bytes` as base64 (the drawing stores its pictures so).
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut text = String::new();
        for chunk in bytes.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= chunk.len() {
                    text.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
                } else {
                    text.push('=');
                }
            }
        }
        text
    }

    /// A PNG of one colour, `width` x `height`.
    fn png(width: u32, height: u32, colour: [u8; 4]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(width, height, image::Rgba(colour))
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encodes");
        out.into_inner()
    }

    /// A drawing of 10 x 10 with one `<image>` that has this `mime` and these bytes.
    fn with_picture(mime: &str, bytes: &[u8]) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="data:{mime};base64,{}"/></svg>"#,
            base64(bytes)
        )
    }

    #[test]
    fn a_picture_stored_in_the_drawing_is_drawn_whatever_name_the_drawing_gives_it() {
        let red = png(4, 4, [255, 0, 0, 255]);
        for mime in ["image/png", "image/jpeg", "text/plain"] {
            let image = render_exact(&with_picture(mime, &red)).expect("drawn");
            let [r, g, b, a] = image.get_pixel(image.width() / 2, image.height() / 2).0;
            assert!(r > 240 && g < 15 && b < 15 && a == 255, "{mime}: {r} {g} {b} {a}");
        }
    }

    #[test]
    fn a_drawing_inside_a_drawing_is_drawn_one_level_deep_and_one_made_to_hurt_is_not() {
        let red = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;
        let is_red = |picture: &RgbaImage| picture.pixels().all(|p| p[0] > 200 && p[1] < 50 && p[3] > 200);
        let is_clear = |picture: &RgbaImage| picture.pixels().all(|p| p[3] == 0);
        for mime in ["image/svg+xml", "text/plain"] {
            let nested = draw(&with_picture(mime, red)).expect("draws");
            assert!(is_red(&nested), "a drawing inside, named {mime}, is drawn");
        }
        let gzip = |bytes: &[u8]| {
            use std::io::Write;
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(bytes).expect("writes");
            encoder.finish().expect("finishes")
        };
        assert!(
            is_red(&draw(&with_picture("image/svg+xml", &gzip(red))).expect("draws")),
            "gzipped too"
        );
        // its own pictures are drawn, but not a drawing inside it
        let picture_inside = with_picture("image/png", &png(4, 4, [255, 0, 0, 255]));
        assert!(is_red(
            &draw(&with_picture("image/svg+xml", picture_inside.as_bytes())).expect("draws")
        ));
        let two_levels = with_picture("image/svg+xml", red);
        assert!(
            is_clear(&draw(&with_picture("image/svg+xml", two_levels.as_bytes())).expect("the outer drawing is fine")),
            "one level only"
        );
        // made to hurt: entities expanding into each other, a gzip that unpacks to more than a file may, tags nested deeper than
        // the stack allows; each one refused quickly and the outer drawing drawn
        let bomb = format!(
            r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "{}"><!ENTITY b "{}">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>{}</text></svg>"#,
            "x".repeat(1000),
            "&a;".repeat(255),
            "&b;".repeat(1000)
        );
        let mut spaces = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#.to_vec();
        spaces.extend(std::iter::repeat_n(b' ', 100 * 1024 * 1024));
        spaces.extend_from_slice(b"</svg>");
        let deep = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}<rect width="10" height="10" fill="red"/>{}</svg>"#,
            "<g>".repeat(MAX_DEPTH),
            "</g>".repeat(MAX_DEPTH)
        );
        for (what, inner) in [
            ("entities", bomb.into_bytes()),
            ("gzip", gzip(&spaces)),
            ("depth", deep.into_bytes()),
        ] {
            let started = std::time::Instant::now();
            let outer = draw(&with_picture("image/svg+xml", &inner)).expect("the outer drawing is fine");
            assert!(is_clear(&outer), "{what}: not drawn");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(10) * crate::slack(),
                "{what}: quickly"
            );
        }
    }

    #[test]
    fn what_is_not_drawn_is_counted_and_said() {
        let skipped = |svg: &str| parse(svg.as_bytes(), fonts(), default_family()).expect("parses").1;
        let red = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"##;
        assert!(skipped(&with_picture("image/svg+xml", red)).is_empty(), "all drawn");
        assert_eq!(skipped(&with_picture("image/svg+xml", red)).note(), None);
        let linked = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="5" height="5" xlink:href="picture.png"/><image width="5" height="5" xlink:href="https://example.com/a.svg"/></svg>"#;
        assert_eq!(
            skipped(linked),
            Skipped {
                linked: 2,
                ..Skipped::default()
            }
        );
        let two_levels = with_picture("image/svg+xml", red);
        let deeper = skipped(&with_picture("image/svg+xml", two_levels.as_bytes()));
        assert_eq!(deeper.drawings, 1, "a drawing inside a drawing inside");
        let garbage = skipped(&with_picture("image/png", b"not a picture at all"));
        assert_eq!(garbage.pictures, 1, "bytes that are no picture");
        let both = Skipped {
            linked: 1,
            drawings: 0,
            pictures: 3,
        };
        assert_eq!(both.note().as_deref(), Some("not drawn: 1 linked file, 3 pictures"));
        // the drawing opened keeps it
        let path = std::env::temp_dir().join(format!("peekel-svg-linked-{}.svg", std::process::id()));
        std::fs::write(&path, linked).expect("writes the file");
        let opened = Drawing::open(&path).map(|d| d.skipped());
        let _ = std::fs::remove_file(&path);
        assert_eq!(opened.expect("opens").linked, 2);
    }

    #[test]
    fn a_picture_over_the_limit_is_not_drawn() {
        // a PNG whose header says 5000 x 5000, 25 megapixels, over the limit of one picture though the file is small (the
        // header of a one pixel PNG with the size and its checksum changed: the limit is judged on the header alone)
        let mut big = png(1, 1, [0, 0, 255, 255]);
        big[16..20].copy_from_slice(&5000u32.to_be_bytes());
        big[20..24].copy_from_slice(&5000u32.to_be_bytes());
        let mut crc = flate2::Crc::new();
        crc.update(&big[12..29]);
        big[29..33].copy_from_slice(&crc.sum().to_be_bytes());
        assert_eq!(
            image::ImageReader::new(std::io::Cursor::new(&big))
                .with_guessed_format()
                .expect("format")
                .into_dimensions()
                .expect("size"),
            (5000, 5000)
        );
        let refused = draw(&with_picture("image/png", &big)).expect("the outer drawing is fine");
        assert!(
            refused.pixels().all(|p| p[3] == 0),
            "a picture over the limit is not decoded"
        );
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
    fn a_drawing_for_a_box_fills_it_as_the_shrunk_large_one_did() {
        assert_eq!(size_within(24.0, 24.0, 300, 200), (200, 200), "a small one is enlarged");
        assert_eq!(size_within(40.0, 20.0, 300, 300), (300, 150));
        assert_eq!(
            size_within(3000.0, 1000.0, 600, 400),
            (600, 200),
            "a large one is made smaller"
        );
        assert_eq!(
            size_within(1.0, 10_000.0, 500, 500),
            (1, 500),
            "a thin one keeps at least a pixel"
        );
        assert_eq!(
            size_within(10.0, 10.0, 9000, 9000),
            (4096, 4096),
            "never over the largest side"
        );
        // the same size as the large drawing shrunk into the box, so the box shows it as it is
        for (w, h, bw, bh) in [(40.0, 20.0, 333, 211), (7.0, 3.0, 641, 517), (100.0, 61.0, 1279, 683)] {
            let (lw, lh) = pixel_size(w, h);
            let large = RgbaImage::new(lw, lh);
            let shrunk = crate::decode::fit(large, bw, bh);
            assert_eq!(
                size_within(w, h, bw, bh),
                shrunk.dimensions(),
                "{w} x {h} in {bw} x {bh}"
            );
        }
    }

    /// A drawing of 80 units with a PNG of `pixels` x `pixels` placed at (8, 8) over `units` x `units`, and a frame of shapes.
    fn drawing_with_picture(pixels: u32, units: u32) -> String {
        use base64::Engine as _;
        let mut png = Vec::new();
        RgbaImage::from_pixel(pixels, pixels, image::Rgba([200, 120, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encodes the PNG");
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 80 80"><image x="8" y="8" width="{units}" height="{units}" href="data:image/png;base64,{}"/><rect x="1" y="1" width="78" height="78" fill="none" stroke="black"/></svg>"#,
            base64::engine::general_purpose::STANDARD.encode(&png)
        )
    }

    fn drawn_for(svg: &str, size_of: Size) -> (u32, u32) {
        let (tree, _) = parse(svg.as_bytes(), fonts(), default_family()).expect("parses");
        drawn_size(&tree, size_of)
    }

    #[test]
    fn a_picture_that_is_most_of_a_drawing_is_not_enlarged_and_a_small_one_leaves_the_drawing_filling_the_box() {
        // a JPEG of 64 x 64 over 64 of 80 units (resvg's test files): the drawing's own size, not the box of 700
        let most = drawing_with_picture(64, 64);
        assert_eq!(drawn_for(&most, Size::Within(900, 700)), (80, 80));
        assert_eq!(drawn_for(&most, Size::Sharp), (80, 80), "the viewer's picture too");
        assert_eq!(
            drawn_for(&most, Size::Within(50, 50)),
            (50, 50),
            "made smaller as before"
        );
        assert_eq!(
            render_within_markup(&most, 900, 700).dimensions(),
            (80, 80),
            "and drawn at that size"
        );
        // an icon of 8 units in a drawing of 80 does not count: the drawing fills the box
        assert_eq!(
            drawn_for(&drawing_with_picture(16, 8), Size::Within(900, 700)),
            (700, 700)
        );
        // a large picture: the drawing grows until the picture is at its own pixels (512 px over 64 of 80 units: 640 px)
        assert_eq!(
            drawn_for(&drawing_with_picture(512, 64), Size::Within(900, 700)),
            (640, 640)
        );
        assert_eq!(
            drawn_for(&drawing_with_picture(512, 64), Size::Within(500, 500)),
            (500, 500)
        );
        // a picture already enlarged in the drawing itself: the drawing keeps its own size, not less
        assert_eq!(
            drawn_for(&drawing_with_picture(8, 64), Size::Within(900, 700)),
            (80, 80)
        );
        // shapes only: as before
        let shapes = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 80 80"><rect x="8" y="8" width="64" height="64"/></svg>"#;
        assert_eq!(drawn_for(shapes, Size::Within(900, 700)), (700, 700));
        assert_eq!(drawn_for(shapes, Size::Sharp), (2048, 2048));
    }

    fn render_within_markup(svg: &str, max_width: usize, max_height: usize) -> RgbaImage {
        draw_with(
            svg.as_bytes(),
            Size::Within(max_width, max_height),
            fonts(),
            default_family(),
        )
        .expect("draws")
    }

    #[test]
    fn a_drawing_made_for_its_box_looks_as_the_large_one_shrunk() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30" viewBox="0 0 40 30">
             <rect x="2" y="2" width="16" height="12" fill="#ff0000"/>
             <circle cx="28" cy="18" r="9" fill="#0000ff" fill-opacity="0.6" stroke="#000" stroke-width="0.5"/>
             <path d="M2 28 L38 4" stroke="#00a000" stroke-width="1.5"/>
           </svg>"##;
        let path = std::env::temp_dir().join(format!("peekel-svg-box-{}.svg", std::process::id()));
        std::fs::write(&path, svg).expect("writes the file");
        let before = crate::decode::fit(render(&path).expect("draws"), 400, 300);
        let now = render_within(&path, 400, 300).expect("draws");
        assert!(matches!(render_within(&path, 0, 300), Err(DecodeError::NoRoom)));
        let _ = std::fs::remove_file(&path);
        assert_eq!(now.dimensions(), before.dimensions());
        // only the edges of the shapes may differ (smoothed once and not twice: the edge of the red square, on a pixel's
        // border, is sharp now and was a pixel wide before), and by little; compared as they are seen, the colour weighed by
        // alpha (a clear pixel's colour is not seen)
        let seen = |p: &image::Rgba<u8>| {
            let a = u32::from(p[3]);
            [0, 1, 2].map(|i| u32::from(p[i]) * a / 255).into_iter().chain([a])
        };
        let (mut total, mut far) = (0u64, 0usize);
        for (a, b) in now.pixels().zip(before.pixels()) {
            let d = seen(a).zip(seen(b)).map(|(x, y)| x.abs_diff(y)).max().unwrap_or(0);
            total += u64::from(d);
            if d > 64 {
                far += 1;
            }
        }
        let mean = total as f64 / f64::from(now.width() * now.height());
        // measured: a mean of 1.0 of 255 and no pixel far off
        assert!(mean < 2.0, "mean difference {mean}");
        assert!(far < 400, "{far} pixels differ much");
    }

    /// The largest difference of two pictures of one size, compared as seen (the colour weighed by alpha), and the mean.
    fn difference(a: &RgbaImage, b: &RgbaImage) -> (u32, f64) {
        assert_eq!(a.dimensions(), b.dimensions());
        let seen = |p: &image::Rgba<u8>| {
            let alpha = u32::from(p[3]);
            [0, 1, 2]
                .map(|i| u32::from(p[i]) * alpha / 255)
                .into_iter()
                .chain([alpha])
        };
        let (mut largest, mut total) = (0u32, 0u64);
        for (x, y) in a.pixels().zip(b.pixels()) {
            let d = seen(x).zip(seen(y)).map(|(p, q)| p.abs_diff(q)).max().unwrap_or(0);
            largest = largest.max(d);
            total += u64::from(d);
        }
        (largest, total as f64 / f64::from(a.width() * a.height()))
    }

    #[test]
    fn a_part_drawn_for_the_window_is_the_part_of_the_picture_drawn_larger() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30" viewBox="0 0 40 30">
             <rect x="2.3" y="2.1" width="16" height="12" fill="#ff0000"/>
             <circle cx="28" cy="18" r="9" fill="#0000ff" fill-opacity="0.6" stroke="#000" stroke-width="0.5"/>
             <path d="M2 28 L38 4" stroke="#00a000" stroke-width="0.3"/>
           </svg>"##;
        let path = std::env::temp_dir().join(format!("peekel-svg-part-{}.svg", std::process::id()));
        std::fs::write(&path, svg).expect("writes the file");
        let drawing = Drawing::open(&path).expect("parses");
        let _ = std::fs::remove_file(&path);
        let whole = drawing.render().expect("draws");
        assert_eq!(whole.dimensions(), (2048, 1536), "as `render` makes it");
        let size = whole.dimensions();
        // at a scale of 1, a part is the same pixels as the picture cut
        let part = drawing
            .render_part(size, (300.0, 200.0), 1.0, (500, 400))
            .expect("draws");
        let cut = image::imageops::crop_imm(&whole, 300, 200, 500, 400).to_image();
        let (largest, mean) = difference(&part, &cut);
        assert!(largest <= 2 && mean < 0.01, "scale 1: largest {largest}, mean {mean}");
        // at a scale of 2, the same part of a drawing made twice as large: the shapes, not the pixels enlarged
        let tree = &drawing.tree;
        let large = draw_tree(
            tree,
            (4096, 3072),
            tiny_skia::Transform::from_scale(4096.0 / 40.0, 3072.0 / 30.0),
        )
        .expect("draws");
        let part = drawing
            .render_part(size, (300.0, 200.0), 2.0, (500, 400))
            .expect("draws");
        let cut = image::imageops::crop_imm(&large, 600, 400, 500, 400).to_image();
        let (largest, mean) = difference(&part, &cut);
        assert!(largest <= 2 && mean < 0.01, "scale 2: largest {largest}, mean {mean}");
        // a part too large for any window is refused, not made
        assert!(matches!(
            drawing.render_part(size, (0.0, 0.0), 8.0, (10_000, 10_000)),
            Err(DecodeError::TooLarge)
        ));
    }

    #[test]
    fn a_drawing_in_bands_is_the_drawing_at_once_but_for_a_trace_and_one_with_a_filter_is_drawn_at_once() {
        assert_eq!(bands(100, 16), 1, "too few rows for two bands");
        assert_eq!(bands(2048, 16), 16);
        assert_eq!(bands(1024, 16), 8, "each band at least MIN_BAND rows");
        assert_eq!(bands(2048, 1), 1, "one core");
        // many shapes with edges, curves and half-clear fills crossing the bands' edges
        let mut shapes = String::new();
        for i in 0..60 {
            let (x, y) = ((i * 37) % 200, (i * 53) % 150);
            shapes += &format!(
                r##"<path d="M{x} {y} Q{} {} {} {} T{} {} Z" fill="#{:06x}" fill-opacity="0.6" stroke="#000" stroke-width="0.4"/>"##,
                x + 30,
                y - 20,
                x + 50,
                y + 25,
                x + 10,
                y + 40,
                (i * 0x0a3f17) % 0xffffff
            );
        }
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="150" viewBox="0 0 200 150">{shapes}</svg>"#
        );
        let path = std::env::temp_dir().join(format!("peekel-svg-bands-{}.svg", std::process::id()));
        std::fs::write(&path, &svg).expect("writes the file");
        // drawn at 600 x 450 (a test build draws slowly), in 3 bands of 150 rows
        let size = (600, 450);
        let transform = tiny_skia::Transform::from_scale(3.0, 3.0);
        let drawing = Drawing::open(&path).expect("parses");
        let once = draw_in_bands(&drawing.tree, size, transform, 1).expect("draws");
        let banded = draw_in_bands(&drawing.tree, size, transform, 3).expect("draws");
        let (largest, mean) = difference(&once, &banded);
        // measured: at most 30 of 255 in a pixel at the edge of a shape, a mean of 0.01; no seam
        assert!(largest <= 64 && mean < 0.1, "largest {largest}, mean {mean}");
        // with a blur the drawing is not cut into bands: the same pixels as at once
        let blurred = svg.replace(
            "<path ",
            r#"<filter id="b"><feGaussianBlur stdDeviation="2"/></filter><path filter="url(#b)" "#,
        );
        std::fs::write(&path, blurred).expect("writes the file");
        let drawing = Drawing::open(&path).expect("parses");
        let _ = std::fs::remove_file(&path);
        assert!(drawing.has_filters());
        assert_eq!(
            draw_tree(&drawing.tree, size, transform).expect("draws"),
            draw_in_bands(&drawing.tree, size, transform, 1).expect("draws")
        );
    }

    #[test]
    fn a_drawing_opened_is_refused_as_a_drawing_rendered_is() {
        let path = std::env::temp_dir().join(format!("peekel-svg-open-{}.svg", std::process::id()));
        let deep = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">{}<rect width="5" height="5"/>{}</svg>"#,
            "<g>".repeat(1000),
            "</g>".repeat(1000)
        );
        std::fs::write(&path, deep).expect("writes the file");
        assert!(matches!(Drawing::open(&path), Err(DecodeError::Unreadable(m)) if m.contains("nested too deeply")));
        let bomb = format!(
            r#"<!DOCTYPE svg [<!ENTITY a "{}"><!ENTITY b "{}">]><svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>{}</text></svg>"#,
            "x".repeat(1000),
            "&a;".repeat(255),
            "&b;".repeat(1000)
        );
        std::fs::write(&path, bomb).expect("writes the file");
        assert!(Drawing::open(&path).is_err());
        std::fs::write(&path, "not xml").expect("writes the file");
        assert!(Drawing::open(&path).is_err());
        let _ = std::fs::remove_file(&path);
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
