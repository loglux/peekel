//! FictionBook (FB2), the XML e-book common in Russian-language collections: one XML file (often in a ZIP, `.fb2.zip`) with the text
//! in sections, the description of the book, and the pictures in it as base64 (`binary`). Read into the blocks of `book`, which lays
//! them out into pages: the cover, the title, the sections (each top one on a new page, its title a line of the contents), paragraphs
//! with emphasis and strong, poems, epigraphs, citations, pictures, and the notes at the end.
//!
//! Limits: the file (`MAX_BYTES`), the XML (`MAX_XML`, and the parser's own on nodes and depth), the pictures (how many, their pixels),
//! the depth of sections that is followed. The encodings read: UTF-8, UTF-16 (with its mark), Windows-1251 (most old Russian FB2) and
//! ISO-8859-1; another is an error that says which.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use image::RgbaImage;
use roxmltree::Node;

use crate::archive::Archive;
use crate::book::{self, Block, SIZE, Span, Style};
use crate::decode::{ContentsItem, DecodeError};
use crate::guard::{read_limited, unreadable};
use crate::xml::{attr, child, children};

/// A bigger file is refused before it is read.
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
/// The largest XML unpacked from a ZIP.
const MAX_XML: usize = 256 * 1024 * 1024;
/// Sections deeper than this are read as if they were at this depth.
const MAX_DEPTH: usize = 32;
/// The most blocks a book makes.
const MAX_BLOCKS: usize = 4_000_000;
/// The longest line of the contents, in characters.
const MAX_TITLE: usize = 200;

/// Whether the name says FB2: `.fb2`, `.fb2.zip` or `.fbz`.
pub fn is_fb2(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.ends_with(".fb2") || name.ends_with(".fb2.zip") || name.ends_with(".fbz")
}

/// The XML of the book as text: the file itself, or the first `.fb2` in it when it is a ZIP.
fn text_of(path: &Path) -> Result<String, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    if data.starts_with(b"PK") {
        let archive = Archive::new(&data)?;
        let name = archive
            .names()
            .find(|n| n.to_ascii_lowercase().ends_with(".fb2"))
            .map(str::to_string)
            .ok_or_else(|| unreadable("the ZIP has no .fb2 file in it"))?;
        return decode_text(&archive.read(&name, MAX_XML)?);
    }
    decode_text(&data)
}

/// The bytes of an XML file as text, by its byte order mark or the encoding its declaration names.
pub(crate) fn decode_text(data: &[u8]) -> Result<String, DecodeError> {
    if let Some(rest) = data.strip_prefix(b"\xEF\xBB\xBF") {
        return Ok(String::from_utf8_lossy(rest).into_owned());
    }
    for (mark, little) in [(b"\xFF\xFE", true), (b"\xFE\xFF", false)] {
        if let Some(rest) = data.strip_prefix(mark) {
            let units = rest.as_chunks::<2>().0.iter().map(|&pair| {
                if little {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            });
            return Ok(char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect());
        }
    }
    let encoding = declared_encoding(data).unwrap_or_else(|| "utf-8".to_string());
    decode_as(data, &encoding)
}

/// `data` (with no byte order mark) in the encoding named `encoding` (lower case): UTF-8, Windows-1251 or ISO-8859-1; another is an
/// error that names it.
pub(crate) fn decode_as(data: &[u8], encoding: &str) -> Result<String, DecodeError> {
    match encoding {
        "utf-8" | "utf8" => Ok(String::from_utf8_lossy(data).into_owned()),
        "windows-1251" | "cp1251" | "cp-1251" | "win-1251" => Ok(data.iter().map(|&b| cp1251(b)).collect()),
        "iso-8859-1" | "latin1" | "latin-1" => Ok(data.iter().map(|&b| char::from(b)).collect()),
        other => Err(unreadable(format!("the encoding {other} is not read"))),
    }
}

/// The `encoding` of the XML declaration at the start, in lower case.
fn declared_encoding(data: &[u8]) -> Option<String> {
    let head = &data[..data.len().min(256)];
    let head: String = head
        .iter()
        .take_while(|&&b| b != b'>')
        .map(|&b| char::from(b))
        .collect();
    if !head.trim_start().starts_with("<?xml") {
        return None;
    }
    let at = head.find("encoding")? + "encoding".len();
    let rest = head[at..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    let value: String = rest[1..].chars().take_while(|c| *c != quote).collect();
    Some(value.trim().to_ascii_lowercase())
}

/// The letters of Windows-1251 from 0x80 to 0xBF; from 0xC0 on they are А to я in order.
const CP1251_HIGH: [u16; 64] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C,
    0x040B, 0x040F, 0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0xFFFD, 0x2122, 0x0459, 0x203A,
    0x045A, 0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9,
    0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407, 0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7,
    0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457,
];

fn cp1251(b: u8) -> char {
    let code = match b {
        0x00..=0x7F => u32::from(b),
        0x80..=0xBF => u32::from(CP1251_HIGH[usize::from(b - 0x80)]),
        _ => 0x0410 + u32::from(b - 0xC0),
    };
    char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// The text of a node, its white space made single spaces.
pub(crate) fn plain_text(node: Node<'_, '_>) -> String {
    let text: String = node
        .descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<Vec<_>>()
        .join(" ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What reading a book has made, and the pictures it may still decode.
struct Reader<'a, 'i> {
    /// The pictures of the book by their id: the node of the `binary`.
    binaries: HashMap<&'a str, Node<'a, 'i>>,
    pictures: book::Pictures,
    blocks: Vec<Block>,
}

impl<'a, 'i> Reader<'a, 'i> {
    fn push(&mut self, block: Block) {
        if self.blocks.len() < MAX_BLOCKS {
            self.blocks.push(block);
        }
    }

    /// The picture a link (`#cover.jpg`) leads to, decoded once; `None` when there is none or it cannot be read, or the limits are met.
    fn picture(&mut self, href: &str) -> Option<Arc<RgbaImage>> {
        let id = href.trim().trim_start_matches('#');
        let binaries = &self.binaries;
        self.pictures.get(id, || {
            let text = binaries.get(id)?.text()?;
            let compact: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(&compact)
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact))
                .ok()
        })
    }

    /// The spans of a paragraph: its text, with the styles of the elements it is in.
    fn spans(node: Node<'_, '_>, base: &Span, out: &mut Vec<Span>) {
        for c in node.children() {
            if c.is_text() {
                if let Some(text) = c.text() {
                    match out.last_mut() {
                        Some(last)
                            if last.bold == base.bold
                                && last.italic == base.italic
                                && last.mono == base.mono
                                && last.raised == base.raised =>
                        {
                            last.text.push_str(text);
                        }
                        _ => out.push(Span {
                            text: text.to_string(),
                            ..base.clone()
                        }),
                    }
                }
                continue;
            }
            if !c.is_element() {
                continue;
            }
            let mut style = base.clone();
            match c.tag_name().name() {
                "strong" => style.bold = true,
                "emphasis" => style.italic = !base.italic,
                "code" => style.mono = true,
                "sup" => style.raised = true,
                "a" if attr(c, "type") == Some("note") => style.raised = true,
                "image" => continue,
                _ => {}
            }
            Self::spans(c, &style, out);
        }
    }

    fn paragraph(&mut self, node: Node<'_, '_>, style: Style) {
        let mut spans = Vec::new();
        Self::spans(node, &Span::default(), &mut spans);
        if spans.iter().any(|s| !s.text.trim().is_empty()) {
            self.push(Block::Paragraph { spans, style });
        }
    }

    /// The paragraphs of a title, each in the style of a heading of `depth`.
    fn title(&mut self, node: Node<'_, '_>, depth: usize) {
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "empty-line" => self.push(Block::Space(SIZE * 0.6)),
                _ => self.paragraph(c, Style::heading(depth)),
            }
        }
    }

    /// The lines of a poem: its title, its stanzas (their lines set in), the author and the date.
    fn poem(&mut self, node: Node<'_, '_>, left: f32, italic: bool) {
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "title" => self.title(c, 3),
                "epigraph" => self.epigraph(c),
                "stanza" => {
                    self.push(Block::Space(SIZE * 0.6));
                    for line in c.children().filter(|l| l.is_element()) {
                        match line.tag_name().name() {
                            "title" => self.title(line, 3),
                            "subtitle" => self.paragraph(line, centred()),
                            _ => self.paragraph(
                                line,
                                Style {
                                    italic,
                                    ..Style::verse(left + SIZE * 3.0)
                                },
                            ),
                        }
                    }
                }
                "text-author" | "date" => self.paragraph(c, signed()),
                _ => {}
            }
        }
        self.push(Block::Space(SIZE * 0.6));
    }

    /// An epigraph: in italics, set in to the right, its author under it.
    fn epigraph(&mut self, node: Node<'_, '_>) {
        let left = book::PAGE.0 * 0.3;
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "p" => self.paragraph(
                    c,
                    Style {
                        italic: true,
                        ..Style::verse(left)
                    },
                ),
                "poem" => self.poem(c, left - SIZE * 3.0, true),
                "cite" => self.cite(c),
                "empty-line" => self.push(Block::Space(SIZE * book::LEADING)),
                "text-author" => self.paragraph(c, signed()),
                _ => {}
            }
        }
        self.push(Block::Space(SIZE));
    }

    /// A citation: set in on both sides, its author under it.
    fn cite(&mut self, node: Node<'_, '_>) {
        let style = Style {
            left: SIZE * 2.0,
            right: SIZE * 2.0,
            ..Style::body()
        };
        self.push(Block::Space(SIZE * 0.4));
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "p" => self.paragraph(c, style.clone()),
                "subtitle" => self.paragraph(c, centred()),
                "poem" => self.poem(c, SIZE * 2.0, false),
                "empty-line" => self.push(Block::Space(SIZE * book::LEADING)),
                "text-author" => self.paragraph(c, signed()),
                "table" => self.table(c),
                _ => {}
            }
        }
        self.push(Block::Space(SIZE * 0.4));
    }

    /// A table, a row a line, its cells parted by a bar: enough to read it, not a drawing of it.
    fn table(&mut self, node: Node<'_, '_>) {
        for row in children(node, "tr") {
            let cells: Vec<String> = row.children().filter(|c| c.is_element()).map(plain_text).collect();
            self.push(Block::Paragraph {
                spans: vec![Span {
                    text: cells.join("  \u{2502}  "),
                    ..Span::default()
                }],
                style: Style::verse(0.0),
            });
        }
    }

    /// A body or a section and what is in it; `depth` 0 for a body. In the notes (`notes`) a section's title is no line of the contents
    /// and starts no page.
    fn section(&mut self, node: Node<'_, '_>, depth: usize, notes: bool) {
        let depth = depth.min(MAX_DEPTH);
        for c in node.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "title" => {
                    if notes && depth > 0 {
                        self.paragraph_all(
                            c,
                            Style {
                                bold: true,
                                align: book::Align::Left,
                                indent: false,
                                before: SIZE * 0.5,
                                ..Style::body()
                            },
                        );
                    } else {
                        let title: String = plain_text(c).chars().take(MAX_TITLE).collect();
                        if !title.is_empty() {
                            self.push(Block::Mark {
                                depth: depth.saturating_sub(1),
                                title,
                            });
                        }
                        self.title(c, depth);
                    }
                }
                "epigraph" => self.epigraph(c),
                "annotation" => {
                    for p in c.children().filter(|p| p.is_element()) {
                        self.paragraph(
                            p,
                            Style {
                                italic: true,
                                ..Style::body()
                            },
                        );
                    }
                }
                "image" => {
                    let image = attr(c, "href").and_then(|h| self.picture(h));
                    self.push(Block::Picture(image));
                }
                "section" => {
                    if depth == 0 && !notes {
                        self.push(Block::NewPage);
                    }
                    self.section(c, depth + 1, notes);
                }
                "p" => self.paragraph(c, Style::body()),
                "subtitle" => self.paragraph(c, centred()),
                "empty-line" => self.push(Block::Space(SIZE * book::LEADING)),
                "poem" => self.poem(c, 0.0, false),
                "cite" => self.cite(c),
                "table" => self.table(c),
                _ => {}
            }
        }
    }

    /// The paragraphs of a node, all in one style.
    fn paragraph_all(&mut self, node: Node<'_, '_>, style: Style) {
        for p in node.children().filter(|p| p.is_element()) {
            self.paragraph(p, style.clone());
        }
    }
}

fn centred() -> Style {
    Style {
        bold: true,
        align: book::Align::Center,
        indent: false,
        before: SIZE * 0.4,
        after: SIZE * 0.4,
        ..Style::body()
    }
}

/// The author of an epigraph or a poem: on the right, in italics.
fn signed() -> Style {
    Style {
        italic: true,
        align: book::Align::Right,
        indent: false,
        ..Style::body()
    }
}

/// The blocks of the book at `path`.
pub fn blocks(path: &Path) -> Result<Vec<Block>, DecodeError> {
    let text = text_of(path)?;
    let document = crate::xml::parse(&text)?;
    let root = document.root_element();
    if root.tag_name().name() != "FictionBook" {
        return Err(unreadable("not a FictionBook file"));
    }
    let mut reader = Reader {
        binaries: children(root, "binary")
            .filter_map(|b| attr(b, "id").map(|id| (id, b)))
            .collect(),
        pictures: book::Pictures::default(),
        blocks: Vec::new(),
    };
    let title_info = child(root, "description").and_then(|d| child(d, "title-info"));
    if let Some(cover) = title_info
        .and_then(|t| child(t, "coverpage"))
        .and_then(|c| child(c, "image"))
        .and_then(|i| attr(i, "href"))
        .and_then(|h| reader.picture(h))
    {
        reader.push(Block::Cover(cover));
    }
    let mut first = true;
    for body in children(root, "body") {
        let notes = !first;
        if notes {
            reader.push(Block::NewPage);
            if child(body, "title").is_none() {
                reader.push(Block::Mark {
                    depth: 0,
                    title: match attr(body, "name") {
                        Some("comments") => "Comments",
                        _ => "Notes",
                    }
                    .to_string(),
                });
            }
        }
        reader.section(body, 0, notes);
        first = false;
    }
    if !reader.blocks.iter().any(|b| matches!(b, Block::Paragraph { .. })) && first {
        return Err(unreadable("the book has no body"));
    }
    Ok(reader.blocks)
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

/// The contents: the titles of the sections and the pages they start on.
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

/// What the description of the book says, for its card: title, authors, genres, language, date, series, publisher, year, ISBN, and how
/// many pictures it has.
pub fn properties(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = text_of(path) else {
        return Vec::new();
    };
    let Ok(document) = crate::xml::parse(&text) else {
        return Vec::new();
    };
    let root = document.root_element();
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut push = |label: &str, value: String| {
        let value: String = value.chars().take(MAX_TITLE).collect();
        if !value.trim().is_empty() {
            rows.push((label.to_string(), value));
        }
    };
    let description = child(root, "description");
    if let Some(info) = description.and_then(|d| child(d, "title-info")) {
        push("Title", child(info, "book-title").map(plain_text).unwrap_or_default());
        let authors: Vec<String> = children(info, "author")
            .map(|a| {
                ["first-name", "middle-name", "last-name", "nickname"]
                    .iter()
                    .filter_map(|n| child(a, n).map(plain_text))
                    .filter(|s| !s.is_empty())
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|a| !a.is_empty())
            .collect();
        push(if authors.len() > 1 { "Authors" } else { "Author" }, authors.join(", "));
        let genres: Vec<String> = children(info, "genre").map(plain_text).collect();
        push("Genre", genres.join(", "));
        push("Language", child(info, "lang").map(plain_text).unwrap_or_default());
        push("Date", child(info, "date").map(plain_text).unwrap_or_default());
        if let Some(sequence) = child(info, "sequence") {
            let name = attr(sequence, "name").unwrap_or("");
            let number = attr(sequence, "number").map_or(String::new(), |n| format!(", {n}"));
            push("Series", format!("{name}{number}"));
        }
    }
    if let Some(publish) = description.and_then(|d| child(d, "publish-info")) {
        push(
            "Publisher",
            child(publish, "publisher").map(plain_text).unwrap_or_default(),
        );
        push("Year", child(publish, "year").map(plain_text).unwrap_or_default());
        push("ISBN", child(publish, "isbn").map(plain_text).unwrap_or_default());
    }
    let pictures = children(root, "binary").count();
    if pictures > 0 {
        push("Pictures", pictures.to_string());
    }
    rows
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use std::io::Write;

    /// A folder of its own for the files of one test.
    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-fb2-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp folder");
        dir
    }

    fn png(width: u32, height: u32) -> String {
        let image = RgbaImage::from_pixel(width, height, image::Rgba([30, 90, 200, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("a PNG");
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    const NS: &str = r#"xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink""#;

    fn book(body: &str) -> String {
        format!(
            r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook {NS}>
<description><title-info><genre>prose</genre><author><first-name>Anna</first-name><last-name>Writer</last-name></author>
<book-title>A Test Book</book-title><coverpage><image l:href="#cover.png"/></coverpage><lang>en</lang>
<sequence name="Tests" number="2"/></title-info>
<publish-info><publisher>Nobody</publisher><year>2026</year></publish-info></description>
<body><title><p>Anna Writer</p><p>A Test Book</p></title>{body}</body>
<body name="notes"><section id="n1"><title><p>1</p></title><p>The note.</p></section></body>
<binary id="cover.png" content-type="image/png">{}</binary>
</FictionBook>"##,
            png(60, 90)
        )
    }

    fn chapters() -> String {
        let text = "It was a quiet morning and the words went on. ".repeat(60);
        format!(
            r##"<section><title><p>Chapter One</p></title><epigraph><p>A saying.</p><text-author>Someone</text-author></epigraph>
<p>{text}</p><p>A <emphasis>word</emphasis> and a <strong>strong</strong> one<a l:href="#n1" type="note">1</a>.</p>
<poem><stanza><v>A line of verse,</v><v>and another.</v></stanza></poem></section>
<section><title><p>Chapter Two</p></title><p>The needle is here.</p><image l:href="#cover.png"/></section>"##
        )
    }

    #[test]
    fn a_book_is_laid_out_with_its_cover_contents_and_notes() {
        let dir = folder("book");
        let path = dir.join("test.fb2");
        std::fs::write(&path, book(&chapters())).expect("written");
        let (cover, count) = render_page(&path, 0).expect("the cover");
        assert!(count >= 5, "{count} pages");
        // the cover fills the page: blue in the middle
        let [r, g, b, _] = cover.get_pixel(cover.width() / 2, cover.height() / 2).0;
        assert!(b > 150 && r < 80 && g < 130, "{r} {g} {b}");
        let contents = contents(&path).expect("reads").expect("has contents");
        let titles: Vec<&str> = contents.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Anna Writer A Test Book", "Chapter One", "Chapter Two", "Notes"]
        );
        let texts = page_texts(&path).expect("reads").expect("has text");
        let two = contents[2].page.expect("a page");
        assert!(texts[two].contains("The needle is here."), "{:?}", texts[two]);
        assert!(texts.last().is_some_and(|t| t.contains("The note.")));
        let (mut page, _) = render_page(&path, two).expect("the page");
        assert_eq!(mark_words(&path, two, "needle", &mut page).expect("marks"), 1);
        let first = render_first_page(&path).expect("the first page");
        assert_eq!(first.dimensions(), cover.dimensions());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_card_says_what_the_description_does() {
        let dir = folder("card");
        let path = dir.join("card.fb2");
        std::fs::write(&path, book("<section><p>Text.</p></section>")).expect("written");
        let rows = properties(&path);
        let get = |label: &str| rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str());
        assert_eq!(get("Title"), Some("A Test Book"));
        assert_eq!(get("Author"), Some("Anna Writer"));
        assert_eq!(get("Language"), Some("en"));
        assert_eq!(get("Series"), Some("Tests, 2"));
        assert_eq!(get("Publisher"), Some("Nobody"));
        assert_eq!(get("Pictures"), Some("1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn windows_1251_and_a_zip_are_read() {
        let dir = folder("encodings");
        let russian = "Глава первая. Съешь же ещё этих мягких французских булок, да выпей чаю. Ёж, «кавычки» — тире.";
        let xml = format!(
            r#"<?xml version="1.0" encoding="windows-1251"?><FictionBook {NS}><body><section><p>{russian}</p></section></body></FictionBook>"#
        );
        let encoded: Vec<u8> = xml
            .chars()
            .map(|c| {
                (0u8..=255)
                    .find(|&b| cp1251(b) == c)
                    .expect("every letter is in Windows-1251")
            })
            .collect();
        let path = dir.join("cp1251.fb2");
        std::fs::write(&path, &encoded).expect("written");
        let texts = page_texts(&path).expect("reads").expect("has text");
        assert!(texts[0].contains("Съешь же ещё"), "{:?}", texts[0]);
        assert!(texts[0].contains("«кавычки» — тире."), "{:?}", texts[0]);

        let zipped = dir.join("book.fb2.zip");
        let mut file = std::fs::File::create(&zipped).expect("created");
        file.write_all(&crate::archive::tests::zip(&[("inner.fb2", &encoded, true)]))
            .expect("written");
        drop(file);
        assert!(is_fb2(&zipped));
        let texts = page_texts(&zipped).expect("reads").expect("has text");
        assert!(texts[0].contains("Ёж"), "{:?}", texts[0]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_or_hostile_file_is_an_error_and_not_a_crash() {
        let dir = folder("hostile");
        let cases: [(&str, Vec<u8>); 5] = [
            ("cut.fb2", book(&chapters()).as_bytes()[..900].to_vec()),
            ("other.fb2", b"<?xml version=\"1.0\"?><html><body/></html>".to_vec()),
            (
                "koi8.fb2",
                b"<?xml version=\"1.0\" encoding=\"koi8-r\"?><FictionBook/>".to_vec(),
            ),
            ("empty.fb2", Vec::new()),
            ("bad-zip.fb2.zip", b"PK\x03\x04 not really a zip".to_vec()),
        ];
        for (name, data) in cases {
            let path = dir.join(name);
            std::fs::write(&path, data).expect("written");
            assert!(render_page(&path, 0).is_err(), "{name}");
            assert!(render_first_page(&path).is_err(), "{name}");
        }
        // sections nested far deeper than anyone writes, and a picture that is no picture: read, with limits
        let deep = format!(
            "{}<p>Deep.</p><image l:href=\"#nothing\"/>{}",
            "<section>".repeat(500),
            "</section>".repeat(500)
        );
        let path = dir.join("deep.fb2");
        std::fs::write(&path, book(&deep)).expect("written");
        let texts = page_texts(&path).expect("reads").expect("has text");
        assert!(texts.iter().any(|t| t.contains("Deep.")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_name_says_fb2() {
        assert!(is_fb2(Path::new("a/Book.FB2")));
        assert!(is_fb2(Path::new("book.fb2.zip")));
        assert!(is_fb2(Path::new("book.fbz")));
        assert!(!is_fb2(Path::new("book.zip")));
        assert!(!is_fb2(Path::new("book.epub")));
    }
}
