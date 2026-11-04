//! The first slide of an OpenDocument presentation (odp, LibreOffice Impress) as a picture: a preview, not a rendering that matches
//! Impress.
//!
//! An odp is a ZIP of XML (`content.xml`, `styles.xml`). Read here: the size of the slide (from the page layout of its master page),
//! the background (the slide's own, then the master page's), the shapes of the slide and the shapes of its master page that are not
//! placeholders (a logo, a bar), with their fill and line (rectangles, ellipses, custom shapes drawn as the box or the ellipse they sit
//! in, lines), text boxes and text in shapes (paragraphs, lists as bullets, spans with their size, bold, italic, underline and colour,
//! alignment, spacing, where the text stands in its shape) and pictures that are PNG, JPEG, GIF, BMP, WebP or TIFF.
//!
//! Not done: slides after the first, notes, transitions and animations, gradients, hatches and picture fills, the shape of a custom
//! shape other than a box or an ellipse, rotation and other transforms, tables and charts, the text of a master placeholder, fields
//! (a date, a page number: their text is shown as saved), text that shrinks to fit its box, shadows.
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser has no DTDs and has limits (`xml`),
//! the shapes, the paragraphs and the depth of groups are counted, and only one slide is read.

use std::cell::Cell;
use std::path::Path;

use image::RgbaImage;
use roxmltree::{Document, Node};

use crate::archive::Archive;
use crate::decode::DecodeError;
use crate::flow::Align as FlowAlign;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::odt::{Props, Size, Styles, length};
use crate::office::{MAX_BYTES, MAX_PART, Pictures, is_encrypted};
use crate::page::{Font, Page};
use crate::text::{self, Align, Para};
use crate::xml::{attr, child, children, descendant, parse};

/// A slide of 28 cm by 15.75 cm (Impress's 16:9), in points, for a file that says nothing.
const DEFAULT_SIZE: (f32, f32) = (793.7, 446.4);
/// The size of text that says nothing.
const DEFAULT_TEXT: f32 = 18.0;
/// The space between the edge of a shape and its text.
const PADDING: f32 = 7.0;
/// The indent of one level of a list.
const LIST_STEP: f32 = 20.0;
/// Shapes looked at in all, paragraphs of one shape, and how deep groups go.
const MAX_SHAPES: usize = 5_000;
const MAX_PARAS: usize = 500;
const MAX_GROUPS: usize = 8;
/// The runs of one paragraph that are kept.
const MAX_RUNS: usize = 2_000;

/// Whether the name says odp.
pub fn is_odp(path: &Path) -> bool {
    has_extension(path, "odp")
}

/// Draws the first slide of the file in `path` on its background; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("odp", "the presentation could not be drawn", || draw(&data))
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    if is_encrypted(&archive) {
        return Err(unreadable("the document is protected with a password"));
    }
    let content = archive.read("content.xml", MAX_PART).map_err(|e| match e {
        DecodeError::Unreadable(_) => unreadable("this is no OpenDocument presentation (no content.xml)"),
        other => other,
    })?;
    let content_text = String::from_utf8_lossy(&content);
    let content_xml = parse(&content_text)?;
    let styles_text = archive.text("styles.xml", MAX_PART);
    let styles_xml = styles_text.as_deref().and_then(|t| parse(t).ok());

    let root = content_xml.root_element();
    let slide = child(root, "body")
        .and_then(|b| child(b, "presentation"))
        .and_then(|p| children(p, "page").next())
        .ok_or_else(|| unreadable("the document has no slide"))?;

    let mut styles = Styles::default();
    if let Some(xml) = &styles_xml {
        styles.read(xml.root_element());
    }
    styles.read(root);
    let master = styles_xml
        .as_ref()
        .and_then(|x| master_page(x, attr(slide, "master-page-name")));
    let (width, height) = styles_xml
        .as_ref()
        .and_then(|x| slide_size(x, master))
        .unwrap_or(DEFAULT_SIZE);

    let deck = Deck {
        styles,
        pictures: Pictures::new(&archive),
        shapes: Cell::new(0),
    };
    let mut page = Page::new(width, height);
    // the background: the slide's own, else the master page's
    let own = deck.styles.resolve("drawing-page", attr(slide, "style-name")).fill;
    let from_master = master
        .and_then(|m| attr(m, "style-name"))
        .and_then(|n| deck.styles.resolve("drawing-page", Some(n)).fill);
    page.rect(
        0.0,
        0.0,
        width,
        height,
        Some(own.or(from_master).unwrap_or([255, 255, 255])),
        None,
    );
    if let Some(master) = master {
        // the master page's own shapes behind (the placeholders are not: the slide has its own text)
        for shape in master.children().filter(|c| c.is_element()) {
            if attr(shape, "class").is_none() {
                deck.shape(shape, &mut page, 0);
            }
        }
    }
    for shape in slide.children().filter(|c| c.is_element()) {
        deck.shape(shape, &mut page, 0);
    }
    page.render()
}

/// The master page of that name in `styles.xml`.
fn master_page<'a, 'i>(styles: &'a Document<'i>, name: Option<&str>) -> Option<Node<'a, 'i>> {
    let name = name?;
    styles
        .root_element()
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "master-page" && attr(*n, "name") == Some(name))
}

/// The size of the slide: the page layout that the master page names (else the first), in points.
fn slide_size(styles: &Document<'_>, master: Option<Node<'_, '_>>) -> Option<(f32, f32)> {
    let wanted = master.and_then(|m| attr(m, "page-layout-name"));
    let layout = styles
        .root_element()
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "page-layout")
        .find(|n| wanted.is_none() || attr(*n, "name") == wanted)?;
    let properties = descendant(layout, "page-layout-properties")?;
    let width = length(attr(properties, "page-width"))?;
    let height = length(attr(properties, "page-height"))?;
    (width >= 72.0 && height >= 72.0).then_some((width.min(4000.0), height.min(4000.0)))
}

struct Deck<'a> {
    styles: Styles,
    pictures: Pictures<'a>,
    /// How many shapes have been looked at.
    shapes: Cell<usize>,
}

/// The look of a shape: the styles it names (`draw:style-name`, `presentation:style-name`), the presentation one over the graphic one.
fn shape_props(deck: &Deck<'_>, node: Node<'_, '_>) -> Props {
    let mut props = Props::default();
    for family in ["graphic", "presentation"] {
        for name in node
            .attributes()
            .filter(|a| a.name() == "style-name")
            .map(|a| a.value())
        {
            props.over(&deck.styles.resolve(family, Some(name)));
        }
    }
    props
}

/// The two lengths of `translate (1cm 2cm)` in a transform.
fn translation(transform: &str) -> Option<(f32, f32)> {
    let inside = transform.split_once("translate")?.1;
    let inside = inside.trim_start().strip_prefix('(')?;
    let (inside, _) = inside.split_once(')')?;
    let mut parts = inside.split_whitespace();
    Some((length(parts.next())?, length(parts.next())?))
}

fn points(node: Node<'_, '_>, name: &str) -> Option<f32> {
    length(attr(node, name))
}

impl Deck<'_> {
    /// One shape of the slide, and what it holds; `depth` is how deep in groups this is.
    fn shape(&self, node: Node<'_, '_>, page: &mut Page, depth: usize) {
        let seen = self.shapes.get() + 1;
        self.shapes.set(seen);
        if seen > MAX_SHAPES {
            return;
        }
        let name = node.tag_name().name();
        if name == "g" {
            if depth < MAX_GROUPS {
                for item in node.children().filter(|c| c.is_element()) {
                    self.shape(item, page, depth + 1);
                }
            }
            return;
        }
        if matches!(name, "line" | "connector") {
            let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                points(node, "x1"),
                points(node, "y1"),
                points(node, "x2"),
                points(node, "y2"),
            ) else {
                return;
            };
            let props = shape_props(self, node);
            page.line((x1, y1), (x2, y2), props.stroke.unwrap_or([0, 0, 0]), 1.0);
            return;
        }
        if !matches!(name, "frame" | "custom-shape" | "rect" | "ellipse" | "circle") {
            return;
        }
        // a shape that is turned has no x and y: they stand in `translate (x y)` of its transform (the turn itself is not made)
        let moved = attr(node, "transform").and_then(translation);
        let (Some(x), Some(y), Some(w), Some(h)) = (
            points(node, "x").or(moved.map(|m| m.0)),
            points(node, "y").or(moved.map(|m| m.1)),
            points(node, "width"),
            points(node, "height"),
        ) else {
            return;
        };
        if w <= 0.0 || h <= 0.0 || w > 20_000.0 || h > 20_000.0 {
            return;
        }
        let props = shape_props(self, node);
        let round = matches!(name, "ellipse" | "circle")
            || descendant(node, "enhanced-geometry").is_some_and(|g| attr(g, "type") == Some("ellipse"));
        if props.fill.is_some() || props.stroke.is_some() {
            let stroke = props
                .stroke
                .map(|c| (c, props.stroke_width.unwrap_or(1.0).clamp(0.5, 20.0)));
            if round {
                page.ellipse(x, y, w, h, props.fill, stroke);
            } else {
                page.rect(x, y, w, h, props.fill, stroke);
            }
        }
        if name == "frame" {
            if let Some(image) = descendant(node, "image") {
                let found = attr(image, "href")
                    .map(|h| h.trim_start_matches("./"))
                    .and_then(|path| self.pictures.get(path));
                if let Some(found) = found {
                    page.picture(x, y, w, h, found);
                }
            }
            if let Some(text_box) = child(node, "text-box") {
                self.text(text_box, (x, y, w, h), &props, page);
            }
        } else {
            self.text(node, (x, y, w, h), &props, page);
        }
    }

    /// The paragraphs under `container`, laid out in the box. Where the text stands (top, middle, bottom) and how it is aligned when its
    /// own style says nothing are the shape's (`draw:textarea-vertical-align`, `draw:textarea-horizontal-align`), else the top and the left.
    fn text(&self, container: Node<'_, '_>, area: (f32, f32, f32, f32), props: &Props, page: &mut Page) {
        let (x, y, w, h) = area;
        let mut paras = Vec::new();
        self.paras(container, 0, props, &mut paras);
        if paras.is_empty() {
            return;
        }
        let width = (w - 2.0 * PADDING).max(10.0);
        let used = text::measure(&paras, width);
        let room = h - 2.0 * PADDING;
        let start = y
            + PADDING
            + match props.valign.unwrap_or(0) {
                1 => ((room - used) / 2.0).max(0.0),
                2 => (room - used).max(0.0),
                _ => 0.0,
            };
        text::layout(&paras, x + PADDING, start, width, page.height, page);
    }

    fn paras(&self, container: Node<'_, '_>, depth: usize, base: &Props, out: &mut Vec<Para>) {
        for item in container.children().filter(|c| c.is_element()) {
            if out.len() >= MAX_PARAS {
                return;
            }
            match item.tag_name().name() {
                "p" | "h" => out.push(self.para(item, depth, base)),
                "list" if depth < MAX_GROUPS => {
                    for entry in children(item, "list-item").chain(children(item, "list-header")) {
                        self.paras(entry, depth + 1, base, out);
                    }
                }
                _ => {}
            }
        }
    }

    /// One paragraph. Its look is the default style's, then the style of its shape (a placeholder's size and colour live there), then its own.
    fn para(&self, node: Node<'_, '_>, depth: usize, base: &Props) -> Para {
        let mut props = self.styles.resolve("paragraph", None);
        props.over(base);
        props.over(&self.styles.chain("paragraph", attr(node, "style-name")));
        let mut runs = Vec::new();
        self.runs(node, &props, &mut runs);
        // white space at the ends of a paragraph is not shown
        if let Some(first) = runs.first_mut() {
            first.0 = first.0.trim_start().to_string();
        }
        if let Some(last) = runs.last_mut() {
            last.0 = last.0.trim_end().to_string();
        }
        runs.retain(|(text, _)| !text.is_empty());
        let font = font_of(&props);
        let bullet = depth > 0;
        Para {
            runs,
            align: match props.align {
                Some(FlowAlign::Center) => Align::Center,
                Some(FlowAlign::Right) => Align::Right,
                _ => Align::Left,
            },
            left: props.left.unwrap_or(0.0).clamp(0.0, 2000.0) + if bullet { LIST_STEP * depth as f32 } else { 0.0 },
            first: props.first.unwrap_or(0.0).clamp(-2000.0, 2000.0),
            before: props.before.unwrap_or(0.0).clamp(0.0, 200.0),
            after: props.after.unwrap_or(0.0).clamp(0.0, 200.0),
            line: props.line.unwrap_or(1.0),
            bullet: bullet.then(|| font.clone()),
            bullet_offset: bullet.then_some(-12.0),
            bullet_char: None,
            empty_size: font.size,
        }
    }

    /// The pieces of text under `node`, each in the font its spans give.
    fn runs(&self, node: Node<'_, '_>, base: &Props, out: &mut Vec<(String, Font)>) {
        for item in node.children() {
            if out.len() >= MAX_RUNS {
                return;
            }
            if item.is_text() {
                out.push((collapse(item.text().unwrap_or("")), font_of(base)));
                continue;
            }
            if !item.is_element() {
                continue;
            }
            match item.tag_name().name() {
                "s" => {
                    let count = attr(item, "c")
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 50);
                    out.push((" ".repeat(count), font_of(base)));
                }
                "tab" => out.push((" ".to_string(), font_of(base))),
                "line-break" => out.push(("\n".to_string(), font_of(base))),
                "note" | "annotation" | "annotation-end" | "tracked-changes" | "frame" | "bookmark"
                | "bookmark-start" | "bookmark-end" | "soft-page-break" => {}
                "span" | "a" => {
                    let mut props = base.clone();
                    props.over(&self.styles.resolve("text", attr(item, "style-name")));
                    self.runs(item, &props, out);
                }
                // the fields (a date, a page number) hold the text they show
                _ => self.runs(item, base, out),
            }
        }
    }
}

/// White space of the XML (a line break and the indent after it) is one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

/// The font of a style, in the size of a slide when the style says none.
fn font_of(props: &Props) -> Font {
    let mut font = props.font();
    font.size = match props.size {
        Some(Size::Points(s)) => s,
        Some(Size::Part(p)) => DEFAULT_TEXT * p,
        None => DEFAULT_TEXT,
    }
    .clamp(1.0, 400.0);
    font
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:xlink="http://www.w3.org/1999/xlink""#;

    fn odp(slide: &str, automatic: &str) -> Vec<u8> {
        let content = format!(
            r#"<?xml version="1.0"?><office:document-content {NS}><office:automatic-styles>{automatic}</office:automatic-styles><office:body><office:presentation><draw:page draw:name="p1" draw:master-page-name="M" draw:style-name="dp1">{slide}</draw:page></office:presentation></office:body></office:document-content>"#
        );
        let styles = format!(
            r#"<?xml version="1.0"?><office:document-styles {NS}><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="28cm" fo:page-height="15.75cm"/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name="M" style:page-layout-name="pm1"/></office:master-styles></office:document-styles>"#
        );
        zip(&[
            ("mimetype", b"application/vnd.oasis.opendocument.presentation", false),
            ("content.xml", content.as_bytes(), true),
            ("styles.xml", styles.as_bytes(), true),
        ])
    }

    #[test]
    fn a_slide_with_a_background_a_shape_and_text_is_drawn() {
        let data = odp(
            r#"<draw:frame svg:x="2cm" svg:y="1cm" svg:width="20cm" svg:height="3cm"><draw:text-box><text:p text:style-name="T">A title</text:p></draw:text-box></draw:frame><draw:rect draw:style-name="gr1" svg:x="2cm" svg:y="6cm" svg:width="8cm" svg:height="4cm"><text:p>In a box</text:p></draw:rect>"#,
            r##"<style:style style:name="dp1" style:family="drawing-page"><style:drawing-page-properties draw:fill="solid" draw:fill-color="#ddeeff"/></style:style><style:style style:name="gr1" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#ffcc00" draw:stroke="solid" svg:stroke-color="#000000"/></style:style><style:style style:name="T" style:family="paragraph"><style:text-properties fo:font-size="36pt" fo:font-weight="bold"/></style:style>"##,
        );
        let image = draw(&data).expect("draws");
        assert!(image.width() > 500 && image.height() > 250);
        let (w, h) = (image.width(), image.height());
        // the background is the slide's, the rectangle is yellow
        assert_eq!(image.get_pixel(w - 2, h - 2).0[..3], [0xdd, 0xee, 0xff]);
        let yellow = image
            .pixels()
            .filter(|p| p.0[0] > 240 && p.0[1] > 190 && p.0[1] < 215 && p.0[2] < 30)
            .count();
        assert!(yellow > 1000, "the rectangle is drawn");
        let dark = image.pixels().filter(|p| p.0[0] < 100 && p.0[1] < 100).count();
        assert!(dark > 300, "there is text");
    }

    #[test]
    fn white_space_of_the_xml_is_one_space() {
        assert_eq!(collapse("a  \n   b"), "a b");
        assert_eq!(collapse("  x "), " x ");
    }

    #[test]
    fn a_file_with_no_slide_or_a_password_is_an_error_not_a_panic() {
        let data = zip(&[("mimetype", b"application/vnd.oasis.opendocument.presentation", false)]);
        assert!(draw(&data).is_err());
        let manifest = br#"<manifest><file-entry><encryption-data/></file-entry></manifest>"#;
        let locked = zip(&[("META-INF/manifest.xml", manifest, true), ("content.xml", b"x", false)]);
        assert!(draw(&locked).expect_err("refused").to_string().contains("password"));
    }

    #[test]
    fn groups_nested_deep_are_given_up() {
        let mut body = String::from(
            r#"<draw:rect svg:x="1cm" svg:y="1cm" svg:width="3cm" svg:height="3cm"><text:p>core</text:p></draw:rect>"#,
        );
        for _ in 0..500 {
            body = format!("<draw:g>{body}</draw:g>");
        }
        assert!(draw(&odp(&body, "")).is_ok());
    }
}
