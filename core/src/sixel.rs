//! Sixel output: six rows of pixels per character, one pass per colour (DEC VT340, as xterm and Windows Terminal
//! 1.22+ read it).
//!
//! Ported from BlueEdit (`Sixel.EncodeIndexed` in `src/Tui/CellPicture.cs`); the bytes are meant to be the same.

use crate::quantize::{BACKGROUND, Indexed, quantize};

/// Ends a Sixel stream: ST.
pub const END: &str = "\x1b\\";

/// A picture (`0xAARRGGBB`, `width` to a row) as a Sixel stream; a transparent pixel is painted `background`
/// (`0x00RRGGBB`), so whatever was under the picture is covered.
pub fn encode(argb: &[u32], width: usize, background: u32) -> String {
    let Indexed { index, palette } = quantize(argb, width);
    let height = argb.len().checked_div(width).unwrap_or(0);
    encode_indexed(&index, &palette, width, 0, height, background)
}

/// Rows `top..top + height` of a picture in palette indexes, every pixel painted ([`BACKGROUND`] in `bg`). One pass
/// per colour a band uses, the band's pixels sorted to their colours first, so a photo's 200 colours cost no more
/// than its width per colour.
pub fn encode_indexed(index: &[u8], palette: &[u32], width: usize, top: usize, height: usize, bg: u32) -> String {
    let threads = if width * height < PARALLEL_FROM {
        1
    } else {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
    };
    encode_on(index, palette, width, top, height, bg, threads)
}

/// [`encode_indexed`] on `threads` threads; the bytes do not depend on the number.
pub fn encode_on(
    index: &[u8],
    palette: &[u32],
    width: usize,
    top: usize,
    height: usize,
    bg: u32,
    threads: usize,
) -> String {
    let mut sb = String::with_capacity(16384);
    // P2 = 1: pixels not painted stay as they are; "1;1 = square pixels, then the size
    sb.push_str("\x1bP0;1;0q\"1;1;");
    push_number(&mut sb, width);
    sb.push(';');
    push_number(&mut sb, height);

    let mut defined = [false; 256];
    for &c in &index[top * width..(top + height) * width] {
        defined[usize::from(c)] = true;
    }
    for (c, _) in defined.iter().enumerate().filter(|(_, d)| **d) {
        let rgb = if c == usize::from(BACKGROUND) {
            bg
        } else {
            palette.get(c).copied().unwrap_or(0)
        };
        sb.push('#');
        push_number(&mut sb, c);
        sb.push_str(";2;");
        push_number(&mut sb, percent(rgb >> 16));
        sb.push(';');
        push_number(&mut sb, percent((rgb >> 8) & 255));
        sb.push(';');
        push_number(&mut sb, percent(rgb & 255));
    }

    // Bands do not depend on each other, so a big picture is cut into runs of bands, one thread to a run, and the
    // pieces are joined in order: the same bytes as one thread makes.
    let bands = height.div_ceil(6);
    let threads = threads.min(bands);
    if threads <= 1 {
        encode_bands(&mut sb, index, width, top, height, 0, bands);
    } else {
        let per_thread = bands.div_ceil(threads);
        let pieces: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..bands)
                .step_by(per_thread)
                .map(|first| {
                    scope.spawn(move || {
                        let mut piece = String::with_capacity(1 << 16);
                        encode_bands(
                            &mut piece,
                            index,
                            width,
                            top,
                            height,
                            first,
                            (first + per_thread).min(bands),
                        );
                        piece
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        for piece in &pieces {
            sb.push_str(piece);
        }
    }
    sb.push_str(END);
    sb
}

/// A picture this big (in pixels) is worth the threads.
const PARALLEL_FROM: usize = 400_000;

/// Bands `first..end` (six rows each) of the `height` rows starting at `top`, as passes; a `-` follows every band
/// but the picture's last.
fn encode_bands(sb: &mut String, index: &[u8], width: usize, top: usize, height: usize, first: usize, end: usize) {
    let mut bits = vec![0u8; 256 * width];
    let mut used: Vec<usize> = Vec::with_capacity(256);
    let mut seen = [false; 256];
    for band in (first * 6..(end * 6).min(height)).step_by(6) {
        let rows = (height - band).min(6);
        for k in 0..rows {
            let row = (top + band + k) * width;
            for x in 0..width {
                let c = usize::from(index[row + x]);
                bits[c * width + x] |= 1 << k;
                if !seen[c] {
                    seen[c] = true;
                    used.push(c);
                }
            }
        }
        for (u, &c) in used.iter().enumerate() {
            if u > 0 {
                sb.push('$'); // back to the band's start for the next colour
            }
            sb.push('#');
            push_number(sb, c);
            let (mut run, mut n) = ('\0', 0usize);
            for x in 0..width {
                let ch = char::from(63 + bits[c * width + x]);
                bits[c * width + x] = 0;
                if ch == run {
                    n += 1;
                    continue;
                }
                push_run(sb, run, n);
                (run, n) = (ch, 1);
            }
            if run != '?' {
                push_run(sb, run, n); // nothing painted at the end: left out
            }
            seen[c] = false;
        }
        used.clear();
        if band + 6 < height {
            sb.push('-');
        }
    }
}

/// Sixel's repeat introducer pays off from a run of four.
fn push_run(sb: &mut String, ch: char, n: usize) {
    if n == 0 {
        return;
    }
    if n > 3 {
        sb.push('!');
        push_number(sb, n);
        sb.push(ch);
    } else {
        for _ in 0..n {
            sb.push(ch);
        }
    }
}

fn push_number<T: ToString>(sb: &mut String, n: T) {
    sb.push_str(&n.to_string());
}

/// A channel value 0..=255 as Sixel's percentage 0..=100, rounded.
fn percent(v: u32) -> u32 {
    (v * 100 + 127) / 255
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: u32 = 0xFFFF_0000;
    const GREEN: u32 = 0xFF00_FF00;

    #[test]
    fn one_red_pixel_is_exactly_this_stream() {
        assert_eq!(encode(&[RED], 1, 0), "\x1bP0;1;0q\"1;1;1;1#0;2;100;0;0#0@\x1b\\");
    }

    #[test]
    fn a_long_run_uses_the_repeat_introducer_and_a_short_one_does_not() {
        // six rows of one colour: every column is all six bits, '~'
        assert_eq!(
            encode(&[RED; 6 * 5], 5, 0),
            "\x1bP0;1;0q\"1;1;5;6#0;2;100;0;0#0!5~\x1b\\"
        );
        assert_eq!(
            encode(&[RED; 6 * 3], 3, 0),
            "\x1bP0;1;0q\"1;1;3;6#0;2;100;0;0#0~~~\x1b\\"
        );
    }

    #[test]
    fn two_colours_in_a_band_are_two_passes_joined_by_dollar() {
        // two pixels side by side: red then green
        let s = encode(&[RED, GREEN], 2, 0);
        assert_eq!(s, "\x1bP0;1;0q\"1;1;2;1#0;2;100;0;0#1;2;0;100;0#0@$#1?@\x1b\\");
    }

    #[test]
    fn bands_are_separated_by_a_dash_and_the_last_band_may_be_short() {
        // 7 rows: a full band, '-', then a band of one row
        let s = encode(&[RED; 7], 1, 0);
        assert_eq!(s, "\x1bP0;1;0q\"1;1;1;7#0;2;100;0;0#0~-#0@\x1b\\");
    }

    #[test]
    fn a_transparent_pixel_is_painted_in_the_background_colour() {
        let s = encode(&[0x0000_0000], 1, 0x00FF_FFFF);
        assert_eq!(s, "\x1bP0;1;0q\"1;1;1;1#255;2;100;100;100#255@\x1b\\");
    }

    #[test]
    fn the_stream_is_a_dcs_with_a_q_and_ends_with_st() {
        let s = encode(&[RED, GREEN, RED, GREEN], 2, 0);
        assert!(s.starts_with("\x1bP"));
        // after ESC P come the parameters (digits and ';'), then the final byte of the DCS: q
        assert_eq!(s.as_bytes()[2..].iter().find(|b| b.is_ascii_alphabetic()), Some(&b'q'));
        assert!(s.ends_with(END));
    }

    #[test]
    fn an_empty_picture_is_a_valid_empty_stream() {
        assert_eq!(encode(&[], 0, 0), "\x1bP0;1;0q\"1;1;0;0\x1b\\");
    }

    #[test]
    fn any_number_of_threads_makes_the_same_bytes() {
        let (width, height) = (97, 61); // the last band is short, and bands do not divide evenly
        let index: Vec<u8> = (0..width * height)
            .map(|i| ((i * 7 + i / width * 3) % 23) as u8)
            .collect();
        let palette: Vec<u32> = (0..23).map(|c| c * 0x0a0b0c).collect();
        let one = encode_on(&index, &palette, width, 0, height, 0, 1);
        for threads in [2, 3, 4, 7, 64] {
            assert_eq!(
                encode_on(&index, &palette, width, 0, height, 0, threads),
                one,
                "{threads} threads"
            );
        }
    }

    #[test]
    fn percent_rounds_like_the_original() {
        assert_eq!([percent(0), percent(128), percent(255)], [0, 50, 100]);
    }
}
