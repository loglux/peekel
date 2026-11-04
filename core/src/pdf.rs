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
pub(crate) const MAX_BYTES: u64 = 256 * 1024 * 1024;
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
pub(crate) fn on_thread<T: Send + 'static>(
    timeout: Duration,
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DecodeError> {
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

/// The characters of PDFDocEncoding (the specification's Annex D, Table D.2) that are not those of Latin-1: the codes 0x18 to 0x1F, 0x80 to
/// 0x9E and 0xA0 (the last is in `text_string`). The codes 0x7F, 0x9F and 0xAD are undefined there.
const PDF_DOC: [(u8, char); 39] = [
    (0x18, '\u{2D8}'),
    (0x19, '\u{2C7}'),
    (0x1A, '\u{2C6}'),
    (0x1B, '\u{2D9}'),
    (0x1C, '\u{2DD}'),
    (0x1D, '\u{2DB}'),
    (0x1E, '\u{2DA}'),
    (0x1F, '\u{2DC}'),
    (0x80, '\u{2022}'),
    (0x81, '\u{2020}'),
    (0x82, '\u{2021}'),
    (0x83, '\u{2026}'),
    (0x84, '\u{2014}'),
    (0x85, '\u{2013}'),
    (0x86, '\u{192}'),
    (0x87, '\u{2044}'),
    (0x88, '\u{2039}'),
    (0x89, '\u{203A}'),
    (0x8A, '\u{2212}'),
    (0x8B, '\u{2030}'),
    (0x8C, '\u{201E}'),
    (0x8D, '\u{201C}'),
    (0x8E, '\u{201D}'),
    (0x8F, '\u{2018}'),
    (0x90, '\u{2019}'),
    (0x91, '\u{201A}'),
    (0x92, '\u{2122}'),
    (0x93, '\u{FB01}'),
    (0x94, '\u{FB02}'),
    (0x95, '\u{141}'),
    (0x96, '\u{152}'),
    (0x97, '\u{160}'),
    (0x98, '\u{178}'),
    (0x99, '\u{17D}'),
    (0x9A, '\u{131}'),
    (0x9B, '\u{142}'),
    (0x9C, '\u{153}'),
    (0x9D, '\u{161}'),
    (0x9E, '\u{17E}'),
];

/// A PDF text string (7.9.2.2) as a string: UTF-16BE with a byte-order mark (the specification), UTF-16LE with one and UTF-8 (seen in files
/// and in PDF 2.0), else PDFDocEncoding. Control characters are dropped, the text is cut to `limit` characters and trimmed.
pub(crate) fn text_string(bytes: &[u8], limit: usize) -> String {
    let text = if let Some(body) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(body) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(body) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(body).into_owned()
    } else if let Ok(utf8) = std::str::from_utf8(bytes) {
        utf8.to_string()
    } else {
        bytes
            .iter()
            .map(|&b| {
                if let Some(&(_, c)) = PDF_DOC.iter().find(|(code, _)| *code == b) {
                    c
                } else if b == 0xA0 {
                    '\u{20AC}'
                } else {
                    char::from(b)
                }
            })
            .collect()
    };
    text.chars()
        .filter(|c| !c.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A text of the PDF's information as a string (see [`text_string`]), at most 300 characters.
fn pdf_text(bytes: &[u8]) -> String {
    text_string(bytes, 300)
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

#[cfg(test)]
mod text_string_tests {
    use super::text_string;

    #[test]
    fn a_text_string_is_read_in_the_encodings_the_specification_names() {
        // UTF-16BE with a byte-order mark
        assert_eq!(text_string(&[0xFE, 0xFF, 0x04, 0x13, 0x04, 0x3B], 100), "Гл");
        // PDFDocEncoding: ASCII, and the codes that are not Latin-1 (a bullet, an en dash, an ellipsis, the Euro sign, a small z with caron)
        assert_eq!(text_string(b"Plain text", 100), "Plain text");
        assert_eq!(
            text_string(&[0x80, b' ', 0x85, b' ', 0x83, b' ', 0xA0, b' ', 0x9E], 100),
            "\u{2022} \u{2013} \u{2026} \u{20AC} \u{17E}"
        );
        // the codes that are the same as Latin-1 stay so (an e with an acute accent)
        assert_eq!(text_string(&[b'e', 0xE9], 100), "e\u{e9}");
        // a string is cut, trimmed, and has no control characters
        assert_eq!(text_string(b"  a\x01b  ", 100), "ab");
        assert_eq!(text_string(b"abcdef", 3), "abc");
    }
}
