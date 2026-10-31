//! The EXIF of a photograph (the camera, the lens, the exposure, the date, the place) read by hand, with every offset and
//! length checked: the block comes from a file that may be made to hurt.
//!
//! Where it is looked for: a JPEG (the APP1 segment), a TIFF (the file is one), a PNG (the `eXIf` chunk) and a WebP (the
//! `EXIF` chunk). Read: the main directory (IFD0), the EXIF directory and the GPS directory behind it, at most
//! `MAX_ENTRIES` entries each. Not read: maker notes, thumbnails, XMP, IPTC, the many tags of cameras that nobody asked for.

/// Entries of a directory that are read at most.
const MAX_ENTRIES: usize = 512;
/// Longest text that is kept.
const MAX_TEXT: usize = 256;

/// What the EXIF of a picture says, as text ready to be shown (a field is `None` when the block does not have it).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Exif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    pub software: Option<String>,
    /// When the picture was taken, "2024-08-12 15:38:00".
    pub taken: Option<String>,
    /// "1/250 s".
    pub exposure: Option<String>,
    /// "f/8.0".
    pub aperture: Option<String>,
    pub iso: Option<String>,
    /// "35 mm", and "52 mm" in 35 mm film terms when the camera says so.
    pub focal: Option<String>,
    pub bias: Option<String>,
    pub flash: Option<String>,
    /// The orientation tag (1 to 8).
    pub orientation: Option<u16>,
    /// Latitude and longitude in degrees (north and east positive).
    pub gps: Option<(f64, f64)>,
}

impl Exif {
    /// Whether anything was found.
    pub fn is_empty(&self) -> bool {
        *self == Exif::default()
    }
}

/// The EXIF in the first bytes of a file (`head` is enough: a JPEG keeps it at the start), if there is one.
pub fn read(head: &[u8]) -> Option<Exif> {
    let tiff = if head.starts_with(&[0xFF, 0xD8]) {
        jpeg(head)?
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        chunk(head, 8, true, b"eXIf")?
    } else if head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        let found = chunk(head, 12, false, b"EXIF")?;
        // some writers put the "Exif\0\0" of a JPEG in front
        found.strip_prefix(b"Exif\0\0").unwrap_or(found)
    } else if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        head
    } else {
        return None;
    };
    let exif = tiff_exif(tiff)?;
    (!exif.is_empty()).then_some(exif)
}

/// The TIFF block of a JPEG: the APP1 segment that starts with `Exif\0\0`.
fn jpeg(data: &[u8]) -> Option<&[u8]> {
    let mut at = 2;
    for _ in 0..64 {
        if data.get(at)? != &0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        if marker == 0xFF {
            at += 1; // padding
            continue;
        }
        // these have no length: stand-alone markers; the picture's data begins at the start of scan
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        if (0xD0..=0xD8).contains(&marker) || marker == 0x01 {
            at += 2;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([*data.get(at + 2)?, *data.get(at + 3)?]));
        if length < 2 {
            return None;
        }
        let body = data.get(at + 4..at + 2 + length)?;
        if marker == 0xE1
            && let Some(tiff) = body.strip_prefix(b"Exif\0\0")
        {
            return Some(tiff);
        }
        at += 2 + length;
    }
    None
}

/// The data of the first chunk called `name` in a PNG (`big_endian`: lengths are big-endian) or a RIFF file.
fn chunk<'a>(data: &'a [u8], mut at: usize, big_endian: bool, name: &[u8; 4]) -> Option<&'a [u8]> {
    for _ in 0..256 {
        let header = data.get(at..at + 8)?;
        let size = if big_endian {
            u32::from_be_bytes([header[0], header[1], header[2], header[3]])
        } else {
            u32::from_le_bytes([header[4], header[5], header[6], header[7]])
        } as usize;
        let kind = if big_endian { &header[4..8] } else { &header[0..4] };
        let start = at + 8;
        if kind == name {
            return data.get(start..start.checked_add(size)?);
        }
        // PNG chunks end with a 4-byte checksum, RIFF chunks are padded to an even size
        at = start
            .checked_add(size)?
            .checked_add(if big_endian { 4 } else { size % 2 })?;
    }
    None
}

#[derive(Clone, Copy)]
struct Reader<'a> {
    data: &'a [u8],
    big: bool,
}

impl<'a> Reader<'a> {
    fn u16(self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(at..at + 2)?.try_into().ok()?;
        Some(if self.big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    }

    fn u32(self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(at..at + 4)?.try_into().ok()?;
        Some(if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    /// The directory entries at `at`: (tag, type, count, where the value is: in the entry itself or at an offset).
    fn entries(self, at: usize) -> Vec<(u16, u16, usize, usize)> {
        let Some(count) = self.u16(at) else {
            return Vec::new();
        };
        (0..usize::from(count).min(MAX_ENTRIES))
            .filter_map(|i| {
                let entry = at + 2 + i * 12;
                let (tag, kind, count) = (self.u16(entry)?, self.u16(entry + 2)?, self.u32(entry + 4)? as usize);
                let size = count.checked_mul(unit(kind))?;
                let value = if size <= 4 {
                    entry + 8
                } else {
                    self.u32(entry + 8)? as usize
                };
                // the value must lie inside the block
                self.data.get(value..value.checked_add(size)?)?;
                Some((tag, kind, count, value))
            })
            .collect()
    }

    fn text(self, (_, kind, count, at): (u16, u16, usize, usize)) -> Option<String> {
        if kind != 2 {
            return None;
        }
        let raw = self.data.get(at..at + count.min(MAX_TEXT))?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let text = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        (!text.is_empty() && !text.chars().all(|c| c == ' ' || c == '?')).then_some(text)
    }

    fn short(self, (_, kind, count, at): (u16, u16, usize, usize)) -> Option<u16> {
        (kind == 3 && count >= 1).then(|| self.u16(at)).flatten()
    }

    /// A rational (or the first of several) as a number.
    fn rational(self, entry: (u16, u16, usize, usize), index: usize) -> Option<f64> {
        let (_, kind, count, at) = entry;
        if !(kind == 5 || kind == 10) || index >= count {
            return None;
        }
        let (n, d) = (self.u32(at + index * 8)?, self.u32(at + index * 8 + 4)?);
        if d == 0 {
            return None;
        }
        Some(if kind == 10 {
            f64::from(n as i32) / f64::from(d as i32)
        } else {
            f64::from(n) / f64::from(d)
        })
    }
}

/// Bytes in a value of this TIFF type.
fn unit(kind: u16) -> usize {
    match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

fn tiff_exif(data: &[u8]) -> Option<Exif> {
    let big = match data.get(..2)? {
        b"II" => false,
        b"MM" => true,
        _ => return None,
    };
    let r = Reader { data, big };
    if r.u16(2)? != 42 {
        return None;
    }
    let mut out = Exif::default();
    let (mut exif_ifd, mut gps_ifd) = (None, None);
    for entry in r.entries(r.u32(4)? as usize) {
        match entry.0 {
            0x010F => out.make = r.text(entry),
            0x0110 => out.model = r.text(entry),
            0x0131 => out.software = r.text(entry),
            0x0132 if out.taken.is_none() => out.taken = r.text(entry).map(|t| date(&t)),
            0x0112 => out.orientation = r.short(entry).filter(|o| (1..=8).contains(o)),
            0x8769 => exif_ifd = r.u32(entry.3),
            0x8825 => gps_ifd = r.u32(entry.3),
            _ => {}
        }
    }
    if let Some(at) = exif_ifd {
        let mut focal = None;
        let mut focal35 = None;
        for entry in r.entries(at as usize) {
            match entry.0 {
                0x829A => out.exposure = r.rational(entry, 0).map(exposure),
                0x829D => out.aperture = r.rational(entry, 0).map(|f| format!("f/{f:.1}")),
                0x8827 => out.iso = r.short(entry).map(|i| i.to_string()),
                0x9003 => out.taken = r.text(entry).map(|t| date(&t)),
                0x9204 => out.bias = r.rational(entry, 0).map(|b| format!("{b:+.1} EV")),
                0x9209 => {
                    out.flash = r
                        .short(entry)
                        .map(|f| (if f & 1 == 1 { "fired" } else { "did not fire" }).to_string());
                }
                0x920A => focal = r.rational(entry, 0),
                0xA405 => focal35 = r.short(entry),
                0xA434 => out.lens = r.text(entry),
                _ => {}
            }
        }
        out.focal = focal.map(|f| match focal35 {
            Some(w) if w > 0 && (f64::from(w) - f).abs() > 1.0 => format!("{f:.0} mm ({w} mm in 35 mm terms)"),
            _ => format!("{f:.0} mm"),
        });
    }
    if let Some(at) = gps_ifd {
        out.gps = gps(r, at as usize);
    }
    Some(out)
}

/// "2024:08:12 15:38:00" is shown as "2024-08-12 15:38:00"; anything else as it is.
fn date(text: &str) -> String {
    let b = text.as_bytes();
    if b.len() >= 19 && b[4] == b':' && b[7] == b':' {
        // `get`: a character that is cut by one of these places is no date, and not a panic
        if let (Some(y), Some(m), Some(d), Some(rest)) =
            (text.get(0..4), text.get(5..7), text.get(8..10), text.get(10..19))
        {
            return format!("{y}-{m}-{d}{rest}");
        }
    }
    text.to_string()
}

/// 0.004 as "1/250 s", 2.5 as "2.5 s".
fn exposure(seconds: f64) -> String {
    if seconds <= 0.0 || !seconds.is_finite() {
        return String::new();
    }
    if seconds < 0.5 {
        format!("1/{} s", (1.0 / seconds).round() as u64)
    } else {
        format!("{seconds:.1} s")
    }
}

fn gps(r: Reader<'_>, at: usize) -> Option<(f64, f64)> {
    let (mut lat_ref, mut lon_ref) = (b'N', b'E');
    let (mut lat, mut lon) = (None, None);
    let degrees = |entry| {
        let (d, m, s) = (r.rational(entry, 0)?, r.rational(entry, 1)?, r.rational(entry, 2)?);
        Some(d + m / 60.0 + s / 3600.0)
    };
    for entry in r.entries(at) {
        let letter = |e: (u16, u16, usize, usize)| r.data.get(e.3).copied();
        match entry.0 {
            1 => lat_ref = letter(entry)?,
            2 => lat = degrees(entry),
            3 => lon_ref = letter(entry)?,
            4 => lon = degrees(entry),
            _ => {}
        }
    }
    let (lat, lon) = (lat?, lon?);
    if !(lat.abs() <= 90.0 && lon.abs() <= 180.0) || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    Some((
        if lat_ref == b'S' { -lat } else { lat },
        if lon_ref == b'W' { -lon } else { lon },
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    /// A little-endian TIFF block with these entries in IFD0 and in the EXIF and GPS directories behind it.
    fn tiff() -> Vec<u8> {
        let mut b: Vec<u8> = b"II*\0\x08\0\0\0".to_vec();
        let push16 = |b: &mut Vec<u8>, v: u16| b.extend(v.to_le_bytes());
        let push32 = |b: &mut Vec<u8>, v: u32| b.extend(v.to_le_bytes());
        let entry = |b: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32| {
            b.extend(tag.to_le_bytes());
            b.extend(kind.to_le_bytes());
            b.extend(count.to_le_bytes());
            b.extend(value.to_le_bytes());
        };
        // IFD0 at 8: 4 entries (make, model, exif ifd, gps ifd) -> 2 + 48 + 4 = 54 bytes, strings after at 62
        push16(&mut b, 4);
        entry(&mut b, 0x010F, 2, 6, 62); // "Nikon\0"
        entry(&mut b, 0x0110, 2, 5, 68); // "D700\0"
        entry(&mut b, 0x8769, 4, 1, 80);
        entry(&mut b, 0x8825, 4, 1, 160);
        push32(&mut b, 0);
        b.extend(b"Nikon\0");
        b.extend(b"D700\0");
        b.resize(80, 0);
        // EXIF IFD at 80: exposure 1/250, f/8, iso 200 -> 2 + 36 + 4 = 42 bytes, rationals after at 122
        push16(&mut b, 3);
        entry(&mut b, 0x829A, 5, 1, 122);
        entry(&mut b, 0x829D, 5, 1, 130);
        entry(&mut b, 0x8827, 3, 1, 200);
        push32(&mut b, 0);
        push32(&mut b, 1);
        push32(&mut b, 250);
        push32(&mut b, 8);
        push32(&mut b, 1);
        b.resize(160, 0);
        // GPS IFD at 160: N, 51 30 0, W, 0 7 30 -> 2 + 48 + 4 = 54, values after at 214
        push16(&mut b, 4);
        entry(&mut b, 1, 2, 2, u32::from(b'N'));
        entry(&mut b, 2, 5, 3, 214);
        entry(&mut b, 3, 2, 2, u32::from(b'W'));
        entry(&mut b, 4, 5, 3, 238);
        push32(&mut b, 0);
        for (n, d) in [(51, 1), (30, 1), (0, 1), (0, 1), (7, 1), (30, 1)] {
            push32(&mut b, n);
            push32(&mut b, d);
        }
        b
    }

    fn in_jpeg(tiff: &[u8]) -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xE1];
        jpeg.extend(((tiff.len() + 8) as u16).to_be_bytes());
        jpeg.extend(b"Exif\0\0");
        jpeg.extend(tiff);
        jpeg.extend([0xFF, 0xDA, 0, 2]);
        jpeg
    }

    #[test]
    fn a_tiff_block_gives_the_camera_the_exposure_and_the_place() {
        let exif = read(&tiff()).expect("exif");
        assert_eq!(exif.make.as_deref(), Some("Nikon"));
        assert_eq!(exif.model.as_deref(), Some("D700"));
        assert_eq!(exif.exposure.as_deref(), Some("1/250 s"));
        assert_eq!(exif.aperture.as_deref(), Some("f/8.0"));
        assert_eq!(exif.iso.as_deref(), Some("200"));
        let (lat, lon) = exif.gps.expect("a place");
        assert!((lat - 51.5).abs() < 1e-9 && (lon + 0.125).abs() < 1e-9, "{lat} {lon}");
    }

    #[test]
    fn the_block_is_found_in_a_jpeg_a_png_and_a_webp() {
        let block = tiff();
        assert_eq!(read(&in_jpeg(&block)).expect("jpeg").model.as_deref(), Some("D700"));
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(0u32.to_be_bytes());
        png.extend(b"IHDR");
        png.extend([0u8; 4]);
        png.extend((block.len() as u32).to_be_bytes());
        png.extend(b"eXIf");
        png.extend(&block);
        png.extend([0u8; 4]);
        assert_eq!(read(&png).expect("png").model.as_deref(), Some("D700"));
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        webp.extend(b"EXIF");
        webp.extend((block.len() as u32).to_le_bytes());
        webp.extend(&block);
        assert_eq!(read(&webp).expect("webp").model.as_deref(), Some("D700"));
    }

    #[test]
    fn nothing_hurts_when_the_block_is_cut_spoilt_or_lies() {
        let good = in_jpeg(&tiff());
        for cut in 0..good.len() {
            let _ = read(&good[..cut]);
        }
        let mut seed = 7u32;
        for _ in 0..2000 {
            let mut spoilt = good.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed >> 8) as usize % spoilt.len();
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                spoilt[at] = (seed >> 16) as u8;
            }
            let _ = read(&spoilt);
        }
        // an offset past the end, a count of billions, a directory that points at itself
        let mut lying = tiff();
        lying[4..8].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(read(&lying).is_none());
        let mut big = tiff();
        big[14..18].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        let _ = read(&big);
        assert!(read(b"").is_none() && read(b"II*\0").is_none() && read(&[0xFF, 0xD8]).is_none());
    }

    #[test]
    fn dates_and_exposures_are_written_for_people() {
        assert_eq!(date("2024:08:12 15:38:00"), "2024-08-12 15:38:00");
        assert_eq!(date("yesterday"), "yesterday");
        // a character cut by a place the date is sliced at: left as it is, and no panic
        let odd = "2024:08:1\u{e9}\u{e9}5:38:00 x";
        assert_eq!(date(odd), odd);
        assert_eq!(exposure(0.004), "1/250 s");
        assert_eq!(exposure(2.5), "2.5 s");
        assert_eq!(exposure(0.0), "");
    }
}
