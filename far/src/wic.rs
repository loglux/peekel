//! Windows' own image codecs (WIC, Windows Imaging Component): HEIC, AVIF, JPEG XL and whatever else is installed on the
//! machine. Nothing of a third party is shipped: Windows does the decoding, and only where the codec is there (the Store
//! extensions HEIF, AV1, Raw Image, JPEG XL are optional; another PC may not have them). So WIC is an addition with a
//! fall-back (a card), never a base: a file that `core` reads is read by `core`. See docs/internal/formats.md.
//!
//! `codecs()` is the list the machine has, asked once of WIC's own enumerator in a thread of its own (COM has to be
//! started in the thread that uses it, and Far's thread is not ours to start it in). `supports(extension)` says whether
//! a decoder of the list opens files of that extension. `decode(path)` opens a file with whichever decoder fits.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use image::RgbaImage;
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmapDecoder, IWICBitmapDecoderInfo,
    IWICBitmapFrameDecode, IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICComponentEnumerateDefault,
    WICDecodeMetadataCacheOnDemand, WICDecoder,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{Interface, PCWSTR};

/// More pixels than this are refused before they are read (as in `core`).
const MAX_PIXELS: u64 = 100_000_000;

/// One decoder of the machine: its name and the extensions of the files it opens (lower case, with the dot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codec {
    pub name: String,
    pub extensions: Vec<String>,
}

static CODECS: OnceLock<Vec<Codec>> = OnceLock::new();

/// Starts reading the list of the machine's decoders, in a thread of its own; `codecs()` has it when that is done (a
/// moment). Called once, when the plugin starts.
pub fn start() {
    std::thread::spawn(|| {
        let list = read_codecs().unwrap_or_default();
        crate::log::line(&format!("WIC: {} decoders on this machine", list.len()));
        let _ = CODECS.set(list);
    });
}

/// The decoders of the machine, as soon as they have been read (None before that).
pub fn codecs() -> Option<&'static [Codec]> {
    CODECS.get().map(Vec::as_slice)
}

/// Whether a decoder of this machine opens files with this extension (with or without the dot, any case).
pub fn supports(extension: &str) -> bool {
    let wanted = format!(".{}", extension.trim_start_matches('.').to_ascii_lowercase());
    codecs().is_some_and(|all| all.iter().any(|c| c.extensions.contains(&wanted)))
}

/// The extensions of files that a listed decoder could not start on (see `is_component_failure`), lower case, no dot.
static FAILED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether `why` (the message of a failed `decode`) says the decoder itself did not start, `WINCODEC_ERR_COMPONENTINITIALIZEFAILURE`
/// (0x88982F8B): the case of a codec that is listed but whose Store extension is not really there, not that of a damaged file.
pub fn is_component_failure(why: &str) -> bool {
    why.contains("0x88982F8B")
}

/// Remembers that a decoder of files with this extension is listed but did not start.
pub fn mark_failed(extension: &str) {
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if let Ok(mut failed) = FAILED.lock()
        && !failed.contains(&extension)
    {
        failed.push(extension);
    }
}

/// Whether a decoder of files with this extension was seen not to start.
pub fn failed(extension: &str) -> bool {
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    FAILED.lock().is_ok_and(|failed| failed.contains(&extension))
}

/// The WIC factory of this thread (COM started in it first); None when COM or WIC is not there.
fn factory() -> Option<IWICImagingFactory> {
    // SAFETY: COM is started once for this thread and never stopped (the threads live as long as the process).
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
    }
}

/// Asks WIC's enumerator for every decoder, the packaged ones (the Store extensions) too.
fn read_codecs() -> Option<Vec<Codec>> {
    let factory = factory()?;
    // SAFETY: calls of the COM objects of WIC with buffers of the sizes they are given.
    unsafe {
        let enumerator = factory
            .CreateComponentEnumerator(WICDecoder.0 as u32, WICComponentEnumerateDefault.0 as u32)
            .ok()?;
        let mut codecs = Vec::new();
        loop {
            let mut item = [None];
            let mut fetched = 0u32;
            if enumerator.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(info) = item[0].take().and_then(|u| u.cast::<IWICBitmapDecoderInfo>().ok()) else {
                continue;
            };
            let mut name = vec![0u16; 256];
            let mut actual = 0u32;
            let _ = info.GetFriendlyName(&mut name, &mut actual);
            let mut extensions = vec![0u16; 1024];
            let mut actual_ext = 0u32;
            let _ = info.GetFileExtensions(&mut extensions, &mut actual_ext);
            codecs.push(Codec {
                name: from_wide(&name),
                extensions: from_wide(&extensions)
                    .split(',')
                    .map(|e| e.trim().to_ascii_lowercase())
                    .filter(|e| e.starts_with('.') && e.len() > 1)
                    .collect(),
            });
        }
        Some(codecs)
    }
}

/// The text of a zero-ended UTF-16 buffer.
fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// An icon or a cursor file: several sizes of one picture in one file, which `decode` opens at the biggest.
fn is_icon(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ico") || e.eq_ignore_ascii_case("cur"))
}

/// The frame of the most pixels (an icon file holds 16 x 16 up to 256 x 256; the first is often the smallest).
fn biggest_frame(decoder: &IWICBitmapDecoder) -> Result<IWICBitmapFrameDecode, String> {
    // SAFETY: calls of the COM objects of WIC; the out parameters are plain integers.
    unsafe {
        let count = decoder.GetFrameCount().map_err(|e| e.to_string())?.clamp(1, 64);
        let mut best: Option<(u64, IWICBitmapFrameDecode)> = None;
        for index in 0..count {
            let Ok(frame) = decoder.GetFrame(index) else { continue };
            let (mut width, mut height) = (0u32, 0u32);
            if frame.GetSize(&mut width, &mut height).is_err() {
                continue;
            }
            let area = u64::from(width) * u64::from(height);
            if best.as_ref().is_none_or(|(most, _)| area > *most) {
                best = Some((area, frame));
            }
        }
        best.map(|(_, frame)| frame)
            .ok_or_else(|| "no frame to read".to_string())
    }
}

/// Opens `path` with the decoder of this machine that fits it and gives its first frame as RGBA. With `fit` (a box in pixels) a
/// frame bigger than the box is shrunk by WIC before it is turned to 8 bits, which is much cheaper for an HDR or a big one
/// (the picture is only ever shown that size); without it, at its own size.
pub fn decode(path: &Path, fit: Option<(usize, usize)>) -> Result<RgbaImage, String> {
    let factory = factory().ok_or("Windows Imaging Component is not available")?;
    let mut wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().collect();
    wide.push(0);
    // SAFETY: calls of the COM objects of WIC; `wide` is zero-ended and lives through the call, the buffer is the size
    // the frame says (width * height * 4).
    unsafe {
        let decoder = factory
            .CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .map_err(|e| format!("no codec of this Windows opens it: {e}"))?;
        let frame = if is_icon(path) {
            biggest_frame(&decoder)?
        } else {
            decoder.GetFrame(0).map_err(|e| e.to_string())?
        };
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).map_err(|e| e.to_string())?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err(format!("{width} x {height} pixels is not a size to read"));
        }
        // shrunk to the box first, when it is bigger (Fant is WIC's good filter for shrinking)
        let (width, height, source): (u32, u32, IWICBitmapSource) = match fit {
            Some((fit_width, fit_height))
                if (width as usize > fit_width || height as usize > fit_height) && fit_width > 0 && fit_height > 0 =>
            {
                let scale = (fit_width as f64 / f64::from(width)).min(fit_height as f64 / f64::from(height));
                let (w, h) = (
                    ((f64::from(width) * scale).round() as u32).max(1),
                    ((f64::from(height) * scale).round() as u32).max(1),
                );
                let scaler = factory.CreateBitmapScaler().map_err(|e| e.to_string())?;
                scaler
                    .Initialize(&frame, w, h, WICBitmapInterpolationModeFant)
                    .map_err(|e| e.to_string())?;
                (w, h, scaler.cast::<IWICBitmapSource>().map_err(|e| e.to_string())?)
            }
            _ => (
                width,
                height,
                frame.cast::<IWICBitmapSource>().map_err(|e| e.to_string())?,
            ),
        };
        // to 8-bit BGRA whatever the frame is (16 bits, floating point, CMYK, grey, a palette)
        let converter = factory.CreateFormatConverter().map_err(|e| e.to_string())?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(|e| e.to_string())?;
        let stride = width * 4;
        let mut pixels = vec![0u8; stride as usize * height as usize];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut pixels)
            .map_err(|e| e.to_string())?;
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2); // BGRA to RGBA
        }
        RgbaImage::from_raw(width, height, pixels).ok_or_else(|| "the pixels do not fit the size".to_string())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its file should stop at once
mod tests {
    use super::*;

    #[test]
    fn the_machine_lists_its_decoders_and_the_png_one_is_there() {
        let all = read_codecs().expect("WIC is on every Windows");
        assert!(!all.is_empty());
        assert!(
            all.iter().any(|c| c.extensions.iter().any(|e| e == ".png")),
            "no decoder for .png in {all:?}"
        );
        assert!(all.iter().all(|c| !c.name.is_empty()));
    }

    /// Not a check: prints what this machine has. `cargo test -p peekel-far -- --ignored --nocapture list_the_codecs`
    #[test]
    #[ignore = "prints the decoders of this machine"]
    #[allow(clippy::print_stdout)]
    fn list_the_codecs() {
        for codec in read_codecs().expect("WIC is on every Windows") {
            println!("{:<44} {}", codec.name, codec.extensions.join(" "));
        }
    }

    #[test]
    fn an_icon_file_is_opened_at_its_biggest_size_not_its_first() {
        // an ICO of two PNG frames, the small one first: 16 x 16 red and 48 x 48 blue
        let png = |side: u32, colour: [u8; 4]| {
            let mut bytes = std::io::Cursor::new(Vec::new());
            RgbaImage::from_pixel(side, side, image::Rgba(colour))
                .write_to(&mut bytes, image::ImageFormat::Png)
                .expect("a PNG");
            bytes.into_inner()
        };
        let frames = [(16u8, png(16, [255, 0, 0, 255])), (48u8, png(48, [0, 0, 255, 255]))];
        let mut ico = vec![0, 0, 1, 0, frames.len() as u8, 0];
        let mut offset = 6 + 16 * frames.len() as u32;
        for (side, data) in &frames {
            ico.extend([*side, *side, 0, 0, 1, 0, 32, 0]);
            ico.extend((data.len() as u32).to_le_bytes());
            ico.extend(offset.to_le_bytes());
            offset += data.len() as u32;
        }
        for (_, data) in &frames {
            ico.extend(data);
        }
        let dir = std::env::temp_dir().join(format!("peekel-wic-ico-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("two.ico");
        std::fs::write(&path, ico).expect("the icon is written");
        let opened = decode(&path, None).expect("Windows opens an icon");
        assert_eq!((opened.width(), opened.height()), (48, 48), "the biggest frame");
        assert_eq!(opened.get_pixel(24, 24).0, [0, 0, 255, 255], "and it is the blue one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Not a check: opens the files named in `PEEKEL_WIC_FILES` (`;` between them) as the plugin does, box 1000 x 700, and writes
    /// `<file>.wic.png` beside each. `cargo test -p peekel-far -- --ignored --nocapture open_the_files_in_the_variable`
    #[test]
    #[ignore = "opens the files of PEEKEL_WIC_FILES"]
    #[allow(clippy::print_stdout)]
    fn open_the_files_in_the_variable() {
        let list = std::env::var("PEEKEL_WIC_FILES").unwrap_or_default();
        for file in list.split(';').filter(|f| !f.is_empty()) {
            let started = std::time::Instant::now();
            match decode(Path::new(file), Some((1000, 700))) {
                Ok(picture) => {
                    println!(
                        "{file}: {} x {} in {:?}",
                        picture.width(),
                        picture.height(),
                        started.elapsed()
                    );
                    picture.save(format!("{file}.wic.png")).expect("the picture is written");
                }
                Err(e) => println!("{file}: {e}"),
            }
        }
    }

    #[test]
    fn a_png_file_is_opened_through_wic_with_its_own_colours() {
        let dir = std::env::temp_dir().join(format!("peekel-wic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("two.png");
        // 3 x 2 pixels: red, green, blue / white, black, a half-clear yellow
        let mut picture = RgbaImage::new(3, 2);
        for (x, y, c) in [
            (0, 0, [255, 0, 0, 255]),
            (1, 0, [0, 255, 0, 255]),
            (2, 0, [0, 0, 255, 255]),
            (0, 1, [255, 255, 255, 255]),
            (1, 1, [0, 0, 0, 255]),
            (2, 1, [255, 255, 0, 128]),
        ] {
            picture.put_pixel(x, y, image::Rgba(c));
        }
        picture.save(&path).expect("saves");
        let read = decode(&path, None).expect("opens through WIC");
        assert_eq!(read.dimensions(), (3, 2));
        assert_eq!(read.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(read.get_pixel(1, 0).0, [0, 255, 0, 255]);
        assert_eq!(read.get_pixel(2, 0).0, [0, 0, 255, 255]);
        assert_eq!(read.get_pixel(1, 1).0, [0, 0, 0, 255]);
        assert_eq!(read.get_pixel(2, 1).0[3], 128, "the half clear pixel stays half clear");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_no_codec_opens_is_an_error_and_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("peekel-wic-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("nothing.png");
        std::fs::write(&path, b"this is not a picture").expect("writes");
        assert!(decode(&path, None).is_err());
        assert!(decode(&dir.join("missing.heic"), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_extensions_asked_for_are_matched_without_the_dot_and_in_any_case() {
        let _ = CODECS.set(vec![Codec {
            name: "A decoder".to_string(),
            extensions: vec![".heic".to_string(), ".avif".to_string()],
        }]);
        // another test may have set the list first (the real one): this one only needs a .png that every Windows has
        assert!(supports("PNG") || supports("heic"));
        assert!(!supports("nonexistent-extension"));
    }
}
