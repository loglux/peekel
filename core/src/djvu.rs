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

pub use peekel_djvu::text::{Rect, normalise};

/// Marks on `image` (the page `page` of the file as `render_page` drew it) the words that `needle` (made by [`normalise`]) is in, as
/// a highlighter does: the paper turns yellow and the print stays dark. How many words were marked. Nothing is marked when the page has
/// no zones, or when the picture is not the size of the page's `INFO` (a turned page, a page drawn smaller: the zones are counted in the
/// page's own pixels, and a picture that is another shape than the page is not guessed at).
pub fn mark_words(path: &Path, page: usize, needle: &str, image: &mut RgbaImage) -> Result<usize, DecodeError> {
    let Some((rects, (width, height))) = find_on_page(path, page, needle)? else {
        return Ok(0);
    };
    if width == 0 || height == 0 || rects.is_empty() {
        return Ok(0);
    }
    let (scale_x, scale_y) = (
        f64::from(image.width()) / f64::from(width),
        f64::from(image.height()) / f64::from(height),
    );
    if (scale_x - scale_y).abs() > 0.02 * scale_x {
        return Ok(0);
    }
    for rect in &rects {
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
    Ok(rects.len())
}

/// What a search finds on a page: the rectangles of the words, and the size of the page they are counted in.
pub type Found = (Vec<Rect>, (u32, u32));

/// Where a text stands on a page: the rectangles (in the pixels of the page, the origin at the top left) of the words that `needle`
/// is in, and the size of the page (`INFO`) they are counted in. `needle` is made by [`normalise`]. `None` when the page has no
/// text layer, or its text has no zones.
pub fn find_on_page(path: &Path, page: usize, needle: &str) -> Result<Option<Found>, DecodeError> {
    let data = bytes_of(path)?;
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

/// A line of the contents of a book: how deep it is (0 for the top level), its title, and the page (from 0) it leads to, if it leads to
/// one of the book's pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentsItem {
    pub depth: usize,
    pub title: String,
    pub page: Option<usize>,
}

/// The contents of the book (its bookmarks, `NAVM`) as lines in reading order, or `None` when it has none.
pub fn contents(path: &Path) -> Result<Option<Vec<ContentsItem>>, DecodeError> {
    let data = bytes_of(path)?;
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
