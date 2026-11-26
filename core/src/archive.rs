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
        let (count, _, at) = end_record(data)?;
        let entries = directory(data, 0, count, at)?;
        Ok(Self { data, entries })
    }

    /// Whether there is an entry of this name (capitals do not matter).
    pub fn contains(&self, name: &str) -> bool {
        find(&self.entries, name).is_some()
    }

    /// The entries as (name, size packed, size unpacked as the archive says).
    pub fn entries(&self) -> impl Iterator<Item = (&str, u64, u64)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.compressed, e.size))
    }

    /// The names of the entries.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// The entry `name` as text (what is not UTF-8 is replaced), at most `limit` bytes; `None` when it cannot be read.
    pub fn text(&self, name: &str, limit: usize) -> Option<String> {
        self.read(name, limit)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The unpacked content of the entry `name`, at most `limit` bytes (more is `TooLarge`).
    pub fn read(&self, name: &str, limit: usize) -> Result<Vec<u8>, DecodeError> {
        let entry = find(&self.entries, name).ok_or_else(|| missing(name))?;
        unpack(entry, self.data.get(entry.offset..).unwrap_or_default(), limit)
    }
}

/// A ZIP on disk whose entries are read where they lie, not the whole file: for what needs a few small entries of a large file (the
/// title of an EPUB, the start of a zipped FB2).
#[derive(Debug)]
pub struct OnDisk {
    file: std::fs::File,
    len: u64,
    entries: Vec<Entry>,
}

/// The longest central directory read from a ZIP on disk.
const MAX_DIRECTORY: u32 = 16 * 1024 * 1024;

impl OnDisk {
    /// Reads the list of entries of the ZIP at `path` from its end.
    pub fn open(path: &std::path::Path) -> Result<Self, DecodeError> {
        let mut file = std::fs::File::open(path).map_err(crate::guard::unreadable)?;
        let len = file.metadata().map_err(crate::guard::unreadable)?.len();
        let from = len.saturating_sub(22 + 65_535);
        let tail = read_at(&mut file, from, (len - from) as usize)?;
        let (count, size, at) = end_record(&tail)?;
        if size > MAX_DIRECTORY {
            return Err(bad("the directory is too long"));
        }
        let held = read_at(&mut file, u64::from(at), size as usize)?;
        let entries = directory(&held, at as usize, count, at)?;
        Ok(Self { file, len, entries })
    }

    /// The names of the entries.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// The unpacked content of the entry `name`, at most `limit` bytes (more is `TooLarge`).
    pub fn read(&mut self, name: &str, limit: usize) -> Result<Vec<u8>, DecodeError> {
        let (method, start, packed) = self.local(name)?;
        // a packed entry longer than the limit allows at the best packing of deflate (about 1 : 1032) is not read
        if packed > (limit as u64).saturating_mul(1032).max(1024) {
            return Err(DecodeError::TooLarge);
        }
        let data = read_at(&mut self.file, start, packed as usize)?;
        unpack_data(method, &data, packed, limit)
    }

    /// The first `limit` bytes (or fewer) of the entry `name` unpacked, reading no more of the packed entry than they need.
    pub fn head(&mut self, name: &str, limit: usize) -> Result<Vec<u8>, DecodeError> {
        use std::io::{Seek, SeekFrom};
        let (method, start, packed) = self.local(name)?;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(crate::guard::unreadable)?;
        let source = (&mut self.file).take(packed);
        let mut out = Vec::new();
        match method {
            0 => source.take(limit as u64).read_to_end(&mut out),
            8 => DeflateDecoder::new(source).take(limit as u64).read_to_end(&mut out),
            _ => return Err(bad("a method of packing is not read")),
        }
        .map_err(|_| bad("an entry is damaged"))?;
        Ok(out)
    }

    /// The method of the entry `name`, where its packed data begin in the file, and how long they are.
    fn local(&mut self, name: &str) -> Result<(u16, u64, u64), DecodeError> {
        let entry = find(&self.entries, name).ok_or_else(|| missing(name))?;
        let (method, at, packed) = (entry.method, entry.offset as u64, entry.compressed);
        let header = read_at(&mut self.file, at, 30)?;
        if !header.starts_with(b"PK\x03\x04") {
            return Err(bad("a local header is damaged"));
        }
        let name_len = u64::from(le_u16(&header, 26).ok_or_else(|| bad("cut"))?);
        let extra_len = u64::from(le_u16(&header, 28).ok_or_else(|| bad("cut"))?);
        let start = at + 30 + name_len + extra_len;
        if start.saturating_add(packed) > self.len {
            return Err(bad("cut"));
        }
        Ok((method, start, packed))
    }
}

/// `n` bytes of `file` from `at` (fewer at its end).
fn read_at(file: &mut std::fs::File, at: u64, n: usize) -> Result<Vec<u8>, DecodeError> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(at)).map_err(crate::guard::unreadable)?;
    let mut out = Vec::with_capacity(n.min(1 << 20));
    file.take(n as u64)
        .read_to_end(&mut out)
        .map_err(crate::guard::unreadable)?;
    Ok(out)
}

fn missing(name: &str) -> DecodeError {
    DecodeError::Unreadable(format!("the archive has no {name}"))
}

fn find<'e>(entries: &'e [Entry], name: &str) -> Option<&'e Entry> {
    entries
        .iter()
        .find(|e| e.name == name)
        .or_else(|| entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)))
}

/// The end-of-central-directory record at the end of `data`: how many entries, the size of the directory and where it begins.
fn end_record(data: &[u8]) -> Result<(usize, u32, u32), DecodeError> {
    // the record is 22 bytes and a comment of up to 64 KB after it: searched from the end
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
    Ok((count, size, at))
}

/// The `count` entries of the directory that begins at `at` in the file, read from `data`, the bytes of the file from `base` on.
fn directory(data: &[u8], base: usize, count: usize, at: u32) -> Result<Vec<Entry>, DecodeError> {
    let mut entries = Vec::with_capacity(count);
    let mut cursor = (at as usize).checked_sub(base).ok_or_else(|| bad("cut"))?;
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
    Ok(entries)
}

/// The content of `entry` from `local`, the bytes of the file from its local header on, at most `limit` bytes (more is `TooLarge`).
fn unpack(entry: &Entry, local: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    if !local.starts_with(b"PK\x03\x04") {
        return Err(bad("a local header is damaged"));
    }
    let name_len = le_u16(local, 26).ok_or_else(|| bad("cut"))? as usize;
    let extra_len = le_u16(local, 28).ok_or_else(|| bad("cut"))? as usize;
    unpack_data(
        entry.method,
        local.get(30 + name_len + extra_len..).ok_or_else(|| bad("cut"))?,
        entry.compressed,
        limit,
    )
}

/// The first `packed` bytes of `data` (the packed data of an entry, and perhaps more after them) unpacked by `method`, at most
/// `limit` bytes (more is `TooLarge`).
fn unpack_data(method: u16, data: &[u8], packed: u64, limit: usize) -> Result<Vec<u8>, DecodeError> {
    let end = usize::try_from(packed).map_err(|_| bad("a size is absurd"))?;
    let packed = data.get(..end).ok_or_else(|| bad("cut"))?;
    match method {
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
