//! The first slide of a PowerPoint file (pptx) as a picture: a preview, not a rendering that matches PowerPoint.
//!
//! A pptx is a ZIP of XML. Read here: the size of the slide, its background (the slide's, then the layout's, then the
//! master's), the theme's colours (with the colour map and the usual lightening and darkening), shapes with their
//! position, fill and outline (also as the shape's theme style gives them), text with the sizes, bullets, alignment and
//! spacing a placeholder inherits from its layout and master, groups of shapes, pictures (PNG, JPEG, GIF, BMP, WebP,
//! TIFF) and simple tables.
//!
//! Not done: rotation and flips, shapes other than rectangles and ellipses (any other preset with a fill is drawn as a
//! rectangle, a custom geometry is not drawn), gradients (the first stop is taken), pictures cropped or used as fills,
//! charts and diagrams (a grey box stands in), table styles (only a cell's own fill), text effects, hyperlinks' colour,
//! footers, dates and slide numbers (left out), slides after the first, animations, notes.
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser refuses DTDs and has
//! limits (`xml`), the shapes read, the depth of groups, and the pixels of pictures are limited.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;

use image::RgbaImage;
use roxmltree::{Document, Node};

use crate::archive::Archive;
use crate::color::{from_hsl, hex, to_hsl};
use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::office::{MAX_BYTES, MAX_PART, Pictures};
use crate::page::{Font, Page, Rgb};
use crate::text::{self, Align, Para};
use crate::xml::{self, attr, child, children, descendant, parse};

/// Shapes drawn at most from the three layers together, groups inside groups, paragraphs of one shape.
const MAX_SHAPES: usize = 1000;
const MAX_GROUP_DEPTH: usize = 8;
const MAX_PARAGRAPHS: usize = 500;

const EMU: f32 = 12700.0;
/// The slide of a file that does not say: 16:9 as PowerPoint makes it.
const WIDE: (f32, f32) = (960.0, 540.0);

/// Whether the name says pptx.
pub fn is_pptx(path: &Path) -> bool {
    has_extension(path, "pptx")
}

/// Draws the first slide of the file in `path` on its background; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("pptx", "the presentation could not be drawn", || draw(&data))
}

/// The `r:id` of an element: the attribute called `id` that has a namespace (the plain `id` is the slide's own number).
fn relationship_id<'a>(node: Node<'a, '_>) -> Option<&'a str> {
    node.attributes()
        .find(|a| a.name() == "id" && a.namespace().is_some())
        .map(|a| a.value())
}

/// The text of a part, or `None` when it is not there.
fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    let presentation_text = archive
        .text("ppt/presentation.xml", MAX_PART)
        .ok_or_else(|| unreadable("this is no PowerPoint file (no ppt/presentation.xml)"))?;
    let presentation = parse(&presentation_text)?;
    let root = presentation.root_element();
    let (width, height) = child(root, "sldSz")
        .and_then(|s| Some((attr(s, "cx")?.parse::<f32>().ok()?, attr(s, "cy")?.parse::<f32>().ok()?)))
        .map_or(WIDE, |(w, h)| (w / EMU, h / EMU));
    let (width, height) = (width.clamp(100.0, 3000.0), height.clamp(100.0, 3000.0));

    // the first slide: the first of the list of slides, or slide1
    let rels = xml::relationships(&archive, "ppt/presentation.xml");
    let slide_name = child(root, "sldIdLst")
        .and_then(|l| child(l, "sldId"))
        .and_then(relationship_id)
        .and_then(|id| rels.get(id).map(|(_, p)| p.clone()))
        .or_else(|| {
            rels.values()
                .filter(|(kind, _)| kind.ends_with("/slide"))
                .map(|(_, p)| p.clone())
                .min()
        })
        .unwrap_or_else(|| "ppt/slides/slide1.xml".to_string());
    let slide_text = archive
        .text(&slide_name, MAX_PART)
        .ok_or_else(|| unreadable("the presentation has no slide"))?;
    let slide = parse(&slide_text)?;

    let slide_rels = xml::relationships(&archive, &slide_name);
    let find = |rels: &HashMap<String, (String, String)>, suffix: &str| {
        rels.values()
            .find(|(kind, _)| kind.ends_with(suffix))
            .map(|(_, path)| path.clone())
    };
    let layout_name = find(&slide_rels, "/slideLayout");
    let layout_text = layout_name.as_deref().and_then(|n| archive.text(n, MAX_PART));
    let layout = layout_text.as_deref().and_then(|t| parse(t).ok());
    let master_name = layout_name
        .as_deref()
        .and_then(|n| find(&xml::relationships(&archive, n), "/slideMaster"));
    let master_text = master_name.as_deref().and_then(|n| archive.text(n, MAX_PART));
    let master = master_text.as_deref().and_then(|t| parse(t).ok());
    let theme_name = master_name
        .as_deref()
        .and_then(|n| find(&xml::relationships(&archive, n), "/theme"))
        .unwrap_or_else(|| "ppt/theme/theme1.xml".to_string());
    let theme_text = archive.text(&theme_name, MAX_PART);
    let theme_xml = theme_text.as_deref().and_then(|t| parse(t).ok());

    let theme = Theme::read(theme_xml.as_ref(), master.as_ref());
    let layers = Layers {
        slide: &slide,
        layout: layout.as_ref(),
        master: master.as_ref(),
    };
    let ctx = Ctx {
        theme,
        layers,
        rels: slide_rels,
        layout_rels: layout_name
            .as_deref()
            .map(|n| xml::relationships(&archive, n))
            .unwrap_or_default(),
        master_rels: master_name
            .as_deref()
            .map(|n| xml::relationships(&archive, n))
            .unwrap_or_default(),
        pictures: Pictures::new(&archive),
        shapes: Cell::new(0),
    };
    let mut page = Page::new(width, height);
    let background = ctx.background().unwrap_or([255, 255, 255]);
    page.rect(0.0, 0.0, width, height, Some(background), None);
    let base = Transform::slide();
    // the master's and the layout's own pictures and shapes behind, the slide's on top
    for (tree, rels) in [
        (ctx.layers.master, &ctx.master_rels),
        (ctx.layers.layout, &ctx.layout_rels),
        (Some(ctx.layers.slide), &ctx.rels),
    ] {
        let Some(doc) = tree else { continue };
        let Some(shapes) = descendant(doc.root_element(), "spTree") else {
            continue;
        };
        let is_slide = std::ptr::eq(doc, ctx.layers.slide);
        ctx.shapes_in(shapes, base, rels, is_slide, 0, &mut page);
    }
    page.render()
}

// ---- colours ----------------------------------------------------------------------------------------------------

struct Theme {
    /// Theme colour names (`dk1`, `lt1`, `accent1`, ...) to colours.
    colours: HashMap<String, Rgb>,
    /// What the names a slide uses (`bg1`, `tx1`, ...) stand for.
    map: HashMap<String, String>,
}

impl Theme {
    fn read(theme: Option<&Document<'_>>, master: Option<&Document<'_>>) -> Self {
        let mut colours = HashMap::new();
        if let Some(scheme) = theme.and_then(|t| descendant(t.root_element(), "clrScheme")) {
            for entry in scheme.children().filter(|c| c.is_element()) {
                let colour = child(entry, "srgbClr")
                    .and_then(|c| hex(attr(c, "val")))
                    .or_else(|| child(entry, "sysClr").and_then(|c| hex(attr(c, "lastClr"))));
                if let Some(colour) = colour {
                    colours.insert(entry.tag_name().name().to_string(), colour);
                }
            }
        }
        let mut map: HashMap<String, String> = [("bg1", "lt1"), ("tx1", "dk1"), ("bg2", "lt2"), ("tx2", "dk2")]
            .iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect();
        if let Some(clr_map) = master.and_then(|m| descendant(m.root_element(), "clrMap")) {
            for a in clr_map.attributes() {
                map.insert(a.name().to_string(), a.value().to_string());
            }
        }
        Self { colours, map }
    }

    /// The colour in an element that holds one (`solidFill`, `bgPr`'s fill, a `fillRef`): srgb, theme, system or preset,
    /// with the lightening and darkening it asks for.
    fn colour(&self, holder: Node<'_, '_>) -> Option<Rgb> {
        let entry = holder.children().find(|c| {
            c.is_element() && matches!(c.tag_name().name(), "srgbClr" | "schemeClr" | "sysClr" | "prstClr")
        })?;
        let base = match entry.tag_name().name() {
            "srgbClr" => hex(attr(entry, "val"))?,
            "sysClr" => hex(attr(entry, "lastClr")).or_else(|| match attr(entry, "val") {
                Some("windowText") => Some([0, 0, 0]),
                _ => Some([255, 255, 255]),
            })?,
            "prstClr" => match attr(entry, "val")? {
                "black" => [0, 0, 0],
                "white" => [255, 255, 255],
                "red" => [255, 0, 0],
                "green" => [0, 128, 0],
                "blue" => [0, 0, 255],
                "yellow" => [255, 255, 0],
                "gray" | "grey" => [128, 128, 128],
                _ => [0, 0, 0],
            },
            _ => {
                let name = attr(entry, "val")?;
                let name = self.map.get(name).map_or(name, String::as_str);
                *self.colours.get(name)?
            }
        };
        let share = |name: &str| {
            child(entry, name)
                .and_then(|m| attr(m, "val"))
                .and_then(|v| v.parse::<f32>().ok())
                .map(|v| v / 100_000.0)
        };
        let (mut h, mut s, mut l) = to_hsl(base);
        if let Some(m) = share("lumMod") {
            l *= m;
        }
        if let Some(o) = share("lumOff") {
            l += o;
        }
        if let Some(m) = share("satMod") {
            s *= m;
        }
        h = h.rem_euclid(360.0);
        let mut out = from_hsl(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
        if let Some(t) = share("tint") {
            out = out.map(|c| (255.0 - (255.0 - f32::from(c)) * t).clamp(0.0, 255.0) as u8);
        }
        if let Some(sh) = share("shade") {
            out = out.map(|c| (f32::from(c) * sh).clamp(0.0, 255.0) as u8);
        }
        Some(out)
    }

    /// The colour of a fill element (`solidFill`; a gradient's first stop), or `None` for no fill or none given.
    fn fill(&self, properties: Node<'_, '_>) -> Option<Option<Rgb>> {
        if child(properties, "noFill").is_some() {
            return Some(None);
        }
        if let Some(solid) = child(properties, "solidFill") {
            return Some(self.colour(solid));
        }
        if let Some(gradient) = child(properties, "gradFill") {
            let stop = child(gradient, "gsLst").and_then(|l| child(l, "gs"));
            return Some(stop.and_then(|s| self.colour(s)));
        }
        None
    }
}

// ---- positions --------------------------------------------------------------------------------------------------

/// From the coordinates of a shape (EMU, in the space of its group) to points on the slide.
#[derive(Clone, Copy, Debug)]
struct Transform {
    sx: f32,
    sy: f32,
    dx: f32,
    dy: f32,
}

impl Transform {
    fn slide() -> Self {
        Self {
            sx: 1.0 / EMU,
            sy: 1.0 / EMU,
            dx: 0.0,
            dy: 0.0,
        }
    }

    /// The transform for the children of a group whose own box is `off`/`ext` and whose children are placed as
    /// `child_off`/`child_ext` say.
    fn inside(self, off: (f32, f32), ext: (f32, f32), child_off: (f32, f32), child_ext: (f32, f32)) -> Self {
        let (kx, ky) = (
            if child_ext.0 > 0.0 { ext.0 / child_ext.0 } else { 1.0 },
            if child_ext.1 > 0.0 { ext.1 / child_ext.1 } else { 1.0 },
        );
        Self {
            sx: self.sx * kx,
            sy: self.sy * ky,
            dx: self.sx * (off.0 - child_off.0 * kx) + self.dx,
            dy: self.sy * (off.1 - child_off.1 * ky) + self.dy,
        }
    }

    /// A box in points: (x, y, width, height).
    fn rect(self, off: (f32, f32), ext: (f32, f32)) -> (f32, f32, f32, f32) {
        (
            off.0 * self.sx + self.dx,
            off.1 * self.sy + self.dy,
            ext.0 * self.sx,
            ext.1 * self.sy,
        )
    }
}

/// `a:xfrm` (or `p:xfrm`): the offset and the extent in EMU.
fn xfrm(properties: Node<'_, '_>) -> Option<((f32, f32), (f32, f32))> {
    let x = child(properties, "xfrm")?;
    let pair = |name: &str, a: &str, b: &str| {
        let n = child(x, name)?;
        Some((attr(n, a)?.parse::<f32>().ok()?, attr(n, b)?.parse::<f32>().ok()?))
    };
    Some((pair("off", "x", "y")?, pair("ext", "cx", "cy")?))
}

// ---- inheritance of text properties -----------------------------------------------------------------------------

/// What a level of a list style (or a paragraph) says: all of it optional, so that levels can be laid over one another.
#[derive(Clone, Default, Debug)]
struct Level {
    align: Option<Align>,
    left: Option<f32>,
    indent: Option<f32>,
    /// `Some(None)`: no bullet; `Some(Some(c))`: this bullet.
    bullet: Option<Option<String>>,
    before: Option<Spacing>,
    after: Option<Spacing>,
    line: Option<Spacing>,
    size: Option<f32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    colour: Option<Rgb>,
    family: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum Spacing {
    Points(f32),
    Share(f32),
}

impl Level {
    fn over(&mut self, top: &Level) {
        macro_rules! take {
            ($($field:ident),*) => { $( if top.$field.is_some() { self.$field = top.$field.clone(); } )* };
        }
        take!(
            align, left, indent, bullet, before, after, line, size, bold, italic, underline, colour, family
        );
    }
}

fn spacing(node: Option<Node<'_, '_>>) -> Option<Spacing> {
    let node = node?;
    if let Some(p) = child(node, "spcPts")
        .and_then(|n| attr(n, "val"))
        .and_then(|v| v.parse::<f32>().ok())
    {
        return Some(Spacing::Points(p / 100.0));
    }
    child(node, "spcPct")
        .and_then(|n| attr(n, "val"))
        .and_then(|v| v.parse::<f32>().ok())
        .map(|p| Spacing::Share(p / 100_000.0))
}

/// The properties in a run's `rPr` or a level's `defRPr`.
fn read_run(rpr: Node<'_, '_>, theme: &Theme, into: &mut Level) {
    into.size = attr(rpr, "sz")
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v / 100.0)
        .or(into.size);
    let flag = |name: &str| attr(rpr, name).map(|v| matches!(v, "1" | "true"));
    into.bold = flag("b").or(into.bold);
    into.italic = flag("i").or(into.italic);
    into.underline = attr(rpr, "u").map(|v| v != "none").or(into.underline);
    if let Some(fill) = theme.fill(rpr) {
        into.colour = fill.or(into.colour);
    }
    if let Some(face) = child(rpr, "latin").and_then(|n| attr(n, "typeface")) {
        // theme fonts (`+mj-lt`) are not resolved
        if !face.starts_with('+') {
            into.family = Some(face.to_string());
        }
    }
}

/// The properties of a paragraph or of a level of a list style (`pPr`, `lvl1pPr`), with its default run properties.
fn read_level(node: Node<'_, '_>, theme: &Theme) -> Level {
    let mut level = Level {
        align: attr(node, "algn").map(|v| match v {
            "ctr" => Align::Center,
            "r" => Align::Right,
            _ => Align::Left,
        }),
        ..Level::default()
    };
    let emu = |name: &str| attr(node, name).and_then(|v| v.parse::<f32>().ok()).map(|v| v / EMU);
    level.left = emu("marL");
    level.indent = emu("indent");
    if child(node, "buNone").is_some() {
        level.bullet = Some(None);
    } else if let Some(c) = child(node, "buChar") {
        level.bullet = Some(Some(attr(c, "char").unwrap_or("\u{2022}").to_string()));
    } else if child(node, "buAutoNum").is_some() {
        level.bullet = Some(Some("\u{2022}".to_string()));
    }
    level.before = spacing(child(node, "spcBef"));
    level.after = spacing(child(node, "spcAft"));
    level.line = spacing(child(node, "lnSpc"));
    if let Some(def) = child(node, "defRPr") {
        read_run(def, theme, &mut level);
    }
    level
}

// ---- the three layers -------------------------------------------------------------------------------------------

struct Layers<'a> {
    slide: &'a Document<'a>,
    layout: Option<&'a Document<'a>>,
    master: Option<&'a Document<'a>>,
}

/// The placeholder a shape stands for: its kind (`title`, `body`, ...) and index.
struct Holder {
    kind: Option<String>,
    index: Option<String>,
}

fn holder(shape: Node<'_, '_>) -> Option<Holder> {
    let ph = child(child(shape, "nvSpPr")?, "nvPr").and_then(|p| child(p, "ph"))?;
    Some(Holder {
        kind: attr(ph, "type").map(str::to_string),
        index: attr(ph, "idx").map(str::to_string),
    })
}

/// What kind of placeholder this is, where a title slide's `ctrTitle` is a title and a `subTitle` a body.
fn kind_of(holder: &Holder) -> &str {
    match holder.kind.as_deref() {
        Some("ctrTitle") => "title",
        Some("subTitle") | None => "body",
        Some(kind) => kind,
    }
}

/// The shape of a layer's tree that the placeholder `ph` stands for: by index when it has one, else by kind.
fn counterpart<'a, 'i>(doc: Option<&'a Document<'i>>, ph: &Holder) -> Option<Node<'a, 'i>> {
    let tree = descendant(doc?.root_element(), "spTree")?;
    let same_kind = |other: &Holder| kind_of(other) == kind_of(ph);
    let shapes = || children(tree, "sp").filter_map(|s| holder(s).map(|h| (s, h)));
    ph.index
        .as_deref()
        .and_then(|idx| shapes().find(|(_, h)| h.index.as_deref() == Some(idx)))
        .or_else(|| shapes().find(|(_, h)| same_kind(h)))
        .map(|(s, _)| s)
}

struct Ctx<'a> {
    theme: Theme,
    layers: Layers<'a>,
    rels: HashMap<String, (String, String)>,
    layout_rels: HashMap<String, (String, String)>,
    master_rels: HashMap<String, (String, String)>,
    pictures: Pictures<'a>,
    shapes: Cell<usize>,
}

impl Ctx<'_> {
    /// The background colour: the slide's own, else the layout's, else the master's.
    fn background(&self) -> Option<Rgb> {
        [Some(self.layers.slide), self.layers.layout, self.layers.master]
            .into_iter()
            .flatten()
            .find_map(|doc| {
                let bg = descendant(doc.root_element(), "bg")?;
                if let Some(properties) = child(bg, "bgPr") {
                    return self.theme.fill(properties).flatten();
                }
                // a reference to the theme's background fills: the colour it names
                child(bg, "bgRef").and_then(|r| self.theme.colour(r))
            })
    }

    /// The shapes of a tree (the slide's, the layout's or the master's): `on_slide` is false for the two others, where
    /// the placeholders (they only show what the slide's own are to look like) are not drawn.
    fn shapes_in(
        &self,
        tree: Node<'_, '_>,
        transform: Transform,
        rels: &HashMap<String, (String, String)>,
        on_slide: bool,
        depth: usize,
        page: &mut Page,
    ) {
        for shape in tree.children().filter(|c| c.is_element()) {
            if self.shapes.get() >= MAX_SHAPES {
                return;
            }
            match shape.tag_name().name() {
                "sp" | "cxnSp" => {
                    self.shapes.set(self.shapes.get() + 1);
                    if on_slide || holder(shape).is_none() {
                        self.shape(shape, transform, on_slide, page);
                    }
                }
                "pic" => {
                    self.shapes.set(self.shapes.get() + 1);
                    self.pic(shape, transform, rels, page);
                }
                "graphicFrame" => {
                    self.shapes.set(self.shapes.get() + 1);
                    if on_slide {
                        self.frame(shape, transform, page);
                    }
                }
                "grpSp" if depth < MAX_GROUP_DEPTH => {
                    let Some(properties) = child(shape, "grpSpPr") else {
                        continue;
                    };
                    let inner = match (
                        xfrm(properties),
                        child(properties, "xfrm").and_then(|x| {
                            let pair = |name: &str, a: &str, b: &str| {
                                let n = child(x, name)?;
                                Some((attr(n, a)?.parse::<f32>().ok()?, attr(n, b)?.parse::<f32>().ok()?))
                            };
                            Some((pair("chOff", "x", "y")?, pair("chExt", "cx", "cy")?))
                        }),
                    ) {
                        (Some((off, ext)), Some((child_off, child_ext))) => {
                            transform.inside(off, ext, child_off, child_ext)
                        }
                        _ => transform,
                    };
                    self.shapes_in(shape, inner, rels, on_slide, depth + 1, page);
                }
                _ => {}
            }
        }
    }

    /// The box of a shape in EMU: its own, or what the placeholder it stands for has in the layout or the master.
    fn place(&self, shape: Node<'_, '_>) -> Option<((f32, f32), (f32, f32))> {
        if let Some(own) = child(shape, "spPr").and_then(xfrm) {
            return Some(own);
        }
        let ph = holder(shape)?;
        [self.layers.layout, self.layers.master]
            .into_iter()
            .find_map(|layer| counterpart(layer, &ph).and_then(|s| child(s, "spPr")).and_then(xfrm))
    }

    fn shape(&self, shape: Node<'_, '_>, transform: Transform, on_slide: bool, page: &mut Page) {
        let ph = holder(shape);
        if let Some(kind) = ph.as_ref().and_then(|p| p.kind.as_deref())
            && matches!(kind, "dt" | "ftr" | "sldNum")
        {
            return;
        }
        let Some((off, ext)) = self.place(shape) else {
            return;
        };
        let (x, y, w, h) = transform.rect(off, ext);
        if !(w.is_finite() && h.is_finite()) || w <= 0.0 && h <= 0.0 {
            return;
        }
        let properties = child(shape, "spPr");
        let style = child(shape, "style");
        let geometry = properties
            .and_then(|p| child(p, "prstGeom"))
            .and_then(|g| attr(g, "prst"))
            .unwrap_or("rect");
        let custom = properties.is_some_and(|p| child(p, "custGeom").is_some());

        // fill: its own, else the theme style's reference
        let fill = properties.and_then(|p| self.theme.fill(p)).unwrap_or_else(|| {
            style
                .and_then(|s| child(s, "fillRef"))
                .filter(|r| attr(*r, "idx").is_some_and(|i| i != "0"))
                .and_then(|r| self.theme.colour(r))
        });
        let outline = properties.and_then(|p| child(p, "ln"));
        let stroke_colour = match outline {
            Some(ln) if child(ln, "noFill").is_some() => None,
            Some(ln) => self.theme.fill(ln).flatten().or_else(|| {
                style
                    .and_then(|s| child(s, "lnRef"))
                    .filter(|r| attr(*r, "idx").is_some_and(|i| i != "0"))
                    .and_then(|r| self.theme.colour(r))
            }),
            None => style
                .and_then(|s| child(s, "lnRef"))
                .filter(|r| attr(*r, "idx").is_some_and(|i| i != "0"))
                .and_then(|r| self.theme.colour(r)),
        };
        let stroke_width = outline
            .and_then(|l| attr(l, "w"))
            .and_then(|v| v.parse::<f32>().ok())
            .map_or(1.0, |v| (v / EMU).clamp(0.25, 12.0));
        let stroke = stroke_colour.map(|c| (c, stroke_width));
        if matches!(geometry, "line" | "straightConnector1") || shape.tag_name().name() == "cxnSp" {
            if let Some((colour, width)) = stroke {
                page.line((x, y), (x + w, y + h), colour, width);
            }
        } else if !custom && (fill.is_some() || stroke.is_some()) {
            if geometry == "ellipse" {
                page.ellipse(x, y, w, h, fill, stroke);
            } else {
                page.rect(x, y, w, h, fill, stroke);
            }
        }

        // text
        let Some(body) = child(shape, "txBody") else {
            return;
        };
        let default_colour = style
            .and_then(|s| child(s, "fontRef"))
            .and_then(|r| self.theme.colour(r));
        let (paras, insets, anchor) = self.paragraphs(shape, body, ph.as_ref(), on_slide, default_colour);
        if paras.is_empty() {
            return;
        }
        let (left, top, right, bottom) = insets;
        let width = (w - left - right).max(10.0);
        let used = text::measure(&paras, width);
        let room = h - top - bottom;
        let start = y
            + top
            + match anchor.as_str() {
                "ctr" => ((room - used) / 2.0).max(0.0),
                "b" => (room - used).max(0.0),
                _ => 0.0,
            };
        text::layout(&paras, x + left, start, width, page.height, page);
    }

    /// The paragraphs of a text body with every property settled; the insets of the box and where the text sits in it.
    fn paragraphs(
        &self,
        shape: Node<'_, '_>,
        body: Node<'_, '_>,
        ph: Option<&Holder>,
        on_slide: bool,
        default_colour: Option<Rgb>,
    ) -> (Vec<Para>, (f32, f32, f32, f32), String) {
        let body_pr = child(body, "bodyPr");
        let inset = |name: &str, default: f32| {
            body_pr
                .and_then(|b| attr(b, name))
                .and_then(|v| v.parse::<f32>().ok())
                .map_or(default, |v| v / EMU)
        };
        let insets = (
            inset("lIns", 7.2),
            inset("tIns", 3.6),
            inset("rIns", 7.2),
            inset("bIns", 3.6),
        );
        let anchor = body_pr.and_then(|b| attr(b, "anchor")).unwrap_or("t").to_string();
        let scale = body_pr
            .and_then(|b| child(b, "normAutofit"))
            .and_then(|n| attr(n, "fontScale"))
            .and_then(|v| v.parse::<f32>().ok())
            .map_or(1.0, |v| (v / 100_000.0).clamp(0.2, 1.0));

        // the list styles that apply, the most specific first
        let mut sources: Vec<Node<'_, '_>> = Vec::new();
        if let Some(own) = child(body, "lstStyle") {
            sources.push(own);
        }
        if on_slide && let Some(ph) = ph {
            for layer in [self.layers.layout, self.layers.master] {
                if let Some(s) = counterpart(layer, ph)
                    .and_then(|s| child(s, "txBody"))
                    .and_then(|b| child(b, "lstStyle"))
                {
                    sources.push(s);
                }
            }
        }
        if let Some(styles) = self
            .layers
            .master
            .and_then(|m| descendant(m.root_element(), "txStyles"))
        {
            let name = match ph.map(|p| p.kind.as_deref()) {
                Some(Some("title" | "ctrTitle")) => "titleStyle",
                Some(_) => "bodyStyle",
                None => "otherStyle",
            };
            if let Some(s) = child(styles, name) {
                sources.push(s);
            }
        }
        let _ = shape;

        let mut paras = Vec::new();
        for p in children(body, "p").take(MAX_PARAGRAPHS) {
            let ppr = child(p, "pPr");
            let lvl = ppr
                .and_then(|n| attr(n, "lvl"))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0)
                .min(8);
            let level_name = format!("lvl{}pPr", lvl + 1);
            let mut level = Level::default();
            for source in sources.iter().rev() {
                if let Some(n) = child(*source, &level_name) {
                    level.over(&read_level(n, &self.theme));
                }
            }
            if let Some(n) = ppr {
                level.over(&read_level(n, &self.theme));
            }
            let mut base = level.clone();
            if base.colour.is_none() {
                base.colour = default_colour;
            }
            let font_of = |run_level: &Level| {
                let mut merged = base.clone();
                merged.over(run_level);
                Font {
                    size: (merged.size.unwrap_or(18.0) * scale).clamp(1.0, 400.0),
                    bold: merged.bold.unwrap_or(false),
                    italic: merged.italic.unwrap_or(false),
                    underline: merged.underline.unwrap_or(false),
                    colour: merged.colour.unwrap_or([0, 0, 0]),
                    family: merged.family.clone(),
                }
            };
            let mut runs: Vec<(String, Font)> = Vec::new();
            for item in p.children().filter(|c| c.is_element()) {
                match item.tag_name().name() {
                    "r" | "fld" => {
                        let mut own = Level::default();
                        if let Some(rpr) = child(item, "rPr") {
                            read_run(rpr, &self.theme, &mut own);
                        }
                        let text = child(item, "t").and_then(|t| t.text()).unwrap_or("");
                        runs.push((text.to_string(), font_of(&own)));
                    }
                    "br" => runs.push(("\n".to_string(), font_of(&Level::default()))),
                    _ => {}
                }
            }
            let empty_size = font_of(&Level::default()).size;
            let first_font = runs
                .first()
                .map(|(_, f)| f.clone())
                .unwrap_or_else(|| font_of(&Level::default()));
            let has_text = runs.iter().any(|(t, _)| !t.trim().is_empty());
            let bullet_char = level.bullet.clone().flatten();
            let bullet = match (&level.bullet, has_text) {
                (Some(Some(_)), true) => Some(Font {
                    bold: false,
                    underline: false,
                    ..first_font
                }),
                _ => None,
            };
            let points = |s: Option<Spacing>, size: f32| match s {
                Some(Spacing::Points(p)) => p,
                Some(Spacing::Share(f)) => f * size,
                None => 0.0,
            };
            let line = match level.line {
                Some(Spacing::Share(f)) => f.clamp(0.3, 4.0),
                Some(Spacing::Points(p)) => (p / (empty_size * 1.2)).clamp(0.3, 4.0),
                None => 1.0,
            };
            let left = level.left.unwrap_or(0.0).max(0.0);
            let indent = level.indent.unwrap_or(0.0);
            // with a bullet the text starts at the left margin and the bullet stands `indent` before it; without one the
            // first line starts `indent` from the margin
            let (first, bullet_offset) = if bullet.is_some() {
                (0.0, Some(indent.max(-left)))
            } else {
                (indent.max(-left), None)
            };
            paras.push(Para {
                runs,
                align: level.align.unwrap_or_default(),
                left,
                first,
                before: points(level.before, empty_size),
                after: points(level.after, empty_size),
                line,
                bullet,
                bullet_offset,
                bullet_char,
                empty_size,
            });
        }
        (paras, insets, anchor)
    }

    fn pic(
        &self,
        shape: Node<'_, '_>,
        transform: Transform,
        rels: &HashMap<String, (String, String)>,
        page: &mut Page,
    ) {
        let Some((off, ext)) = child(shape, "spPr").and_then(xfrm) else {
            return;
        };
        let (x, y, w, h) = transform.rect(off, ext);
        if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
            return;
        }
        let image = child(shape, "blipFill")
            .and_then(|f| child(f, "blip"))
            .and_then(|b| attr(b, "embed"))
            .and_then(|id| rels.get(id))
            .and_then(|(_, path)| self.pictures.get(path));
        match image {
            Some(image) => page.picture(x, y, w, h, image),
            None => page.rect(x, y, w, h, Some([235, 235, 235]), Some(([170, 170, 170], 0.75))),
        }
    }

    /// A table, simply: columns and rows as written, a cell's own fill, thin borders, the text of the cells. Anything else
    /// in a frame (a chart, a diagram) is a grey box.
    fn frame(&self, shape: Node<'_, '_>, transform: Transform, page: &mut Page) {
        let Some((off, ext)) = xfrm_of_frame(shape) else {
            return;
        };
        let (x, y, w, h) = transform.rect(off, ext);
        if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
            return;
        }
        let Some(table) = descendant(shape, "tbl") else {
            page.rect(x, y, w, h, Some([240, 240, 240]), Some(([190, 190, 190], 0.75)));
            return;
        };
        let columns: Vec<f32> = child(table, "tblGrid")
            .map(|g| {
                children(g, "gridCol")
                    .take(64)
                    .map(|c| attr(c, "w").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) * transform.sx)
                    .collect()
            })
            .unwrap_or_default();
        let mut top = y;
        for row in children(table, "tr").take(500) {
            let height = attr(row, "h").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) * transform.sy;
            let mut left = x;
            let mut tallest = height.max(8.0);
            let mut cells = Vec::new();
            for (index, cell) in children(row, "tc").take(64).enumerate() {
                let width = columns.get(index).copied().unwrap_or(40.0).max(4.0);
                let fill = child(cell, "tcPr").and_then(|p| self.theme.fill(p)).flatten();
                let paras = child(cell, "txBody")
                    .map(|b| self.paragraphs(cell, b, None, false, None).0)
                    .unwrap_or_default();
                let used = text::measure(&paras, (width - 14.4).max(10.0)) + 7.2;
                tallest = tallest.max(used);
                cells.push((left, width, fill, paras));
                left += width;
            }
            for (cell_x, width, fill, paras) in &cells {
                page.rect(*cell_x, top, *width, tallest, *fill, Some(([180, 180, 180], 0.5)));
                text::layout(
                    paras,
                    cell_x + 7.2,
                    top + 3.6,
                    (width - 14.4).max(10.0),
                    page.height,
                    page,
                );
            }
            top += tallest;
            if top > page.height {
                break;
            }
        }
    }
}

/// `p:xfrm` of a graphic frame: the same shape as `a:xfrm`, a child of the frame itself.
fn xfrm_of_frame(frame: Node<'_, '_>) -> Option<((f32, f32), (f32, f32))> {
    xfrm(frame)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    /// A presentation of one slide with this shape tree, a layout with a title placeholder at the top, and a master.
    fn deck(slide_shapes: &str, slide_extra: &str) -> Vec<u8> {
        let presentation = format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="5143500"/></p:presentation>"#
        );
        let rels = |target: &str, kind: &str| {
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/></Relationships>"#
            )
        };
        let slide = format!(r#"<p:sld {NS}><p:cSld>{slide_extra}<p:spTree>{slide_shapes}</p:spTree></p:cSld></p:sld>"#);
        let layout = format!(
            r#"<p:sldLayout {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="228600"/><a:ext cx="8229600" cy="857250"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#
        );
        let master = format!(
            r#"<p:sldMaster {NS}><p:cSld><p:bg><p:bgPr><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></p:bgPr></p:bg><p:spTree/></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2"/><p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="3200" b="1"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL="342900" indent="-342900"><a:buChar char="&#8226;"/><a:defRPr sz="2000"/></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr><a:defRPr sz="1800"/></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#
        );
        let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="t"><a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F3864"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2></a:clrScheme></a:themeElements></a:theme>"#;
        zip(&[
            ("ppt/presentation.xml", presentation.as_bytes(), true),
            (
                "ppt/_rels/presentation.xml.rels",
                rels("slides/slide1.xml", "slide").as_bytes(),
                true,
            ),
            ("ppt/slides/slide1.xml", slide.as_bytes(), true),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                rels("../slideLayouts/slideLayout1.xml", "slideLayout").as_bytes(),
                true,
            ),
            ("ppt/slideLayouts/slideLayout1.xml", layout.as_bytes(), true),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                rels("../slideMasters/slideMaster1.xml", "slideMaster").as_bytes(),
                true,
            ),
            ("ppt/slideMasters/slideMaster1.xml", master.as_bytes(), true),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels",
                rels("../theme/theme1.xml", "theme").as_bytes(),
                true,
            ),
            ("ppt/theme/theme1.xml", theme.as_bytes(), true),
        ])
    }

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
    fn a_title_that_takes_its_place_and_size_from_the_layout_and_the_master_is_drawn_at_the_top() {
        let title = r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>A title from the layout</a:t></a:r></a:p></p:txBody></p:sp>"#;
        let img = draw(&deck(title, "")).expect("draws");
        // 16:9 at 2048 across
        assert_eq!(img.width(), 2048);
        assert!((img.height() as f32 / 2048.0 - 0.5625).abs() < 0.01, "{}", img.height());
        assert!(
            dark(&img, (50, 50, 900, 130), 100) > 1500,
            "the title (32 pt bold) near the top"
        );
        assert_eq!(dark(&img, (0, 400, 1000, 600), 200), 0, "nothing lower");
    }

    #[test]
    fn a_shape_with_a_theme_fill_and_a_lighter_shade_and_a_background_are_drawn() {
        let shapes = r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Box"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="914400" y="1828800"/><a:ext cx="2743200" cy="1371600"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:schemeClr val="accent2"/></a:solidFill></p:spPr></p:sp>
            <p:sp><p:nvSpPr><p:cNvPr id="4" name="Light"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="4572000" y="1828800"/><a:ext cx="2743200" cy="1371600"/></a:xfrm><a:solidFill><a:schemeClr val="accent1"><a:lumMod val="40000"/><a:lumOff val="60000"/></a:schemeClr></a:solidFill></p:spPr></p:sp>"#;
        let background = r#"<p:bg><p:bgPr><a:solidFill><a:srgbClr val="102030"/></a:solidFill></p:bgPr></p:bg>"#;
        let img = draw(&deck(shapes, background)).expect("draws");
        let at = |x: f32, y: f32| {
            img.get_pixel((img.width() as f32 * x) as u32, (img.height() as f32 * y) as u32)
                .0
        };
        assert_eq!(&at(0.02, 0.02)[..3], &[0x10, 0x20, 0x30], "the slide's own background");
        let [r, g, b, _] = at(0.2, 0.45);
        assert!(
            r > 220 && (100..160).contains(&g) && b < 80,
            "accent2 orange: {r} {g} {b}"
        );
        let [r, g, b, _] = at(0.6, 0.45);
        assert!(r > 150 && g > 170 && b > 220, "accent1 made lighter: {r} {g} {b}");
    }

    #[test]
    fn a_group_moves_and_scales_what_is_in_it() {
        let group = r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="5" name="G"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="4572000" y="2571750"/><a:ext cx="2286000" cy="1285875"/><a:chOff x="0" y="0"/><a:chExt cx="1143000" cy="642937"/></a:xfrm></p:grpSpPr>
            <p:sp><p:nvSpPr><p:cNvPr id="6" name="In"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1143000" cy="642937"/></a:xfrm><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></p:spPr></p:sp></p:grpSp>"#;
        let img = draw(&deck(group, "")).expect("draws");
        // the child fills the group's box: from half the width, half the height, a quarter wide, a quarter high
        let at = |x: f32, y: f32| {
            img.get_pixel((img.width() as f32 * x) as u32, (img.height() as f32 * y) as u32)
                .0
        };
        assert_eq!(at(0.6, 0.6), [255, 0, 0, 255], "inside the moved and scaled child");
        assert_eq!(at(0.4, 0.6), [255, 255, 255, 255], "left of it");
        assert_eq!(at(0.6, 0.9), [255, 255, 255, 255], "below it");
    }

    #[test]
    fn what_is_not_a_presentation_or_is_damaged_is_an_error_and_not_a_panic() {
        assert!(draw(b"not a zip").is_err());
        assert!(draw(&zip(&[("ppt/other.xml", b"<x/>", false)])).is_err());
        assert!(draw(&zip(&[("ppt/presentation.xml", b"<p:presentation>", false)])).is_err());
        // a presentation without any slide
        assert!(draw(&zip(&[("ppt/presentation.xml", b"<presentation/>", false)])).is_err());
    }

    #[test]
    fn colours_are_made_lighter_and_darker_the_way_the_theme_says() {
        let doc = parse(
            r#"<a:solidFill xmlns:a="u"><a:srgbClr val="4472C4"><a:lumMod val="50000"/></a:srgbClr></a:solidFill>"#,
        )
        .expect("parses");
        let theme = Theme::read(None, None);
        let dark = theme.colour(doc.root_element()).expect("a colour");
        assert!(dark[2] < 0x80 && dark[0] < 0x30, "{dark:?}");
        let (h, s, l) = to_hsl([68, 114, 196]);
        let back = from_hsl(h, s, l);
        assert!(
            back.iter().zip([68u8, 114, 196]).all(|(a, b)| a.abs_diff(b) <= 1),
            "{back:?}"
        );
    }
}
