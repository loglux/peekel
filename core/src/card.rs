//! A card for a file that is no picture: its kind (told by the first bytes, not the name), its name, size and date,
//! drawn as an SVG with the installed fonts. The Far plugin shows it in the quick view in place of the file's bytes.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use image::RgbaImage;

use crate::decode::DecodeError;
use crate::svg;
use crate::xml::escape;

/// The families a card's text is set in: Segoe UI, then the free fonts that stand for it (`fontset`), then Arial and the system's.
const FAMILIES: &str = "Segoe UI, Selawik, Liberation Sans, Arial, sans-serif";

/// What the signature of an OLE compound file says by itself: it is the container of old Word, Excel and PowerPoint files and also
/// of Windows Installer packages, Outlook messages, Visio and Publisher files and more, so which one it is is for the
/// extension to say (`info::of`).
pub const OLE_COMPOUND: &str = "OLE compound file (old Office document, installer, ...)";

/// How the start of a file tells its kind: (offset, bytes, name). The first that matches wins.
const SIGNS: &[(usize, &[u8], &str)] = &[
    (0, b"%PDF-", "PDF document"),
    (0, b"AT&TFORM", "DjVu document"),
    (0, b"\xff\xd8\xff", "JPEG image"),
    (0, b"\x89PNG\r\n\x1a\n", "PNG image"),
    (0, b"GIF87a", "GIF image"),
    (0, b"GIF89a", "GIF image"),
    (0, b"II*\x00", "TIFF image"),
    (0, b"MM\x00*", "TIFF image"),
    (0, b"MZ", "Windows program or library"),
    (0, b"\x7fELF", "Linux program or library"),
    (0, b"PK\x03\x04", "ZIP archive"),
    (0, b"7z\xbc\xaf\x27\x1c", "7-Zip archive"),
    (0, b"Rar!\x1a\x07", "RAR archive"),
    (0, b"\x1f\x8b", "gzip archive"),
    (0, b"BZh", "bzip2 archive"),
    (0, b"\xfd7zXZ\x00", "xz archive"),
    (0, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1", OLE_COMPOUND),
    (0, b"SQLite format 3\x00", "SQLite database"),
    (0, b"ID3", "MP3 audio"),
    (0, b"fLaC", "FLAC audio"),
    (0, b"OggS", "Ogg audio or video"),
    (0, b"\x1aE\xdf\xa3", "Matroska or WebM video"),
    (4, b"ftyp", "MP4 or MOV video"),
    (0, b"RIFF", "RIFF file (WAV, AVI)"),
    (0, b"\x00\x00\x01\x00", "Windows icon"),
    (0, b"\x00asm", "WebAssembly module"),
    (0, b"\xca\xfe\xba\xbe", "Java class"),
    (0, b"wOFF", "Web font"),
    (0, b"wOF2", "Web font"),
    (0, b"\x00\x01\x00\x00\x00", "TrueType font"),
    (0, b"OTTO", "OpenType font"),
    (0, b"ttcf", "Font collection"),
];

/// The kind of file the first bytes say, or a plain "binary data".
pub fn kind(head: &[u8]) -> &'static str {
    // a WebP is a RIFF file with a word in it, a BMP starts with two letters and four zero bytes after its size
    if head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return "WebP image";
    }
    if head.get(..2) == Some(b"BM") && head.get(6..10) == Some(&[0, 0, 0, 0]) {
        return "BMP image";
    }
    // an ISO media file (`ftyp`) is a video, but also a HEIC or AVIF picture or a Canon CR3 photograph: the brand says which
    if head.get(4..8) == Some(b"ftyp") {
        return match head.get(8..12) {
            Some(b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1") => "HEIF image (HEIC)",
            Some(b"avif" | b"avis") => "AVIF image",
            Some(b"crx ") => "Canon CR3 RAW photograph",
            Some(b"qt  ") => "QuickTime video (MOV)",
            Some(b"M4A ") => "MP4 audio",
            _ => "MP4 or MOV video",
        };
    }
    SIGNS
        .iter()
        .find(|(at, sign, _)| head.get(*at..*at + sign.len()) == Some(sign))
        .map_or("Binary data", |&(_, _, name)| name)
}

/// 1536 -> "1.5 KB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The calendar date of a day counted from 1970-01-01 (Howard Hinnant's algorithm): year, month, day.
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// "2026-10-30 12:41 UTC".
pub fn date(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// `text` broken at spaces into at most four lines of `max` characters (the second ends in `…` when there was more), a word
/// longer than a line cut in the middle.
fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(4);
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split_whitespace() {
        let word = if word.chars().count() > max {
            shorten(word, max)
        } else {
            word.to_string()
        };
        let current = lines.last().map_or(0, |l| l.chars().count());
        if current == 0 {
            if let Some(last) = lines.last_mut() {
                last.push_str(&word);
            }
        } else if current + 1 + word.chars().count() <= max {
            if let Some(last) = lines.last_mut() {
                last.push(' ');
                last.push_str(&word);
            }
        } else {
            lines.push(word);
        }
    }
    if lines.len() > 4 {
        lines.truncate(4);
        if let Some(last) = lines.last_mut() {
            *last = shorten(&format!("{last} …"), max);
        }
    }
    lines.retain(|l| !l.is_empty());
    lines
}

/// A name cut in the middle to at most `max` characters, so that the extension stays.
fn shorten(name: &str, max: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max || max < 8 {
        return name.to_string();
    }
    let tail = max * 2 / 5;
    let head = max - tail - 1;
    chars[..head]
        .iter()
        .chain(['…'].iter())
        .chain(&chars[chars.len() - tail..])
        .collect()
}

/// The card of the file at `path`, `width` × `height` pixels, transparent behind the text, whose colour is chosen to be
/// read on `background`: its kind, its name, and what `info` knows of it (size, dates, the warning when the name and the
/// content disagree, the camera of a photograph, the pages of a PDF and so on).
pub fn render(path: &Path, width: usize, height: usize, background: [u8; 3]) -> Result<RgbaImage, DecodeError> {
    render_with_note(path, width, height, background, None)
}

/// `render`, with one more row (`Codec`) when the caller has something to say about the file that the card cannot know, like the
/// extension of the system that would open it.
pub fn render_with_note(
    path: &Path,
    width: usize,
    height: usize,
    background: [u8; 3],
    note: Option<&str>,
) -> Result<RgbaImage, DecodeError> {
    std::fs::metadata(path).map_err(|e| DecodeError::Unreadable(e.to_string()))?;
    let mut info = crate::info::of(path);
    if let Some(note) = note {
        info.rows.push(("Codec".to_string(), note.to_string()));
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if (info.kind.ends_with("font") || info.kind == "Font collection")
        && let Some(card) = font_card(path, &info, &name, width, height, background)
    {
        return Ok(card);
    }
    let mut card = render_rows(&info.kind, &name, &info.rows, width, height, background)?;
    // a program shows its own icon in the corner
    if info.kind == "Windows program or library"
        && let Some(icon) = std::fs::read(path)
            .ok()
            .filter(|d| d.len() <= 64 * 1024 * 1024)
            .and_then(|d| crate::pe::icon(&d))
    {
        let side = (height as f32 * 0.22).clamp(32.0, 128.0) as u32;
        let scaled = image::imageops::resize(&icon, side, side, image::imageops::FilterType::Lanczos3);
        let (x, y) = (
            width as i64 - i64::from(side) - (width as f32 * 0.05) as i64,
            (height as f32 * 0.06) as i64,
        );
        image::imageops::overlay(&mut card, &scaled, x, y);
    }
    Ok(card)
}

/// The card of a font: its rows above, and below them lines of text set in the font (the Cyrillic one only when the font has the
/// letters). `None` when the font cannot be read for it.
fn font_card(
    path: &Path,
    info: &crate::info::Info,
    name: &str,
    width: usize,
    height: usize,
    background: [u8; 3],
) -> Option<RgbaImage> {
    const LATIN: &str = "The quick brown fox jumps over the lazy dog";
    const CYRILLIC: &str = "Съешь же ещё этих мягких французских булок";
    let data = std::fs::read(path).ok().filter(|d| d.len() <= 64 * 1024 * 1024)?;
    let (width, height) = (width.clamp(80, 4096), height.clamp(60, 4096));
    let [r, g, b] = background;
    let light = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 128;
    let ink = if light { "#202020" } else { "#f0f0f0" };
    // the rows keep the upper half, the sample the lower
    let upper = (height as f32 * 0.55) as usize;
    let rows = &info.rows[..info.rows.len().min(9)];
    let mut canvas = render_rows(&info.kind, name, rows, width, upper, background).ok()?;
    let left = width as f32 * 0.07;
    // the longest line is 43 letters, up to 0.6 of the size wide each: it must fit in the width that is left
    let size = (height as f32 / 12.0).min(width as f32 * 0.86 / 27.0).clamp(9.0, 44.0);
    let mut lines = vec![
        (size, LATIN.to_string()),
        (size * 0.8, "ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789".to_string()),
    ];
    if crate::font::has_letters(&data, CYRILLIC) {
        lines.push((size, CYRILLIC.to_string()));
    }
    let area = (width as f32 - 2.0 * left) as u32;
    let sample = svg::render_in_font(data, area, (height - upper) as u32, ink, &lines).ok()?;
    let mut full = RgbaImage::new(width as u32, height as u32);
    image::imageops::overlay(&mut full, &canvas, 0, 0);
    image::imageops::overlay(&mut full, &sample, left as i64, upper as i64);
    canvas = full;
    Some(canvas)
}

/// A card made of a title, a line under it, and rows of (label, value) in two columns, `width` × `height` pixels, transparent
/// behind the text, whose colour is chosen to be read on `background`. Used for the card of a file and for the properties the
/// viewer shows.
pub fn render_rows(
    title: &str,
    subtitle: &str,
    rows: &[(String, String)],
    width: usize,
    height: usize,
    background: [u8; 3],
) -> Result<RgbaImage, DecodeError> {
    let (width, height) = (width.clamp(80, 4096), height.clamp(60, 4096));
    let [r, g, b] = background;
    let light = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 128;
    let (ink, soft, warn) = if light {
        ("#202020", "#606060", "#b00020")
    } else {
        ("#f0f0f0", "#a0a0a0", "#ff8a80")
    };
    let rows = &rows[..rows.len().min(16)];
    let left = width as f32 * 0.07;
    let room = width as f32 - 2.0 * left;
    // a character is about half the font size wide
    let chars_in = |room: f32, size: f32| (room / (size * 0.5)).max(4.0) as usize;
    let label_chars = rows.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0) as f32;
    let values_for = |unit: f32| -> Vec<Vec<String>> {
        let value_room = room - (label_chars * unit * 0.5 + unit);
        rows.iter()
            .map(|(_, value)| wrap(value, chars_in(value_room, unit)))
            .collect()
    };
    // as large as the width and the height allow, down to a limit: smaller until the lines that the values need fit
    let mut unit = (height as f32 * 0.86 / ((rows.len() as f32 + 3.2) * 1.55))
        .min(width as f32 / 22.0)
        .clamp(7.0, 40.0);
    let mut values = values_for(unit);
    loop {
        let lines: usize = values.iter().map(Vec::len).sum();
        if (lines as f32 + 3.2) * unit * 1.55 <= height as f32 * 0.9 || unit <= 7.0 {
            break;
        }
        unit = (unit * 0.92).max(7.0);
        values = values_for(unit);
    }
    let pitch = unit * 1.55;
    let label_width = label_chars * unit * 0.5 + unit;
    let lines: usize = values.iter().map(Vec::len).sum();
    let block = pitch * (lines as f32 + 2.2);
    let mut y = ((height as f32 - block) / 2.0).max(unit * 0.5) + unit * 1.2;
    let mut body = String::new();
    let mut add = |x: f32, y: f32, size: f32, colour: &str, text: &str, bold: bool| {
        body += &format!(
            r#"<text x="{x:.1}" y="{y:.1}" font-size="{size:.1}" fill="{colour}"{} font-family="{FAMILIES}" xml:space="preserve">{}</text>"#,
            if bold { r#" font-weight="bold""# } else { "" },
            escape(text)
        );
    };
    add(
        left,
        y,
        unit * 1.45,
        ink,
        &shorten(title, chars_in(room, unit * 1.45)),
        true,
    );
    y += pitch * 1.15;
    add(left, y, unit, soft, &shorten(subtitle, chars_in(room, unit)), false);
    y += pitch * 1.1;
    for ((label, _), pieces) in rows.iter().zip(&values) {
        let colour = if label == "Warning" { warn } else { ink };
        add(left, y, unit, soft, label, false);
        for piece in pieces {
            add(left + label_width, y, unit, colour, piece, false);
            y += pitch;
        }
        if pieces.is_empty() {
            y += pitch;
        }
    }
    svg::render_exact(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">{body}</svg>"#
    ))
}

/// Where the rows of a list drawn by [`render_list`] stand in a picture of `width` × `height` pixels: the pixel row where the first row
/// begins, the height of a row, and how many rows fit. The mouse uses it to find the row that was clicked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListShape {
    pub top: f32,
    pub pitch: f32,
    pub rows: usize,
}

/// The shape of a list in a picture of this size: rows as high as the window allows (a row is a twenty-fourth of the height, between 18
/// and 44 pixels), under a title and a line. A row is a whole number of terminal cells high, and the first row begins on a cell's edge:
/// then a row is a picture of its own that can be kept and put on the screen at any line (`cell_height` is the height of a cell).
pub fn list_shape(width: usize, height: usize, cell_height: usize) -> ListShape {
    let _ = width;
    let cell = cell_height.max(1) as f32;
    let wanted = (height as f32 / 24.0).clamp(18.0, 44.0);
    let pitch = (wanted / cell).round().max(1.0) * cell;
    let top = (pitch * 2.4 / cell).ceil() * cell;
    let rows = (((height as f32 - top - pitch * 0.4) / pitch).floor().max(1.0)) as usize;
    ListShape { top, pitch, rows }
}

/// The width in pixels of the bar at the right edge that shows where in the whole list the lines are: a whole number of cells, so that
/// it can be drawn alone beside the rows (`cell_width` is the width of a cell).
pub fn list_bar_width(width: usize, cell_width: usize) -> usize {
    let cell = cell_width.max(1);
    let left = width as f32 * 0.04;
    ((left * 0.6 / cell as f32).ceil() as usize).max(1) * cell
}

/// How many characters the longest right-hand text of `lines` has (at least 5, the width of "p.123"): the rows of a list are laid out
/// by it, so a row drawn alone must be told it.
pub fn list_widest(lines: &[ListLine]) -> usize {
    lines.iter().map(|l| l.right.chars().count()).max().unwrap_or(0).max(5)
}

/// The row (from 0) of a list at the pixel row `y` of the picture, if there is one there.
pub fn list_row_at(shape: ListShape, y: f32) -> Option<usize> {
    if y < shape.top {
        return None;
    }
    let row = ((y - shape.top) / shape.pitch) as usize;
    (row < shape.rows).then_some(row)
}

/// A line of a list: the text, how deep it is (it is moved to the right by that), and what stands at the right edge (a page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLine {
    pub depth: usize,
    pub text: String,
    pub right: String,
}

/// A list of lines, `width` × `height` pixels, transparent behind the text: a title and a line under it, the `lines` that are on the
/// screen (as many as [`list_shape`] says), the `marked` one on a bar of colour, and a bar at the right that shows where in the whole
/// list (`first` of `total`) these lines are.
#[allow(clippy::too_many_arguments)]
pub fn render_list(
    title: &str,
    subtitle: &str,
    lines: &[ListLine],
    marked: Option<usize>,
    first: usize,
    total: usize,
    width: usize,
    height: usize,
    cell: (usize, usize),
    background: [u8; 3],
) -> Result<RgbaImage, DecodeError> {
    render_list_part(
        title, subtitle, lines, marked, first, total, width, height, cell, background, 0, height,
    )
}

/// The rows of pixels `from..to` of the list that [`render_list`] draws, as a picture `width` pixels wide and `to - from` high: only what
/// is in those rows is set and drawn, so a strip costs a fraction of the whole list (the viewer draws only the strips that change when the
/// chosen line moves).
#[allow(clippy::too_many_arguments)]
pub fn render_list_part(
    title: &str,
    subtitle: &str,
    lines: &[ListLine],
    marked: Option<usize>,
    first: usize,
    total: usize,
    width: usize,
    height: usize,
    cell: (usize, usize),
    background: [u8; 3],
    from: usize,
    to: usize,
) -> Result<RgbaImage, DecodeError> {
    let widest = list_widest(lines);
    draw_list(
        title, subtitle, lines, marked, first, total, width, height, cell, background, from, to, 0, widest,
    )
}

/// One row of a list as a picture of its own, `width` minus the bar at the right wide and a row high: the same pixels that stand in
/// that place of the whole list, so it can be kept and put on the screen at any line. `widest` is that of the whole list
/// ([`list_widest`]).
#[allow(clippy::too_many_arguments)]
pub fn render_list_row(
    line: &ListLine,
    marked: bool,
    widest: usize,
    width: usize,
    height: usize,
    cell: (usize, usize),
    background: [u8; 3],
) -> Result<RgbaImage, DecodeError> {
    let shape = list_shape(width, height, cell.1);
    let (top, bottom) = (shape.top.round() as usize, (shape.top + shape.pitch).round() as usize);
    let cut = list_bar_width(width, cell.0);
    draw_list(
        "",
        "",
        std::slice::from_ref(line),
        marked.then_some(0),
        0,
        0,
        width,
        height,
        cell,
        background,
        top,
        bottom,
        cut,
        widest,
    )
}

/// The bar at the right edge of a list alone, [`list_bar_width`] wide and as high as the rows: where the `shown` lines that begin at
/// `first` are in the whole list of `total`.
#[allow(clippy::too_many_arguments)]
pub fn render_list_bar(
    first: usize,
    shown: usize,
    total: usize,
    width: usize,
    height: usize,
    cell: (usize, usize),
    background: [u8; 3],
) -> Result<RgbaImage, DecodeError> {
    let shape = list_shape(width, height, cell.1);
    let bar_width = list_bar_width(width, cell.0);
    let track = shape.pitch * shape.rows as f32;
    let [r, g, b] = background;
    let light = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 128;
    let soft = if light { "#606060" } else { "#a0a0a0" };
    let mut body = String::new();
    if total > shape.rows && total > 0 {
        let (at, thumb) = thumb_of(shape, first, shown, total);
        let (x, w) = thumb_x(width, bar_width);
        body = format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{w:.1}" height="{thumb:.1}" rx="{:.1}" fill="{soft}" fill-opacity="0.6"/>"#,
            x - (width - bar_width) as f32,
            at - shape.top,
            w / 2.0
        );
    }
    svg::render_exact(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{bar_width}" height="{}">{body}</svg>"#,
        track.round() as usize
    ))
}

/// Where the thumb of the bar stands (the pixel row where it begins) and how high it is.
fn thumb_of(shape: ListShape, first: usize, shown: usize, total: usize) -> (f32, f32) {
    let track = shape.pitch * shape.rows as f32;
    let thumb = (track * shown.min(shape.rows) as f32 / total as f32).max(shape.pitch * 0.6);
    let at = shape.top + (track - thumb) * first as f32 / (total - shape.rows).max(1) as f32;
    (at, thumb)
}

/// Where the thumb stands across the bar (the pixel column where it begins) and how wide it is: in the middle of the bar, which is
/// at the right edge of a picture `width` wide.
fn thumb_x(width: usize, bar_width: usize) -> (f32, f32) {
    let thumb = (width as f32 * 0.04 * 0.25).min(bar_width as f32 * 0.5);
    ((width - bar_width) as f32 + (bar_width as f32 - thumb) / 2.0, thumb)
}

/// The pixels `from..to` of a list; `cut` pixels at the right edge are left out (the bar, when it is drawn alone).
#[allow(clippy::too_many_arguments)]
fn draw_list(
    title: &str,
    subtitle: &str,
    lines: &[ListLine],
    marked: Option<usize>,
    first: usize,
    total: usize,
    width: usize,
    height: usize,
    cell: (usize, usize),
    background: [u8; 3],
    from: usize,
    to: usize,
    cut: usize,
    widest: usize,
) -> Result<RgbaImage, DecodeError> {
    let (width, height) = (width.clamp(80, 4096), height.clamp(60, 4096));
    let to = to.min(height);
    let from = from.min(to.saturating_sub(1));
    let (from_y, to_y) = (from as f32, to as f32);
    let [r, g, b] = background;
    let light = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 128;
    let (ink, soft, bar, bar_ink) = if light {
        ("#202020", "#606060", "#1a5fd0", "#ffffff")
    } else {
        ("#f0f0f0", "#a0a0a0", "#2f7bff", "#ffffff")
    };
    let shape = list_shape(width, height, cell.1);
    let size = shape.pitch * 0.62;
    let left = width as f32 * 0.04;
    // the column at the right is as wide as its longest text (a page: "p.123"; a setting: "for each book")
    let page_room = size * (0.6 * widest as f32 + 2.0);
    let text_room = width as f32 - 2.0 * left - page_room - size;
    let chars_in = |room: f32| (room / (size * 0.5)).max(4.0) as usize;
    let mut body = String::new();
    fn put(body: &mut String, x: f32, y: f32, size: f32, colour: &str, text: &str, bold: bool, end: bool) {
        *body += &format!(
            r#"<text x="{x:.1}" y="{y:.1}" font-size="{size:.1}" fill="{colour}"{}{} font-family="{FAMILIES}" xml:space="preserve">{}</text>"#,
            if bold { r#" font-weight="bold""# } else { "" },
            if end { r#" text-anchor="end""# } else { "" },
            escape(text)
        );
    }
    // the title and the line under it stand in the heading
    if from_y < shape.top {
        put(
            &mut body,
            left,
            shape.pitch * 1.0,
            size * 1.35,
            ink,
            &shorten(title, chars_in(text_room)),
            true,
            false,
        );
        put(
            &mut body,
            left,
            shape.pitch * 1.75,
            size * 0.9,
            soft,
            &shorten(subtitle, chars_in(width as f32 - 2.0 * left)),
            false,
            false,
        );
    }
    for (i, line) in lines.iter().take(shape.rows).enumerate() {
        let top = shape.top + i as f32 * shape.pitch;
        if top + shape.pitch <= from_y || top >= to_y {
            continue;
        }
        let baseline = top + shape.pitch * 0.72;
        let marked_here = marked == Some(i);
        if marked_here {
            body += &format!(
                r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="{:.1}" fill="{bar}"/>"#,
                left * 0.5,
                top + shape.pitch * 0.06,
                // it ends before the bar at the right, so that a row can be drawn without the bar
                width as f32 - list_bar_width(width, cell.0) as f32 - left * 0.5 - 2.0,
                shape.pitch * 0.88,
                shape.pitch * 0.18
            );
        }
        let indent = line.depth.min(6) as f32 * size * 1.5;
        let (text_colour, page_colour) = if marked_here { (bar_ink, bar_ink) } else { (ink, soft) };
        put(
            &mut body,
            left + indent,
            baseline,
            size,
            text_colour,
            &shorten(&line.text, chars_in(text_room - indent)),
            marked_here,
            false,
        );
        put(
            &mut body,
            width as f32 - left - size * 1.2,
            baseline,
            size,
            page_colour,
            &line.right,
            false,
            true,
        );
    }
    // where these lines are in the whole list: a thin bar at the right edge
    if total > shape.rows && total > 0 {
        let (at, thumb) = thumb_of(shape, first, lines.len(), total);
        let (x, w) = thumb_x(width, list_bar_width(width, cell.0));
        if at + thumb > from_y && at < to_y {
            body += &format!(
                r#"<rect x="{x:.1}" y="{at:.1}" width="{w:.1}" height="{thumb:.1}" rx="{:.1}" fill="{soft}" fill-opacity="0.6"/>"#,
                w / 2.0
            );
        }
    }
    let part = to - from;
    let shown_width = width.saturating_sub(cut).max(1);
    svg::render_exact(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{shown_width}" height="{part}" viewBox="0 {from} {shown_width} {part}">{body}</svg>"#
    ))
}
#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn the_families_of_a_card_are_the_ones_the_font_set_makes_of_the_old_list() {
        assert_eq!(FAMILIES, crate::fontset::expand("Segoe UI, Arial, sans-serif"));
    }

    #[test]
    fn an_iso_media_file_is_told_by_its_brand_a_picture_is_no_video() {
        assert_eq!(kind(b"\0\0\0\x18ftypheic\0\0\0\0"), "HEIF image (HEIC)");
        assert_eq!(kind(b"\0\0\0\x18ftypmif1\0\0\0\0"), "HEIF image (HEIC)");
        assert_eq!(kind(b"\0\0\0\x1cftypavif\0\0\0\0"), "AVIF image");
        assert_eq!(kind(b"\0\0\0\x18ftypcrx \0\0\0\0"), "Canon CR3 RAW photograph");
        assert_eq!(kind(b"\0\0\0\x14ftypqt  \0\0\0\0"), "QuickTime video (MOV)");
        assert_eq!(kind(b"\0\0\0\x18ftypisom\0\0\0\0"), "MP4 or MOV video");
        assert_eq!(kind(b"\0\0\0\x18ftyp"), "MP4 or MOV video");
    }

    #[test]
    fn the_kind_is_told_by_the_first_bytes() {
        assert_eq!(kind(b"%PDF-1.7 ..."), "PDF document");
        assert_eq!(kind(b"MZ\x90\x00"), "Windows program or library");
        assert_eq!(kind(b"\0\0\0\x18ftypmp42"), "MP4 or MOV video");
        assert_eq!(kind(b"\x01\x02\x03"), "Binary data");
        assert_eq!(kind(b""), "Binary data");
    }

    #[test]
    fn sizes_and_dates_are_written_for_people() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1023), "1023 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(date(UNIX_EPOCH), "1970-01-01 00:00 UTC");
        assert_eq!(
            date(UNIX_EPOCH + std::time::Duration::from_secs(1_793_364_060)),
            "2026-10-30 12:41 UTC"
        );
        assert_eq!(
            date(UNIX_EPOCH + std::time::Duration::from_secs(951_782_400)),
            "2000-02-29 00:00 UTC"
        );
    }

    #[test]
    fn a_long_name_keeps_its_end_and_markup_is_escaped() {
        assert_eq!(shorten("short.bin", 20), "short.bin");
        let cut = shorten("a-very-long-name-of-a-file-that-goes-on.tar.gz", 20);
        assert!(
            cut.chars().count() <= 20 && cut.ends_with(".tar.gz") && cut.contains('…'),
            "{cut}"
        );
    }

    #[test]
    fn a_list_has_as_many_rows_as_the_window_allows_and_the_row_clicked_is_found() {
        let shape = list_shape(1000, 580, 20);
        assert!((12..=26).contains(&shape.rows), "{shape:?}");
        assert_eq!(shape.pitch % 20.0, 0.0, "a row is whole cells high");
        assert_eq!(shape.top % 20.0, 0.0, "the first row begins on a cell's edge");
        assert_eq!(list_row_at(shape, shape.top - 1.0), None, "the title is no row");
        assert_eq!(list_row_at(shape, shape.top + shape.pitch * 0.5), Some(0));
        assert_eq!(list_row_at(shape, shape.top + shape.pitch * 2.5), Some(2));
        assert_eq!(list_row_at(shape, 579.0), None, "below the last row");
    }

    #[test]
    fn the_marked_row_of_a_list_is_on_a_bar_and_the_others_are_not() {
        let lines: Vec<ListLine> = ["Preface", "Chapter I", "Chapter II"]
            .iter()
            .enumerate()
            .map(|(i, t)| ListLine {
                depth: usize::from(i == 2),
                text: (*t).to_string(),
                right: format!("p.{}", 3 + i),
            })
            .collect();
        let image = render_list(
            "Contents",
            "2 of 3",
            &lines,
            Some(1),
            0,
            3,
            1000,
            580,
            (10, 20),
            [0, 0, 0],
        )
        .expect("a list");
        assert_eq!((image.width(), image.height()), (1000, 580));
        let shape = list_shape(1000, 580, 20);
        let y_of = |row: usize| (shape.top + shape.pitch * (row as f32 + 0.5)) as u32;
        // far from any text, at the right of the middle: the bar of the marked row is opaque and blue, the other rows are clear
        let x = 800;
        let marked = image.get_pixel(x, y_of(1)).0;
        assert!(marked[3] > 200 && marked[2] > marked[0], "{marked:?}");
        assert_eq!(image.get_pixel(x, y_of(0)).0[3], 0);
        assert_eq!(image.get_pixel(x, y_of(2)).0[3], 0);
    }

    #[test]
    fn a_strip_of_the_list_is_the_same_rows_as_in_the_whole_list() {
        let lines: Vec<ListLine> = (0..30)
            .map(|i| ListLine {
                depth: i % 3,
                text: format!("Глава {i}. Натуральные числа"),
                right: format!("p.{i}"),
            })
            .collect();
        let (width, height) = (1000, 580);
        let shape = list_shape(width, height, 20);
        let whole = render_list(
            "Contents",
            "5 of 30",
            &lines,
            Some(4),
            0,
            30,
            width,
            height,
            (10, 20),
            [12, 12, 12],
        )
        .expect("a list");
        // the heading and a strip with the marked line (the fourth)
        for (from, to) in [(0usize, 60usize), (140, 200), (200, 260)] {
            let part = render_list_part(
                "Contents",
                "5 of 30",
                &lines,
                Some(4),
                0,
                30,
                width,
                height,
                (10, 20),
                [12, 12, 12],
                from,
                to,
            )
            .expect("a strip");
            assert_eq!((part.width() as usize, part.height() as usize), (width, to - from));
            let same = (0..part.height())
                .flat_map(|y| (0..part.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| part.get_pixel(x, y) == whole.get_pixel(x, y + from as u32))
                .count();
            let all = (part.width() * part.height()) as usize;
            assert!(
                same * 1000 >= all * 995,
                "rows {from}..{to}: {same} of {all} pixels equal; shape {shape:?}"
            );
        }
    }

    #[test]
    fn a_row_and_the_bar_drawn_alone_are_what_stands_in_their_place_of_the_whole_list() {
        let lines: Vec<ListLine> = (0..40)
            .map(|i| ListLine {
                depth: i % 3,
                text: format!("Глава {i}. Натуральные числа"),
                right: format!("p.{i}"),
            })
            .collect();
        let (width, height, cell) = (1000, 580, (10, 20));
        let shape = list_shape(width, height, cell.1);
        let shown = &lines[5..5 + shape.rows];
        let whole = render_list(
            "Contents",
            "7 of 40",
            shown,
            Some(2),
            5,
            40,
            width,
            height,
            cell,
            [12, 12, 12],
        )
        .expect("a list");
        let widest = list_widest(shown);
        let bar = list_bar_width(width, cell.0);
        assert_eq!(bar % cell.0, 0, "the bar is whole cells wide");
        let close = |a: &RgbaImage, ax: u32, ay: u32, w: u32, h: u32| {
            let same = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .filter(|&(x, y)| a.get_pixel(x, y) == whole.get_pixel(x + ax, y + ay))
                .count();
            let first_difference = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .find(|&(x, y)| a.get_pixel(x, y) != whole.get_pixel(x + ax, y + ay))
                .map(|(x, y)| (x, y, a.get_pixel(x, y).0, whole.get_pixel(x + ax, y + ay).0));
            assert!(
                same * 1000 >= (w * h) as usize * 995,
                "{same} of {} pixels equal at {ax},{ay}; the first difference {first_difference:?}",
                w * h
            );
        };
        for (i, marked) in [(0usize, false), (2, true), (7, false)] {
            let row = render_list_row(&shown[i], marked, widest, width, height, cell, [12, 12, 12]).expect("a row");
            assert_eq!(
                (row.width() as usize, row.height() as usize),
                (width - bar, shape.pitch as usize)
            );
            close(
                &row,
                0,
                (shape.top + shape.pitch * i as f32) as u32,
                row.width(),
                row.height(),
            );
        }
        let strip = render_list_bar(5, shown.len(), 40, width, height, cell, [12, 12, 12]).expect("a bar");
        assert_eq!(strip.width() as usize, bar);
        close(
            &strip,
            (width - bar) as u32,
            shape.top as u32,
            strip.width(),
            strip.height(),
        );
    }

    #[test]
    fn a_card_is_drawn_with_the_size_asked() {
        let path = std::env::temp_dir().join(format!("peekel-card-{}.bin", std::process::id()));
        std::fs::write(&path, b"\x00\x01binary").expect("writes");
        let card = render(&path, 400, 300, [0, 0, 128]);
        let _ = std::fs::remove_file(&path);
        let card = card.expect("a card");
        assert_eq!((card.width(), card.height()), (400, 300));
    }
}
