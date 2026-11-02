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
/// again; a bigger one is read for each page. Only the last book is kept.
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

/// Whether the name says DjVu (`.djvu` or `.djv`).
pub fn is_djvu(path: &Path) -> bool {
    has_extension(path, "djvu") || has_extension(path, "djv")
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
        return Err(DecodeError::TooLarge);
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

/// The dictionary of shapes that pages include under `name`, decoded once for the book whose bytes these are.
fn included_dictionary(data: &Arc<Vec<u8>>, name: &[u8], chunk: &[u8]) -> Result<Arc<Dictionary>, DecodeError> {
    let is_kept = |book: &Book| Arc::ptr_eq(&book.data, data);
    if let Ok(kept) = BOOK.lock()
        && let Some(book) = kept.as_ref().filter(|b| is_kept(b))
        && let Some(found) = book.dictionaries.get(name)
    {
        return Ok(Arc::clone(found));
    }
    let decoded = Arc::new(jb2::decode_dictionary(chunk, None).map_err(unreadable)?);
    if let Ok(mut kept) = BOOK.lock()
        && let Some(book) = kept.as_mut().filter(|b| is_kept(b))
        && book.dictionaries.len() < MAX_DICTIONARIES
    {
        book.dictionaries.insert(name.to_vec(), Arc::clone(&decoded));
    }
    Ok(decoded)
}

/// Page `page` (counted from 0; a number past the end is the last page) of the file and how many pages it has.
pub fn render_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, None)
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
    render_page_within(path, page, Some((max_width, max_height))).map(|(image, _)| image)
}

pub fn render_within(path: &Path, max_width: usize, max_height: usize) -> Result<RgbaImage, DecodeError> {
    render_page_within(path, 0, Some((max_width, max_height))).map(|(image, _)| image)
}

fn render_page_within(
    path: &Path,
    page: usize,
    room: Option<(usize, usize)>,
) -> Result<(RgbaImage, usize), DecodeError> {
    let data = bytes_of(path)?;
    let mut pages = 1;
    let image = draw_on_thread("djvu", "the page could not be drawn", || {
        let (image, count) = draw(&data, page, room)?;
        pages = count;
        Ok(image)
    })?;
    Ok((image, pages))
}

fn draw(data: &Arc<Vec<u8>>, index: usize, room: Option<(usize, usize)>) -> Result<(RgbaImage, usize), DecodeError> {
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
        Some((width, height)) => page::render_within(&page, dictionary.as_deref(), width, height),
        None => page::render_with(&page, dictionary.as_deref()),
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
