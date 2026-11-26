//! A page that a document format lays out (words, boxes, lines, pictures, in points from the top left) and that is drawn
//! into pixels here, so that every document format (docx, pptx, xlsx) has only its own reading and laying out to do.
//!
//! The text is drawn by `resvg` as SVG text with the installed fonts; the pictures are laid over afterwards (resvg here
//! reads no raster pictures). Words are measured by a rough table of widths, not by the font: enough for a preview, where
//! a line a little longer or shorter than in Word does not show.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use image::{Rgba, RgbaImage, imageops};

use crate::decode::DecodeError;
use crate::svg;
use crate::xml::escape;

/// A picture on a page: its pixels, or the bytes of its file kept packed and decoded when the page is drawn (a book's, whose layout
/// is kept in memory: a textbook's pictures took 396 MB once decoded, Peekel TV, #235).
#[derive(Debug, Clone)]
pub enum Picture {
    Pixels(Arc<RgbaImage>),
    Packed(Arc<Packed>),
}

/// The file of a picture, and the size it is drawn from: its own once turned as it says, made no larger than the `shrink` it was
/// packed with on its longest side.
#[derive(Debug)]
pub struct Packed {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    max_pixels: u64,
    /// Names it among the pictures decoded last (`DECODED`); an address could be met again by another picture.
    id: u64,
}

/// The pictures decoded last for a drawing, by their `Packed::id`, the newest first, as long as they weigh no more than
/// `DECODED_BYTES` together: a page drawn again (a spread, a step of zoom) does not decode them again.
static DECODED: Mutex<Vec<(u64, Arc<RgbaImage>)>> = Mutex::new(Vec::new());
const DECODED_BYTES: usize = 48 * 1024 * 1024;

impl Packed {
    /// The picture in `bytes`, read as far as its size and how it is turned (its pixels are not decoded); drawn no larger than
    /// `shrink` on its longest side. `None` when it is not a picture that can be read or has more than `max_pixels`.
    pub fn new(bytes: Vec<u8>, max_pixels: u64, shrink: u32) -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let (w, h) = crate::decode::picture_size(&bytes, max_pixels).ok()?;
        let (width, height) = shrunk((w, h), shrink);
        Some(Self {
            bytes,
            width,
            height,
            max_pixels,
            id: NEXT.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Its pixels at its size, decoded now or kept from a drawing before; `None` when the file turns out to be spoilt past its header.
    pub fn pixels(&self) -> Option<Arc<RgbaImage>> {
        if let Ok(mut decoded) = DECODED.lock()
            && let Some(index) = decoded.iter().position(|(id, _)| *id == self.id)
        {
            let entry = decoded.remove(index);
            let image = Arc::clone(&entry.1);
            decoded.insert(0, entry);
            return Some(image);
        }
        let image = crate::decode::decode_bytes(&self.bytes, self.max_pixels).ok()?;
        let image = if image.dimensions() == (self.width, self.height) {
            image
        } else {
            crate::resize::triangle(&image, self.width, self.height)
        };
        let image = Arc::new(image);
        if let Ok(mut decoded) = DECODED.lock() {
            decoded.insert(0, (self.id, Arc::clone(&image)));
            let mut total = 0;
            decoded.retain(|(_, i)| {
                total += i.as_raw().len();
                total <= DECODED_BYTES
            });
        }
        Some(image)
    }
}

/// Lets go of the pictures kept decoded for drawing (`Packed::pixels`).
pub fn forget_decoded() {
    if let Ok(mut decoded) = DECODED.lock() {
        decoded.clear();
    }
}

/// `(w, h)` made no larger than `side` on its longest side, its proportions kept, at least a pixel each way.
pub(crate) fn shrunk((w, h): (u32, u32), side: u32) -> (u32, u32) {
    if w.max(h) <= side {
        return (w, h);
    }
    let scale = f64::from(side) / f64::from(w.max(h));
    (
        ((f64::from(w) * scale).round() as u32).max(1),
        ((f64::from(h) * scale).round() as u32).max(1),
    )
}

impl Picture {
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Pixels(image) => image.dimensions(),
            Self::Packed(packed) => packed.dimensions(),
        }
    }

    /// Its pixels (decoded now when it is packed); `None` when they cannot be read.
    pub fn pixels(&self) -> Option<Arc<RgbaImage>> {
        match self {
            Self::Pixels(image) => Some(Arc::clone(image)),
            Self::Packed(packed) => packed.pixels(),
        }
    }

    /// The bytes it holds: its pixels, or its file.
    pub fn weight(&self) -> usize {
        match self {
            Self::Pixels(image) => image.as_raw().len(),
            Self::Packed(packed) => packed.bytes.len(),
        }
    }

    /// Where it lies, to count a picture met on several pages once.
    fn address(&self) -> usize {
        match self {
            Self::Pixels(image) => Arc::as_ptr(image) as usize,
            Self::Packed(packed) => Arc::as_ptr(packed) as usize,
        }
    }
}

impl From<Arc<RgbaImage>> for Picture {
    fn from(image: Arc<RgbaImage>) -> Self {
        Self::Pixels(image)
    }
}

/// The longest side of the picture of a page, in pixels.
const SIDE: f32 = 2048.0;
/// The longest side of a page drawn for a box ([`Page::render_within`]): a column of a 4K screen (1920 x 2160) fits.
const MAX_SIDE: f32 = 4096.0;

/// A colour as red, green, blue.
pub type Rgb = [u8; 3];

/// How a piece of text looks.
#[derive(Clone, PartialEq, Debug)]
pub struct Font {
    /// Size in points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Struck through.
    pub strike: bool,
    pub colour: Rgb,
    /// The family the document asks for; the fallbacks are added when drawing.
    pub family: Option<String>,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            colour: [0, 0, 0],
            family: None,
        }
    }
}

#[derive(Debug, Clone)]
enum Item {
    /// A line of text: pieces in their own fonts, one after the other, set by the renderer with the real widths.
    Text {
        x: f32,
        baseline: f32,
        runs: Vec<(String, Font)>,
    },
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        fill: Option<Rgb>,
        stroke: Option<(Rgb, f32)>,
    },
    Ellipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        fill: Option<Rgb>,
        stroke: Option<(Rgb, f32)>,
    },
    Line {
        from: (f32, f32),
        to: (f32, f32),
        colour: Rgb,
        width: f32,
    },
    Picture {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        image: Picture,
    },
}

/// What is on one page, in points.
#[derive(Debug, Clone)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    items: Vec<Item>,
}

impl Page {
    /// About how many bytes the page holds: its items and their text, and the pixels of its pictures that are not in `seen` yet (a
    /// picture shared by several pages, or met before, is counted once). For the layouts kept in memory (`book`).
    pub(crate) fn weight(&self, seen: &mut std::collections::HashSet<usize>) -> usize {
        self.items
            .iter()
            .map(|item| {
                std::mem::size_of::<Item>()
                    + match item {
                        Item::Text { runs, .. } => runs
                            .iter()
                            .map(|(text, font)| {
                                std::mem::size_of::<(String, Font)>()
                                    + text.len()
                                    + font.family.as_ref().map_or(0, String::len)
                            })
                            .sum(),
                        Item::Picture { image, .. } if seen.insert(image.address()) => image.weight(),
                        _ => 0,
                    }
            })
            .sum()
    }
}

/// The width of `text` set in `font`, roughly: a table of widths by kind of letter, in units of the size.
pub fn text_width(text: &str, font: &Font) -> f32 {
    let units: f32 =
        text.chars()
            .map(|c| match c {
                ' ' => 0.28,
                'i' | 'j' | 'l' | 't' | 'f' | 'I' | '.' | ',' | ';' | ':' | '\'' | '|' | '!' | '(' | ')' | '['
                | ']' => 0.29,
                'm' | 'w' | 'M' | 'W' | '@' | '%' => 0.86,
                'A'..='Z' => 0.66,
                '0'..='9' => 0.55,
                c if c as u32 >= 0x2E80 => 1.0,                      // CJK
                c if c as u32 >= 0x400 && c as u32 <= 0x4FF => 0.56, // Cyrillic
                _ => 0.5,
            })
            .sum();
    units * font.size * if font.bold { 1.07 } else { 1.0 }
}

fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

/// A family name, or a list of them (`Georgia, Times New Roman, serif`), made safe to put in an attribute (quotes and markup out), with
/// the fallbacks.
pub(crate) fn family(name: Option<&str>) -> String {
    let clean: String = name
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == ',')
        .collect();
    let clean = clean.trim();
    // each family followed by the free font of its width, for a machine that has not the original (`fontset`)
    if clean.is_empty() {
        crate::fontset::expand("Segoe UI, Arial, sans-serif")
    } else {
        crate::fontset::expand(&format!("{clean}, Segoe UI, Arial, sans-serif"))
    }
}

impl Page {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            items: Vec::new(),
        }
    }

    /// A piece of text whose baseline starts at (`x`, `baseline`).
    pub fn text(&mut self, x: f32, baseline: f32, text: &str, font: &Font) {
        self.runs(x, baseline, vec![(text.to_string(), font.clone())]);
    }

    /// A line whose baseline starts at (`x`, `baseline`) made of pieces, each in its own font.
    pub fn runs(&mut self, x: f32, baseline: f32, runs: Vec<(String, Font)>) {
        if runs.iter().any(|(text, _)| !text.trim().is_empty()) {
            self.items.push(Item::Text { x, baseline, runs });
        }
    }

    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, fill: Option<Rgb>, stroke: Option<(Rgb, f32)>) {
        self.items.push(Item::Rect {
            x,
            y,
            width,
            height,
            fill,
            stroke,
        });
    }

    /// How many boxes filled with `colour` the page has (for tests).
    #[cfg(test)]
    pub(crate) fn filled(&self, colour: Rgb) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Rect { fill: Some(c), .. } if *c == colour))
            .count()
    }

    /// How many lines of `width` the page has (for tests).
    #[cfg(test)]
    pub(crate) fn lines_of(&self, width: f32) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, Item::Line { width: w, .. } if *w == width))
            .count()
    }

    /// Where the last item put on the page is in its list (to grow a box drawn under what follows it, `grow_rect`).
    pub(crate) fn last_item(&self) -> Option<usize> {
        self.items.len().checked_sub(1)
    }

    /// The box `index` made to reach down to `bottom`, when it is a box whose foot is `foot` (one backdrop for a block of lines, not a
    /// box a line, whose edges would leave hairlines of the page between them); whether it was.
    pub(crate) fn grow_rect(&mut self, index: usize, foot: f32, bottom: f32) -> bool {
        match self.items.get_mut(index) {
            Some(Item::Rect { y, height, .. }) if (*y + *height - foot).abs() < 0.01 && bottom >= foot => {
                *height = bottom - *y;
                true
            }
            _ => false,
        }
    }

    /// An ellipse inside this box.
    pub fn ellipse(&mut self, x: f32, y: f32, width: f32, height: f32, fill: Option<Rgb>, stroke: Option<(Rgb, f32)>) {
        self.items.push(Item::Ellipse {
            x,
            y,
            width,
            height,
            fill,
            stroke,
        });
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), colour: Rgb, width: f32) {
        self.items.push(Item::Line {
            from,
            to,
            colour,
            width,
        });
    }

    /// A picture laid over the page in this box.
    pub fn picture(&mut self, x: f32, y: f32, width: f32, height: f32, image: impl Into<Picture>) {
        self.items.push(Item::Picture {
            x,
            y,
            width,
            height,
            image: image.into(),
        });
    }

    /// Whether anything is on the page.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The page on white, its longest side `SIDE` pixels.
    pub fn render(&self) -> Result<RgbaImage, DecodeError> {
        let (width, height) = (self.width.max(1.0), self.height.max(1.0));
        self.render_scaled(SIDE / width.max(height))
    }

    /// The page on white at the size at which it fits the box `max_width` x `max_height` pixels: smaller than [`Page::render`] for
    /// a small box, larger for a large one (text and lines are drawn anew at any size; the pictures in it are scaled), its longest
    /// side within 1 and `MAX_SIDE` pixels.
    pub fn render_within(&self, max_width: usize, max_height: usize) -> Result<RgbaImage, DecodeError> {
        let (width, height) = (self.width.max(1.0), self.height.max(1.0));
        let fit = (max_width.max(1) as f32 / width).min(max_height.max(1) as f32 / height);
        let longest = width.max(height);
        self.render_scaled(fit.clamp(1.0 / longest, MAX_SIDE / longest))
    }

    /// The page on white, `scale` pixels to a point.
    fn render_scaled(&self, scale: f32) -> Result<RgbaImage, DecodeError> {
        let (width, height) = (self.width.max(1.0), self.height.max(1.0));
        let (px_w, px_h) = ((width * scale).round().max(1.0), (height * scale).round().max(1.0));
        let mut body = String::new();
        for item in &self.items {
            match item {
                Item::Text { x, baseline, runs } => {
                    body += &format!(r#"<text x="{x:.2}" y="{baseline:.2}" xml:space="preserve">"#);
                    for (text, font) in runs {
                        body += &format!(
                            r#"<tspan font-size="{:.2}" fill="{}" font-family="{}"{}{}{}>{}</tspan>"#,
                            font.size,
                            hex(font.colour),
                            family(font.family.as_deref()),
                            if font.bold { r#" font-weight="bold""# } else { "" },
                            if font.italic { r#" font-style="italic""# } else { "" },
                            match (font.underline, font.strike) {
                                (true, true) => r#" text-decoration="underline line-through""#,
                                (true, false) => r#" text-decoration="underline""#,
                                (false, true) => r#" text-decoration="line-through""#,
                                (false, false) => "",
                            },
                            escape(text)
                        );
                    }
                    body += "</text>";
                }
                Item::Rect {
                    x,
                    y,
                    width,
                    height,
                    fill,
                    stroke,
                } => {
                    let fill = fill.map_or("none".to_string(), hex);
                    let stroke = stroke.map_or(String::new(), |(c, w)| {
                        format!(r#" stroke="{}" stroke-width="{w:.2}""#, hex(c))
                    });
                    body += &format!(
                        r#"<rect x="{x:.2}" y="{y:.2}" width="{:.2}" height="{:.2}" fill="{fill}"{stroke}/>"#,
                        width.max(0.0),
                        height.max(0.0)
                    );
                }
                Item::Ellipse {
                    x,
                    y,
                    width,
                    height,
                    fill,
                    stroke,
                } => {
                    let fill = fill.map_or("none".to_string(), hex);
                    let stroke = stroke.map_or(String::new(), |(c, w)| {
                        format!(r#" stroke="{}" stroke-width="{w:.2}""#, hex(c))
                    });
                    body += &format!(
                        r#"<ellipse cx="{:.2}" cy="{:.2}" rx="{:.2}" ry="{:.2}" fill="{fill}"{stroke}/>"#,
                        x + width / 2.0,
                        y + height / 2.0,
                        (width / 2.0).max(0.0),
                        (height / 2.0).max(0.0)
                    );
                }
                Item::Line {
                    from,
                    to,
                    colour,
                    width,
                } => {
                    body += &format!(
                        r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width:.2}"/>"#,
                        from.0,
                        from.1,
                        to.0,
                        to.1,
                        hex(*colour)
                    );
                }
                Item::Picture { .. } => {}
            }
        }
        let markup = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{px_w}" height="{px_h}" viewBox="0 0 {width:.2} {height:.2}"><rect width="100%" height="100%" fill="#ffffff"/>{body}</svg>"##
        );
        let mut canvas = svg::render_exact(&markup)?;
        for item in &self.items {
            if let Item::Picture {
                x,
                y,
                width,
                height,
                image,
            } = item
            {
                let (w, h) = ((width * scale).round().max(1.0), (height * scale).round().max(1.0));
                if w > 8192.0 || h > 8192.0 {
                    continue;
                }
                // a packed picture spoilt past its header is left out
                let Some(image) = image.pixels() else {
                    continue;
                };
                let scaled = crate::resize::triangle(&image, w as u32, h as u32);
                // what is clear in the picture lets the white of the page through
                let (ox, oy) = ((x * scale).round() as i64, (y * scale).round() as i64);
                imageops::overlay(&mut canvas, &scaled, ox, oy);
            }
        }
        // the page is opaque
        for p in canvas.pixels_mut() {
            *p = Rgba([p.0[0], p.0[1], p.0[2], 255]);
        }
        Ok(canvas)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn words_are_measured_roughly_and_bold_is_wider() {
        let plain = Font::default();
        let bold = Font {
            bold: true,
            ..Font::default()
        };
        assert!(text_width("iiii", &plain) < text_width("MMMM", &plain));
        assert!(text_width("word", &bold) > text_width("word", &plain));
        assert_eq!(text_width("", &plain), 0.0);
    }

    #[test]
    fn a_page_is_drawn_with_its_box_text_and_picture() {
        let mut page = Page::new(200.0, 100.0);
        assert!(page.is_empty());
        page.rect(10.0, 10.0, 60.0, 30.0, Some([255, 0, 0]), None);
        page.text(
            80.0,
            40.0,
            "Hi <&> \"there\"",
            &Font {
                size: 18.0,
                ..Font::default()
            },
        );
        page.picture(
            150.0,
            60.0,
            30.0,
            30.0,
            Arc::new(RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]))),
        );
        let img = page.render().expect("renders");
        assert_eq!(img.dimensions(), (2048, 1024));
        assert_eq!(img.get_pixel(1000, 100).0, [255, 255, 255, 255]);
        let [r, g, b, _] = img.get_pixel(400, 200).0;
        assert!(r > 240 && g < 20 && b < 20, "red box: {r} {g} {b}");
        let [r, g, b, _] = img.get_pixel(1600, 750).0;
        assert!(b > 240 && r < 20 && g < 20, "picture: {r} {g} {b}");
        let dark = (800..1600)
            .flat_map(|x| (250..420).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[0] < 80)
            .count();
        assert!(dark > 1000, "{dark} dark pixels of text");
    }

    fn picture_file(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("images")
                .join(name),
        )
        .expect("a picture of the tests")
    }

    #[test]
    fn a_packed_picture_has_the_size_it_decodes_to_turned_as_it_says() {
        for n in 1..=8 {
            let bytes = picture_file(&format!("exif-{n}.jpg"));
            let decoded = crate::decode::decode_bytes(&bytes, u64::MAX).expect("decodes");
            let packed = Packed::new(bytes, u64::MAX, 100_000).expect("packed");
            assert_eq!(packed.dimensions(), decoded.dimensions(), "exif-{n}");
            assert_eq!(packed.pixels().expect("decoded").as_raw(), decoded.as_raw(), "exif-{n}");
        }
        // made no larger than it was packed with, its proportions kept
        let bytes = picture_file("growth.png");
        let (w, h) = crate::decode::decode_bytes(&bytes, u64::MAX)
            .expect("decodes")
            .dimensions();
        let small = Packed::new(bytes.clone(), u64::MAX, 50).expect("packed");
        assert_eq!(small.dimensions(), shrunk((w, h), 50));
        assert!(small.dimensions().0.max(small.dimensions().1) == 50);
        assert_eq!(small.pixels().expect("decoded").dimensions(), small.dimensions());
        // more pixels than allowed: not packed
        assert!(Packed::new(bytes, u64::from(w) * u64::from(h) - 1, 50).is_none());
        assert!(Packed::new(b"not a picture".to_vec(), u64::MAX, 50).is_none());
    }

    #[test]
    fn a_page_draws_a_packed_picture_as_its_pixels_and_leaves_out_one_spoilt_past_its_header() {
        let bytes = picture_file("growth.png");
        let pixels = Arc::new(crate::decode::decode_bytes(&bytes, u64::MAX).expect("decodes"));
        let draw = |picture: Picture| {
            let mut page = Page::new(200.0, 100.0);
            page.picture(20.0, 10.0, 120.0, 80.0, picture);
            page.render().expect("renders")
        };
        let packed = Picture::Packed(Arc::new(Packed::new(bytes.clone(), u64::MAX, 100_000).expect("packed")));
        assert!(packed.weight() < Picture::Pixels(Arc::clone(&pixels)).weight());
        let from_pixels = draw(Picture::Pixels(pixels));
        assert_eq!(draw(packed.clone()).as_raw(), from_pixels.as_raw());
        // drawn again: the same, from the pixels kept
        assert_eq!(draw(packed).as_raw(), from_pixels.as_raw());

        // the header whole, the rest cut: packed, then left out of the drawing, which is white
        let cut = bytes[..bytes.len() / 2].to_vec();
        let spoilt = Packed::new(cut, u64::MAX, 100_000).expect("its header is read");
        assert!(spoilt.pixels().is_none());
        let page = draw(Picture::Packed(Arc::new(spoilt)));
        assert!(page.pixels().all(|p| p.0 == [255, 255, 255, 255]));
    }
}
