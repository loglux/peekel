//! DjVu, read from the public specification (DjVu v3, <https://www.sndjvu.org/spec.html>).
//!
//! What is here: the IFF container ([`iff`]), the page information ([`info`]) and the structure of a document, single page or
//! bundled ([`document`]). The pictures (JB2, IW44) come in later steps; see docs/internal/djvu.md for the order of work.
//!
//! The rules of this crate, as of the other readers of files that may be made to hurt: safe code only, no panic on any input
//! (every length is checked before it is used), and limits on how deep and how big what is read can be. Nothing is copied from
//! the input: what is read borrows its bytes.

#![forbid(unsafe_code)]

pub mod bzz;
pub mod document;
pub mod iff;
pub mod info;
pub mod iw44;
pub mod jb2;
pub mod page;
pub mod zp;
mod zp_table;

use std::fmt;

/// Why a file could not be read as DjVu. `at` is the offset in the file (or in the part being read) where it was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Does not begin with `AT&T` followed by a `FORM`, or is a `FORM` of a kind that is not a document.
    NotDjvu,
    /// A chunk says it is longer than the bytes that are there.
    Truncated { at: usize },
    /// A `FORM` too short to hold its kind, or a length that does not fit in memory arithmetic.
    BadLength { at: usize },
    /// Chunks nested deeper than DjVu ever nests them.
    TooDeep,
    /// More chunks than a real document has.
    TooMany,
    /// A page without its `INFO` chunk.
    NoInfo,
    /// An `INFO` chunk too short to hold the size of the page.
    BadInfo,
    /// Asked for a page the document does not have.
    NoSuchPage { index: usize, pages: usize },
    /// A document whose components are separate files (a directory with the flag "indirect"): there is only one file here.
    Indirect,
    /// A `DIRM` chunk too short for the number of components it declares.
    BadDirectory,
    /// BZZ data that cannot be decoded: cut short, not a valid block, or more than the limit.
    BadBzz(&'static str),
    /// JB2 data that cannot be decoded: cut short, a record that is not allowed, or past a limit.
    BadJb2(&'static str),
    /// IW44 data that cannot be decoded: a first chunk that is missing or too short, chunks out of order, or past a limit.
    BadIw44(&'static str),
    /// A page whose parts do not fit together: a mask that is not the size of the page.
    BadPage(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDjvu => write!(f, "not a DjVu file"),
            Self::Truncated { at } => write!(
                f,
                "the file is cut short (a chunk at byte {at} is longer than what is left)"
            ),
            Self::BadLength { at } => write!(f, "an impossible length at byte {at}"),
            Self::TooDeep => write!(f, "chunks nested too deep"),
            Self::TooMany => write!(f, "too many chunks"),
            Self::NoInfo => write!(f, "a page without an INFO chunk"),
            Self::BadInfo => write!(f, "an INFO chunk too short to hold the size of the page"),
            Self::NoSuchPage { index, pages } => write!(f, "page {index} asked for, the document has {pages}"),
            Self::Indirect => write!(f, "an indirect document: its components are separate files"),
            Self::BadDirectory => write!(f, "a document directory too short for its components"),
            Self::BadBzz(why) => write!(f, "BZZ data that cannot be decoded: {why}"),
            Self::BadJb2(why) => write!(f, "JB2 data that cannot be decoded: {why}"),
            Self::BadIw44(why) => write!(f, "IW44 data that cannot be decoded: {why}"),
            Self::BadPage(why) => write!(f, "a page that does not fit together: {why}"),
        }
    }
}

impl std::error::Error for Error {}
