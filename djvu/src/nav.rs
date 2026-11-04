//! The bookmarks of a document (spec 8.3.? `NAVM`): a tree of titles, each with the place it leads to. The chunk is BZZ coded.
//!
//! The decoded data is a count of 16 bits (the bookmarks of all levels together), then the bookmarks one after another, each parent before
//! its children: the number of its children (16 bits, the low byte first), the length of its title (16 bits) and the title (UTF-8), the
//! length of its address (24 bits) and the address. (DjVuLibre's own widths; an older form of the specification has 8, 24 and 24 bits, which
//! is the same bytes whenever there are fewer than 256 children, as in every book met.) An address is `#` and a page number counted from 1
//! (`#12`), or `#` and the identifier of a page of the directory (`#p0012.djvu`).

use crate::document::{ComponentKind, DirectoryEntry};
use crate::{Error, bzz};

/// The most the decoded chunk may be (a book has a few hundred bookmarks of a few dozen bytes).
const MAX_NAV: usize = 8 * 1024 * 1024;

/// The deepest tree read.
const MAX_DEPTH: usize = 32;

/// A bookmark: a title, an address, and the bookmarks under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub title: String,
    /// As the file has it (`#12`, `#p0012.djvu`, an address of the Internet); see [`page_of`].
    pub address: Vec<u8>,
    pub children: Vec<Bookmark>,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        let end = self
            .at
            .checked_add(n)
            .ok_or(Error::BadNav("a length that does not fit"))?;
        let bytes = self.data.get(self.at..end).ok_or(Error::BadNav("cut short"))?;
        self.at = end;
        Ok(bytes)
    }

    /// 16 bits, the low byte first (the number of children).
    fn u16_low_first(&mut self) -> Result<usize, Error> {
        let b = self.take(2)?;
        Ok(usize::from(b[1]) << 8 | usize::from(b[0]))
    }

    /// 16 bits, the high byte first (the length of a title).
    fn u16(&mut self) -> Result<usize, Error> {
        let b = self.take(2)?;
        Ok(usize::from(b[0]) << 8 | usize::from(b[1]))
    }

    fn u24(&mut self) -> Result<usize, Error> {
        let b = self.take(3)?;
        Ok(usize::from(b[0]) << 16 | usize::from(b[1]) << 8 | usize::from(b[2]))
    }

    fn node(&mut self, left: &mut usize, depth: usize) -> Result<Bookmark, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::BadNav("nested too deep"));
        }
        if *left == 0 {
            return Err(Error::BadNav("more bookmarks than the count says"));
        }
        *left -= 1;
        let count = self.u16_low_first()?;
        let title_len = self.u16()?;
        let title = String::from_utf8_lossy(self.take(title_len)?).into_owned();
        let address_len = self.u24()?;
        let address = self.take(address_len)?.to_vec();
        let mut children = Vec::with_capacity(count);
        for _ in 0..count {
            children.push(self.node(left, depth + 1)?);
        }
        Ok(Bookmark {
            title,
            address,
            children,
        })
    }
}

/// The bookmarks of a `NAVM` chunk (its bytes as they are in the file), the top level first.
///
/// # Errors
/// [`Error::BadBzz`] for data that cannot be decoded; [`Error::BadNav`] for a tree that is cut short, nested too deep, or has more
/// bookmarks than its count says.
pub fn parse(chunk: &[u8]) -> Result<Vec<Bookmark>, Error> {
    parse_with_leftover(chunk).map(|(tree, _)| tree)
}

/// [`parse`], and how many bytes of the decoded data are left after the last bookmark (none in a well made file; a check for the
/// reader of a format that has no sample to compare with).
///
/// # Errors
/// The errors of [`parse`].
pub fn parse_with_leftover(chunk: &[u8]) -> Result<(Vec<Bookmark>, usize), Error> {
    parse_decoded(&bzz::decode(chunk, MAX_NAV)?)
}

/// The bookmarks in data that is decoded already (what [`parse`] does after the BZZ layer), and how many bytes are left over.
///
/// # Errors
/// [`Error::BadNav`], as [`parse`].
pub fn parse_decoded(data: &[u8]) -> Result<(Vec<Bookmark>, usize), Error> {
    let mut reader = Reader { data, at: 0 };
    let head = reader.take(2)?;
    let total = usize::from(head[0]) << 8 | usize::from(head[1]);
    let mut left = total;
    let mut top = Vec::new();
    while left > 0 {
        top.push(reader.node(&mut left, 0)?);
    }
    Ok((top, data.len() - reader.at))
}

/// The page (counted from 0) that an address leads to: `#12` is the twelfth page, `#p0012.djvu` the page whose identifier, name or title
/// in the directory is that. None for an address of another kind, a page that is not there, or a name that is no page.
pub fn page_of(address: &[u8], entries: &[DirectoryEntry], pages: usize) -> Option<usize> {
    let rest = address.strip_prefix(b"#")?;
    if !rest.is_empty() && rest.iter().all(u8::is_ascii_digit) {
        let number: usize = std::str::from_utf8(rest).ok()?.parse().ok()?;
        return number.checked_sub(1).filter(|&p| p < pages);
    }
    entries
        .iter()
        .filter(|e| e.kind == ComponentKind::Page)
        .position(|e| e.id == rest || e.name.as_deref() == Some(rest) || e.title.as_deref() == Some(rest))
}

/// The bookmarks as a flat list in the order they are read, each with how deep it is (0 for the top level).
pub fn flatten(tree: &[Bookmark]) -> Vec<(usize, &Bookmark)> {
    fn walk<'a>(list: &'a [Bookmark], depth: usize, out: &mut Vec<(usize, &'a Bookmark)>) {
        for b in list {
            out.push((depth, b));
            walk(&b.children, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, 0, &mut out);
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot read what it made should stop at once
mod tests {
    use super::*;

    fn entry(kind: ComponentKind, id: &str) -> DirectoryEntry {
        DirectoryEntry {
            kind,
            size: 0,
            id: id.as_bytes().to_vec(),
            name: None,
            title: None,
        }
    }

    #[test]
    fn an_address_is_a_page_number_from_1_or_the_name_of_a_page() {
        let entries = [
            entry(ComponentKind::Included, "dict.djvi"),
            entry(ComponentKind::Page, "p0001.djvu"),
            entry(ComponentKind::Page, "p0002.djvu"),
        ];
        assert_eq!(page_of(b"#1", &entries, 2), Some(0));
        assert_eq!(page_of(b"#2", &entries, 2), Some(1));
        assert_eq!(page_of(b"#3", &entries, 2), None, "past the end");
        assert_eq!(page_of(b"#0", &entries, 2), None);
        assert_eq!(
            page_of(b"#p0002.djvu", &entries, 2),
            Some(1),
            "the page, not counting the shared component"
        );
        assert_eq!(page_of(b"#dict.djvi", &entries, 2), None, "not a page");
        assert_eq!(page_of(b"http://example.org/", &entries, 2), None);
        assert_eq!(page_of(b"#", &entries, 2), None);
    }

    #[test]
    fn a_bookmark_may_have_more_than_255_children() {
        // the decoded data of a node with 300 children (a count of 16 bits, the low byte first), as DjVuLibre writes it
        let mut data = 301u16.to_be_bytes().to_vec();
        data.extend([0x2C, 0x01, 0, 1, b'R', 0, 0, 0]);
        for _ in 0..300 {
            data.extend([0, 0, 0, 1, b'c', 0, 0, 2, b'#', b'1']);
        }
        let (tree, left) = parse_decoded(&data).expect("reads");
        assert_eq!((tree.len(), tree[0].children.len(), left), (1, 300, 0));
        assert_eq!(tree[0].children[299].address, b"#1");
        // a count that promises more bookmarks than there are, and data cut short, are errors
        let mut short = 3u16.to_be_bytes().to_vec();
        short.extend([0, 0, 0, 1, b'x', 0, 0, 0]);
        assert_eq!(parse_decoded(&short), Err(Error::BadNav("cut short")));
        assert!(parse_decoded(&data[..data.len() - 3]).is_err());
    }

    #[test]
    fn the_tree_is_flattened_parent_first() {
        let leaf = |t: &str| Bookmark {
            title: t.to_string(),
            address: b"#1".to_vec(),
            children: Vec::new(),
        };
        let tree = vec![
            Bookmark {
                children: vec![leaf("1.1"), leaf("1.2")],
                ..leaf("1")
            },
            leaf("2"),
        ];
        let titles: Vec<(usize, &str)> = flatten(&tree).iter().map(|(d, b)| (*d, b.title.as_str())).collect();
        assert_eq!(titles, [(0, "1"), (1, "1.1"), (1, "1.2"), (0, "2")]);
    }

    #[test]
    fn data_of_any_kind_is_an_error_or_bookmarks_and_never_a_panic() {
        let mut seed = 0x9e37_79b9_u32;
        for round in 0..500 {
            let len = (round * 5) % 300;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = parse(&data);
        }
    }
}
