//! JPEG XL (`.jxl`): the first frame as a picture, read by `jxl-oxide`, a decoder written in Rust (MIT or Apache 2.0), so it is the same on
//! Windows and Linux and needs no codec of the system (the plugin's WIC could not open it on the author's PC).
//!
//! A file that may be made to hurt is judged before memory is made: the file is read with a size limit, the pixels the header
//! claims are counted before anything is drawn, the decoder is given a limit on what it may allocate, and it runs on a thread of its own
//! on which a panic is an error (`guard`). The colours are asked of the decoder in sRGB (an ICC profile inside is turned by `moxcms`,
//! which `core` uses already). Not read: the other frames of an animation, a CMYK image, the high dynamic range of a PQ or HLG
//! image (it is clipped to what sRGB shows, not tone mapped).

use std::path::Path;

use image::RgbaImage;
use jxl_oxide::{AllocTracker, EnumColourEncoding, JxlImage, Moxcms, RenderingIntent};

use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};

/// A file bigger than this is not read (it is held in memory whole).
const MAX_FILE: u64 = 256 << 20;

/// More pixels than this are refused: the decoder works in four floats a pixel, 50 megapixels is 800 MB.
const MAX_PIXELS: u64 = 50_000_000;

/// What the decoder may allocate at most.
const MAX_ALLOC: usize = 2 << 30;

/// Whether the name says JPEG XL.
pub fn is_jxl(path: &Path) -> bool {
    has_extension(path, "jxl")
}

/// The first frame of the JPEG XL file at `path`, in sRGB, turned upright.
pub fn render(path: &Path) -> Result<RgbaImage, DecodeError> {
    let bytes = read_limited(path, MAX_FILE)?;
    draw_on_thread("jxl", "the JPEG XL decoder failed on this file", || {
        render_bytes(&bytes)
    })
}

/// The same for the bytes of a file.
pub fn render_bytes(bytes: &[u8]) -> Result<RgbaImage, DecodeError> {
    let mut image = JxlImage::builder()
        .alloc_tracker(AllocTracker::with_limit(MAX_ALLOC))
        .read(bytes)
        .map_err(unreadable)?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(unreadable("a JPEG XL image with no pixels"));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(DecodeError::TooLarge);
    }
    if image.pixel_format().has_black() {
        return Err(unreadable("a CMYK JPEG XL image is not read"));
    }
    image.set_cms(Moxcms);
    image.request_color_encoding(EnumColourEncoding::srgb(RenderingIntent::Relative));
    if image.num_loaded_keyframes() == 0 {
        return Err(unreadable("a JPEG XL file with no complete frame (cut short)"));
    }
    let render = image.render_frame(0).map_err(unreadable)?;
    let mut stream = render.stream();
    let channels = stream.channels() as usize;
    let (w, h) = (stream.width() as usize, stream.height() as usize);
    let mut samples = vec![0u8; w * h * channels];
    stream.write_to_buffer::<u8>(&mut samples);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for pixel in samples.chunks_exact(channels.max(1)) {
        match *pixel {
            [g] => rgba.extend([g, g, g, 255]),
            [g, a] => rgba.extend([g, g, g, a]),
            [r, g, b] => rgba.extend([r, g, b, 255]),
            [r, g, b, a, ..] => rgba.extend([r, g, b, a]),
            [] => {}
        }
    }
    let picture = RgbaImage::from_raw(w as u32, h as u32, rgba)
        .ok_or_else(|| unreadable("a JPEG XL image of an unexpected shape"))?;
    // the turn the file asks for (1 to 8, as EXIF numbers it)
    let mut picture = image::DynamicImage::ImageRgba8(picture);
    if let Some(turn) = u8::try_from(render.orientation())
        .ok()
        .and_then(image::metadata::Orientation::from_exif)
    {
        picture.apply_orientation(turn);
    }
    Ok(picture.into_rgba8())
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot read its own fixture should stop at once
mod tests {
    use super::*;

    /// The 42-byte image of the decoder's own documentation: a small picture, made for an example.
    const SMALL: [u8; 42] = [
        0xff, 0x0a, 0x30, 0x54, 0x10, 0x09, 0x08, 0x06, 0x01, 0x00, 0x78, 0x00, 0x4b, 0x38, 0x41, 0x3c, 0xb6, 0x3a,
        0x51, 0xfe, 0x00, 0x47, 0x1e, 0xa0, 0x85, 0xb8, 0x27, 0x1a, 0x48, 0x45, 0x84, 0x1b, 0x71, 0x4f, 0xa8, 0x3e,
        0x8e, 0x30, 0x03, 0x92, 0x84, 0x01,
    ];

    #[test]
    fn the_name_says_jpeg_xl() {
        assert!(is_jxl(Path::new("a/b/photo.JXL")));
        assert!(!is_jxl(Path::new("photo.png")));
    }

    #[test]
    fn a_small_picture_is_drawn() {
        let picture = render_bytes(&SMALL).expect("the example of the decoder reads");
        assert_eq!(picture.dimensions(), (240, 135));
    }

    #[test]
    fn a_file_cut_short_or_spoilt_is_an_error_and_never_a_panic() {
        for cut in 0..SMALL.len() {
            let _ = render_bytes(&SMALL[..cut]);
        }
        let mut spoilt = SMALL;
        for at in 0..spoilt.len() {
            let was = spoilt[at];
            spoilt[at] = !was;
            let _ = render_bytes(&spoilt);
            spoilt[at] = was;
        }
        assert!(render_bytes(&[]).is_err());
        assert!(render_bytes(b"not a picture at all, just text").is_err());
    }

    #[test]
    fn a_header_that_claims_too_many_pixels_is_refused_before_anything_is_drawn() {
        // a codestream (FF 0A) whose size header says 1 073 741 823 x 1 073 741 823 would be refused at once
        let started = std::time::Instant::now();
        let mut huge = vec![0xff, 0x0a];
        huge.extend([0xff; 12]);
        let _ = render_bytes(&huge);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
