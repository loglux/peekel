//! The JPEG layers of a page: `BGjp` (the background) and `FGjp` (the colours of the foreground), a JPEG picture in place of the
//! IW44 chunks (spec 7.1.3.3: "a JPEG-encoded color image"; nothing more is said). DjVuLibre decodes it with libjpeg at full size
//! with the defaults: a colour JPEG is YCbCr turned to RGB, a grey one is copied to the three channels, and the picture is then
//! used like the picture of the IW44 chunks (it may be smaller than the page: the page's size divided by a whole number, rounded
//! up). The decoder is `zune-jpeg` (the one `image` uses, MIT or Apache or Zlib), so this crate does not carry a JPEG decoder of its
//! own.

use zune_core::bytestream::ZCursor;

use crate::Error;
use crate::iw44::Pixmap;

/// The most pixels a JPEG layer may have (as the page: `page::MAX_PIXELS`).
const MAX_PIXELS: usize = 100_000_000;

/// Decodes a JPEG to RGB.
///
/// # Errors
/// `BadJpeg` for data that is not a JPEG the decoder takes (CMYK too), or too big.
pub fn decode(data: &[u8]) -> Result<Pixmap, Error> {
    // the decoder is written not to panic on bad data, but a page that makes it must not end the program
    std::panic::catch_unwind(|| decode_inner(data)).unwrap_or(Err(Error::BadJpeg("the decoder failed")))
}

fn decode_inner(data: &[u8]) -> Result<Pixmap, Error> {
    let mut decoder = zune_jpeg::JpegDecoder::new(ZCursor::new(data));
    decoder
        .decode_headers()
        .map_err(|_| Error::BadJpeg("the header cannot be read"))?;
    let info = decoder.info().ok_or(Error::BadJpeg("no header"))?;
    let (width, height) = (usize::from(info.width), usize::from(info.height));
    if width == 0 || height == 0 || width * height > MAX_PIXELS {
        return Err(Error::BadJpeg("the picture has no size or too many pixels"));
    }
    let samples = decoder
        .decode()
        .map_err(|_| Error::BadJpeg("the picture cannot be decoded"))?;
    let rgb = if samples.len() == width * height * 3 {
        samples
    } else if samples.len() == width * height {
        samples.iter().flat_map(|&grey| [grey, grey, grey]).collect()
    } else {
        return Err(Error::BadJpeg("neither grey nor RGB"));
    };
    Ok(Pixmap { width, height, rgb })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_that_is_not_a_jpeg_is_refused() {
        assert!(decode(b"").is_err());
        assert!(decode(b"not a picture at all").is_err());
        assert!(decode(&[0xFF, 0xD8, 0xFF]).is_err());
    }
}
