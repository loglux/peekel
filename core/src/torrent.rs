//! A `.torrent` file (BitTorrent, BEP 3; v2 and hybrids, BEP 52): what it would download, read for the card and the properties. The
//! file is bencode: integers `i42e`, byte strings `4:spam`, lists `l...e` and dictionaries `d...e`. Read here with limits (the size
//! of the file, the depth, the number of values), every length checked against what is left. No network: how many people share it
//! is not known from the file.

use std::ops::Range;
use std::time::{Duration, UNIX_EPOCH};

use crate::card;
use crate::sha;

/// A `.torrent` bigger than this is not read (the biggest have the hashes of a few hundred thousand pieces: a few MB).
pub const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// How deep lists and dictionaries may be nested (a v2 tree of files is as deep as its folders).
const MAX_DEPTH: usize = 64;
/// How many values one file may hold.
const MAX_VALUES: usize = 2_000_000;
/// How many of the files are named on the card, the largest first.
const SHOWN_FILES: usize = 10;

/// Whether the name says a `.torrent`.
pub fn is_torrent(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
}

/// A value of bencode, borrowing its bytes from the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    /// The keys in the order of the file, each with its value and where the value is in the file (the info-hash is of those bytes).
    Dict(Vec<(&'a [u8], Value<'a>, Range<usize>)>),
}

impl<'a> Value<'a> {
    fn get(&self, key: &str) -> Option<&Value<'a>> {
        self.entry(key).map(|(v, _)| v)
    }

    fn entry(&self, key: &str) -> Option<(&Value<'a>, Range<usize>)> {
        match self {
            Value::Dict(items) => items
                .iter()
                .find(|(k, _, _)| *k == key.as_bytes())
                .map(|(_, v, r)| (v, r.clone())),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    fn bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    fn text(&self) -> Option<String> {
        self.bytes().map(|b| String::from_utf8_lossy(b).into_owned())
    }

    fn list(&self) -> &[Value<'a>] {
        match self {
            Value::List(items) => items,
            _ => &[],
        }
    }
}

/// Why a file is not bencode that can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

struct Parser<'a> {
    data: &'a [u8],
    at: usize,
    values: usize,
}

impl<'a> Parser<'a> {
    fn fail<T>(&self, what: &str) -> Result<T, Error> {
        Err(Error(format!("{what} at byte {}", self.at)))
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.at).copied()
    }

    /// The digits up to `end`, as a number (an optional minus first).
    fn number(&mut self, end: u8) -> Result<i64, Error> {
        let start = self.at;
        let Some(len) = self.data[start..].iter().position(|&b| b == end) else {
            return self.fail("a number with no end");
        };
        let text = std::str::from_utf8(&self.data[start..start + len]).unwrap_or("");
        let Ok(n) = text.parse::<i64>() else {
            return self.fail("not a number");
        };
        self.at = start + len + 1;
        Ok(n)
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, Error> {
        self.values += 1;
        if self.values > MAX_VALUES {
            return self.fail("too many values");
        }
        if depth > MAX_DEPTH {
            return self.fail("nested too deep");
        }
        match self.peek() {
            Some(b'i') => {
                self.at += 1;
                Ok(Value::Int(self.number(b'e')?))
            }
            Some(b'0'..=b'9') => {
                let len = self.number(b':')?;
                let Ok(len) = usize::try_from(len) else {
                    return self.fail("a negative length");
                };
                if len > self.data.len() - self.at {
                    return self.fail("a string longer than the file");
                }
                let bytes = &self.data[self.at..self.at + len];
                self.at += len;
                Ok(Value::Bytes(bytes))
            }
            Some(b'l') => {
                self.at += 1;
                let mut items = Vec::new();
                while self.peek() != Some(b'e') {
                    if self.peek().is_none() {
                        return self.fail("a list with no end");
                    }
                    items.push(self.value(depth + 1)?);
                }
                self.at += 1;
                Ok(Value::List(items))
            }
            Some(b'd') => {
                self.at += 1;
                let mut items = Vec::new();
                while self.peek() != Some(b'e') {
                    if self.peek().is_none() {
                        return self.fail("a dictionary with no end");
                    }
                    let Value::Bytes(key) = self.value(depth + 1)? else {
                        return self.fail("a key that is not a string");
                    };
                    let start = self.at;
                    let value = self.value(depth + 1)?;
                    items.push((key, value, start..self.at));
                }
                self.at += 1;
                Ok(Value::Dict(items))
            }
            Some(_) => self.fail("not bencode"),
            None => self.fail("the file ends"),
        }
    }
}

/// Reads bencode: the one value the file holds.
pub fn parse(data: &[u8]) -> Result<Value<'_>, Error> {
    let mut parser = Parser { data, at: 0, values: 0 };
    let value = parser.value(0)?;
    if !matches!(value, Value::Dict(_)) {
        return Err(Error("not a dictionary".to_string()));
    }
    Ok(value)
}

/// One file the torrent downloads: its path (folders joined by `/`) and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
}

/// What a `.torrent` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Torrent {
    pub name: String,
    pub files: Vec<Entry>,
    pub piece_length: u64,
    pub pieces: u64,
    pub private: bool,
    pub trackers: Vec<String>,
    pub web_seeds: usize,
    pub created_by: String,
    pub created: Option<i64>,
    pub comment: String,
    /// SHA-1 of the info dictionary (BitTorrent v1 and hybrids).
    pub hash_v1: Option<String>,
    /// SHA-256 of it (v2 and hybrids).
    pub hash_v2: Option<String>,
}

/// The text of a key, its `.utf-8` twin first (old torrents put the UTF-8 name beside one in another code page).
fn text(dict: &Value<'_>, key: &str) -> String {
    dict.get(&format!("{key}.utf-8"))
        .or_else(|| dict.get(key))
        .and_then(Value::text)
        .unwrap_or_default()
}

/// The path of a v1 file: its list of names.
fn path_of(file: &Value<'_>) -> String {
    let parts = file.get("path.utf-8").or_else(|| file.get("path"));
    parts
        .map(|p| p.list().iter().filter_map(Value::text).collect::<Vec<_>>().join("/"))
        .unwrap_or_default()
}

/// The files of a v2 tree: a dictionary of names, a file being one whose only key is the empty one, holding its `length`.
fn walk(tree: &Value<'_>, prefix: &str, out: &mut Vec<Entry>) {
    let Value::Dict(items) = tree else {
        return;
    };
    for (name, value, _) in items {
        if name.is_empty() {
            if let Some(size) = value.get("length").and_then(Value::int) {
                out.push(Entry {
                    path: prefix.trim_end_matches('/').to_string(),
                    size: size.max(0) as u64,
                });
            }
        } else if out.len() < MAX_VALUES {
            walk(value, &format!("{prefix}{}/", String::from_utf8_lossy(name)), out);
        }
    }
}

/// Reads what a `.torrent` says.
pub fn read(data: &[u8]) -> Result<Torrent, Error> {
    let root = parse(data)?;
    let Some((info, span)) = root.entry("info") else {
        return Err(Error("no info dictionary".to_string()));
    };
    if !matches!(info, Value::Dict(_)) {
        return Err(Error("the info is not a dictionary".to_string()));
    }
    let raw = &data[span];
    let v2 = info.get("meta version").and_then(Value::int) == Some(2);
    let v1 = info.get("pieces").is_some();
    let mut torrent = Torrent {
        name: text(info, "name"),
        piece_length: info.get("piece length").and_then(Value::int).unwrap_or(0).max(0) as u64,
        private: info.get("private").and_then(Value::int) == Some(1),
        created_by: text(&root, "created by"),
        created: root.get("creation date").and_then(Value::int),
        comment: text(&root, "comment"),
        hash_v1: (v1 || !v2).then(|| sha::hex(&sha::sha1(raw))),
        hash_v2: v2.then(|| sha::hex(&sha::sha256(raw))),
        ..Torrent::default()
    };
    if let Some(files) = info.get("files") {
        for file in files.list() {
            // BEP 47: a padding file (attr with `p`) is no file of the user's
            if file
                .get("attr")
                .and_then(Value::bytes)
                .is_some_and(|a| a.contains(&b'p'))
            {
                continue;
            }
            torrent.files.push(Entry {
                path: path_of(file),
                size: file.get("length").and_then(Value::int).unwrap_or(0).max(0) as u64,
            });
        }
    } else if let Some(length) = info.get("length").and_then(Value::int) {
        torrent.files.push(Entry {
            path: torrent.name.clone(),
            size: length.max(0) as u64,
        });
    } else if let Some(tree) = info.get("file tree") {
        walk(tree, "", &mut torrent.files);
    }
    torrent.pieces = match info.get("pieces").and_then(Value::bytes) {
        Some(pieces) => pieces.len() as u64 / 20,
        None if torrent.piece_length > 0 => {
            let total: u64 = torrent.files.iter().map(|f| f.size).sum();
            total.div_ceil(torrent.piece_length)
        }
        None => 0,
    };
    let mut trackers: Vec<String> = Vec::new();
    let mut add = |url: Option<String>| {
        if let Some(host) = url.as_deref().and_then(host_of)
            && !trackers.contains(&host)
        {
            trackers.push(host);
        }
    };
    add(root.get("announce").and_then(Value::text));
    for tier in root.get("announce-list").map(Value::list).unwrap_or_default() {
        for url in tier.list() {
            add(url.text());
        }
    }
    torrent.trackers = trackers;
    torrent.web_seeds = match root.get("url-list") {
        Some(Value::Bytes(b)) if !b.is_empty() => 1,
        Some(Value::List(items)) => items.len(),
        _ => 0,
    };
    Ok(torrent)
}

/// The host of a tracker's address: only that is shown, as the rest of the address of a private tracker holds the user's own key.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    (!host.is_empty()).then(|| host.to_string())
}

/// The rows of the card and the properties of a `.torrent` (none when it cannot be read: the card then says only its size).
pub fn properties(data: &[u8]) -> Vec<(String, String)> {
    let Ok(torrent) = read(data) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let mut push = |label: &str, value: String| {
        if !value.trim().is_empty() {
            rows.push((label.to_string(), value));
        }
    };
    push("Name", torrent.name.clone());
    let total: u64 = torrent.files.iter().map(|f| f.size).sum();
    let count = torrent.files.len();
    push(
        "Downloads",
        format!(
            "{} in {count} {}",
            card::human_size(total),
            if count == 1 { "file" } else { "files" }
        ),
    );
    if torrent.piece_length > 0 {
        push(
            "Pieces",
            format!(
                "{} of {}",
                crate::info::grouped(torrent.pieces),
                card::human_size(torrent.piece_length)
            ),
        );
    }
    let mut made = torrent
        .created
        .filter(|s| *s > 0)
        .map(|s| card::date(UNIX_EPOCH + Duration::from_secs(s as u64)))
        .unwrap_or_default();
    if !torrent.created_by.is_empty() {
        made = if made.is_empty() {
            format!("by {}", torrent.created_by)
        } else {
            format!("{made}, by {}", torrent.created_by)
        };
    }
    push("Made", made);
    if torrent.private {
        push(
            "Private",
            "yes: its own trackers only (no DHT, no peer exchange)".to_string(),
        );
    }
    if let Some((first, rest)) = torrent.trackers.split_first() {
        push(
            "Tracker",
            if rest.is_empty() {
                first.clone()
            } else {
                format!("{first} (and {} more)", rest.len())
            },
        );
    }
    if torrent.web_seeds > 0 {
        push("Web seeds", torrent.web_seeds.to_string());
    }
    push("Comment", torrent.comment.chars().take(300).collect());
    if let Some(hash) = &torrent.hash_v1 {
        push("Info-hash", hash.clone());
    }
    if let Some(hash) = &torrent.hash_v2 {
        push("Info-hash v2", hash.clone());
    }
    if count > 1 {
        let mut largest: Vec<&Entry> = torrent.files.iter().collect();
        largest.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
        for file in largest.iter().take(SHOWN_FILES) {
            push(&card::human_size(file.size), file.path.chars().take(120).collect());
        }
        if count > SHOWN_FILES {
            push("", format!("and {} more files", count - SHOWN_FILES));
        }
    }
    rows
}

/// The magnet link of the `.torrent` at `path`, when it can be read.
pub fn magnet_of(path: &std::path::Path) -> Option<String> {
    let data = crate::guard::read_limited(path, MAX_BYTES).ok()?;
    magnet(&read(&data).ok()?)
}

/// The magnet link of a torrent: its info-hash and name (the trackers are left out, as their addresses may hold a user's key).
pub fn magnet(torrent: &Torrent) -> Option<String> {
    let mut link = match (&torrent.hash_v1, &torrent.hash_v2) {
        (Some(v1), Some(v2)) => format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}"),
        (Some(v1), None) => format!("magnet:?xt=urn:btih:{v1}"),
        (None, Some(v2)) => format!("magnet:?xt=urn:btmh:1220{v2}"),
        (None, None) => return None,
    };
    if !torrent.name.is_empty() {
        link.push_str("&dn=");
        for byte in torrent.name.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                link.push(byte as char);
            } else {
                link.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Some(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A v1 torrent of two files and a padding file, with a private tracker whose address holds a key.
    fn two_files() -> Vec<u8> {
        let info = b"d5:filesld6:lengthi3000e4:pathl2:CD6:a.flaceed4:attr1:p6:lengthi96e4:pathl4:.pad2:96eed6:lengthi5000e4:pathl5:b.txteee4:name5:Album12:piece lengthi4096e6:pieces40:aaaaaaaaaaaaaaaaaaaabbbbbbbbbbbbbbbbbbbb7:privatei1ee";
        let mut data = b"d8:announce42:https://tracker.example.org/ann?key=SECRET13:announce-listll42:https://tracker.example.org/ann?key=SECRETel18:udp://other.org:80ee7:comment5:hello10:created by13:qBittorrent 413:creation datei1700000000e4:info".to_vec();
        data.extend_from_slice(info);
        data.push(b'e');
        data
    }

    #[test]
    fn a_torrent_is_read_with_its_files_trackers_and_hash() {
        let data = two_files();
        let torrent = read(&data).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(torrent.name, "Album");
        assert_eq!(
            torrent.files,
            vec![
                Entry {
                    path: "CD/a.flac".to_string(),
                    size: 3000
                },
                Entry {
                    path: "b.txt".to_string(),
                    size: 5000
                }
            ],
            "the padding file is left out"
        );
        assert_eq!((torrent.piece_length, torrent.pieces), (4096, 2));
        assert!(torrent.private);
        assert_eq!(torrent.trackers, vec!["tracker.example.org", "other.org:80"]);
        assert_eq!(torrent.created_by, "qBittorrent 4");
        // the hash is of the info dictionary's own bytes, as they stand in the file
        let start = data.windows(7).position(|w| w == b"4:infod").map_or(0, |p| p + 6);
        assert_eq!(
            torrent.hash_v1,
            Some(sha::hex(&sha::sha1(&data[start..data.len() - 1])))
        );
        assert_eq!(torrent.hash_v2, None);
        let rows = properties(&data);
        let text = format!("{rows:?}");
        assert!(text.contains("7.8 KB in 2 files"), "{text}");
        assert!(!text.contains("SECRET"), "a tracker's key is not shown: {text}");
        assert!(text.contains("2023-11-14 22:13 UTC, by qBittorrent 4"), "{text}");
        assert_eq!(
            rows.iter().position(|(l, _)| l == "4.9 KB").map(|i| rows[i].1.as_str()),
            Some("b.txt")
        );
        let link = magnet(&torrent).unwrap_or_default();
        assert!(
            link.starts_with("magnet:?xt=urn:btih:") && link.ends_with("&dn=Album"),
            "{link}"
        );
    }

    #[test]
    fn a_torrent_of_version_2_has_its_tree_and_a_sha_256() {
        let data = b"d4:infod9:file treed4:Bookd5:a.pdfd0:d6:lengthi10e11:pieces root32:rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrreeee12:meta versioni2e4:name4:Book12:piece lengthi16384eee";
        let torrent = read(data).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(
            torrent.files,
            vec![Entry {
                path: "Book/a.pdf".to_string(),
                size: 10
            }]
        );
        assert_eq!(torrent.hash_v1, None);
        assert_eq!(torrent.hash_v2.as_ref().map(String::len), Some(64));
        assert_eq!(torrent.pieces, 1);
    }

    #[test]
    fn spoilt_bencode_is_an_error_and_not_a_crash() {
        let deep = "l".repeat(100_000);
        for bad in [
            &b""[..],
            b"d",
            b"i12",
            b"d4:infoi1ee",
            b"d999999999:xe",
            b"d-5:xe",
            b"l1:ae",
            b"d4:info",
            b"\xff\xfe",
            deep.as_bytes(),
        ] {
            assert!(
                read(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(&bad[..bad.len().min(20)])
            );
            let _ = properties(bad);
        }
        // a million values in a list: refused by the count, not by memory
        let mut many = b"d4:infol".to_vec();
        many.extend(std::iter::repeat_n(&b"i1e"[..], 2_100_000).flatten());
        many.extend_from_slice(b"ee");
        assert!(read(&many).is_err());
    }
}
