//! Peekel core: decode, orient, scale, quantize and encode pictures as Sixel, and the model of what to show of one.

#![forbid(unsafe_code)]

pub mod archive;
pub mod card;
pub mod color;
pub mod dds;
pub mod decode;
pub mod djvu;
pub mod docx;
pub mod exif;
pub mod font;
pub mod fontset;
mod guard;
pub mod icc;
pub mod info;
pub mod jxl;
mod office;
pub mod page;
pub mod pdf;
pub mod pe;
pub mod photo;
pub mod pptx;
pub mod quantize;
pub mod raw;
pub mod sixel;
pub mod svg;
pub mod text;
pub mod view;
pub mod xlsx;
pub mod xml;
