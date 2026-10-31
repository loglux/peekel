//! A small reader of ZIP files, for the documents that are one (docx, pptx, xlsx, odt): only what is needed, with limits,
//! because an archive is a favourite shape of a file made to hurt.
//!
//! Read: stored and deflated entries, found by the central directory at the end. Not read: ZIP64, encryption, other
//! methods, split archives (all are errors). The sizes the archive declares are not trusted: an entry is unpacked with a
//! limit and refused when it is longer.

use std::io::Read;

use flate2::read::DeflateDecoder;

use crate::decode::DecodeError;
use crate::guard::{le_u16, le_u32};

/// More entries than this and the archive is refused (a real document has some dozens).
const MAX_ENTRIES: usize = 20_000;
/// Longest name of an entry that is kept.
const MAX_NAME: usize = 1024;

#[derive(Debug)]
struct Entry {
    name: String,
    method: u16,
    compressed: u64,
    /// The size the archive says the entry has when unpacked (not trusted: only for showing).
    size: u64,
    offset: usize,
}

/// The entries of a ZIP held in memory.
pub struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl std::fmt::Debug for Archive<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

fn bad(why: &str) -> DecodeError {
    DecodeError::Unreadable(format!("the archive is not readable ({why})"))
}

impl<'a> Archive<'a> {
    /// Reads the list of entries of `data`.
    pub fn new(data: &'a [u8]) -> Result<Self, DecodeError> {
        // the end-of-central-directory record is 22 bytes and a comment of up to 64 KB after it: searched from the end
        let from = data.len().saturating_sub(22 + 65_535);
        let end = (from..data.len().saturating_sub(21))
            .rev()
            .find(|&i| data[i..].starts_with(b"PK\x05\x06"))
            .ok_or_else(|| bad("no end of directory"))?;
        let count = le_u16(data, end + 10).ok_or_else(|| bad("cut"))? as usize;
        let size = le_u32(data, end + 12).ok_or_else(|| bad("cut"))?;
        let at = le_u32(data, end + 16).ok_or_else(|| bad("cut"))?;
        if count == 0xFFFF || size == 0xFFFF_FFFF || at == 0xFFFF_FFFF {
            return Err(bad("ZIP64 is not read"));
        }
        if count > MAX_ENTRIES {
            return Err(bad("too many entries"));
        }
        let mut entries = Vec::with_capacity(count);
        let mut cursor = at as usize;
        for _ in 0..count {
            if data.get(cursor..cursor + 4) != Some(b"PK\x01\x02") {
                return Err(bad("the directory is damaged"));
            }
            let flags = le_u16(data, cursor + 8).ok_or_else(|| bad("cut"))?;
            let method = le_u16(data, cursor + 10).ok_or_else(|| bad("cut"))?;
            let compressed = u64::from(le_u32(data, cursor + 20).ok_or_else(|| bad("cut"))?);
            let size = u64::from(le_u32(data, cursor + 24).ok_or_else(|| bad("cut"))?);
            let name_len = le_u16(data, cursor + 28).ok_or_else(|| bad("cut"))? as usize;
            let extra_len = le_u16(data, cursor + 30).ok_or_else(|| bad("cut"))? as usize;
            let comment_len = le_u16(data, cursor + 32).ok_or_else(|| bad("cut"))? as usize;
            let offset = le_u32(data, cursor + 42).ok_or_else(|| bad("cut"))? as usize;
            let name = data
                .get(cursor + 46..cursor + 46 + name_len)
                .ok_or_else(|| bad("cut"))?;
            if flags & 1 != 0 {
                return Err(bad("it is encrypted"));
            }
            if name_len <= MAX_NAME {
                entries.push(Entry {
                    name: String::from_utf8_lossy(name).replace('\\', "/"),
                    method,
                    compressed,
                    size,
                    offset,
                });
            }
            cursor += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { data, entries })
    }

    /// Whether there is an entry of this name (capitals do not matter).
    pub fn contains(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// The entries as (name, size packed, size unpacked as the archive says).
    pub fn entries(&self) -> impl Iterator<Item = (&str, u64, u64)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.compressed, e.size))
    }

    /// The names of the entries.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .or_else(|| self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)))
    }

    /// The entry `name` as text (what is not UTF-8 is replaced), at most `limit` bytes; `None` when it cannot be read.
    pub fn text(&self, name: &str, limit: usize) -> Option<String> {
        self.read(name, limit)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The unpacked content of the entry `name`, at most `limit` bytes (more is `TooLarge`).
    pub fn read(&self, name: &str, limit: usize) -> Result<Vec<u8>, DecodeError> {
        let entry = self
            .find(name)
            .ok_or_else(|| DecodeError::Unreadable(format!("the archive has no {name}")))?;
        let at = entry.offset;
        if self.data.get(at..at + 4) != Some(b"PK\x03\x04") {
            return Err(bad("a local header is damaged"));
        }
        let name_len = le_u16(self.data, at + 26).ok_or_else(|| bad("cut"))? as usize;
        let extra_len = le_u16(self.data, at + 28).ok_or_else(|| bad("cut"))? as usize;
        let start = at + 30 + name_len + extra_len;
        let end = usize::try_from(entry.compressed)
            .ok()
            .and_then(|n| start.checked_add(n))
            .ok_or_else(|| bad("a size is absurd"))?;
        let packed = self.data.get(start..end).ok_or_else(|| bad("cut"))?;
        match entry.method {
            0 if packed.len() <= limit => Ok(packed.to_vec()),
            0 => Err(DecodeError::TooLarge),
            8 => {
                let mut out = Vec::new();
                DeflateDecoder::new(packed)
                    .take(limit as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| bad("an entry is damaged"))?;
                if out.len() > limit {
                    return Err(DecodeError::TooLarge);
                }
                Ok(out)
            }
            _ => Err(bad("a method of packing is not read")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
pub(crate) mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    /// A ZIP of (name, bytes, deflated) made by hand.
    pub(crate) fn zip(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut directory = Vec::new();
        for (name, bytes, deflated) in files {
            let packed = if *deflated {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
                encoder.write_all(bytes).expect("writes");
                encoder.finish().expect("finishes")
            } else {
                bytes.to_vec()
            };
            let method: u16 = if *deflated { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(b"PK\x03\x04");
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0u8; 8]); // time, date, crc (not checked)
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((bytes.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&packed);
            directory.extend(b"PK\x01\x02");
            directory.extend([20, 0, 20, 0, 0, 0]);
            directory.extend(method.to_le_bytes());
            directory.extend([0u8; 8]);
            directory.extend((packed.len() as u32).to_le_bytes());
            directory.extend((bytes.len() as u32).to_le_bytes());
            directory.extend((name.len() as u16).to_le_bytes());
            directory.extend([0u8; 12]);
            directory.extend(offset.to_le_bytes());
            directory.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend(&directory);
        out.extend(b"PK\x05\x06");
        out.extend([0u8; 4]);
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((files.len() as u16).to_le_bytes());
        out.extend((directory.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn stored_and_deflated_entries_are_read_and_names_are_found_without_regard_to_capitals() {
        let data = zip(&[
            ("a.txt", b"stored", false),
            ("dir/b.xml", b"<x>deflated deflated deflated</x>", true),
        ]);
        let archive = Archive::new(&data).expect("reads");
        assert_eq!(archive.read("a.txt", 100).expect("a"), b"stored");
        assert_eq!(
            archive.read("DIR/B.XML", 100).expect("b"),
            b"<x>deflated deflated deflated</x>"
        );
        assert!(archive.contains("dir/b.xml") && !archive.contains("c"));
        assert!(archive.read("c", 10).is_err());
        assert_eq!(archive.names().count(), 2);
    }

    #[test]
    fn an_entry_over_the_limit_is_refused_whatever_the_archive_says_of_its_size() {
        let big = vec![b' '; 5_000_000];
        let data = zip(&[("big.xml", &big, true)]);
        assert!(data.len() < 100_000, "a bomb: {} bytes", data.len());
        let archive = Archive::new(&data).expect("reads");
        assert!(matches!(archive.read("big.xml", 1_000_000), Err(DecodeError::TooLarge)));
        assert_eq!(archive.read("big.xml", 6_000_000).expect("within").len(), 5_000_000);
        let stored = zip(&[("s", &big, false)]);
        assert!(matches!(
            Archive::new(&stored).expect("reads").read("s", 1000),
            Err(DecodeError::TooLarge)
        ));
    }

    #[test]
    fn rubbish_and_cut_archives_are_errors_and_not_panics() {
        assert!(Archive::new(b"").is_err());
        assert!(Archive::new(b"PK\x05\x06").is_err());
        assert!(Archive::new(&[0x50; 100_000]).is_err());
        let data = zip(&[("a.txt", b"hello hello hello", true), ("b", b"x", false)]);
        for cut in 0..data.len() {
            if let Ok(archive) = Archive::new(&data[..cut]) {
                let _ = archive.read("a.txt", 100);
                let _ = archive.read("b", 100);
            }
        }
        let mut spoilt = data.clone();
        for i in (0..spoilt.len()).step_by(7) {
            spoilt[i] ^= 0xA5;
            if let Ok(archive) = Archive::new(&spoilt) {
                let _ = archive.read("a.txt", 100);
            }
        }
    }
}
