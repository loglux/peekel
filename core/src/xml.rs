//! What the document formats (docx, pptx, xlsx) share in reading XML: parsing with limits, and finding things by their local
//! name (the namespace is not looked at: the prefixes differ from file to file, and the names do not).

use roxmltree::{Document, Node, ParsingOptions};

use crate::decode::DecodeError;

/// Parses `text`: DTDs are refused (so entities cannot multiply), the number of nodes is limited, and the parser has a limit
/// of depth of its own.
pub fn parse(text: &str) -> Result<Document<'_>, DecodeError> {
    Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 3_000_000,
            ..ParsingOptions::default()
        },
    )
    .map_err(|e| DecodeError::Unreadable(format!("the XML is damaged ({e})")))
}

/// `text` made safe to put in XML or SVG markup: control characters out, markup characters as entities.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().filter(|c| !c.is_control()) {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The first child element with this local name.
pub fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

/// The child elements with this local name.
pub fn children<'a, 'i>(node: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

/// The first descendant element with this local name.
pub fn descendant<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.descendants()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

/// The value of an attribute by its local name (`w:val` is `val`).
pub fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// The path of a part named by `target` in a relationship of the part at `from` (a path in the archive): relative to the
/// folder of `from`, or from the root when it starts with a slash; `..` and `.` are resolved and cannot climb out.
pub fn resolve(from: &str, target: &str) -> String {
    let joined = if let Some(absolute) = target.strip_prefix('/') {
        absolute.to_string()
    } else {
        let folder = from.rsplit_once('/').map_or("", |(folder, _)| folder);
        format!("{folder}/{target}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// The relationships of a part: id -> (type, target path in the archive), read from the `_rels` file beside it.
pub fn relationships(
    archive: &crate::archive::Archive<'_>,
    part: &str,
) -> std::collections::HashMap<String, (String, String)> {
    let (folder, name) = part.rsplit_once('/').unwrap_or(("", part));
    let rels_path = format!("{folder}/_rels/{name}.rels");
    let mut out = std::collections::HashMap::new();
    let Ok(bytes) = archive.read(&rels_path, 16 * 1024 * 1024) else {
        return out;
    };
    let text = String::from_utf8_lossy(&bytes);
    let Ok(xml) = parse(&text) else {
        return out;
    };
    for r in xml.root_element().children().filter(|c| c.is_element()) {
        if let (Some(id), Some(target)) = (attr(r, "Id"), attr(r, "Target"))
            && attr(r, "TargetMode") != Some("External")
        {
            out.insert(
                id.to_string(),
                (attr(r, "Type").unwrap_or("").to_string(), resolve(part, target)),
            );
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn markup_characters_become_entities_and_control_characters_go() {
        assert_eq!(escape("a<b>&\"'\u{7}"), "a&lt;b&gt;&amp;&quot;&apos;");
    }

    #[test]
    fn targets_are_resolved_against_the_part_and_cannot_climb_out_of_the_archive() {
        assert_eq!(resolve("word/document.xml", "media/a.png"), "word/media/a.png");
        assert_eq!(resolve("ppt/slides/slide1.xml", "../media/a.png"), "ppt/media/a.png");
        assert_eq!(resolve("ppt/slides/slide1.xml", "/ppt/media/a.png"), "ppt/media/a.png");
        assert_eq!(resolve("a/b.xml", "../../../x"), "x");
        assert_eq!(resolve("top.xml", "x.xml"), "x.xml");
    }

    #[test]
    fn local_names_are_found_whatever_the_prefix() {
        let doc = parse(r#"<p:a xmlns:p="u" xmlns:q="v"><q:b q:val="1"/><p:b/></p:a>"#).expect("parses");
        let root = doc.root_element();
        assert_eq!(attr(child(root, "b").expect("b"), "val"), Some("1"));
        assert_eq!(children(root, "b").count(), 2);
        assert!(descendant(root, "c").is_none());
        assert!(parse("<!DOCTYPE d [<!ENTITY a 'x'>]><d>&a;</d>").is_err());
    }
}
