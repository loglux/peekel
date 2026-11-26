//! The start of a Word document (docx) as a picture: a preview, not a rendering that matches Word.
//!
//! A docx is a ZIP of XML. Read here: the page size and margins, the styles (sizes, bold, italic, colour, alignment,
//! spacing, with `basedOn`), paragraphs with their runs, tabs and line breaks, bulleted lists (every list as bullets),
//! tables (column widths, spans, shading, borders when the table has them) and pictures that are PNG, JPEG, GIF, BMP,
//! WebP or TIFF. The first page only: the layout stops where the text reaches the bottom margin (or a page break).
//!
//! Not done: headers and footers, numbered lists as numbers, floating pictures (they flow with the text; ones behind the
//! text are left out), text boxes, shapes, charts, formulas, columns, tracked changes beyond leaving out what was
//! deleted, justified text (set left), vertically merged cells, fonts by theme, exact line breaks (words are measured
//! by a rough table, so a line may be longer or shorter than in Word).
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser has none of DTDs and
//! has a limit of nodes and depth, and the layout stops at one page.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;

use image::RgbaImage;
use roxmltree::{Document, Node};

use crate::archive::Archive;
use crate::book::{self, Block, Span};
use crate::color::hex;
use crate::decode::{ContentsItem, DecodeError};
use crate::flow::{self, Align, Atom, Layout, words};
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::office::{MAX_BYTES, MAX_PART, Pictures};
use crate::page::{Font, Page, Picture, Rgb};
use crate::xml::{self, attr, child, parse};

/// Tables inside tables are laid out down to this depth: each level lays its cells out twice (to find the height of the
/// row, then to draw), so the work doubles with every level.
const MAX_TABLE_DEPTH: usize = 4;
/// The columns of a table and the cells of a row that are read.
/// The words of one paragraph that are laid out at most.
const MAX_ATOMS: usize = 20_000;

/// A4 and one inch of margin, in points, for a document that says nothing.
const A4: (f32, f32) = (595.3, 841.9);
const MARGIN: f32 = 72.0;
const CELL_PAD: f32 = 4.5;
const MAX_COLUMNS: usize = 64;

/// Whether the name says docx, or a file with no extension begins as one (`sniff`).
pub fn is_docx(path: &Path) -> bool {
    has_extension(path, "docx") || crate::sniff::sniffed(path) == Some(crate::sniff::Sniffed::Docx)
}

/// Draws the first page of the document in `path` on white; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("docx", "the document could not be drawn", || draw(&data))
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    let body = archive.read("word/document.xml", MAX_PART).map_err(|e| match e {
        DecodeError::Unreadable(_) => unreadable("this is no Word document (no word/document.xml)"),
        other => other,
    })?;
    let text = String::from_utf8_lossy(&body);
    let xml = parse(&text)?;
    let styles_text = archive.text("word/styles.xml", MAX_PART);
    let styles_xml = styles_text.as_deref().and_then(|t| parse(t).ok());
    let rels = xml::relationships(&archive, "word/document.xml");

    let root = xml.root_element();
    let body = child(root, "body").ok_or_else(|| unreadable("the document has no body"))?;
    let geometry = geometry(body);
    let doc = Doc {
        rels,
        styles: styles_xml.as_ref().map(Styles::read).unwrap_or_default(),
        pictures: Pictures::new(&archive),
        table_depth: Cell::new(0),
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
    );
    page.render()
}

// ---- reading the XML --------------------------------------------------------------------------------------------

/// A property that is on when it is there and not switched off (`<w:b/>`, `<w:b w:val="0"/>`).
fn switch(node: Node<'_, '_>, name: &str) -> Option<bool> {
    child(node, name).map(|n| !matches!(attr(n, "val"), Some("0" | "false" | "off" | "none")))
}

fn twips(value: Option<&str>) -> Option<f32> {
    value
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v / 20.0)
}

#[derive(Clone, Default, Debug)]
struct ParaProps {
    align: Option<Align>,
    before: Option<f32>,
    after: Option<f32>,
    line: Option<f32>,
    left: Option<f32>,
    first: Option<f32>,
    /// What the book reading takes besides: Word's alignment as it is written (`both` is justified), the right indent, the outline
    /// level (0 a heading of the first level, 9 body text), the list (`numId`, `ilvl`), a page break before, and whether the space
    /// between paragraphs of one style is left out (`contextualSpacing`).
    justify: Option<bool>,
    right: Option<f32>,
    outline: Option<u8>,
    num: Option<(String, usize)>,
    page_before: Option<bool>,
    contextual: Option<bool>,
}

impl ParaProps {
    fn over(&mut self, top: &ParaProps) {
        self.align = top.align.or(self.align);
        self.before = top.before.or(self.before);
        self.after = top.after.or(self.after);
        self.line = top.line.or(self.line);
        self.left = top.left.or(self.left);
        self.first = top.first.or(self.first);
        self.justify = top.justify.or(self.justify);
        self.right = top.right.or(self.right);
        self.outline = top.outline.or(self.outline);
        self.num = top.num.clone().or_else(|| self.num.take());
        self.page_before = top.page_before.or(self.page_before);
        self.contextual = top.contextual.or(self.contextual);
    }
}

#[derive(Clone, Default, Debug)]
struct RunProps {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    size: Option<f32>,
    colour: Option<Rgb>,
    family: Option<String>,
    /// What the book reading takes besides: struck through, raised (1) or lowered (-1), small capitals, all capitals, hidden.
    strike: Option<bool>,
    lift: Option<i8>,
    small_caps: Option<bool>,
    caps: Option<bool>,
    hidden: Option<bool>,
}

impl RunProps {
    fn over(&mut self, top: &RunProps) {
        self.bold = top.bold.or(self.bold);
        self.italic = top.italic.or(self.italic);
        self.underline = top.underline.or(self.underline);
        self.size = top.size.or(self.size);
        self.colour = top.colour.or(self.colour);
        self.family = top.family.clone().or_else(|| self.family.take());
        self.strike = top.strike.or(self.strike);
        self.lift = top.lift.or(self.lift);
        self.small_caps = top.small_caps.or(self.small_caps);
        self.caps = top.caps.or(self.caps);
        self.hidden = top.hidden.or(self.hidden);
    }

    fn font(&self) -> Font {
        Font {
            size: self.size.unwrap_or(11.0).clamp(1.0, 400.0),
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
            strike: false,
            colour: self.colour.unwrap_or([0, 0, 0]),
            family: self.family.clone(),
        }
    }
}

fn run_props(node: Node<'_, '_>) -> RunProps {
    let Some(rpr) = child(node, "rPr") else {
        return RunProps::default();
    };
    RunProps {
        bold: switch(rpr, "b"),
        italic: switch(rpr, "i"),
        underline: switch(rpr, "u"),
        size: child(rpr, "sz")
            .and_then(|n| attr(n, "val"))
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map(|v| v / 2.0),
        colour: child(rpr, "color").and_then(|n| hex(attr(n, "val"))),
        family: child(rpr, "rFonts")
            .and_then(|n| attr(n, "ascii").or_else(|| attr(n, "hAnsi")))
            .map(str::to_string),
        strike: switch(rpr, "strike").or_else(|| switch(rpr, "dstrike")),
        lift: child(rpr, "vertAlign").and_then(|n| attr(n, "val")).map(|v| match v {
            "superscript" => 1,
            "subscript" => -1,
            _ => 0,
        }),
        small_caps: switch(rpr, "smallCaps"),
        caps: switch(rpr, "caps"),
        hidden: switch(rpr, "vanish"),
    }
}

/// The properties of a paragraph as written on it, and the name of its style.
fn para_props(node: Node<'_, '_>) -> (ParaProps, Option<String>, bool) {
    let Some(ppr) = child(node, "pPr") else {
        return (ParaProps::default(), None, false);
    };
    let spacing = child(ppr, "spacing");
    let ind = child(ppr, "ind");
    let first = ind.and_then(|n| twips(attr(n, "firstLine")).or_else(|| twips(attr(n, "hanging")).map(|h| -h)));
    let line = spacing.and_then(|n| {
        let auto = attr(n, "lineRule").is_none_or(|r| r == "auto");
        attr(n, "line")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| auto && v.is_finite() && *v > 0.0)
            .map(|v| (v / 240.0).clamp(0.5, 5.0))
    });
    let numbered = child(ppr, "numPr")
        .and_then(|n| child(n, "numId"))
        .and_then(|n| attr(n, "val"))
        .is_some_and(|v| v != "0");
    let num = child(ppr, "numPr").and_then(|n| {
        let id = child(n, "numId").and_then(|i| attr(i, "val"))?;
        let level = child(n, "ilvl")
            .and_then(|l| attr(l, "val"))
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0)
            .min(8);
        Some((id.to_string(), level))
    });
    let jc = child(ppr, "jc").and_then(|n| attr(n, "val"));
    (
        ParaProps {
            align: jc.map(|v| match v {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                _ => Align::Left,
            }),
            justify: jc.map(|v| {
                matches!(
                    v,
                    "both" | "distribute" | "justify" | "lowKashida" | "mediumKashida" | "highKashida"
                )
            }),
            right: ind.and_then(|n| twips(attr(n, "right").or_else(|| attr(n, "end")))),
            outline: child(ppr, "outlineLvl")
                .and_then(|n| attr(n, "val"))
                .and_then(|v| v.parse::<u8>().ok()),
            num,
            page_before: switch(ppr, "pageBreakBefore"),
            contextual: switch(ppr, "contextualSpacing"),
            before: spacing.and_then(|n| twips(attr(n, "before"))),
            after: spacing.and_then(|n| twips(attr(n, "after"))),
            line,
            left: ind.and_then(|n| twips(attr(n, "left").or_else(|| attr(n, "start")))),
            first,
        },
        child(ppr, "pStyle").and_then(|n| attr(n, "val")).map(str::to_string),
        numbered,
    )
}

struct Style {
    based_on: Option<String>,
    /// Its name in small letters (Word keeps the English names of its own styles, `heading 1`, whatever the language of the file).
    name: Option<String>,
    para: ParaProps,
    run: RunProps,
    borders: bool,
    default: bool,
}

#[derive(Default)]
struct Styles {
    by_id: HashMap<String, Style>,
    default_para: ParaProps,
    default_run: RunProps,
    /// The id of the paragraph style that is the default (`Normal`).
    normal: Option<String>,
}

/// Whether a `tblBorders` element has a border that is drawn.
fn has_borders(properties: Node<'_, '_>) -> bool {
    child(properties, "tblBorders").is_some_and(|b| {
        b.children()
            .filter(|c| c.is_element())
            .any(|c| !matches!(attr(c, "val"), None | Some("nil" | "none")))
    })
}

impl Styles {
    fn read(xml: &Document<'_>) -> Self {
        let mut styles = Styles::default();
        let root = xml.root_element();
        if let Some(defaults) = child(root, "docDefaults") {
            if let Some(r) = child(defaults, "rPrDefault") {
                styles.default_run = run_props(r);
            }
            if let Some(p) = child(defaults, "pPrDefault") {
                styles.default_para = para_props(p).0;
            }
        }
        for style in root
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "style")
        {
            let Some(id) = attr(style, "styleId") else {
                continue;
            };
            let default = attr(style, "default") == Some("1");
            if default && attr(style, "type") == Some("paragraph") {
                styles.normal = Some(id.to_string());
            }
            styles.by_id.insert(
                id.to_string(),
                Style {
                    based_on: child(style, "basedOn").and_then(|n| attr(n, "val")).map(str::to_string),
                    name: child(style, "name").and_then(|n| attr(n, "val")).map(str::to_lowercase),
                    para: para_props(style).0,
                    run: run_props(style),
                    borders: child(style, "tblPr").is_some_and(has_borders),
                    default,
                },
            );
        }
        styles
    }

    /// The paragraph and run properties of a style, its ancestors (`basedOn`, a few levels at most) underneath.
    fn resolve(&self, id: Option<&str>) -> (ParaProps, RunProps) {
        let mut chain = Vec::new();
        let mut at = id.or(self.normal.as_deref());
        while let Some(name) = at {
            if chain.len() >= 8 {
                break;
            }
            let Some(style) = self.by_id.get(name) else {
                // a heading the file does not describe
                if let Some(level) = name
                    .strip_prefix("Heading")
                    .or_else(|| name.strip_prefix("heading"))
                    .and_then(|n| n.parse::<usize>().ok())
                {
                    let sizes = [20.0, 16.0, 14.0, 12.0, 11.0, 11.0];
                    let run = RunProps {
                        bold: Some(true),
                        size: sizes.get(level.saturating_sub(1)).copied(),
                        ..RunProps::default()
                    };
                    return self.finish(Vec::new(), Some(run));
                }
                break;
            };
            chain.push(style);
            at = style.based_on.as_deref();
        }
        self.finish(chain, None)
    }

    fn finish(&self, chain: Vec<&Style>, extra: Option<RunProps>) -> (ParaProps, RunProps) {
        let mut para = self.default_para.clone();
        let mut run = self.default_run.clone();
        for style in chain.iter().rev() {
            para.over(&style.para);
            run.over(&style.run);
        }
        if let Some(extra) = extra {
            run.over(&extra);
        }
        (para, run)
    }

    fn table_has_borders(&self, id: Option<&str>) -> bool {
        let mut at = id.or_else(|| {
            self.by_id
                .iter()
                .find(|(_, s)| s.default && s.borders)
                .map(|(k, _)| k.as_str())
        });
        for _ in 0..8 {
            let Some(style) = at.and_then(|n| self.by_id.get(n)) else {
                return false;
            };
            if style.borders {
                return true;
            }
            at = style.based_on.as_deref();
        }
        false
    }

    /// The run properties of a character style (`rStyle`) and its ancestors, without the defaults (they are under it already).
    fn char_run(&self, id: &str) -> RunProps {
        let mut chain = Vec::new();
        let mut at = Some(id);
        while let Some(style) = at.and_then(|n| self.by_id.get(n)) {
            if chain.len() >= 8 {
                break;
            }
            chain.push(style);
            at = style.based_on.as_deref();
        }
        let mut run = RunProps::default();
        for style in chain.iter().rev() {
            run.over(&style.run);
        }
        run
    }

    /// The level of a heading its style (or one it is based on) says by its name or id: 0 the title (`Title`), 1 to 9 `heading 1`
    /// to `heading 9`; `None` for any other style.
    fn named_level(&self, id: Option<&str>) -> Option<usize> {
        fn level(id: &str, name: Option<&str>) -> Option<usize> {
            let number = |s: &str| s.trim().parse::<usize>().ok().filter(|n| (1..=9).contains(n));
            if let Some(name) = name {
                if name == "title" {
                    return Some(0);
                }
                if let Some(n) = name.strip_prefix("heading").and_then(number) {
                    return Some(n);
                }
            }
            if id == "Title" {
                return Some(0);
            }
            id.strip_prefix("Heading")
                .or_else(|| id.strip_prefix("heading"))
                .and_then(number)
        }
        let mut at = id;
        for _ in 0..8 {
            let name = at?;
            let Some(style) = self.by_id.get(name) else {
                return level(name, None);
            };
            if let Some(found) = level(name, style.name.as_deref()) {
                return Some(found);
            }
            at = style.based_on.as_deref();
        }
        None
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

/// The size and margins of the last section of the body.
fn geometry(body: Node<'_, '_>) -> Geometry {
    let section = body
        .descendants()
        .rfind(|n| n.is_element() && n.tag_name().name() == "sectPr");
    let size = section.and_then(|s| child(s, "pgSz"));
    let margins = section.and_then(|s| child(s, "pgMar"));
    let get = |node: Option<Node<'_, '_>>, name: &str, default: f32, low: f32, high: f32| {
        node.and_then(|n| twips(attr(n, name)))
            .unwrap_or(default)
            .clamp(low, high)
    };
    let (mut width, mut height) = (get(size, "w", A4.0, 72.0, 2000.0), get(size, "h", A4.1, 72.0, 2000.0));
    if size.and_then(|n| attr(n, "orient")) == Some("landscape") && width < height {
        std::mem::swap(&mut width, &mut height);
    }
    let mut g = Geometry {
        width,
        height,
        left: get(margins, "left", MARGIN, 0.0, 1000.0),
        right: get(margins, "right", MARGIN, 0.0, 1000.0),
        top: get(margins, "top", MARGIN, 0.0, 1000.0),
        bottom: get(margins, "bottom", MARGIN, 0.0, 1000.0),
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
    rels: HashMap<String, (String, String)>,
    styles: Styles,
    pictures: Pictures<'a>,
    /// How many tables are being laid out, one inside the other.
    table_depth: Cell<usize>,
}

impl Doc<'_> {
    /// Lays out the block elements under `parent` (paragraphs, tables, content controls) from `y` down to `bottom`;
    /// false when the page is full.
    fn blocks(&self, parent: Node<'_, '_>, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
        for block in parent.children().filter(|c| c.is_element()) {
            let fits = match block.tag_name().name() {
                "p" => self.paragraph(block, x, width, y, bottom, out),
                "tbl" => self.table(block, x, width, y, bottom, out),
                "sdt" => child(block, "sdtContent").is_none_or(|c| self.blocks(c, x, width, y, bottom, out)),
                _ => true,
            };
            if !fits {
                return false;
            }
        }
        true
    }

    /// The words, pictures, tabs and breaks of the runs under `node`, with the properties each run has.
    fn atoms(&self, node: Node<'_, '_>, base: &RunProps, atoms: &mut Vec<Atom>) {
        for item in node.children().filter(|c| c.is_element()) {
            if atoms.len() >= MAX_ATOMS {
                return;
            }
            match item.tag_name().name() {
                "r" => {
                    let mut props = base.clone();
                    props.over(&run_props(item));
                    let font = props.font();
                    for part in item.children().filter(|c| c.is_element()) {
                        match part.tag_name().name() {
                            "t" => words(part.text().unwrap_or(""), &font, atoms),
                            "tab" => atoms.push(Atom::Tab),
                            "noBreakHyphen" => words("-", &font, atoms),
                            "br" | "cr" => atoms.push(if attr(part, "type") == Some("page") {
                                Atom::PageBreak
                            } else {
                                Atom::Break
                            }),
                            "drawing" | "pict" => self.drawing(part, atoms),
                            _ => {}
                        }
                    }
                }
                // runs inside a link, a field, a tag, an insertion or a content control; a deletion is left out
                "hyperlink" | "fldSimple" | "smartTag" | "ins" | "sdtContent" | "sdt" | "customXml" => {
                    self.atoms(item, base, atoms);
                }
                _ => {}
            }
        }
    }

    fn drawing(&self, node: Node<'_, '_>, atoms: &mut Vec<Atom>) {
        // a picture that floats behind the text (a watermark, a background) is not part of the text's flow
        if node
            .descendants()
            .any(|n| n.is_element() && n.tag_name().name() == "anchor" && attr(n, "behindDoc") == Some("1"))
        {
            return;
        }
        let Some(blip) = node
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "blip")
        else {
            return;
        };
        let extent = node
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "extent");
        let emu = |name: &str| {
            extent
                .and_then(|e| attr(e, name))
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v / 12700.0)
        };
        let image = attr(blip, "embed")
            .and_then(|id| self.rels.get(id))
            .and_then(|(_, path)| self.pictures.get(path));
        let natural = image
            .as_ref()
            .map(|i| (i.width() as f32 * 0.75, i.height() as f32 * 0.75));
        let (width, height) = match (emu("cx"), emu("cy"), natural) {
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

    /// One paragraph: its properties from its style and from itself, its content as atoms, then `flow` sets it; `y` is moved
    /// below it. False when the page is full.
    fn paragraph(&self, node: Node<'_, '_>, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
        let (own, style, numbered) = para_props(node);
        let (mut props, run) = self.styles.resolve(style.as_deref());
        props.over(&own);
        let mut atoms = Vec::new();
        self.atoms(node, &run, &mut atoms);

        let mut left = props.left.unwrap_or(0.0).clamp(0.0, width * 0.8);
        if numbered && props.left.is_none() {
            left = 18.0;
        }
        let first = props
            .first
            .unwrap_or(if numbered { -12.0 } else { 0.0 })
            .clamp(-left, width * 0.5);
        let set = Layout {
            align: props.align.unwrap_or_default(),
            before: props.before.unwrap_or(0.0).clamp(0.0, 200.0),
            after: props.after.unwrap_or(0.0).clamp(0.0, 200.0),
            line_factor: props.line.unwrap_or(1.0),
            left,
            first,
            bullet: numbered,
            font: run.font(),
        };
        flow::paragraph(atoms, &set, x, width, y, bottom, out)
    }

    /// A table: columns as the grid says (scaled to the room), rows as high as their tallest cell; false when a row does
    /// not fit on the page.
    fn table(&self, node: Node<'_, '_>, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
        let depth = self.table_depth.get();
        if depth >= MAX_TABLE_DEPTH {
            return true; // a table nested deeper than this is left out
        }
        self.table_depth.set(depth + 1);
        let fits = self.table_laid_out(node, x, width, y, bottom, out);
        self.table_depth.set(depth);
        fits
    }

    fn table_laid_out(&self, node: Node<'_, '_>, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
        let properties = child(node, "tblPr");
        let borders = properties.is_some_and(|p| {
            has_borders(p)
                || self
                    .styles
                    .table_has_borders(child(p, "tblStyle").and_then(|n| attr(n, "val")))
        });
        let mut columns: Vec<f32> = child(node, "tblGrid")
            .map(|g| {
                g.children()
                    .filter(|c| c.is_element() && c.tag_name().name() == "gridCol")
                    .map(|c| twips(attr(c, "w")).unwrap_or(0.0).max(1.0))
                    .collect()
            })
            .unwrap_or_default();
        if columns.is_empty() {
            let count = node
                .children()
                .filter(|r| r.is_element() && r.tag_name().name() == "tr")
                .map(|r| {
                    r.children()
                        .filter(|c| c.is_element() && c.tag_name().name() == "tc")
                        .count()
                })
                .max()
                .unwrap_or(1)
                .max(1);
            columns = vec![width / count as f32; count];
        }
        columns.truncate(MAX_COLUMNS);
        let total: f32 = columns.iter().sum();
        if total > width {
            for c in &mut columns {
                *c *= width / total;
            }
        }
        for row in node
            .children()
            .filter(|r| r.is_element() && r.tag_name().name() == "tr")
        {
            // each cell: where it starts, how wide it is, and the node
            let mut cells = Vec::new();
            let mut at = 0usize;
            for cell in row
                .children()
                .filter(|c| c.is_element() && c.tag_name().name() == "tc")
                .take(MAX_COLUMNS)
            {
                let properties = child(cell, "tcPr");
                let span = properties
                    .and_then(|p| child(p, "gridSpan"))
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 64);
                let before: f32 = columns.iter().take(at).sum();
                let wide: f32 = columns.iter().skip(at).take(span).sum();
                let fill = properties
                    .and_then(|p| child(p, "shd"))
                    .and_then(|n| hex(attr(n, "fill")));
                cells.push((before, wide.max(1.0), cell, fill));
                at += span;
            }
            // the height of the row is found by laying the cells out on a page that is thrown away
            let mut tallest = 0.0f32;
            for (_, wide, cell, _) in &cells {
                let mut scratch = Page::new(out.width, out.height);
                let mut top = 0.0;
                self.blocks(
                    *cell,
                    0.0,
                    (wide - 2.0 * CELL_PAD).max(10.0),
                    &mut top,
                    f32::MAX,
                    &mut scratch,
                );
                tallest = tallest.max(top);
            }
            let height = tallest.max(12.0) + 4.0;
            if *y + height > bottom {
                return false;
            }
            for (before, wide, cell, fill) in &cells {
                if fill.is_some() || borders {
                    out.rect(
                        x + before,
                        *y,
                        *wide,
                        height,
                        *fill,
                        borders.then_some(([60, 60, 60], 0.5)),
                    );
                }
                let mut top = *y + 2.0;
                self.blocks(
                    *cell,
                    x + before + CELL_PAD,
                    (wide - 2.0 * CELL_PAD).max(10.0),
                    &mut top,
                    *y + height,
                    out,
                );
            }
            *y += height;
        }
        *y += 4.0;
        true
    }
}

// ---- read whole, as a book whose text flows ---------------------------------------------------------------------

/// Elements nested deeper than this (content controls in content controls, tables in tables, text boxes in text boxes) are left out.
const MAX_NEST: usize = 32;
/// The rows of a table that are read, and the letters of the text of one cell.
const MAX_ROWS: usize = 5000;
const MAX_CELL_TEXT: usize = 20_000;
/// The letters of a heading that go into the contents.
const MAX_TITLE: usize = 200;
/// The footnotes and the endnotes that are read at most, each.
const MAX_NOTES: usize = 10_000;
/// Empty paragraphs in a row that make space at most (a document spaced by empty lines has many).
const MAX_EMPTY: usize = 2;
/// The fonts whose text is code (a listing).
const MONO_FONTS: [&str; 8] = [
    "courier",
    "consolas",
    "menlo",
    "monaco",
    "lucida console",
    "lucida sans typewriter",
    "inconsolata",
    "fixedsys",
];

/// The lists of `word/numbering.xml`: each `numId`'s levels (the format of the number, the text with `%1`... in it, the first number),
/// and the numbers counted so far.
#[derive(Default)]
struct Numbering {
    levels: HashMap<String, Vec<(String, String, i64)>>,
    counters: HashMap<String, [i64; 9]>,
}

impl Numbering {
    fn read(xml: &Document<'_>) -> Self {
        let root = xml.root_element();
        let mut abstracts: HashMap<&str, Vec<(String, String, i64)>> = HashMap::new();
        for list in root
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "abstractNum")
        {
            let Some(id) = attr(list, "abstractNumId") else {
                continue;
            };
            let mut levels = vec![(String::from("bullet"), String::new(), 1); 9];
            for level in list
                .children()
                .filter(|c| c.is_element() && c.tag_name().name() == "lvl")
            {
                let Some(at) = attr(level, "ilvl")
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|l| *l < 9)
                else {
                    continue;
                };
                let value = |name: &str| child(level, name).and_then(|n| attr(n, "val"));
                levels[at] = (
                    value("numFmt").unwrap_or("decimal").to_string(),
                    value("lvlText").unwrap_or("").to_string(),
                    value("start")
                        .and_then(|v| v.parse::<i64>().ok())
                        .unwrap_or(1)
                        .clamp(0, 1_000_000),
                );
            }
            abstracts.insert(id, levels);
        }
        let mut levels = HashMap::new();
        for num in root
            .children()
            .filter(|c| c.is_element() && c.tag_name().name() == "num")
        {
            if let (Some(id), Some(list)) = (
                attr(num, "numId"),
                child(num, "abstractNumId").and_then(|n| attr(n, "val")),
            ) && let Some(found) = abstracts.get(list)
            {
                levels.insert(id.to_string(), found.clone());
            }
        }
        Self {
            levels,
            counters: HashMap::new(),
        }
    }

    /// The mark of the next item of list `id` at `level`, with a space after it: a bullet, or its number as the list writes it
    /// (`%1.%2.`); empty for a list that shows none.
    fn next(&mut self, id: &str, level: usize) -> String {
        let Some(levels) = self.levels.get(id) else {
            return "\u{2022} ".to_string();
        };
        let counters = self.counters.entry(id.to_string()).or_insert([0; 9]);
        let level = level.min(8);
        counters[level] = if counters[level] == 0 {
            levels[level].2.max(1)
        } else {
            counters[level].saturating_add(1)
        };
        for deeper in counters.iter_mut().skip(level + 1) {
            *deeper = 0;
        }
        let (format, text, _) = &levels[level];
        match format.as_str() {
            "bullet" => return "\u{2022} ".to_string(),
            "none" => return String::new(),
            _ => {}
        }
        let mut mark = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match (c, chars.peek().and_then(|d| d.to_digit(10))) {
                ('%', Some(d)) if (1..=9).contains(&d) => {
                    chars.next();
                    let at = d as usize - 1;
                    let n = if counters[at] == 0 {
                        levels[at].2.max(1)
                    } else {
                        counters[at]
                    };
                    mark.push_str(&number_as(&levels[at].0, n));
                }
                _ => mark.push(c),
            }
        }
        if mark.trim().is_empty() {
            mark = number_as(format, counters[level]);
        }
        mark.push(' ');
        mark
    }
}

/// A number in a format of Word's (`decimal`, `lowerLetter`, `upperRoman`...); one it does not know as a decimal one.
fn number_as(format: &str, n: i64) -> String {
    use crate::css::Marker;
    let marker = match format {
        "lowerLetter" => Marker::LowerAlpha,
        "upperLetter" => Marker::UpperAlpha,
        "lowerRoman" => Marker::LowerRoman,
        "upperRoman" => Marker::UpperRoman,
        "decimalZero" if (0..10).contains(&n) => return format!("0{n}"),
        _ => Marker::Decimal,
    };
    let mark = marker.of(n);
    mark.strip_suffix(". ").map_or(mark.clone(), str::to_string)
}

/// A piece of a paragraph as it is gathered: text (a line break ends one), a picture, a page break.
enum Piece {
    Text(Vec<Span>),
    Picture(Picture),
    PageBreak,
}

#[derive(Default)]
struct Gathered<'a, 'i> {
    pieces: Vec<Piece>,
    spans: Vec<Span>,
    /// The text boxes met, read as blocks after the paragraph.
    boxes: Vec<Node<'a, 'i>>,
    /// Only the text is wanted (a table's cell): pictures and text boxes are passed over.
    plain: bool,
}

impl Gathered<'_, '_> {
    fn text(&mut self, text: &str, look: &Span) {
        let text: String = text.chars().filter(|c| !matches!(c, '\u{AD}' | '\u{200B}')).collect();
        if text.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some(last)
                if Span {
                    text: String::new(),
                    ..last.clone()
                } == *look =>
            {
                last.text.push_str(&text)
            }
            _ => self.spans.push(Span { text, ..look.clone() }),
        }
    }

    fn end_line(&mut self) {
        let spans = std::mem::take(&mut self.spans);
        self.pieces.push(Piece::Text(spans));
    }
}

/// The document read into the blocks of a book.
struct Reader<'a> {
    archive: &'a Archive<'a>,
    rels: HashMap<String, (String, String)>,
    styles: Styles,
    numbering: Numbering,
    pictures: book::Pictures,
    blocks: Vec<Block>,
    /// The size of the text of the `Normal` style, which is the book's size.
    base: f32,
    /// Empty paragraphs met in a row.
    empty: usize,
    /// The style of the last paragraph and where its last block is, while nothing has come after it.
    last_paragraph: Option<(Option<String>, usize)>,
    /// The footnotes (0) and endnotes (1) met in the text, in order: their ids and the marks they were given.
    notes: [Vec<(String, String)>; 2],
    /// How each kind is numbered (`numFmt`).
    note_formats: [String; 2],
    /// The text of a note is being read: its references and headings count for nothing.
    in_note: bool,
}

/// Whether a font is one that code is set in.
fn is_mono(family: Option<&str>) -> bool {
    family.is_some_and(|f| {
        let f = f.to_lowercase();
        MONO_FONTS.iter().any(|m| f.contains(m))
            || f.split(|c: char| !c.is_alphanumeric())
                .any(|w| w.ends_with("mono") || w == "monospace")
    })
}

impl Reader<'_> {
    /// The block elements under `parent`: paragraphs, tables, and what holds them (content controls, custom XML, insertions).
    fn body(&mut self, parent: Node<'_, '_>, depth: usize) {
        if depth > MAX_NEST {
            return;
        }
        for block in parent.children().filter(|c| c.is_element()) {
            match block.tag_name().name() {
                "p" => self.paragraph(block, depth),
                "tbl" => self.table(block, depth + 1),
                "sdt" => {
                    if let Some(content) = child(block, "sdtContent") {
                        self.body(content, depth + 1);
                    }
                }
                "customXml" | "ins" | "moveTo" => self.body(block, depth + 1),
                _ => {}
            }
        }
    }

    /// The notes the text referred to, as an FB2's are: after the text, on a new page with a line of the contents ("Notes" for the
    /// footnotes, "Endnotes" for the endnotes), each its mark in bold and then its text. Word's separators (`w:type`) are no notes.
    fn notes_at_the_end(&mut self) {
        for (kind, part, title) in [(0, "word/footnotes.xml", "Notes"), (1, "word/endnotes.xml", "Endnotes")] {
            let used = std::mem::take(&mut self.notes[kind]);
            if used.is_empty() {
                continue;
            }
            let Some(text) = self.archive.text(part, MAX_PART) else {
                continue;
            };
            let Ok(xml) = parse(&text) else {
                continue;
            };
            let element = if kind == 0 { "footnote" } else { "endnote" };
            let notes: HashMap<&str, Node<'_, '_>> = xml
                .root_element()
                .children()
                .filter(|n| {
                    n.is_element() && n.tag_name().name() == element && attr(*n, "type").is_none_or(|t| t == "normal")
                })
                .filter_map(|n| attr(n, "id").map(|id| (id, n)))
                .collect();
            if !used.iter().any(|(id, _)| notes.contains_key(id.as_str())) {
                continue;
            }
            let rels = std::mem::replace(&mut self.rels, xml::relationships(self.archive, part));
            self.new_page();
            self.blocks.push(Block::Mark {
                depth: 0,
                title: title.to_string(),
            });
            self.in_note = true;
            for (id, mark) in &used {
                let Some(note) = notes.get(id.as_str()) else {
                    continue;
                };
                self.blocks.push(Block::Paragraph {
                    spans: vec![Span {
                        text: mark.clone(),
                        bold: true,
                        ..Span::default()
                    }],
                    style: book::Style {
                        bold: true,
                        align: book::Align::Left,
                        indent: false,
                        before: book::SIZE * 0.5,
                        ..book::Style::body()
                    },
                });
                self.last_paragraph = None;
                self.empty = 0;
                self.body(*note, 1);
            }
            self.in_note = false;
            self.rels = rels;
        }
    }

    /// A new page, unless nothing has been put on this one yet.
    fn new_page(&mut self) {
        let shown = self
            .blocks
            .iter()
            .rev()
            .take_while(|b| !matches!(b, Block::NewPage))
            .any(|b| matches!(b, Block::Paragraph { .. } | Block::Picture(_) | Block::Table(_)));
        if shown {
            self.blocks.push(Block::NewPage);
        }
    }

    /// The look of a run as a span of the book; `size` is the size of its paragraph's text.
    fn look(props: &RunProps, size: f32) -> Span {
        let own = props.size.unwrap_or(size);
        let ratio = own / size.max(1.0);
        Span {
            text: String::new(),
            bold: props.bold.unwrap_or(false),
            italic: props.italic.unwrap_or(false),
            mono: is_mono(props.family.as_deref()),
            raised: props.lift == Some(1),
            lowered: props.lift == Some(-1),
            small_caps: props.small_caps.unwrap_or(false),
            scale: ((ratio - 1.0).abs() > 0.05 && ratio.is_finite()).then(|| ratio.clamp(0.5, 2.5)),
            underline: props.underline.unwrap_or(false),
            strike: props.strike.unwrap_or(false),
            colour: props.colour.filter(|c| c.iter().any(|v| *v > 40)),
            sans: false,
        }
    }

    /// The runs under `node` gathered, in the properties `base` (the paragraph's) with each run's own over them.
    fn runs<'d, 'i>(
        &mut self,
        node: Node<'d, 'i>,
        base: &RunProps,
        size: f32,
        out: &mut Gathered<'d, 'i>,
        depth: usize,
    ) {
        if depth > MAX_NEST {
            return;
        }
        for item in node.children().filter(|c| c.is_element()) {
            match item.tag_name().name() {
                "r" => self.run(item, base, size, out),
                // runs inside a link, a field, a tag, an insertion or a content control; a deletion is left out
                "hyperlink" | "fldSimple" | "smartTag" | "ins" | "moveTo" | "sdt" | "sdtContent" | "customXml"
                | "dir" | "bdo" => self.runs(item, base, size, out, depth + 1),
                // a formula: its text, in italics
                "oMath" | "oMathPara" => {
                    let text: String = item
                        .descendants()
                        .filter(|n| n.is_element() && n.tag_name().name() == "t")
                        .filter_map(|n| n.text())
                        .collect();
                    let look = Span {
                        italic: true,
                        ..Self::look(base, size)
                    };
                    out.text(&text, &look);
                }
                _ => {}
            }
        }
    }

    fn run<'d, 'i>(&mut self, item: Node<'d, 'i>, base: &RunProps, size: f32, out: &mut Gathered<'d, 'i>) {
        let mut props = base.clone();
        if let Some(id) = child(item, "rPr")
            .and_then(|p| child(p, "rStyle"))
            .and_then(|n| attr(n, "val"))
        {
            props.over(&self.styles.char_run(id));
        }
        props.over(&run_props(item));
        if props.hidden == Some(true) {
            return;
        }
        let look = Self::look(&props, size);
        let caps = props.caps == Some(true);
        for part in item.children().filter(|c| c.is_element()) {
            match part.tag_name().name() {
                "t" => {
                    let text = part.text().unwrap_or("");
                    if caps {
                        out.text(&text.to_uppercase(), &look);
                    } else {
                        out.text(text, &look);
                    }
                }
                "tab" | "ptab" => out.text("\t", &look),
                "noBreakHyphen" => out.text("-", &look),
                // a note's mark: its number, raised, as a book's note is; the note itself at the end (`notes_at_the_end`)
                "footnoteReference" | "endnoteReference" if !self.in_note => {
                    let kind = usize::from(part.tag_name().name() == "endnoteReference");
                    if let Some(id) = attr(part, "id")
                        && self.notes[kind].len() < MAX_NOTES
                    {
                        let mark = number_as(&self.note_formats[kind], self.notes[kind].len() as i64 + 1);
                        out.text(
                            &mark,
                            &Span {
                                raised: true,
                                lowered: false,
                                ..look.clone()
                            },
                        );
                        self.notes[kind].push((id.to_string(), mark));
                    }
                }
                "br" | "cr" => match attr(part, "type") {
                    Some("page") => {
                        out.end_line();
                        out.pieces.push(Piece::PageBreak);
                    }
                    Some("column") => out.text(" ", &look),
                    _ => out.end_line(),
                },
                "drawing" | "pict" if !out.plain => self.drawing(part, out),
                // a drawing written two ways (a newer one and a fallback for older programs): the first
                "AlternateContent" if !out.plain => {
                    if let Some(first) = part.children().find(|c| c.is_element()) {
                        for drawing in first
                            .children()
                            .filter(|c| c.is_element() && matches!(c.tag_name().name(), "drawing" | "pict"))
                        {
                            self.drawing(drawing, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// A picture in the text (one that floats behind it is left out) and the text boxes of a drawing.
    fn drawing<'d, 'i>(&mut self, node: Node<'d, 'i>, out: &mut Gathered<'d, 'i>) {
        let boxes = node.descendants().filter(|n| {
            n.is_element()
                && n.tag_name().name() == "txbxContent"
                && !n
                    .ancestors()
                    .skip(1)
                    .take_while(|a| *a != node)
                    .any(|a| a.tag_name().name() == "txbxContent")
        });
        out.boxes.extend(boxes);
        if node
            .descendants()
            .any(|n| n.is_element() && n.tag_name().name() == "anchor" && attr(n, "behindDoc") == Some("1"))
        {
            return;
        }
        let id = node.descendants().find_map(|n| match n.tag_name().name() {
            "blip" if n.is_element() => attr(n, "embed"),
            "imagedata" if n.is_element() => attr(n, "id"),
            _ => None,
        });
        let Some((_, path)) = id.and_then(|id| self.rels.get(id)) else {
            return;
        };
        let archive = self.archive;
        if let Some(picture) = self.pictures.get(path, || archive.read(path, MAX_PART).ok()) {
            out.end_line();
            out.pieces.push(Piece::Picture(picture));
        }
    }

    /// The properties of a paragraph from its style and itself, the level of its heading, and the run properties of its text.
    fn props_of(&self, node: Node<'_, '_>) -> (ParaProps, Option<usize>, RunProps) {
        let (own, style, _) = para_props(node);
        let (mut props, run) = self.styles.resolve(style.as_deref());
        props.over(&own);
        let level = match props.outline {
            Some(o) if o < 9 => Some(usize::from(o) + 1),
            Some(_) => None,
            None => self.styles.named_level(style.as_deref()),
        };
        (props, level, run)
    }

    fn paragraph(&mut self, node: Node<'_, '_>, depth: usize) {
        let (props, level, run) = self.props_of(node);
        if props.page_before == Some(true) {
            self.new_page();
        }
        let size = run.size.unwrap_or(self.base);
        let mut gathered = Gathered::default();
        self.runs(node, &run, size, &mut gathered, depth + 1);
        gathered.end_line();
        let Gathered { pieces, boxes, .. } = gathered;

        let has_text = pieces
            .iter()
            .any(|p| matches!(p, Piece::Text(spans) if spans.iter().any(|s| !s.text.trim().is_empty())));
        let has_picture = pieces.iter().any(|p| matches!(p, Piece::Picture(_)));
        if !has_text && !has_picture {
            if !pieces.iter().any(|p| matches!(p, Piece::PageBreak))
                && !self.blocks.is_empty()
                && self.empty < MAX_EMPTY
            {
                self.blocks.push(Block::Space(book::SIZE * book::LEADING));
            }
            self.empty += 1;
        } else {
            self.empty = 0;
        }

        let mut style = self.style_of(&props, level, size);
        // `contextualSpacing`: no space between this paragraph and the one before it when they are of one style (the items of a list)
        let style_id = para_props(node).1;
        if props.contextual == Some(true)
            && let Some((id, at)) = &self.last_paragraph
            && *id == style_id
            && *at + 1 == self.blocks.len()
        {
            style.before = 0.0;
            if let Some(Block::Paragraph { style: previous, .. }) = self.blocks.get_mut(*at) {
                previous.after = 0.0;
            }
        }
        let mark = match &props.num {
            Some((id, at)) if id != "0" && has_text && level.is_none() => Some(self.numbering.next(id, *at)),
            _ => None,
        };
        if let Some(level) = level.filter(|l| (1..=3).contains(l))
            && has_text
            && !self.in_note
        {
            let title: String = pieces
                .iter()
                .filter_map(|p| match p {
                    Piece::Text(spans) => Some(spans.iter().map(|s| s.text.as_str()).collect::<String>()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(MAX_TITLE)
                .collect();
            if !title.is_empty() {
                self.blocks.push(Block::Mark {
                    depth: level - 1,
                    title,
                });
            }
        }
        let count = self.blocks.len();
        self.pieces(pieces, style, mark, run, size);
        self.last_paragraph = match self.blocks.last() {
            Some(Block::Paragraph { .. }) if self.blocks.len() > count => Some((style_id, self.blocks.len() - 1)),
            _ => None,
        };
        for content in boxes {
            self.body(content, depth + 1);
        }
        // a section that starts on a new page
        let section = child(node, "pPr").and_then(|p| child(p, "sectPr"));
        if section.is_some_and(|s| {
            !matches!(
                child(s, "type").and_then(|n| attr(n, "val")),
                Some("continuous" | "nextColumn")
            )
        }) {
            self.new_page();
        }
    }

    /// How a paragraph is set in the book: a heading in the book's sizes; any other with Word's alignment, indents and spaces.
    fn style_of(&self, props: &ParaProps, level: Option<usize>, size: f32) -> book::Style {
        let align = match (props.justify, props.align) {
            (Some(true), _) => book::Align::Justify,
            (_, Some(Align::Center)) => book::Align::Center,
            (_, Some(Align::Right)) => book::Align::Right,
            _ => book::Align::Left,
        };
        let loose = align == book::Align::Left;
        let space = |v: Option<f32>| v.map(|v| v.clamp(0.0, 36.0));
        let (before, after) = (space(props.before), space(props.after));
        let left = props.left.unwrap_or(0.0).clamp(0.0, 144.0);
        let right = props.right.unwrap_or(0.0).clamp(0.0, 144.0);
        if let Some(level) = level {
            let heading = book::Style::heading(level);
            return book::Style {
                align: if loose { book::Align::Left } else { align },
                before: before.unwrap_or(heading.before),
                after: after.unwrap_or(heading.after),
                left,
                right,
                ..heading
            };
        }
        let first = props.first.unwrap_or(0.0);
        book::Style {
            scale: (size / self.base.max(1.0)).clamp(0.6, 2.0),
            align,
            loose,
            indent: first > 0.5,
            indent_by: (first > 0.5).then(|| first.min(72.0)),
            left,
            right,
            before: before.unwrap_or(0.0),
            after: after.unwrap_or(0.0),
            ..book::Style::body()
        }
    }

    /// The pieces of a paragraph as blocks: its space above on the first line, below on the last, a line after a break not indented.
    fn pieces(&mut self, pieces: Vec<Piece>, style: book::Style, mark: Option<String>, run: RunProps, size: f32) {
        let last_text = pieces
            .iter()
            .rposition(|p| matches!(p, Piece::Text(spans) if spans.iter().any(|s| !s.text.trim().is_empty())));
        let mut first = true;
        let mut mark = mark;
        for (at, piece) in pieces.into_iter().enumerate() {
            match piece {
                Piece::PageBreak => self.new_page(),
                Piece::Picture(picture) => self.blocks.push(Block::Picture(Some(picture))),
                Piece::Text(mut spans) => {
                    if !spans.iter().any(|s| !s.text.trim().is_empty()) {
                        continue;
                    }
                    let mut style = style.clone();
                    if !first {
                        style.before = 0.0;
                        style.indent = false;
                    }
                    if Some(at) != last_text {
                        style.after = 0.0;
                    }
                    let code = spans.iter().all(|s| s.mono || s.text.trim().is_empty());
                    if code {
                        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
                        spans = vec![Span {
                            text: crate::epub::code_text(&text),
                            mono: true,
                            ..Span::default()
                        }];
                        style = book::Style {
                            align: book::Align::Left,
                            loose: false,
                            indent: false,
                            indent_by: None,
                            code: true,
                            ..style
                        };
                    } else {
                        for span in &mut spans {
                            span.text = span.text.replace('\t', " ");
                        }
                    }
                    let item = mark.is_some();
                    if let Some(mark) = mark.take().filter(|m| !m.is_empty()) {
                        spans.insert(
                            0,
                            Span {
                                text: mark,
                                ..Self::look(&run, size)
                            },
                        );
                        style.indent = false;
                        style.left = style.left.max(18.0);
                    }
                    // a paragraph with no indent and no space above, after another such: the layout may mark where it begins
                    // (the item of a list has its mark)
                    style.unmarked = style.loose
                        && !item
                        && !style.indent
                        && style.before == 0.0
                        && first
                        && matches!(self.blocks.last(), Some(Block::Paragraph { style: s, .. }) if s.loose && !s.code && s.after == 0.0);
                    first = false;
                    self.blocks.push(Block::Paragraph { spans, style });
                }
            }
        }
    }

    /// A table as a grid of the book: its cells' text, their spans (`gridSpan`, `vMerge`), its header rows (`tblHeader`). A table of
    /// one cell (a frame around the text, or a page laid out in it) is read as the text it holds.
    fn table<'d, 'i>(&mut self, node: Node<'d, 'i>, depth: usize) {
        if depth > MAX_NEST {
            return;
        }
        let rows: Vec<Node<'d, 'i>> = node
            .children()
            .filter(|r| r.is_element() && r.tag_name().name() == "tr")
            .take(MAX_ROWS)
            .collect();
        let cells_of = |row: &Node<'d, 'i>| -> Vec<Node<'d, 'i>> {
            row.children()
                .filter(|c| c.is_element() && c.tag_name().name() == "tc")
                .take(MAX_COLUMNS)
                .collect()
        };
        if rows.len() == 1
            && let [only] = cells_of(&rows[0]).as_slice()
        {
            self.body(*only, depth + 1);
            return;
        }
        #[derive(PartialEq)]
        enum Merge {
            None,
            Start,
            Go,
        }
        // each row: its cells with the column they start at, their span and their vertical merge
        let grid: Vec<Vec<(usize, usize, Merge, Node<'d, 'i>)>> = rows
            .iter()
            .map(|row| {
                let mut at = child(*row, "trPr")
                    .and_then(|p| child(p, "gridBefore"))
                    .and_then(|n| attr(n, "val"))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(MAX_COLUMNS);
                cells_of(row)
                    .into_iter()
                    .map(|cell| {
                        let properties = child(cell, "tcPr");
                        let span = properties
                            .and_then(|p| child(p, "gridSpan"))
                            .and_then(|n| attr(n, "val"))
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_COLUMNS);
                        let merge = match properties.and_then(|p| child(p, "vMerge")) {
                            None => Merge::None,
                            Some(m) if attr(m, "val") == Some("restart") => Merge::Start,
                            Some(_) => Merge::Go,
                        };
                        let start = at;
                        at += span;
                        (start, span, merge, cell)
                    })
                    .collect()
            })
            .collect();
        let mut table = book::Table::default();
        for (r, row) in grid.iter().enumerate() {
            let head = rows[r]
                .children()
                .find(|c| c.is_element() && c.tag_name().name() == "trPr")
                .and_then(|p| switch(p, "tblHeader"))
                .unwrap_or(false);
            let mut cells = Vec::new();
            for (column, span, merge, cell) in row {
                if *merge == Merge::Go {
                    continue;
                }
                let down = if *merge == Merge::Start {
                    1 + grid[r + 1..]
                        .iter()
                        .take_while(|next| next.iter().any(|(c, _, m, _)| c == column && *m == Merge::Go))
                        .count()
                } else {
                    1
                };
                let (text, bold) = self.cell_text(*cell, depth + 1);
                cells.push(book::Cell {
                    text,
                    columns: *span,
                    rows: down,
                    head,
                    bold: head || bold,
                });
            }
            table.rows.push(book::Row { cells, head });
        }
        if table.rows.iter().any(|r| !r.cells.is_empty()) {
            self.blocks.push(Block::Table(table));
        }
    }

    /// The text of a cell, a line for each of its paragraphs and line breaks (a table inside it a line a row), and whether all of it is
    /// bold.
    fn cell_text(&mut self, cell: Node<'_, '_>, depth: usize) -> (String, bool) {
        let mut text = String::new();
        let mut bold = true;
        let mut any = false;
        self.cell_lines(cell, depth, &mut text, &mut bold, &mut any);
        let text: String = text.trim_end().chars().take(MAX_CELL_TEXT).collect();
        (text, bold && any)
    }

    fn cell_lines(&mut self, node: Node<'_, '_>, depth: usize, text: &mut String, bold: &mut bool, any: &mut bool) {
        if depth > MAX_NEST || text.len() > MAX_CELL_TEXT {
            return;
        }
        for block in node.children().filter(|c| c.is_element()) {
            match block.tag_name().name() {
                "p" => {
                    let (_, _, run) = self.props_of(block);
                    let size = run.size.unwrap_or(self.base);
                    let mut gathered = Gathered {
                        plain: true,
                        ..Gathered::default()
                    };
                    self.runs(block, &run, size, &mut gathered, depth + 1);
                    gathered.end_line();
                    for piece in gathered.pieces {
                        if let Piece::Text(spans) = piece {
                            for span in &spans {
                                if !span.text.trim().is_empty() {
                                    *any = true;
                                    *bold &= span.bold;
                                }
                                text.push_str(&span.text.replace('\t', " "));
                            }
                            text.push('\n');
                        }
                    }
                }
                "tbl" => {
                    for row in block
                        .children()
                        .filter(|r| r.is_element() && r.tag_name().name() == "tr")
                    {
                        let mut line = Vec::new();
                        for cell in row.children().filter(|c| c.is_element() && c.tag_name().name() == "tc") {
                            let mut inner = String::new();
                            self.cell_lines(cell, depth + 1, &mut inner, bold, any);
                            line.push(inner.split_whitespace().collect::<Vec<_>>().join(" "));
                            if text.len() + inner.len() > MAX_CELL_TEXT {
                                break;
                            }
                        }
                        text.push_str(line.join("   ").trim());
                        text.push('\n');
                    }
                }
                "sdt" => {
                    if let Some(content) = child(block, "sdtContent") {
                        self.cell_lines(content, depth + 1, text, bold, any);
                    }
                }
                "customXml" | "ins" | "moveTo" => self.cell_lines(block, depth + 1, text, bold, any),
                _ => {}
            }
        }
    }
}

/// The blocks of the document at `path`, read whole as a book (`book`): its paragraphs, Word's headings as the book's (levels 1 to 3
/// the contents), lists with their bullets and numbers, tables, pictures in the text, page breaks as new pages.
pub fn blocks(path: &Path) -> Result<Vec<Block>, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    blocks_of(&data)
}

fn blocks_of(data: &[u8]) -> Result<Vec<Block>, DecodeError> {
    let archive = Archive::new(data)?;
    let body = archive.read("word/document.xml", MAX_PART).map_err(|e| match e {
        DecodeError::Unreadable(_) => unreadable("this is no Word document (no word/document.xml)"),
        other => other,
    })?;
    let text = String::from_utf8_lossy(&body);
    let xml = parse(&text)?;
    let styles_text = archive.text("word/styles.xml", MAX_PART);
    let styles_xml = styles_text.as_deref().and_then(|t| parse(t).ok());
    let numbering_text = archive.text("word/numbering.xml", MAX_PART);
    let numbering_xml = numbering_text.as_deref().and_then(|t| parse(t).ok());
    let styles = styles_xml.as_ref().map(Styles::read).unwrap_or_default();
    let base = styles.resolve(None).1.size.unwrap_or(11.0).clamp(4.0, 72.0);
    let mut reader = Reader {
        archive: &archive,
        rels: xml::relationships(&archive, "word/document.xml"),
        styles,
        numbering: numbering_xml.as_ref().map(Numbering::read).unwrap_or_default(),
        pictures: book::Pictures::default(),
        blocks: Vec::new(),
        base,
        empty: 0,
        last_paragraph: None,
        notes: [Vec::new(), Vec::new()],
        note_formats: [String::new(), String::new()],
        in_note: false,
    };
    let root = xml.root_element();
    let body = child(root, "body").ok_or_else(|| unreadable("the document has no body"))?;
    // how the notes are numbered: the last section's, else the settings', else Word's (1, 2, 3 and i, ii, iii)
    let settings_text = archive.text("word/settings.xml", MAX_PART);
    let settings_xml = settings_text.as_deref().and_then(|t| parse(t).ok());
    let section = body
        .children()
        .rfind(|c| c.is_element() && c.tag_name().name() == "sectPr");
    let format = |name: &str| {
        let of = |node: Option<Node<'_, '_>>| {
            node.and_then(|n| child(n, name))
                .and_then(|p| child(p, "numFmt"))
                .and_then(|f| attr(f, "val"))
                .map(str::to_string)
        };
        of(section).or_else(|| of(settings_xml.as_ref().map(|s| s.root_element())))
    };
    reader.note_formats = [
        format("footnotePr").unwrap_or_else(|| "decimal".to_string()),
        format("endnotePr").unwrap_or_else(|| "lowerRoman".to_string()),
    ];
    reader.body(body, 0);
    reader.notes_at_the_end();
    if !reader
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Paragraph { .. } | Block::Picture(_) | Block::Table(_)))
    {
        return Err(unreadable("the document has no text"));
    }
    Ok(reader.blocks)
}

/// The title and author of the document (`dc:title`, `dc:creator` of `docProps/core.xml`), reading only that entry of the archive.
pub fn title_and_author(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(mut zip) = crate::archive::OnDisk::open(path) else {
        return (None, None);
    };
    let Ok(bytes) = zip.read("docProps/core.xml", 1024 * 1024) else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let Ok(core) = parse(&text) else {
        return (None, None);
    };
    let value = |name: &str| {
        xml::descendant(core.root_element(), name)
            .and_then(|n| n.text())
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| !t.is_empty())
    };
    (value("title"), value("creator"))
}

/// Page `page` (from 0) of the document read whole, and the number of pages.
pub fn render_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    book::page(&*book::laid(path, blocks)?, page)
}

/// As [`render_page`], drawn at the size at which it fits the box `max_width` x `max_height` pixels (`book::page_within`).
pub fn render_page_within(
    path: &Path,
    page: usize,
    max_width: usize,
    max_height: usize,
) -> Result<(RgbaImage, usize), DecodeError> {
    book::page_within(&*book::laid(path, blocks)?, page, max_width, max_height)
}

/// The text of every page, for a search.
pub fn page_texts(path: &Path) -> Result<Option<Vec<String>>, DecodeError> {
    Ok(book::texts(&*book::laid(path, blocks)?))
}

/// The contents: the headings of levels 1 to 3.
pub fn contents(path: &Path) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    Ok(book::contents(&*book::laid(path, blocks)?))
}

/// Marks the words that `needle` is in on page `page` as [`render_page`] drew it; how many.
pub fn mark_words(path: &Path, page: usize, needle: &str, image: &mut RgbaImage) -> Result<usize, DecodeError> {
    Ok(book::mark_words(&*book::laid(path, blocks)?, page, needle, image))
}

/// The place of page `page` in the text, at the size of the text now.
pub fn place_of(path: &Path, page: usize) -> Result<usize, DecodeError> {
    Ok(book::place_of(&*book::laid(path, blocks)?, page))
}

/// The page that place `place` of the text is on, at the size of the text now.
pub fn page_at(path: &Path, place: usize) -> Result<usize, DecodeError> {
    Ok(book::page_at(&*book::laid(path, blocks)?, place))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    fn docx(body: &str, styles: &str) -> Vec<u8> {
        let document = format!(r#"<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>"#);
        let styles = format!(r#"<?xml version="1.0"?><w:styles {W}>{styles}</w:styles>"#);
        zip(&[
            ("word/document.xml", document.as_bytes(), true),
            ("word/styles.xml", styles.as_bytes(), true),
        ])
    }

    fn render(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
        draw(bytes)
    }

    /// The number of pixels darker than `limit` in a box of the picture (x, y, width, height as shares of 1000).
    fn dark(img: &RgbaImage, area: (u32, u32, u32, u32), limit: u8) -> usize {
        let (w, h) = img.dimensions();
        let (x0, y0, x1, y1) = (
            w * area.0 / 1000,
            h * area.1 / 1000,
            w * (area.0 + area.2) / 1000,
            h * (area.1 + area.3) / 1000,
        );
        (x0..x1)
            .flat_map(|x| (y0..y1).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[0] < limit)
            .count()
    }

    #[test]
    fn a_heading_and_a_paragraph_are_drawn_at_the_top_of_a_page_of_a4() {
        let styles = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="48"/></w:rPr></w:style>"#;
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>A heading of the document</w:t></w:r></w:p>
            <w:p><w:r><w:t>The first paragraph has some words in it, enough to be seen as a block of text on the page.</w:t></w:r></w:p>"#;
        let img = render(&docx(body, styles)).expect("draws");
        assert_eq!(img.height(), 2048);
        assert!(
            (img.width() as f32 / 2048.0 - 595.3 / 841.9).abs() < 0.01,
            "A4: {}",
            img.width()
        );
        assert!(dark(&img, (100, 80, 800, 60), 100) > 1500, "the heading");
        assert!(dark(&img, (100, 120, 800, 60), 100) > 400, "the paragraph");
        assert_eq!(dark(&img, (100, 600, 800, 300), 200), 0, "nothing below");
    }

    #[test]
    fn long_text_wraps_and_stops_at_the_bottom_of_the_page() {
        let word = "word ".repeat(100);
        let paragraph = format!("<w:p><w:r><w:t>{word}</w:t></w:r></w:p>");
        let img = render(&docx(&paragraph.repeat(200), "")).expect("draws");
        // text fills the page down to the margin and no further
        assert!(dark(&img, (100, 850, 800, 100), 100) > 500, "text near the bottom");
        assert_eq!(dark(&img, (0, 950, 1000, 50), 200), 0, "the margin is empty");
    }

    #[test]
    fn a_table_with_borders_and_shading_is_drawn_and_a_picture_is_laid_in() {
        let body = r#"<w:tbl><w:tblPr><w:tblBorders><w:top w:val="single"/><w:left w:val="single"/></w:tblBorders></w:tblPr>
            <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>
            <w:tr><w:tc><w:tcPr><w:shd w:val="clear" w:fill="FF0000"/></w:tcPr><w:p><w:r><w:t>one</w:t></w:r></w:p></w:tc>
                  <w:tc><w:p><w:r><w:t>two</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let img = render(&docx(body, "")).expect("draws");
        // the red cell: from the left margin (72 pt = 10% of the width) at the top margin
        let [r, g, b, _] = img.get_pixel(img.width() / 5, img.height() * 80 / 842).0;
        assert!(r > 240 && g < 20 && b < 20, "shaded cell: {r} {g} {b}");
    }

    #[test]
    fn a_page_size_and_landscape_in_the_section_are_taken() {
        let body = r#"<w:p><w:r><w:t>x</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840" w:orient="landscape"/><w:pgMar w:left="720" w:right="720" w:top="720" w:bottom="720"/></w:sectPr>"#;
        let img = render(&docx(body, "")).expect("draws");
        // letter turned: 792 x 612
        assert_eq!(img.width(), 2048);
        assert!(
            (img.height() as f32 / 2048.0 - 612.0 / 792.0).abs() < 0.01,
            "{}",
            img.height()
        );
    }

    #[test]
    fn what_is_not_a_document_or_is_damaged_is_an_error_and_not_a_panic() {
        assert!(render(b"not a zip").is_err());
        let no_body = zip(&[("word/other.xml", b"<x/>", false)]);
        assert!(render(&no_body).is_err());
        let broken = zip(&[("word/document.xml", b"<w:document><w:body>", false)]);
        assert!(render(&broken).is_err());
        let dtd = zip(&[(
            "word/document.xml",
            b"<!DOCTYPE d [<!ENTITY a \"aaaa\">]><d>&a;</d>",
            false,
        )]);
        assert!(render(&dtd).is_err(), "a DTD is not read");
    }

    #[test]
    fn a_word_wider_than_the_line_is_broken_and_stays_inside_the_margins() {
        let long = "x".repeat(400);
        let body = format!("<w:p><w:r><w:t>See {long} end</w:t></w:r></w:p>");
        let img = render(&docx(&body, "")).expect("draws");
        assert!(dark(&img, (100, 80, 780, 80), 100) > 800, "the word is drawn");
        assert_eq!(dark(&img, (885, 0, 115, 1000), 200), 0, "nothing in the right margin");
    }

    #[test]
    fn tables_nested_very_deep_are_given_up_in_good_time() {
        let mut body = String::from("<w:p><w:r><w:t>core</w:t></w:r></w:p>");
        for _ in 0..300 {
            body = format!("<w:tbl><w:tr><w:tc>{body}<w:p/></w:tc></w:tr></w:tbl>");
        }
        let started = std::time::Instant::now();
        let img = render(&docx(&body, ""));
        assert!(img.is_ok(), "{:?}", img.err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10) * crate::slack(),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn words_keep_the_spaces_between_runs_and_do_not_invent_them() {
        let font = Font::default();
        let mut atoms = Vec::new();
        words("Hello ", &font, &mut atoms);
        words("wor", &font, &mut atoms);
        words("ld", &font, &mut atoms);
        words(" again", &font, &mut atoms);
        let flags: Vec<(String, bool)> = atoms
            .iter()
            .filter_map(|a| match a {
                Atom::Word { text, space, .. } => Some((text.clone(), *space)),
                _ => None,
            })
            .collect();
        assert_eq!(
            flags,
            vec![
                ("Hello".into(), true),
                ("wor".into(), false),
                ("ld".into(), true),
                ("again".into(), false)
            ]
        );
    }

    // ---- read whole ----

    const NUMBERING: &str = r#"<?xml version="1.0"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
            <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/></w:lvl></w:abstractNum>
        <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val=""/></w:lvl></w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

    const STYLES: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:rPr><w:sz w:val="22"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/><w:rPr><w:sz w:val="56"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Chapter"><w:name w:val="My chapter"/><w:basedOn w:val="1"/></w:style>
        <w:style w:type="paragraph" w:styleId="Deep"><w:name w:val="Deep"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
        <w:style w:type="character" w:styleId="Code"><w:name w:val="Code"/><w:rPr><w:rFonts w:ascii="Consolas"/></w:rPr></w:style>"#;

    const FOOTNOTES: &str = r#"<?xml version="1.0"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>
        <w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:t>not a note</w:t></w:r></w:p></w:footnote>
        <w:footnote w:id="1"><w:p><w:r><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> The first footnote.</w:t></w:r></w:p></w:footnote>
        <w:footnote w:id="2"><w:p><w:r><w:t>A note never referred to.</w:t></w:r></w:p></w:footnote></w:footnotes>"#;

    const ENDNOTES: &str = r#"<?xml version="1.0"?><w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:endnote w:id="5"><w:p><w:pPr><w:pStyle w:val="1"/></w:pPr><w:r><w:t>An endnote set as a heading</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p></w:endnote></w:endnotes>"#;

    /// A docx of a body, the styles, the numbering and the notes above, and a picture `media/p.png` as `rId1`.)
    fn whole(body: &str) -> Vec<u8> {
        let document = format!(
            r#"<?xml version="1.0"?><w:document {W} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><w:body>{body}</w:body></w:document>"#
        );
        let styles = format!(r#"<?xml version="1.0"?><w:styles {W}>{STYLES}</w:styles>"#);
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="image" Target="media/p.png"/></Relationships>"#;
        let mut png = Vec::new();
        RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("a PNG");
        zip(&[
            ("word/document.xml", document.as_bytes(), true),
            ("word/styles.xml", styles.as_bytes(), true),
            ("word/numbering.xml", NUMBERING.as_bytes(), true),
            ("word/_rels/document.xml.rels", rels.as_bytes(), false),
            ("word/media/p.png", &png, false),
            ("word/footnotes.xml", FOOTNOTES.as_bytes(), true),
            ("word/endnotes.xml", ENDNOTES.as_bytes(), true),
        ])
    }

    fn para(style: &str, text: &str) -> String {
        format!(
            r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
        )
    }

    fn item(list: u32, level: u32, text: &str) -> String {
        format!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{list}"/></w:numPr></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
        )
    }

    fn texts(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { spans, .. } => Some(spans.iter().map(|s| s.text.as_str()).collect()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn code_is_known_by_its_font_and_a_serif_named_mono_something_is_not_code() {
        for family in [
            "Courier New",
            "Consolas",
            "Roboto Mono",
            "JetBrains Mono",
            "DejaVu Sans Mono",
        ] {
            assert!(is_mono(Some(family)), "{family}");
        }
        for family in ["Monomakh Unicode", "Times New Roman", "Calibri"] {
            assert!(!is_mono(Some(family)), "{family}");
        }
    }

    #[test]
    fn headings_by_name_id_base_and_outline_level_lead_the_contents() {
        let body = [
            para("Title", "The title"),
            para("1", "One"),
            para("Chapter", "Two"),
            para("Deep", "Two point one"),
            para("Normal", "Text."),
            para("Heading3", "Undescribed third"),
        ]
        .concat();
        let blocks = blocks_of(&whole(&body)).expect("reads");
        let marks: Vec<(usize, &str)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Mark { depth, title } => Some((*depth, title.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks,
            vec![(0, "One"), (0, "Two"), (1, "Two point one"), (2, "Undescribed third")]
        );
        let laid = book::lay_out(&blocks, usize::MAX);
        let titles: Vec<&str> = laid.contents.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, vec!["One", "Two", "Two point one", "Undescribed third"]);
        assert!(laid.pages[0].text.contains("The title"));
    }

    #[test]
    fn runs_keep_their_look_and_lists_get_their_marks() {
        let body = format!(
            r#"<w:p><w:r><w:t xml:space="preserve">Plain </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:rPr><w:i/><w:color w:val="C00000"/></w:rPr><w:t xml:space="preserve"> red</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:t>2</w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r><w:del><w:r><w:delText>gone</w:delText></w:r></w:del><w:hyperlink><w:r><w:t xml:space="preserve"> link</w:t></w:r></w:hyperlink></w:p>
            {}{}{}{}{}{}
            <w:p><w:r><w:rPr><w:rStyle w:val="Code"/></w:rPr><w:tab/><w:t>let x = 1;</w:t></w:r></w:p>"#,
            item(1, 0, "first"),
            item(1, 1, "inner"),
            item(1, 1, "inner again"),
            item(1, 0, "second"),
            item(2, 0, "a bullet"),
            item(7, 0, "a list the file does not describe"),
        );
        let blocks = blocks_of(&whole(&body)).expect("reads");
        let Some(Block::Paragraph { spans, .. }) = blocks.first() else {
            panic!("{blocks:?}");
        };
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "Plain bold red2 link");
        assert!(spans.iter().any(|s| s.text == "bold" && s.bold));
        assert!(
            spans
                .iter()
                .any(|s| s.text == " red" && s.italic && s.colour == Some([0xC0, 0, 0]))
        );
        assert!(spans.iter().any(|s| s.text == "2" && s.raised));
        assert_eq!(
            texts(&blocks)[1..7],
            [
                "1. first",
                "1.a) inner",
                "1.b) inner again",
                "2. second",
                "\u{2022} a bullet",
                "\u{2022} a list the file does not describe"
            ]
        );
        let Some(Block::Paragraph { spans, style }) = blocks.last() else {
            panic!("{blocks:?}");
        };
        assert!(style.code && spans.iter().all(|s| s.mono), "{spans:?}");
        assert!(
            spans[0].text.starts_with('\u{2007}'),
            "the tab kept as an indent: {spans:?}"
        );
    }

    #[test]
    fn a_table_keeps_its_spans_and_merges_and_a_picture_and_a_page_break_come_through() {
        let cell = |text: &str, extra: &str| {
            format!("<w:tc><w:tcPr>{extra}</w:tcPr><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>")
        };
        let table = format!(
            r#"<w:tbl><w:tr><w:trPr><w:tblHeader/></w:trPr>{}{}</w:tr><w:tr>{}{}{}</w:tr><w:tr>{}{}{}</w:tr></w:tbl>"#,
            cell("Name", ""),
            cell("Seasons", r#"<w:gridSpan w:val="2"/>"#),
            cell("Apples", r#"<w:vMerge w:val="restart"/>"#),
            cell("12", ""),
            cell("30", ""),
            cell("", "<w:vMerge/>"),
            cell("14", ""),
            cell("28", ""),
        );
        let picture = r#"<w:p><w:r><w:t>Before</w:t></w:r><w:r><w:drawing><wp:inline><wp:extent cx="381000" cy="285750"/><a:graphic><a:graphicData><a:blip r:embed="rId1"/></a:graphicData></a:graphic></wp:inline></w:drawing></w:r><w:r><w:t>after</w:t></w:r></w:p>"#;
        let body = format!(
            r#"{table}{picture}<w:p><w:r><w:t>On the first page</w:t></w:r><w:r><w:br w:type="page"/></w:r><w:r><w:t>On the second</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>A frame of one cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        );
        let blocks = blocks_of(&whole(&body)).expect("reads");
        let Some(Block::Table(table)) = blocks.first() else {
            panic!("{blocks:?}");
        };
        assert!(table.rows[0].head);
        assert_eq!(table.rows[0].cells[1].text, "Seasons");
        assert_eq!(table.rows[0].cells[1].columns, 2);
        assert_eq!(
            (table.rows[1].cells[0].text.as_str(), table.rows[1].cells[0].rows),
            ("Apples", 2)
        );
        let lower: Vec<&str> = table.rows[2].cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(lower, vec!["14", "28"], "the merged cell is not repeated");
        assert!(
            matches!(
                &blocks[1..4],
                [
                    Block::Paragraph { .. },
                    Block::Picture(Some(_)),
                    Block::Paragraph { .. }
                ]
            ),
            "{:?}",
            &blocks[1..4]
        );
        let at = blocks
            .iter()
            .position(|b| matches!(b, Block::NewPage))
            .expect("a new page");
        assert_eq!(
            texts(&blocks[..at]).last().map(String::as_str),
            Some("On the first page")
        );
        assert_eq!(
            texts(&blocks[at..]),
            vec!["On the second", "A frame of one cell"],
            "a table of one cell is its text"
        );
        let laid = book::lay_out(&blocks, usize::MAX);
        assert_eq!(laid.pages.len(), 2);
        assert!(laid.pages[1].text.contains("frame"));
    }

    #[test]
    fn notes_are_marked_raised_in_the_text_and_set_at_the_end_as_a_book_s() {
        let body = r#"<w:p><w:r><w:t>A sentence</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r><w:r><w:t xml:space="preserve"> and another</w:t></w:r><w:r><w:endnoteReference w:id="5"/></w:r><w:r><w:t>.</w:t></w:r></w:p>"#;
        let blocks = blocks_of(&whole(body)).expect("reads");
        let Some(Block::Paragraph { spans, .. }) = blocks.first() else {
            panic!("{blocks:?}");
        };
        let raised: Vec<&str> = spans.iter().filter(|s| s.raised).map(|s| s.text.as_str()).collect();
        assert_eq!(raised, vec!["1", "i"], "a footnote 1, an endnote i: {spans:?}");
        assert_eq!(
            spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            "A sentence1 and anotheri."
        );
        let marks: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Mark { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            marks,
            vec!["Notes", "Endnotes"],
            "a heading in a note is no line of the contents"
        );
        assert_eq!(
            texts(&blocks),
            vec![
                "A sentence1 and anotheri.",
                "1",
                " The first footnote.",
                "i",
                "An endnote set as a heading"
            ],
            "separators and notes never referred to are left out; a reference inside a note gives no mark"
        );
        assert_eq!(blocks.iter().filter(|b| matches!(b, Block::NewPage)).count(), 2);
        let laid = book::lay_out(&blocks, usize::MAX);
        assert_eq!(laid.pages.len(), 3);
        assert_eq!(laid.contents[0].page, Some(1));
    }

    #[test]
    fn text_boxes_formulas_and_content_controls_give_their_text_once() {
        let body = r#"<w:sdt><w:sdtContent><w:p><w:r><w:t>In a control</w:t></w:r></w:p></w:sdtContent></w:sdt>
            <w:p><w:r><mc:AlternateContent xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><mc:Choice><w:drawing><w:txbxContent><w:p><w:r><w:t>In a box</w:t></w:r></w:p></w:txbxContent></w:drawing></mc:Choice><mc:Fallback><w:pict><w:txbxContent><w:p><w:r><w:t>In a box</w:t></w:r></w:p></w:txbxContent></w:pict></mc:Fallback></mc:AlternateContent></w:r><w:r><w:t>Beside</w:t></w:r></w:p>
            <w:p><m:oMath><m:r><m:t>x+1</m:t></m:r></m:oMath></w:p>"#;
        let blocks = blocks_of(&whole(body)).expect("reads");
        assert_eq!(texts(&blocks), vec!["In a control", "Beside", "In a box", "x+1"]);
    }

    #[test]
    fn a_document_spoilt_empty_or_nested_too_deep_is_an_error_or_read_in_good_time() {
        assert!(blocks_of(b"not a zip").is_err());
        assert!(blocks_of(&zip(&[("word/other.xml", b"<x/>", false)])).is_err());
        assert!(blocks_of(&zip(&[("word/document.xml", b"<w:document><w:body>", false)])).is_err());
        assert!(blocks_of(&docx("<w:p/><w:p/>", "")).is_err(), "no text");
        let sample = whole(&para("Normal", "some text"));
        for cut in [10, 100, sample.len() / 2, sample.len() - 10] {
            assert!(blocks_of(&sample[..cut]).is_err(), "cut at {cut}");
        }
        let started = std::time::Instant::now();
        let mut body = String::from("<w:p><w:r><w:t>core</w:t></w:r></w:p>");
        for _ in 0..300 {
            body = format!(
                "<w:sdt><w:sdtContent><w:tbl><w:tr><w:tc>{body}<w:p/></w:tc><w:tc/></w:tr></w:tbl></w:sdtContent></w:sdt>"
            );
        }
        let read = draw_on_thread("test", "failed", || blocks_of(&whole(&body)));
        if let Err(e) = &read {
            assert!(!e.to_string().contains("failed"), "a panic: {e}");
        }
        let mut links = String::from("<w:r><w:t>deep</w:t></w:r>");
        for _ in 0..2000 {
            links = format!("<w:hyperlink>{links}</w:hyperlink>");
        }
        let body = format!("<w:p><w:r><w:t>x</w:t></w:r>{links}</w:p>");
        let read = draw_on_thread("test", "failed", || blocks_of(&whole(&body)));
        assert_eq!(read.map(|b| texts(&b)).ok(), Some(vec!["x".to_string()]));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10) * crate::slack(),
            "{:?}",
            started.elapsed()
        );
    }
}
