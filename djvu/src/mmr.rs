//! The `Smmr` chunk: the mask of a page as a Fax G4 (MMR, ITU-T T.6) picture, in place of the JB2 `Sjbz`.
//!
//! The spec (8.2.2 and 7.1.3.3) only says "a bi-level image encoded with the Fax-G4/MMR method"; the layout of the chunk was found
//! with `djvumake` and `ddjvu` (what the first one accepts and the second one draws) and by having DjVuLibre's `MMRDecoder`
//! described in words, see docs/internal/djvu.md. It is: the bytes `MMR` and a flag byte (bit 0 reverses black and white, bit 1
//! says the picture is in stripes), the width and the height as 16 bits each, big-endian; when striped, the number of rows in a
//! stripe (16 bits); then the G4 data, which when striped is cut into stripes, each with its own length (32 bits) before it and its
//! own G4 stream (the first row of a stripe is coded against a white row). The G4 decoder itself is the `fax` crate (MIT, safe code).

use crate::Error;
use crate::jb2::Bitmap;

/// The most pixels the mask of a page may have (as the page: `page::MAX_PIXELS`).
const MAX_PIXELS: usize = 100_000_000;

/// Reads a 16-bit big-endian number at `at`.
fn be16(data: &[u8], at: usize) -> Option<usize> {
    let bytes = data.get(at..at + 2)?;
    Some(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
}

/// Decodes the data of an `Smmr` chunk to the mask: 1 is black, the top row first.
///
/// # Errors
/// `BadMmr` for a header that is not one, a stripe that is cut short, or G4 data that is not G4.
pub fn decode(data: &[u8]) -> Result<Bitmap, Error> {
    if data.len() < 8 || &data[..3] != b"MMR" || data[3] > 3 {
        return Err(Error::BadMmr("not the header of an MMR mask"));
    }
    let (reversed, striped) = (data[3] & 1 != 0, data[3] & 2 != 0);
    let (width, height) = (be16(data, 4).unwrap_or(0), be16(data, 6).unwrap_or(0));
    if width == 0 || height == 0 {
        return Err(Error::BadMmr("the picture has no size"));
    }
    if width * height > MAX_PIXELS {
        return Err(Error::BadMmr("the picture has more pixels than the limit"));
    }
    let mut mask = Bitmap {
        width,
        height,
        pixels: vec![0; width * height],
    };
    let mut row = 0;
    if striped {
        let rows_in_stripe = be16(data, 8).ok_or(Error::BadMmr("the header is cut short"))?;
        if rows_in_stripe == 0 {
            return Err(Error::BadMmr("a stripe of no rows"));
        }
        let mut at = 10;
        while row < height {
            let length = data
                .get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
                .ok_or(Error::BadMmr("a stripe is missing"))?;
            let stripe = data
                .get(at + 4..)
                .and_then(|rest| rest.get(..length))
                .ok_or(Error::BadMmr("a stripe is cut short"))?;
            let rows = rows_in_stripe.min(height - row);
            stripe_rows(stripe, &mut mask, row, rows, reversed)?;
            row += rows;
            at += 4 + length;
        }
    } else {
        stripe_rows(&data[8..], &mut mask, 0, height, reversed)?;
    }
    Ok(mask)
}

/// Decodes `rows` rows of one G4 stream into the rows of `mask` from `first`. Rows that the stream does not have are white.
fn stripe_rows(stream: &[u8], mask: &mut Bitmap, first: usize, rows: usize, reversed: bool) -> Result<(), Error> {
    let width = mask.width;
    let mut y = first;
    // the width and the number of rows are of 16 bits, so they fit what the decoder takes
    let width16 = u16::try_from(width).map_err(|_| Error::BadMmr("the picture is too wide"))?;
    let rows16 = u16::try_from(rows).map_err(|_| Error::BadMmr("too many rows in a stripe"))?;
    let done = fax::decoder::decode_g4(stream.iter().copied(), width16, Some(rows16), |changes| {
        if y >= first + rows {
            return;
        }
        let line = &mut mask.pixels[y * width..(y + 1) * width];
        // the positions where the colour changes, the first run being white: the black runs are from an odd change to the next
        let mut black = reversed;
        let mut from = 0usize;
        for &change in changes.iter().chain(std::iter::once(&width16)) {
            let to = usize::from(change).min(width);
            if black && to > from {
                line[from..to].fill(1);
            }
            from = to.max(from);
            black = !black;
        }
        y += 1;
    });
    done.ok_or(Error::BadMmr("the G4 data cannot be decoded"))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn a_header_that_is_not_one_is_refused() {
        assert!(decode(b"").is_err());
        assert!(decode(b"MMR").is_err());
        assert!(decode(b"MMQ\0\0\x10\0\x10").is_err());
        // a flag that does not exist
        assert!(decode(b"MMR\x04\0\x10\0\x10").is_err());
        // no size
        assert!(decode(b"MMR\0\0\0\0\x10").is_err());
    }

    #[test]
    fn a_picture_of_white_rows_is_white() {
        // 16 x 4, every row coded as "vertical, no change" (the bit 1) against a white row: four bits, then the end of block
        let mask = decode(b"MMR\0\0\x10\0\x04\xF0\x00\x10\x01").expect("decodes");
        assert_eq!((mask.width, mask.height), (16, 4));
        assert!(mask.pixels.iter().all(|&p| p == 0));
        // reversed, the same rows are all black
        let mask = decode(b"MMR\x01\0\x10\0\x04\xF0\x00\x10\x01").expect("decodes");
        assert!(mask.pixels.iter().all(|&p| p == 1));
    }

    #[test]
    fn a_stripe_that_is_cut_short_is_refused() {
        // striped, 16 x 4, two rows a stripe, the first stripe says 9 bytes and has 1
        assert!(decode(b"MMR\x02\0\x10\0\x04\0\x02\0\0\0\x09\xF0").is_err());
    }
}
