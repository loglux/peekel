//! Peekel core: decode, orient, scale, quantize and encode pictures as Sixel, and the model of what to show of one.

#![forbid(unsafe_code)]

pub mod archive;
pub mod book;
pub mod card;
pub mod color;
pub mod dds;
pub mod decode;
pub mod djvu;
pub mod docx;
pub mod epub;
pub mod exif;
pub mod fb2;
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
pub mod odp;
pub mod ods;
pub mod odt;
mod office;
pub mod page;
pub mod pdf;
mod pdf_outline;
pub mod pdf_text;
pub mod pe;
pub mod photo;
pub mod pptx;
pub mod quantize;
pub mod raw;
pub mod resize;
pub mod sixel;
pub mod svg;
pub mod text;
pub mod view;
pub mod xlsx;
pub mod xml;
