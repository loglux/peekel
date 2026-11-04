//! The contents of a PDF: its outline (the bookmarks, `/Outlines` of the catalogue) as lines in reading order, each with how deep it is, its
//! title and the page it leads to.
//!
//! Made after the specification (ISO 32000-1:2008, 12.3.2 Destinations, 12.3.3 Document Outline, 7.9.6 Name Trees, 7.9.2.2 Text Strings;
//! `docs/internal/pdf.md`) on the objects that `hayro-syntax` gives. An item has a `/Dest` (an array, or a name or a string that stands for
//! one) or an `/A` action (`GoTo`, with a `/D` that is the same); the first element of a destination array is a reference to a page object,
//! which is matched against the page objects of the document; a name is looked up in the catalogue's `/Dests` dictionary and in the `/Dests`
//! name tree of its `/Names`. An item whose destination does not give a page of this document (a remote one, a name that is not found) is a
//! line with no page.
//!
//! A file made to hurt (a loop of `/Next` or `/First`, a tree a million deep, a name tree that loops) is met by counting: the items and the
//! depth of the outline, and the nodes looked at for a name, are limited, and an item that is met a second time ends the walk of its chain. The
//! reading is done on a thread of its own (`pdf::on_thread`) with a time limit. `/Last`, `/Prev` and `/Count` are not trusted and not used.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use hayro::hayro_syntax::object::String as PdfString;
use hayro::hayro_syntax::object::{Array, Dict, MaybeRef, Object, ObjectIdentifier};
use hayro::hayro_syntax::{LoadPdfError, Pdf};

use crate::decode::{ContentsItem, DecodeError};
use crate::guard::unreadable;
use crate::pdf::{MAX_BYTES, on_thread, text_string};

/// The most outline items that are read, and how deep the outline may go.
const MAX_ITEMS: usize = 20_000;
const MAX_DEPTH: usize = 32;
/// The nodes of a name tree that are looked at for one name, and how deep the tree may go.
const MAX_TREE_NODES: usize = 20_000;
const MAX_TREE_DEPTH: usize = 32;
/// How long the caller waits.
const TIMEOUT: Duration = Duration::from_secs(20);
/// A title is cut to this many characters.
const MAX_TITLE: usize = 300;

/// The contents of the PDF in `path`: its outline as lines in reading order, or `None` when it has none.
///
/// # Errors
/// An unreadable file, a file too big, a file with a password, a reading that takes too long.
pub fn contents(path: &Path) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    if std::fs::metadata(path).map_err(unreadable)?.len() > MAX_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let data = std::fs::read(path).map_err(unreadable)?;
    on_thread(TIMEOUT, move || read_contents(data))?
}

/// The outline of the PDF in `data` (see [`contents`]).
fn read_contents(data: Vec<u8>) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    let pdf = match Pdf::new(data) {
        Ok(pdf) => pdf,
        Err(LoadPdfError::Decryption(_)) => return Err(unreadable("the PDF is protected with a password")),
        Err(_) => return Err(unreadable("the PDF could not be read")),
    };
    let xref = pdf.xref();
    let Some(root) = xref.get::<Dict<'_>>(xref.root_id()) else {
        return Ok(None);
    };
    let Some(outlines) = root.get::<Dict<'_>>(b"Outlines".as_slice()) else {
        return Ok(None);
    };
    // the page objects of the document, to turn the reference in a destination into a page number
    let pages: HashMap<ObjectIdentifier, usize> = pdf
        .pages()
        .iter()
        .enumerate()
        .filter_map(|(number, page)| page.raw().obj_id().map(|id| (id, number)))
        .collect();
    let resolver = Resolver { root, pages };

    let mut items: Vec<ContentsItem> = Vec::new();
    let mut seen: HashSet<ObjectIdentifier> = HashSet::new();
    // the chains still to walk: the next item of a level and how deep that level is; a stack, so that the children of an item come before
    // its next sibling
    let mut stack: Vec<(Option<Dict<'_>>, usize)> = vec![(outlines.get::<Dict<'_>>(b"First".as_slice()), 0)];
    while let Some((node, depth)) = stack.pop() {
        let Some(item) = node else {
            continue;
        };
        if items.len() >= MAX_ITEMS {
            break;
        }
        // an item that is met again is a loop: this chain ends here
        if let Some(id) = item.obj_id()
            && !seen.insert(id)
        {
            continue;
        }
        let title = item
            .get::<PdfString<'_>>(b"Title".as_slice())
            .map(|t| text_string(t.as_bytes(), MAX_TITLE))
            .unwrap_or_default();
        items.push(ContentsItem {
            depth,
            title,
            page: resolver.page_of_item(&item),
        });
        stack.push((item.get::<Dict<'_>>(b"Next".as_slice()), depth));
        if depth + 1 < MAX_DEPTH {
            stack.push((item.get::<Dict<'_>>(b"First".as_slice()), depth + 1));
        }
    }
    Ok((!items.is_empty()).then_some(items))
}

/// What turns a destination into a page number of the document.
struct Resolver<'a> {
    root: Dict<'a>,
    pages: HashMap<ObjectIdentifier, usize>,
}

impl<'a> Resolver<'a> {
    /// The page (from 0) that an outline item leads to: by its `/Dest`, or by the `/D` of its `GoTo` action.
    fn page_of_item(&self, item: &Dict<'a>) -> Option<usize> {
        let dest = item.get::<Object<'a>>(b"Dest".as_slice()).or_else(|| {
            let action = item.get::<Dict<'a>>(b"A".as_slice())?;
            let kind = action.get::<hayro::hayro_syntax::object::Name<'a>>(b"S".as_slice())?;
            let kind: &[u8] = kind.as_ref();
            (kind == b"GoTo").then(|| action.get::<Object<'a>>(b"D".as_slice()))?
        })?;
        self.page_of(dest)
    }

    /// The page of a destination: an array, or a name or a string that stands for one (or for a dictionary that has one under `/D`).
    fn page_of(&self, dest: Object<'a>) -> Option<usize> {
        let array = match dest {
            Object::Array(array) => array,
            Object::Dict(dict) => dict.get::<Array<'a>>(b"D".as_slice())?,
            Object::Name(name) => {
                let key: &[u8] = name.as_ref();
                self.named(key)?
            }
            Object::String(string) => self.named(string.as_bytes())?,
            _ => return None,
        };
        // the first element is a reference to a page object (a number, for a page of another file, leads nowhere here)
        match array.raw_iter().next()? {
            MaybeRef::Ref(reference) => self.pages.get(&ObjectIdentifier::from(reference)).copied(),
            MaybeRef::NotRef(_) => None,
        }
    }

    /// The destination array that `name` stands for: in the catalogue's `/Dests` dictionary (PDF 1.1), else in the `/Dests` name tree of the
    /// name dictionary (PDF 1.2).
    fn named(&self, name: &[u8]) -> Option<Array<'a>> {
        let value = self
            .root
            .get::<Dict<'a>>(b"Dests".as_slice())
            .and_then(|dests| dests.get::<Object<'a>>(name))
            .or_else(|| {
                let tree = self
                    .root
                    .get::<Dict<'a>>(b"Names".as_slice())?
                    .get::<Dict<'a>>(b"Dests".as_slice())?;
                let mut budget = MAX_TREE_NODES;
                tree_lookup(&tree, name, &mut budget, 0)
            })?;
        match value {
            Object::Array(array) => Some(array),
            Object::Dict(dict) => dict.get::<Array<'a>>(b"D".as_slice()),
            _ => None,
        }
    }
}

/// The value that the name tree under `node` gives for `key`: a leaf's `/Names` is `[key value key value ...]`, an inner node's `/Kids` are
/// nodes with `/Limits` (the least and the greatest key under them) that tell which to go into. `budget` counts the nodes looked at.
fn tree_lookup<'a>(node: &Dict<'a>, key: &[u8], budget: &mut usize, depth: usize) -> Option<Object<'a>> {
    if *budget == 0 || depth > MAX_TREE_DEPTH {
        return None;
    }
    *budget -= 1;
    if let Some(names) = node.get::<Array<'a>>(b"Names".as_slice()) {
        let flat: Vec<Object<'a>> = names.iter::<Object<'a>>().take(2 * MAX_TREE_NODES).collect();
        for pair in flat.as_chunks::<2>().0 {
            if let Object::String(candidate) = &pair[0]
                && candidate.as_bytes() == key
            {
                return Some(pair[1].clone());
            }
        }
    }
    if let Some(kids) = node.get::<Array<'a>>(b"Kids".as_slice()) {
        for kid in kids.iter::<Dict<'a>>() {
            if *budget == 0 {
                return None;
            }
            // `/Limits` says whether the key can be under this kid at all (keys are compared byte by byte, as the specification says)
            if let Some(limits) = kid.get::<Array<'a>>(b"Limits".as_slice()) {
                let bounds: Vec<PdfString<'a>> = limits.iter::<PdfString<'a>>().take(2).collect();
                if let [low, high] = bounds.as_slice()
                    && (key < low.as_bytes() || key > high.as_bytes())
                {
                    continue;
                }
            }
            if let Some(found) = tree_lookup(&kid, key, budget, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    /// A PDF of the given objects (number, body) with a catalogue as object 1 and an xref table.
    fn pdf_of(objects: &[(usize, String)]) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets: HashMap<usize, usize> = HashMap::new();
        for (number, body) in objects {
            offsets.insert(*number, out.len());
            out.extend(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
        let xref = out.len();
        out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for n in 1..size {
            match offsets.get(&n) {
                Some(at) => out.extend(format!("{at:010} 00000 n \n").as_bytes()),
                None => out.extend(b"0000000000 65535 f \n"),
            }
        }
        out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        out
    }

    fn sample() -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pdf/outline.pdf")).expect("reads the sample")
    }

    #[test]
    fn the_sample_outline_is_read_with_its_pages_depths_and_titles() {
        let items = read_contents(sample()).expect("reads").expect("has an outline");
        let seen: Vec<(usize, &str, Option<usize>)> =
            items.iter().map(|i| (i.depth, i.title.as_str(), i.page)).collect();
        assert_eq!(
            seen,
            vec![
                (0, "Chapter 1", Some(0)),
                (1, "Section 1.1", Some(1)),
                (0, "Глава 2", Some(2)),
                (0, "Elsewhere \u{2022}", None),
            ]
        );
    }

    #[test]
    fn a_pdf_with_no_outline_has_none() {
        let data = pdf_of(&[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_string()),
        ]);
        assert_eq!(read_contents(data).expect("reads"), None);
    }

    #[test]
    fn an_outline_that_loops_ends_and_does_not_repeat_its_items() {
        // item 5 says that its next is itself, and that its child is item 4, whose next is item 5 again
        let data = pdf_of(&[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [6 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Outlines /First 5 0 R >>".to_string()),
            (4, "<< /Title (b) /Next 5 0 R /Dest [6 0 R /Fit] >>".to_string()),
            (
                5,
                "<< /Title (a) /Next 5 0 R /First 4 0 R /Dest [6 0 R /Fit] >>".to_string(),
            ),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_string()),
        ]);
        let items = read_contents(data).expect("reads").expect("has items");
        assert_eq!(items.len(), 2, "each item once: {items:?}");
    }

    #[test]
    fn a_name_tree_that_loops_or_is_very_deep_is_given_up() {
        // a name tree whose only kid is itself, and an outline item that names something in it
        let data = pdf_of(&[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R /Names << /Dests 7 0 R >> >>".to_string(),
            ),
            (2, "<< /Type /Pages /Kids [6 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Outlines /First 5 0 R >>".to_string()),
            (5, "<< /Title (a) /Dest (nowhere) >>".to_string()),
            (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_string()),
            (7, "<< /Kids [7 0 R] >>".to_string()),
        ]);
        let started = std::time::Instant::now();
        let items = read_contents(data).expect("reads").expect("has items");
        assert_eq!(items[0].page, None);
        assert!(started.elapsed().as_secs() < 5);
    }

    #[test]
    fn many_items_are_cut_at_the_limit_in_good_time() {
        // 22 000 items in a chain: the walk stops at the limit
        let mut objects = vec![
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_string()),
            (3, "<< /Type /Outlines /First 5 0 R >>".to_string()),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_string()),
        ];
        for n in 5..22_005 {
            objects.push((n, format!("<< /Title (x) /Next {} 0 R /Dest [4 0 R /Fit] >>", n + 1)));
        }
        let started = std::time::Instant::now();
        let items = read_contents(pdf_of(&objects)).expect("reads").expect("has items");
        assert_eq!(items.len(), MAX_ITEMS);
        assert!(started.elapsed().as_secs() < 30);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_panic() {
        assert!(read_contents(b"%PDF-1.4 not a pdf".to_vec()).is_err());
        let sample = sample();
        for part in 0..10 {
            // a file cut short: an error, or what could be read, but no panic
            let _ = read_contents(sample[..sample.len() * part / 10].to_vec());
        }
    }
}
