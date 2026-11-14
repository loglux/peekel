//! The night look of a book (the viewer's `b`, and `night = yes` in its settings): the page dark and its text light, for reading in a dark room.
//!
//! The lightness of every pixel is turned over and its hue and saturation are kept: a white page goes dark, black text goes light, a
//! red heading stays red (a plain negative would make it cyan), and a picture on the page keeps its colours, with its light and shade
//! turned over. Only a page that is mostly paper is turned: a photograph, a cover or a page that is dark already is left as it is (on
//! a brochure of photographs the negative was ugly, and a dark page came out glaring). The result is then pressed between a dark grey and a soft white, so that the page is not a black hole and the text does
//! not glare. The pixels that are clear (the gap between the pages of a spread) stay clear: the terminal's own background shows there.

use image::RgbaImage;

/// The darkest the page gets (the white of the paper), and the lightest (the black of the text).
const DARK: u8 = 24;
const LIGHT: u8 = 214;

/// A pixel this light in all three channels is paper.
const PAPER: u8 = 200;
/// A page is turned only when at least this share of it is paper: a page of text (with a picture or two on it) is; a photograph, a
/// cover or a page that is dark already is not, and is left as it is (turned over, it would be a negative, and a dark page would glare).
const PAPER_SHARE: f64 = 0.4;

/// The page turned to its night look, in place, when it is a page of paper (see [`PAPER_SHARE`]); whether it was turned.
pub fn darken(image: &mut RgbaImage) -> bool {
    if !is_paper(image) {
        return false;
    }
    for pixel in image.pixels_mut() {
        let [r, g, b, a] = pixel.0;
        let [r, g, b] = night([r, g, b]);
        pixel.0 = [r, g, b, a];
    }
    true
}

/// Whether the page is mostly paper: of its pixels that are not clear, one in sixteen is looked at (enough for a share).
fn is_paper(image: &RgbaImage) -> bool {
    let (mut seen, mut paper) = (0u64, 0u64);
    for pixel in image.pixels().step_by(16) {
        let [r, g, b, a] = pixel.0;
        if a == 0 {
            continue;
        }
        seen += 1;
        if r.min(g).min(b) >= PAPER {
            paper += 1;
        }
    }
    seen > 0 && paper as f64 >= seen as f64 * PAPER_SHARE
}

/// One colour in the night look. In HSL terms the lightness L becomes 1 - L, the hue and saturation are as they were: adding
/// `255 - max - min` to each channel does it (the differences between the channels, which make the hue, are kept; the largest becomes
/// `255 - min` and the smallest `255 - max`).
fn night(c: [u8; 3]) -> [u8; 3] {
    let max = i32::from(c[0].max(c[1]).max(c[2]));
    let min = i32::from(c[0].min(c[1]).min(c[2]));
    let shift = 255 - max - min;
    c.map(|v| {
        let turned = (i32::from(v) + shift).clamp(0, 255);
        let pressed = i32::from(DARK) + turned * i32::from(LIGHT - DARK) / 255;
        pressed as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_paper_goes_dark_black_text_goes_light_and_grey_stays_in_the_middle() {
        assert_eq!(night([255, 255, 255]), [DARK; 3]);
        assert_eq!(night([0, 0, 0]), [LIGHT; 3]);
        let [g, _, _] = night([128, 128, 128]);
        assert!((115..=125).contains(&g), "a middle grey stays about the middle: {g}");
    }

    #[test]
    fn a_colour_keeps_its_hue_a_dark_red_becomes_a_light_red_not_a_cyan() {
        let [r, g, b] = night([150, 0, 0]);
        assert!(r > g && r > b && g == b, "still red: {r} {g} {b}");
        assert!(r > 150, "and lighter, as the dark red was dark: {r}");
    }

    #[test]
    fn the_clear_gap_between_pages_stays_clear() {
        let mut page = RgbaImage::from_pixel(32, 1, image::Rgba([255, 255, 255, 255]));
        page.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        assert!(darken(&mut page));
        assert_eq!(page.get_pixel(0, 0).0, [DARK, DARK, DARK, 255]);
        assert_eq!(page.get_pixel(1, 0).0[3], 0, "the clear pixel is still clear");
    }

    #[test]
    fn a_page_of_text_is_turned_and_a_photograph_or_a_dark_page_is_left_as_it_is() {
        // white paper with a band of black text: mostly paper
        let mut text = RgbaImage::from_pixel(64, 64, image::Rgba([255, 255, 255, 255]));
        for x in 0..64 {
            for y in 20..30 {
                text.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        assert!(darken(&mut text));
        // a photograph: sky blue and brown earth, no paper
        let mut photo = RgbaImage::from_fn(64, 64, |_, y| {
            image::Rgba(if y < 32 {
                [90, 140, 200, 255]
            } else {
                [120, 90, 50, 255]
            })
        });
        let before = photo.clone();
        assert!(!darken(&mut photo));
        assert_eq!(photo, before);
        // a page that is dark already
        let mut dark = RgbaImage::from_pixel(64, 64, image::Rgba([10, 10, 10, 255]));
        assert!(!darken(&mut dark));
        assert_eq!(dark.get_pixel(0, 0).0, [10, 10, 10, 255]);
    }
}
