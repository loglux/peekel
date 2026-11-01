//! The IFF85 container of DjVu (DjVu v3 spec 8.1 and 8.2).
//!
//! A file is the four bytes `AT&T` and one `FORM` chunk. A chunk is its id (four bytes), the length of its data (big-endian, 32
//! bits), the data, and one pad byte when the length is odd. A `FORM` starts its data with a four-byte kind (`DJVU`, `DJVM`,
//! `DJVI`, ...) and holds chunks, `FORM`s among them. A chunk that is not known is ignored by the one who reads it, so every
//! chunk is kept here, known or not.

use crate::Error;

/// The first four bytes of a DjVu file (spec 8.1).
pub const MAGIC: [u8; 4] = *b"AT&T";

/// How deep `FORM`s may be nested. DjVu has two levels (a `DJVM` holding `DJVU` and `DJVI`); this leaves room for what a real
/// file may do and refuses a file built to nest without end.
pub const MAX_DEPTH: usize = 4;

/// How many chunks (`FORM`s included) one file may hold. The biggest book of the author's has a thousand pages of a handful of
/// chunks each; this is a thousand times that, and a ceiling on the memory a file can make us use for its tree.
pub const MAX_CHUNKS: usize = 4 * 1024 * 1024;

/// A chunk with data of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub id: [u8; 4],
    pub data: &'a [u8],
}

/// A `FORM`: its kind and what it holds, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form<'a> {
    pub kind: [u8; 4],
    pub children: Vec<Node<'a>>,
}

/// A thing in a `FORM`: a chunk or another `FORM`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node<'a> {
    Chunk(Chunk<'a>),
    Form(Form<'a>),
}

impl<'a> Form<'a> {
    /// The chunks of this form (not of the forms in it) with this id, in order.
    pub fn chunks<'s>(&'s self, id: [u8; 4]) -> impl Iterator<Item = &'s Chunk<'a>> + 's {
        self.children.iter().filter_map(move |node| match node {
            Node::Chunk(chunk) if chunk.id == id => Some(chunk),
            _ => None,
        })
    }

    /// The first chunk of this form with this id.
    pub fn first_chunk(&self, id: [u8; 4]) -> Option<&Chunk<'a>> {
        self.chunks(id).next()
    }

    /// The forms inside this one with this kind, in order.
    pub fn forms<'s>(&'s self, kind: [u8; 4]) -> impl Iterator<Item = &'s Form<'a>> + 's {
        self.children.iter().filter_map(move |node| match node {
            Node::Form(form) if form.kind == kind => Some(form),
            _ => None,
        })
    }
}

/// Reads a DjVu file: the magic, then the one `FORM` that is the file. What follows that chunk (zero padding, junk) is ignored.
///
/// # Errors
/// [`Error::NotDjvu`] without the magic or a `FORM` after it; [`Error::Truncated`], [`Error::BadLength`], [`Error::TooDeep`],
/// [`Error::TooMany`] for a file that is cut, lies about its lengths, or is built to be a burden.
pub fn parse(file: &[u8]) -> Result<Form<'_>, Error> {
    let body = file.strip_prefix(&MAGIC).ok_or(Error::NotDjvu)?;
    let header = read_header(body, 0, MAGIC.len())?.ok_or(Error::NotDjvu)?;
    if header.id != *b"FORM" {
        return Err(Error::NotDjvu);
    }
    let mut budget = MAX_CHUNKS;
    match read_node(body, &header, MAGIC.len(), 0, &mut budget)? {
        Node::Form(form) => Ok(form),
        Node::Chunk(_) => Err(Error::NotDjvu),
    }
}

/// The header of a chunk found at `pos` in `body`: where its data is, and how long.
struct Header {
    id: [u8; 4],
    start: usize,
    len: usize,
}

/// Reads the eight bytes of a chunk header at `pos`. None when what is left is nothing but zero padding; an error when it is
/// neither. `base` is where `body` begins in the file, only to say where an error was seen.
fn read_header(body: &[u8], pos: usize, base: usize) -> Result<Option<Header>, Error> {
    let rest = body.get(pos..).unwrap_or(&[]);
    if rest.iter().all(|&byte| byte == 0) {
        return Ok(None);
    }
    let Some(header) = rest.get(..8) else {
        return Err(Error::Truncated { at: base + pos });
    };
    let id = [header[0], header[1], header[2], header[3]];
    let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let len = usize::try_from(len).map_err(|_| Error::BadLength { at: base + pos })?;
    Ok(Some(Header {
        id,
        start: pos + 8,
        len,
    }))
}

/// The chunk whose header is `header`, inside `body`.
fn read_node<'a>(
    body: &'a [u8],
    header: &Header,
    base: usize,
    depth: usize,
    budget: &mut usize,
) -> Result<Node<'a>, Error> {
    *budget = budget.checked_sub(1).ok_or(Error::TooMany)?;
    let end = header.start.checked_add(header.len).ok_or(Error::BadLength {
        at: base + header.start,
    })?;
    let data = body.get(header.start..end).ok_or(Error::Truncated {
        at: base + header.start - 8,
    })?;
    if header.id != *b"FORM" {
        return Ok(Node::Chunk(Chunk { id: header.id, data }));
    }
    if depth >= MAX_DEPTH {
        return Err(Error::TooDeep);
    }
    let Some(kind) = data.get(..4) else {
        return Err(Error::BadLength {
            at: base + header.start,
        });
    };
    let children = read_children(&data[4..], base + header.start + 4, depth + 1, budget)?;
    Ok(Node::Form(Form {
        kind: [kind[0], kind[1], kind[2], kind[3]],
        children,
    }))
}

/// The chunks that follow one another in `body` (the data of a `FORM` after its kind).
fn read_children<'a>(body: &'a [u8], base: usize, depth: usize, budget: &mut usize) -> Result<Vec<Node<'a>>, Error> {
    let mut nodes = Vec::new();
    let mut pos = 0;
    while let Some(header) = read_header(body, pos, base)? {
        let node = read_node(body, &header, base, depth, budget)?;
        // the data is padded to an even length; a pad byte missing at the very end of the data is let pass
        pos = (header.start + header.len + (header.len & 1)).min(body.len());
        nodes.push(node);
    }
    Ok(nodes)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // a test that cannot make its file should stop at once
pub(crate) mod tests {
    use super::*;

    /// A chunk as it is in a file: id, length, data, a pad byte when the length is odd.
    pub(crate) fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// A `FORM` chunk of this kind holding these (already made) chunks.
    pub(crate) fn form(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
        let mut data = kind.to_vec();
        for child in children {
            data.extend(child);
        }
        chunk(b"FORM", &data)
    }

    /// A whole file.
    pub(crate) fn file(form: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(form);
        out
    }

    #[test]
    fn a_single_page_file_is_a_form_with_its_chunks_in_order() {
        let bytes = file(&form(
            b"DJVU",
            &[chunk(b"INFO", &[0; 10]), chunk(b"Sjbz", b"abc"), chunk(b"TXTz", b"de")],
        ));
        let root = parse(&bytes).unwrap();
        assert_eq!(root.kind, *b"DJVU");
        let ids: Vec<[u8; 4]> = root
            .children
            .iter()
            .map(|n| match n {
                Node::Chunk(c) => c.id,
                Node::Form(f) => f.kind,
            })
            .collect();
        assert_eq!(ids, vec![*b"INFO", *b"Sjbz", *b"TXTz"]);
        assert_eq!(
            root.first_chunk(*b"Sjbz").unwrap().data,
            b"abc",
            "an odd length is read without its pad byte"
        );
    }

    #[test]
    fn forms_nest_and_a_chunk_after_an_odd_one_is_found_past_the_pad_byte() {
        let inner = form(b"DJVI", &[chunk(b"Djbz", b"x")]);
        let bytes = file(&form(b"DJVM", &[chunk(b"DIRM", b"abc"), inner, chunk(b"NAVM", b"zz")]));
        let root = parse(&bytes).unwrap();
        assert_eq!(root.forms(*b"DJVI").count(), 1);
        assert_eq!(
            root.forms(*b"DJVI").next().unwrap().first_chunk(*b"Djbz").unwrap().data,
            b"x"
        );
        assert_eq!(root.first_chunk(*b"NAVM").unwrap().data, b"zz");
    }

    #[test]
    fn no_magic_or_no_form_is_not_djvu() {
        assert_eq!(parse(b"").unwrap_err(), Error::NotDjvu);
        assert_eq!(parse(b"PNG\x89 not a djvu file at all").unwrap_err(), Error::NotDjvu);
        assert_eq!(parse(&file(&chunk(b"INFO", &[0; 10]))).unwrap_err(), Error::NotDjvu);
        assert_eq!(
            parse(&MAGIC).unwrap_err(),
            Error::NotDjvu,
            "the magic and nothing after it"
        );
    }

    #[test]
    fn a_chunk_longer_than_what_is_there_is_truncated_not_a_panic() {
        let mut bytes = file(&form(b"DJVU", &[chunk(b"INFO", &[0; 10]), chunk(b"Sjbz", &[1; 100])]));
        bytes.truncate(bytes.len() - 30);
        assert!(
            matches!(parse(&bytes), Err(Error::Truncated { .. })),
            "{:?}",
            parse(&bytes)
        );
    }

    #[test]
    fn a_length_that_overflows_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(b"FORM");
        bytes.extend(u32::MAX.to_be_bytes());
        bytes.extend(b"DJVU");
        assert!(parse(&bytes).is_err());
        // a chunk inside a form that claims 4 GB
        let mut inner = b"DJVU".to_vec();
        inner.extend(b"INFO");
        inner.extend(u32::MAX.to_be_bytes());
        let mut bytes = MAGIC.to_vec();
        bytes.extend(b"FORM");
        bytes.extend(u32::try_from(inner.len()).unwrap().to_be_bytes());
        bytes.extend(inner);
        assert!(matches!(parse(&bytes), Err(Error::Truncated { .. })));
    }

    #[test]
    fn a_form_too_short_for_its_kind_is_a_bad_length() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(b"FORM");
        bytes.extend(2u32.to_be_bytes());
        bytes.extend(b"DJ");
        assert!(matches!(parse(&bytes), Err(Error::BadLength { .. })));
    }

    #[test]
    fn forms_nested_without_end_are_refused_before_the_stack_is() {
        let mut inner = chunk(b"INFO", &[0; 10]);
        for _ in 0..10_000 {
            inner = form(b"DJVM", &[inner]);
        }
        assert_eq!(parse(&file(&inner)).unwrap_err(), Error::TooDeep);
    }

    #[test]
    fn zero_padding_after_the_chunks_and_after_the_form_is_let_pass() {
        let mut bytes = file(&form(b"DJVU", &[chunk(b"INFO", &[0; 10])]));
        bytes.extend([0; 7]);
        assert_eq!(parse(&bytes).unwrap().children.len(), 1);
        // inside the form: junk that is not zero and is shorter than a header is truncation
        let mut data = b"DJVU".to_vec();
        data.extend(chunk(b"INFO", &[0; 10]));
        data.extend([1, 2, 3]);
        let mut bytes = MAGIC.to_vec();
        bytes.extend(chunk(b"FORM", &data));
        assert!(matches!(parse(&bytes), Err(Error::Truncated { .. })));
    }

    #[test]
    fn a_missing_pad_byte_at_the_very_end_is_let_pass() {
        let mut data = b"DJVU".to_vec();
        data.extend(b"TXTz");
        data.extend(3u32.to_be_bytes());
        data.extend(b"abc"); // odd length and no pad byte: the form ends here
        let mut bytes = MAGIC.to_vec();
        bytes.extend(b"FORM");
        bytes.extend(u32::try_from(data.len()).unwrap().to_be_bytes());
        bytes.extend(data);
        let root = parse(&bytes).unwrap();
        assert_eq!(root.first_chunk(*b"TXTz").unwrap().data, b"abc");
    }

    #[test]
    fn a_file_of_many_empty_chunks_hits_the_ceiling_not_the_memory() {
        // sixteen empty chunks in a form; the real ceiling is four million, so the same code path is run on a budget of five
        let kind_and_chunks = {
            let mut v = b"DJVU".to_vec();
            for _ in 0..16 {
                v.extend(chunk(b"ANTz", b""));
            }
            v
        };
        let bytes = {
            let mut b = MAGIC.to_vec();
            b.extend(chunk(b"FORM", &kind_and_chunks));
            b
        };
        let mut budget = 5;
        let body = bytes.strip_prefix(&MAGIC).unwrap();
        let header = read_header(body, 0, 4).unwrap().unwrap();
        assert_eq!(read_node(body, &header, 4, 0, &mut budget).unwrap_err(), Error::TooMany);
    }
}
