//! HTML, to look at in the viewer: a page read as a book whose text flows (`book`), as an EPUB part is. HTML is not XML (tags left
//! open, attributes with no quotes, `<br>`, the rules of a browser for what is broken), so it is parsed by `html5ever` (the parser of
//! Servo, by the rules of HTML5) into a tree of our own, which is written out as XML and read by the reader of EPUB (`epub::page_blocks`):
//! the same headings, paragraphs, lists, tables, listings and pictures. The headings `h1` to `h3` are the contents. The style sheets are
//! read only for the classes set in a monospace font, as in an EPUB; scripts, forms and frames are passed over, so a page that lives by
//! them (a web application) shows little.
//!
//! Pictures are read by the path the page gives relative to its folder (a saved page's `name_files`, a site's `../images`), or from the
//! link itself (`data:`); a link to the network, to another drive or to the root is not followed, and a picture that cannot be read is
//! passed over (a web page has many: icons, SVG, pictures on the network). A `nav` (a site's menu) is passed over. The plugin leaves an
//! HTML file to Far, which shows it as text.
//!
//! Limits: the file (`MAX_BYTES`), the nodes of the tree (`MAX_NODES`), the depth written out (`MAX_DEPTH`; deeper elements give their
//! text to the one above), and those of `book` and of the XML parser.

use std::borrow::Cow;
use std::cell::{Cell, Ref, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, QualName, ns, parse_document};
use image::RgbaImage;

use crate::book::{self, Block};
use crate::decode::{ContentsItem, DecodeError};
use crate::fb2::decode_as;
use crate::guard::{read_limited, unreadable};

/// A bigger file is refused before it is read.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Nodes made at most; what the parser makes past them is dropped.
const MAX_NODES: usize = 3_000_000;
/// Elements nested deeper than this are written out without their tags.
const MAX_DEPTH: usize = 256;
/// The reading stops at an element nested deeper than this, or after `MAX_TIME`: the parser's work grows as the square of the
/// elements left open (100 000 open `div`s took 22 s), and no page that is read needs so many.
const MAX_NEST: usize = 512;
const MAX_TIME: Duration = Duration::from_secs(10);
/// The text is given to the parser in pieces of this many bytes, so that the limits are looked at between them.
const PIECE: usize = 16 * 1024;

/// Whether the name says HTML.
pub fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["html", "htm", "xhtml"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A node of the tree.
struct Node {
    /// The element's name (an empty one for any other node, whose name the parser never asks).
    name: QualName,
    attrs: Vec<(String, String)>,
    text: Option<String>,
    element: bool,
    parent: Option<usize>,
    children: Vec<usize>,
    /// The contents of a `template` (never shown).
    template: Option<usize>,
    /// How deep it is in the tree, when it was put there.
    depth: usize,
}

impl Node {
    fn new(name: QualName, element: bool) -> Self {
        Self {
            name,
            attrs: Vec::new(),
            text: None,
            element,
            parent: None,
            children: Vec::new(),
            template: None,
            depth: 0,
        }
    }
}

/// The document being built (`html5ever` calls it by a shared reference).
struct Tree {
    nodes: RefCell<Vec<Node>>,
    /// Set when an element is put deeper than `MAX_NEST`: the reading stops there (`xml_of`).
    deep: Rc<Cell<bool>>,
}

/// The document is node 0; node 1 is where what passes the limit goes, never written out.
const DOCUMENT: usize = 0;
const VOID: usize = 1;

fn empty_name() -> QualName {
    QualName::new(None, ns!(), LocalName::from(""))
}

impl Tree {
    fn new(deep: Rc<Cell<bool>>) -> Self {
        let void = QualName::new(None, ns!(html), LocalName::from("div"));
        Self {
            nodes: RefCell::new(vec![Node::new(empty_name(), false), Node::new(void, true)]),
            deep,
        }
    }

    /// A new node, or the void one past the limit.
    fn add(&self, node: Node) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        if nodes.len() >= MAX_NODES {
            return VOID;
        }
        nodes.push(node);
        nodes.len() - 1
    }

    fn detach(nodes: &mut [Node], child: usize) {
        if let Some(parent) = nodes[child].parent.take() {
            nodes[parent].children.retain(|&c| c != child);
        }
    }

    /// Puts `child` in `parent` at `at` (the end when `None`); text next to text is joined to it.
    fn insert(&self, parent: usize, at: Option<usize>, child: NodeOrText<usize>) {
        if parent == VOID {
            return;
        }
        let node = match child {
            NodeOrText::AppendNode(node) => node,
            NodeOrText::AppendText(text) => {
                {
                    let mut nodes = self.nodes.borrow_mut();
                    let before = match at {
                        Some(at) => at.checked_sub(1).map(|i| nodes[parent].children[i]),
                        None => nodes[parent].children.last().copied(),
                    };
                    if let Some(before) = before
                        && let Some(existing) = nodes[before].text.as_mut()
                    {
                        existing.push_str(&text);
                        return;
                    }
                }
                let mut node = Node::new(empty_name(), false);
                node.text = Some(text.to_string());
                self.add(node)
            }
        };
        if node == VOID || node == parent {
            return;
        }
        let mut nodes = self.nodes.borrow_mut();
        Self::detach(&mut nodes, node);
        let at = at
            .unwrap_or(nodes[parent].children.len())
            .min(nodes[parent].children.len());
        nodes[parent].children.insert(at, node);
        nodes[node].parent = Some(parent);
        let depth = nodes[parent].depth + 1;
        nodes[node].depth = depth;
        if depth > MAX_NEST {
            self.deep.set(true);
        }
    }
}

impl TreeSink for Tree {
    type Handle = usize;
    type Output = Self;
    type ElemName<'a> = Ref<'a, QualName>;

    fn finish(self) -> Self {
        self
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> usize {
        DOCUMENT
    }

    fn elem_name<'a>(&'a self, target: &'a usize) -> Ref<'a, QualName> {
        Ref::map(self.nodes.borrow(), |nodes| &nodes[*target].name)
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> usize {
        let mut node = Node::new(name, true);
        node.attrs = attrs
            .into_iter()
            .map(|a| (a.name.local.to_string(), a.value.to_string()))
            .collect();
        let element = self.add(node);
        if flags.template && element != VOID {
            let contents = self.add(Node::new(empty_name(), false));
            self.nodes.borrow_mut()[element].template = Some(contents);
        }
        element
    }

    fn create_comment(&self, _text: StrTendril) -> usize {
        self.add(Node::new(empty_name(), false))
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> usize {
        self.add(Node::new(empty_name(), false))
    }

    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        self.insert(*parent, None, child);
    }

    fn append_based_on_parent_node(&self, element: &usize, prev_element: &usize, child: NodeOrText<usize>) {
        let has_parent = self.nodes.borrow()[*element].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(&self, _name: StrTendril, _public_id: StrTendril, _system_id: StrTendril) {}

    fn get_template_contents(&self, target: &usize) -> usize {
        self.nodes.borrow()[*target].template.unwrap_or(VOID)
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn append_before_sibling(&self, sibling: &usize, new_node: NodeOrText<usize>) {
        let place = {
            let nodes = self.nodes.borrow();
            nodes[*sibling]
                .parent
                .and_then(|p| nodes[p].children.iter().position(|c| c == sibling).map(|i| (p, i)))
        };
        if let Some((parent, at)) = place {
            // a node moved from before its sibling in the same parent shifts the place by one
            if let NodeOrText::AppendNode(node) = &new_node {
                let nodes = self.nodes.borrow();
                if nodes[*node].parent == Some(parent)
                    && nodes[parent]
                        .children
                        .iter()
                        .position(|c| c == node)
                        .is_some_and(|i| i < at)
                {
                    drop(nodes);
                    self.insert(parent, Some(at - 1), new_node);
                    return;
                }
            }
            self.insert(parent, Some(at), new_node);
        }
    }

    fn add_attrs_if_missing(&self, target: &usize, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        let node = &mut nodes[*target];
        for a in attrs {
            let name = a.name.local.to_string();
            if !node.attrs.iter().any(|(n, _)| *n == name) {
                node.attrs.push((name, a.value.to_string()));
            }
        }
    }

    fn remove_from_parent(&self, target: &usize) {
        Self::detach(&mut self.nodes.borrow_mut(), *target);
    }

    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let mut nodes = self.nodes.borrow_mut();
        let children = std::mem::take(&mut nodes[*node].children);
        for &c in &children {
            nodes[c].parent = Some(*new_parent);
        }
        nodes[*new_parent].children.extend(children);
    }
}

/// Whether `name` may stand as the name of an element or an attribute in XML (the names of HTML all may; a made-up one may not).
fn xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Text made safe for XML: markup characters as entities, control characters out (a line break and a tab kept, for `pre`).
fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\n' | '\t' => out.push(c),
            '\r' => {}
            c if c.is_control() || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
}

/// The tree written out as XML: elements by their local names, attributes with names XML takes (each once), text escaped; comments
/// and the contents of templates left out. Not recursive: a page made to hurt is as deep as it likes.
fn to_xml(tree: &Tree) -> String {
    let nodes = tree.nodes.borrow();
    let mut out = String::new();
    // (node, whether its tag was written and must be closed): a node is met once going down and once coming back
    let mut stack: Vec<(usize, bool, bool)> = vec![(DOCUMENT, false, false)];
    let mut depth = 0usize;
    while let Some((index, open, back)) = stack.pop() {
        let node = &nodes[index];
        if back {
            if open {
                out.push_str("</");
                out.push_str(&node.name.local);
                out.push('>');
                depth -= 1;
            }
            continue;
        }
        if let Some(text) = &node.text {
            escape_text(text, &mut out);
            continue;
        }
        let write = node.element && depth < MAX_DEPTH && xml_name(&node.name.local);
        if write {
            out.push('<');
            out.push_str(&node.name.local);
            let mut seen: Vec<&str> = Vec::new();
            for (name, value) in &node.attrs {
                let name = name.as_str();
                if xml_name(name) && name != "xmlns" && !seen.contains(&name) {
                    seen.push(name);
                    out.push(' ');
                    out.push_str(name);
                    out.push_str("=\"");
                    escape_text(&value.replace(['\n', '\t'], " "), &mut out);
                    out.push('"');
                }
            }
            out.push('>');
            depth += 1;
        }
        stack.push((index, write, true));
        for &child in node.children.iter().rev() {
            stack.push((child, false, false));
        }
    }
    if out.is_empty() {
        out.push_str("<html/>");
    } else if !out.starts_with("<html") {
        out = format!("<html>{out}</html>");
    }
    out
}

/// The encoding a page says it is in, from its `<meta charset>` or `content="...; charset=..."` in the first 2 KB, in lower case.
fn meta_charset(data: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&data[..data.len().min(2048)]).to_ascii_lowercase();
    let at = head.find("charset=")? + "charset=".len();
    let name: String = head[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The text of a page: by its byte order mark, else by what its `meta` says (UTF-8, Windows-1251, Latin-1), else UTF-8.
fn page_text(data: &[u8]) -> Result<String, DecodeError> {
    if data.starts_with(b"\xEF\xBB\xBF") || data.starts_with(b"\xFF\xFE") || data.starts_with(b"\xFE\xFF") {
        return crate::fb2::decode_text(data);
    }
    let encoding = match meta_charset(data).as_deref() {
        None | Some("utf-8" | "utf8" | "us-ascii" | "ascii") => "utf-8".to_string(),
        Some("windows-1252" | "cp1252" | "iso-8859-1" | "latin1") => "iso-8859-1".to_string(),
        Some(other) => other.to_string(),
    };
    decode_as(data, &encoding)
}

/// HTML made XML (see `to_xml`), and whether the reading stopped before the end (`MAX_NEST`, `MAX_TIME`).
pub(crate) fn xml_of(text: &str) -> (String, bool) {
    let deep = Rc::new(Cell::new(false));
    let mut parser = parse_document(Tree::new(Rc::clone(&deep)), html5ever::ParseOpts::default());
    let start = Instant::now();
    let mut rest = text;
    while !rest.is_empty() {
        if deep.get() || start.elapsed() > MAX_TIME {
            break;
        }
        let mut end = rest.len().min(PIECE);
        while !rest.is_char_boundary(end) {
            end += 1;
        }
        parser.process(StrTendril::from(&rest[..end]));
        rest = &rest[end..];
    }
    let cut = !rest.is_empty();
    (to_xml(&parser.finish()), cut)
}

/// The blocks of the page at `path`.
pub fn blocks(path: &Path) -> Result<Vec<Block>, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    let (xml, cut) = xml_of(&page_text(&data)?);
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| unreadable("the page has no name"))?;
    let mut blocks = crate::epub::page_blocks(&xml, folder, &name)?;
    if cut {
        blocks.push(Block::Paragraph {
            spans: vec![book::Span {
                text: "[the rest of the page is not shown: its elements are nested too deep]".to_string(),
                italic: true,
                ..book::Span::default()
            }],
            style: book::Style::verse(0.0),
        });
    }
    Ok(blocks)
}

/// The first page.
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

/// The contents: the headings.
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
mod tests {
    use super::*;

    fn paragraphs(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { spans, .. } => Some(spans.iter().map(|s| s.text.as_str()).collect::<String>()),
                _ => None,
            })
            .collect()
    }

    fn page(name: &str, html: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-html-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("page.html");
        let _ = std::fs::write(&path, html);
        path
    }

    #[test]
    fn html_that_is_no_xml_is_made_xml_by_the_rules_of_a_browser() {
        let (xml, _) = xml_of(
            "<!DOCTYPE html><title>T</title><p>One &nbsp;&mdash; <b>bold <i>both</b> italic</i>\
             <p class=x id=a data-1=y>Two<br>three<!-- a comment --><script>if (a < b) {}</script>\
             <weird:tag x:y=1 1bad=2>in</weird:tag>",
        );
        let document = crate::xml::parse(&xml).unwrap_or_else(|e| panic!("{e}: {xml}"));
        let ps: Vec<_> = document.descendants().filter(|n| n.has_tag_name("p")).collect();
        assert_eq!(ps.len(), 2, "{xml}");
        assert!(
            xml.contains("One \u{a0}\u{2014} <b>bold <i>both</i></b><i> italic</i>"),
            "{xml}"
        );
        assert!(xml.contains("<br></br>three"), "{xml}");
        assert!(!xml.contains("comment"), "{xml}");
        assert!(xml.contains("in"), "a name XML does not take gives its text: {xml}");
    }

    #[test]
    fn a_page_is_read_as_a_book_with_its_headings_as_the_contents_and_its_pictures_from_its_folder() {
        let mut png = Vec::new();
        let _ = image::RgbaImage::new(4, 3).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        let up = format!("peekel-html-up-{}.png", std::process::id());
        let html = format!(
            "<html><head><meta charset=\"utf-8\"><style>.code {{ font-family: monospace }}</style></head><body>\
             <h1>A title</h1><p>Text of the page.\
             <h2>A part</h2><div class=code>let a = 1;<br>  let b = 2;</div>\
             <img src=\"page_files/a.png?v=2\"><img src=\"../{up}\"><img src=\"missing.png\"><nav><p>a menu</nav>\
             <img src=\"http://example.com/x.png\"><img src=\"C:/Windows/x.png\"><img src=\"/etc/x.png\"></body></html>"
        );
        let path = page("book", html.as_bytes());
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let _ = std::fs::create_dir_all(folder.join("page_files"));
        let _ = std::fs::write(folder.join("page_files").join("a.png"), &png);
        let above = folder.parent().map(|p| p.join(&up)).unwrap_or_default();
        let _ = std::fs::write(&above, &png);
        let blocks = blocks(&path).unwrap_or_else(|e| panic!("{e}"));
        let marks: Vec<(usize, &str)> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Mark { depth, title } => Some((*depth, title.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(marks, vec![(0, "A title"), (1, "A part")]);
        let text = paragraphs(&blocks);
        assert!(text.iter().any(|t| t == "Text of the page."), "{text:?}");
        assert!(text.iter().any(|t| t.starts_with("let a")), "{text:?}");
        let pictures: Vec<bool> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Picture(p) => Some(p.is_some()),
                _ => None,
            })
            .collect();
        // in its folder (the query cut off) and above it, as the page wrote them; one missing, the network, a drive, the root: passed over
        assert_eq!(pictures, vec![true, true], "{pictures:?}");
        assert!(
            !text.iter().any(|t| t.contains("a menu")),
            "the nav is passed over: {text:?}"
        );
        let _ = std::fs::remove_dir_all(folder);
        let _ = std::fs::remove_file(above);
    }

    #[test]
    fn the_quick_view_of_the_plugin_draws_the_first_page_and_not_the_source() {
        let path = page("quick", b"<!DOCTYPE html><h1>A title</h1><p>Text of the page.");
        assert!(
            crate::decode::is_special(&path),
            "the plugin asks core, not Far's text view"
        );
        assert!(crate::decode::is_special(Path::new("x.HTM")));
        let picture = crate::decode::decode_full(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(picture.width() > 0 && picture.height() > 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
    }

    #[test]
    fn a_page_in_windows_1251_says_so_in_its_meta() {
        // "Привет" in Windows-1251
        let mut html = b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\"><p>".to_vec();
        html.extend_from_slice(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]);
        let path = page("cp1251", &html);
        let text = paragraphs(&blocks(&path).unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(text, vec!["Привет".to_string()]);
        let _ = std::fs::remove_dir_all(path.parent().map(Path::to_path_buf).unwrap_or_default());
    }

    #[test]
    fn a_picture_in_the_link_itself_is_read() {
        let mut png = Vec::new();
        let _ = image::RgbaImage::new(2, 2).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        use base64::Engine;
        let html = format!(
            "<p>x</p><img src=\"data:image/png;base64,{}\"><img src=\"data:image/png;base64,!!!\">",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let path = page("data", html.as_bytes());
        let found: Vec<bool> = blocks(&path)
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .filter_map(|b| match b {
                Block::Picture(p) => Some(p.is_some()),
                _ => None,
            })
            .collect();
        assert_eq!(found, vec![true], "the spoilt one is passed over");
        let _ = std::fs::remove_dir_all(path.parent().map(Path::to_path_buf).unwrap_or_default());
    }

    #[test]
    fn a_hostile_page_is_an_error_or_a_page_and_not_a_crash() {
        // deep, unclosed, empty, binary, a page of scripts only
        // 100 000 open elements took the parser 22 s: the reading stops past `MAX_NEST`, what came before kept
        let deep = "<p>before</p>".to_string() + &"<div>".repeat(100_000) + "deep text";
        let start = Instant::now();
        let (xml, cut) = xml_of(&deep);
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(cut);
        assert!(crate::xml::parse(&xml).is_ok());
        assert!(xml.contains("before"));
        let tables = "<table><tr><td>".repeat(5_000) + "cell";
        assert!(crate::xml::parse(&xml_of(&tables).0).is_ok());
        let path = page("deep", deep.as_bytes());
        let text = paragraphs(&blocks(&path).unwrap_or_else(|e| panic!("{e}")));
        assert!(text.last().is_some_and(|t| t.contains("nested too deep")), "{text:?}");
        let _ = std::fs::remove_dir_all(path.parent().map(Path::to_path_buf).unwrap_or_default());
        for (name, bytes) in [
            ("empty", &b""[..]),
            ("binary", &b"\x00\x01\x02\xFF\xFE garbage <<<>>> &&&"[..]),
            ("scripts", &b"<script>alert(1)</script><style>p{}</style>"[..]),
        ] {
            let path = page(name, bytes);
            let _ = blocks(&path);
            let _ = std::fs::remove_dir_all(path.parent().map(Path::to_path_buf).unwrap_or_default());
        }
        assert!(meta_charset(b"<meta charset=koi8-r>").is_some_and(|c| c == "koi8-r"));
        assert!(
            page_text(b"<meta charset=koi8-r>").is_err(),
            "an encoding not read is said"
        );
    }
}
