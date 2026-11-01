//! A picture file turned the way its EXIF says and scaled down to fit a box.
//!
//! Follows BlueEdit's `Renderer.Decode` (`src/MathImages/Renderer.cs`): PNG, JPEG, GIF's first frame, WebP, BMP, TIFF;
//! the eight EXIF orientations; scaled down, never up, so a large photo does not alias. Skia (BlueEdit) shrinks with
//! mipmaps; here the picture is halved by 2 × 2 averages while it is at least twice the target, then the last step is a
//! linear resize. The two are not the same filter, so pixels can differ a little from BlueEdit's.

use std::fmt;
use std::path::Path;

use crate::guard::unreadable;
use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageDecoder, ImageReader, RgbaImage};

/// More pixels than this are refused before they are decoded: 100 megapixels is 400 MB.
const MAX_PIXELS: u64 = 100_000_000;

/// A decoded picture: `0xAARRGGBB` pixels, row after row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub argb: Vec<u32>,
}

/// Why a file did not become a [`Picture`].
#[derive(Debug)]
pub enum DecodeError {
    /// The box to fit it in has no area.
    NoRoom,
    /// More than 100 megapixels.
    TooLarge,
    /// Not there, not a picture this program reads, or damaged; the text says which.
    Unreadable(String),
    /// A file that is no picture, which a viewer skips (a font, an archive, a program): not an error to report.
    NotAPicture,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRoom => f.write_str("no room"),
            Self::TooLarge => f.write_str("too large (over 100 megapixels)"),
            Self::Unreadable(why) => f.write_str(why),
            Self::NotAPicture => f.write_str("not a picture"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Reads `path`, turns it upright and scales it down to fit `max_width` × `max_height` (never up).
pub fn decode(path: &Path, max_width: usize, max_height: usize) -> Result<Picture, DecodeError> {
    if max_width == 0 || max_height == 0 {
        return Err(DecodeError::NoRoom);
    }
    Ok(to_picture(&fit(decode_full(path)?, max_width, max_height)))
}

/// A format that the `image` crate does not read, with its own module: how its name is recognised and how a file of it
/// is drawn. A new format is one more line here and a module of its own.
struct Special {
    is: fn(&Path) -> bool,
    draw: fn(&Path) -> Result<RgbaImage, DecodeError>,
}

const SPECIAL: [Special; 6] = [
    Special {
        is: crate::raw::is_raw,
        draw: crate::raw::render,
    },
    Special {
        is: crate::svg::is_svg,
        draw: crate::svg::render,
    },
    Special {
        is: crate::pdf::is_pdf,
        draw: crate::pdf::render_first_page,
    },
    Special {
        is: crate::docx::is_docx,
        draw: crate::docx::render_first_page,
    },
    Special {
        is: crate::pptx::is_pptx,
        draw: crate::pptx::render_first_page,
    },
    Special {
        is: crate::xlsx::is_xlsx,
        draw: crate::xlsx::render_first_page,
    },
];

/// Whether the name says a format of its own (SVG, PDF, ...) and not one of the pictures `image` reads.
pub fn is_special(path: &Path) -> bool {
    SPECIAL.iter().any(|s| (s.is)(path))
}

/// Reads `path` and turns it upright, at its own size.
pub fn decode_full(path: &Path) -> Result<RgbaImage, DecodeError> {
    if let Some(special) = SPECIAL.iter().find(|s| (s.is)(path)) {
        return (special.draw)(path);
    }
    decode_reader(ImageReader::open(path).map_err(unreadable)?, MAX_PIXELS, 1)
}

/// Page `page` (counted from 0) of the file at `path` and the number of pages it has: a PDF has many, anything else one
/// (and `page` is not looked at). A page past the end is the last.
pub fn decode_page(path: &Path, page: usize) -> Result<(RgbaImage, usize), DecodeError> {
    if crate::pdf::is_pdf(path) {
        return crate::pdf::render_page(path, page);
    }
    decode_full(path).map(|image| (image, 1))
}

/// A picture held in memory (one inside a document), turned upright, at its own size.
/// One that claims more than `max_pixels` is refused before it is decoded.
pub fn decode_bytes(bytes: &[u8], max_pixels: u64) -> Result<RgbaImage, DecodeError> {
    decode_reader(ImageReader::new(std::io::Cursor::new(bytes)), max_pixels, 1)
}

/// The same for a picture that is a part of another file (the preview of a RAW photograph), which says in its own EXIF
/// how it is turned, if it does; when it does not, `fallback` is the EXIF orientation (1 to 8) of the file it is in.
pub(crate) fn decode_bytes_oriented(bytes: &[u8], max_pixels: u64, fallback: u16) -> Result<RgbaImage, DecodeError> {
    decode_reader(ImageReader::new(std::io::Cursor::new(bytes)), max_pixels, fallback)
}

fn decode_reader<R: std::io::BufRead + std::io::Seek>(
    reader: ImageReader<R>,
    max_pixels: u64,
    fallback: u16,
) -> Result<RgbaImage, DecodeError> {
    let mut decoder = reader
        .with_guessed_format()
        .map_err(unreadable)?
        .into_decoder()
        .map_err(unreadable)?;
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > max_pixels.min(MAX_PIXELS) {
        return Err(DecodeError::TooLarge);
    }
    let mut orientation = decoder.orientation().map_err(unreadable)?;
    if orientation == image::metadata::Orientation::NoTransforms
        && let Some(from_file) = u8::try_from(fallback)
            .ok()
            .and_then(image::metadata::Orientation::from_exif)
    {
        orientation = from_file;
    }
    let profile = decoder.icc_profile().ok().flatten();
    let mut picture = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    picture.apply_orientation(orientation);
    let mut rgba = picture.into_rgba8();
    if let Some(profile) = profile {
        crate::icc::to_srgb(&mut rgba, &profile);
    }
    Ok(rgba)
}

/// `img` scaled down to fit the box, keeping its proportions; it is returned as it is when it fits already.
pub fn fit(img: RgbaImage, max_width: usize, max_height: usize) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return img;
    }
    let scale = (max_width as f64 / f64::from(w))
        .min(max_height as f64 / f64::from(h))
        .min(1.0);
    // Math.Round of the C# rounds halves to even
    let target = |side: u32| ((f64::from(side) * scale).round_ties_even() as u32).max(1);
    let (tw, th) = (target(w), target(h));
    if (tw, th) == (w, h) {
        return img;
    }
    let mut current = img;
    while current.width() / 2 >= tw && current.height() / 2 >= th {
        current = halve(&current);
    }
    if current.dimensions() == (tw, th) {
        return current;
    }
    imageops::resize(&current, tw, th, FilterType::Triangle)
}

/// Half the size: each pixel the mean of a 2 × 2 block, weighted by alpha so a transparent pixel's colour does not
/// leak into its neighbours. An odd last row or column is dropped.
///
/// Rows are shared out among the cores (a 20 MP photo is halved twice, and one core took about 100 ms of the 150 the
/// shrink needs). A block of four opaque pixels, which is every pixel of a JPEG, is a plain mean: the same bytes the
/// weighted formula gives for it (the unit tests check that against the formula on random pictures).
pub(crate) fn halve(img: &RgbaImage) -> RgbaImage {
    /// Below this many output pixels the threads cost more than they save.
    const PARALLEL_FROM: usize = 1 << 16;

    let (w, h) = (img.width() as usize / 2, img.height() as usize / 2);
    let mut out = vec![0u8; w * h * 4];
    if w > 0 && h > 0 {
        let src = img.as_raw();
        let src_stride = img.width() as usize * 4;
        let threads = if w * h < PARALLEL_FROM {
            1
        } else {
            std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
        };
        let rows_per_thread = h.div_ceil(threads);
        std::thread::scope(|scope| {
            for (n, chunk) in out.chunks_mut(rows_per_thread * w * 4).enumerate() {
                scope.spawn(move || halve_rows(src, src_stride, w, n * rows_per_thread, chunk));
            }
        });
    }
    RgbaImage::from_raw(w as u32, h as u32, out).unwrap_or_else(|| RgbaImage::new(w as u32, h as u32))
}

/// The output rows `first_row..` that fit in `out`, each pixel from a 2 × 2 block of `src` (rows `src_stride` bytes).
fn halve_rows(src: &[u8], src_stride: usize, width: usize, first_row: usize, out: &mut [u8]) {
    for (r, row) in out.chunks_mut(width * 4).enumerate() {
        let y = first_row + r;
        let top = &src[2 * y * src_stride..];
        let bottom = &src[(2 * y + 1) * src_stride..];
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let at = x * 8;
            let block = [
                &top[at..at + 4],
                &top[at + 4..at + 8],
                &bottom[at..at + 4],
                &bottom[at + 4..at + 8],
            ];
            let alpha: u32 = block.iter().map(|p| u32::from(p[3])).sum();
            if alpha == 4 * 255 {
                for ch in 0..3 {
                    let sum: u32 = block.iter().map(|p| u32::from(p[ch])).sum();
                    pixel[ch] = ((sum + 2) / 4) as u8;
                }
                pixel[3] = 255;
            } else {
                for ch in 0..3 {
                    let sum: u32 = block.iter().map(|p| u32::from(p[ch]) * u32::from(p[3])).sum();
                    pixel[ch] = (sum + alpha / 2).checked_div(alpha).unwrap_or(0) as u8;
                }
                pixel[3] = ((alpha + 2) / 4) as u8;
            }
        }
    }
}

/// The pixels of `img` as a [`Picture`].
pub fn to_picture(img: &RgbaImage) -> Picture {
    let argb = img
        .pixels()
        .map(|p| u32::from(p[3]) << 24 | u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
        .collect();
    Picture {
        width: img.width() as usize,
        height: img.height() as usize,
        argb,
    }
}

/// Lays the pixels (`0xAARRGGBB`) over `background`: each becomes opaque, a half clear one half its own colour and half the
/// background's. A Sixel picture has no half-clear pixel, so this is what makes a soft shadow or a half-clear shape look as
/// it should.
pub fn composite_over(argb: &mut [u32], background: [u8; 3]) {
    let [br, bg, bb] = background;
    for p in argb {
        let a = *p >> 24;
        if a < 255 {
            let mix = |shift: u32, back: u8| (((*p >> shift) & 255) * a + u32::from(back) * (255 - a) + 127) / 255;
            *p = 0xFF00_0000 | mix(16, br) << 16 | mix(8, bg) << 8 | mix(0, bb);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot decode its own fixture should stop at once
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn composite_over_mixes_by_the_alpha_and_leaves_an_opaque_pixel_alone() {
        let mut pixels = [0xFF11_2233, 0x0000_0000, 0x80FF_0000, 0x00FF_FFFF];
        composite_over(&mut pixels, [0, 0, 128]);
        assert_eq!(pixels[0], 0xFF11_2233, "opaque: as it was");
        assert_eq!(pixels[1], 0xFF00_0080, "clear: the background");
        assert_eq!(pixels[2], 0xFF80_0040, "half clear red over blue: half and half");
        assert_eq!(pixels[3], 0xFF00_0080, "clear whatever its colour");
    }

    fn image_path(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("images")
            .join(name)
    }

    fn rgb(p: u32) -> (u32, u32, u32) {
        ((p >> 16) & 255, (p >> 8) & 255, p & 255)
    }

    #[test]
    fn a_photo_is_turned_the_way_its_exif_says() {
        // each file is stored turned some way and says so: all show 40 x 20, red left, blue right, green top left
        for orientation in 1..=8 {
            let got = decode(&image_path(&format!("exif-{orientation}.jpg")), 400, 400).expect("decodes");
            assert_eq!((got.width, got.height), (40, 20), "exif-{orientation}");
            let at = |x: usize, y: usize| rgb(got.argb[y * got.width + x]);
            let (r, g, b) = at(2, 2);
            assert!(
                g > 150 && r < 80,
                "exif-{orientation}: green at the top left, got {:?}",
                (r, g, b)
            );
            let (r, _, b) = at(12, 10);
            assert!(r > 150 && b < 80, "exif-{orientation}: red on the left");
            let (r, _, b) = at(30, 10);
            assert!(b > 150 && r < 80, "exif-{orientation}: blue on the right");
        }
    }

    #[test]
    fn a_picture_is_scaled_down_to_fit_and_never_up() {
        let small = decode(&image_path("exif-1.jpg"), 20, 20).expect("decodes");
        assert_eq!((small.width, small.height), (20, 10), "proportions kept");
        let roomy = decode(&image_path("exif-1.jpg"), 4000, 4000).expect("decodes");
        assert_eq!((roomy.width, roomy.height), (40, 20), "not scaled up");
    }

    #[test]
    fn nothing_decodes_into_no_room() {
        assert!(matches!(
            decode(&image_path("exif-1.jpg"), 0, 10),
            Err(DecodeError::NoRoom)
        ));
        assert!(matches!(
            decode(&image_path("exif-1.jpg"), 10, 0),
            Err(DecodeError::NoRoom)
        ));
    }

    #[test]
    fn a_missing_file_and_a_file_that_is_no_picture_are_errors_not_panics() {
        assert!(matches!(
            decode(&image_path("nothing.png"), 10, 10),
            Err(DecodeError::Unreadable(_))
        ));
        let not_a_picture = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(matches!(
            decode(&not_a_picture, 10, 10),
            Err(DecodeError::Unreadable(_))
        ));
    }

    #[test]
    fn halving_averages_a_checkerboard_to_grey_and_keeps_a_flat_colour() {
        let board = RgbaImage::from_fn(8, 8, |x, y| {
            if (x + y) % 2 == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        let grey = fit(board, 4, 4);
        assert!(
            grey.pixels().all(|p| (126..=129).contains(&p[0]) && p[3] == 255),
            "{:?}",
            grey.get_pixel(0, 0)
        );
        let flat = fit(RgbaImage::from_pixel(64, 48, Rgba([10, 200, 30, 255])), 8, 6);
        assert_eq!((flat.width(), flat.height()), (8, 6));
        assert!(flat.pixels().all(|p| p.0 == [10, 200, 30, 255]));
    }

    /// The first version, one pixel at a time through `get_pixel`: what `halve` must give the same bytes as.
    fn halve_reference(img: &RgbaImage) -> RgbaImage {
        let (w, h) = (img.width() / 2, img.height() / 2);
        RgbaImage::from_fn(w, h, |x, y| {
            let mut sum = [0u32; 3];
            let mut alpha = 0u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = img.get_pixel(x * 2 + dx, y * 2 + dy).0;
                let a = u32::from(p[3]);
                for ch in 0..3 {
                    sum[ch] += u32::from(p[ch]) * a;
                }
                alpha += a;
            }
            let mean = |s: u32| (s + alpha / 2).checked_div(alpha).unwrap_or(0) as u8;
            Rgba([mean(sum[0]), mean(sum[1]), mean(sum[2]), ((alpha + 2) / 4) as u8])
        })
    }

    /// A picture of pseudo-random pixels; `alpha`: how each pixel's opacity is chosen.
    fn random_picture(width: u32, height: u32, alpha: impl Fn(u32, u8) -> u8) -> RgbaImage {
        let mut state = 0x2545_F491u32;
        RgbaImage::from_fn(width, height, |x, y| {
            let mut next = || {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            };
            let (r, g, b, a) = (next(), next(), next(), next());
            Rgba([r, g, b, alpha(x + y, a)])
        })
    }

    #[test]
    fn the_fast_halving_gives_the_same_bytes_as_the_formula() {
        // odd sizes (the last row and column are dropped), every kind of opacity, small (one thread) and large (several)
        for (w, h) in [(2, 2), (3, 5), (37, 23), (64, 64), (701, 503)] {
            for (name, picture) in [
                ("opaque", random_picture(w, h, |_, _| 255)),
                ("random alpha", random_picture(w, h, |_, a| a)),
                (
                    "mostly opaque",
                    random_picture(w, h, |n, a| if n % 7 == 0 { a } else { 255 }),
                ),
                ("fully transparent", random_picture(w, h, |_, _| 0)),
            ] {
                assert_eq!(halve(&picture), halve_reference(&picture), "{w} x {h}, {name}");
            }
        }
    }

    #[test]
    fn a_transparent_pixel_does_not_tint_its_neighbours_when_halved() {
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, Rgba([255, 0, 0, 0])); // red, but fully transparent
        let halved = halve(&img);
        assert_eq!(
            halved.get_pixel(0, 0).0,
            [0, 0, 255, 191],
            "still blue, a quarter less opaque"
        );
    }

    #[test]
    fn a_target_size_rounds_halves_to_even_as_the_original_does() {
        // 2 x 5 into a width of 1: the scale is 0.5 and the height 2.5. Math.Round(2.5) is 2 in C# (to even), 3 away from zero
        let out = fit(RgbaImage::from_pixel(2, 5, Rgba([1, 2, 3, 255])), 1, 1000);
        assert_eq!(out.dimensions(), (1, 2));
        // 2 x 7: 3.5 is 4, the even one
        let out = fit(RgbaImage::from_pixel(2, 7, Rgba([1, 2, 3, 255])), 1, 1000);
        assert_eq!(out.dimensions(), (1, 4));
    }

    #[test]
    fn a_picture_that_fits_is_returned_as_it_is() {
        let out = fit(RgbaImage::from_pixel(5, 5, Rgba([1, 2, 3, 255])), 1000, 1000);
        assert_eq!(out.dimensions(), (5, 5));
    }
}
