//! The start of an OpenDocument text (odt, LibreOffice and OpenOffice) as a picture: a preview, not a rendering that matches the
//! program that made it.
//!
//! An odt is a ZIP of XML (`content.xml`, `styles.xml`). Read here: the page size and margins, the styles (sizes, bold, italic,
//! underline, colour, family, alignment, spacing, indents, with the parent style), headings, paragraphs with their spans, tabs,
//! spaces and line breaks, lists (every list as bullets), tables (column widths, spans, cell borders and shading) and pictures
//! that are PNG, JPEG, GIF, BMP, WebP or TIFF, set in the line. The first page only: the layout stops where the text reaches the
//! bottom margin (or a page break).
//!
//! Not done: headers and footers, numbered lists as numbers, pictures and text boxes that float beside the text or have a fill (they are
//! set in the line, under their paragraph), shapes, charts, formulas, columns, notes, tracked changes, the shading of a run of text,
//! justified text (set left), tables in a section that
//! carries its own columns, the size of a line given as a length, fonts by theme, exact line breaks (words are measured by a rough
//! table, so a line may be longer or shorter than in the program that made the file).
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser has no DTDs and has a limit of
//! nodes and depth, tables and lists are laid out down to a depth, the number of blocks looked at is counted, and the layout stops
//! at one page.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;

use image::RgbaImage;
use roxmltree::{Document, Node};

use crate::archive::Archive;
use crate::color::hex;
use crate::decode::DecodeError;
use crate::flow::{self, Align, Atom, Layout, words};
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::office::{MAX_BYTES, MAX_PART, Pictures, is_encrypted};
use crate::page::{Font, Page, Rgb};
use crate::xml::{attr, child, children, descendant, parse};

/// Tables inside tables, and lists inside lists, are laid out down to this depth: a table lays its cells out twice (to find the height
/// of the row, then to draw), so the work doubles with every level.
const MAX_DEPTH: usize = 4;
/// The columns of a table that are read, and the rows.
const MAX_COLUMNS: usize = 64;
const MAX_ROWS: usize = 5_000;
/// The blocks (paragraphs, headings, tables, rows) that are looked at in all.
const MAX_BLOCKS: usize = 200_000;
/// The words of one paragraph that are laid out at most.
const MAX_ATOMS: usize = 20_000;
/// The styles that are read, and how long a chain of parents is followed.
const MAX_STYLES: usize = 20_000;
const MAX_CHAIN: usize = 16;

/// A4 and one inch of margin, in points, for a document that says nothing.
const A4: (f32, f32) = (595.3, 841.9);
const MARGIN: f32 = 72.0;
const CELL_PAD: f32 = 4.5;
/// The indent of one level of a list.
const LIST_STEP: f32 = 18.0;

/// Whether the name says odt.
pub fn is_odt(path: &Path) -> bool {
    has_extension(path, "odt")
}

/// Draws the first page of the document in `path` on white; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("odt", "the document could not be drawn", || draw(&data))
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    if is_encrypted(&archive) {
        return Err(unreadable("the document is protected with a password"));
    }
    let content = archive.read("content.xml", MAX_PART).map_err(|e| match e {
        DecodeError::Unreadable(_) => unreadable("this is no OpenDocument text (no content.xml)"),
        other => other,
    })?;
    let content_text = String::from_utf8_lossy(&content);
    let content_xml = parse(&content_text)?;
    let styles_text = archive.text("styles.xml", MAX_PART);
    let styles_xml = styles_text.as_deref().and_then(|t| parse(t).ok());

    let root = content_xml.root_element();
    let body = child(root, "body")
        .and_then(|b| child(b, "text"))
        .ok_or_else(|| unreadable("the document has no text"))?;

    let mut styles = Styles::default();
    // the file's own styles first, then the automatic ones of the content (which may name them as parents)
    if let Some(xml) = &styles_xml {
        styles.read(xml.root_element());
    }
    styles.read(root);
    let first_master = first_master_page(body, root, styles_xml.as_ref().map(Document::root_element));
    let geometry = geometry(styles_xml.as_ref().map(Document::root_element), root, first_master);

    let doc = Doc {
        styles,
        pictures: Pictures::new(&archive),
        blocks: Cell::new(0),
    };
    let mut page = Page::new(geometry.width, geometry.height);
    let mut y = geometry.top;
    doc.blocks(
        body,
        geometry.left,
        geometry.width - geometry.left - geometry.right,
        &mut y,
        geometry.height - geometry.bottom,
        &mut page,
        0,
    );
    page.render()
}

// ---- reading the XML --------------------------------------------------------------------------------------------

/// A length (`2.5cm`, `10mm`, `0.5in`, `12pt`, `1pc`, `3px`) in points; a number with no unit is not one.
pub(crate) fn length(value: Option<&str>) -> Option<f32> {
    let value = value?.trim();
    let cut = value.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = value.split_at(cut);
    let number = number.trim().parse::<f32>().ok().filter(|n| n.is_finite())?;
    let factor = match unit {
        "cm" => 72.0 / 2.54,
        "mm" => 72.0 / 25.4,
        "in" => 72.0,
        "pt" => 1.0,
        "pc" => 12.0,
        "px" => 0.75,
        _ => return None,
    };
    Some(number * factor)
}

/// A size of text: points, or a part of the size it stands in (`120%`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Size {
    Points(f32),
    Part(f32),
}

fn size(value: Option<&str>) -> Option<Size> {
    let value = value?.trim();
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|p| p.is_finite() && *p > 0.0)
            .map(|p| Size::Part(p / 100.0));
    }
    length(Some(value)).map(Size::Points)
}

/// What a style says, for the paragraph and for the text in it at once (a style of text has only the second half).
#[derive(Clone, Default, Debug)]
pub(crate) struct Props {
    pub(crate) align: Option<Align>,
    pub(crate) before: Option<f32>,
    pub(crate) after: Option<f32>,
    /// The height of a line as a multiple of the usual one (`150%`).
    pub(crate) line: Option<f32>,
    pub(crate) left: Option<f32>,
    pub(crate) first: Option<f32>,
    pub(crate) page_break: Option<bool>,
    pub(crate) bold: Option<bool>,
    pub(crate) italic: Option<bool>,
    pub(crate) underline: Option<bool>,
    pub(crate) size: Option<Size>,
    pub(crate) colour: Option<Rgb>,
    pub(crate) family: Option<String>,
    /// For a cell: a border is drawn, the fill.
    pub(crate) border: Option<bool>,
    pub(crate) fill: Option<Rgb>,
    /// For a cell: the text wraps.
    pub(crate) wrap: Option<bool>,
    /// For a shape: the colour of its line, and where its text stands (0 top, 1 middle, 2 bottom).
    pub(crate) stroke: Option<Rgb>,
    /// The line is as thick as this, and the style says there is no fill or no line (so that what a parent style gives is taken back).
    pub(crate) stroke_width: Option<f32>,
    pub(crate) fill_none: bool,
    pub(crate) stroke_none: bool,
    pub(crate) valign: Option<u8>,
}

impl Props {
    /// `top` laid over this: what it says wins.
    pub(crate) fn over(&mut self, top: &Props) {
        self.align = top.align.or(self.align);
        self.before = top.before.or(self.before);
        self.after = top.after.or(self.after);
        self.line = top.line.or(self.line);
        self.left = top.left.or(self.left);
        self.first = top.first.or(self.first);
        self.page_break = top.page_break.or(self.page_break);
        self.bold = top.bold.or(self.bold);
        self.italic = top.italic.or(self.italic);
        self.underline = top.underline.or(self.underline);
        // a part of the size is a part of the size below it
        self.size = match (top.size, self.size) {
            (Some(Size::Part(p)), Some(Size::Points(s))) => Some(Size::Points(s * p)),
            (Some(Size::Part(p)), Some(Size::Part(q))) => Some(Size::Part(p * q)),
            (top, below) => top.or(below),
        };
        self.colour = top.colour.or(self.colour);
        self.family = top.family.clone().or_else(|| self.family.take());
        self.border = top.border.or(self.border);
        if top.fill.is_some() {
            self.fill = top.fill;
            self.fill_none = false;
        } else if top.fill_none {
            self.fill = None;
            self.fill_none = true;
        }
        self.wrap = top.wrap.or(self.wrap);
        if top.stroke.is_some() {
            self.stroke = top.stroke;
            self.stroke_none = false;
        } else if top.stroke_none {
            self.stroke = None;
            self.stroke_none = true;
        }
        self.stroke_width = top.stroke_width.or(self.stroke_width);
        self.valign = top.valign.or(self.valign);
    }

    pub(crate) fn font(&self) -> Font {
        let points = match self.size {
            Some(Size::Points(s)) => s,
            Some(Size::Part(p)) => 12.0 * p,
            None => 12.0,
        };
        Font {
            size: points.clamp(1.0, 400.0),
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            colour: self.colour.unwrap_or([0, 0, 0]),
            family: self.family.clone(),
        }
    }
}

/// A colour as ODF writes it (`#336699`).
pub(crate) fn colour(value: Option<&str>) -> Option<Rgb> {
    hex(value.map(|v| v.trim_start_matches('#')))
}

/// Whether a border property draws something (`0.5pt solid #000000`; `none` and `0cm` do not).
fn drawn(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value != "none"
        && !value.starts_with("0cm")
        && !value.starts_with("0pt")
        && !value.starts_with("0in")
}

/// The properties one `style:style` (or `default-style`) element says.
fn props_of(style: Node<'_, '_>, fonts: &HashMap<String, String>) -> Props {
    let mut props = Props::default();
    if let Some(p) = child(style, "paragraph-properties") {
        props.align = attr(p, "text-align").map(|v| match v {
            "center" => Align::Center,
            "right" | "end" => Align::Right,
            _ => Align::Left,
        });
        props.before = length(attr(p, "margin-top"));
        props.after = length(attr(p, "margin-bottom"));
        props.left = length(attr(p, "margin-left"));
        props.first = length(attr(p, "text-indent"));
        props.line = attr(p, "line-height")
            .and_then(|v| v.strip_suffix('%'))
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| (v / 100.0).clamp(0.5, 5.0));
        props.page_break = attr(p, "break-before").map(|v| v == "page");
    }
    if let Some(t) = child(style, "text-properties") {
        props.bold = attr(t, "font-weight").map(|v| v == "bold" || v.parse::<u32>().is_ok_and(|w| w >= 600));
        props.italic = attr(t, "font-style").map(|v| v == "italic" || v == "oblique");
        props.underline = attr(t, "text-underline-style").map(|v| v != "none");
        props.size = size(attr(t, "font-size"));
        props.colour = colour(attr(t, "color"));
        props.family = attr(t, "font-family")
            .or_else(|| attr(t, "font-name").and_then(|n| fonts.get(n).map(String::as_str)))
            .map(|f| f.trim_matches('\'').to_string());
    }
    if let Some(c) = child(style, "table-cell-properties") {
        let sides = ["border", "border-top", "border-bottom", "border-left", "border-right"];
        let said: Vec<&str> = sides.iter().filter_map(|s| attr(c, s)).collect();
        if !said.is_empty() {
            props.border = Some(said.iter().any(|v| drawn(v)));
        }
        props.fill = colour(attr(c, "background-color"));
        props.wrap = attr(c, "wrap-option").map(|v| v == "wrap");
    }
    // a shape, or the page of a presentation: the fill, the line, where the text stands
    if let Some(g) = child(style, "graphic-properties").or_else(|| child(style, "drawing-page-properties")) {
        match attr(g, "fill") {
            Some("solid") => props.fill = colour(attr(g, "fill-color")),
            Some("none") => props.fill_none = true,
            _ => {}
        }
        match attr(g, "stroke") {
            Some("solid" | "dash") => props.stroke = colour(attr(g, "stroke-color")).or(Some([0, 0, 0])),
            Some("none") => props.stroke_none = true,
            _ => {}
        }
        props.stroke_width = length(attr(g, "stroke-width"));
        // where the text of a shape stands in it, as the style of the shape says (it overrides the paragraph's own alignment here only
        // when the style says it)
        if let Some(align) = attr(g, "textarea-horizontal-align").and_then(|v| match v {
            "center" => Some(Align::Center),
            "right" => Some(Align::Right),
            "left" => Some(Align::Left),
            _ => None,
        }) {
            props.align = Some(align);
        }
        props.valign = attr(g, "textarea-vertical-align").and_then(|v| match v {
            "top" => Some(0),
            "middle" => Some(1),
            "bottom" => Some(2),
            _ => None,
        });
    }
    props
}

struct Style {
    parent: Option<String>,
    props: Props,
}

#[derive(Default)]
pub(crate) struct Styles {
    /// By family and name.
    by_name: HashMap<(String, String), Style>,
    /// The default style of each family (the text of a paragraph that no style reaches, the look of a cell that none names).
    defaults: HashMap<String, Props>,
    /// The widths of the columns of the table styles, by name.
    pub(crate) columns: HashMap<String, f32>,
    /// The heights of the row styles, by name.
    pub(crate) rows: HashMap<String, f32>,
}

impl Styles {
    /// Reads the styles under `root` (a `styles.xml` or a `content.xml`): the named, the automatic, the default, the fonts.
    pub(crate) fn read(&mut self, root: Node<'_, '_>) {
        let mut fonts: HashMap<String, String> = HashMap::new();
        for face in root
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "font-face")
        {
            if let (Some(name), Some(family)) = (attr(face, "name"), attr(face, "font-family")) {
                fonts.insert(name.to_string(), family.to_string());
            }
        }
        for style in root.descendants().filter(|n| n.is_element()) {
            if self.by_name.len() >= MAX_STYLES {
                return;
            }
            match style.tag_name().name() {
                "default-style" => {
                    if let Some(family) = attr(style, "family") {
                        self.defaults
                            .entry(family.to_string())
                            .or_default()
                            .over(&props_of(style, &fonts));
                    }
                }
                "style" => {
                    let (Some(name), Some(family)) = (attr(style, "name"), attr(style, "family")) else {
                        continue;
                    };
                    if family == "table-column"
                        && let Some(width) =
                            child(style, "table-column-properties").and_then(|p| length(attr(p, "column-width")))
                    {
                        self.columns.insert(name.to_string(), width);
                    }
                    if family == "table-row"
                        && let Some(height) = child(style, "table-row-properties")
                            .and_then(|p| length(attr(p, "row-height")).or_else(|| length(attr(p, "min-row-height"))))
                    {
                        self.rows.insert(name.to_string(), height);
                    }
                    self.by_name.insert(
                        (family.to_string(), name.to_string()),
                        Style {
                            parent: attr(style, "parent-style-name").map(str::to_string),
                            props: props_of(style, &fonts),
                        },
                    );
                }
                _ => {}
            }
        }
    }

    /// Whether the style `name` of this family, or one of its parents, says a size (the default style of the family does not count).
    pub(crate) fn says_size(&self, family: &str, name: Option<&str>) -> bool {
        let mut at = name;
        for _ in 0..MAX_CHAIN {
            let Some(current) = at else {
                return false;
            };
            let Some(style) = self.by_name.get(&(family.to_string(), current.to_string())) else {
                return false;
            };
            if style.props.size.is_some() {
                return true;
            }
            at = style.parent.as_deref();
        }
        false
    }

    /// The properties of the style `name` of this family, over those of its parents (the default style at the bottom of a paragraph).
    pub(crate) fn resolve(&self, family: &str, name: Option<&str>) -> Props {
        let mut props = self.defaults.get(family).cloned().unwrap_or_default();
        props.over(&self.chain(family, name));
        props
    }

    /// What the style `name` of this family says, over what its parents say, without the default style of the family.
    pub(crate) fn chain(&self, family: &str, name: Option<&str>) -> Props {
        let mut chain = Vec::new();
        let mut at = name;
        while let Some(current) = at {
            if chain.len() >= MAX_CHAIN {
                break;
            }
            let Some(style) = self.by_name.get(&(family.to_string(), current.to_string())) else {
                break;
            };
            chain.push(&style.props);
            at = style.parent.as_deref();
        }
        let mut props = Props::default();
        for p in chain.into_iter().rev() {
            props.over(p);
        }
        props
    }
}

struct Geometry {
    width: f32,
    height: f32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

/// The size and margins of the first page layout (in `styles.xml`, else in the content).
/// The master page that the first page uses: the one that the style of the first paragraph or table names (`style:master-page-name`, which
/// the specification says starts a page with that master page), else none (the first master page of the file is taken then).
fn first_master_page<'a>(body: Node<'a, '_>, content: Node<'a, '_>, styles: Option<Node<'a, '_>>) -> Option<&'a str> {
    let first = body
        .children()
        .find(|n| n.is_element() && matches!(n.tag_name().name(), "p" | "h" | "table"))?;
    let wanted = attr(first, "style-name")?;
    [Some(content), styles].into_iter().flatten().find_map(|root| {
        root.descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "style" && attr(*n, "name") == Some(wanted))
            .and_then(|style| attr(style, "master-page-name"))
    })
}

/// The size and margins of the page layout of the first page: that of its master page (`first_master`, else the first master page of the
/// file), the properties it does not say taken from the default page layout (`style:default-page-layout`).
fn geometry(styles: Option<Node<'_, '_>>, content: Node<'_, '_>, first_master: Option<&str>) -> Geometry {
    // the file's first `page-layout-properties` may be the default style's, which has no size: the layout is found by the master page's name
    let named = styles.and_then(|root| {
        let master = root.descendants().find(|n| {
            n.is_element()
                && n.tag_name().name() == "master-page"
                && first_master.is_none_or(|name| attr(*n, "name") == Some(name))
        })?;
        let wanted = attr(master, "page-layout-name")?;
        let layout = root
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "page-layout" && attr(*n, "name") == Some(wanted))?;
        descendant(layout, "page-layout-properties")
    });
    let layout = named
        .or_else(|| styles.and_then(|root| descendant(root, "page-layout-properties")))
        .or_else(|| descendant(content, "page-layout-properties"));
    let defaults = styles
        .and_then(|root| descendant(root, "default-page-layout"))
        .and_then(|d| child(d, "page-layout-properties"));
    let get = |name: &str, default: f32, low: f32, high: f32| {
        [layout, defaults]
            .into_iter()
            .flatten()
            .find_map(|n| {
                length(attr(n, name)).or_else(|| length(attr(n, "margin")).filter(|_| name.starts_with("margin")))
            })
            .unwrap_or(default)
            .clamp(low, high)
    };
    let (mut width, mut height) = (
        get("page-width", A4.0, 72.0, 2000.0),
        get("page-height", A4.1, 72.0, 2000.0),
    );
    if layout.and_then(|n| attr(n, "print-orientation")) == Some("landscape") && width < height {
        std::mem::swap(&mut width, &mut height);
    }
    let mut g = Geometry {
        width,
        height,
        left: get("margin-left", MARGIN, 0.0, 1000.0),
        right: get("margin-right", MARGIN, 0.0, 1000.0),
        top: get("margin-top", MARGIN, 0.0, 1000.0),
        bottom: get("margin-bottom", MARGIN, 0.0, 1000.0),
    };
    // margins that leave nothing to write in are taken as the default ones
    if g.left + g.right > g.width * 0.8 {
        (g.left, g.right) = (MARGIN.min(g.width / 8.0), MARGIN.min(g.width / 8.0));
    }
    if g.top + g.bottom > g.height * 0.8 {
        (g.top, g.bottom) = (MARGIN.min(g.height / 8.0), MARGIN.min(g.height / 8.0));
    }
    g
}

// ---- laying out -------------------------------------------------------------------------------------------------

struct Doc<'a> {
    styles: Styles,
    pictures: Pictures<'a>,
    /// How many blocks have been looked at.
    blocks: Cell<usize>,
}

impl Doc<'_> {
    /// One more block looked at; false when too many have been (the rest of the document is left).
    fn spend(&self) -> bool {
        let n = self.blocks.get() + 1;
        self.blocks.set(n);
        n <= MAX_BLOCKS
    }

    /// Lays out the blocks under `parent` (paragraphs, headings, lists, tables, sections) from `y` down to `bottom`; `level` is how
    /// deep in lists and tables this is. False when the page is full.
    #[allow(clippy::too_many_arguments)] // the room (x, width, y, bottom, page) and how deep this is
    fn blocks(
        &self,
        parent: Node<'_, '_>,
        x: f32,
        width: f32,
        y: &mut f32,
        bottom: f32,
        out: &mut Page,
        level: usize,
    ) -> bool {
        for block in parent.children().filter(|c| c.is_element()) {
            if !self.spend() {
                return false;
            }
            let fits = match block.tag_name().name() {
                "p" => self.paragraph(block, x, width, y, bottom, out, None, 0),
                "h" => self.paragraph(block, x, width, y, bottom, out, Some(heading_level(block)), 0),
                "list" => level >= MAX_DEPTH || self.list(block, x, width, y, bottom, out, level),
                "table" => level >= MAX_DEPTH || self.table(block, x, width, y, bottom, out, level),
                "section" | "index-body" | "table-of-content" | "alphabetical-index" | "user-index"
                | "bibliography" => self.blocks(block, x, width, y, bottom, out, level),
                "frame" => {
                    // a picture anchored to the page stands under the text, not in a paragraph: set it as a line of its own
                    let mut atoms = Vec::new();
                    self.frame(block, &mut atoms);
                    let set = Layout {
                        align: Align::Left,
                        before: 0.0,
                        after: 0.0,
                        line_factor: 1.0,
                        left: 0.0,
                        first: 0.0,
                        bullet: false,
                        font: Font::default(),
                    };
                    let fits = flow::paragraph(atoms, &set, x, width, y, bottom, out);
                    fits && self.text_boxes(block, x, width, y, bottom, out, level)
                }
                _ => true,
            };
            if !fits {
                return false;
            }
        }
        true
    }

    /// The items of a list: paragraphs with a bullet, a list inside an item one step in.
    #[allow(clippy::too_many_arguments)] // the room (x, width, y, bottom, page) and how deep this is
    fn list(
        &self,
        node: Node<'_, '_>,
        x: f32,
        width: f32,
        y: &mut f32,
        bottom: f32,
        out: &mut Page,
        level: usize,
    ) -> bool {
        for item in children(node, "list-item").chain(children(node, "list-header")) {
            if !self.spend() {
                return false;
            }
            for block in item.children().filter(|c| c.is_element()) {
                let fits = match block.tag_name().name() {
                    "p" => self.paragraph(block, x, width, y, bottom, out, None, level + 1),
                    "h" => self.paragraph(block, x, width, y, bottom, out, Some(heading_level(block)), level + 1),
                    "list" => level + 1 >= MAX_DEPTH || self.list(block, x, width, y, bottom, out, level + 1),
                    _ => true,
                };
                if !fits {
                    return false;
                }
            }
        }
        true
    }

    /// The words, pictures, tabs and breaks under `node`, with the font each piece has.
    fn atoms(&self, node: Node<'_, '_>, base: &Props, atoms: &mut Vec<Atom>) {
        for item in node.children() {
            if atoms.len() >= MAX_ATOMS {
                return;
            }
            if item.is_text() {
                words(item.text().unwrap_or(""), &base.font(), atoms);
                continue;
            }
            if !item.is_element() {
                continue;
            }
            match item.tag_name().name() {
                "span" | "a" | "meta" | "ruby" | "ruby-base" | "ruby-text" => {
                    let mut props = base.clone();
                    props.over(&self.styles.resolve("text", attr(item, "style-name")));
                    self.atoms(item, &props, atoms);
                }
                // the spaces that ODF writes as a count; one is enough for a preview and the rest are as wide as they are
                "s" => {
                    let count = attr(item, "c")
                        .and_then(|c| c.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, 50);
                    words(&" ".repeat(count), &base.font(), atoms);
                }
                "tab" => atoms.push(Atom::Tab),
                "line-break" => atoms.push(Atom::Break),
                "frame" => self.frame(item, atoms),
                // what is not in the text and not in the line (a note, a comment, a mark of a change, a shape, a text box, a bookmark)
                "note"
                | "annotation"
                | "annotation-end"
                | "tracked-changes"
                | "changed-region"
                | "change"
                | "change-start"
                | "change-end"
                | "text-box"
                | "custom-shape"
                | "rect"
                | "ellipse"
                | "line"
                | "polygon"
                | "path"
                | "g"
                | "control"
                | "object"
                | "object-ole"
                | "plugin"
                | "applet"
                | "floating-frame"
                | "bookmark"
                | "bookmark-start"
                | "bookmark-end"
                | "reference-mark"
                | "reference-mark-start"
                | "reference-mark-end"
                | "index-title"
                | "soft-page-break" => {}
                // the fields (a date, a time, a page number, a title, a cross reference...) hold the text they show
                _ => self.atoms(item, base, atoms),
            }
        }
    }

    /// A picture in the text: its size from the frame, else its own.
    fn frame(&self, node: Node<'_, '_>, atoms: &mut Vec<Atom>) {
        let Some(image_node) = node
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "image")
        else {
            return;
        };
        let image = attr(image_node, "href")
            .map(|h| h.trim_start_matches("./"))
            .and_then(|path| self.pictures.get(path));
        let natural = image
            .as_ref()
            .map(|i| (i.width() as f32 * 0.75, i.height() as f32 * 0.75));
        let (width, height) = match (length(attr(node, "width")), length(attr(node, "height")), natural) {
            (Some(w), Some(h), _) => (w, h),
            (_, _, Some(n)) => n,
            _ => (60.0, 40.0),
        };
        atoms.push(Atom::Picture {
            image,
            width: width.clamp(1.0, 3000.0),
            height: height.clamp(1.0, 3000.0),
        });
    }

    /// One paragraph or heading, set by `flow`; `heading` is the outline level of a heading; `bullets` how deep in lists it stands
    /// (0: not in a list). False when the page is full.
    #[allow(clippy::too_many_arguments)] // the room (x, width, y, bottom, page) and what the block is
    fn paragraph(
        &self,
        node: Node<'_, '_>,
        x: f32,
        width: f32,
        y: &mut f32,
        bottom: f32,
        out: &mut Page,
        heading: Option<usize>,
        bullets: usize,
    ) -> bool {
        let mut props = self.styles.resolve("paragraph", attr(node, "style-name"));
        if let Some(level) = heading {
            // a heading whose style says nothing of its look is bold and bigger the higher it stands
            if props.bold.is_none() {
                props.bold = Some(true);
            }
            if !self.styles.says_size("paragraph", attr(node, "style-name")) {
                props.size = Some(Size::Points([18.0, 16.0, 14.0, 14.0, 12.0][level.clamp(1, 5) - 1]));
            }
        }
        if props.page_break == Some(true) && !out.is_empty() {
            return false;
        }
        let mut atoms = Vec::new();
        self.atoms(node, &props, &mut atoms);

        let mut left = props.left.unwrap_or(0.0).clamp(0.0, width * 0.8);
        let bullet = bullets > 0 && heading.is_none();
        if bullet {
            left = (left + LIST_STEP * bullets as f32).min(width * 0.8);
        }
        let first = if bullet { -12.0 } else { props.first.unwrap_or(0.0) }.clamp(-left, width * 0.5);
        let set = Layout {
            align: props.align.unwrap_or_default(),
            before: props.before.unwrap_or(0.0).clamp(0.0, 200.0),
            after: props.after.unwrap_or(0.0).clamp(0.0, 200.0),
            line_factor: props.line.unwrap_or(1.0),
            left,
            first,
            bullet,
            font: props.font(),
        };
        let fits = flow::paragraph(atoms, &set, x, width, y, bottom, out);
        fits && self.text_boxes(node, x, width, y, bottom, out, bullets)
    }

    /// The text boxes in the frames of `node` (the node itself, or a paragraph that holds them): their paragraphs are set under it, in
    /// the width the frame says when it says one, since a preview has no place to float them. False when the page is full.
    #[allow(clippy::too_many_arguments)] // the room (x, width, y, bottom, page) and how deep this is
    fn text_boxes(
        &self,
        node: Node<'_, '_>,
        x: f32,
        width: f32,
        y: &mut f32,
        bottom: f32,
        out: &mut Page,
        level: usize,
    ) -> bool {
        let frames = std::iter::once(node)
            .chain(node.children())
            .filter(|n| n.is_element() && n.tag_name().name() == "frame");
        for frame in frames {
            let Some(text_box) = child(frame, "text-box") else {
                continue;
            };
            if level >= MAX_DEPTH {
                return true;
            }
            let room = length(attr(frame, "width")).map_or(width, |w| w.clamp(20.0, width));
            if !self.blocks(text_box, x, room, y, bottom, out, level + 1) {
                return false;
            }
        }
        true
    }

    /// A table: columns as the styles say (scaled to the room), rows as high as their tallest cell; false when a row does not fit on
    /// the page.
    #[allow(clippy::too_many_arguments)] // the room (x, width, y, bottom, page) and how deep this is
    fn table(
        &self,
        node: Node<'_, '_>,
        x: f32,
        width: f32,
        y: &mut f32,
        bottom: f32,
        out: &mut Page,
        level: usize,
    ) -> bool {
        let mut columns: Vec<f32> = Vec::new();
        for column in node
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "table-column")
        {
            let repeat = attr(column, "number-columns-repeated")
                .and_then(|r| r.parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, MAX_COLUMNS);
            let wide = attr(column, "style-name")
                .and_then(|n| self.styles.columns.get(n))
                .copied()
                .unwrap_or(0.0);
            for _ in 0..repeat {
                if columns.len() < MAX_COLUMNS {
                    columns.push(wide);
                }
            }
        }
        let rows: Vec<Node<'_, '_>> = node
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "table-row")
            .take(MAX_ROWS)
            .collect();
        // a table that names no columns takes as many as its widest row has
        let in_row = |row: Node<'_, '_>| -> usize {
            children(row, "table-cell")
                .chain(children(row, "covered-table-cell"))
                .map(|c| {
                    attr(c, "number-columns-repeated")
                        .and_then(|r| r.parse::<usize>().ok())
                        .unwrap_or(1)
                        .clamp(1, MAX_COLUMNS)
                })
                .sum::<usize>()
                .min(MAX_COLUMNS)
        };
        let count = columns.len().max(rows.iter().map(|r| in_row(*r)).max().unwrap_or(0));
        if count == 0 {
            return true;
        }
        columns.resize(count, 0.0);
        // columns with no width share what the others leave; all of them are scaled to the room
        let known: f32 = columns.iter().sum();
        let unknown = columns.iter().filter(|w| **w <= 0.0).count();
        let share = if unknown == 0 {
            0.0
        } else {
            ((width - known).max(width * 0.2 * unknown as f32 / count as f32)) / unknown as f32
        };
        for w in &mut columns {
            if *w <= 0.0 {
                *w = share;
            }
        }
        let total: f32 = columns.iter().sum();
        let scale = if total > 0.0 { width / total } else { 1.0 };
        let widths: Vec<f32> = columns.iter().map(|w| w * scale).collect();

        for row in rows {
            if !self.spend() {
                return false;
            }
            // the cells of the row with the column each starts in and how many it spans
            let mut cells: Vec<(Node<'_, '_>, usize, usize)> = Vec::new();
            let mut column = 0;
            for cell in row.children().filter(|c| c.is_element()) {
                let name = cell.tag_name().name();
                if name != "table-cell" && name != "covered-table-cell" {
                    continue;
                }
                let repeat = attr(cell, "number-columns-repeated")
                    .and_then(|r| r.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_COLUMNS);
                let span = attr(cell, "number-columns-spanned")
                    .and_then(|r| r.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_COLUMNS);
                for _ in 0..repeat {
                    if column >= count {
                        break;
                    }
                    if name == "table-cell" {
                        cells.push((cell, column, span.min(count - column)));
                    }
                    column += if name == "table-cell" { span } else { 1 };
                }
            }
            let at = |c: usize| x + widths.iter().take(c).sum::<f32>();
            // how high the row is: each cell laid out on a page of its own that has no bottom
            let mut height = 0.0f32;
            for (cell, start, span) in &cells {
                let room = widths[*start..*start + *span].iter().sum::<f32>() - 2.0 * CELL_PAD;
                let mut scratch = Page::new(room.max(1.0) + 2.0 * CELL_PAD, 1.0e6);
                let mut inner = 0.0;
                self.blocks(*cell, 0.0, room.max(10.0), &mut inner, 1.0e6, &mut scratch, level + 1);
                height = height.max(inner + 2.0 * CELL_PAD);
            }
            height = height.max(CELL_PAD * 2.0 + 10.0);
            if *y + height > bottom && !out.is_empty() {
                return false;
            }
            for (cell, start, span) in &cells {
                let (left, room) = (at(*start), widths[*start..*start + *span].iter().sum::<f32>());
                let props = self.styles.resolve("table-cell", attr(*cell, "style-name"));
                if props.fill.is_some() || props.border == Some(true) {
                    out.rect(
                        left,
                        *y,
                        room,
                        height,
                        props.fill,
                        (props.border == Some(true)).then_some(([0, 0, 0], 0.5)),
                    );
                }
                let mut inner = *y + CELL_PAD;
                self.blocks(
                    *cell,
                    left + CELL_PAD,
                    (room - 2.0 * CELL_PAD).max(10.0),
                    &mut inner,
                    *y + height,
                    out,
                    level + 1,
                );
            }
            *y += height;
        }
        true
    }
}

/// The outline level of a heading (`text:outline-level`), 1 when it says nothing.
fn heading_level(node: Node<'_, '_>) -> usize {
    attr(node, "outline-level")
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 10)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink""#;

    fn odt(body: &str, automatic: &str) -> Vec<u8> {
        let content = format!(
            r#"<?xml version="1.0"?><office:document-content {NS}><office:automatic-styles>{automatic}</office:automatic-styles><office:body><office:text>{body}</office:text></office:body></office:document-content>"#
        );
        let styles = format!(
            r#"<?xml version="1.0"?><office:document-styles {NS}><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin-left="2cm" fo:margin-right="2cm" fo:margin-top="2cm" fo:margin-bottom="2cm"/></style:page-layout></office:automatic-styles></office:document-styles>"#
        );
        zip(&[
            ("mimetype", b"application/vnd.oasis.opendocument.text", false),
            ("content.xml", content.as_bytes(), true),
            ("styles.xml", styles.as_bytes(), true),
        ])
    }

    fn dark_pixels(image: &RgbaImage) -> usize {
        image.pixels().filter(|p| p.0[0] < 128 && p.0[3] > 0).count()
    }

    #[test]
    fn lengths_are_read_in_points() {
        assert_eq!(length(Some("1in")), Some(72.0));
        assert_eq!(length(Some("12pt")), Some(12.0));
        assert!((length(Some("2.54cm")).expect("cm") - 72.0).abs() < 0.01);
        assert_eq!(length(Some("10")), None);
        assert_eq!(length(Some("3furlongs")), None);
        assert_eq!(length(None), None);
    }

    #[test]
    fn a_paragraph_a_heading_and_a_bullet_are_drawn() {
        let data = odt(
            r#"<text:h text:outline-level="1">Title</text:h><text:p>Some words <text:span text:style-name="B">in bold</text:span> here.</text:p><text:list><text:list-item><text:p>one</text:p></text:list-item></text:list>"#,
            r#"<style:style style:name="B" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#,
        );
        let image = draw(&data).expect("draws");
        assert!(image.width() > 500 && image.height() > 700);
        assert!(dark_pixels(&image) > 200, "there is text on the page");
    }

    #[test]
    fn a_table_with_borders_and_a_fill_is_drawn() {
        let data = odt(
            r#"<table:table><table:table-column table:number-columns-repeated="2"/><table:table-row><table:table-cell table:style-name="C"><text:p>a</text:p></table:table-cell><table:table-cell><text:p>b</text:p></table:table-cell></table:table-row></table:table>"#,
            r##"<style:style style:name="C" style:family="table-cell"><style:table-cell-properties fo:border="0.5pt solid #000000" fo:background-color="#ffcc00"/></style:style>"##,
        );
        let image = draw(&data).expect("draws");
        let yellow = image
            .pixels()
            .filter(|p| p.0[0] > 240 && p.0[1] > 190 && p.0[1] < 215 && p.0[2] < 30)
            .count();
        assert!(yellow > 100, "the fill of the cell is there");
    }

    #[test]
    fn a_field_shows_its_text_and_a_note_is_left_out() {
        let with = draw(&odt(
            r#"<text:p>Today is <text:date text:fixed="true">the fourth of November</text:date>.</text:p>"#,
            "",
        ))
        .expect("draws");
        let without = draw(&odt(r#"<text:p>Today is .</text:p>"#, "")).expect("draws");
        assert!(dark_pixels(&with) > dark_pixels(&without) + 100, "the date is drawn");
        let note = draw(&odt(
            r#"<text:p>Text<text:note><text:note-body><text:p>a footnote that is long enough to show</text:p></text:note-body></text:note></text:p>"#,
            "",
        ))
        .expect("draws");
        let plain = draw(&odt(r#"<text:p>Text</text:p>"#, "")).expect("draws");
        assert_eq!(
            dark_pixels(&note),
            dark_pixels(&plain),
            "the note is not in the preview"
        );
    }

    #[test]
    fn a_text_box_is_set_under_its_paragraph() {
        let boxed = draw(&odt(
            r#"<text:p><draw:frame svg:width="5cm"><draw:text-box><text:p>Words in a box</text:p></draw:text-box></draw:frame></text:p>"#,
            "",
        ))
        .expect("draws");
        let empty = draw(&odt("<text:p/>", "")).expect("draws");
        assert!(dark_pixels(&boxed) > dark_pixels(&empty) + 100);
    }

    #[test]
    fn the_page_size_is_that_of_the_master_page_not_of_the_first_layout_in_the_file() {
        let content = format!(
            r#"<?xml version="1.0"?><office:document-content {NS}><office:body><office:text><text:p>Hello</text:p></office:text></office:body></office:document-content>"#
        );
        // the default page layout (no size) comes first; the master page names the real one, a small landscape page
        let styles = format!(
            r#"<?xml version="1.0"?><office:document-styles {NS}><office:styles><style:default-page-layout><style:page-layout-properties style:writing-mode="lr-tb"/></style:default-page-layout></office:styles><office:automatic-styles><style:page-layout style:name="Mpm1"><style:page-layout-properties fo:page-width="14.8cm" fo:page-height="10.5cm"/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="Mpm1"/></office:master-styles></office:document-styles>"#
        );
        let data = zip(&[
            ("content.xml", content.as_bytes(), true),
            ("styles.xml", styles.as_bytes(), true),
        ]);
        let image = draw(&data).expect("draws");
        assert!(image.width() > image.height(), "{} x {}", image.width(), image.height());
    }

    #[test]
    fn a_heading_whose_style_says_no_size_is_bigger_than_the_text() {
        let plain = draw(&odt(r#"<text:p>Heading words</text:p>"#, "")).expect("draws");
        let heading = draw(&odt(r#"<text:h text:outline-level="1">Heading words</text:h>"#, "")).expect("draws");
        // the same words, bold and bigger: more ink
        assert!(dark_pixels(&heading) > dark_pixels(&plain) * 3 / 2);
    }

    #[test]
    fn the_first_page_takes_the_master_page_that_the_style_of_the_first_paragraph_names() {
        let content = format!(
            r#"<?xml version="1.0"?><office:document-content {NS}><office:automatic-styles><style:style style:name="P1" style:family="paragraph" style:master-page-name="Second"/></office:automatic-styles><office:body><office:text><text:p text:style-name="P1">Hello</text:p></office:text></office:body></office:document-content>"#
        );
        let styles = format!(
            r#"<?xml version="1.0"?><office:document-styles {NS}><office:automatic-styles><style:page-layout style:name="A"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm"/></style:page-layout><style:page-layout style:name="B"><style:page-layout-properties fo:page-width="29.7cm" fo:page-height="21cm"/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="A"/><style:master-page style:name="Second" style:page-layout-name="B"/></office:master-styles></office:document-styles>"#
        );
        let data = zip(&[
            ("content.xml", content.as_bytes(), true),
            ("styles.xml", styles.as_bytes(), true),
        ]);
        let image = draw(&data).expect("draws");
        assert!(
            image.width() > image.height(),
            "the landscape master page is the one: {} x {}",
            image.width(),
            image.height()
        );
    }
    #[test]
    fn a_file_with_no_content_is_an_error_not_a_panic() {
        let data = zip(&[("mimetype", b"application/vnd.oasis.opendocument.text", false)]);
        assert!(draw(&data).is_err());
    }

    #[test]
    fn a_style_that_is_its_own_parent_ends() {
        let data = odt(
            r#"<text:p text:style-name="A">loop</text:p>"#,
            r#"<style:style style:name="A" style:family="paragraph" style:parent-style-name="B"/><style:style style:name="B" style:family="paragraph" style:parent-style-name="A"/>"#,
        );
        assert!(draw(&data).is_ok());
    }

    #[test]
    fn the_page_is_left_where_it_is_full() {
        // a page that is full and a page that is much fuller are the same size: the layout stops at the bottom margin
        let some = draw(&odt(&"<text:p>Line of text.</text:p>".repeat(200), "")).expect("draws");
        let many = draw(&odt(&"<text:p>Line of text.</text:p>".repeat(5000), "")).expect("draws");
        assert_eq!((some.width(), some.height()), (many.width(), many.height()));
    }

    #[test]
    fn lists_and_tables_nested_deep_are_given_up_in_good_time() {
        let mut body = String::from("<text:p>end</text:p>");
        for _ in 0..3000 {
            body = format!(
                "<table:table><table:table-row><table:table-cell>{body}</table:table-cell></table:table-row></table:table>"
            );
        }
        let started = std::time::Instant::now();
        let _ = draw(&odt(&body, ""));
        assert!(started.elapsed().as_secs() < 20);
    }
}
