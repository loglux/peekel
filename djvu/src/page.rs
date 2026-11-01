//! Step 5: a page as one picture. The black-and-white mask of the text (`Sjbz`, JB2) is laid over the colour pictures: the
//! background (`BG44`) and the foreground (`FG44`), both of which may be smaller than the page (spec 7.1.3).
//!
//! Written from the spec as far as it goes; the rest (how a smaller picture is made big, the gamma) is settled by comparing with
//! DjVuLibre's `ddjvu` and recorded in docs/internal/djvu.md.

use std::sync::Mutex;

use crate::Error;
use crate::document::{DirectoryEntry, Document, Page};
use crate::info::Rotation;
use crate::iw44::{Iw44, Pixmap};
use crate::jb2;

/// Where the dictionary of shapes a page uses is: the page's own `Djbz` chunk (with no name: it belongs to this page), or else the
/// `Djbz` of the first `INCL` that names a component with one (with that name: the same dictionary serves every page that includes
/// it, so a caller that turns many pages can decode it once). The data of the chunk.
pub fn dictionary_chunk<'a>(
    document: &Document<'a>,
    entries: &[DirectoryEntry],
    page: &Page<'_, 'a>,
) -> Option<(Option<&'a [u8]>, &'a [u8])> {
    if let Some(chunk) = page.chunks(*b"Djbz").next() {
        return Some((None, chunk.data));
    }
    for name in page.included_names() {
        if let Some(form) = document.included(entries, name)
            && let Some(chunk) = form.first_chunk(*b"Djbz")
        {
            return Some((Some(name), chunk.data));
        }
    }
    None
}

/// The dictionary of shapes a page uses, decoded.
///
/// # Errors
/// As [`jb2::decode_dictionary`].
pub fn shared_dictionary(
    document: &Document<'_>,
    entries: &[DirectoryEntry],
    page: &Page<'_, '_>,
) -> Result<Option<jb2::Dictionary>, Error> {
    match dictionary_chunk(document, entries, page) {
        Some((_, data)) => jb2::decode_dictionary(data, None).map(Some),
        None => Ok(None),
    }
}

/// The colour picture made of the chunks with this id (`BG44` or `FG44`), when the page has any.
fn layer(page: &Page<'_, '_>, id: [u8; 4]) -> Result<Option<Pixmap>, Error> {
    let mut picture = Iw44::new();
    let mut any = false;
    for chunk in page.chunks(id) {
        picture.add_chunk(chunk.data)?;
        any = true;
    }
    if any { picture.pixmap().map(Some) } else { Ok(None) }
}

/// The most pixels a page may have: a scan at 600 dpi of a large page is about 70 million; a page that says more is refused before
/// anything is made.
pub const MAX_PIXELS: usize = 100_000_000;

/// The colours of the shapes of a page (the `FGbz` chunk, spec 8.3.10): a palette, and for each shape put on the page (each blit, in
/// order) the number of its colour in the palette.
struct Palette {
    colours: Vec<[u8; 3]>,
    indices: Vec<u16>,
}

/// The most shapes one page may colour.
const MAX_COLOURED_SHAPES: usize = 16 * 1024 * 1024;

/// Reads an `FGbz` chunk. None when the chunk is not of a kind that colours the shapes one by one (no colour numbers, an unknown
/// version, data that is cut or wrong): the page is then drawn as if it had none.
fn palette(data: &[u8]) -> Option<Palette> {
    let (&version, rest) = data.split_first()?;
    if version & 0x7F != 0 || version & 0x80 == 0 {
        return None;
    }
    let (size, rest) = rest.split_first_chunk::<2>()?;
    let size = usize::from(u16::from_be_bytes(*size));
    let (entries, rest) = rest.split_at_checked(size * 3)?;
    // the colours are stored blue, green, red
    let colours: Vec<[u8; 3]> = entries.as_chunks::<3>().0.iter().map(|c| [c[2], c[1], c[0]]).collect();
    let (count, rest) = rest.split_first_chunk::<3>()?;
    let count = (usize::from(count[0]) << 16) | (usize::from(count[1]) << 8) | usize::from(count[2]);
    if count > MAX_COLOURED_SHAPES {
        return None;
    }
    let raw = crate::bzz::decode(rest, count * 2).ok()?;
    let indices: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .take(count)
        .map(|&c| u16::from_be_bytes(c))
        .collect();
    if indices.len() != count || indices.iter().any(|&i| usize::from(i) >= colours.len()) {
        return None;
    }
    Some(Palette { colours, indices })
}

/// The shapes of a page painted on `rgb` (`width` x `height`, the background already there) each in the colour its number says in the
/// palette. Where two shapes of the same colour overlap the pixel has that colour, and where shapes of different colours overlap their
/// colours are added (up to 255), as DjVuLibre does (found on pages of real books where one pixel was different).
fn paint_palette(rgb: &mut [u8], width: usize, height: usize, drawn: &jb2::PageShapes, palette: &Palette) {
    // the number of the colour (and one) that painted each pixel last: 0 for none
    let mut owner = vec![0u16; width * height];
    for (blit, &number) in drawn.blits.iter().zip(&palette.indices) {
        let Some(shape) = drawn.shapes.get(blit.shape) else {
            continue;
        };
        if shape.width == 0 {
            continue;
        }
        let colour = palette.colours[usize::from(number)];
        for (sy, line) in shape.pixels.chunks_exact(shape.width).enumerate() {
            let y = blit.top + sy as i64;
            if y < 0 || y >= height as i64 {
                continue;
            }
            for (sx, &pixel) in line.iter().enumerate() {
                let x = blit.left + sx as i64;
                if x < 0 || x >= width as i64 || pixel == 0 {
                    continue;
                }
                let at = y as usize * width + x as usize;
                let painted = &mut owner[at];
                let out = &mut rgb[at * 3..][..3];
                if *painted == 0 {
                    out.copy_from_slice(&colour);
                } else if palette.colours[usize::from(*painted - 1)] != colour {
                    for (o, c) in out.iter_mut().zip(colour) {
                        *o = o.saturating_add(c);
                    }
                }
                *painted = number.saturating_add(1);
            }
        }
    }
}

/// The page as RGB, the top row first, the size the `INFO` chunk gives.
///
/// # Errors
/// An error of the decoder of a chunk, or [`Error::BadPage`] when the mask is not the size of the page.
pub fn render(document: &Document<'_>, entries: &[DirectoryEntry], page: &Page<'_, '_>) -> Result<Pixmap, Error> {
    let dictionary = shared_dictionary(document, entries, page)?;
    render_with(page, dictionary.as_ref())
}

/// Does two jobs at once: `first` on a thread of its own and `second` here. If the thread cannot be made, `first` is done here after
/// `second`, so there is no new way to fail; the answer of `first` is None only if its thread panicked.
fn alongside<A: Send, B>(first: impl FnOnce() -> A + Send, second: impl FnOnce() -> B) -> (Option<A>, B) {
    let job = Mutex::new(Some(first));
    let answer: Mutex<Option<A>> = Mutex::new(None);
    // whoever takes the job first does it: the thread when it starts, else this thread when it has done its own
    let run = || {
        let job = job.lock().ok().and_then(|mut slot| slot.take());
        if let Some(job) = job {
            let done = job();
            if let Ok(mut slot) = answer.lock() {
                *slot = Some(done);
            }
        }
    };
    let second = std::thread::scope(|scope| {
        let run = &run;
        let _ = std::thread::Builder::new().spawn_scoped(scope, run);
        let second = second();
        run();
        second
    });
    (answer.into_inner().ok().flatten(), second)
}

/// As [`render`], with the dictionary of shapes already decoded (see [`dictionary_chunk`]): a caller that turns the pages of one
/// document decodes it once.
///
/// # Errors
/// As [`render`].
pub fn render_with(page: &Page<'_, '_>, dictionary: Option<&jb2::Dictionary>) -> Result<Pixmap, Error> {
    let (width, height) = (usize::from(page.info.width), usize::from(page.info.height));
    if width * height > MAX_PIXELS {
        return Err(Error::BadPage("the page has more pixels than the limit"));
    }
    let colours = page.chunks(*b"FGbz").next().and_then(|chunk| palette(chunk.data));
    // the pictures of the background and the foreground, which do not depend on the mask, are made on a thread while the mask is
    let layers =
        || -> Result<(Option<Pixmap>, Option<Pixmap>), Error> { Ok((layer(page, *b"BG44")?, layer(page, *b"FG44")?)) };
    // the mask, and when the shapes have colours of their own, the shapes too
    let masks = || -> Result<(Option<jb2::Bitmap>, Option<jb2::PageShapes>), Error> {
        Ok(match (page.chunks(*b"Sjbz").next(), colours.is_some()) {
            (Some(chunk), true) => {
                let mut drawn = jb2::decode_page(chunk.data, dictionary)?;
                (Some(std::mem::take(&mut drawn.mask)), Some(drawn))
            }
            (Some(chunk), false) => (Some(jb2::decode_image(chunk.data, dictionary)?), None),
            (None, _) => (None, None),
        })
    };
    let (layers, masks) = alongside(layers, masks);
    let (background, foreground) = layers.ok_or(Error::BadPage("a thread of the page failed"))??;
    let (mask, shapes) = masks?;
    if let Some(mask) = &mask
        && (mask.width, mask.height) != (width, height)
    {
        return Err(Error::BadPage("the mask is not the size of the page"));
    }

    let mut rgb = vec![255u8; width * height * 3];
    if let Some(bg) = &background {
        fill_from(&mut rgb, width, height, bg);
    }
    if let (Some(palette), Some(drawn)) = (&colours, &shapes)
        && drawn.blits.len() == palette.indices.len()
    {
        paint_palette(&mut rgb, width, height, drawn, palette);
    } else if let Some(mask) = &mask {
        for (y, (mask_row, out_row)) in mask
            .pixels
            .chunks_exact(width.max(1))
            .zip(rgb.chunks_exact_mut((width * 3).max(1)))
            .enumerate()
        {
            // most of a row is white: eight pixels that are all white are passed over at once
            for (block, chunk) in mask_row.chunks(8).enumerate() {
                if chunk.len() == 8 && chunk.iter().fold(0u8, |or, &p| or | p) == 0 {
                    continue;
                }
                for (i, &pixel) in chunk.iter().enumerate() {
                    if pixel != 0 {
                        let x = block * 8 + i;
                        let colour = foreground
                            .as_ref()
                            .map_or([0, 0, 0], |fg| nearest(fg, x, y, width, height));
                        out_row[x * 3..][..3].copy_from_slice(&colour);
                    }
                }
            }
        }
    }
    Ok(turned(
        corrected(Pixmap { width, height, rgb }, page.info.gamma),
        page.info.rotation,
    ))
}

/// The page made directly at the size it has when it is shrunk to fit the box `max_width` x `max_height` (the box as it is shown, the
/// page turned as its `INFO` says; the same size as [`render_with`] and a shrinking to the box give, not a pixel more or less), when
/// that is at most half the size of the page: for a picture that is looked at small (a card, a panel) that is much less work than the
/// whole page and the shrinking of it. When the box is bigger than that, or the background is too small to be shrunk to the size
/// (the background is not made big again here), the whole page is made as [`render_with`] does and the caller shrinks it.
///
/// # Errors
/// As [`render`].
pub fn render_within(
    page: &Page<'_, '_>,
    dictionary: Option<&jb2::Dictionary>,
    max_width: usize,
    max_height: usize,
) -> Result<Pixmap, Error> {
    let (width, height) = (usize::from(page.info.width), usize::from(page.info.height));
    if width * height > MAX_PIXELS {
        return Err(Error::BadPage("the page has more pixels than the limit"));
    }
    // the box in the orientation the page is stored in
    let (box_w, box_h) = match page.info.rotation {
        Rotation::Ccw90 | Rotation::Cw90 => (max_height, max_width),
        Rotation::None | Rotation::Turn180 => (max_width, max_height),
    };
    if width > 0 && height > 0 && box_w > 0 && box_h > 0 {
        let scale = (box_w as f64 / width as f64).min(box_h as f64 / height as f64);
        if scale <= 0.5 {
            // the size that a shrinking to the box gives: each side by the scale, rounded, halves to even, and not less than 1
            let side = |n: usize| ((n as f64 * scale).round_ties_even() as usize).max(1);
            if let Some(small) = reduced(page, dictionary, side(width), side(height))? {
                return Ok(turned(corrected(small, page.info.gamma), page.info.rotation));
            }
        }
    }
    render_with(page, dictionary)
}

/// The size of the picture that the first chunk of the layer `id` gives (the header of the chunk), without decoding the picture.
fn layer_size(page: &Page<'_, '_>, id: [u8; 4]) -> Option<(usize, usize)> {
    let header = page.chunks(id).next()?.data;
    let wh = header.get(4..8)?;
    Some((
        usize::from(u16::from_be_bytes([wh[0], wh[1]])),
        usize::from(u16::from_be_bytes([wh[2], wh[3]])),
    ))
}

/// The page as `out_w` x `out_h` pixels (each at most the size of the page): each pixel the mean of the pixels of the page that it
/// covers, the pixels of the page being given to the small ones in rows and columns of the same widths as near as can be. None
/// when the background is smaller than that.
fn reduced(
    page: &Page<'_, '_>,
    dictionary: Option<&jb2::Dictionary>,
    out_w: usize,
    out_h: usize,
) -> Result<Option<Pixmap>, Error> {
    let (width, height) = (usize::from(page.info.width), usize::from(page.info.height));
    if let Some((bg_w, bg_h)) = layer_size(page, *b"BG44")
        && (out_w > bg_w || out_h > bg_h)
    {
        return Ok(None);
    }
    // the background (its own picture shrunk to the size) and the foreground picture are made on a thread while the shapes are
    let layers = || -> Result<(Option<Vec<u8>>, Option<Pixmap>), Error> {
        let rgb = match layer(page, *b"BG44")? {
            Some(bg) if out_w <= bg.width && out_h <= bg.height => Some(shrunk(&bg, out_w, out_h)),
            Some(_) => None,
            None => Some(vec![255u8; out_w * out_h * 3]),
        };
        Ok((rgb, layer(page, *b"FG44")?))
    };
    let shapes = || -> Result<Option<jb2::PageShapes>, Error> {
        page.chunks(*b"Sjbz")
            .next()
            .map(|chunk| jb2::decode_shapes(chunk.data, dictionary))
            .transpose()
    };
    let (layers, drawn) = alongside(layers, shapes);
    let (rgb, foreground) = layers.ok_or(Error::BadPage("a thread of the page failed"))??;
    let drawn = drawn?;
    let Some(mut rgb) = rgb else {
        return Ok(None);
    };
    let colours = page.chunks(*b"FGbz").next().and_then(|chunk| palette(chunk.data));
    let Some(drawn) = drawn else {
        return Ok(Some(Pixmap {
            width: out_w,
            height: out_h,
            rgb,
        }));
    };
    if (drawn.mask.width, drawn.mask.height) != (width, height) {
        return Err(Error::BadPage("the mask is not the size of the page"));
    }
    // the small pixel that each column and each row of the page falls in, and how many columns and rows each small pixel has
    let column: Vec<u32> = (0..width).map(|x| (x * out_w / width) as u32).collect();
    let row_of: Vec<u32> = (0..height).map(|y| (y * out_h / height) as u32).collect();
    let (mut columns_in, mut rows_in) = (vec![0u32; out_w], vec![0u32; out_h]);
    for &u in &column {
        columns_in[u as usize] += 1;
    }
    for &v in &row_of {
        rows_in[v as usize] += 1;
    }
    // how many pixels of the page that are black fall in each pixel of the small picture, and the sum of their colours
    let mut covered = vec![0u32; out_w * out_h];
    let mut sums = vec![0u32; out_w * out_h * 3];
    let palette = colours.as_ref().filter(|p| p.indices.len() == drawn.blits.len());
    for (n, blit) in drawn.blits.iter().enumerate() {
        let Some(shape) = drawn.shape(dictionary, blit.shape) else {
            continue;
        };
        if shape.width == 0 {
            continue;
        }
        let colour = palette.map(|p| p.colours[usize::from(p.indices[n])]);
        for (sy, line) in shape.pixels.chunks_exact(shape.width).enumerate() {
            let y = blit.top + sy as i64;
            if y < 0 || y >= height as i64 {
                continue;
            }
            let v = row_of[y as usize] as usize;
            for (sx, &pixel) in line.iter().enumerate() {
                let x = blit.left + sx as i64;
                if pixel == 0 || x < 0 || x >= width as i64 {
                    continue;
                }
                let at = v * out_w + column[x as usize] as usize;
                covered[at] += 1;
                if let Some(c) = colour {
                    for (sum, value) in sums[at * 3..][..3].iter_mut().zip(c) {
                        *sum += u32::from(value);
                    }
                }
            }
        }
    }
    // the colour of the black pixels of a small pixel when the shapes have none of their own: the foreground picture, or black
    if palette.is_none() {
        for v in 0..out_h {
            for u in 0..out_w {
                let at = v * out_w + u;
                if covered[at] == 0 {
                    continue;
                }
                let (x, y) = (
                    ((2 * u + 1) * width / (2 * out_w)).min(width - 1),
                    ((2 * v + 1) * height / (2 * out_h)).min(height - 1),
                );
                let colour = foreground
                    .as_ref()
                    .map_or([0, 0, 0], |fg| nearest(fg, x, y, width, height));
                for (sum, value) in sums[at * 3..][..3].iter_mut().zip(colour) {
                    *sum = covered[at] * u32::from(value);
                }
            }
        }
    }
    // the picture: the background where the pixels of the page are white and the colours where they are black, by how many are each
    for (v, &rows_here) in rows_in.iter().enumerate() {
        for (u, &columns_here) in columns_in.iter().enumerate() {
            let at = v * out_w + u;
            if covered[at] == 0 {
                continue;
            }
            let area = (columns_here * rows_here).max(1);
            let black = covered[at].min(area);
            for c in 0..3 {
                let value = u32::from(rgb[at * 3 + c]) * (area - black) + sums[at * 3 + c].min(black * 255);
                rgb[at * 3 + c] = ((value + area / 2) / area) as u8;
            }
        }
    }
    Ok(Some(Pixmap {
        width: out_w,
        height: out_h,
        rgb,
    }))
}

/// A picture made smaller to `out_w` x `out_h` (not bigger than it is) by the mean of the pixels each pixel of the small one covers.
fn shrunk(picture: &Pixmap, out_w: usize, out_h: usize) -> Vec<u8> {
    if (picture.width, picture.height) == (out_w, out_h) {
        return picture.rgb.clone();
    }
    // the pixels of the picture that the pixel `i` of `out` covers, `from` being the size of the picture along that side: from
    // `i * from / out` to where the next one begins, and at least one
    let span = |i: usize, from: usize, out: usize| {
        let first = i * from / out;
        (first, ((i + 1) * from / out).max(first + 1).min(from))
    };
    // along the rows first: for each row of the picture the sums over the columns that each pixel of the small one covers
    let mut rows = vec![0u32; picture.height * out_w * 3];
    for (y, row) in picture.rgb.chunks_exact(picture.width * 3).enumerate() {
        for u in 0..out_w {
            let (from, to) = span(u, picture.width, out_w);
            for x in from..to {
                for c in 0..3 {
                    rows[(y * out_w + u) * 3 + c] += u32::from(row[x * 3 + c]);
                }
            }
        }
    }
    let mut out = vec![0u8; out_w * out_h * 3];
    for v in 0..out_h {
        let (y_from, y_to) = span(v, picture.height, out_h);
        for u in 0..out_w {
            let (x_from, x_to) = span(u, picture.width, out_w);
            let count = ((y_to - y_from) * (x_to - x_from)) as u32;
            for c in 0..3 {
                let sum: u32 = (y_from..y_to).map(|y| rows[(y * out_w + u) * 3 + c]).sum();
                out[(v * out_w + u) * 3 + c] = ((sum + count / 2) / count) as u8;
            }
        }
    }
    out
}

/// The gamma that the pictures of a page are shown for: the screen that DjVuLibre's ddjvu takes by default, and the one of a viewer in a
/// terminal. A page made for the same gamma is shown as it is.
const DISPLAY_GAMMA: f32 = 2.2;

/// The lowest and the highest gamma of a page that is taken as it is: the `INFO` byte is in tenths and a page that says less or more is
/// shown for the nearest of these.
const MIN_GAMMA: f32 = 0.3;
const MAX_GAMMA: f32 = 5.0;

/// The page with its values changed for the display when the gamma it was made for is not the display's (`INFO` byte 8, tenths): each
/// value `v` of 0 to 255 becomes `255 * (v / 255) ^ (page gamma / display gamma)`, rounded (the exponent is the inverse of the
/// correction `display gamma / page gamma` that DjVuLibre makes), and 0 and 255 stay. A page whose gamma is within a thousandth of
/// the display's is not changed. Found with ddjvu on a page of a real document with other values of the gamma in `INFO`: see
/// docs/internal/djvu.md.
fn corrected(mut page: Pixmap, page_gamma: f32) -> Pixmap {
    // a gamma outside 0.3 to 5.0 (and 0, which is not given) is taken as the nearest end of that, as ddjvu takes it
    let page_gamma = page_gamma.clamp(MIN_GAMMA, MAX_GAMMA);
    let correction = DISPLAY_GAMMA / page_gamma;
    if (0.999..=1.001).contains(&correction) {
        return page;
    }
    let mut table = [0u8; 256];
    for (value, entry) in table.iter_mut().enumerate() {
        *entry = match value {
            0 => 0,
            255 => 255,
            _ => (255.0 * (value as f64 / 255.0).powf(1.0 / f64::from(correction)) + 0.5).floor() as u8,
        };
    }
    for value in &mut page.rgb {
        *value = table[usize::from(*value)];
    }
    page
}

/// The page turned the way its `INFO` says (spec 8.3.11): the size in `INFO` is the size of the page as it is stored, and the picture
/// that is shown is that turned, so for a quarter turn the width and the height change places. Found with ddjvu (a page of a real
/// document with each value of the flags): see docs/internal/djvu.md.
fn turned(page: Pixmap, rotation: Rotation) -> Pixmap {
    let (w, h) = (page.width, page.height);
    let (width, height) = match rotation {
        Rotation::None | Rotation::Turn180 => (w, h),
        Rotation::Ccw90 | Rotation::Cw90 => (h, w),
    };
    if rotation == Rotation::None || w == 0 || h == 0 {
        return page;
    }
    let mut rgb = vec![0u8; page.rgb.len()];
    for (y, row) in page.rgb.chunks_exact(w * 3).enumerate() {
        for (x, pixel) in row.as_chunks::<3>().0.iter().enumerate() {
            // where the pixel (x, y) of the stored page is in the picture that is shown
            let (to_x, to_y) = match rotation {
                Rotation::Turn180 => (w - 1 - x, h - 1 - y),
                Rotation::Ccw90 => (y, w - 1 - x),
                Rotation::Cw90 => (h - 1 - y, x),
                Rotation::None => (x, y),
            };
            rgb[(to_y * width + to_x) * 3..][..3].copy_from_slice(pixel);
        }
    }
    Pixmap { width, height, rgb }
}

/// By how much a picture of `small` pixels was made smaller than a page of `page`: the whole number `k`, 1 to 12, for which the
/// small picture is `page / k` rounded up in both directions (spec 7.1.3.1, 7.1.3.2). None when there is no such number.
fn reduction(small: &Pixmap, width: usize, height: usize) -> Option<usize> {
    (1..=12).find(|&k| width.div_ceil(k) == small.width && height.div_ceil(k) == small.height)
}

/// Makes the background as big as the page. The page is cut into bands of rows that the threads there are take from a shared list
/// (a picture of a million pixels or more pays for them); this thread takes from it too, so a thread that cannot be made only means
/// that this one does more.
fn fill_from(rgb: &mut [u8], width: usize, height: usize, small: &Pixmap) {
    if small.width == 0 || small.height == 0 || width == 0 || height == 0 {
        return;
    }
    let Some(k) = reduction(small, width, height) else {
        // not a whole reduction of the page (a file that is not made as the spec says): the nearest pixel
        for y in 0..height {
            for x in 0..width {
                let sx = (x * small.width / width).min(small.width - 1);
                let sy = (y * small.height / height).min(small.height - 1);
                let at = (sy * small.width + sx) * 3;
                rgb[(y * width + x) * 3..][..3].copy_from_slice(&small.rgb[at..at + 3]);
            }
        }
        return;
    };
    let up = Upscale::new(small, k, width, height);
    let threads = if width * height >= 1 << 20 {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
    } else {
        1
    };
    let rows_per_band = (height / (threads * 4)).max(16);
    let bands: Mutex<Vec<(usize, &mut [u8])>> = Mutex::new(
        rgb.chunks_mut(rows_per_band * width * 3)
            .enumerate()
            .map(|(band, out)| (band * rows_per_band, out))
            .collect(),
    );
    let work = || {
        loop {
            let next = bands.lock().ok().and_then(|mut list| list.pop());
            match next {
                Some((first, out)) => up.fill_rows(out, width, first),
                None => break,
            }
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            let _ = std::thread::Builder::new().spawn_scoped(scope, work);
        }
        work();
    });
}

/// The background of the page alone, as big as the page (white when the page has none).
///
/// # Errors
/// An error of the decoder of the chunks.
pub fn background(page: &Page<'_, '_>) -> Result<Pixmap, Error> {
    let (width, height) = (usize::from(page.info.width), usize::from(page.info.height));
    if width * height > MAX_PIXELS {
        return Err(Error::BadPage("the page has more pixels than the limit"));
    }
    let mut rgb = vec![255u8; width * height * 3];
    if let Some(bg) = layer(page, *b"BG44")? {
        fill_from(&mut rgb, width, height, &bg);
    }
    Ok(Pixmap { width, height, rgb })
}

/// How a smaller picture is made as big as the page: each pixel of the page looks at the pixels of the small picture round the
/// place where its centre falls, and takes a mixture of two by two of them, the shares being counted in sixteenths. Found by
/// comparing with ddjvu (a step and an impulse of known size, then real pages), see docs/internal/djvu.md.
struct Upscale<'a> {
    small: &'a Pixmap,
    /// For each column of the page, where its centre falls among the columns of the small picture, in sixteenths of a column.
    xs: Vec<i32>,
    ys: Vec<i32>,
}

/// For each of `count` pixels of the page, the place of its centre among the pixels of a picture of `size` that was made `k` times
/// smaller than the page, in sixteenths of a pixel (the middle of pixel 0 is 0), kept inside the picture. The picture is stretched by
/// exactly `k` (not by the ratio of the two sizes, which differs a little when the page is not a multiple of `k`); a place is the
/// nearest sixteenth, a half going up.
fn places(size: usize, k: usize, count: usize) -> Vec<i32> {
    let (k, limit) = (k as i64, ((size - 1) * 16) as i64);
    let begin = (16 + k) / (2 * k) - 8;
    (0..count as i64)
        .map(|x| (begin + (x * 16 + k / 2) / k).clamp(0, limit) as i32)
        .collect()
}

impl<'a> Upscale<'a> {
    fn new(small: &'a Pixmap, k: usize, width: usize, height: usize) -> Self {
        // the rows are counted from the bottom of the picture, as DjVu does (the shares come out the other way round otherwise)
        let mut ys = places(small.height, k, height);
        ys.reverse();
        Self {
            small,
            xs: places(small.width, k, width),
            ys,
        }
    }

    /// The rows of the page from `first` on, as many as `out` holds (`width * 3` bytes each). The two small rows that a row of the
    /// page looks at are mixed first (once for each place they are looked at from), then, along that, the two columns.
    fn fill_rows(&self, out: &mut [u8], width: usize, first: usize) {
        let small = self.small;
        let stride = small.width * 3;
        let mut line = vec![0u8; stride];
        let mut made_for = i32::MIN;
        for (row, dst) in out.chunks_exact_mut(width * 3).enumerate() {
            let place = self.ys[first + row];
            if place != made_for {
                let (y0, share) = (small.height - 1 - (place >> 4) as usize, place & 15);
                let y1 = y0.saturating_sub(1);
                let (below, above) = (&small.rgb[y0 * stride..][..stride], &small.rgb[y1 * stride..][..stride]);
                for ((l, &a), &b) in line.iter_mut().zip(below).zip(above) {
                    *l = mix(a, b, share);
                }
                made_for = place;
            }
            for (pixel, &place) in dst.as_chunks_mut::<3>().0.iter_mut().zip(&self.xs) {
                let (x0, share) = ((place >> 4) as usize, place & 15);
                let x1 = (x0 + 1).min(small.width - 1);
                for (c, value) in pixel.iter_mut().enumerate() {
                    *value = mix(line[x0 * 3 + c], line[x1 * 3 + c], share);
                }
            }
        }
    }
}

/// One mix of two samples: `share` sixteenths of the way from `a` to `b`, to the nearest, a half going up.
fn mix(a: u8, b: u8, share: i32) -> u8 {
    let (a, d) = (i32::from(a), i32::from(b) - i32::from(a));
    (a + ((share * d + 8) >> 4)) as u8
}
/// The pixel of a smaller picture that covers the pixel (x, y) of the page, by repeating each pixel of the small one (the foreground
/// colours are not mixed).
fn nearest(small: &Pixmap, x: usize, y: usize, width: usize, height: usize) -> [u8; 3] {
    let (sx, sy) = match reduction(small, width, height) {
        // the blocks of k rows are counted from the bottom of the page, as DjVu keeps its rows; the top block is the short one
        Some(k) => (x / k, small.height - 1 - (height - 1 - y) / k),
        None => (x * small.width / width, y * small.height / height),
    };
    let at = (sy.min(small.height - 1) * small.width + sx.min(small.width - 1)) * 3;
    [small.rgb[at], small.rgb[at + 1], small.rgb[at + 2]]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // 24 x 24 grey pictures that the IW44 encoder of djvu-rs (MIT) made, and the hash (FNV) of what DjVuLibre's ddjvu made of the
    // page 3 (RAMP) and 5 (VRAMP) times as big, in background mode.
    const RAMP_0: [u8; 23] = [
        0, 10, 129, 2, 0, 24, 0, 24, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const RAMP_1: [u8; 16] = [
        1, 10, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const RAMP_2: [u8; 16] = [
        2, 10, 20, 63, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const RAMP_3: [u8; 16] = [
        3, 10, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const RAMP_4: [u8; 16] = [4, 10, 10, 62, 39, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255];
    const RAMP_5: [u8; 16] = [5, 10, 129, 246, 36, 90, 12, 3, 255, 255, 255, 255, 255, 255, 255, 255];
    const RAMP_6: [u8; 16] = [6, 10, 36, 232, 167, 53, 28, 145, 208, 188, 170, 17, 160, 144, 127, 255];
    const RAMP_7: [u8; 10] = [7, 4, 0, 249, 146, 223, 255, 255, 255, 255];
    const RAMP_HASH: u64 = 0xb7d77f5df56b995f;
    const VRAMP_0: [u8; 23] = [
        0, 10, 129, 2, 0, 24, 0, 24, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_1: [u8; 16] = [
        1, 10, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_2: [u8; 16] = [
        2, 10, 23, 223, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_3: [u8; 16] = [
        3, 10, 31, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_4: [u8; 16] = [
        4, 10, 3, 191, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_5: [u8; 16] = [
        5, 10, 63, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_6: [u8; 16] = [
        6, 10, 168, 103, 135, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
    const VRAMP_7: [u8; 10] = [7, 4, 201, 255, 255, 255, 255, 255, 255, 255];
    const VRAMP_HASH: u64 = 0xd52fb90dafcfa3f5;

    fn fnv(data: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    fn chunk(id: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    }

    /// A one-page document of `width` x `height` made of the given chunks.
    fn file(width: u16, height: u16, chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"DJVU".to_vec();
        let mut info = Vec::new();
        info.extend_from_slice(&width.to_be_bytes());
        info.extend_from_slice(&height.to_be_bytes());
        info.extend_from_slice(&[24, 0, 44, 1, 22, 1]);
        chunk(b"INFO", &info, &mut body);
        for (id, data) in chunks {
            chunk(id, data, &mut body);
        }
        let mut out = b"AT&T".to_vec();
        out.extend_from_slice(b"FORM");
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn rendered(file: &[u8]) -> Result<Pixmap, Error> {
        let document = Document::parse(file)?;
        let entries = Vec::new();
        render(&document, &entries, &document.page(0)?)
    }

    #[test]
    fn a_small_background_is_made_big_as_ddjvu_makes_it() {
        // 24 x 24 grey pictures (made by the encoder of djvu-rs), the page 3 and 5 times as big: the pixels are the ones ddjvu made
        let ramp: Vec<&[u8]> = vec![&RAMP_0, &RAMP_1, &RAMP_2, &RAMP_3, &RAMP_4, &RAMP_5, &RAMP_6, &RAMP_7];
        let chunks: Vec<(&[u8; 4], &[u8])> = ramp.iter().map(|c| (b"BG44", *c)).collect();
        let page = rendered(&file(72, 72, &chunks)).unwrap();
        assert_eq!(fnv(&page.rgb), RAMP_HASH);
        let vramp: Vec<&[u8]> = vec![
            &VRAMP_0, &VRAMP_1, &VRAMP_2, &VRAMP_3, &VRAMP_4, &VRAMP_5, &VRAMP_6, &VRAMP_7,
        ];
        let chunks: Vec<(&[u8; 4], &[u8])> = vramp.iter().map(|c| (b"BG44", *c)).collect();
        let page = rendered(&file(120, 120, &chunks)).unwrap();
        assert_eq!(fnv(&page.rgb), VRAMP_HASH);
    }

    #[test]
    fn the_places_of_the_pixels_are_sixteenths_counted_from_the_middle_of_a_pixel() {
        // made three times as big: the middle of the first big pixel is a third of a small one before the middle of the first
        assert_eq!(places(4, 3, 9), vec![0, 0, 6, 11, 16, 22, 27, 32, 38]);
        // twice as big: a quarter of a small pixel before and after the middle
        assert_eq!(places(3, 2, 6), vec![0, 4, 12, 20, 28, 32]);
        // no change: every pixel on its own
        assert_eq!(places(3, 1, 3), vec![0, 16, 32]);
    }

    #[test]
    fn a_reduction_is_a_whole_number_that_gives_the_size_of_the_picture() {
        let small = |w, h| Pixmap {
            width: w,
            height: h,
            rgb: vec![0; w * h * 3],
        };
        assert_eq!(reduction(&small(1164, 1705), 3490, 5114), Some(3));
        assert_eq!(reduction(&small(291, 427), 3490, 5114), Some(12));
        assert_eq!(reduction(&small(3490, 5114), 3490, 5114), Some(1));
        assert_eq!(reduction(&small(100, 100), 3490, 5114), None);
    }

    #[test]
    fn the_blocks_of_a_reduced_picture_are_counted_from_the_bottom_of_the_page() {
        // three rows of colour for a page 25 rows high (made 9 times smaller, the least whole number that gives 3 rows): the blocks of 9
        // rows start at the bottom, so the top block has 7
        let mut small = Pixmap {
            width: 1,
            height: 3,
            rgb: vec![0; 9],
        };
        for (row, value) in [10u8, 20, 30].into_iter().enumerate() {
            small.rgb[row * 3..row * 3 + 3].fill(value);
        }
        let at = |y| nearest(&small, 0, y, 1, 25)[0];
        assert_eq!([at(0), at(6), at(7), at(15), at(16), at(24)], [10, 10, 20, 20, 30, 30]);
    }

    #[test]
    fn a_page_that_claims_too_many_pixels_is_refused_before_anything_is_made() {
        assert!(matches!(rendered(&file(65535, 65535, &[])), Err(Error::BadPage(_))));
        assert!(matches!(rendered(&file(10_001, 10_000, &[])), Err(Error::BadPage(_))));
    }

    fn pixmap_3x2() -> Pixmap {
        // rows of three pixels, the value of a pixel in all three channels:   1 2 3 / 4 5 6
        let mut rgb = Vec::new();
        for v in 1..=6u8 {
            rgb.extend_from_slice(&[v, v, v]);
        }
        Pixmap {
            width: 3,
            height: 2,
            rgb,
        }
    }

    fn values(p: &Pixmap) -> Vec<u8> {
        p.rgb.as_chunks::<3>().0.iter().map(|c| c[0]).collect()
    }

    #[test]
    fn a_page_is_turned_as_the_flags_say_and_a_quarter_turn_changes_width_and_height() {
        let page = pixmap_3x2();
        let upside_down = turned(page.clone(), Rotation::Turn180);
        assert_eq!(
            (upside_down.width, upside_down.height, values(&upside_down)),
            (3, 2, vec![6, 5, 4, 3, 2, 1])
        );
        // 90 degrees counter-clockwise: the right edge becomes the top
        let ccw = turned(page.clone(), Rotation::Ccw90);
        assert_eq!((ccw.width, ccw.height, values(&ccw)), (2, 3, vec![3, 6, 2, 5, 1, 4]));
        // 90 degrees clockwise: the left edge becomes the top
        let cw = turned(page.clone(), Rotation::Cw90);
        assert_eq!((cw.width, cw.height, values(&cw)), (2, 3, vec![4, 1, 5, 2, 6, 3]));
        assert_eq!(turned(page.clone(), Rotation::None), page);
        // four quarter turns are no turn
        let round = (0..4).fold(page.clone(), |p, _| turned(p, Rotation::Cw90));
        assert_eq!(round, page);
    }

    #[test]
    fn the_rotation_of_the_info_chunk_is_applied_to_the_page() {
        // the flags are the byte 9 of the INFO data, which begins at byte 24 of the file
        let mut bytes = file(5, 4, &[]);
        bytes[24 + 9] = 6;
        let page = rendered(&bytes).unwrap();
        assert_eq!((page.width, page.height), (4, 5));
        bytes[24 + 9] = 2;
        let page = rendered(&bytes).unwrap();
        assert_eq!((page.width, page.height), (5, 4));
    }

    #[test]
    fn a_page_with_nothing_on_it_is_white_and_the_mask_alone_is_black_on_white() {
        let page = rendered(&file(5, 4, &[])).unwrap();
        assert_eq!((page.width, page.height), (5, 4));
        assert!(page.rgb.iter().all(|&v| v == 255));
    }

    #[test]
    fn the_colours_of_the_shapes_are_read_and_a_chunk_that_is_wrong_is_none() {
        // version 128 (indices follow), two colours (blue, green, red in the file), no shapes
        let mut data = vec![0x80, 0, 2, 1, 2, 3, 4, 5, 6, 0, 0, 0];
        let colours = palette(&data).map(|p| p.colours);
        assert_eq!(colours, Some(vec![[3, 2, 1], [6, 5, 4]]));
        data[0] = 0x00; // no indices: the shapes cannot be coloured one by one
        assert!(palette(&data).is_none());
        data[0] = 0x81; // a version that does not exist
        assert!(palette(&data).is_none());
        assert!(palette(&[]).is_none());
        assert!(palette(&[0x80, 0xFF, 0xFF, 1, 2]).is_none());
    }

    #[test]
    fn data_of_any_kind_is_an_error_or_a_page_and_never_a_panic() {
        let ramp: Vec<&[u8]> = vec![&RAMP_0, &RAMP_1, &RAMP_2, &RAMP_3];
        let chunks: Vec<(&[u8; 4], &[u8])> = ramp.iter().map(|c| (b"BG44", *c)).collect();
        let good = file(72, 72, &chunks);
        // the file cut short at every length, and a byte changed at every place
        for len in 0..good.len() {
            let _ = rendered(&good[..len]);
        }
        for at in (0..good.len()).step_by(3) {
            let mut bad = good.clone();
            bad[at] ^= 0xA5;
            let _ = rendered(&bad);
        }
        // a mask that is noise, a palette that is noise
        let mut seed = 7u64;
        for _ in 0..200 {
            let noise: Vec<u8> = (0..64)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 24) as u8
                })
                .collect();
            let _ = rendered(&file(30, 20, &[(b"Sjbz", &noise), (b"FGbz", &noise)]));
            let _ = palette(&noise);
        }
    }

    fn within(file: &[u8], max_width: usize, max_height: usize) -> Result<Pixmap, Error> {
        let document = Document::parse(file)?;
        render_within(&document.page(0)?, None, max_width, max_height)
    }

    /// The pages made of the first chunks of the ramp picture (a 24 x 24 grey background) as a page of 72 x 72.
    fn ramp_page() -> Vec<u8> {
        let chunks: Vec<(&[u8; 4], &[u8])> = [
            &RAMP_0[..],
            &RAMP_1,
            &RAMP_2,
            &RAMP_3,
            &RAMP_4,
            &RAMP_5,
            &RAMP_6,
            &RAMP_7,
        ]
        .iter()
        .map(|c| (b"BG44", *c))
        .collect();
        file(72, 72, &chunks)
    }

    #[test]
    fn a_page_for_a_small_box_is_made_small_at_the_size_a_shrinking_gives() {
        let bytes = ramp_page();
        // the box is a quarter of the page along each side: made directly at 18 x 18, the background shrunk to it
        let small = within(&bytes, 18, 18).unwrap();
        assert_eq!((small.width, small.height), (18, 18));
        // the box is not the same shape as the page: the size is what a shrinking that keeps the proportions gives
        let small = within(&bytes, 20, 10).unwrap();
        assert_eq!((small.width, small.height), (10, 10));
        // it is the whole page made and then shrunk, give or take the way the shrinking is done
        let whole = rendered(&bytes).unwrap();
        let small = within(&bytes, 18, 18).unwrap();
        let mean_of_block = |x: usize, y: usize| -> i32 {
            let mut sum = 0;
            for dy in 0..4 {
                for dx in 0..4 {
                    sum += i32::from(whole.rgb[((y * 4 + dy) * 72 + x * 4 + dx) * 3]);
                }
            }
            sum / 16
        };
        for (x, y) in [(2, 2), (9, 9), (15, 4), (3, 14)] {
            let ours = i32::from(small.rgb[(y * 18 + x) * 3]);
            assert!(
                (ours - mean_of_block(x, y)).abs() <= 12,
                "({x}, {y}): {ours} against {}",
                mean_of_block(x, y)
            );
        }
    }

    #[test]
    fn a_box_that_is_not_much_smaller_than_the_page_or_a_background_too_small_for_it_makes_the_whole_page() {
        let bytes = ramp_page();
        // 60 x 60 of 72: not at most half; 30 x 30 would need a background of 30 pixels and it has 24
        for room in [(60, 60), (30, 30)] {
            let page = within(&bytes, room.0, room.1).unwrap();
            assert_eq!((page.width, page.height), (72, 72), "{room:?}");
        }
        // a box that has no area is not a reason to fail
        assert!(within(&bytes, 0, 0).is_ok());
    }

    #[test]
    fn a_turned_page_is_made_small_in_the_box_as_it_is_shown() {
        let mut bytes = ramp_page();
        bytes[24 + 9] = 6; // turned a quarter: shown 72 x 72 still; make the page 72 x 48 by its INFO to tell the sides apart
        bytes[24 + 2..24 + 4].copy_from_slice(&48u16.to_be_bytes());
        // the page is stored 72 x 48 and shown 48 x 72; a box of 12 x 18 is a quarter of it
        let small = within(&bytes, 12, 18).unwrap();
        assert_eq!((small.width, small.height), (12, 18));
    }

    #[test]
    fn data_for_a_small_picture_is_an_error_or_a_picture_and_never_a_panic() {
        let good = ramp_page();
        for len in (0..good.len()).step_by(2) {
            let _ = within(&good[..len], 10, 10);
        }
        for at in (0..good.len()).step_by(3) {
            let mut bad = good.clone();
            bad[at] ^= 0xA5;
            let _ = within(&bad, 10, 10);
        }
        let mut seed = 11u64;
        for _ in 0..100 {
            let noise: Vec<u8> = (0..80)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 24) as u8
                })
                .collect();
            let _ = within(
                &file(40, 30, &[(b"Sjbz", &noise), (b"FGbz", &noise), (b"BG44", &noise)]),
                8,
                6,
            );
        }
    }

    #[test]
    fn a_page_made_for_another_gamma_is_changed_as_ddjvu_changes_it() {
        // the ramp page of 72 x 72 with the gamma byte of INFO (byte 8 of its data, at 24 in the file) set to 10 and to 30: the hashes
        // of what ddjvu made of them
        let hashes = [(10u8, 0x798d_348d_91ce_0429u64), (30, 0x3cc1_acb9_db23_2c8b)];
        for (byte, hash) in hashes {
            let mut bytes = ramp_page();
            bytes[24 + 8] = byte;
            assert_eq!(fnv(&rendered(&bytes).unwrap().rgb), hash, "gamma byte {byte}");
        }
    }

    #[test]
    fn the_gamma_of_the_display_changes_nothing_and_one_out_of_range_is_taken_as_the_nearest_end() {
        let page = pixmap_3x2();
        assert_eq!(corrected(page.clone(), 2.2), page);
        // 0 (not given) and anything below 0.3 are 0.3; above 5.0 is 5.0
        assert_eq!(corrected(page.clone(), 0.0), corrected(page.clone(), 0.3));
        assert_eq!(corrected(page.clone(), 0.1), corrected(page.clone(), 0.3));
        assert_eq!(corrected(page.clone(), 25.5), corrected(page.clone(), 5.0));
        // black and white stay, whatever the gamma
        let black_and_white = Pixmap {
            width: 2,
            height: 1,
            rgb: vec![0, 0, 0, 255, 255, 255],
        };
        for gamma in [0.3, 1.0, 1.8, 3.0, 5.0] {
            assert_eq!(corrected(black_and_white.clone(), gamma), black_and_white);
        }
    }

    fn block(width: usize, height: usize) -> jb2::Bitmap {
        jb2::Bitmap {
            width,
            height,
            pixels: vec![1; width * height],
        }
    }

    #[test]
    fn shapes_of_the_same_colour_that_overlap_keep_it_and_of_different_colours_add_their_colours() {
        // a white page of 6 x 1 and three shapes of 3 x 1: red at columns 0 to 2, red at 2 to 4 and green at 3 to 5
        let mut rgb = vec![255u8; 6 * 3];
        let drawn = jb2::PageShapes {
            mask: jb2::Bitmap::default(),
            blits: vec![
                jb2::Blit {
                    left: 0,
                    top: 0,
                    shape: 0,
                },
                jb2::Blit {
                    left: 2,
                    top: 0,
                    shape: 0,
                },
                jb2::Blit {
                    left: 3,
                    top: 0,
                    shape: 0,
                },
            ],
            shapes: vec![block(3, 1)],
            inherited: 0,
        };
        let palette = Palette {
            colours: vec![[200, 10, 20], [30, 220, 40]],
            indices: vec![0, 0, 1],
        };
        paint_palette(&mut rgb, 6, 1, &drawn, &palette);
        let pixel = |x: usize| [rgb[x * 3], rgb[x * 3 + 1], rgb[x * 3 + 2]];
        assert_eq!(pixel(0), [200, 10, 20], "red alone");
        assert_eq!(
            pixel(2),
            [200, 10, 20],
            "red over red stays red, and the white behind is gone"
        );
        assert_eq!(
            pixel(3),
            [230, 230, 60],
            "red and green add: 200 + 30, 10 + 220, 20 + 40"
        );
        assert_eq!(pixel(5), [30, 220, 40], "the green where the second red has ended");
    }

    #[test]
    fn colours_that_add_up_to_more_than_255_stop_at_255() {
        let mut rgb = vec![255u8; 3];
        let drawn = jb2::PageShapes {
            mask: jb2::Bitmap::default(),
            blits: vec![
                jb2::Blit {
                    left: 0,
                    top: 0,
                    shape: 0,
                },
                jb2::Blit {
                    left: 0,
                    top: 0,
                    shape: 0,
                },
            ],
            shapes: vec![block(1, 1)],
            inherited: 0,
        };
        let palette = Palette {
            colours: vec![[200, 100, 0], [100, 200, 0]],
            indices: vec![0, 1],
        };
        paint_palette(&mut rgb, 1, 1, &drawn, &palette);
        assert_eq!(rgb, vec![255, 255, 0]);
    }
}
