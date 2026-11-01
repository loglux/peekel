//! The structure of a DjVu document (spec 7 and 8.3.1): a single page (`FORM:DJVU`), or a bundled document (`FORM:DJVM`) that holds
//! a directory (`DIRM`), maybe bookmarks (`NAVM`), and its components: pages (`FORM:DJVU`) and shared components (`FORM:DJVI`,
//! the shape dictionaries and annotations that pages name with an `INCL` chunk).
//!
//! The names in the directory are BZZ-compressed and come with the BZZ decoder in a later step; what is read here is its
//! unencoded head (how many components, where they are). A document whose components are separate files ("indirect") cannot be
//! read from one file and is refused.

use crate::Error;
use crate::iff::{self, Chunk, Form};
use crate::info::PageInfo;

/// How the document is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// One page in a `FORM:DJVU`.
    SinglePage,
    /// Many components in a `FORM:DJVM`.
    Bundled,
}

/// The unencoded head of a `DIRM` chunk (spec 8.3.2.1); the rest of it is BZZ data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryHead<'a> {
    pub bundled: bool,
    /// The version in the low seven bits of the first byte (1 at the time of the spec).
    pub version: u8,
    /// How many components the directory lists.
    pub files: u16,
    /// Where each component is in the `FORM:DJVM` (bundled documents only; as the file says).
    pub offsets: Vec<u32>,
    /// The BZZ-compressed part: sizes, flags, names.
    pub encoded: &'a [u8],
}

impl<'a> DirectoryHead<'a> {
    /// # Errors
    /// [`Error::BadDirectory`] for a chunk too short for what it declares.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let Some(&[flags, f0, f1]) = data.first_chunk::<3>() else {
            return Err(Error::BadDirectory);
        };
        let bundled = flags & 0x80 != 0;
        let files = u16::from_be_bytes([f0, f1]);
        let mut at = 3;
        let mut offsets = Vec::new();
        if bundled {
            let table = usize::from(files) * 4;
            let bytes = data.get(at..at + table).ok_or(Error::BadDirectory)?;
            offsets = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_be_bytes(*c))
                .collect();
            at += table;
        }
        Ok(Self {
            bundled,
            version: flags & 0x7f,
            files,
            offsets,
            encoded: data.get(at..).unwrap_or(&[]),
        })
    }
}

/// What a component of a bundled document is (the low six bits of its flag byte in the directory, spec 8.3.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// Included by other files: a shared dictionary or shared annotations (`FORM:DJVI`).
    Included,
    /// A page (`FORM:DJVU`).
    Page,
    /// Thumbnails (`FORM:THUM`).
    Thumbnails,
    /// A value the spec does not list.
    Other(u8),
}

/// One component as the directory lists it, in the order the components stand in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub kind: ComponentKind,
    /// The size of the component in the file, as the directory says (may be 0 in an indirect document).
    pub size: u32,
    /// The identifier that the `INCL` chunks of pages use, as bytes (it need not be text: the books of Zorich have two bytes that
    /// are not UTF-8 at the start of every id).
    pub id: Vec<u8>,
    /// The name of the file, when it is not the id (the flag "hasname").
    pub name: Option<Vec<u8>>,
    /// The title, when it is not the id (the flag "hastitle").
    pub title: Option<Vec<u8>>,
}

/// The most that the BZZ part of a directory may decode to (65,535 components of a few hundred bytes each at the very most).
const MAX_DIRECTORY: usize = 16 * 1024 * 1024;

/// The zero-terminated string at `at` in `data`; `at` moves past its terminator.
fn read_zstr(data: &[u8], at: &mut usize) -> Result<Vec<u8>, Error> {
    let rest = data.get(*at..).ok_or(Error::BadDirectory)?;
    let len = rest.iter().position(|&b| b == 0).ok_or(Error::BadDirectory)?;
    *at += len + 1;
    Ok(rest[..len].to_vec())
}

impl DirectoryHead<'_> {
    /// The components the directory lists (spec 8.3.2.2): the BZZ part holds the sizes (24 bits each), a flag byte for each, and then
    /// one to three zero-terminated strings for each.
    ///
    /// # Errors
    /// [`Error::BadBzz`] when the BZZ data cannot be decoded; [`Error::BadDirectory`] when what it decodes to is shorter than the
    /// number of components says.
    pub fn entries(&self) -> Result<Vec<DirectoryEntry>, Error> {
        let files = usize::from(self.files);
        let meta = crate::bzz::decode(self.encoded, MAX_DIRECTORY)?;
        let sizes = meta.get(..files * 3).ok_or(Error::BadDirectory)?;
        let flags = meta.get(files * 3..files * 4).ok_or(Error::BadDirectory)?;
        let mut at = files * 4;
        let mut entries = Vec::with_capacity(files);
        for (size, &flag) in sizes.as_chunks::<3>().0.iter().zip(flags) {
            let id = read_zstr(&meta, &mut at)?;
            let name = (flag & 0x80 != 0).then(|| read_zstr(&meta, &mut at)).transpose()?;
            let title = (flag & 0x40 != 0).then(|| read_zstr(&meta, &mut at)).transpose()?;
            entries.push(DirectoryEntry {
                kind: match flag & 0x3F {
                    0 => ComponentKind::Included,
                    1 => ComponentKind::Page,
                    2 => ComponentKind::Thumbnails,
                    other => ComponentKind::Other(other),
                },
                size: u32::from_be_bytes([0, size[0], size[1], size[2]]),
                id,
                name,
                title,
            });
        }
        Ok(entries)
    }
}

/// A document read from the bytes of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document<'a> {
    root: Form<'a>,
    layout: Layout,
}

impl<'a> Document<'a> {
    /// # Errors
    /// The errors of [`iff::parse`]; [`Error::NotDjvu`] for a file that is a `FORM` of another kind than `DJVU` and `DJVM`, or a
    /// `DJVM` without its directory; [`Error::BadDirectory`]; [`Error::Indirect`] for a directory that is not bundled.
    pub fn parse(file: &'a [u8]) -> Result<Self, Error> {
        let root = iff::parse(file)?;
        let layout = match &root.kind {
            b"DJVU" => Layout::SinglePage,
            b"DJVM" => {
                let dirm = root.first_chunk(*b"DIRM").ok_or(Error::NotDjvu)?;
                if !DirectoryHead::parse(dirm.data)?.bundled {
                    return Err(Error::Indirect);
                }
                Layout::Bundled
            }
            _ => return Err(Error::NotDjvu),
        };
        Ok(Self { root, layout })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// The directory of a bundled document.
    pub fn directory(&self) -> Option<DirectoryHead<'a>> {
        self.root
            .first_chunk(*b"DIRM")
            .and_then(|chunk| DirectoryHead::parse(chunk.data).ok())
    }

    /// The bookmarks chunk (`NAVM`, BZZ data) of a bundled document, as it is.
    pub fn bookmarks(&self) -> Option<&Chunk<'a>> {
        self.root.first_chunk(*b"NAVM")
    }

    /// The pages: the `FORM:DJVU`s of a bundled document in the order they stand, or the one page of a single-page file.
    fn page_forms(&self) -> Vec<&Form<'a>> {
        match self.layout {
            Layout::SinglePage => vec![&self.root],
            Layout::Bundled => self.root.forms(*b"DJVU").collect(),
        }
    }

    pub fn page_count(&self) -> usize {
        match self.layout {
            Layout::SinglePage => 1,
            Layout::Bundled => self.root.forms(*b"DJVU").count(),
        }
    }

    /// The shared components (`FORM:DJVI`) of a bundled document, in the order they stand.
    pub fn shared_components(&self) -> impl Iterator<Item = &Form<'a>> {
        let forms = match self.layout {
            Layout::SinglePage => None,
            Layout::Bundled => Some(self.root.forms(*b"DJVI")),
        };
        forms.into_iter().flatten()
    }

    /// How many components of every kind there are in a bundled document (pages, shared ones, thumbnails); what the directory
    /// should say its `files` are.
    pub fn component_count(&self) -> usize {
        match self.layout {
            Layout::SinglePage => 1,
            Layout::Bundled => self
                .root
                .children
                .iter()
                .filter(|n| matches!(n, iff::Node::Form(_)))
                .count(),
        }
    }

    /// The component that the `INCL` name `name` stands for: the form at the place in the file where the directory lists `name`
    /// (the components stand in the file in the order the directory lists them). None when the directory has no such id, or lists
    /// more components than there are forms.
    pub fn included<'s>(&'s self, entries: &[DirectoryEntry], name: &[u8]) -> Option<&'s Form<'a>> {
        let place = entries.iter().position(|entry| entry.id == name)?;
        self.root
            .children
            .iter()
            .filter_map(|node| match node {
                iff::Node::Form(form) => Some(form),
                iff::Node::Chunk(_) => None,
            })
            .nth(place)
    }

    /// The page at `index` (from 0).
    ///
    /// # Errors
    /// [`Error::NoSuchPage`]; [`Error::NoInfo`] or [`Error::BadInfo`] for a page without a usable `INFO` chunk.
    pub fn page(&self, index: usize) -> Result<Page<'_, 'a>, Error> {
        let forms = self.page_forms();
        let pages = forms.len();
        let form = *forms.get(index).ok_or(Error::NoSuchPage { index, pages })?;
        // spec 8.3.1.2: the INFO chunk must be the first; one that is elsewhere is still found
        let info = form.first_chunk(*b"INFO").ok_or(Error::NoInfo)?;
        Ok(Page {
            form,
            info: PageInfo::parse(info.data)?,
        })
    }
}

/// A page of a document: its chunks and what its `INFO` says.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<'d, 'a: 'd> {
    form: &'d Form<'a>,
    pub info: PageInfo,
}

impl<'d, 'a: 'd> Page<'d, 'a> {
    /// The chunks of this page with this id, in order (`Sjbz`, `BG44`, `FG44`, `TXTz`, ...).
    pub fn chunks(&self, id: [u8; 4]) -> impl Iterator<Item = &'d Chunk<'a>> + use<'d, 'a> {
        self.form.chunks(id)
    }

    /// The names the page gives in its `INCL` chunks, as bytes: padding (NUL, white space) at the end taken off, nothing else
    /// done to them (a name need not be text; the names in the directory are compared as the same bytes).
    pub fn included_names(&self) -> Vec<&'a [u8]> {
        self.form.chunks(*b"INCL").map(|chunk| trim_name(chunk.data)).collect()
    }
}

/// A name from an `INCL` chunk without the NUL or white space that pads it.
pub fn trim_name(data: &[u8]) -> &[u8] {
    let end = data
        .iter()
        .rposition(|&byte| byte != 0 && !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    &data[..end]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // a test that cannot make its file should stop at once
mod tests {
    use super::*;
    use crate::iff::tests::{chunk, file, form};

    fn info(width: u16, height: u16) -> Vec<u8> {
        let mut data = width.to_be_bytes().to_vec();
        data.extend(height.to_be_bytes());
        data.extend([26, 0, 0x2C, 0x01, 22, 1]);
        chunk(b"INFO", &data)
    }

    /// A bundled DIRM head for `files` components (the BZZ part is not made, only a few bytes stand for it).
    fn dirm(bundled: bool, files: u16) -> Vec<u8> {
        let mut data = vec![if bundled { 0x81 } else { 0x01 }];
        data.extend(files.to_be_bytes());
        if bundled {
            for i in 0..files {
                data.extend((u32::from(i) * 100).to_be_bytes());
            }
        }
        data.extend([9, 9, 9]);
        chunk(b"DIRM", &data)
    }

    #[test]
    fn a_single_page_file_has_one_page_with_its_info_and_chunks() {
        let bytes = file(&form(
            b"DJVU",
            &[info(2202, 967), chunk(b"INCL", b"dict.iff\0"), chunk(b"Sjbz", b"abc")],
        ));
        let doc = Document::parse(&bytes).unwrap();
        assert_eq!((doc.layout(), doc.page_count()), (Layout::SinglePage, 1));
        let page = doc.page(0).unwrap();
        assert_eq!((page.info.width, page.info.height, page.info.dpi), (2202, 967, 300));
        assert_eq!(page.chunks(*b"Sjbz").next().unwrap().data, b"abc");
        assert_eq!(
            page.included_names(),
            vec![b"dict.iff".as_slice()],
            "the padding of the name is taken off"
        );
        assert_eq!(doc.page(1).unwrap_err(), Error::NoSuchPage { index: 1, pages: 1 });
    }

    #[test]
    fn a_bundled_document_lists_its_pages_in_order_and_keeps_the_shared_components_apart() {
        let bytes = file(&form(
            b"DJVM",
            &[
                dirm(true, 4),
                chunk(b"NAVM", b"nav"),
                form(b"DJVI", &[chunk(b"Djbz", b"shapes")]),
                form(b"DJVU", &[info(100, 200), chunk(b"INCL", b"d.djbz")]),
                form(b"DJVU", &[info(300, 400)]),
                form(b"THUM", &[chunk(b"TH44", b"t")]),
            ],
        ));
        let doc = Document::parse(&bytes).unwrap();
        assert_eq!((doc.layout(), doc.page_count()), (Layout::Bundled, 2));
        assert_eq!(doc.page(0).unwrap().info.width, 100);
        assert_eq!(doc.page(1).unwrap().info.width, 300);
        assert_eq!(doc.shared_components().count(), 1);
        assert_eq!(doc.component_count(), 4, "the thumbnails form is a component too");
        assert_eq!(doc.bookmarks().unwrap().data, b"nav");
        let head = doc.directory().unwrap();
        assert_eq!((head.bundled, head.version, head.files), (true, 1, 4));
        assert_eq!(head.offsets, vec![0, 100, 200, 300]);
        assert_eq!(head.encoded, &[9, 9, 9]);
        assert_eq!(doc.page(0).unwrap().included_names(), vec![b"d.djbz".as_slice()]);
    }

    #[test]
    fn an_indirect_document_and_a_djvm_without_its_directory_are_refused() {
        let indirect = file(&form(b"DJVM", &[dirm(false, 2)]));
        assert_eq!(Document::parse(&indirect).unwrap_err(), Error::Indirect);
        let headless = file(&form(b"DJVM", &[form(b"DJVU", &[info(1, 1)])]));
        assert_eq!(Document::parse(&headless).unwrap_err(), Error::NotDjvu);
        let other = file(&form(b"DJVI", &[chunk(b"Djbz", b"x")]));
        assert_eq!(Document::parse(&other).unwrap_err(), Error::NotDjvu);
    }

    #[test]
    fn a_directory_shorter_than_it_says_is_a_bad_directory() {
        assert_eq!(DirectoryHead::parse(&[0x81]).unwrap_err(), Error::BadDirectory);
        assert_eq!(
            DirectoryHead::parse(&[0x81, 0xFF, 0xFF, 0, 0, 0, 0]).unwrap_err(),
            Error::BadDirectory,
            "65535 offsets that are not there"
        );
        let bytes = file(&form(b"DJVM", &[chunk(b"DIRM", &[0x81, 0, 3, 0, 0])]));
        assert_eq!(Document::parse(&bytes).unwrap_err(), Error::BadDirectory);
    }

    #[test]
    fn a_page_without_info_or_with_a_short_one_is_an_error_of_that_page_only() {
        let bytes = file(&form(
            b"DJVM",
            &[
                dirm(true, 3),
                form(b"DJVU", &[chunk(b"Sjbz", b"a")]),
                form(b"DJVU", &[chunk(b"INFO", &[0, 1])]),
                form(b"DJVU", &[info(5, 6)]),
            ],
        ));
        let doc = Document::parse(&bytes).unwrap();
        assert_eq!(doc.page(0).unwrap_err(), Error::NoInfo);
        assert_eq!(doc.page(1).unwrap_err(), Error::BadInfo);
        assert_eq!(doc.page(2).unwrap().info.height, 6, "the other pages are read");
    }

    #[test]
    fn names_are_bytes_and_are_compared_as_bytes() {
        // two bytes that are not UTF-8 at the start, as in the books of Zorich (docs/internal/djvu.md)
        let name = [0xFF, 0xFE, b'0', b'0', b'1', b'0', b'.', b'd', b'j', b'b', b'z', 0, 0];
        assert_eq!(trim_name(&name), &name[..11]);
        assert_eq!(trim_name(b"  \0 "), b"");
        assert_eq!(trim_name(b""), b"");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod directory_tests {
    use super::*;
    use crate::iff::tests::{chunk, file, form};

    // The example of spec 8.3.2.2 (three components: sizes 3501, 115024, 7877; flags 0, 1, 1; the ids dict0002.iff, p0001.djvu,
    // p0002.djvu) as the BZZ encoder of djvu-rs (MIT) writes it.
    const DIRM_SPEC_EXAMPLE: [u8; 44] = [
        0xff, 0xff, 0xcf, 0xbf, 0x8a, 0x1f, 0x6c, 0x23, 0x28, 0x74, 0xe2, 0x81, 0x49, 0x7f, 0x66, 0x72, 0xd3, 0xfe,
        0x94, 0xa0, 0x67, 0xe0, 0x2b, 0x0d, 0x09, 0x13, 0xdf, 0x5f, 0xf9, 0xca, 0xd9, 0xc0, 0x5d, 0xe0, 0xec, 0x81,
        0x42, 0x46, 0xf6, 0xee, 0xd2, 0xb7, 0x87, 0x6f,
    ];

    /// The whole DIRM chunk of that example: the head of the spec (bundled, version 1, 3 files, offsets 84, 3586, 118610) and the BZZ data.
    fn spec_dirm() -> Vec<u8> {
        let mut data = vec![0x81, 0x00, 0x03, 0, 0, 0, 0x54, 0, 0, 0x0e, 0x02, 0, 0x01, 0xcf, 0x52];
        data.extend(DIRM_SPEC_EXAMPLE);
        data
    }

    #[test]
    fn the_directory_of_the_spec_example_gives_its_three_components() {
        let data = spec_dirm();
        let head = DirectoryHead::parse(&data).unwrap();
        assert_eq!((head.bundled, head.version, head.files), (true, 1, 3));
        assert_eq!(head.offsets, vec![84, 3586, 118_610]);
        let entries = head.entries().unwrap();
        let summary: Vec<(ComponentKind, u32, &[u8])> =
            entries.iter().map(|e| (e.kind, e.size, e.id.as_slice())).collect();
        assert_eq!(
            summary,
            vec![
                (ComponentKind::Included, 3501, b"dict0002.iff".as_slice()),
                (ComponentKind::Page, 115_024, b"p0001.djvu".as_slice()),
                (ComponentKind::Page, 7877, b"p0002.djvu".as_slice()),
            ]
        );
        assert!(entries.iter().all(|e| e.name.is_none() && e.title.is_none()));
    }

    #[test]
    fn a_page_finds_the_shared_component_that_its_incl_names_through_the_directory() {
        let bytes = file(&form(
            b"DJVM",
            &[
                chunk(b"DIRM", &spec_dirm()),
                form(b"DJVI", &[chunk(b"Djbz", b"shapes")]),
                form(
                    b"DJVU",
                    &[
                        chunk(b"INFO", &[0, 5, 0, 5, 26, 0, 0x2C, 1, 22, 1]),
                        chunk(b"INCL", b"dict0002.iff"),
                    ],
                ),
                form(b"DJVU", &[chunk(b"INFO", &[0, 5, 0, 5, 26, 0, 0x2C, 1, 22, 1])]),
            ],
        ));
        let doc = Document::parse(&bytes).unwrap();
        let entries = doc.directory().unwrap().entries().unwrap();
        let name = doc.page(0).unwrap().included_names()[0];
        let shared = doc.included(&entries, name).unwrap();
        assert_eq!(shared.first_chunk(*b"Djbz").unwrap().data, b"shapes");
        assert!(doc.included(&entries, b"nothing.iff").is_none());
    }

    #[test]
    fn a_directory_whose_data_is_cut_or_short_is_an_error_not_a_panic() {
        let data = spec_dirm();
        for cut in 15..data.len() {
            let head = DirectoryHead::parse(&data[..cut]).unwrap();
            let _ = head.entries(); // an error or some entries, never a panic
        }
        // a head that says 60,000 components over BZZ data that holds three
        let mut many = vec![0x01, 0xEA, 0x60]; // indirect, 60,000 components
        many.extend(DIRM_SPEC_EXAMPLE);
        let head = DirectoryHead::parse(&many).unwrap();
        assert!(head.entries().is_err());
    }
}
