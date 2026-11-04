//! DDS (DirectDraw Surface, the texture files of games and tools): the first image, read by `image_dds` (MIT; the block formats BC1 to BC7 are
//! unpacked by `bcdec_rs`, written in safe Rust) on `ddsfile`'s reader.
//!
//! What the `image` crate's own DDS reader opens is DXT1, 3 and 5 with the old header and nothing else; this reads those and BC4, BC5,
//! BC6H (an HDR texture is clipped to 8 bits), BC7, files with the newer DX10 header, and the uncompressed 24 and 32 bit ones. The old uncompressed kinds that
//! `image_dds` leaves out (16-bit colours, luminance, a palette) are read by `legacy` below. Measured on the 14 ordinary DDS files of
//! Pillow's tests: all 14 are read, against 3 by the `image` crate's own reader. Not read (an error, the file gets its card): a volume
//! texture and the kinds that are not known. The first mipmap of the first layer is what is drawn.
//!
//! A file that may be made to hurt is judged before memory is made: the file is read with a size limit, the pixels the header claims are
//! counted before anything is unpacked, and the work runs on a thread of its own on which a panic is an error (`guard`).

use std::path::Path;

use image::RgbaImage;
use image_dds::ddsfile::Dds;

use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};

/// A file bigger than this is not read (it is held in memory whole).
const MAX_FILE: u64 = 512 << 20;

/// More pixels than this are refused: 100 megapixels is 400 MB.
const MAX_PIXELS: u64 = 100_000_000;

/// Whether the name says DDS.
pub fn is_dds(path: &Path) -> bool {
    has_extension(path, "dds")
}

/// The first image of the DDS file at `path`.
pub fn render(path: &Path) -> Result<RgbaImage, DecodeError> {
    let bytes = read_limited(path, MAX_FILE)?;
    draw_on_thread("dds", "the DDS reader failed on this file", || render_bytes(&bytes))
}

/// The same for the bytes of a file.
pub fn render_bytes(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let dds = Dds::read(bytes).map_err(unreadable)?;
    if u64::from(dds.get_width()) * u64::from(dds.get_height()) > MAX_PIXELS {
        return Err(DecodeError::TooLarge);
    }
    // `image_dds` makes black of an 8-bit luminance file (seen in a test), so these are read here first
    if dds
        .header
        .spf
        .flags
        .contains(image_dds::ddsfile::PixelFormatFlags::LUMINANCE)
        && let Some(image) = legacy(&dds)?
    {
        return Ok(image);
    }
    match image_dds::image_from_dds(&dds, 0) {
        Ok(image) => Ok(image),
        // the old uncompressed kinds that `image_dds` leaves out: read by their bit masks (`legacy`)
        Err(first) => legacy(&dds)?.ok_or_else(|| unreadable(first)),
    }
}

/// The value of the bits `mask` picks out of `pixel`, made into a number from 0 to 255; `fallback` when the file has no such mask.
fn channel(pixel: u32, mask: Option<u32>, fallback: u8) -> u8 {
    match mask {
        Some(mask) if mask != 0 => {
            let shift = mask.trailing_zeros();
            let top = mask >> shift;
            let value = (pixel & mask) >> shift;
            // the value over its largest (a 5-bit 31 is 255), rounded
            ((u64::from(value) * 255 + u64::from(top) / 2) / u64::from(top)) as u8
        }
        _ => fallback,
    }
}

/// The old uncompressed DDS files that name their pixels by bit masks (`X1R5G5B5`, `R5G6B5`, `A4R4G4B4`, 8 and 16 bit luminance, 24 and 32 bit
/// colours in any order) or by a palette of 256 colours stored before the indexes: the first image as RGBA. `None` when the file is
/// none of those (a block format, or a kind that is not known), so that the error `image_dds` gave is the one that is told.
fn legacy(dds: &Dds) -> Result<Option<RgbaImage>, DecodeError> {
    use image_dds::ddsfile::PixelFormatFlags as Flags;
    let format = &dds.header.spf;
    if format.flags.contains(Flags::FOURCC) || dds.header10.is_some() {
        return Ok(None);
    }
    let (width, height) = (dds.get_width() as usize, dds.get_height() as usize);
    // A palette of 256 colours before 8-bit indexes (the flag 0x20, which `ddsfile` has no name for and drops, with the bit count, which it
    // keeps only for colour and luminance): told by no flag, no code and no bit count, and by the data being the palette and the indexes.
    let indexed = format.flags.bits() & 0x20 != 0
        || (format.flags.is_empty()
            && format.fourcc.is_none()
            && format.rgb_bit_count.is_none()
            && Some(dds.data.len()) == width.checked_mul(height).and_then(|n| n.checked_add(1024)));
    let bits = if indexed {
        Some(8)
    } else {
        format.rgb_bit_count.map(|b| b as usize)
    };
    let Some(bits) = bits.filter(|b| matches!(b, 8 | 16 | 24 | 32)) else {
        return Ok(None);
    };
    let (per_pixel, palette_len) = (bits / 8, if indexed { 1024 } else { 0 });
    let pitch = width.checked_mul(per_pixel).ok_or(DecodeError::TooLarge)?;
    let needed = pitch
        .checked_mul(height)
        .and_then(|n| n.checked_add(palette_len))
        .ok_or(DecodeError::TooLarge)?;
    if dds.data.len() < needed {
        return Err(unreadable("a DDS file cut short"));
    }
    let (palette, pixels) = dds.data.split_at(palette_len);
    let luminance = format.flags.contains(Flags::LUMINANCE);
    let colour = format.flags.contains(Flags::RGB);
    if palette_len == 0 && !luminance && !colour {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(width * height * 4);
    for row in pixels.chunks_exact(pitch.max(1)).take(height) {
        for raw in row.chunks_exact(per_pixel) {
            let value = raw.iter().rev().fold(0u32, |v, &b| v << 8 | u32::from(b));
            if palette_len > 0 {
                let entry = &palette[usize::from(raw[0]) * 4..][..4];
                out.extend([entry[0], entry[1], entry[2], entry[3]]);
            } else if luminance {
                // `ddsfile` gives no masks for these: a pixel is the luminance alone, or the luminance and the alpha in a byte each
                let with_alpha = format.flags.contains(Flags::ALPHA_PIXELS) && bits == 16;
                let grey_mask = format
                    .r_bit_mask
                    .or(Some(if with_alpha { 0x00FF } else { u32::MAX >> (32 - bits) }));
                let alpha_mask = format.a_bit_mask.or(with_alpha.then_some(0xFF00));
                let grey = channel(value, grey_mask, 0);
                out.extend([grey, grey, grey, channel(value, alpha_mask, 255)]);
            } else {
                out.extend([
                    channel(value, format.r_bit_mask, 0),
                    channel(value, format.g_bit_mask, 0),
                    channel(value, format.b_bit_mask, 0),
                    channel(value, format.a_bit_mask, 255),
                ]);
            }
        }
    }
    Ok(RgbaImage::from_raw(width as u32, height as u32, out))
}
