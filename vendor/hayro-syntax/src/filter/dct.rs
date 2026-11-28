use crate::object::Dict;
use crate::object::dict::keys::COLOR_TRANSFORM;
use crate::object::stream::{FilterResult, ImageColorSpace, ImageData, ImageDecodeParams};
use alloc::borrow::Cow;
use core::num::NonZeroU32;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::colorspace::ColorSpace::CMYK;
use core::sync::atomic::{AtomicU8, Ordering};
use zune_jpeg::zune_core::options::{DecoderOptions, JpegScale};

pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    if image_params.width > u16::MAX as u32 || image_params.height > u16::MAX as u32 {
        return None;
    }

    // Some PDFs have weird JPEGs where the JPEG metadata is completely wrong
    // (for example indicating that one of the dimensions is u16::MAX), but the
    // metadata in the PDF image dictionary is correct. Therefore, we first
    // validate the JPEG metadata and patch the data if any of the dimensions
    // are too large (if they are too small, they will just be padded later on).
    let data = maybe_patch_jpeg_dimensions(data, image_params)?;

    let options = DecoderOptions::default()
        .set_max_width(u16::MAX as usize)
        .set_max_height(u16::MAX as usize);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&*data), options);
    decoder.decode_headers().ok()?;

    let color_transform = params.get::<u8>(COLOR_TRANSFORM);
    let input_color_space = decoder.input_colorspace().unwrap();

    let mut out_colorspace = if let Some(num_components) = image_params.num_components
        && !matches!(num_components, 1 | 3 | 4)
    {
        ColorSpace::MultiBand(NonZeroU32::new(num_components as u32)?)
    } else {
        match input_color_space {
            ColorSpace::YCbCr => {
                if color_transform.is_none_or(|c| c == 1) {
                    ColorSpace::RGB
                } else {
                    ColorSpace::YCbCr
                }
            }
            ColorSpace::RGB | ColorSpace::RGBA => ColorSpace::RGB,
            ColorSpace::Luma | ColorSpace::LumaA => ColorSpace::Luma,
            // TODO: Find test case with color transform on cmyk
            CMYK => CMYK,
            ColorSpace::YCCK => ColorSpace::YCCK,
            _ => ColorSpace::RGB,
        }
    };

    // In case image had APP14 marker, we might have to override the colorspace.
    if input_color_space == CMYK && decoder.info().unwrap().components == 3 {
        out_colorspace = ColorSpace::RGB;
    }

    decoder.set_options(DecoderOptions::default().jpeg_set_out_colorspace(out_colorspace));
    // PEEKEL: a picture drawn smaller than it is coded is decoded at 1/8, 1/4 or 1/2 of its size, the least that still covers
    // the size it is drawn at (the reduced IDCT, as libjpeg's scale_num in MuPDF): far less work than decoding it whole and
    // shrinking it after. The interpreter scales a smaller result back by its size against the image's own
    // (`scale_factors`), as it does for JPEG 2000. Anything that fails falls back to the whole picture.
    let scaled = reduced_scale(&decoder, image_params).and_then(|scale| {
        let (w, h) = decoder.scaled_dimensions(scale)?;
        let mut out = alloc::vec![0_u8; decoder.scaled_output_buffer_size(scale)?];
        decoder.decode_scaled_into(scale, &mut out).ok()?;
        Some((out, u32::try_from(w).ok()?, u32::try_from(h).ok()?))
    });
    let (mut decoded, width, height) = match scaled {
        Some(s) => s,
        None => {
            let whole = decoder.decode().ok()?;
            let (w, h) = decoder.dimensions()?;
            (whole, w as u32, h as u32)
        }
    };

    if out_colorspace == ColorSpace::YCCK {
        // See <https://github.com/mozilla/pdf.js/blob/69595a29192b7704733404a42a2ebb537601117b/src/core/jpg.js#L1331>
        for c in decoded.chunks_mut(4) {
            let y = c[0] as f32;
            let cb = c[1] as f32;
            let cr = c[2] as f32;
            c[0] = (434.456 - y - 1.402 * cr) as u8;
            c[1] = (119.541 - y + 0.344 * cb + 0.714 * cr) as u8;
            c[2] = (481.816 - y - 1.772 * cb) as u8;
        }
    }

    let image_data = ImageData {
        alpha: None,
        color_space: match out_colorspace {
            ColorSpace::RGB | ColorSpace::YCbCr => Some(ImageColorSpace::Rgb),
            ColorSpace::Luma => Some(ImageColorSpace::Gray),
            ColorSpace::YCCK | CMYK => Some(ImageColorSpace::Cmyk),
            ColorSpace::MultiBand(_) => None,
            _ => None,
        },
        bits_per_component: 8,
        width,
        height,
    };

    Some(FilterResult {
        data: Cow::Owned(decoded),
        image_data: Some(image_data),
    })
}

/// PEEKEL: how far below the size it is drawn at a reduced picture may fall, in per cent (0: never, the default). Above 0 a
/// page is a little softer and much faster: a magazine's JPEG of 2172 pixels drawn at 1200 is decoded at half (1086 pixels)
/// with 10, whole with 0.
static REDUCED_JPEG_SLACK: AtomicU8 = AtomicU8::new(0);

/// PEEKEL: sets [`REDUCED_JPEG_SLACK`] for every picture decoded after (0 to 50 per cent).
pub fn set_reduced_jpeg_slack(percent: u8) {
    REDUCED_JPEG_SLACK.store(percent.min(50), Ordering::Relaxed);
}

/// PEEKEL: the least of 1/8, 1/4 and 1/2 whose size covers the size the picture is drawn at (less the slack), or none. The
/// other ratios (3/8, 5/8 ...) are left out: zune-jpeg's IDCT for them is slower than the whole picture (measured on a PC and
/// a 32-bit ARM tablet, 27 November 2026).
fn reduced_scale<T: zune_jpeg::zune_core::bytestream::ZByteReaderTrait>(
    decoder: &zune_jpeg::JpegDecoder<T>,
    image_params: &ImageDecodeParams,
) -> Option<JpegScale> {
    let (target_w, target_h) = image_params.target_dimension?;
    let keep = 100 - u64::from(REDUCED_JPEG_SLACK.load(Ordering::Relaxed));
    [JpegScale::Eighth, JpegScale::Quarter, JpegScale::Half]
        .into_iter()
        .find(|&scale| {
            decoder.scaled_dimensions(scale).is_some_and(|(w, h)| {
                w as u64 * 100 >= u64::from(target_w) * keep && h as u64 * 100 >= u64::from(target_h) * keep
            })
        })
}

fn maybe_patch_jpeg_dimensions<'a>(
    data: &'a [u8],
    image_params: &ImageDecodeParams,
) -> Option<Cow<'a, [u8]>> {
    let sof_offset = find_sof_marker(data)?;

    let height_offset = sof_offset.checked_add(5)?;
    let width_offset = sof_offset.checked_add(7)?;

    let jpeg_height = u16::from_be_bytes([
        *data.get(height_offset)?,
        *data.get(height_offset.checked_add(1)?)?,
    ]);
    let jpeg_width = u16::from_be_bytes([
        *data.get(width_offset)?,
        *data.get(width_offset.checked_add(1)?)?,
    ]);

    let jpeg_area = (jpeg_width as usize).checked_mul(jpeg_height as usize)?;
    let image_area = (image_params.width as usize).checked_mul(image_params.height as usize)?;
    let need_patch = jpeg_area > image_area;

    if !need_patch {
        return Some(Cow::Borrowed(data));
    }

    let target_w = (image_params.width as u16).to_be_bytes();
    let target_h = (image_params.height as u16).to_be_bytes();

    let mut patched = data.to_vec();
    patched[height_offset..height_offset.checked_add(2)?].copy_from_slice(&target_h);
    patched[width_offset..width_offset.checked_add(2)?].copy_from_slice(&target_w);

    Some(Cow::Owned(patched))
}

fn find_sof_marker(data: &[u8]) -> Option<usize> {
    let mut i = 0_usize;

    while i.checked_add(1).is_some_and(|next| next < data.len()) {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }

        let marker = data[i + 1];

        // Note: Not sure if 100% correct/robust, is AI-generated.
        match marker {
            // All SOF markers carry dimensions: SOF0–SOF15, excluding
            // 0xC4 (DHT), 0xC8 (JPG), 0xCC (DAC) which are not frame markers.
            0xC0..=0xCF if marker != 0xC4 && marker != 0xC8 && marker != 0xCC => {
                return Some(i);
            }
            // Skip padding bytes (0xFF followed by 0xFF).
            0xFF => {
                i += 1;

                continue;
            }
            // SOI (0xD8), EOI (0xD9), TEM (0x01) and stuffed byte (0x00)
            // are standalone markers with no payload.
            0xD8 | 0xD9 | 0x01 | 0x00 => {
                i += 2;

                continue;
            }
            // All other markers have a 2-byte length field — skip over them.
            _ => {
                let len_start = i.checked_add(2)?;
                let len_end = i.checked_add(3)?;
                let seg_len =
                    u16::from_be_bytes([*data.get(len_start)?, *data.get(len_end)?]) as usize;

                i = i.checked_add(2)?.checked_add(seg_len)?;
            }
        }
    }

    None
}
