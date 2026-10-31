//! What a file says about itself, as rows of text for people: its kind (by the first bytes, not by its name), size, dates and
//! attributes, a warning when the name and the content do not agree, and what the kind has: the camera and exposure of a
//! photograph (EXIF), the pages and the author of a PDF, the title, author and counts of a Word, PowerPoint or Excel file.
//!
//! One module for the three that need it: the card the Far plugin draws for a file it cannot show, the properties the viewer
//! shows when `i` is pressed, and whatever comes next. It reads at most the first 128 KB of a file, and the whole of a ZIP or
//! PDF only when it is up to 64 MB; every part is read with the limits of the module that reads it. It never fails: what
//! cannot be read is a row less.

use std::path::Path;
use std::time::SystemTime;

use crate::archive::Archive;
use crate::card;
use crate::exif;
use crate::xml::{descendant, parse};

/// How much of the start of a file is read for the kind and the EXIF.
const HEAD: usize = 128 * 1024;
/// A ZIP or PDF larger than this is not opened for what is in it.
const MAX_WHOLE: u64 = 64 * 1024 * 1024;
const MAX_PART: usize = 8 * 1024 * 1024;

/// A file's properties: what kind of file it is and the rows (label, value) to show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub kind: String,
    pub rows: Vec<(String, String)>,
}

/// 1234567 as "1 234 567".
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn push(rows: &mut Vec<(String, String)>, label: &str, value: impl Into<String>) {
    let value = value.into();
    if !value.trim().is_empty() {
        rows.push((label.to_string(), value));
    }
}

/// The date of a file system time as "2026-10-30 12:41 UTC".
fn when(time: SystemTime) -> String {
    card::date(time)
}

/// What the kind says the extension should be, when the extension is one whose content is easy to tell: (extension, the start
/// of the kind that goes with it).
const EXPECTED: &[(&str, &str)] = &[
    ("jpg", "JPEG"),
    ("jpeg", "JPEG"),
    ("png", "PNG"),
    ("gif", "GIF"),
    ("webp", "WebP"),
    ("pdf", "PDF"),
    ("zip", "ZIP"),
    ("7z", "7-Zip"),
    ("rar", "RAR"),
    ("gz", "gzip"),
    ("exe", "Windows program"),
    ("dll", "Windows program"),
    ("mp3", "MP3"),
    ("flac", "FLAC"),
];

/// Whether the extension of `path` is one of `EXPECTED` and what the content says does not agree with it.
fn mismatch(path: &Path, kind: &str) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let (_, expected) = EXPECTED.iter().find(|(e, _)| *e == extension)?;
    if kind.starts_with(expected) {
        return None;
    }
    // the documents of Office and the like are ZIPs: their kind begins with ZIP when the archive was not opened
    if kind.starts_with("ZIP") && matches!(extension.as_str(), "zip") {
        return None;
    }
    Some(if kind == "Binary data" {
        format!("named .{extension}, but its content is not that kind of file (damaged, or another format)")
    } else {
        format!("named .{extension}, but it looks like: {kind}")
    })
}

/// The kind of file, with a ZIP told apart by what is in it (a Word, PowerPoint or Excel file, an OpenDocument one, an app).
fn kind_of(head: &[u8], archive: Option<&Archive<'_>>) -> String {
    let kind = card::kind(head);
    let Some(archive) = archive.filter(|_| kind == "ZIP archive") else {
        return kind.to_string();
    };
    let has = |name: &str| archive.contains(name);
    let count = archive.names().count();
    if has("word/document.xml") {
        "Word document".to_string()
    } else if has("ppt/presentation.xml") {
        "PowerPoint presentation".to_string()
    } else if has("xl/workbook.xml") {
        "Excel workbook".to_string()
    } else if has("META-INF/manifest.xml") {
        "OpenDocument file".to_string()
    } else if has("AndroidManifest.xml") {
        "Android package".to_string()
    } else if has("META-INF/MANIFEST.MF") {
        format!("Java archive ({count} entries)")
    } else {
        format!("ZIP archive ({count} entries)")
    }
}

/// The properties of the file at `path`.
pub fn of(path: &Path) -> Info {
    let mut info = Info::default();
    let Ok(meta) = std::fs::metadata(path) else {
        info.kind = "File that cannot be read".to_string();
        return info;
    };
    if meta.is_dir() {
        info.kind = "Folder".to_string();
        if let Ok(modified) = meta.modified() {
            push(&mut info.rows, "Modified", when(modified));
        }
        return info;
    }
    let mut head = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        use std::io::Read;
        let _ = file.take(HEAD as u64).read_to_end(&mut head);
    }
    let whole = (meta.len() <= MAX_WHOLE).then(|| std::fs::read(path).ok()).flatten();
    let archive = whole
        .as_deref()
        .filter(|_| head.starts_with(b"PK"))
        .and_then(|b| Archive::new(b).ok());
    info.kind = kind_of(&head, archive.as_ref());

    push(
        &mut info.rows,
        "Size",
        if meta.len() < 1024 {
            format!("{} bytes", meta.len())
        } else {
            format!("{} ({} bytes)", card::human_size(meta.len()), grouped(meta.len()))
        },
    );
    if let Ok(modified) = meta.modified() {
        push(&mut info.rows, "Modified", when(modified));
    }
    if let Ok(created) = meta.created() {
        push(&mut info.rows, "Created", when(created));
    }
    push(&mut info.rows, "Attributes", attributes(&meta));
    if let Some(note) = mismatch(path, &info.kind) {
        push(&mut info.rows, "Warning", note);
    }

    // what the kind has
    if info.kind.contains("image") || info.kind == "SVG drawing" {
        picture(path, &head, &mut info.rows);
    } else if info.kind == "PDF document" {
        if let Some(bytes) = whole {
            info.rows.extend(crate::pdf::properties(bytes));
        }
    } else if info.kind.ends_with("font") || info.kind == "Font collection" {
        if let Some(bytes) = &whole {
            info.rows.extend(crate::font::properties(bytes));
        }
    } else if info.kind == "Windows program or library" {
        if let Some(bytes) = &whole {
            program(bytes, &mut info.rows);
        }
    } else if info.kind == "gzip archive" {
        if let Some(bytes) = &whole {
            gzip(bytes, &mut info.rows);
        }
    } else if let Some(archive) = &archive {
        if matches!(
            info.kind.as_str(),
            "Word document" | "PowerPoint presentation" | "Excel workbook"
        ) {
            office(archive, &info.kind, &mut info.rows);
        } else {
            contents(archive, &mut info.rows);
        }
    }
    info
}

/// The Windows attributes that are worth a word.
fn attributes(meta: &std::fs::Metadata) -> String {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let a = meta.file_attributes();
        let names: Vec<&str> = [
            (0x1, "read-only"),
            (0x2, "hidden"),
            (0x4, "system"),
            (0x400, "link"),
            (0x800, "compressed"),
            (0x4000, "encrypted"),
        ]
        .iter()
        .filter(|(bit, _)| a & bit != 0)
        .map(|(_, name)| *name)
        .collect();
        names.join(", ")
    }
    #[cfg(not(windows))]
    {
        if meta.permissions().readonly() {
            "read-only".to_string()
        } else {
            String::new()
        }
    }
}

/// The size of a picture and, if it has them, the EXIF rows.
fn picture(path: &Path, head: &[u8], rows: &mut Vec<(String, String)>) {
    if let Ok(reader) = image::ImageReader::open(path).and_then(|r| r.with_guessed_format())
        && let Ok((w, h)) = reader.into_dimensions()
    {
        let megapixels = f64::from(w) * f64::from(h) / 1e6;
        push(rows, "Dimensions", format!("{w} x {h} pixels ({megapixels:.1} MP)"));
    }
    let Some(exif) = exif::read(head) else {
        return;
    };
    let camera = match (&exif.make, &exif.model) {
        // the model often begins with the make: "NIKON CORPORATION" / "NIKON D700"
        (Some(make), Some(model))
            if model
                .to_lowercase()
                .starts_with(&make.split(' ').next().unwrap_or("").to_lowercase()) =>
        {
            Some(model.clone())
        }
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (Some(one), None) | (None, Some(one)) => Some(one.clone()),
        (None, None) => None,
    };
    push(rows, "Camera", camera.unwrap_or_default());
    push(rows, "Lens", exif.lens.clone().unwrap_or_default());
    push(rows, "Taken", exif.taken.clone().unwrap_or_default());
    let settings: Vec<String> = [
        exif.exposure.clone(),
        exif.aperture.clone(),
        exif.iso.as_ref().map(|i| format!("ISO {i}")),
        exif.focal.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    push(rows, "Exposure", settings.join("  ·  "));
    push(rows, "Exposure bias", exif.bias.clone().unwrap_or_default());
    push(rows, "Flash", exif.flash.clone().unwrap_or_default());
    push(rows, "Software", exif.software.clone().unwrap_or_default());
    if let Some(o) = exif.orientation.filter(|&o| o != 1) {
        let name = match o {
            2 => "mirrored",
            3 => "turned 180°",
            4 => "mirrored upside down",
            5 => "mirrored, turned 90° counter-clockwise",
            6 => "turned 90° clockwise",
            7 => "mirrored, turned 90° clockwise",
            _ => "turned 90° counter-clockwise",
        };
        push(rows, "Orientation", name);
    }
    if let Some((lat, lon)) = exif.gps {
        push(rows, "GPS", format!("{lat:.5}, {lon:.5}"));
    }
}

/// What a Windows program or library says: bits, kind, build time, a certificate, and its version resource.
fn program(bytes: &[u8], rows: &mut Vec<(String, String)>) {
    let Some(pe) = crate::pe::parse(bytes) else {
        return;
    };
    let what = if pe.is_dll { "library (DLL)" } else { "program" };
    let kind = [
        pe.machine.clone(),
        if pe.dotnet { ".NET".to_string() } else { String::new() },
        pe.subsystem.clone(),
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect::<Vec<_>>()
    .join(", ");
    push(rows, "Type", format!("{what} ({kind})"));
    if let Some(built) = pe.built {
        push(
            rows,
            "Built",
            card::date(std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(built))),
        );
    }
    if pe.signed {
        push(rows, "Signed", "a certificate is attached (not verified here)");
    }
    let value = |name: &str| {
        pe.strings
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    push(rows, "Product", value("ProductName"));
    push(rows, "Description", value("FileDescription"));
    push(rows, "Version", value("FileVersion"));
    push(rows, "Company", value("CompanyName"));
    push(rows, "Copyright", value("LegalCopyright"));
    push(rows, "Original name", value("OriginalFilename"));
}

/// What a gzip file says: the name it was made from and how big it unpacks to (the last four bytes, modulo 4 GB).
fn gzip(bytes: &[u8], rows: &mut Vec<(String, String)>) {
    if bytes.len() < 18 {
        return;
    }
    let flags = bytes[3];
    let mut at = 10usize;
    if flags & 0x04 != 0 {
        let extra = usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
        at = at.saturating_add(2 + extra);
    }
    if flags & 0x08 != 0
        && let Some(end) = bytes.get(at..).and_then(|rest| rest.iter().position(|&b| b == 0))
    {
        let name: String = bytes[at..at + end]
            .iter()
            .map(|&b| char::from(b))
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        push(rows, "Made from", name);
    }
    let size = u32::from_le_bytes([
        bytes[bytes.len() - 4],
        bytes[bytes.len() - 3],
        bytes[bytes.len() - 2],
        bytes[bytes.len() - 1],
    ]);
    push(
        rows,
        "Unpacks to",
        format!("{} (modulo 4 GB)", card::human_size(u64::from(size))),
    );
}

/// What is in a ZIP that is no Office file: how many files, how big, and the first names.
fn contents(archive: &Archive<'_>, rows: &mut Vec<(String, String)>) {
    let files: Vec<(&str, u64, u64)> = archive.entries().filter(|(name, _, _)| !name.ends_with('/')).collect();
    let packed: u64 = files.iter().map(|e| e.1).sum();
    let unpacked: u64 = files.iter().map(|e| e.2).sum();
    push(
        rows,
        "Contents",
        format!(
            "{} files, {} ({} packed)",
            files.len(),
            card::human_size(unpacked),
            card::human_size(packed)
        ),
    );
    let shown: Vec<String> = files
        .iter()
        .take(8)
        .map(|(name, _, _)| {
            let name = name.rsplit('/').next().unwrap_or(name);
            name.chars().take(40).collect::<String>()
        })
        .collect();
    let more = if files.len() > 8 {
        format!(", and {} more", files.len() - 8)
    } else {
        String::new()
    };
    push(rows, "First files", format!("{}{more}", shown.join(", ")));
}

/// An ISO date of Office ("2024-08-12T15:38:00Z") as "2024-08-12 15:38".
fn iso(text: &str) -> String {
    text.get(..16).map_or_else(|| text.to_string(), |s| s.replace('T', " "))
}

/// The properties of a Word, PowerPoint or Excel file: `docProps/core.xml`, `docProps/app.xml`, and the sheets of a workbook.
fn office(archive: &Archive<'_>, kind: &str, rows: &mut Vec<(String, String)>) {
    let read = |name: &str| {
        archive
            .read(name, MAX_PART)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let text_of = |doc: &roxmltree::Document<'_>, name: &str| {
        descendant(doc.root_element(), name)
            .and_then(|n| n.text())
            .map(|t| t.trim().chars().take(300).collect::<String>())
            .unwrap_or_default()
    };
    if let Some(text) = read("docProps/core.xml")
        && let Ok(core) = parse(&text)
    {
        push(rows, "Title", text_of(&core, "title"));
        push(rows, "Subject", text_of(&core, "subject"));
        push(rows, "Author", text_of(&core, "creator"));
        push(rows, "Last saved by", text_of(&core, "lastModifiedBy"));
        push(rows, "Keywords", text_of(&core, "keywords"));
        push(rows, "Created", iso(&text_of(&core, "created")));
        push(rows, "Modified", iso(&text_of(&core, "modified")));
    }
    if let Some(text) = read("docProps/app.xml")
        && let Ok(app) = parse(&text)
    {
        push(rows, "Made with", text_of(&app, "Application"));
        push(rows, "Company", text_of(&app, "Company"));
        let count = |label: &str, name: &str, rows: &mut Vec<(String, String)>| {
            let n = text_of(&app, name);
            if n.parse::<u64>().is_ok_and(|n| n > 0) {
                push(rows, label, n);
            }
        };
        match kind {
            "Word document" => {
                count("Pages", "Pages", rows);
                count("Words", "Words", rows);
            }
            "PowerPoint presentation" => count("Slides", "Slides", rows),
            _ => {}
        }
    }
    if kind == "Excel workbook"
        && let Some(text) = read("xl/workbook.xml")
        && let Ok(book) = parse(&text)
    {
        let names: Vec<String> = book
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "sheet")
            .filter_map(|n| n.attribute("name").map(str::to_string))
            .collect();
        let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
        let more = if names.len() > 8 {
            format!(", and {} more", names.len() - 8)
        } else {
            String::new()
        };
        push(rows, "Sheets", format!("{}: {}{more}", names.len(), shown.join(", ")));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_file(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join(path)
    }

    fn row<'a>(info: &'a Info, label: &str) -> Option<&'a str> {
        info.rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.as_str())
    }

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1 000");
        assert_eq!(grouped(1_234_567), "1 234 567");
    }

    #[test]
    fn a_picture_says_its_kind_size_and_dimensions() {
        let info = of(&test_file("images/growth.png"));
        assert_eq!(info.kind, "PNG image");
        assert!(row(&info, "Size").is_some_and(|s| s.contains("bytes")), "{info:?}");
        assert_eq!(row(&info, "Dimensions"), Some("800 x 420 pixels (0.3 MP)"));
        assert!(row(&info, "Modified").is_some());
        assert_eq!(row(&info, "Warning"), None);
    }

    #[test]
    fn the_exif_of_a_photograph_is_shown() {
        // the test photos of the orientations carry only that tag
        let info = of(&test_file("images/exif-6.jpg"));
        assert_eq!(info.kind, "JPEG image");
        assert_eq!(row(&info, "Orientation"), Some("turned 90° clockwise"), "{info:?}");
        assert_eq!(
            row(&of(&test_file("images/exif-1.jpg")), "Orientation"),
            None,
            "normal needs no word"
        );
    }

    #[test]
    fn a_pdf_says_its_pages_and_paper() {
        let info = of(&test_file("pdf/three-pages.pdf"));
        assert_eq!(info.kind, "PDF document");
        assert_eq!(row(&info, "Pages"), Some("3"));
        assert!(
            row(&info, "Page size").is_some_and(|s| s.starts_with("71 x 35 mm")),
            "{info:?}"
        );
        assert_eq!(row(&info, "PDF version"), Some("1.4"));
    }

    #[test]
    fn office_files_are_told_apart_and_say_what_they_hold() {
        assert_eq!(of(&test_file("docx/sample.docx")).kind, "Word document");
        assert_eq!(of(&test_file("pptx/sample.pptx")).kind, "PowerPoint presentation");
        let book = of(&test_file("xlsx/sample.xlsx"));
        assert_eq!(book.kind, "Excel workbook");
        assert_eq!(row(&book, "Sheets"), Some("1: Budget"), "{book:?}");
    }

    #[test]
    fn a_name_that_does_not_fit_the_content_is_warned_of() {
        let dir = std::env::temp_dir().join(format!("peekel-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("makes a folder");
        let png = std::fs::read(test_file("images/growth.png")).expect("reads");
        let liar = dir.join("holiday.jpg");
        std::fs::write(&liar, &png).expect("writes");
        let noise = dir.join("noise.png");
        std::fs::write(&noise, [1u8, 2, 3, 4, 5, 6, 7, 8]).expect("writes");
        let honest = dir.join("fine.png");
        std::fs::write(&honest, &png).expect("writes");
        let (a, b, c) = (of(&liar), of(&noise), of(&honest));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            row(&a, "Warning").is_some_and(|w| w.contains(".jpg") && w.contains("PNG")),
            "{a:?}"
        );
        assert!(row(&b, "Warning").is_some_and(|w| w.contains("not that kind")), "{b:?}");
        assert_eq!(row(&c, "Warning"), None);
    }

    /// A file of `bytes` in a folder of this test's own, to look at with `of`.
    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peekel-info-tests-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("makes a folder");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("writes");
        path
    }

    #[test]
    fn a_zip_says_how_many_files_it_holds_and_the_first_names() {
        let data = crate::archive::tests::zip(&[
            ("src/one.py", b"print(1)\n".repeat(50).as_slice(), true),
            ("src/two.py", b"print(2)\n".repeat(50).as_slice(), true),
            ("docs/", b"", false),
        ]);
        let path = scratch("project.zip", &data);
        let info = of(&path);
        assert_eq!(info.kind, "ZIP archive (3 entries)");
        let contents = row(&info, "Contents").expect("contents");
        assert!(contents.starts_with("2 files, "), "{contents}");
        assert_eq!(row(&info, "First files"), Some("one.py, two.py"));
    }

    #[test]
    fn a_gzip_says_the_name_it_was_made_from_and_what_it_unpacks_to() {
        use std::io::Write;
        let mut encoder = flate2::GzBuilder::new()
            .filename("backup.sql")
            .write(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&vec![b'x'; 100_000]).expect("writes");
        let data = encoder.finish().expect("finishes");
        let info = of(&scratch("dump.gz", &data));
        assert_eq!(info.kind, "gzip archive");
        assert_eq!(row(&info, "Made from"), Some("backup.sql"));
        assert!(
            row(&info, "Unpacks to").is_some_and(|u| u.starts_with("97.7 KB")),
            "{info:?}"
        );
    }

    #[test]
    fn a_program_and_a_font_of_this_machine_are_described_when_there_is_one() {
        if let Some(path) = [r"C:\Windows\System32\notepad.exe"]
            .iter()
            .find(|p| Path::new(p).is_file())
        {
            let info = of(Path::new(path));
            assert_eq!(info.kind, "Windows program or library");
            assert!(
                row(&info, "Type").is_some_and(|t| t.starts_with("program (")),
                "{info:?}"
            );
            assert!(
                row(&info, "Company").is_some_and(|c| c.contains("Microsoft")),
                "{info:?}"
            );
        }
        if let Some(path) = [
            r"C:\Windows\Fonts\arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
        .iter()
        .find(|p| Path::new(p).is_file())
        {
            let info = of(Path::new(path));
            assert!(info.kind.ends_with("font"), "{info:?}");
            assert!(row(&info, "Family").is_some(), "{info:?}");
            assert!(row(&info, "Glyphs").is_some(), "{info:?}");
        }
    }

    #[test]
    fn what_is_missing_or_is_a_folder_is_not_a_failure() {
        assert_eq!(of(Path::new("no such file at all")).kind, "File that cannot be read");
        assert_eq!(of(&test_file("images")).kind, "Folder");
    }
}
