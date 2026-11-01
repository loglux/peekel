//! RAW photographs (NEF, CR2, ARW, DNG, ORF, ...): the JPEG preview the camera put in the file.
//!
//! A RAW file is a TIFF container, and its picture, the data of the sensor, is not drawn here: that wants a "development"
//! (demosaicing, white balance, a curve) which every maker does in its own way. What is shown is what the camera itself made
//! of it, the JPEG preview that nearly every RAW file carries (often the full size, otherwise 1 to 2 megapixels), as the
//! maker's own viewers show it first. It costs a few milliseconds where developing a 20 MP file costs seconds.
//!
//! The directories of the file (IFD0, the SubIFDs, the ones that follow) are walked and every JPEG found is a candidate:
//! by `JPEGInterchangeFormat` and its length (tags 0x201, 0x202), by a strip of a JPEG-compressed image (compression 6 or
//! 7), or by `JpgFromRaw` (0x2E, Panasonic). The biggest wins. Every offset is checked against the file, the number of
//! directories is limited, and a loop (a directory that points back) ends the walk: a damaged file gives no preview, not
//! a panic. The turn of the picture is the Orientation of IFD0 unless the preview says its own.

use std::path::Path;

use image::RgbaImage;

use crate::decode::{DecodeError, decode_bytes_oriented};
use crate::guard::{has_extension, unreadable};

/// The extensions of the RAW formats whose container is TIFF-like. (Canon's CR3 is another container, ISO BMFF; Fuji's RAF
/// has a header of its own: neither is read yet.)
const EXTENSIONS: [&str; 15] = [
    "nef", "nrw", "cr2", "arw", "sr2", "srf", "dng", "orf", "pef", "3fr", "erf", "mef", "kdc", "dcr", "rw2",
];

/// A RAW file bigger than this is not read (it is held in memory whole).
const MAX_FILE: u64 = 512 << 20;

/// How many directories are looked at; a damaged file may chain them for ever.
const MAX_IFDS: usize = 64;

/// A preview smaller than this is a thumbnail, not worth showing as the photograph.
const MIN_PREVIEW: usize = 4096;

/// Whether the name says a RAW photograph.
pub fn is_raw(path: &Path) -> bool {
    EXTENSIONS.iter().any(|e| has_extension(path, e))
}

/// The JPEG preview of the RAW photograph at `path`, turned upright.
pub fn render(path: &Path) -> Result<RgbaImage, DecodeError> {
    let length = std::fs::metadata(path).map_err(unreadable)?.len();
    if length > MAX_FILE {
        return Err(DecodeError::Unreadable("a RAW file over 512 MB".to_string()));
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    let preview = find_preview(&bytes)
        .ok_or_else(|| DecodeError::Unreadable("a RAW photograph with no JPEG preview in it".to_string()))?;
    decode_bytes_oriented(
        &bytes[preview.start..preview.start + preview.length],
        u64::MAX,
        preview.orientation,
    )
}

/// Where the biggest JPEG preview is in a file, and the turn of the photograph.
#[derive(Debug, PartialEq, Eq)]
pub struct Preview {
    pub start: usize,
    pub length: usize,
    /// The EXIF orientation (1 to 8) of IFD0.
    pub orientation: u16,
    /// The biggest `ImageWidth` x `ImageLength` of any directory: the sensor's data (IFD0 is only a thumbnail).
    pub size: Option<(u32, u32)>,
}

/// The bytes of a TIFF file, read in its byte order, every read checked.
struct Tiff<'a> {
    data: &'a [u8],
    big: bool,
}

impl<'a> Tiff<'a> {
    /// `II` or `MM`, then 42 (or the makers' own numbers: Olympus 0x4F52 and 0x5352, Panasonic 0x55).
    fn new(data: &'a [u8]) -> Option<Self> {
        let big = match data.get(..2)? {
            b"II" => false,
            b"MM" => true,
            _ => return None,
        };
        let tiff = Self { data, big };
        matches!(tiff.u16(2)?, 42 | 0x4F52 | 0x5352 | 0x55).then_some(tiff)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    /// The one value of an entry that holds one: a SHORT (3) or a LONG (4) kept in the entry itself.
    fn value(&self, entry: usize) -> Option<usize> {
        let (kind, count) = (self.u16(entry + 2)?, self.u32(entry + 4)?);
        if count != 1 {
            return None;
        }
        match kind {
            3 => self.u16(entry + 8).map(usize::from),
            4 => self.u32(entry + 8).and_then(|v| usize::try_from(v).ok()),
            _ => None,
        }
    }

    /// Whether `start .. start + length` is a JPEG (it starts with the marker) that lies in the file.
    fn is_jpeg(&self, start: usize, length: usize) -> bool {
        length >= MIN_PREVIEW
            && start.checked_add(length).is_some_and(|end| end <= self.data.len())
            && self.data.get(start..start + 3) == Some(&[0xFF, 0xD8, 0xFF])
    }
}

/// The biggest JPEG preview in a TIFF-like file, or None when there is none (or the file is not TIFF or is damaged).
pub fn find_preview(data: &[u8]) -> Option<Preview> {
    let tiff = Tiff::new(data)?;
    let first = usize::try_from(tiff.u32(4)?).ok()?;
    let mut queue = vec![first];
    let mut seen: Vec<usize> = Vec::new();
    let mut best: Option<(usize, usize)> = None;
    let mut orientation = 1u16;
    let mut size: Option<(u32, u32)> = None;
    while let Some(ifd) = queue.pop() {
        if ifd == 0 || seen.contains(&ifd) || seen.len() >= MAX_IFDS {
            continue;
        }
        let is_first = seen.is_empty();
        seen.push(ifd);
        let Some(count) = tiff.u16(ifd).map(usize::from) else {
            continue;
        };
        // (offset, length) of a JPEG named by the entries of this directory
        let (mut format, mut format_length) = (None, None);
        let (mut compression, mut strip, mut strip_length) = (None, None, None);
        let (mut width, mut height) = (None, None);
        for i in 0..count {
            let entry = ifd + 2 + i * 12;
            let Some(tag) = tiff.u16(entry) else {
                break;
            };
            match tag {
                0x0112 if is_first => {
                    orientation = tiff.value(entry).and_then(|v| u16::try_from(v).ok()).unwrap_or(1);
                }
                0x0100 => width = tiff.value(entry).and_then(|v| u32::try_from(v).ok()),
                0x0101 => height = tiff.value(entry).and_then(|v| u32::try_from(v).ok()),
                0x0103 => compression = tiff.value(entry),
                0x0111 => strip = tiff.value(entry),
                0x0117 => strip_length = tiff.value(entry),
                0x0201 => format = tiff.value(entry),
                0x0202 => format_length = tiff.value(entry),
                // JpgFromRaw (Panasonic): the count is the length, the value the offset
                0x002E => {
                    if let (Some(length), Some(start)) = (tiff.u32(entry + 4), tiff.u32(entry + 8)) {
                        consider(&tiff, &mut best, start as usize, length as usize);
                    }
                }
                // SubIFDs: one offset in the entry, or the offset of a list of them
                0x014A => {
                    let Some(n) = tiff.u32(entry + 4).map(|n| (n as usize).min(16)) else {
                        continue;
                    };
                    if n == 1 {
                        queue.extend(tiff.u32(entry + 8).map(|v| v as usize));
                    } else if let Some(list) = tiff.u32(entry + 8).map(|v| v as usize) {
                        queue.extend((0..n).filter_map(|k| tiff.u32(list.checked_add(k * 4)?).map(|v| v as usize)));
                    }
                }
                _ => {}
            }
        }
        if let (Some(w), Some(h)) = (width, height)
            && size.is_none_or(|(bw, bh)| u64::from(w) * u64::from(h) > u64::from(bw) * u64::from(bh))
        {
            size = Some((w, h));
        }
        if let (Some(start), Some(length)) = (format, format_length) {
            consider(&tiff, &mut best, start, length);
        }
        if matches!(compression, Some(6 | 7))
            && let (Some(start), Some(length)) = (strip, strip_length)
        {
            consider(&tiff, &mut best, start, length);
        }
        // the next directory
        if let Some(next) = count
            .checked_mul(12)
            .and_then(|n| ifd.checked_add(2)?.checked_add(n))
            .and_then(|at| tiff.u32(at))
        {
            queue.push(next as usize);
        }
    }
    best.map(|(start, length)| Preview {
        start,
        length,
        orientation,
        size,
    })
}

/// Keeps the biggest of the JPEGs seen.
fn consider(tiff: &Tiff<'_>, best: &mut Option<(usize, usize)>, start: usize, length: usize) {
    if tiff.is_jpeg(start, length) && best.is_none_or(|(_, biggest)| length > biggest) {
        *best = Some((start, length));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot build its file should stop at once
mod tests {
    use super::*;

    /// A JPEG of `w` x `h` pixels (a gradient), as bytes.
    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        // a gradient with a pattern over it, so that the file is not tiny (a JPEG of a plain gradient is a few hundred bytes)
        let picture = image::RgbImage::from_fn(w, h, |x, y| {
            let pattern = ((x * 7 + y * 13) % 41) as u8;
            image::Rgb([
                (x * 200 / w) as u8 + pattern,
                (y * 200 / h) as u8 + pattern,
                90 + pattern,
            ])
        });
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&picture)
            .expect("encodes");
        out
    }

    fn put16(out: &mut Vec<u8>, v: u16) {
        out.extend(v.to_le_bytes());
    }

    fn put32(out: &mut Vec<u8>, v: u32) {
        out.extend(v.to_le_bytes());
    }

    /// An entry: tag, type, count, value.
    fn entry(out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32) {
        put16(out, tag);
        put16(out, kind);
        put32(out, count);
        put32(out, value);
    }

    /// A little-endian TIFF like a NEF: IFD0 (Orientation, a small JPEG thumbnail, SubIFDs) and one SubIFD with the big
    /// JPEG; the JPEGs after the directories. `turn` is the orientation of IFD0.
    fn raw_file(thumb: &[u8], big: &[u8], turn: u16) -> Vec<u8> {
        // layout: header 8 | IFD0 at 8: 7 entries = 2 + 84 + 4 = 90 | SubIFD at 98: 4 entries = 2 + 48 + 4 = 54 | data at 152
        let data_at = 152u32;
        let mut out = Vec::new();
        out.extend(b"II* ");
        put32(&mut out, 8);
        put16(&mut out, 7);
        entry(&mut out, 0x0100, 3, 1, 160);
        entry(&mut out, 0x0101, 3, 1, 120);
        entry(&mut out, 0x0112, 3, 1, u32::from(turn));
        entry(&mut out, 0x0201, 4, 1, data_at);
        entry(&mut out, 0x0202, 4, 1, thumb.len() as u32);
        entry(&mut out, 0x0103, 3, 1, 1);
        entry(&mut out, 0x014A, 4, 1, 98);
        put32(&mut out, 0);
        assert_eq!(out.len(), 98);
        put16(&mut out, 4);
        entry(&mut out, 0x0100, 3, 1, 5600);
        entry(&mut out, 0x0101, 3, 1, 3728);
        entry(&mut out, 0x0201, 4, 1, data_at + thumb.len() as u32);
        entry(&mut out, 0x0202, 4, 1, big.len() as u32);
        put32(&mut out, 0);
        assert_eq!(out.len(), 152);
        out.extend(thumb);
        out.extend(big);
        out
    }

    #[test]
    fn the_biggest_jpeg_of_the_file_is_the_preview_whichever_directory_it_is_in() {
        let (thumb, big) = (jpeg(64, 48), jpeg(640, 480));
        assert!(big.len() > thumb.len());
        let file = raw_file(&thumb, &big, 1);
        let found = find_preview(&file).expect("a preview");
        assert_eq!(found.length, big.len());
        assert_eq!(&file[found.start..found.start + 3], &[0xFF, 0xD8, 0xFF]);
        assert_eq!(found.orientation, 1);
        assert_eq!(
            found.size,
            Some((5600, 3728)),
            "the sensor's size, not the thumbnail's 160 x 120"
        );
        // the order of the two does not matter: the thumbnail may be the bigger one in the file
        let swapped = raw_file(&big, &thumb, 1);
        assert_eq!(find_preview(&swapped).expect("a preview").length, big.len());
    }

    #[test]
    fn the_orientation_of_ifd0_turns_a_preview_that_says_nothing_of_it() {
        let (thumb, big) = (jpeg(64, 48), jpeg(640, 480));
        let dir = std::env::temp_dir().join(format!("peekel-raw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("shot.nef");
        std::fs::write(&path, raw_file(&thumb, &big, 6)).expect("writes");
        let drawn = render(&path).expect("draws");
        // 6 is a quarter turn: 640 x 480 stands up as 480 x 640
        assert_eq!((drawn.width(), drawn.height()), (480, 640));
        std::fs::write(&path, raw_file(&thumb, &big, 1)).expect("writes");
        assert_eq!(render(&path).expect("draws").dimensions(), (640, 480));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_extension_says_a_raw_photograph() {
        for name in ["a.NEF", "b.cr2", "c.Arw", "d.dng", "e.ORF"] {
            assert!(is_raw(Path::new(name)), "{name}");
        }
        for name in ["a.jpg", "b.tif", "c.nefx", "d"] {
            assert!(!is_raw(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn a_file_with_no_preview_or_not_tiff_or_damaged_gives_none_and_does_not_panic() {
        assert_eq!(find_preview(b""), None);
        assert_eq!(find_preview(b"not a tiff at all"), None);
        assert_eq!(find_preview(b"II*\0"), None);
        // every cut of a good file: a preview or none, never a panic
        let file = raw_file(&jpeg(64, 48), &jpeg(640, 480), 1);
        for cut in (0..file.len()).step_by(7) {
            let _ = find_preview(&file[..cut]);
        }
        // every single byte of the directories spoilt in turn
        for at in 0..152 {
            let mut spoilt = file.clone();
            spoilt[at] ^= 0xFF;
            let _ = find_preview(&spoilt);
        }
    }

    #[test]
    fn directories_that_point_back_or_claim_too_much_end_the_walk() {
        // IFD0 whose next directory is itself, with the SubIFD pointing at IFD0
        let mut file = Vec::new();
        file.extend(b"II*\0");
        put32(&mut file, 8);
        put16(&mut file, 1);
        entry(&mut file, 0x014A, 4, 1, 8);
        put32(&mut file, 8);
        assert_eq!(find_preview(&file), None);
        // a SubIFDs list of a billion entries, a count of directory entries that runs past the end
        let mut big = Vec::new();
        big.extend(b"II*\0");
        put32(&mut big, 8);
        put16(&mut big, 60000);
        entry(&mut big, 0x014A, 4, 1_000_000_000, 8);
        assert_eq!(find_preview(&big), None);
    }

    #[test]
    fn a_jpeg_that_runs_past_the_end_of_the_file_is_not_a_preview() {
        let (thumb, big) = (jpeg(160, 120), jpeg(640, 480));
        assert!(
            thumb.len() >= MIN_PREVIEW,
            "the test's thumbnail is {} bytes",
            thumb.len()
        );
        let mut file = raw_file(&thumb, &big, 1);
        file.truncate(file.len() - 10);
        let found = find_preview(&file).expect("the thumbnail is whole");
        assert_eq!(found.length, thumb.len());
    }
}
