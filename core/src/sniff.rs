//! The format of a file that has no extension, told by its first bytes: a book given by Android as `/proc/self/fd/N` (Peekel TV,
//! #235 item 6), or a file saved without one. A file with an extension is never looked into: it is what its extension says, as before.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// A format told by the first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sniffed {
    Pdf,
    Djvu,
    Fb2,
    /// An FB2 in a ZIP (`.fb2.zip`, `.fbz`): the first file in it is a `.fb2`.
    Fb2Zip,
    Epub,
    /// A Word document (`.docx`): a ZIP whose `[Content_Types].xml` names Word's main part, or one of whose first files is
    /// `word/document.xml`.
    Docx,
}

/// How many bytes are looked at: an FB2 may begin with a long `<?xml ...?>` and comments before `<FictionBook`.
const HEAD: usize = 1024;

/// The file last asked about, by its path, size and time of change: the viewer asks on every frame.
type Key = (PathBuf, u64, Option<SystemTime>);
static LAST: Mutex<Option<(Key, Option<Sniffed>)>> = Mutex::new(None);

/// The format of `path` told by its first bytes, when it has no extension; `None` for a file with an extension, one that cannot be
/// read, or one that is none of these.
pub fn sniffed(path: &Path) -> Option<Sniffed> {
    if path.extension().is_some() {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if let Ok(last) = LAST.lock()
        && let Some((k, found)) = last.as_ref()
        && *k == key
    {
        return *found;
    }
    let mut head = Vec::with_capacity(HEAD);
    let found = std::fs::File::open(path)
        .ok()
        .and_then(|f| f.take(HEAD as u64).read_to_end(&mut head).ok())
        .and_then(|_| of(&head));
    if let Ok(mut last) = LAST.lock() {
        *last = Some((key, found));
    }
    found
}

/// The format of a file that begins with `head`.
pub fn of(head: &[u8]) -> Option<Sniffed> {
    if head.starts_with(b"%PDF-") {
        return Some(Sniffed::Pdf);
    }
    if head.starts_with(b"AT&TFORM") {
        return Some(Sniffed::Djvu);
    }
    if head.starts_with(b"PK\x03\x04") {
        return zip(head);
    }
    // XML: perhaps a byte order mark, then `<?xml ...?>`, comments and the root
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}');
    (text.trim_start().starts_with('<') && text.contains("<FictionBook")).then_some(Sniffed::Fb2)
}

/// A ZIP by its first file: an EPUB begins with `mimetype` holding `application/epub+zip`, stored; a zipped FB2 with a `.fb2`.
fn zip(head: &[u8]) -> Option<Sniffed> {
    let u16_at = |i: usize| {
        head.get(i..i + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
    };
    let (name_len, extra_len) = (u16_at(26)?, u16_at(28)?);
    let name = head.get(30..30 + name_len)?;
    if name == b"mimetype" {
        let start = 30 + name_len + extra_len;
        let content = head.get(start..)?;
        return content.starts_with(b"application/epub+zip").then_some(Sniffed::Epub);
    }
    if name.to_ascii_lowercase().ends_with(b".fb2") {
        return Some(Sniffed::Fb2Zip);
    }
    word(head).then_some(Sniffed::Docx)
}

/// Whether the files of a ZIP that begin in `head` say Word: `[Content_Types].xml` (Word writes it first) naming the main part of a
/// Word document, unpacked as far as `head` has it, or a file `word/document.xml` (a ZIP made by another program). A macro-enabled
/// document or a template has another type of main part and is not taken.
fn word(head: &[u8]) -> bool {
    const MAIN: &[u8] = b"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
    let u16_at = |i: usize| {
        head.get(i..i + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
    };
    let u32_at = |i: usize| {
        head.get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let mut at = 0;
    while head.get(at..at + 4) == Some(b"PK\x03\x04") {
        let (Some(flags), Some(method), Some(packed), Some(name_len), Some(extra_len)) = (
            u16_at(at + 6),
            u16_at(at + 8),
            u32_at(at + 18),
            u16_at(at + 26),
            u16_at(at + 28),
        ) else {
            return false;
        };
        let Some(name) = head.get(at + 30..at + 30 + name_len) else {
            return false;
        };
        let start = at + 30 + name_len + extra_len;
        let data = head.get(start..).unwrap_or_default();
        let data = if packed > 0 {
            &data[..data.len().min(packed)]
        } else {
            data
        };
        if name == b"word/document.xml" {
            return true;
        }
        if name == b"[Content_Types].xml" {
            let text = match method {
                0 => data.to_vec(),
                8 => inflate_part(data),
                _ => Vec::new(),
            };
            return text.windows(MAIN.len()).any(|w| w == MAIN);
        }
        // the next file, when this one's size is known (not written after its data)
        if flags & 8 != 0 {
            return false;
        }
        let Some(next) = start.checked_add(packed) else {
            return false;
        };
        at = next;
    }
    false
}

/// As much of deflated `data` as can be unpacked, when it is cut short (at most 64 KB of it).
fn inflate_part(data: &[u8]) -> Vec<u8> {
    let mut decoder = flate2::Decompress::new(false);
    let mut out = Vec::with_capacity(64 * 1024);
    let _ = decoder.decompress_vec(data, &mut out, flate2::FlushDecompress::None);
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn zip_with(name: &str, content: &[u8]) -> Vec<u8> {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0; 22]); // version, flags, method, time, date, crc, sizes
        bytes.extend_from_slice(&u16::try_from(name.len()).expect("short").to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(content);
        bytes
    }

    #[test]
    fn the_first_bytes_tell_the_format() {
        assert_eq!(of(b"%PDF-1.7\n..."), Some(Sniffed::Pdf));
        assert_eq!(of(b"AT&TFORM\0\0\0\x10DJVU"), Some(Sniffed::Djvu));
        assert_eq!(
            of("\u{feff}<?xml version=\"1.0\"?>\n<!-- a comment -->\n<FictionBook xmlns=\"x\">".as_bytes()),
            Some(Sniffed::Fb2)
        );
        assert_eq!(of(&zip_with("mimetype", b"application/epub+zip")), Some(Sniffed::Epub));
        assert_eq!(of(&zip_with("Book.FB2", b"<?xml")), Some(Sniffed::Fb2Zip));
        // what is none of them
        assert_eq!(of(&zip_with("word/document.xml", b"")), Some(Sniffed::Docx));
        assert_eq!(of(&zip_with("xl/workbook.xml", b"")), None, "another ZIP");
        assert_eq!(of(&zip_with("mimetype", b"application/vnd.oasis")), None);
        assert_eq!(of(b"<html><body>FictionBook</body></html>"), None);
        assert_eq!(of(b"\x89PNG\r\n"), None, "a picture is told by its own decoder");
        assert_eq!(of(b""), None);
        assert_eq!(of(b"PK\x03\x04\0\0"), None, "a ZIP cut short");
    }

    #[test]
    fn a_file_with_an_extension_is_never_looked_into_and_one_without_is() {
        let dir = std::env::temp_dir().join(format!("peekel-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let named = dir.join("book.txt");
        let bare = dir.join("book");
        std::fs::write(&named, b"%PDF-1.4").expect("writes");
        std::fs::write(&bare, b"%PDF-1.4").expect("writes");
        let found = (sniffed(&named), sniffed(&bare), sniffed(&dir));
        // the same path with other bytes is looked at again
        std::fs::write(&bare, b"AT&TFORM and more").expect("writes");
        let again = sniffed(&bare);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, (None, Some(Sniffed::Pdf), None));
        assert_eq!(again, Some(Sniffed::Djvu));
    }

    #[test]
    fn a_word_document_is_told_by_its_content_types_or_its_first_files_and_other_office_files_are_not() {
        let head = |folder: &str, name: &str| {
            let bytes = std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .join(folder)
                    .join(name),
            )
            .expect("a test file");
            bytes[..bytes.len().min(HEAD)].to_vec()
        };
        // made by Word: `[Content_Types].xml` first, deflated
        assert_eq!(of(&head("docx", "word-made.docx")), Some(Sniffed::Docx));
        // made by a script: `word/document.xml` first
        assert_eq!(of(&head("docx", "sample.docx")), Some(Sniffed::Docx));
        assert_eq!(of(&head("xlsx", "sample.xlsx")), None);
        assert_eq!(of(&head("pptx", "sample.pptx")), None);
        assert_eq!(of(&head("odt", "sample.odt")), None);
        // a macro-enabled document's main part is another type
        let macros = br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.ms-word.document.macroEnabled.main+xml"/></Types>"#;
        assert_eq!(of(&zip_with("[Content_Types].xml", macros)), None);
        // rubbish where the sizes are
        let mut spoilt = head("docx", "word-made.docx");
        spoilt[18..22].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(of(&spoilt), Some(Sniffed::Docx), "the start is unpacked all the same");
        spoilt.truncate(60);
        assert_eq!(of(&spoilt), None);
    }

    #[test]
    fn a_word_document_with_no_extension_is_read_as_one_and_named_docx_as_well() {
        let dir = std::env::temp_dir().join(format!("peekel-sniff-docx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("docx")
            .join("word-made.docx");
        let bare = dir.join("17");
        let named = dir.join("17.docx");
        std::fs::copy(&source, &bare).expect("copies");
        std::fs::copy(&source, &named).expect("copies");
        for path in [&bare, &named] {
            assert!(crate::docx::is_docx(path), "{path:?}");
            assert!(crate::decode::text_flows(path), "{path:?}");
            assert_eq!(crate::decode::page_count(path).expect("lays out"), Some(3), "{path:?}");
            assert_eq!(
                crate::title::of(path).title.as_deref(),
                Some("A test document of Peekel"),
                "{path:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
