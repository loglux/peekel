//! A picture that carries an ICC profile is turned into sRGB, which is what a terminal shows.
//!
//! Done: RGB profiles (matrix and table ones, Display P3, Adobe RGB, camera and monitor profiles). Not done: grey
//! profiles (taken as sRGB's curve) and CMYK ones (the decoder has turned the picture to RGB before the profile can be
//! used). A profile that cannot be read leaves the picture as it is.

use image::RgbaImage;
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};

/// Colours that an sRGB profile must leave as they are (within `TOLERANCE`): when the transform of the profile does,
/// the whole picture is not converted.
const PROBES: [[u8; 4]; 8] = [
    [0, 0, 0, 255],
    [255, 255, 255, 255],
    [255, 0, 0, 255],
    [0, 255, 0, 255],
    [0, 0, 255, 255],
    [128, 128, 128, 255],
    [200, 150, 120, 255],
    [30, 60, 90, 255],
];
const TOLERANCE: i16 = 2;

/// Converts `img` from the profile in `icc` to sRGB, in place. Returns whether the pixels changed.
pub fn to_srgb(img: &mut RgbaImage, icc: &[u8]) -> bool {
    let Ok(source) = ColorProfile::new_from_slice(icc) else {
        return false;
    };
    if source.color_space != DataColorSpace::Rgb {
        return false;
    }
    let Ok(transform) = source.create_transform_8bit(
        Layout::Rgba,
        &ColorProfile::new_srgb(),
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return false;
    };
    let mut probed = [0u8; PROBES.len() * 4];
    if transform.transform(PROBES.as_flattened(), &mut probed).is_err() {
        return false;
    }
    if probed
        .iter()
        .zip(PROBES.as_flattened())
        .all(|(&got, &was)| (i16::from(got) - i16::from(was)).abs() <= TOLERANCE)
    {
        return false;
    }
    let mut out = vec![0u8; img.as_raw().len()];
    if transform.transform(img.as_raw(), &mut out).is_err() {
        return false;
    }
    // the transform may not carry the alpha over: it is kept as it was
    let (new_pixels, _) = out.as_chunks_mut::<4>();
    let (old_pixels, _) = img.as_raw().as_chunks::<4>();
    for (new, old) in new_pixels.iter_mut().zip(old_pixels) {
        new[3] = old[3];
    }
    match RgbaImage::from_raw(img.width(), img.height(), out) {
        Some(converted) => {
            *img = converted;
            true
        }
        None => false,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn flat(rgb: [u8; 3], alpha: u8) -> RgbaImage {
        RgbaImage::from_pixel(4, 4, image::Rgba([rgb[0], rgb[1], rgb[2], alpha]))
    }

    #[test]
    fn a_display_p3_picture_is_turned_to_srgb_and_keeps_its_alpha() {
        let p3 = ColorProfile::new_display_p3().encode().expect("encodes");
        let mut img = flat([100, 200, 100], 77);
        assert!(to_srgb(&mut img, &p3));
        let [r, g, b, a] = img.get_pixel(1, 1).0;
        // P3's greens are more saturated than sRGB's: the same numbers are a duller colour once in sRGB's terms
        assert!(r < 60 && g > 200 && b < 95, "{r} {g} {b}");
        assert_eq!(a, 77);
    }

    #[test]
    fn a_srgb_picture_and_a_broken_profile_are_left_alone() {
        let srgb = ColorProfile::new_srgb().encode().expect("encodes");
        let mut img = flat([100, 200, 100], 255);
        assert!(!to_srgb(&mut img, &srgb));
        assert_eq!(img.get_pixel(0, 0).0, [100, 200, 100, 255]);
        assert!(!to_srgb(&mut img, b"not a profile"));
        assert!(!to_srgb(&mut img, &[]));
        assert_eq!(img.get_pixel(0, 0).0, [100, 200, 100, 255]);
    }
}
