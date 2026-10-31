//! Windows programs and libraries (the PE format: `.exe`, `.dll`, `.sys`, `.ocx`): what the headers say (32 or 64 bit, program or
//! library, window or console, the time it was built, whether a certificate is attached, .NET), what its version resource says
//! (product, description, version, company, copyright) and its icon, read by hand with every offset and length checked, because
//! the file may be made to hurt.
//!
//! Not done: the certificate is not verified (only that there is one), imports and exports, the manifest, resources other than
//! the version and the icon, other languages than the first of each resource.

use image::RgbaImage;

use crate::decode;
use crate::guard::{le_u16, le_u32};

/// Entries of a resource directory that are read, and strings of the version resource.
const MAX_ENTRIES: usize = 256;
const MAX_STRINGS: usize = 64;
/// The icon's pixels at most (the biggest size of an icon is 256 x 256).
const MAX_ICON_PIXELS: u64 = 512 * 512;

/// What was found in a PE file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pe {
    /// "x64", "x86", "ARM64", "ARM" or the number of the machine.
    pub machine: String,
    pub is_dll: bool,
    /// "graphical", "console" or another kind of subsystem.
    pub subsystem: String,
    /// The time the linker wrote in the header (seconds since 1970), when it is plausible.
    pub built: Option<u32>,
    /// Whether the file has a certificate attached (not verified).
    pub signed: bool,
    /// Whether it is a .NET assembly.
    pub dotnet: bool,
    /// The strings of the version resource: (name, value).
    pub strings: Vec<(String, String)>,
}

struct Image<'a> {
    data: &'a [u8],
    /// (virtual address, virtual size, file offset, size in the file) of each section.
    sections: Vec<(u32, u32, u32, u32)>,
}

impl Image<'_> {
    /// The bytes at a relative virtual address (as a part of the file), `len` of them.
    fn rva(&self, rva: u32, len: usize) -> Option<&[u8]> {
        let (va, _, raw, raw_size) = *self
            .sections
            .iter()
            .find(|(va, vsize, _, raw_size)| rva >= *va && rva - *va < (*vsize).max(*raw_size))?;
        let at = usize::try_from(raw)
            .ok()?
            .checked_add(usize::try_from(rva - va).ok()?)?;
        // not past the section's data in the file
        let end = at.checked_add(len)?;
        (end <= usize::try_from(raw.checked_add(raw_size)?).ok()?.min(self.data.len())).then(|| &self.data[at..end])
    }
}

struct Headers<'a> {
    image: Image<'a>,
    /// (RVA, size) of the data directories.
    directories: Vec<(u32, u32)>,
    pe: Pe,
}

fn headers(data: &[u8]) -> Option<Headers<'_>> {
    if data.get(..2)? != b"MZ" {
        return None;
    }
    let at = usize::try_from(le_u32(data, 0x3C)?).ok()?;
    if data.get(at..at.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let machine = le_u16(data, at + 4)?;
    let sections_count = usize::from(le_u16(data, at + 6)?);
    let timestamp = le_u32(data, at + 8)?;
    let optional_size = usize::from(le_u16(data, at + 20)?);
    let characteristics = le_u16(data, at + 22)?;
    let optional = at + 24;
    let magic = le_u16(data, optional)?;
    let (dirs_at, count_at) = match magic {
        0x10B => (optional + 96, optional + 92),
        0x20B => (optional + 112, optional + 108),
        _ => return None,
    };
    let subsystem = le_u16(data, optional + 68)?;
    let dir_count = usize::try_from(le_u32(data, count_at)?).ok()?.min(16);
    let directories = (0..dir_count)
        .map(|i| Some((le_u32(data, dirs_at + i * 8)?, le_u32(data, dirs_at + i * 8 + 4)?)))
        .collect::<Option<Vec<_>>>()?;
    let table = optional.checked_add(optional_size)?;
    let sections = (0..sections_count.min(96))
        .map(|i| {
            let s = table + i * 40;
            Some((
                le_u32(data, s + 12)?,
                le_u32(data, s + 8)?,
                le_u32(data, s + 20)?,
                le_u32(data, s + 16)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let pe = Pe {
        machine: match machine {
            0x8664 => "x64".to_string(),
            0x14C => "x86".to_string(),
            0xAA64 => "ARM64".to_string(),
            0x1C0 | 0x1C4 => "ARM".to_string(),
            other => format!("machine {other:#x}"),
        },
        is_dll: characteristics & 0x2000 != 0,
        subsystem: match subsystem {
            2 => "graphical".to_string(),
            3 => "console".to_string(),
            1 => "native".to_string(),
            9 => "Windows CE".to_string(),
            10..=14 => "EFI".to_string(),
            _ => String::new(),
        },
        // a time in the years 1995 to 2100; many builds put a hash there instead
        built: (788_918_400..4_102_444_800).contains(&timestamp).then_some(timestamp),
        signed: directories
            .get(4)
            .is_some_and(|&(offset, size)| offset != 0 && size != 0),
        dotnet: directories.get(14).is_some_and(|&(rva, size)| rva != 0 && size != 0),
        strings: Vec::new(),
    };
    Some(Headers {
        image: Image { data, sections },
        directories,
        pe,
    })
}

/// A resource directory at `at` (an offset in the resource section): its entries as (name or id, offset, is a directory).
fn entries(root: &[u8], at: usize) -> Vec<(u32, usize, bool)> {
    let (Some(named), Some(ids)) = (le_u16(root, at + 12), le_u16(root, at + 14)) else {
        return Vec::new();
    };
    (0..(usize::from(named) + usize::from(ids)).min(MAX_ENTRIES))
        .filter_map(|i| {
            let entry = at + 16 + i * 8;
            let (name, offset) = (le_u32(root, entry)?, le_u32(root, entry + 4)?);
            Some((name, (offset & 0x7FFF_FFFF) as usize, offset & 0x8000_0000 != 0))
        })
        .collect()
}

/// The resources of type `kind` as (id, data): the first language of each.
fn resources<'a>(image: &'a Image<'_>, root: &'a [u8], kind: u32) -> Vec<(u32, &'a [u8])> {
    let mut found = Vec::new();
    for (type_id, offset, is_dir) in entries(root, 0) {
        if type_id != kind || !is_dir {
            continue;
        }
        for (id, offset, is_dir) in entries(root, offset) {
            if !is_dir {
                continue;
            }
            // the first language
            let Some((_, leaf, false)) = entries(root, offset).into_iter().next() else {
                continue;
            };
            let (Some(rva), Some(size)) = (le_u32(root, leaf), le_u32(root, leaf + 4)) else {
                continue;
            };
            if let Some(bytes) = image.rva(rva, usize::try_from(size).unwrap_or(usize::MAX).min(8 * 1024 * 1024)) {
                found.push((id, bytes));
            }
        }
    }
    found
}

/// A block of a version resource at `at`: (key, where its value begins, the length of the value in bytes, is text, end).
fn block(d: &[u8], at: usize) -> Option<(String, usize, usize, bool, usize)> {
    let length = usize::from(le_u16(d, at)?);
    let value_length = usize::from(le_u16(d, at + 2)?);
    let is_text = le_u16(d, at + 4)? == 1;
    let end = at.checked_add(length)?.min(d.len());
    let mut key_units = Vec::new();
    let mut p = at + 6;
    loop {
        let unit = le_u16(d, p)?;
        p += 2;
        if unit == 0 {
            break;
        }
        key_units.push(unit);
        if key_units.len() > 128 {
            return None;
        }
    }
    let value_at = (p + 3) & !3; // aligned to four bytes
    let value_bytes = if is_text { value_length * 2 } else { value_length };
    Some((
        String::from_utf16_lossy(&key_units),
        value_at,
        value_bytes,
        is_text,
        end,
    ))
}

fn version_strings(version: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some((key, value_at, value_bytes, _, end)) = block(version, 0) else {
        return out;
    };
    if key != "VS_VERSION_INFO" {
        return out;
    }
    // the children come after the fixed file info
    let mut p = (value_at + value_bytes + 3) & !3;
    while p + 6 <= end && out.len() < MAX_STRINGS {
        let Some((key, child_at, child_bytes, _, child_end)) = block(version, p) else {
            break;
        };
        if key == "StringFileInfo" {
            // the string tables (one per language): take the first
            let mut table = child_at;
            if let Some((_, strings_at, strings_bytes, _, table_end)) = block(version, table) {
                let mut s = (strings_at + strings_bytes + 3) & !3;
                while s + 6 <= table_end && out.len() < MAX_STRINGS {
                    let Some((name, text_at, text_bytes, is_text, string_end)) = block(version, s) else {
                        break;
                    };
                    if is_text
                        && let Some(raw) = version.get(text_at..text_at.saturating_add(text_bytes).min(string_end))
                    {
                        let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
                        let value = String::from_utf16_lossy(&units);
                        let value = value.trim_end_matches('\0').trim().to_string();
                        if !value.is_empty() {
                            out.push((name, value.chars().filter(|c| !c.is_control()).take(200).collect()));
                        }
                    }
                    if string_end <= s {
                        break;
                    }
                    s = (string_end + 3) & !3;
                }
                let _ = &mut table;
            }
            break;
        }
        if child_end <= p {
            break;
        }
        let _ = child_bytes;
        p = (child_end + 3) & !3;
    }
    out
}

/// The headers and the version strings of a PE file in `data`.
pub fn parse(data: &[u8]) -> Option<Pe> {
    let headers = headers(data)?;
    let mut pe = headers.pe.clone();
    if let Some(&(rva, size)) = headers.directories.get(2).filter(|(rva, size)| *rva != 0 && *size != 0)
        && let Some(root) = headers
            .image
            .rva(rva, usize::try_from(size).unwrap_or(0).min(64 * 1024 * 1024))
        && let Some((_, version)) = resources(&headers.image, root, 16).into_iter().next()
    {
        pe.strings = version_strings(version);
    }
    Some(pe)
}

/// The icon of a program: the biggest of the first group of icons, decoded.
pub fn icon(data: &[u8]) -> Option<RgbaImage> {
    let headers = headers(data)?;
    let &(rva, size) = headers
        .directories
        .get(2)
        .filter(|(rva, size)| *rva != 0 && *size != 0)?;
    let root = headers
        .image
        .rva(rva, usize::try_from(size).ok()?.min(64 * 1024 * 1024))?;
    let (_, group) = resources(&headers.image, root, 14).into_iter().next()?;
    let count = usize::from(le_u16(group, 4)?).min(32);
    // the entry with the most pixels and then the most colours
    let best = (0..count)
        .filter_map(|i| {
            let e = 6 + i * 14;
            let side = |b: u8| if b == 0 { 256 } else { u32::from(b) };
            Some((
                side(*group.get(e)?) * side(*group.get(e + 1)?),
                le_u16(group, e + 6)?,
                le_u16(group, e + 12)?,
            ))
        })
        .max()?;
    let (pixels, bits, id) = best;
    if u64::from(pixels) > MAX_ICON_PIXELS {
        return None;
    }
    let _ = bits;
    let icons = resources(&headers.image, root, 3);
    let (_, bytes) = icons.into_iter().find(|(i, _)| *i == u32::from(id))?;
    if bytes.starts_with(b"\x89PNG") {
        return decode::decode_bytes(bytes, MAX_ICON_PIXELS).ok();
    }
    // a DIB without the file header of a .ico: put one in front (width, height, colours, reserved, planes, bits, size, offset)
    let mut ico = vec![0u8, 0, 1, 0, 1, 0];
    let (w, h) = (le_u32(bytes, 4)?, le_u32(bytes, 8)? / 2);
    ico.extend([w.min(255) as u8, h.min(255) as u8, 0, 0, 1, 0]);
    ico.extend(le_u16(bytes, 14)?.to_le_bytes());
    ico.extend(u32::try_from(bytes.len()).ok()?.to_le_bytes());
    ico.extend(22u32.to_le_bytes());
    ico.extend(bytes);
    decode::decode_bytes(&ico, MAX_ICON_PIXELS).ok()
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    /// A real program of Windows, if this is Windows: the real thing, where there is one to look at.
    fn notepad() -> Option<Vec<u8>> {
        [
            r"C:\Windows\System32\notepad.exe",
            r"C:\Windows\notepad.exe",
            r"C:\Windows\System32\calc.exe",
        ]
        .iter()
        .find_map(|p| std::fs::read(p).ok())
    }

    #[test]
    fn what_is_no_program_gives_nothing() {
        assert!(parse(b"").is_none());
        assert!(parse(b"MZ").is_none());
        assert!(parse(&[b'M', b'Z', 0, 0]).is_none());
        let mut lying = vec![0u8; 0x100];
        lying[..2].copy_from_slice(b"MZ");
        lying[0x3C..0x40].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        assert!(parse(&lying).is_none());
        assert!(icon(b"MZ").is_none());
    }

    #[test]
    fn a_real_program_says_what_it_is_and_who_made_it_and_has_an_icon() {
        let Some(data) = notepad() else {
            return; // not Windows: nothing to look at
        };
        let pe = parse(&data).expect("a program");
        assert!(["x64", "x86", "ARM64"].contains(&pe.machine.as_str()), "{pe:?}");
        assert!(!pe.is_dll);
        assert!(
            pe.strings
                .iter()
                .any(|(k, v)| k == "CompanyName" && v.contains("Microsoft")),
            "{pe:?}"
        );
        assert!(pe.strings.iter().any(|(k, _)| k == "FileDescription"), "{pe:?}");
        let icon = icon(&data).expect("an icon");
        assert!(
            icon.width() >= 16 && icon.width() == icon.height(),
            "{}x{}",
            icon.width(),
            icon.height()
        );
        assert!(icon.pixels().any(|p| p.0[3] > 0), "not all clear");
    }

    #[test]
    fn a_program_cut_or_spoilt_is_read_as_far_as_it_goes_and_never_hurts() {
        let Some(data) = notepad() else {
            return;
        };
        for part in 0..=20 {
            let cut = &data[..data.len() * part / 20];
            let _ = parse(cut);
            let _ = icon(cut);
        }
        let mut seed = 5u32;
        for _ in 0..400 {
            let mut spoilt = data.clone();
            for _ in 0..24 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let at = (seed >> 8) as usize % spoilt.len().min(4096 + (seed as usize % 65_536));
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                spoilt[at] = (seed >> 16) as u8;
            }
            let _ = parse(&spoilt);
            let _ = icon(&spoilt);
        }
    }
}
