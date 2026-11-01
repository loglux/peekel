//! The `INFO` chunk: the first chunk of a page, with its size, resolution and rotation.
//!
//! Layout (ten bytes): width and height as big-endian 16-bit numbers, the minor and the major version (one byte each), the
//! resolution in dots per inch as a **little-endian** 16-bit number, the gamma as one byte (tenths: 22 is 2.2), and a flags byte
//! whose last three bits are the rotation (the first five are reserved). This is section "Info Chunk: INFO" (page 24) of the
//! official DjVu v3 specification, `DjVu3Spec.djvu` (LizardTech; a copy in the Internet Archive, see docs/internal/djvu.md); the
//! spec says that any data after the ten bytes is to be ignored, and its example is the first test below, byte for byte. A chunk
//! shorter than ten bytes is not what the spec describes; it is read with defaults for what is missing, as other readers do.

use crate::Error;

/// How a page is turned for display. A value of the flags that is not one of these four is read as no rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// 0 degrees (flags 1, and any value not listed).
    None,
    /// 90 degrees counter-clockwise (flags 6).
    Ccw90,
    /// 180 degrees (flags 2).
    Turn180,
    /// 90 degrees clockwise, which is 270 counter-clockwise (flags 5).
    Cw90,
}

/// What the `INFO` chunk of a page says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageInfo {
    pub width: u16,
    pub height: u16,
    pub minor_version: u8,
    pub major_version: u8,
    /// Dots per inch; 300 when the chunk is too short to hold it.
    pub dpi: u16,
    /// The gamma the page was made for; 2.2 when the chunk is too short to hold it.
    pub gamma: f32,
    pub rotation: Rotation,
}

/// The resolution taken when an `INFO` chunk is shorter than eight bytes (a few real files have short ones).
pub const DEFAULT_DPI: u16 = 300;

/// The gamma taken when an `INFO` chunk is shorter than nine bytes.
pub const DEFAULT_GAMMA: f32 = 2.2;

impl PageInfo {
    /// Reads the data of an `INFO` chunk. The size is required (four bytes); a chunk shorter than ten bytes gets the default
    /// for what it lacks.
    ///
    /// # Errors
    /// [`Error::BadInfo`] when there are fewer than four bytes.
    pub fn parse(data: &[u8]) -> Result<Self, Error> {
        let (Some(&[w0, w1, h0, h1]), minor, major) = (data.first_chunk::<4>(), data.get(4), data.get(5)) else {
            return Err(Error::BadInfo);
        };
        let dpi = data.get(6..8).map_or(DEFAULT_DPI, |b| u16::from_le_bytes([b[0], b[1]]));
        let gamma = data.get(8).map_or(DEFAULT_GAMMA, |&tenths| f32::from(tenths) / 10.0);
        let rotation = match data.get(9).map_or(0, |flags| flags & 0b111) {
            6 => Rotation::Ccw90,
            2 => Rotation::Turn180,
            5 => Rotation::Cw90,
            _ => Rotation::None,
        };
        Ok(Self {
            width: u16::from_be_bytes([w0, w1]),
            height: u16::from_be_bytes([h0, h1]),
            minor_version: minor.copied().unwrap_or(0),
            major_version: major.copied().unwrap_or(0),
            dpi,
            gamma,
            rotation,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // a test that cannot make its input should stop at once
mod tests {
    use super::*;

    #[test]
    fn the_example_of_the_spec_is_read_as_the_spec_says() {
        // "INFO [10 bytes] 2202 x 967, version 26, 300 dpi, gamma = 2.2"
        let info =
            PageInfo::parse(&[0x08, 0x9A, 0x03, 0xC7, 26, 0, 0x2C, 0x01, 22, 1]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((info.width, info.height), (2202, 967));
        assert_eq!((info.minor_version, info.major_version), (26, 0));
        assert_eq!(info.dpi, 300, "the resolution is little-endian");
        assert!((info.gamma - 2.2).abs() < 1e-6);
        assert_eq!(info.rotation, Rotation::None);
    }

    #[test]
    fn the_four_rotations_of_the_flags_and_the_rest_as_none() {
        let with_flags = |flags: u8| PageInfo::parse(&[0, 1, 0, 1, 0, 0, 0, 0, 22, flags]).map(|i| i.rotation);
        assert_eq!(with_flags(1), Ok(Rotation::None));
        assert_eq!(with_flags(6), Ok(Rotation::Ccw90));
        assert_eq!(with_flags(2), Ok(Rotation::Turn180));
        assert_eq!(with_flags(5), Ok(Rotation::Cw90));
        assert_eq!(
            with_flags(0b1111_1001),
            Ok(Rotation::None),
            "only the low three bits are the rotation, and 1 is none"
        );
        assert_eq!(
            with_flags(0b1111_1110),
            Ok(Rotation::Ccw90),
            "the other bits are not the rotation"
        );
    }

    #[test]
    fn a_short_chunk_gets_the_defaults_and_one_below_four_bytes_is_refused() {
        let short = PageInfo::parse(&[0, 5, 0, 7, 24]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((short.width, short.height, short.minor_version), (5, 7, 24));
        assert_eq!(
            (short.dpi, short.gamma, short.rotation),
            (DEFAULT_DPI, DEFAULT_GAMMA, Rotation::None)
        );
        assert_eq!(PageInfo::parse(&[0, 5, 0]).unwrap_err(), Error::BadInfo);
        assert_eq!(PageInfo::parse(&[]).unwrap_err(), Error::BadInfo);
    }
}
