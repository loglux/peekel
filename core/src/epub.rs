//! EPUB, the e-book of most shops and libraries: a ZIP with a package file (OPF: the description of the book, the list of its parts,
//! their order) and the text as XHTML files, one a chapter or so. Read into the blocks of `book`, as FB2 is, and laid out the same way:
//! the cover, each part of the order on a new page, headings, paragraphs with emphasis and strong, lists, quotations, preformatted
//! text, tables (a row a line), pictures. The contents come from the book's own (`nav.xhtml` of EPUB 3, `toc.ncx` of EPUB 2), and
//! lead to a part or to a place inside one. The style sheets are hardly read: a book is set as every book here is (`book`), with what
//! the markup says (a heading, emphasis) and a centred line where a `style` or a class says `center`. Only the classes the sheets set
//! in a monospace font are taken from them: many books mark their listings so and not with `pre` (`mono_classes`).
//!
//! A book with DRM (`META-INF/rights.xml`, or `encryption.xml` naming a cipher that is not the obfuscation of fonts) is not opened:
//! its text cannot be read without the shop's key, and the card says so. Limits: the file (`MAX_BYTES`), each part unpacked
//! (`MAX_PART`), the parts of the order, the depth of the markup followed, the lines of the contents, and the pictures (`book`).
//!
//! An XHTML file is XML, but one with HTML's named entities (`&nbsp;`) and a DOCTYPE that the XML parser refuses (`xml::parse` takes no
//! DTD, so entities cannot multiply): the DOCTYPE is cut out and the entities of HTML that books use are put as their letters.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use image::RgbaImage;
use roxmltree::Node;

use crate::archive::Archive;
use crate::book::{self, Align, Block, SIZE, Span, Style};
use crate::decode::{ContentsItem, DecodeError};
use crate::fb2::{decode_text, plain_text};
use crate::guard::{read_limited, unreadable};
use crate::xml::{attr, child, children, descendant, resolve};

/// A bigger file is refused before it is read.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The largest part unpacked: an XHTML file, the package, the contents.
const MAX_PART: usize = 64 * 1024 * 1024;
/// The largest picture file unpacked.
const MAX_PICTURE: usize = 64 * 1024 * 1024;
/// Parts of the order read at most.
const MAX_SPINE: usize = 5_000;
/// Elements nested deeper than this are read as plain text.
const MAX_DEPTH: usize = 64;
/// Lines of the contents read at most, and the longest one, in characters.
const MAX_TOC: usize = 20_000;
const MAX_TITLE: usize = 200;
/// The most blocks a book makes.
const MAX_BLOCKS: usize = 4_000_000;
/// Style sheets read at most, and the largest one read.
const MAX_SHEETS: usize = 100;
const MAX_SHEET: usize = 4 * 1024 * 1024;

/// Whether the name says EPUB.
pub fn is_epub(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
}

/// Whether an archive is an EPUB: its `mimetype` says so.
pub fn is_epub_archive(archive: &Archive<'_>) -> bool {
    archive
        .text("mimetype", 64)
        .is_some_and(|m| m.trim().starts_with("application/epub+zip"))
}

/// A line of the book's contents: the part it leads to, the place in it (an id), its depth and its title.
#[derive(Clone)]
struct TocLine {
    path: String,
    fragment: Option<String>,
    depth: usize,
    title: String,
}

/// What the package says: the order of the parts, the cover, where the contents are, and the description of the book.
struct Package {
    spine: Vec<String>,
    cover: Option<String>,
    nav: Option<String>,
    ncx: Option<String>,
    /// Title, authors, language, publisher, date, identifier.
    rows: Vec<(String, String)>,
    pictures: usize,
    /// The style sheets of the manifest (read only for the classes set in a monospace font).
    sheets: Vec<String>,
}

/// A path in a link with its `%20` and the like decoded.
fn unescape(href: &str) -> String {
    let bytes = href.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(value) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(value);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A link (`text/ch1.xhtml#p5`) from the part at `from`: the path of the part it leads to and the place in it.
fn target(from: &str, href: &str) -> (String, Option<String>) {
    let (file, fragment) = match href.split_once('#') {
        Some((file, fragment)) => (file, Some(fragment.to_string()).filter(|f| !f.is_empty())),
        None => (href, None),
    };
    let path = if file.is_empty() {
        from.to_string()
    } else {
        resolve(from, &unescape(file))
    };
    (path, fragment)
}

/// The named entities of HTML that books use, as their letters (XML knows only five).
fn entity(name: &str) -> Option<char> {
    let code = match name {
        "nbsp" => 0xA0,
        "shy" => 0xAD,
        "mdash" => 0x2014,
        "ndash" => 0x2013,
        "hellip" => 0x2026,
        "laquo" => 0xAB,
        "raquo" => 0xBB,
        "lsaquo" => 0x2039,
        "rsaquo" => 0x203A,
        "ldquo" => 0x201C,
        "rdquo" => 0x201D,
        "lsquo" => 0x2018,
        "rsquo" => 0x2019,
        "bdquo" => 0x201E,
        "sbquo" => 0x201A,
        "copy" => 0xA9,
        "reg" => 0xAE,
        "trade" => 0x2122,
        "thinsp" => 0x2009,
        "ensp" => 0x2002,
        "emsp" => 0x2003,
        "zwnj" => 0x200C,
        "zwj" => 0x200D,
        "middot" => 0xB7,
        "bull" => 0x2022,
        "deg" => 0xB0,
        "times" => 0xD7,
        "divide" => 0xF7,
        "minus" => 0x2212,
        "plusmn" => 0xB1,
        "para" => 0xB6,
        "sect" => 0xA7,
        "dagger" => 0x2020,
        "Dagger" => 0x2021,
        "prime" => 0x2032,
        "Prime" => 0x2033,
        "frac12" => 0xBD,
        "frac14" => 0xBC,
        "frac34" => 0xBE,
        "euro" => 0x20AC,
        "pound" => 0xA3,
        "cent" => 0xA2,
        "yen" => 0xA5,
        "iexcl" => 0xA1,
        "iquest" => 0xBF,
        "szlig" => 0xDF,
        "agrave" => 0xE0,
        "aacute" => 0xE1,
        "acirc" => 0xE2,
        "auml" => 0xE4,
        "ccedil" => 0xE7,
        "egrave" => 0xE8,
        "eacute" => 0xE9,
        "ecirc" => 0xEA,
        "euml" => 0xEB,
        "iacute" => 0xED,
        "icirc" => 0xEE,
        "iuml" => 0xEF,
        "ntilde" => 0xF1,
        "oacute" => 0xF3,
        "ocirc" => 0xF4,
        "ouml" => 0xF6,
        "uacute" => 0xFA,
        "ucirc" => 0xFB,
        "uuml" => 0xFC,
        "Eacute" => 0xC9,
        "Agrave" => 0xC0,
        "Ouml" => 0xD6,
        "Uuml" => 0xDC,
        "Auml" => 0xC4,
        _ => return None,
    };
    char::from_u32(code)
}

/// An XHTML or XML part made ready for the XML parser: the DOCTYPE cut out, HTML's named entities put as letters (an unknown one is
/// shown as it was written).
fn prepare(text: &str) -> String {
    let text = text.trim_start_matches('\u{FEFF}');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // the DOCTYPE, with or without a subset in brackets
    if let Some(at) = rest.find("<!DOCTYPE").or_else(|| rest.find("<!doctype")) {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let close = after.find('>').unwrap_or(after.len());
        let end = match after[..close].find('[') {
            Some(_) => after.find("]>").map_or(after.len(), |e| e + 2),
            None => (close + 1).min(after.len()),
        };
        rest = &after[end..];
    }
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let name_end = after
            .char_indices()
            .take(12)
            .find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '#');
        match name_end {
            Some((i, ';')) if i > 0 => {
                let name = &after[..i];
                if name.starts_with('#') || matches!(name, "amp" | "lt" | "gt" | "quot" | "apos") {
                    out.push('&');
                    out.push_str(name);
                    out.push(';');
                } else if let Some(c) = entity(name) {
                    out.push(c);
                } else {
                    out.push_str("&amp;");
                    out.push_str(name);
                    out.push(';');
                }
                rest = &after[i + 1..];
            }
            _ => {
                out.push_str("&amp;");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A part of the archive as text ready for the parser.
fn part(archive: &Archive<'_>, name: &str) -> Result<String, DecodeError> {
    Ok(prepare(&decode_text(&archive.read(name, MAX_PART)?)?))
}

/// Whether the book is locked by DRM: Adobe's `rights.xml`, or an encrypted part whose cipher is not one of the two that only hide
/// fonts (IDPF's and Adobe's obfuscation, which every reader undoes).
fn protected(archive: &Archive<'_>) -> bool {
    if archive.contains("META-INF/rights.xml") {
        return true;
    }
    let Ok(text) = part(archive, "META-INF/encryption.xml") else {
        return false;
    };
    let Ok(document) = crate::xml::parse(&text) else {
        return true;
    };
    document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "EncryptionMethod")
        .filter_map(|n| attr(n, "Algorithm"))
        .any(|a| a != "http://www.idpf.org/2008/embedding" && a != "http://ns.adobe.com/pdf/enc#RC")
}

/// The package of the book: found by `META-INF/container.xml`, read.
fn package(archive: &Archive<'_>) -> Result<Package, DecodeError> {
    let container = part(archive, "META-INF/container.xml")?;
    let container = crate::xml::parse(&container)?;
    let opf = descendant(container.root_element(), "rootfile")
        .and_then(|r| attr(r, "full-path"))
        .map(|p| resolve("", &unescape(p)))
        .ok_or_else(|| unreadable("the EPUB names no package"))?;
    let text = part(archive, &opf)?;
    let document = crate::xml::parse(&text)?;
    let root = document.root_element();
    // the manifest: id -> (path, media type, properties)
    let mut manifest: HashMap<&str, (String, &str, &str)> = HashMap::new();
    if let Some(list) = child(root, "manifest") {
        for item in children(list, "item") {
            if let (Some(id), Some(href)) = (attr(item, "id"), attr(item, "href")) {
                manifest.insert(
                    id,
                    (
                        target(&opf, href).0,
                        attr(item, "media-type").unwrap_or(""),
                        attr(item, "properties").unwrap_or(""),
                    ),
                );
            }
        }
    }
    let spine_node = child(root, "spine");
    let spine: Vec<String> = spine_node
        .into_iter()
        .flat_map(|s| children(s, "itemref"))
        .filter_map(|r| manifest.get(attr(r, "idref")?))
        .filter(|(path, media, _)| {
            media.contains("html") || {
                let lower = path.to_ascii_lowercase();
                lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
            }
        })
        .map(|(path, _, _)| path.clone())
        .take(MAX_SPINE)
        .collect();
    let nav = manifest
        .values()
        .find(|(_, _, properties)| properties.split_whitespace().any(|p| p == "nav"))
        .map(|(path, _, _)| path.clone());
    let ncx = spine_node
        .and_then(|s| attr(s, "toc"))
        .and_then(|id| manifest.get(id))
        .or_else(|| {
            manifest
                .values()
                .find(|(_, media, _)| *media == "application/x-dtbncx+xml")
        })
        .map(|(path, _, _)| path.clone());
    let metadata = child(root, "metadata");
    // the cover: EPUB 3's property, EPUB 2's meta, or an image named so
    let cover = manifest
        .values()
        .find(|(_, _, properties)| properties.split_whitespace().any(|p| p == "cover-image"))
        .or_else(|| {
            let id = metadata
                .into_iter()
                .flat_map(|m| children(m, "meta"))
                .find(|m| attr(*m, "name") == Some("cover"))
                .and_then(|m| attr(m, "content"))?;
            manifest.get(id)
        })
        .or_else(|| {
            manifest
                .iter()
                .find(|(id, (_, media, _))| media.starts_with("image/") && id.to_ascii_lowercase().contains("cover"))
                .map(|(_, item)| item)
        })
        .filter(|(_, media, _)| media.starts_with("image/"))
        .map(|(path, _, _)| path.clone());
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(metadata) = metadata {
        let all = |name: &str| -> Vec<String> {
            children(metadata, name)
                .map(plain_text)
                .filter(|t| !t.is_empty())
                .map(|t| t.chars().take(MAX_TITLE).collect())
                .collect()
        };
        let mut push = |label: &str, values: Vec<String>| {
            if !values.is_empty() {
                rows.push((label.to_string(), values.join(", ")));
            }
        };
        push("Title", all("title").into_iter().take(1).collect());
        let authors = all("creator");
        push(if authors.len() > 1 { "Authors" } else { "Author" }, authors);
        push("Language", all("language"));
        push("Publisher", all("publisher"));
        push(
            "Date",
            all("date")
                .into_iter()
                .take(1)
                .map(|d| d.chars().take(10).collect())
                .collect(),
        );
        let identifiers = all("identifier");
        let isbn: Vec<String> = identifiers
            .iter()
            .filter(|i| i.to_ascii_lowercase().contains("isbn") || i.chars().filter(char::is_ascii_digit).count() == 13)
            .map(|i| {
                i.trim_start_matches("urn:isbn:")
                    .trim_start_matches("isbn:")
                    .to_string()
            })
            .take(1)
            .collect();
        push("ISBN", isbn);
    }
    rows.push(("Parts".to_string(), spine.len().to_string()));
    let pictures = manifest
        .values()
        .filter(|(_, media, _)| media.starts_with("image/"))
        .count();
    let mut sheets: Vec<String> = manifest
        .values()
        .filter(|(_, media, _)| *media == "text/css")
        .map(|(path, _, _)| path.clone())
        .collect();
    sheets.sort();
    sheets.truncate(MAX_SHEETS);
    Ok(Package {
        spine,
        cover,
        nav,
        ncx,
        rows,
        pictures,
        sheets,
    })
}

/// The contents of the book: from `nav.xhtml` (EPUB 3) or else `toc.ncx` (EPUB 2).
fn toc(archive: &Archive<'_>, package: &Package) -> Vec<TocLine> {
    let mut out = Vec::new();
    if let Some(nav) = &package.nav
        && let Ok(text) = part(archive, nav)
        && let Ok(document) = crate::xml::parse(&text)
    {
        let navs: Vec<Node<'_, '_>> = document
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "nav")
            .collect();
        let toc = navs
            .iter()
            .find(|n| attr(**n, "type") == Some("toc"))
            .or_else(|| navs.first());
        if let Some(list) = toc.and_then(|n| descendant(*n, "ol")) {
            nav_list(list, nav, 0, &mut out);
        }
    }
    if out.is_empty()
        && let Some(ncx) = &package.ncx
        && let Ok(text) = part(archive, ncx)
        && let Ok(document) = crate::xml::parse(&text)
        && let Some(map) = descendant(document.root_element(), "navMap")
    {
        ncx_points(map, ncx, 0, &mut out);
    }
    out
}

fn nav_list(list: Node<'_, '_>, from: &str, depth: usize, out: &mut Vec<TocLine>) {
    for item in children(list, "li") {
        if out.len() >= MAX_TOC || depth > MAX_DEPTH {
            return;
        }
        let label = child(item, "a").or_else(|| child(item, "span"));
        let title: String = label
            .map(plain_text)
            .unwrap_or_default()
            .chars()
            .take(MAX_TITLE)
            .collect();
        if let Some(href) = label.and_then(|a| attr(a, "href"))
            && !title.is_empty()
        {
            let (path, fragment) = target(from, href);
            out.push(TocLine {
                path,
                fragment,
                depth,
                title,
            });
        }
        if let Some(sub) = child(item, "ol") {
            nav_list(sub, from, depth + 1, out);
        }
    }
}

fn ncx_points(node: Node<'_, '_>, from: &str, depth: usize, out: &mut Vec<TocLine>) {
    for point in children(node, "navPoint") {
        if out.len() >= MAX_TOC || depth > MAX_DEPTH {
            return;
        }
        let title: String = child(point, "navLabel")
            .map(plain_text)
            .unwrap_or_default()
            .chars()
            .take(MAX_TITLE)
            .collect();
        if let Some(src) = child(point, "content").and_then(|c| attr(c, "src"))
            && !title.is_empty()
        {
            let (path, fragment) = target(from, src);
            out.push(TocLine {
                path,
                fragment,
                depth,
                title,
            });
        }
        ncx_points(point, from, depth + 1, out);
    }
}

/// How an element of XHTML is read.
#[derive(PartialEq)]
enum Kind {
    Skip,
    Inline,
    Block,
}

fn kind(name: &str) -> Kind {
    match name {
        "script" | "style" | "head" | "title" | "noscript" | "template" | "math" | "iframe" | "object" | "embed"
        | "canvas" | "video" | "audio" | "select" => Kind::Skip,
        "span" | "a" | "em" | "i" | "strong" | "b" | "code" | "tt" | "kbd" | "samp" | "var" | "sup" | "sub"
        | "small" | "big" | "cite" | "dfn" | "abbr" | "acronym" | "q" | "u" | "s" | "strike" | "del" | "ins"
        | "mark" | "font" | "time" | "bdi" | "bdo" | "label" | "ruby" | "rb" | "rt" | "rp" | "wbr" => Kind::Inline,
        _ => Kind::Block,
    }
}

/// The style of a piece of text in an inline element (`mono`: the classes the book's style sheets set in a monospace font).
fn inline_style(node: Node<'_, '_>, base: &Span, mono: &HashSet<String>) -> Span {
    let mut span = base.clone();
    match node.tag_name().name() {
        "em" | "i" | "cite" | "dfn" | "var" => span.italic = !base.italic,
        "strong" | "b" => span.bold = true,
        "code" | "tt" | "kbd" | "samp" => span.mono = true,
        "sup" | "sub" => span.raised = true,
        _ => {}
    }
    if monospaced(node, mono) {
        span.mono = true;
    }
    if let Some(style) = attr(node, "style") {
        let style = style.to_ascii_lowercase();
        if style.contains("italic") {
            span.italic = true;
        }
        if style.contains("bold") {
            span.bold = true;
        }
    }
    span
}

/// Whether an element says its lines are centred (by `style` or a class).
fn centred(node: Node<'_, '_>) -> bool {
    let style = attr(node, "style").unwrap_or("").to_ascii_lowercase().replace(' ', "");
    style.contains("text-align:center")
        || attr(node, "class").is_some_and(|c| c.to_ascii_lowercase().split_whitespace().any(|c| c.contains("center")))
        || node.tag_name().name() == "center"
}

/// Whether declarations (`font-family: "PT Mono", monospace; margin: 0`) set a monospace font.
fn mono_font(declarations: &str) -> bool {
    declarations.split(';').any(|d| {
        let Some((name, value)) = d.split_once(':') else {
            return false;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        (name == "font-family" || name == "font")
            && (value.contains("mono") || value.contains("courier") || value.contains("consolas"))
    })
}

/// The classes a style sheet sets in a monospace font: listings that are marked only so (`p.listing-body`, `.code`), and commands in
/// the text (`span.command`). Only a selector of one element with one class counts (`p.x`, `.x`); one that depends on where the
/// element is (`pre .x`, `p.x > span`) and the rules of `@font-face` do not. Nothing else of the sheet is read.
fn mono_classes(css: &str, into: &mut HashSet<String>) {
    // the comments out
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        text.push_str(&rest[..start]);
        rest = rest[start + 2..].split_once("*/").map_or("", |(_, after)| after);
    }
    text.push_str(rest);
    for rule in text.split('}') {
        // `@media print { p.x { ... ` : the selectors are what stands after the last `{` but one
        let Some((head, declarations)) = rule.rsplit_once('{') else {
            continue;
        };
        let selectors = head.rsplit('{').next().unwrap_or("").trim();
        if selectors.starts_with('@') || !mono_font(declarations) {
            continue;
        }
        for selector in selectors.split(',') {
            let selector = selector.trim();
            if selector.is_empty() || selector.contains(|c: char| c.is_whitespace() || ">+~:[#*".contains(c)) {
                continue;
            }
            let mut parts = selector.split('.');
            let element = parts.next().unwrap_or("");
            let classes: Vec<&str> = parts.collect();
            if classes.len() == 1 && !classes[0].is_empty() && element.chars().all(|c| c.is_ascii_alphanumeric()) {
                into.insert(classes[0].to_string());
            }
        }
    }
}

/// Whether an element is set in a monospace font by a class of the book's style sheets or by its own `style`.
fn monospaced(node: Node<'_, '_>, mono: &HashSet<String>) -> bool {
    attr(node, "class").is_some_and(|c| c.split_whitespace().any(|c| mono.contains(c)))
        || attr(node, "style").is_some_and(mono_font)
}

/// A line of code as it is drawn: its indent kept as figure spaces (which the layout neither stretches nor breaks at); inside the line
/// a run of spaces is one space a long line may break at, with figure spaces before it for the rest of the run.
fn code_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut leading = true;
    let mut spaces = 0;
    for c in line.chars() {
        let width = match c {
            ' ' | '\u{a0}' => 1,
            '\t' => 4,
            _ => 0,
        };
        if width > 0 {
            if leading {
                out.extend(std::iter::repeat_n('\u{2007}', width));
            } else {
                spaces += width;
            }
            continue;
        }
        leading = false;
        if spaces > 0 {
            out.extend(std::iter::repeat_n('\u{2007}', spaces - 1));
            out.push(' ');
            spaces = 0;
        }
        out.push(c);
    }
    out
}

/// Adds text to the spans of a paragraph in a style, the soft hyphens and zero-width spaces of the book left out (the layout breaks
/// words itself).
fn add_text(run: &mut Vec<Span>, text: &str, style: &Span) {
    let text: String = text.chars().filter(|c| !matches!(c, '\u{AD}' | '\u{200B}')).collect();
    match run.last_mut() {
        Some(last)
            if last.bold == style.bold
                && last.italic == style.italic
                && last.mono == style.mono
                && last.raised == style.raised =>
        {
            last.text.push_str(&text);
        }
        _ => run.push(Span { text, ..style.clone() }),
    }
}

/// Where the pictures of what is read are: the archive of an EPUB, or the folder of an HTML file (`html`).
#[derive(Clone, Copy)]
pub(crate) enum Source<'r, 'a> {
    Archive(&'r Archive<'a>),
    Folder(&'r Path),
}

impl Source<'_, '_> {
    /// The file at `path`, at most `max` bytes: in an archive, a path as `resolve` makes it from its root; in a folder, a path relative to
    /// it as the page wrote it (`relative`), which may go above it (a site's pictures beside its pages) but not to another drive, a
    /// server or the root.
    fn read(&self, path: &str, max: usize) -> Option<Vec<u8>> {
        match self {
            Self::Archive(archive) => archive.read(path, max).ok(),
            Self::Folder(folder) => {
                if path.is_empty() || path.starts_with('/') || path.contains([':', '\\']) {
                    return None;
                }
                let file = path
                    .split('/')
                    .filter(|p| !p.is_empty() && *p != ".")
                    .fold(folder.to_path_buf(), |f, p| f.join(p));
                read_limited(&file, max as u64).ok()
            }
        }
    }
}

/// A link of a page in a folder as a path relative to that folder: its place (`#...`) and query (`?...`) cut off, `%20` and the like
/// decoded.
fn relative(href: &str) -> String {
    let file = href.split(['#', '?']).next().unwrap_or_default();
    unescape(file.trim())
}
/// A picture written in the link itself (`data:image/png;base64,...`), as saved web pages have them; at most `max` bytes.
fn data_url(href: &str, max: usize) -> Option<Vec<u8>> {
    use base64::Engine;
    let rest = href.trim().strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    if !head.ends_with(";base64") || data.len() / 4 * 3 > max {
        return None;
    }
    let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(&compact)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
        .ok()
}

/// What reading a book has made.
pub(crate) struct Reader<'r, 'a> {
    source: Source<'r, 'a>,
    pictures: book::Pictures,
    blocks: Vec<Block>,
    /// The lines of the contents that lead to a place in the part being read, by the id of that place.
    places: HashMap<String, Vec<(usize, String)>>,
    /// A list's mark (`•`, `3.`) for the next paragraph.
    prefix: Option<String>,
    /// The classes the book's style sheets set in a monospace font (`mono_classes`).
    mono: HashSet<String>,
    /// An HTML page (`page_blocks`): its headings `h1` to `h3` make the contents (it has none of its own), its `nav` (a site's menu)
    /// is passed over, and a picture that cannot be read leaves no grey box (a web page's icons and pictures on the network).
    page: bool,
}

/// Where the text is read: the part's path (links in it are from there), the paragraph's style and the text's.
#[derive(Clone)]
struct Context<'c> {
    path: &'c str,
    style: Style,
    span: Span,
}

impl Reader<'_, '_> {
    fn push(&mut self, block: Block) {
        if self.blocks.len() < MAX_BLOCKS {
            self.blocks.push(block);
        }
    }

    /// The lines of the contents that lead to an element with this id.
    fn anchor(&mut self, node: Node<'_, '_>) {
        if let Some(id) = attr(node, "id")
            && let Some(lines) = self.places.remove(id)
        {
            for (depth, title) in lines {
                self.push(Block::Mark { depth, title });
            }
        }
    }

    /// The picture at `href` from the part at `from` (or in the link itself, `data:`). A link to another place (`http:`) is no
    /// picture that can be read, and is passed over.
    fn picture(&mut self, from: &str, href: &str) {
        if href.trim_start().starts_with("data:") {
            // keyed by where it is, so that the same picture written twice is decoded twice, within the limits
            let key = format!("data:{}:{}", self.blocks.len(), href.len());
            let image = self.pictures.get(&key, || data_url(href, MAX_PICTURE));
            if image.is_some() || !self.page {
                self.push(Block::Picture(image));
            }
            return;
        }
        if href.contains("://") {
            return;
        }
        let path = match self.source {
            Source::Archive(_) => target(from, href).0,
            Source::Folder(_) => relative(href),
        };
        let source = self.source;
        let image = self.pictures.get(&path, || source.read(&path, MAX_PICTURE));
        if image.is_some() || !self.page {
            self.push(Block::Picture(image));
        }
    }

    /// The picture an `img`, an `image` (in SVG) or an `svg` element shows, if it shows one.
    fn picture_of(&mut self, node: Node<'_, '_>, from: &str) {
        let source = match node.tag_name().name() {
            "img" => attr(node, "src"),
            "image" => attr(node, "href"),
            _ => descendant(node, "image").and_then(|i| attr(i, "href")),
        };
        if let Some(source) = source {
            self.picture(from, source);
        }
    }

    /// Lines of code, each a paragraph of its own: monospace, left, not stretched nor hyphenated, the spaces kept; an empty line is
    /// a space of one line.
    fn code_lines<'l>(&mut self, lines: impl Iterator<Item = &'l str>, context: &Context<'_>) {
        let style = Style {
            align: Align::Left,
            indent: false,
            ..context.style.clone()
        };
        for line in lines {
            if line.trim().is_empty() {
                self.push(Block::Space(SIZE * book::LEADING));
            } else {
                self.push(Block::Paragraph {
                    spans: vec![Span {
                        text: code_text(line.trim_end()),
                        mono: true,
                        ..Span::default()
                    }],
                    style: style.clone(),
                });
            }
        }
    }

    /// The text of a listing set as lines: `br` and the edges of inner blocks break the line; the places the contents lead to are
    /// kept.
    fn code_text_of(&mut self, node: Node<'_, '_>, text: &mut String, level: usize) {
        for c in node.children() {
            if c.is_text() {
                text.push_str(&c.text().unwrap_or("").replace('\n', " "));
                continue;
            }
            if !c.is_element() {
                continue;
            }
            self.anchor(c);
            let name = c.tag_name().name();
            match kind(name) {
                Kind::Skip => {}
                _ if name == "br" => text.push('\n'),
                _ if level > MAX_DEPTH => text.push_str(&plain_text(c)),
                Kind::Inline => self.code_text_of(c, text, level + 1),
                Kind::Block => {
                    text.push('\n');
                    self.code_text_of(c, text, level + 1);
                    text.push('\n');
                }
            }
        }
    }

    /// The spans waiting become a paragraph (when there is text in them).
    fn flush(&mut self, run: &mut Vec<Span>, context: &Context<'_>, indent: bool) {
        let spans = std::mem::take(run);
        if !spans.iter().any(|s| !s.text.trim().is_empty()) {
            return;
        }
        let mut spans = spans;
        if let Some(prefix) = self.prefix.take() {
            spans.insert(
                0,
                Span {
                    text: prefix,
                    ..context.span.clone()
                },
            );
        }
        self.push(Block::Paragraph {
            spans,
            style: Style {
                indent: context.style.indent && indent,
                ..context.style.clone()
            },
        });
    }

    /// The children of a block element: text and inline elements gathered into paragraphs, block elements read in turn.
    fn flow(&mut self, node: Node<'_, '_>, context: &Context<'_>, level: usize) {
        let mut run: Vec<Span> = Vec::new();
        let mut indent = true;
        self.inline(node, &context.span, &mut run, &mut indent, context, level);
        self.flush(&mut run, context, indent);
    }

    /// The children of `node` in the paragraph being gathered (`run`); a block element ends it.
    fn inline(
        &mut self,
        node: Node<'_, '_>,
        span: &Span,
        run: &mut Vec<Span>,
        indent: &mut bool,
        context: &Context<'_>,
        level: usize,
    ) {
        for c in node.children() {
            if c.is_text() {
                if let Some(text) = c.text() {
                    add_text(run, text, span);
                }
                continue;
            }
            if !c.is_element() {
                continue;
            }
            self.anchor(c);
            let name = c.tag_name().name();
            if level > MAX_DEPTH {
                add_text(run, &plain_text(c), span);
                continue;
            }
            match (name, kind(name)) {
                (_, Kind::Skip) => {}
                ("nav", _) if self.page => {}
                ("br", _) => {
                    self.flush(run, context, *indent);
                    *indent = false;
                }
                ("img" | "image" | "svg", _) => {
                    self.flush(run, context, *indent);
                    self.picture_of(c, context.path);
                }
                (_, Kind::Inline) => {
                    let style = inline_style(c, span, &self.mono);
                    self.inline(c, &style, run, indent, context, level + 1);
                }
                (_, Kind::Block) => {
                    self.flush(run, context, *indent);
                    *indent = true;
                    self.element(c, context, level + 1);
                }
            }
        }
    }

    /// A block element.
    fn element(&mut self, node: Node<'_, '_>, context: &Context<'_>, level: usize) {
        let name = node.tag_name().name();
        let with = |style: Style| Context {
            style,
            ..context.clone()
        };
        let centre = |style: Style| {
            if centred(node) {
                Style {
                    align: Align::Center,
                    indent: false,
                    ..style
                }
            } else {
                style
            }
        };
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let depth = match name {
                    "h1" => 1,
                    "h2" => 2,
                    _ => 3,
                };
                if self.page && matches!(name, "h1" | "h2" | "h3") {
                    let title: String = plain_text(node).chars().take(MAX_TITLE).collect();
                    if !title.is_empty() {
                        self.push(Block::Mark {
                            depth: depth - 1,
                            title,
                        });
                    }
                }
                self.flow(node, &with(Style::heading(depth)), level);
            }
            "p" if !monospaced(node, &self.mono) => self.flow(node, &with(centre(context.style.clone())), level),
            "blockquote" => {
                self.push(Block::Space(SIZE * 0.4));
                let style = Style {
                    left: context.style.left + SIZE * 2.0,
                    right: context.style.right + SIZE * 2.0,
                    ..context.style.clone()
                };
                self.flow(node, &with(style), level);
                self.push(Block::Space(SIZE * 0.4));
            }
            "ul" | "ol" => {
                let numbered = name == "ol";
                let start: usize = attr(node, "start").and_then(|s| s.parse().ok()).unwrap_or(1);
                let style = Style {
                    align: Align::Left,
                    indent: false,
                    left: context.style.left + SIZE * 1.5,
                    ..context.style.clone()
                };
                for (n, item) in children(node, "li").enumerate() {
                    self.anchor(item);
                    self.prefix = Some(if numbered {
                        format!("{}. ", start + n)
                    } else {
                        "\u{2022} ".to_string()
                    });
                    self.flow(item, &with(style.clone()), level);
                    self.prefix = None;
                }
            }
            "pre" => {
                let text: String = node
                    .descendants()
                    .filter(|n| n.is_text())
                    .filter_map(|n| n.text())
                    .collect();
                self.code_lines(text.lines(), context);
            }
            // a listing that only the style sheet sets in a monospace font: a paragraph a line, or a block of lines broken by `br`
            "p" | "div" if monospaced(node, &self.mono) => {
                let mut text = String::new();
                self.code_text_of(node, &mut text, level);
                let lines: Vec<String> = text
                    .split('\n')
                    .map(|line| {
                        // spaces in HTML's text run together, the book's no-break ones (its indents) stay
                        let mut joined = String::new();
                        for word in line.split([' ', '\t', '\r']).filter(|w| !w.is_empty()) {
                            if !joined.is_empty() {
                                joined.push(' ');
                            }
                            joined.push_str(word);
                        }
                        joined
                    })
                    .collect();
                // the line breaks put before and after inner blocks leave no empty line at the ends
                let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
                let last = lines.iter().rposition(|l| !l.is_empty()).map_or(first, |l| l + 1);
                self.code_lines(lines[first..last].iter().map(String::as_str), context);
            }
            "hr" => self.push(Block::Space(SIZE)),
            "table" => {
                for row in node
                    .descendants()
                    .filter(|n| n.is_element() && n.tag_name().name() == "tr")
                {
                    let cells: Vec<String> = row.children().filter(|c| c.is_element()).map(plain_text).collect();
                    self.push(Block::Paragraph {
                        spans: vec![Span {
                            text: cells.join("  \u{2502}  "),
                            ..Span::default()
                        }],
                        style: Style::verse(context.style.left),
                    });
                }
            }
            "img" | "image" | "svg" => self.picture_of(node, context.path),
            "figcaption" | "caption" => self.flow(
                node,
                &with(Style {
                    align: Align::Center,
                    indent: false,
                    italic: true,
                    ..context.style.clone()
                }),
                level,
            ),
            _ => self.flow(node, &with(centre(context.style.clone())), level),
        }
    }

    /// One part of the order: a new page, the lines of the contents that lead to it, its text.
    fn part(&mut self, path: &str, lines: Vec<TocLine>, cover: Option<&str>) {
        let Source::Archive(archive) = self.source else {
            return;
        };
        let text = match part(archive, path) {
            Ok(text) => text,
            Err(_) => {
                self.missing(path, lines);
                return;
            }
        };
        let Ok(document) = crate::xml::parse(&text) else {
            self.missing(path, lines);
            return;
        };
        let root = document.root_element();
        let body = descendant(root, "body").unwrap_or(root);
        // a page that only shows the cover, which is the book's first page already
        if let Some(cover) = cover
            && plain_text(body).is_empty()
        {
            let shows: Vec<String> = body
                .descendants()
                .filter_map(|n| match n.tag_name().name() {
                    "img" => attr(n, "src"),
                    "image" => attr(n, "href"),
                    _ => None,
                })
                .map(|h| target(path, h).0)
                .collect();
            if shows.iter().all(|s| s == cover) {
                self.marks(lines.into_iter().map(|l| (l.depth, l.title)));
                return;
            }
        }
        if !self.blocks.is_empty() {
            self.push(Block::NewPage);
        }
        let ids: HashSet<&str> = body.descendants().filter_map(|n| attr(n, "id")).collect();
        self.places.clear();
        let mut here = Vec::new();
        for line in lines {
            match line.fragment {
                Some(id) if ids.contains(id.as_str()) => {
                    self.places.entry(id).or_default().push((line.depth, line.title));
                }
                _ => here.push((line.depth, line.title)),
            }
        }
        self.marks(here.into_iter());
        let context = Context {
            path,
            style: Style::body(),
            span: Span::default(),
        };
        self.anchor(body);
        self.flow(body, &context, 0);
        // a place that was never met leads to the end of the part
        let left: Vec<(usize, String)> = self.places.drain().flat_map(|(_, lines)| lines).collect();
        self.marks(left.into_iter());
    }

    fn marks(&mut self, lines: impl Iterator<Item = (usize, String)>) {
        for (depth, title) in lines {
            self.push(Block::Mark { depth, title });
        }
    }

    /// A part that could not be read: said on its page, so the reader knows text is missing.
    fn missing(&mut self, path: &str, lines: Vec<TocLine>) {
        if !self.blocks.is_empty() {
            self.push(Block::NewPage);
        }
        self.marks(lines.into_iter().map(|l| (l.depth, l.title)));
        self.push(Block::Paragraph {
            spans: vec![Span {
                text: format!("[a part of the book could not be read: {path}]"),
                italic: true,
                ..Span::default()
            }],
            style: Style::verse(0.0),
        });
    }
}

/// The blocks of the book at `path`.
pub fn blocks(path: &Path) -> Result<Vec<Block>, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    let archive = Archive::new(&data)?;
    if !is_epub_archive(&archive) && !archive.contains("META-INF/container.xml") {
        return Err(unreadable("not an EPUB book"));
    }
    if protected(&archive) {
        return Err(unreadable("the book is protected by DRM: its text cannot be read"));
    }
    let package = package(&archive)?;
    if package.spine.is_empty() {
        return Err(unreadable("the book has no text in its order"));
    }
    let toc = toc(&archive, &package);
    let mut mono = HashSet::new();
    for sheet in &package.sheets {
        if let Ok(bytes) = archive.read(sheet, MAX_SHEET) {
            mono_classes(&String::from_utf8_lossy(&bytes), &mut mono);
        }
    }
    let mut reader = Reader {
        source: Source::Archive(&archive),
        pictures: book::Pictures::default(),
        blocks: Vec::new(),
        places: HashMap::new(),
        prefix: None,
        mono,
        page: false,
    };
    if let Some(cover) = &package.cover {
        let archive = &archive;
        if let Some(image) = reader.pictures.get(cover, || archive.read(cover, MAX_PICTURE).ok()) {
            reader.push(Block::Cover(image));
        }
    }
    let cover = package.cover.as_deref();
    for (index, path) in package.spine.iter().enumerate() {
        // a part that the contents do not name gets no line; with no contents at all, each part is named by its heading
        let mut lines: Vec<TocLine> = toc.iter().filter(|l| &l.path == path).cloned().collect();
        if toc.is_empty()
            && let Some(title) = heading_line(&archive, path)
        {
            lines.push(TocLine {
                path: path.clone(),
                fragment: None,
                depth: 0,
                title,
            });
        }
        reader.part(path, lines, if index < 3 { cover } else { None });
    }
    if !reader
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Paragraph { .. } | Block::Picture(_) | Block::Cover(_)))
    {
        return Err(unreadable("the book has no text"));
    }
    Ok(reader.blocks)
}

/// The blocks of a page of HTML made XML (`html`), whose file is `name` in `folder`: its body read as a part of a book is, its headings
/// the contents, its pictures from the folder (or from the links themselves), the classes set in a monospace font by its `style`
/// elements and the style sheets it links to in the folder.
pub(crate) fn page_blocks(xml: &str, folder: &Path, name: &str) -> Result<Vec<Block>, DecodeError> {
    let document = crate::xml::parse(xml)?;
    let root = document.root_element();
    let source = Source::Folder(folder);
    let mut mono = HashSet::new();
    let mut sheets = 0;
    for node in root.descendants().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "style" => mono_classes(&plain_text_raw(node), &mut mono),
            "link"
                if sheets < MAX_SHEETS
                    && attr(node, "rel").is_some_and(|r| r.to_ascii_lowercase().contains("stylesheet")) =>
            {
                sheets += 1;
                if let Some(href) = attr(node, "href").filter(|h| !h.contains("://"))
                    && let Some(bytes) = source.read(&relative(href), MAX_SHEET)
                {
                    mono_classes(&String::from_utf8_lossy(&bytes), &mut mono);
                }
            }
            _ => {}
        }
    }
    let mut reader = Reader {
        source,
        pictures: book::Pictures::default(),
        blocks: Vec::new(),
        places: HashMap::new(),
        prefix: None,
        mono,
        page: true,
    };
    let body = descendant(root, "body").unwrap_or(root);
    let context = Context {
        path: name,
        style: Style::body(),
        span: Span::default(),
    };
    reader.flow(body, &context, 0);
    if !reader
        .blocks
        .iter()
        .any(|b| matches!(b, Block::Paragraph { .. } | Block::Picture(_)))
    {
        return Err(unreadable("the page has no text"));
    }
    Ok(reader.blocks)
}

/// The text of an element as it is written (a style sheet), its spaces kept.
fn plain_text_raw(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

/// The first heading of a part, for a book with no contents of its own.
fn heading_line(archive: &Archive<'_>, path: &str) -> Option<String> {
    let text = part(archive, path).ok()?;
    let document = crate::xml::parse(&text).ok()?;
    let heading = document
        .descendants()
        .find(|n| n.is_element() && matches!(n.tag_name().name(), "h1" | "h2" | "h3"))?;
    let title: String = plain_text(heading).chars().take(MAX_TITLE).collect();
    (!title.is_empty()).then_some(title)
}

/// The first page (the cover, or the start of the text): for the quick view and the list of files.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    book::first_page(path, blocks)
}

/// Page `page` (from 0) and the number of pages.
pub fn render_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    book::page(&*book::laid(path, blocks)?, page)
}

/// The text of every page, for a search.
pub fn page_texts(path: &Path) -> Result<Option<Vec<String>>, DecodeError> {
    Ok(book::texts(&*book::laid(path, blocks)?))
}

/// The contents: the book's own, with the pages its lines lead to.
pub fn contents(path: &Path) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    Ok(book::contents(&*book::laid(path, blocks)?))
}

/// Marks the words that `needle` is in on page `page` as [`render_page`] drew it; how many.
pub fn mark_words(path: &Path, page: usize, needle: &str, image: &mut RgbaImage) -> Result<usize, DecodeError> {
    Ok(book::mark_words(&*book::laid(path, blocks)?, page, needle, image))
}

/// The place of page `page` in the text (`book::place_of`), at the size of the text now.
pub fn place_of(path: &Path, page: usize) -> Result<usize, DecodeError> {
    Ok(book::place_of(&*book::laid(path, blocks)?, page))
}

/// The page that place `place` of the text is on, at the size of the text now.
pub fn page_at(path: &Path, place: usize) -> Result<usize, DecodeError> {
    Ok(book::page_at(&*book::laid(path, blocks)?, place))
}

/// What the package says of the book, for its card; and whether it is locked by DRM.
pub fn properties(archive: &Archive<'_>) -> Vec<(String, String)> {
    let mut rows = match package(archive) {
        Ok(package) => {
            let mut rows = package.rows;
            if package.pictures > 0 {
                rows.push(("Pictures".to_string(), package.pictures.to_string()));
            }
            rows
        }
        Err(_) => Vec::new(),
    };
    if protected(archive) {
        rows.push((
            "Protected".to_string(),
            "by DRM: the text cannot be read without the shop's key".to_string(),
        ));
    }
    rows
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-epub-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        dir
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([200, 40, 30, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("a PNG");
        bytes
    }

    const CONTAINER: &str = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;

    const CSS: &str = r#"@font-face { font-family: "PT Mono"; src: url(../fonts/mono.ttf) }
/* p.commented { font-family: monospace } */
p { text-indent: 1em } p.listing { font-family: "PT Mono", monospace; margin-left: 2em }
@media screen { .code-line { font: 10pt Courier New } }
span.cmd, h2.title { font-family: Consolas } pre .inner { font-family: monospace } p.a.b { font-family: monospace }
p.serif { font-family: Georgia, serif }"#;

    fn chapter(title: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title><style>p {{ color: red }}</style></head>
<body><h2 id="top">{title}</h2>{body}</body></html>"#
        )
    }

    /// An EPUB 3 book: a cover, two chapters (the second with a place the contents lead to), its contents in `nav.xhtml`; `extra` more
    /// entries (an `encryption.xml`).
    fn book(extra: &[(&str, &[u8])], nav: bool) -> Vec<u8> {
        let text = "It was a quiet morning and the words went on. ".repeat(80);
        let one = chapter(
            "Chapter&nbsp;One",
            &format!(
                "<p>{text}</p><p>A <em>word</em>, a <b>strong</b> one &mdash; and &laquo;quotes&raquo;.</p>\
                 <ul><li>first</li><li>second</li></ul><blockquote><p>Quoted.</p></blockquote>\
                 <p><img src=\"../images/pic%201.png\" alt=\"\"/></p><pre>code  line</pre>\
                 <p class=\"listing\">def f(x):</p><p class=\"listing\">&#160;&#160;&#160;&#160;return   x * 2</p>\
                 <p class=\"listing\">&#160;</p><p class=\"listing\">print(f(2))</p><p>Run <span class=\"cmd\">ls</span> now.</p>"
            ),
        );
        let two = chapter(
            "Chapter Two",
            &format!("<p>{text}</p><p id=\"deep\">The needle is here.</p>"),
        );
        let cover_page = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><body>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="../images/cover.png"/></svg></body></html>"#;
        let nav_xhtml = r##"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="toc"><ol><li><a href="text/one.xhtml">Chapter One</a></li>
<li><a href="text/two.xhtml">Chapter Two</a><ol><li><a href="text/two.xhtml#deep">The needle</a></li></ol></li></ol></nav></body></html>"##;
        let ncx = r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
<navPoint id="a"><navLabel><text>First (NCX)</text></navLabel><content src="text/one.xhtml"/></navPoint>
<navPoint id="b"><navLabel><text>Second (NCX)</text></navLabel><content src="text/two.xhtml"/>
<navPoint id="c"><navLabel><text>Deep (NCX)</text></navLabel><content src="text/two.xhtml#deep"/></navPoint></navPoint></navMap></ncx>"#;
        let opf = format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>A Test Book</dc:title><dc:creator>Anna Writer</dc:creator>
<dc:language>en</dc:language><dc:publisher>Nobody</dc:publisher><dc:identifier>urn:isbn:9780000000002</dc:identifier>
<meta name="cover" content="cover"/></metadata>
<manifest><item id="cover" href="images/cover.png" media-type="image/png"/>
<item id="pic" href="images/pic%201.png" media-type="image/png"/>
<item id="cp" href="text/cover.xhtml" media-type="application/xhtml+xml"/>
<item id="one" href="text/one.xhtml" media-type="application/xhtml+xml"/>
<item id="two" href="text/two.xhtml" media-type="application/xhtml+xml"/>
<item id="css" href="styles/book.css" media-type="text/css"/>
{}<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest>
<spine toc="ncx"><itemref idref="cp"/><itemref idref="one"/><itemref idref="two"/></spine></package>"#,
            if nav {
                r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#
            } else {
                ""
            }
        );
        let (cover, picture) = (png(60, 90), png(40, 20));
        let mut files: Vec<(&str, &[u8], bool)> = vec![
            ("mimetype", b"application/epub+zip", false),
            ("META-INF/container.xml", CONTAINER.as_bytes(), true),
            ("OEBPS/content.opf", opf.as_bytes(), true),
            ("OEBPS/nav.xhtml", nav_xhtml.as_bytes(), true),
            ("OEBPS/toc.ncx", ncx.as_bytes(), true),
            ("OEBPS/text/cover.xhtml", cover_page.as_bytes(), true),
            ("OEBPS/text/one.xhtml", one.as_bytes(), true),
            ("OEBPS/text/two.xhtml", two.as_bytes(), true),
            ("OEBPS/images/cover.png", &cover, false),
            ("OEBPS/images/pic 1.png", &picture, false),
            ("OEBPS/styles/book.css", CSS.as_bytes(), true),
        ];
        files.extend(extra.iter().map(|(n, b)| (*n, *b, true)));
        crate::archive::tests::zip(&files)
    }

    #[test]
    fn a_book_is_laid_out_with_its_cover_and_its_own_contents() {
        let dir = folder("book");
        let path = dir.join("test.epub");
        std::fs::write(&path, book(&[], true)).expect("written");
        let (cover, count) = render_page(&path, 0).expect("the cover");
        assert!(count >= 4, "{count} pages");
        let [r, g, b, _] = cover.get_pixel(cover.width() / 2, cover.height() / 2).0;
        assert!(r > 150 && g < 90 && b < 90, "{r} {g} {b}");
        let contents = contents(&path).expect("reads").expect("has contents");
        let lines: Vec<(&str, usize)> = contents.iter().map(|c| (c.title.as_str(), c.depth)).collect();
        assert_eq!(lines, [("Chapter One", 0), ("Chapter Two", 0), ("The needle", 1)]);
        let texts = page_texts(&path).expect("reads").expect("has text");
        // the cover's own page is not a page of its own again
        let one = contents[0].page.expect("a page");
        assert_eq!(one, 1);
        assert!(texts[one].starts_with("Chapter One"), "{:?}", texts[one]);
        assert!(texts.iter().any(|t| t.contains("A word, a strong one — and «quotes».")));
        assert!(texts.iter().any(|t| t.contains("\u{2022} first")));
        let needle = contents[2].page.expect("a page");
        assert!(texts[needle].contains("The needle is here."), "{:?}", texts[needle]);
        let (mut page, _) = render_page(&path, needle).expect("the page");
        assert_eq!(mark_words(&path, needle, "needle", &mut page).expect("marks"), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_style_sheets_give_only_the_classes_set_in_a_monospace_font() {
        let mut mono = HashSet::new();
        mono_classes(CSS, &mut mono);
        let mut found: Vec<&str> = mono.iter().map(String::as_str).collect();
        found.sort_unstable();
        assert_eq!(found, ["cmd", "code-line", "listing", "title"]);
        // a sheet cut off or of nonsense makes nothing and does not stop
        let mut none = HashSet::new();
        mono_classes("p.x { font-family: monospace /* never closed", &mut none);
        mono_classes("}}}{{{ .y { font-family: mono } {", &mut none);
        mono_classes("", &mut none);
        assert!(none.contains("y"), "{none:?}");
    }

    #[test]
    fn a_listing_marked_only_by_its_class_is_set_as_code() {
        let dir = folder("listing");
        let path = dir.join("code.epub");
        std::fs::write(&path, book(&[], true)).expect("written");
        let blocks = blocks(&path).expect("reads");
        let code: Vec<(&str, &Style)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { spans, style } if spans.len() == 1 && spans[0].mono => {
                    Some((spans[0].text.as_str(), style))
                }
                _ => None,
            })
            .collect();
        let f = "\u{2007}";
        let texts: Vec<&str> = code.iter().map(|(t, _)| *t).collect();
        // `pre` as before; the listing a paragraph a line, its no-break indent kept, its other spaces run together
        assert_eq!(
            texts,
            [
                format!("code{f} line").as_str(),
                "def f(x):",
                format!("{f}{f}{f}{f}return x * 2").as_str(),
                "print(f(2))",
            ]
        );
        assert!(code.iter().all(|(_, s)| s.align == Align::Left && !s.indent));
        // the line of no-break spaces alone is an empty line of the listing
        let at = blocks
            .iter()
            .position(|b| matches!(b, Block::Paragraph { spans, .. } if spans[0].text.ends_with('2') && spans[0].mono))
            .expect("the line");
        assert!(matches!(blocks[at + 1], Block::Space(_)), "{:?}", blocks[at + 1]);
        // a command in the text is monospace inside its paragraph
        assert!(blocks.iter().any(|b| matches!(b, Block::Paragraph { spans, .. }
            if spans.iter().any(|s| s.mono && s.text == "ls") && spans.iter().any(|s| !s.mono && s.text.contains("now")))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_epub_2_book_has_the_contents_of_its_ncx() {
        let dir = folder("ncx");
        let path = dir.join("old.epub");
        std::fs::write(&path, book(&[], false)).expect("written");
        let contents = contents(&path).expect("reads").expect("has contents");
        let titles: Vec<&str> = contents.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["First (NCX)", "Second (NCX)", "Deep (NCX)"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_book_with_drm_is_not_opened_and_its_card_says_why() {
        let dir = folder("drm");
        let locked = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
<enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
<enc:CipherData><enc:CipherReference URI="OEBPS/text/one.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"#;
        let path = dir.join("locked.epub");
        std::fs::write(&path, book(&[("META-INF/encryption.xml", locked)], true)).expect("written");
        let err = render_page(&path, 0).expect_err("refused");
        assert!(err.to_string().contains("DRM"), "{err}");
        let data = std::fs::read(&path).expect("read");
        let rows = properties(&Archive::new(&data).expect("a zip"));
        assert!(rows.iter().any(|(l, _)| l == "Protected"), "{rows:?}");
        assert!(rows.iter().any(|(l, v)| l == "Title" && v == "A Test Book"));
        // fonts obfuscated by the IDPF's way are no DRM
        let fonts = br#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
<enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/></enc:EncryptedData></encryption>"#;
        let path = dir.join("fonts.epub");
        std::fs::write(&path, book(&[("META-INF/encryption.xml", fonts)], true)).expect("written");
        assert!(render_page(&path, 0).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_card_says_what_the_package_does() {
        let data = book(&[], true);
        let archive = Archive::new(&data).expect("a zip");
        assert!(is_epub_archive(&archive));
        let rows = properties(&archive);
        let get = |label: &str| rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str());
        assert_eq!(get("Author"), Some("Anna Writer"));
        assert_eq!(get("Language"), Some("en"));
        assert_eq!(get("ISBN"), Some("9780000000002"));
        assert_eq!(get("Parts"), Some("3"));
        assert_eq!(get("Pictures"), Some("2"));
    }

    #[test]
    fn a_damaged_or_hostile_book_is_an_error_or_a_note_and_not_a_crash() {
        let dir = folder("hostile");
        let mut whole = book(&[], true);
        whole.truncate(whole.len() / 2);
        let no_package = crate::archive::tests::zip(&[("mimetype", b"application/epub+zip", false)]);
        for (name, data) in [
            ("cut.epub", whole),
            ("nothing.epub", Vec::new()),
            ("text.epub", b"not a zip at all".to_vec()),
            ("bare.epub", no_package),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, data).expect("written");
            assert!(render_page(&path, 0).is_err(), "{name}");
            assert!(render_first_page(&path).is_err(), "{name}");
        }
        // a chapter that is no XML, and one nested far deeper than anyone writes: the book is still read
        let deep = format!("{}<p>Deep.</p>{}", "<div>".repeat(3000), "</div>".repeat(3000));
        let data = book(&[], true);
        let archive = Archive::new(&data).expect("a zip");
        let mut files: Vec<(String, Vec<u8>)> = archive
            .names()
            .map(|n| (n.to_string(), archive.read(n, MAX_PART).expect("an entry")))
            .collect();
        for (name, bytes) in &mut files {
            if name.ends_with("one.xhtml") {
                *bytes = b"<html><body><p>broken".to_vec();
            }
            if name.ends_with("two.xhtml") {
                *bytes = chapter("Deep", &deep).into_bytes();
            }
        }
        let entries: Vec<(&str, &[u8], bool)> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice(), true)).collect();
        let path = dir.join("broken.epub");
        std::fs::write(&path, crate::archive::tests::zip(&entries)).expect("written");
        let texts = page_texts(&path).expect("reads").expect("has text");
        assert!(texts.iter().any(|t| t.contains("could not be read")), "{texts:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn xhtml_is_made_ready_for_the_xml_parser() {
        let ready =
            prepare("<!DOCTYPE html [ <!ENTITY x \"y\"> ]><p>a&nbsp;b &amp; c &#169; &unknownname; R&D &hellip;</p>");
        assert_eq!(
            ready,
            "<p>a\u{A0}b &amp; c &#169; &amp;unknownname; R&amp;D \u{2026}</p>"
        );
        assert_eq!(unescape("pic%201.png"), "pic 1.png");
        assert_eq!(
            target("OEBPS/text/one.xhtml", "../images/a.png#x"),
            ("OEBPS/images/a.png".to_string(), Some("x".to_string()))
        );
        assert_eq!(target("OEBPS/text/one.xhtml", "#here").0, "OEBPS/text/one.xhtml");
    }
}
