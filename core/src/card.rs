//! A card for a file that is no picture: its kind (told by the first bytes, not the name), its name, size and date,
//! drawn as an SVG with the installed fonts. The Far plugin shows it in the quick view in place of the file's bytes.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use image::RgbaImage;

use crate::decode::DecodeError;
use crate::svg;
use crate::xml::escape;

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
            r#"<text x="{x:.1}" y="{y:.1}" font-size="{size:.1}" fill="{colour}"{} font-family="Segoe UI, Arial, sans-serif" xml:space="preserve">{}</text>"#,
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

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

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
    fn a_card_is_drawn_with_the_size_asked() {
        let path = std::env::temp_dir().join(format!("peekel-card-{}.bin", std::process::id()));
        std::fs::write(&path, b"\x00\x01binary").expect("writes");
        let card = render(&path, 400, 300, [0, 0, 128]);
        let _ = std::fs::remove_file(&path);
        let card = card.expect("a card");
        assert_eq!((card.width(), card.height()), (400, 300));
    }
}
