//! What to show of a picture: the picture held in memory, a zoom and a position, and the pixels that make up the window
//! for them. Pure arithmetic over pixels, so it is tested without a terminal.
//!
//! A zoom is a scale: pixels drawn for each pixel of the picture. [`Zoom::Fit`] is the whole picture in the room, never
//! enlarged (a small picture is shown as it is). The position is the picture's point (in its own pixels) in the middle
//! of the window; it is kept so that the window never looks past the picture's edge.

use image::RgbaImage;
use image::imageops::{self, FilterType};

use crate::decode::{Picture, halve, to_picture};

/// How much one step of zooming in or out changes the scale.
pub const ZOOM_STEP: f64 = 1.25;

/// The largest scale: 32 pixels drawn for each pixel of the picture.
pub const MAX_SCALE: f64 = 32.0;

/// How the picture is scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    /// The whole picture in the room, not enlarged.
    Fit,
    /// This many pixels drawn for each pixel of the picture.
    Scale(f64),
}

/// The room a picture is drawn in, in pixels.
pub type Room = (usize, usize);

/// A picture's size in pixels.
pub type Size = (u32, u32);

/// The scale at which the whole picture fills the room one way or the other (above 1: it would have to be enlarged).
pub fn fit_scale(picture: Size, room: Room) -> f64 {
    (room.0 as f64 / f64::from(picture.0)).min(room.1 as f64 / f64::from(picture.1))
}

/// The scale at which the picture is as wide as the room.
pub fn width_scale(picture: Size, room: Room) -> f64 {
    room.0 as f64 / f64::from(picture.0)
}

/// A zoom and a position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: Zoom,
    /// The picture's point at the middle of the window, in the picture's pixels.
    pub center: (f64, f64),
}

impl View {
    /// The whole picture, centred.
    pub fn new(picture: Size) -> Self {
        Self {
            zoom: Zoom::Fit,
            center: (f64::from(picture.0) / 2.0, f64::from(picture.1) / 2.0),
        }
    }

    /// The scale in effect.
    pub fn scale(&self, picture: Size, room: Room) -> f64 {
        match self.zoom {
            Zoom::Fit => fit_scale(picture, room).min(1.0),
            Zoom::Scale(s) => s,
        }
    }

    /// Back to the whole picture, centred.
    pub fn fit(&mut self, picture: Size) {
        *self = Self::new(picture);
    }

    /// One pixel of the picture to one of the window.
    pub fn actual_size(&mut self, picture: Size, room: Room) {
        self.set_scale(1.0, picture, room);
    }

    /// As wide as the room (the picture is enlarged if it is narrower).
    pub fn fit_width(&mut self, picture: Size, room: Room) {
        self.set_scale(width_scale(picture, room), picture, room);
    }

    /// Scales by `factor` (more than 1: zooms in), about the middle of the window. Zooming out stops at a quarter of
    /// the scale at which the whole picture fits; a scale within 2 % of that is Fit again.
    pub fn zoom_by(&mut self, factor: f64, picture: Size, room: Room) {
        let wanted = self.scale(picture, room) * factor;
        self.set_scale(wanted, picture, room);
    }

    fn set_scale(&mut self, wanted: f64, picture: Size, room: Room) {
        let fit = fit_scale(picture, room).min(1.0);
        let scale = wanted.clamp(fit / 4.0, MAX_SCALE.max(fit));
        self.zoom = if (scale - fit).abs() <= fit * 0.02 {
            Zoom::Fit
        } else {
            Zoom::Scale(scale)
        };
        self.keep_inside(picture, room);
    }

    /// Moves the window by this many pixels of the window (right and down are positive); stops at the picture's edge.
    pub fn pan(&mut self, dx: f64, dy: f64, picture: Size, room: Room) {
        let scale = self.scale(picture, room);
        self.center.0 += dx / scale;
        self.center.1 += dy / scale;
        self.keep_inside(picture, room);
    }

    /// The picture's own pixels the window covers: its size (the picture's, if the window is larger).
    pub fn visible(&self, picture: Size, room: Room) -> (f64, f64) {
        let scale = self.scale(picture, room);
        (
            (room.0 as f64 / scale).min(f64::from(picture.0)),
            (room.1 as f64 / scale).min(f64::from(picture.1)),
        )
    }

    /// Keeps the window inside the picture; when the whole picture shows, it is centred.
    fn keep_inside(&mut self, picture: Size, room: Room) {
        let (visible_w, visible_h) = self.visible(picture, room);
        let (w, h) = (f64::from(picture.0), f64::from(picture.1));
        self.center.0 = self.center.0.clamp(visible_w / 2.0, w - visible_w / 2.0);
        self.center.1 = self.center.1.clamp(visible_h / 2.0, h - visible_h / 2.0);
    }
}

/// A picture and the copies of it halved again and again (built when first needed), so that drawing a small scale of a
/// large photo does not start from all its pixels every time.
#[derive(Debug)]
pub struct Image {
    levels: Vec<RgbaImage>,
}

impl Image {
    pub fn new(picture: RgbaImage) -> Self {
        Self { levels: vec![picture] }
    }

    pub fn size(&self) -> Size {
        self.levels.first().map_or((0, 0), RgbaImage::dimensions)
    }

    /// The copy halved `wanted` times, or the smallest that exists when it cannot be halved that often.
    fn level(&mut self, wanted: usize) -> &RgbaImage {
        while self.levels.len() <= wanted {
            let Some(last) = self.levels.last() else { break };
            if last.width() < 2 || last.height() < 2 {
                break;
            }
            let next = halve(last);
            self.levels.push(next);
        }
        let k = wanted.min(self.levels.len().saturating_sub(1));
        &self.levels[k]
    }
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub picture: Picture,
}

/// The pixels of the window for `view` in `room`; None when there is no room or no picture.
pub fn render(image: &mut Image, view: &View, room: Room) -> Option<Frame> {
    let size = image.size();
    if room.0 == 0 || room.1 == 0 || size.0 == 0 || size.1 == 0 {
        return None;
    }
    let scale = view.scale(size, room);
    let (visible_w, visible_h) = view.visible(size, room);
    let x = (view.center.0 - visible_w / 2.0).clamp(0.0, f64::from(size.0) - visible_w);
    let y = (view.center.1 - visible_h / 2.0).clamp(0.0, f64::from(size.1) - visible_h);
    let out_w = ((visible_w * scale).round() as u32).clamp(1, u32::try_from(room.0).unwrap_or(u32::MAX));
    let out_h = ((visible_h * scale).round() as u32).clamp(1, u32::try_from(room.1).unwrap_or(u32::MAX));

    // from the copy halved as often as keeps the rest of the shrinking within 1/2 and 1: the largest k with
    // scale * 2^k <= 1
    let halvings = if scale < 1.0 {
        (-scale.log2()).floor() as usize
    } else {
        0
    };
    let level = image.level(halvings);
    let (fx, fy) = (
        f64::from(size.0) / f64::from(level.width()),
        f64::from(size.1) / f64::from(level.height()),
    );
    let crop_x = ((x / fx).floor() as u32).min(level.width().saturating_sub(1));
    let crop_y = ((y / fy).floor() as u32).min(level.height().saturating_sub(1));
    let crop_w = ((visible_w / fx).ceil() as u32).clamp(1, level.width() - crop_x);
    let crop_h = ((visible_h / fy).ceil() as u32).clamp(1, level.height() - crop_y);
    let cropped = imageops::crop_imm(level, crop_x, crop_y, crop_w, crop_h).to_image();
    let drawn = if cropped.dimensions() == (out_w, out_h) {
        cropped
    } else {
        imageops::resize(&cropped, out_w, out_h, FilterType::Triangle)
    };
    Some(Frame {
        picture: to_picture(&drawn),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that gets no frame should stop at once
mod tests {
    use super::*;
    use image::Rgba;

    const ROOM: Room = (1000, 580);

    /// A picture whose red channel says the column (x / `per_red`) and whose green says the row (y / `per_green`).
    fn ramp(width: u32, height: u32, per_red: u32, per_green: u32) -> Image {
        Image::new(RgbaImage::from_fn(width, height, |x, y| {
            Rgba([(x / per_red).min(255) as u8, (y / per_green).min(255) as u8, 0, 255])
        }))
    }

    fn drawn_size(image: &mut Image, view: &View, room: Room) -> (usize, usize) {
        let frame = render(image, view, room).expect("a frame");
        (frame.picture.width, frame.picture.height)
    }

    fn red_at(frame: &Frame, x: usize, y: usize) -> u32 {
        (frame.picture.argb[y * frame.picture.width + x] >> 16) & 255
    }

    #[test]
    fn fit_never_enlarges_a_small_picture() {
        let mut image = ramp(100, 50, 1, 1);
        let view = View::new(image.size());
        assert_eq!(view.scale(image.size(), ROOM), 1.0);
        assert_eq!(drawn_size(&mut image, &view, ROOM), (100, 50));
    }

    #[test]
    fn fit_shrinks_a_large_picture_to_the_room_keeping_its_proportions() {
        let mut image = ramp(4000, 2000, 16, 8);
        let view = View::new(image.size());
        assert_eq!(view.scale(image.size(), (1000, 500)), 0.25);
        assert_eq!(drawn_size(&mut image, &view, (1000, 500)), (1000, 500));
        // a room that is tall: the width decides
        assert_eq!(drawn_size(&mut image, &view, (400, 580)), (400, 200));
    }

    #[test]
    fn zooming_in_enlarges_a_small_picture_and_the_frame_follows_the_scale() {
        let mut image = ramp(100, 50, 1, 1);
        let mut view = View::new(image.size());
        view.zoom_by(2.0, image.size(), ROOM);
        assert_eq!(view.zoom, Zoom::Scale(2.0));
        assert_eq!(drawn_size(&mut image, &view, ROOM), (200, 100));
    }

    #[test]
    fn at_actual_size_a_large_picture_shows_its_middle_and_pan_moves_the_window() {
        let mut image = ramp(4000, 2000, 16, 8); // red = x / 16, green = y / 8
        let mut view = View::new(image.size());
        view.actual_size(image.size(), (1000, 500));
        let frame = render(&mut image, &view, (1000, 500)).expect("a frame");
        assert_eq!((frame.picture.width, frame.picture.height), (1000, 500));
        // the window starts at x = 2000 - 500 = 1500 and y = 1000 - 250 = 750: red 1500 / 16 = 93, green 750 / 8 = 93
        assert_eq!(red_at(&frame, 0, 0), 93);
        view.pan(160.0, 0.0, image.size(), (1000, 500)); // 160 pixels right: ten red steps
        let moved = render(&mut image, &view, (1000, 500)).expect("a frame");
        assert_eq!(red_at(&moved, 0, 0), 103);
    }

    #[test]
    fn the_window_never_looks_past_the_edge_and_a_picture_that_fits_does_not_move() {
        let size = (4000, 2000);
        let mut view = View::new(size);
        view.actual_size(size, (1000, 500));
        view.pan(1.0e9, 1.0e9, size, (1000, 500));
        assert_eq!(view.center, (3500.0, 1750.0), "the far corner");
        view.pan(-1.0e9, -1.0e9, size, (1000, 500));
        assert_eq!(view.center, (500.0, 250.0), "the near corner");

        let small = (100, 50);
        let mut fitted = View::new(small);
        fitted.pan(300.0, 300.0, small, ROOM);
        assert_eq!(fitted.center, (50.0, 25.0), "the whole picture shows: nothing to move");
    }

    #[test]
    fn zooming_out_stops_at_a_quarter_of_the_fit_and_zooming_in_stops_at_the_largest_scale() {
        let size = (4000, 2000);
        let mut view = View::new(size);
        let fit = view.scale(size, (1000, 500));
        for _ in 0..40 {
            view.zoom_by(1.0 / ZOOM_STEP, size, (1000, 500));
        }
        assert_eq!(view.scale(size, (1000, 500)), fit / 4.0);
        for _ in 0..80 {
            view.zoom_by(ZOOM_STEP, size, (1000, 500));
        }
        assert_eq!(view.scale(size, (1000, 500)), MAX_SCALE);
    }

    #[test]
    fn a_scale_within_two_percent_of_fit_is_fit_again() {
        let size = (4000, 2000);
        let room = (1000, 500);
        let mut view = View::new(size);
        view.zoom_by(ZOOM_STEP, size, room);
        assert!(matches!(view.zoom, Zoom::Scale(_)));
        view.zoom_by(1.0 / ZOOM_STEP, size, room);
        assert_eq!(view.zoom, Zoom::Fit, "back where it was");
        assert_eq!(view.center, (2000.0, 1000.0));
    }

    #[test]
    fn fit_width_makes_the_picture_as_wide_as_the_room() {
        let mut image = ramp(400, 1000, 2, 4); // tall and narrow
        let mut view = View::new(image.size());
        view.fit_width(image.size(), (1000, 580));
        assert_eq!(view.scale(image.size(), (1000, 580)), 2.5);
        assert_eq!(
            drawn_size(&mut image, &view, (1000, 580)),
            (1000, 580),
            "the window is full: the picture is taller"
        );
    }

    #[test]
    fn every_scale_gives_a_frame_that_fits_the_room_and_is_not_empty() {
        // odd sizes, tiny and huge scales, rooms that are not a multiple of anything
        for (w, h) in [(1, 1), (3, 2), (1201, 599), (129, 2501)] {
            let mut image = ramp(w, h, 1, 1);
            for scale in [0.001, 0.07, 0.33, 0.5, 0.99, 1.0, 1.7, 5.0, 32.0] {
                for room in [(1, 1), (37, 19), (1000, 580)] {
                    let mut view = View::new(image.size());
                    view.zoom = Zoom::Scale(scale);
                    let (fw, fh) = drawn_size(&mut image, &view, room);
                    assert!(
                        fw >= 1 && fh >= 1 && fw <= room.0 && fh <= room.1,
                        "{w}x{h} at {scale} in {room:?}: {fw}x{fh}"
                    );
                }
            }
        }
    }

    #[test]
    fn no_room_or_no_picture_gives_no_frame() {
        let mut image = ramp(10, 10, 1, 1);
        let view = View::new(image.size());
        assert_eq!(render(&mut image, &view, (0, 100)), None);
        assert_eq!(render(&mut image, &view, (100, 0)), None);
    }

    #[test]
    fn a_shrunk_frame_comes_from_a_halved_copy_and_looks_like_the_whole_picture() {
        // red grows with x across the whole picture: a quarter-scale frame must grow the same way, edge to edge
        let mut image = ramp(4000, 2000, 16, 8);
        let view = View::new(image.size());
        let frame = render(&mut image, &view, (1000, 500)).expect("a frame");
        let (left, right) = (red_at(&frame, 0, 250), red_at(&frame, 999, 250));
        assert!(left <= 2 && right >= 248, "{left} .. {right}");
        assert!(image.levels.len() >= 3, "halved twice or more: {}", image.levels.len());
    }
}
