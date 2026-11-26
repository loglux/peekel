//! A DjVu file (a scanned book, as a rule) drawn with `peekel-djvu`, the reader of this project (written from the spec, in safe
//! code): any page of it as a picture, and the few things the card shows.
//!
//! A page is put together at its own size (a scan is 2 000 to 7 000 pixels a side) and then handled like any other picture. Not
//! done: documents whose pages are separate files (the "indirect" kind: there is only one file here), the text layer, the
//! bookmarks.
//!
//! The reader works on bytes with limits and no panic on any input; the page is still drawn on a thread with a large stack, as
//! the other documents are, so that a surprise is an error and not the end of the caller.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use image::RgbaImage;
use peekel_djvu::document::Document;
use peekel_djvu::info::Rotation;
use peekel_djvu::jb2::{self, Dictionary};
use peekel_djvu::page;

use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};

/// A bigger file is refused before it is read (the books of a library are 5 to 300 MB).
pub(crate) const MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// A book of this size or less is kept in memory after a page of it was drawn, so that turning to the next page does not read it
/// again; a bigger bundled one is read by its parts, a page and what it includes (`Skeleton`), and any other bigger one whole for
/// each page. Only the last book is kept.
const KEEP_BYTES: u64 = 96 * 1024 * 1024;

/// The most dictionaries of shapes kept for the book (a document has one, a few at most).
const MAX_DICTIONARIES: usize = 8;

/// The last book that was opened, and what its pages share.
struct Book {
    path: PathBuf,
    length: u64,
    modified: Option<SystemTime>,
    data: Arc<Vec<u8>>,
    /// The decoded dictionaries of shapes, by the name that a page includes them under.
    dictionaries: HashMap<Vec<u8>, Arc<Dictionary>>,
}

static BOOK: Mutex<Option<Book>> = Mutex::new(None);

/// Whether a page drawn at half its size or less has its background made as DjVuLibre makes it (`page::Shrink::AsDjvulibre`:
/// faster, less memory, the halftone of a printed picture may show as grain) instead of the mean of the whole one (`Mean`, the
/// default). The author, 24 November 2026: both ways kept, to be switched. Measured on a book of scans at 600 dpi:
/// docs/internal/djvu.md, "A smaller page as DjVuLibre makes it".
static SHRINK_AS_DJVULIBRE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets how the background of a page drawn small is made (`SHRINK_AS_DJVULIBRE`); the next page drawn is made so.
pub fn set_shrink_as_djvulibre(on: bool) {
    SHRINK_AS_DJVULIBRE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the background of a page drawn small is made as DjVuLibre makes it.
pub fn shrink_as_djvulibre() -> bool {
    SHRINK_AS_DJVULIBRE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether a page drawn at half its size or less has its mask, with the colours of its shapes, put on as DjVuLibre puts it
/// (`page::Small::mask`: the page put together at a whole reduction and then mixed to the size; softer letters, smoother pictures
/// drawn in dots) instead of ours (`Mean`, the default: the share of black of the pixels each small pixel covers; sharper letters).
/// The author, 24 November 2026: both ways kept, to be switched; with `SHRINK_AS_DJVULIBRE` too, the page is ddjvu's.
static MASK_AS_DJVULIBRE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Sets how the mask of a page drawn small is put on (`MASK_AS_DJVULIBRE`); the next page drawn is made so.
pub fn set_mask_as_djvulibre(on: bool) {
    MASK_AS_DJVULIBRE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the mask of a page drawn small is put on as DjVuLibre puts it.
pub fn mask_as_djvulibre() -> bool {
    MASK_AS_DJVULIBRE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The ways a page drawn small is made, as the two settings say.
fn small() -> page::Small {
    let way = |djvulibre: bool| {
        if djvulibre {
            page::Shrink::AsDjvulibre
        } else {
            page::Shrink::Mean
        }
    };
    page::Small {
        background: way(shrink_as_djvulibre()),
        mask: way(mask_as_djvulibre()),
    }
}

/// Whether the name says DjVu (`.djvu` or `.djv`), or, for a file with no extension, its first bytes (`sniff`).
pub fn is_djvu(path: &Path) -> bool {
    has_extension(path, "djvu")
        || has_extension(path, "djv")
        || crate::sniff::sniffed(path) == Some(crate::sniff::Sniffed::Djvu)
}

/// The first page of the DjVu file at `path`.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    render_page(path, 0).map(|(image, _)| image)
}

/// The bytes of the file: those of the book that is kept, when the file is that book and has not changed, else read now (and kept,
/// if it is small enough).
fn bytes_of(path: &Path) -> Result<Arc<Vec<u8>>, DecodeError> {
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if meta.len() > MAX_BYTES {
        return Err(DecodeError::FileTooLarge(MAX_BYTES));
    }
    let modified = meta.modified().ok();
    if let Ok(kept) = BOOK.lock()
        && let Some(book) = kept.as_ref()
        && book.path == path
        && book.length == meta.len()
        && book.modified == modified
    {
        return Ok(Arc::clone(&book.data));
    }
    let data = Arc::new(read_limited(path, MAX_BYTES)?);
    if let Ok(mut kept) = BOOK.lock() {
        *kept = (meta.len() <= KEEP_BYTES).then(|| Book {
            path: path.to_path_buf(),
            length: meta.len(),
            modified,
            data: Arc::clone(&data),
            dictionaries: HashMap::new(),
        });
    }
    Ok(data)
}

/// The dictionary of shapes that pages include under `name`, decoded once for the book whose bytes these are: the book kept whole, or
/// the big one read by its parts (`Skeleton`), whose page these bytes were put together for.
fn included_dictionary(data: &Arc<Vec<u8>>, name: &[u8], chunk: &[u8]) -> Result<Arc<Dictionary>, DecodeError> {
    let is_kept = |book: &Book| Arc::ptr_eq(&book.data, data);
    if let Ok(kept) = BOOK.lock()
        && let Some(book) = kept.as_ref().filter(|b| is_kept(b))
        && let Some(found) = book.dictionaries.get(name)
    {
        return Ok(Arc::clone(found));
    }
    let skeleton = SKELETON
        .lock()
        .ok()
        .and_then(|s| s.as_ref().filter(|s| s.made(data)).map(Arc::clone));
    if let Some(found) = skeleton
        .as_ref()
        .and_then(|s| s.dictionaries.lock().ok().and_then(|d| d.get(name).map(Arc::clone)))
    {
        return Ok(found);
    }
    let decoded = Arc::new(jb2::decode_dictionary(chunk, None).map_err(unreadable)?);
    if let Ok(mut kept) = BOOK.lock()
        && let Some(book) = kept.as_mut().filter(|b| is_kept(b))
        && book.dictionaries.len() < MAX_DICTIONARIES
    {
        book.dictionaries.insert(name.to_vec(), Arc::clone(&decoded));
    }
    if let Some(skeleton) = skeleton
        && let Ok(mut dictionaries) = skeleton.dictionaries.lock()
        && dictionaries.len() < MAX_DICTIONARIES
    {
        dictionaries.insert(name.to_vec(), Arc::clone(&decoded));
    }
    Ok(decoded)
}

/// A bundled book bigger than `KEEP_BYTES`, read by its parts (Peekel TV, #235: a book of 257 MB was read whole for each page, which
/// took seconds and hundreds of MB on a tablet): its head (the directory, `DIRM`, and the bookmarks, `NAVM`, which stand before the
/// components), where each component stands and what it is, and the dictionaries of shapes decoded for its pages. A page is drawn
/// from a file put together in memory (`sparse`): the head, that page and the components it includes as they are in the file, and an
/// empty form of the same kind for every other component, so that the document is read as it would be whole.
struct Skeleton {
    path: PathBuf,
    length: u64,
    modified: Option<SystemTime>,
    head: Vec<u8>,
    components: Vec<Component>,
    dictionaries: Mutex<HashMap<Vec<u8>, Arc<Dictionary>>>,
    /// The files put together for pages lately (by where their bytes are), so that a dictionary decoded for one is the book's.
    made: Mutex<Vec<usize>>,
}

struct Component {
    kind: peekel_djvu::document::ComponentKind,
    offset: u64,
    id: Vec<u8>,
}

static SKELETON: Mutex<Option<Arc<Skeleton>>> = Mutex::new(None);

/// The most the head of a book (its directory and bookmarks) and one component of it may be, read by its parts.
const MAX_HEAD: u64 = 64 * 1024 * 1024;
const MAX_COMPONENT: u64 = 256 * 1024 * 1024;

impl Skeleton {
    fn is(&self, path: &Path, meta: &std::fs::Metadata) -> bool {
        self.path == path && self.length == meta.len() && self.modified == meta.modified().ok()
    }

    /// Whether `data` is a file put together for a page of this book lately.
    fn made(&self, data: &Arc<Vec<u8>>) -> bool {
        self.made
            .lock()
            .is_ok_and(|m| m.contains(&(Arc::as_ptr(data) as usize)))
    }

    /// What a bundled book has at its start, read from it: `None` for a file that is not a bundled DjVu, or whose directory does not
    /// say where its components are (it is then read whole, as a smaller one is).
    fn read(path: &Path, meta: &std::fs::Metadata) -> Result<Option<Self>, DecodeError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path).map_err(unreadable)?;
        let mut start = [0u8; 24];
        if file.read_exact(&mut start).is_err()
            || &start[..8] != b"AT&TFORM"
            || &start[12..16] != b"DJVM"
            || &start[16..20] != b"DIRM"
        {
            return Ok(None);
        }
        let dirm = u64::from(u32::from_be_bytes([start[20], start[21], start[22], start[23]]));
        if dirm > MAX_HEAD {
            return Ok(None);
        }
        let mut data = vec![0u8; dirm as usize];
        file.read_exact(&mut data).map_err(unreadable)?;
        let Ok(head) = peekel_djvu::document::DirectoryHead::parse(&data) else {
            return Ok(None);
        };
        let Ok(entries) = head.entries() else {
            return Ok(None);
        };
        if !head.bundled || head.offsets.len() != entries.len() || entries.is_empty() {
            return Ok(None);
        }
        // the head is all that stands before the first component
        let first = head.offsets.iter().copied().min().map_or(0, u64::from);
        if first < 24 + dirm || first > MAX_HEAD || first > meta.len() {
            return Ok(None);
        }
        let mut head_bytes = vec![0u8; (first - 16) as usize];
        file.seek(SeekFrom::Start(16)).map_err(unreadable)?;
        file.read_exact(&mut head_bytes).map_err(unreadable)?;
        let components = entries
            .into_iter()
            .zip(&head.offsets)
            .map(|(entry, &offset)| Component {
                kind: entry.kind,
                offset: u64::from(offset),
                id: entry.id,
            })
            .collect();
        Ok(Some(Self {
            path: path.to_path_buf(),
            length: meta.len(),
            modified: meta.modified().ok(),
            head: head_bytes,
            components,
            dictionaries: Mutex::new(HashMap::new()),
            made: Mutex::new(Vec::new()),
        }))
    }

    /// The bytes of component `index`, its form's header and all, as they are in the file.
    fn component(&self, file: &mut std::fs::File, index: usize) -> Result<Vec<u8>, DecodeError> {
        use std::io::{Read, Seek, SeekFrom};
        let at = self
            .components
            .get(index)
            .ok_or_else(|| unreadable("no such component"))?
            .offset;
        file.seek(SeekFrom::Start(at)).map_err(unreadable)?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header).map_err(unreadable)?;
        if &header[..4] != b"FORM" {
            return Err(unreadable("the directory does not say where a component is"));
        }
        let size = u64::from(u32::from_be_bytes([header[4], header[5], header[6], header[7]]));
        if size > MAX_COMPONENT || at + 8 + size > self.length {
            return Err(unreadable("a component of the book is larger than the file"));
        }
        let mut bytes = Vec::with_capacity(8 + size as usize);
        bytes.extend_from_slice(&header);
        bytes.resize(8 + size as usize, 0);
        file.read_exact(&mut bytes[8..]).map_err(unreadable)?;
        Ok(bytes)
    }

    /// A file of the book with page `page` (a number past the end is the last; `None`, no page: the directory and bookmarks alone) and
    /// the components it includes; every other component an empty form of its kind.
    fn sparse(&self, page: Option<usize>) -> Result<Arc<Vec<u8>>, DecodeError> {
        use peekel_djvu::document::ComponentKind;
        let mut file = std::fs::File::open(&self.path).map_err(unreadable)?;
        let mut real: HashMap<usize, Vec<u8>> = HashMap::new();
        let pages: Vec<usize> = (0..self.components.len())
            .filter(|&i| self.components[i].kind == ComponentKind::Page)
            .collect();
        if let Some(page) = page
            && let Some(&index) = pages.get(page.min(pages.len().saturating_sub(1)))
        {
            let bytes = self.component(&mut file, index)?;
            for name in included_names(&bytes) {
                if let Some(shared) = self.components.iter().position(|c| c.id == name)
                    && !real.contains_key(&shared)
                    && shared != index
                {
                    let shared_bytes = self.component(&mut file, shared)?;
                    real.insert(shared, shared_bytes);
                }
            }
            real.insert(index, bytes);
        }
        let mut out: Vec<u8> = Vec::with_capacity(
            16 + self.head.len() + real.values().map(Vec::len).sum::<usize>() + self.components.len() * 12,
        );
        out.extend_from_slice(b"AT&TFORM\0\0\0\0DJVM");
        out.extend_from_slice(&self.head);
        if out.len() % 2 == 1 {
            out.push(0);
        }
        for (i, component) in self.components.iter().enumerate() {
            match real.get(&i) {
                Some(bytes) => out.extend_from_slice(bytes),
                None => {
                    let kind: &[u8; 4] = match component.kind {
                        ComponentKind::Page => b"DJVU",
                        ComponentKind::Thumbnails => b"THUM",
                        ComponentKind::Included | ComponentKind::Other(_) => b"DJVI",
                    };
                    out.extend_from_slice(b"FORM\0\0\0\x04");
                    out.extend_from_slice(kind);
                }
            }
            if out.len() % 2 == 1 {
                out.push(0);
            }
        }
        let size = u32::try_from(out.len() - 12).map_err(unreadable)?;
        out[8..12].copy_from_slice(&size.to_be_bytes());
        let out = Arc::new(out);
        if let Ok(mut made) = self.made.lock() {
            made.push(Arc::as_ptr(&out) as usize);
            let extra = made.len().saturating_sub(8);
            made.drain(..extra);
        }
        Ok(out)
    }
}

/// The names in the `INCL` chunks of a component's form (its bytes, header and all), padding off.
fn included_names(form: &[u8]) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    let mut at = 12;
    while let Some(header) = form.get(at..at + 8) {
        let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let Some(data) = form.get(at + 8..at + 8 + len) else {
            break;
        };
        if &header[..4] == b"INCL" {
            let end = data
                .iter()
                .rposition(|&b| b != 0 && !b.is_ascii_whitespace())
                .map_or(0, |e| e + 1);
            names.push(data[..end].to_vec());
        }
        at += 8 + len + len % 2;
    }
    names
}

/// The bytes to read page `page` of the file with (`None`: what the whole book is about, its directory and bookmarks): those of the
/// book as `bytes_of` gives them, but for a bundled book bigger than `KEEP_BYTES`, which is read by its parts (`Skeleton`).
fn bytes_for(path: &Path, page: Option<usize>) -> Result<Arc<Vec<u8>>, DecodeError> {
    bytes_for_keeping(path, page, KEEP_BYTES)
}

/// Page `page` drawn for the box as `decode_page_within` draws it, the book read by its parts whatever its size (for the measure
/// `core/examples/djvu_parts.rs`, which compares it with the page drawn from the whole file), and the bytes read for it.
#[doc(hidden)]
pub fn render_by_parts(
    path: &Path,
    page: usize,
    max_width: usize,
    max_height: usize,
) -> Result<(RgbaImage, usize), DecodeError> {
    let data = bytes_for_keeping(path, Some(page), 0)?;
    let read = data.len();
    let image = draw_on_thread("djvu", "the page could not be drawn", || {
        draw(&data, page, Room::Box(max_width, max_height)).map(|(image, _)| image)
    })?;
    Ok((image, read))
}

fn bytes_for_keeping(path: &Path, page: Option<usize>, keep: u64) -> Result<Arc<Vec<u8>>, DecodeError> {
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if meta.len() <= keep {
        return bytes_of(path);
    }
    let known = SKELETON
        .lock()
        .ok()
        .and_then(|s| s.as_ref().filter(|s| s.is(path, &meta)).map(Arc::clone));
    let skeleton = match known {
        Some(skeleton) => skeleton,
        None => match Skeleton::read(path, &meta)? {
            Some(read) => {
                let read = Arc::new(read);
                if let Ok(mut slot) = SKELETON.lock() {
                    *slot = Some(Arc::clone(&read));
                }
                read
            }
            None => return bytes_of(path),
        },
    };
    skeleton.sparse(page)
}

/// Page `page` (counted from 0; a number past the end is the last page) of the file and how many pages it has.
pub fn render_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, Room::Whole)
}

/// Page `page` of the file (a number past the end is the last) drawn at about `scale` of its own size (0 to 1; the page as it is
/// shown, turned as its `INFO` says), never smaller than that, and how many pages the file has. Below a half the page is made directly
/// at the smaller size, which is far less work and memory than the whole page; at a half or more it is the whole page, as
/// [`render_page`] gives it, and the caller shrinks it.
pub fn render_page_scaled(path: &Path, page: usize, scale: f64) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, Room::Scale(scale))
}

/// Page `page` made exactly `width` x `height` pixels (as [`render_page`] turns it), each side scaled on its own; a size bigger than
/// the page is the page's size. A page at half its size or less is made small directly, as the settings say (`set_shrink_as_djvulibre`,
/// `set_mask_as_djvulibre`).
pub fn render_page_exact(
    path: &Path,
    page: usize,
    width: usize,
    height: usize,
) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, Room::Exact(width, height))
}

/// As [`render_page_scaled`], rougher and faster, for finding where the print of the page is (its margins): the background is made
/// from its first chunk only, the coarse first pass of it; the mask is drawn as for reading. Never for a page that is shown.
pub fn render_page_glance(path: &Path, page: usize, scale: f64) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, Room::Glance(scale))
}

/// The size of page `page` (a number past the end is the last) as [`render_page`] draws it, turned as its `INFO` says, and how many
/// pages the file has; nothing is drawn.
pub fn page_size(path: &Path, page: usize) -> Result<((u32, u32), usize), DecodeError> {
    let data = bytes_for(path, Some(page))?;
    // on a thread of its own as the text is: a file made to hurt is an error and not the end of the caller
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu size".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let count = document.page_count();
                if count == 0 {
                    return Err(unreadable("the document has no pages"));
                }
                let page = document.page(page.min(count - 1)).map_err(unreadable)?;
                let (w, h) = (u32::from(page.info.width), u32::from(page.info.height));
                let shown = match page.info.rotation {
                    Rotation::Ccw90 | Rotation::Cw90 => (h, w),
                    Rotation::None | Rotation::Turn180 => (w, h),
                };
                Ok((shown, count))
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the size of the page could not be read")))
    })
}

/// How large a page is made.
#[derive(Debug, Clone, Copy)]
enum Room {
    /// At its own size.
    Whole,
    /// Not smaller than a box of this many pixels (the page as shown) when the box is at most half the page.
    Box(usize, usize),
    /// Not smaller than this part of its own size, when that is at most a half.
    Scale(f64),
    /// As `Scale`, rougher and faster: the background from its first chunk only (`page::Shrink::FirstChunk`), for finding the print.
    Glance(f64),
    /// Exactly this many pixels, each side scaled on its own (`page::render_exact`).
    Exact(usize, usize),
}

/// The first page of the file at a size that is not smaller than the box `max_width` x `max_height` when the box is at least twice
/// as small as the page (see `peekel_djvu::page::render_within`): for a picture that is shown small, that is much less work than the
/// whole page and a shrinking of it. The caller shrinks it the rest of the way.
/// Page `page` of the file in the same way (for tests and measures).
pub fn render_page_for_room(
    path: &Path,
    page: usize,
    max_width: usize,
    max_height: usize,
) -> Result<RgbaImage, DecodeError> {
    render_page_within(path, page, Room::Box(max_width, max_height)).map(|(image, _)| image)
}

pub fn render_within(path: &Path, max_width: usize, max_height: usize) -> Result<RgbaImage, DecodeError> {
    render_page_within(path, 0, Room::Box(max_width, max_height)).map(|(image, _)| image)
}

fn render_page_within(path: &Path, page: usize, room: Room) -> Result<(RgbaImage, usize), DecodeError> {
    let data = bytes_for(path, Some(page))?;
    let mut pages = 1;
    let image = draw_on_thread("djvu", "the page could not be drawn", || {
        let (image, count) = draw(&data, page, room)?;
        pages = count;
        Ok(image)
    })?;
    Ok((image, pages))
}

fn draw(data: &Arc<Vec<u8>>, index: usize, room: Room) -> Result<(RgbaImage, usize), DecodeError> {
    let document = Document::parse(data).map_err(unreadable)?;
    let count = document.page_count();
    if count == 0 {
        return Err(unreadable("the document has no pages"));
    }
    let entries = document
        .directory()
        .map(|directory| directory.entries().unwrap_or_default())
        .unwrap_or_default();
    let page = document.page(index.min(count - 1)).map_err(unreadable)?;
    let dictionary = match page::dictionary_chunk(&document, &entries, &page) {
        Some((Some(name), chunk)) => Some(included_dictionary(data, name, chunk)?),
        Some((None, chunk)) => Some(Arc::new(jb2::decode_dictionary(chunk, None).map_err(unreadable)?)),
        None => None,
    };
    let pixmap = match room {
        Room::Box(width, height) => page::render_within_by(&page, dictionary.as_deref(), width, height, small()),
        Room::Exact(width, height) => page::render_exact(&page, dictionary.as_deref(), width, height, small()),
        Room::Scale(scale) | Room::Glance(scale) if scale > 0.0 && scale <= 0.5 => {
            // the box of the page as it is shown, at the scale; `render_within` makes it at that size (rounded as a shrinking is)
            let (w, h) = (usize::from(page.info.width), usize::from(page.info.height));
            let (w, h) = match page.info.rotation {
                Rotation::Ccw90 | Rotation::Cw90 => (h, w),
                Rotation::None | Rotation::Turn180 => (w, h),
            };
            let side = |n: usize| ((n as f64 * scale).ceil() as usize).max(1);
            let look = match room {
                Room::Glance(_) => page::Small {
                    background: page::Shrink::FirstChunk,
                    mask: page::Shrink::Mean,
                },
                _ => small(),
            };
            page::render_within_by(&page, dictionary.as_deref(), side(w), side(h), look)
        }
        Room::Whole | Room::Scale(_) | Room::Glance(_) => page::render_with(&page, dictionary.as_deref()),
    }
    .map_err(unreadable)?;
    let mut rgba = vec![255u8; pixmap.width * pixmap.height * 4];
    for (out, pixel) in rgba
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(pixmap.rgb.as_chunks::<3>().0)
    {
        out[..3].copy_from_slice(pixel);
    }
    let width = u32::try_from(pixmap.width).map_err(unreadable)?;
    let height = u32::try_from(pixmap.height).map_err(unreadable)?;
    let image = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| unreadable("the page has the wrong size"))?;
    Ok((image, count))
}

pub use peekel_djvu::text::{Rect, normalise};

/// Marks on `image` (the page `page` of the file as `render_page` drew it) the words that `needle` (made by [`normalise`]) is in, as
/// a highlighter does: the paper turns yellow and the print stays dark. How many words were marked. Nothing is marked when the page has
/// no zones, or when the picture is not the size of the page's `INFO` (a turned page, a page drawn smaller: the zones are counted in the
/// page's own pixels, and a picture that is another shape than the page is not guessed at).
pub fn mark_words(path: &Path, page: usize, needle: &str, image: &mut RgbaImage) -> Result<usize, DecodeError> {
    Ok(find_on_page(path, page, needle)?.map_or(0, |found| paint_found(&found, image)))
}

/// Marks the rectangles found (counted in a page of the size given) on `image`, the picture of that page at any scale; how many. Used for
/// the words of a PDF too (`pdf_text`).
pub(crate) fn paint_found((rects, (width, height)): &Found, image: &mut RgbaImage) -> usize {
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 || rects.is_empty() {
        return 0;
    }
    let (scale_x, scale_y) = (
        f64::from(image.width()) / f64::from(width),
        f64::from(image.height()) / f64::from(height),
    );
    if (scale_x - scale_y).abs() > 0.02 * scale_x {
        return 0;
    }
    for rect in rects {
        // one pixel of margin round the word at the picture's scale, so that the print at its edge is covered
        let margin = scale_x.max(1.0);
        let edge = |v: i32, scale: f64, by: f64, limit: u32| ((f64::from(v) * scale + by).max(0.0) as u32).min(limit);
        let (left, right) = (
            edge(rect.left, scale_x, -margin, image.width()),
            edge(rect.right, scale_x, margin, image.width()),
        );
        let (top, bottom) = (
            edge(rect.top, scale_y, -margin, image.height()),
            edge(rect.bottom, scale_y, margin, image.height()),
        );
        for y in top..bottom {
            for x in left..right {
                let pixel = image.get_pixel_mut(x, y);
                // multiplied by a yellow: blue goes down, red and green stay (a dark print stays dark)
                pixel.0[2] = (u16::from(pixel.0[2]) * 90 / 255) as u8;
            }
        }
    }
    rects.len()
}

/// What a search finds on a page: the rectangles of the words, and the size of the page they are counted in.
pub type Found = (Vec<Rect>, (u32, u32));

/// The words of a page with where each stands, and the size of the page they are counted in.
pub type PageWords = (Vec<peekel_djvu::text::Word>, (u32, u32));

/// Where a text stands on a page: the rectangles (in the pixels of the page, the origin at the top left) of the words that `needle`
/// is in, and the size of the page (`INFO`) they are counted in. `needle` is made by [`normalise`]. `None` when the page has no
/// text layer, or its text has no zones.
pub fn find_on_page(path: &Path, page: usize, needle: &str) -> Result<Option<Found>, DecodeError> {
    let data = bytes_for(path, Some(page))?;
    // on a thread of its own as the text is: a file made to hurt is an error and not the end of the caller
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu words".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let page = document.page(page).map_err(unreadable)?;
                let size = (u32::from(page.info.width), u32::from(page.info.height));
                let words = page.words().map_err(unreadable)?;
                Ok::<_, DecodeError>(words.map(|words| (peekel_djvu::text::locate(&words, needle), size)))
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the words could not be read")))
    })
}

/// The words of page `page` with where each stands (in the pixels of the page, the origin at the top left; the lines when the zones
/// go no deeper), and the size of the page (`INFO`) they are counted in. `None` when the page has no text layer or its text has no zones.
pub fn page_words(path: &Path, page: usize) -> Result<Option<PageWords>, DecodeError> {
    let data = bytes_for(path, Some(page))?;
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu words".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let page = document
                    .page(page.min(document.page_count().saturating_sub(1)))
                    .map_err(unreadable)?;
                let size = (u32::from(page.info.width), u32::from(page.info.height));
                Ok::<_, DecodeError>(page.words().map_err(unreadable)?.map(|words| (words, size)))
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the words could not be read")))
    })
}

/// The text of page `page` as its text layer has it (lines, and the marks of paragraphs, regions and columns); `None` when the page
/// has no text layer.
pub fn page_text(path: &Path, page: usize) -> Result<Option<String>, DecodeError> {
    let data = bytes_for(path, Some(page))?;
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu text".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let page = document
                    .page(page.min(document.page_count().saturating_sub(1)))
                    .map_err(unreadable)?;
                page.text().map_err(unreadable)
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the text could not be read")))
    })
}

/// The text of every page of the file (an empty string for a page with no text layer), or `None` when no page has a text layer. A
/// page whose text cannot be read counts as one without. For a search in the book.
pub fn page_texts(path: &Path) -> Result<Option<Vec<String>>, DecodeError> {
    let data = bytes_of(path)?;
    // read on a thread of its own, so that a surprise in a file made to hurt is an error and not the end of the caller
    let texts = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu text".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let mut any = false;
                let texts: Vec<String> = (0..document.page_count())
                    .map(|index| {
                        let text = document.page(index).ok().and_then(|p| p.text().ok().flatten());
                        any |= text.is_some();
                        text.unwrap_or_default()
                    })
                    .collect();
                Ok::<_, DecodeError>(any.then_some(texts))
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the text could not be read")))
    })?;
    Ok(texts)
}

pub use crate::decode::ContentsItem;

/// The contents of the book (its bookmarks, `NAVM`) as lines in reading order, or `None` when it has none.
pub fn contents(path: &Path) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    let data = bytes_for(path, None)?;
    // read on a thread of its own, as the text is
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("djvu contents".to_string())
            .stack_size(crate::guard::STACK)
            .spawn_scoped(scope, || {
                let document = Document::parse(&data).map_err(unreadable)?;
                let Some(chunk) = document.bookmarks() else {
                    return Ok(None);
                };
                let tree = peekel_djvu::nav::parse(chunk.data).map_err(unreadable)?;
                let entries = document
                    .directory()
                    .map(|d| d.entries().unwrap_or_default())
                    .unwrap_or_default();
                let pages = document.page_count();
                let items: Vec<ContentsItem> = peekel_djvu::nav::flatten(&tree)
                    .into_iter()
                    .map(|(depth, b)| ContentsItem {
                        depth,
                        title: b.title.clone(),
                        page: peekel_djvu::nav::page_of(&b.address, &entries, pages),
                    })
                    .collect();
                Ok::<_, DecodeError>((!items.is_empty()).then_some(items))
            })
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable("the contents could not be read")))
    })
}

/// What the card shows of a DjVu file: how many pages, and the size of the first as it is shown (a page that is stored turned a
/// quarter has its width and height the other way round).
pub fn properties(data: &[u8]) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let Ok(document) = Document::parse(data) else {
        return rows;
    };
    rows.push(("Pages".to_string(), document.page_count().to_string()));
    if let Ok(first) = document.page(0) {
        let (mut width, mut height, dpi) = (first.info.width, first.info.height, first.info.dpi);
        let turn = match first.info.rotation {
            Rotation::None => None,
            Rotation::Turn180 => Some("turned upside down"),
            Rotation::Ccw90 => Some("turned a quarter counter-clockwise"),
            Rotation::Cw90 => Some("turned a quarter clockwise"),
        };
        if matches!(first.info.rotation, Rotation::Ccw90 | Rotation::Cw90) {
            std::mem::swap(&mut width, &mut height);
        }
        let mut size = format!("{width} x {height} px");
        if dpi > 0 {
            let mm = |pixels: u16| f64::from(pixels) * 25.4 / f64::from(dpi);
            size.push_str(&format!(", {dpi} dpi ({:.0} x {:.0} mm)", mm(width), mm(height)));
        }
        rows.push(("Page size".to_string(), size));
        if let Some(turn) = turn {
            rows.push(("Rotation".to_string(), format!("the first page is {turn}")));
        }
        // the layers of the page that this reader does not draw: the page is then white or without its text where they are
        let missing: Vec<&str> = [
            (*b"BG2k", "the JPEG 2000 background"),
            (*b"FG2k", "the JPEG 2000 foreground colours"),
        ]
        .iter()
        .filter(|(id, _)| first.chunks(*id).next().is_some())
        .map(|(_, what)| *what)
        .collect();
        if !missing.is_empty() {
            rows.push((
                "Not drawn".to_string(),
                format!("on the first page: {}", missing.join(", ")),
            ));
        }
    }
    rows
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot read its fixture should stop at once
mod tests {
    use super::*;

    fn sample(folder: &str, name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the repository")
            .join("djvu")
            .join("tests")
            .join(folder)
            .join(name)
    }

    /// Page `page` drawn from `data` as the drawing calls draw it.
    fn drawn(data: &Arc<Vec<u8>>, page: usize) -> (RgbaImage, usize) {
        draw(data, page, Room::Whole).expect("drawn")
    }

    #[test]
    fn a_bundled_book_read_by_its_parts_draws_its_pages_as_the_whole_file_does() {
        for path in [sample("text", "text.djvu"), sample("nav", "nav.djvu")] {
            let whole = Arc::new(std::fs::read(&path).expect("read"));
            let count = Document::parse(&whole).expect("parses").page_count();
            assert!(count >= 2, "{path:?}");
            for page in 0..=count {
                // read by its parts whatever its size; a page past the end is the last
                let parts = bytes_for_keeping(&path, Some(page), 0).expect("by its parts");
                assert!(
                    parts.len() < whole.len() || count == 1,
                    "{} of {}",
                    parts.len(),
                    whole.len()
                );
                let document = Document::parse(&parts).expect("a document");
                assert_eq!(document.page_count(), count);
                let (a, n) = drawn(&whole, page);
                let (b, m) = drawn(&parts, page);
                assert_eq!((n, m), (count, count));
                assert!(a == b, "page {page} of {path:?}");
                // its text and its words too
                let index = page.min(count - 1);
                assert_eq!(
                    document.page(index).expect("a page").text().expect("text"),
                    Document::parse(&whole)
                        .expect("parses")
                        .page(index)
                        .expect("a page")
                        .text()
                        .expect("text")
                );
            }
            // the directory and the bookmarks alone: the contents as the whole file has them
            let head = bytes_for_keeping(&path, None, 0).expect("the head");
            let (a, b) = (
                Document::parse(&whole).expect("parses"),
                Document::parse(&head).expect("parses"),
            );
            assert_eq!(a.bookmarks().map(|c| c.data), b.bookmarks().map(|c| c.data));
            assert_eq!(a.page_count(), b.page_count());
        }
    }

    #[test]
    fn a_part_of_a_page_is_made_exactly_the_size_asked_and_cut_where_the_part_is() {
        let path = sample("text", "text.djvu");
        let ((w, h), _) = page_size(&path, 0).expect("a size");
        let whole = crate::content::PageBox {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        // the whole page as the part, at about a third: the page made at that size, nothing cut
        let (third_w, third_h) = (w / 3, h / 3);
        let (part, count) = crate::decode::render_page_part(&path, 0, whole, third_w, third_h).expect("drawn");
        let (page, _) = render_page_exact(&path, 0, third_w as usize, third_h as usize).expect("drawn");
        assert!(count >= 2);
        assert!(part == page);
        // the right half at about a third: the right edge of the page made at the size at which the half is that wide
        let right = crate::content::PageBox {
            x: w / 2,
            y: 0,
            width: w - w / 2,
            height: h,
        };
        let half_w = (w - w / 2) / 3;
        let (half, _) = crate::decode::render_page_part(&path, 0, right, half_w, third_h).expect("drawn");
        assert_eq!(half.dimensions(), (half_w, third_h));
        let page_w = (f64::from(w) * f64::from(half_w) / f64::from(w - w / 2)).round() as u32;
        let (page, _) = render_page_exact(&path, 0, page_w as usize, third_h as usize).expect("drawn");
        let x = page.width() - half_w;
        assert!(half == image::imageops::crop_imm(&page, x, 0, half_w, third_h).to_image());
        // other sizes, of other proportions, and a part made bigger than it is: always the size asked
        let corner = crate::content::PageBox {
            x: w / 4,
            y: h / 4,
            width: w / 8,
            height: h / 8,
        };
        for (pw, ph) in [(97, 41), (13, 200), (w / 4, h / 4), (1, 1)] {
            let (made, _) = crate::decode::render_page_part(&path, 0, corner, pw, ph).expect("drawn");
            assert_eq!(made.dimensions(), (pw, ph));
        }
    }

    #[test]
    fn a_file_that_is_not_a_bundled_book_is_read_whole_and_a_spoilt_directory_is_an_error() {
        // a single page is read whole, as before
        let single = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("djvu")
            .join("ramp.djvu");
        let whole = std::fs::read(&single).expect("read");
        assert_eq!(*bytes_for_keeping(&single, Some(0), 0).expect("whole"), whole);

        // a bundled book whose directory says a component stands where none does
        let mut bytes = std::fs::read(sample("text", "text.djvu")).expect("read");
        let dir = std::env::temp_dir().join(format!("peekel-djvu-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        // the first offset of the directory (after its flag and count, 3 bytes into the DIRM data at 24) made to point past the end
        bytes[27..31].copy_from_slice(&u32::MAX.to_be_bytes());
        let path = dir.join("spoilt.djvu");
        std::fs::write(&path, &bytes).expect("written");
        // the first page stands past the end of the file: an error, not a page of nothing
        let result = bytes_for_keeping(&path, Some(0), 0);
        assert!(result.is_err(), "{:?}", result.map(|d| d.len()));
        // the page made at a size, glanced at, or a part of it made at a size, from such a file: an error or the size asked, as the
        // page drawn whole is
        let part = crate::content::PageBox {
            x: 10,
            y: 10,
            width: 100,
            height: 100,
        };
        let whole = render_page(&path, 0).map(|(image, _)| image.dimensions());
        let exact = render_page_exact(&path, 0, 50, 70).map(|(image, _)| image.dimensions());
        assert_eq!(exact.is_ok(), whole.is_ok());
        assert!(exact.is_err() || exact.ok() == Some((50, 70)));
        assert_eq!(render_page_glance(&path, 0, 0.25).is_ok(), whole.is_ok());
        let made = crate::decode::render_page_part(&path, 0, part, 30, 40).map(|(image, _)| image.dimensions());
        assert!(made.is_err() || made.ok() == Some((30, 40)));
        // a component made to say it is far larger than the file
        let mut bytes = std::fs::read(sample("text", "text.djvu")).expect("read");
        let head = peekel_djvu::document::DirectoryHead::parse(&bytes[24..]).expect("a directory");
        let at = head.offsets[head.offsets.len() - 1] as usize;
        bytes[at + 4..at + 8].copy_from_slice(&u32::MAX.to_be_bytes());
        let path = dir.join("large.djvu");
        std::fs::write(&path, &bytes).expect("written");
        let last = Document::parse(&std::fs::read(sample("text", "text.djvu")).expect("read"))
            .expect("parses")
            .page_count()
            - 1;
        assert!(bytes_for_keeping(&path, Some(last), 0).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
