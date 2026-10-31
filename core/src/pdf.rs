//! The first page of a PDF drawn into pixels with `hayro` (pure Rust, with the standard fonts and the codecs of scans:
//! JBIG2, JPEG 2000, CCITT, inside it).
//!
//! Like an SVG, a page has no pixel size of its own, so it is drawn once with its longest side at `SIDE` pixels and then
//! handled like any other picture. A scan (one picture per page) and a text page are drawn the same way. Not done:
//! encrypted files (refused), pages other than the first, annotations' appearance beyond what `hayro` draws.
//!
//! A file made to hurt (a page of a billion points, a cut file, loops of references) must not take the caller down:
//! the page is drawn on a thread with a large stack, a panic of it is an error, and the size of the page, of the file and
//! of the picture are limited before anything is drawn.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use image::RgbaImage;

use crate::decode::DecodeError;
use crate::guard::{STACK, has_extension, unreadable};

/// A bigger file is refused before it is parsed.
const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The longest side of the picture of a page.
const SIDE: f32 = 2048.0;
/// Neither side of the picture is longer than this (a page of absurd proportions would be a very long strip).
const MAX_SIDE: f32 = 4096.0;
/// How long the caller waits for a page. Real scans of 26 to 59 MB took 1.5 to 7 s to draw, a file made of a hundred
/// thousand shapes 13 s; drawing cannot be cancelled, so after this the caller gets an error and the thread is left to
/// finish by itself, unseen.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Whether the name says PDF.
pub fn is_pdf(path: &Path) -> bool {
    has_extension(path, "pdf")
}

/// Draws the first page of the PDF in `path` on white. The drawing runs on a thread of its own with a large stack; a
/// panic of it is an error, and so is a drawing that takes longer than `TIMEOUT`.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    render_first_page_within(path, TIMEOUT)
}

/// `render_first_page` with another time to wait.
pub fn render_first_page_within(path: &Path, timeout: Duration) -> Result<RgbaImage, DecodeError> {
    render_page_within(path, 0, timeout).map(|(image, _)| image)
}

/// Page number `page` (counted from 0; a number past the end is the last page) of the PDF, and how many pages it has.
pub fn render_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    render_page_within(path, page, TIMEOUT)
}

/// `render_page` with another time to wait.
pub fn render_page_within(path: &Path, page: usize, timeout: Duration) -> Result<(RgbaImage, usize), DecodeError> {
    if std::fs::metadata(path).map_err(unreadable)?.len() > MAX_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let data = std::fs::read(path).map_err(unreadable)?;
    on_thread(timeout, move || draw(data, page))?
}

/// Runs `job` on a thread of its own with a large stack and waits for it at most `timeout`; a panic of the job, and a job
/// that takes longer, are errors (the thread of a job that is given up is left to end by itself).
fn on_thread<T: Send + 'static>(timeout: Duration, job: impl FnOnce() -> T + Send + 'static) -> Result<T, DecodeError> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("pdf".to_string())
        .stack_size(STACK)
        .spawn(move || {
            // a panic ends the thread before it sends: the caller then sees a closed channel
            let _ = send.send(job());
        })
        .map_err(unreadable)?;
    match receive.recv_timeout(timeout) {
        Ok(result) => Ok(result),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(unreadable("the PDF took too long")),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(unreadable("the PDF could not be read")),
    }
}

/// What the PDF in `data` says about itself, as (label, value) rows: pages, size of the first page, version, title, author and
/// the rest of its information. An error of reading is a row too (an encrypted file says so).
pub fn properties(data: Vec<u8>) -> Vec<(String, String)> {
    on_thread(Duration::from_secs(5), move || read_properties(data)).unwrap_or_default()
}

/// A text of the PDF's information as a string: UTF-16 with a byte-order mark, UTF-8, or the 8-bit PDFDocEncoding.
fn pdf_text(bytes: &[u8]) -> String {
    let text = if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else if let Ok(utf8) = std::str::from_utf8(bytes) {
        utf8.to_string()
    } else {
        bytes.iter().map(|&b| char::from(b)).collect()
    };
    text.chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect::<String>()
        .trim()
        .to_string()
}

fn paper(width_pt: f32, height_pt: f32) -> String {
    let (w, h) = (width_pt * 25.4 / 72.0, height_pt * 25.4 / 72.0);
    let (short, long) = (w.min(h), w.max(h));
    let name = [
        ("A3", 297.0, 420.0),
        ("A4", 210.0, 297.0),
        ("A5", 148.0, 210.0),
        ("Letter", 215.9, 279.4),
        ("Legal", 215.9, 355.6),
    ]
    .iter()
    .find(|(_, a, b)| (short - a).abs() < 2.0 && (long - b).abs() < 2.0)
    .map(|(n, _, _)| format!(", {n}"))
    .unwrap_or_default();
    format!("{w:.0} x {h:.0} mm{name}")
}

fn read_properties(data: Vec<u8>) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let pdf = match Pdf::new(data) {
        Ok(pdf) => pdf,
        Err(hayro::hayro_syntax::LoadPdfError::Decryption(_)) => {
            rows.push(("Encrypted".to_string(), "yes, a password is needed".to_string()));
            return rows;
        }
        Err(_) => return rows,
    };
    let pages = pdf.pages();
    rows.push(("Pages".to_string(), pages.len().to_string()));
    if let Some(first) = pages.first() {
        let (w, h) = first.render_dimensions();
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            rows.push(("Page size".to_string(), paper(w, h)));
        }
    }
    // "Pdf17" is PDF 1.7, "Pdf20" is 2.0
    let digits = format!("{:?}", pdf.version()).replace("Pdf", "");
    if digits.len() == 2 {
        rows.push(("PDF version".to_string(), format!("{}.{}", &digits[..1], &digits[1..])));
    }
    let meta = pdf.metadata();
    for (label, value) in [
        ("Title", &meta.title),
        ("Author", &meta.author),
        ("Subject", &meta.subject),
        ("Keywords", &meta.keywords),
        ("Created with", &meta.creator),
        ("Producer", &meta.producer),
    ] {
        if let Some(text) = value.as_deref().map(pdf_text).filter(|t| !t.is_empty()) {
            rows.push((label.to_string(), text));
        }
    }
    for (label, date) in [("Created", meta.creation_date), ("Modified", meta.modification_date)] {
        if let Some(d) = date {
            rows.push((
                label.to_string(),
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    d.year, d.month, d.day, d.hour, d.minute
                ),
            ));
        }
    }
    rows
}

/// The scale that brings a page of `width` × `height` points to `SIDE` pixels on its longest side, or `None` when the
/// page has no usable size.
fn scale_for(width: f32, height: f32) -> Option<f32> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return None;
    }
    let longest = width.max(height);
    let scale = SIDE / longest;
    // the short side must stay at least a pixel, and the long one within the limit
    ((width * scale).min(height * scale) >= 1.0 && (width * scale).max(height * scale) <= MAX_SIDE).then_some(scale)
}

fn draw(data: Vec<u8>, number: usize) -> Result<(RgbaImage, usize), DecodeError> {
    let pdf = Pdf::new(data).map_err(|e| unreadable(format!("the PDF cannot be read ({e:?})")))?;
    let pages = pdf.pages();
    let count = pages.len();
    let page = pages
        .get(number.min(count.saturating_sub(1)))
        .ok_or_else(|| unreadable("the PDF has no pages"))?;
    let (width, height) = page.render_dimensions();
    let scale = scale_for(width, height).ok_or_else(|| unreadable("the page of the PDF has no usable size"))?;
    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
    // the page is drawn on white, so the pixels are opaque and premultiplied is the same as straight
    RgbaImage::from_raw(w, h, pixmap.data_as_u8_slice().to_vec())
        .map(|image| (image, count))
        .ok_or_else(|| unreadable("the page was not drawn"))
}
