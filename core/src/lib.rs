//! Peekel core: decode, orient, scale, quantize and encode pictures as Sixel, and the model of what to show of one.

#![forbid(unsafe_code)]

pub mod archive;
pub mod book;
pub mod card;
pub mod color;
pub mod column;
pub mod content;
mod css;
pub mod dds;
pub mod decode;
pub mod djvu;
pub mod docx;
pub mod epub;
pub mod exif;
pub mod fb2;
pub mod find;
mod flow;
pub mod font;
pub mod fontset;
mod guard;
pub mod html;
mod hyphen;
pub mod icc;
pub mod info;
pub mod jxl;
mod measure;
pub mod night;
pub mod odp;
pub mod ods;
pub mod odt;
mod office;
pub mod page;
pub mod pdf;
mod pdf_meta;
mod pdf_outline;
pub mod pdf_text;
pub mod pe;
pub mod photo;
pub mod pptx;
pub mod quantize;
pub mod raw;
pub mod reading;
pub mod resize;
pub mod sha;
pub mod sixel;
pub mod sniff;
pub mod spread;
pub mod svg;
pub mod text;
pub mod title;
pub mod torrent;
pub mod view;
pub mod xlsx;
pub mod xml;

/// How much longer a test's limit of time is on a CI runner (`CI` is set there, on GitHub's machines and on our own runner on the LAN
/// server): many tests run at once on a few shared cores, and a limit that holds on a quiet computer is too short there (an outline of
/// 22 000 items took 7 s here and over 30 s there, 14 November 2026). A real hang is still caught: it never ends. As
/// `tests/hostile.rs` does.
#[cfg(test)]
pub(crate) fn slack() -> u32 {
    if std::env::var_os("CI").is_some() { 4 } else { 1 }
}
