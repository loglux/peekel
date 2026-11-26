//! A book's title and author alone, reading no more of the file than they need (Peekel TV, #235, the second round, item 4): for a list
//! of books, where `info::of` would read a PDF or an EPUB whole, and only up to 64 MB, so that a larger book showed its file's name.
//!
//! - **PDF**: the information dictionary found from the end of the file (`pdf_meta`), a few small reads whatever the size; the whole
//!   file (up to the reader's 256 MB) only when that cannot be done (an encrypted file, a damaged cross-reference).
//! - **EPUB**: `container.xml` and the package read where they lie in the ZIP (`archive::OnDisk`).
//! - **FB2**: the description at the start of the file, or of the `.fb2` in a ZIP.
//! - **Word (docx)**: `docProps/core.xml` read where it lies in the ZIP (`dc:title`, `dc:creator`).
//! - **DjVu**: none. Its metadata (`print-meta` of `djvused`) was in 1 of 224 books of the author's (a title, no author; 19 November
//!   2026), so it is not read; the caller shows the file's name.

use std::path::Path;

/// A book's title and author (several authors joined by ", "); either may be missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Title {
    pub title: Option<String>,
    pub author: Option<String>,
}

/// The title and author of the book at `path`; both `None` for a file that has none, cannot be read, or is not a PDF, EPUB,
/// FB2 or Word document.
pub fn of(path: &Path) -> Title {
    let (title, author) = if crate::pdf::is_pdf(path) {
        crate::pdf_meta::title_and_author(path).unwrap_or_else(|| whole_pdf(path))
    } else if crate::fb2::is_fb2(path) {
        crate::fb2::title_and_author(path)
    } else if crate::epub::is_epub(path) {
        crate::epub::title_and_author(path).unwrap_or_default()
    } else if crate::docx::is_docx(path) {
        crate::docx::title_and_author(path)
    } else {
        (None, None)
    };
    let clean = |text: Option<String>| text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    Title {
        title: clean(title),
        author: clean(author),
    }
}

/// From the whole PDF, as `info::of` reads it.
fn whole_pdf(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(data) = crate::guard::read_limited(path, crate::pdf::MAX_BYTES) else {
        return (None, None);
    };
    let rows = crate::pdf::properties(data);
    let value = |label: &str| rows.iter().find(|(l, _)| l == label).map(|(_, v)| v.clone());
    (value("Title"), value("Author"))
}
