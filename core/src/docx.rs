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
use std::sync::Arc;

use image::RgbaImage;
use roxmltree::{Document, Node};

use crate::archive::Archive;
use crate::color::hex;
use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::office::{MAX_BYTES, MAX_PART, Pictures};
use crate::page::{Font, Page, Rgb, text_width};
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
const TAB: f32 = 36.0;
const CELL_PAD: f32 = 4.5;
const MAX_COLUMNS: usize = 64;

/// Whether the name says docx.
pub fn is_docx(path: &Path) -> bool {
    has_extension(path, "docx")
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

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Default, Debug)]
struct ParaProps {
    align: Option<Align>,
    before: Option<f32>,
    after: Option<f32>,
    line: Option<f32>,
    left: Option<f32>,
    first: Option<f32>,
}

impl ParaProps {
    fn over(&mut self, top: &ParaProps) {
        self.align = top.align.or(self.align);
        self.before = top.before.or(self.before);
        self.after = top.after.or(self.after);
        self.line = top.line.or(self.line);
        self.left = top.left.or(self.left);
        self.first = top.first.or(self.first);
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
}

impl RunProps {
    fn over(&mut self, top: &RunProps) {
        self.bold = top.bold.or(self.bold);
        self.italic = top.italic.or(self.italic);
        self.underline = top.underline.or(self.underline);
        self.size = top.size.or(self.size);
        self.colour = top.colour.or(self.colour);
        self.family = top.family.clone().or_else(|| self.family.take());
    }

    fn font(&self) -> Font {
        Font {
            size: self.size.unwrap_or(11.0).clamp(1.0, 400.0),
            bold: self.bold.unwrap_or(false),
            italic: self.italic.unwrap_or(false),
            underline: self.underline.unwrap_or(false),
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
    (
        ParaProps {
            align: child(ppr, "jc").and_then(|n| attr(n, "val")).map(|v| match v {
                "center" => Align::Center,
                "right" | "end" => Align::Right,
                _ => Align::Left,
            }),
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

/// A piece of a paragraph's content.
enum Atom {
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

    /// One paragraph: wrapped into lines and drawn from `y`; `y` is moved below it. False when the page is full.
    fn paragraph(&self, node: Node<'_, '_>, x: f32, width: f32, y: &mut f32, bottom: f32, out: &mut Page) -> bool {
        let (own, style, numbered) = para_props(node);
        let (mut props, run) = self.styles.resolve(style.as_deref());
        props.over(&own);
        let base_font = run.font();
        let mut atoms = Vec::new();
        self.atoms(node, &run, &mut atoms);

        let line_factor = props.line.unwrap_or(1.0);
        let mut left = props.left.unwrap_or(0.0).clamp(0.0, width * 0.8);
        if numbered && props.left.is_none() {
            left = 18.0;
        }
        let first = props
            .first
            .unwrap_or(if numbered { -12.0 } else { 0.0 })
            .clamp(-left, width * 0.5);
        let align = props.align.unwrap_or_default();
        *y += props.before.unwrap_or(0.0).clamp(0.0, 200.0);

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
            ..base_font.clone()
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
                .fold(base_font.size, f32::max);
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
            self.draw_line(line, &mut cursor, baseline, start, room, out);
            *y += height;
        }
        *y += props.after.unwrap_or(0.0).clamp(0.0, 200.0);
        fits && !page_break
    }

    /// The atoms of one line: the words between two tabs or pictures are one line of text made of pieces in their own
    /// fonts, which the renderer sets with the real widths (the widths here are only a guess, for wrapping).
    fn draw_line(&self, line: &[Atom], cursor: &mut f32, baseline: f32, start: f32, room: f32, out: &mut Page) {
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

/// The width of an atom; `with_space` counts the space after a word.
fn atom_width(atom: &Atom, with_space: bool) -> f32 {
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
fn words(text: &str, font: &Font, atoms: &mut Vec<Atom>) {
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
            started.elapsed() < std::time::Duration::from_secs(10),
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
}
