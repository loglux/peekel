//! The title and the author of a PDF read from where they lie, not from the whole file (#235, the second round, item 4: a PDF of
//! hundreds of megabytes showed its file's name, as `info::of` reads a PDF whole only up to 64 MB).
//!
//! From the end: `startxref`, the cross-reference section it names (a table or a stream, PDF 1.5) and those before it (`/Prev`,
//! `/XRefStm`), the trailer's `/Info`, and that one object (also in an object stream). A few small reads; a stream is read and unpacked
//! with limits. What this cannot read (an encrypted file, whose strings are encrypted; a damaged cross-reference that a reader would
//! rebuild by scanning the file) is `None`, and the caller may read the file whole instead.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

/// How much of the end is read for `startxref`.
const TAIL: u64 = 4096;
/// How much is read for an object, a trailer or the head of a stream.
const CHUNK: usize = 64 * 1024;
/// The longest stream (packed) read, and the longest unpacked.
const MAX_STREAM: usize = 16 * 1024 * 1024;
const MAX_UNPACKED: u64 = 64 * 1024 * 1024;
/// How many cross-reference sections are followed, and how many subsections of a table are looked at.
const MAX_SECTIONS: usize = 64;
const MAX_SUBSECTIONS: usize = 100_000;
/// How deep arrays and dictionaries may nest.
const MAX_DEPTH: usize = 32;
/// The longest title or author kept, in characters.
const MAX_TEXT: usize = 300;

/// The title and the author of the PDF at `path` (either may be `None`); `None` when they cannot be read this way.
pub(crate) fn title_and_author(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let mut file = Reader::open(path)?;
    let tail = file.at(file.len.saturating_sub(TAIL), TAIL as usize)?;
    let at = rfind(&tail, b"startxref")?;
    let first = Lexer::new(&tail[at + 9..]).uint()?;
    let mut sections = Vec::new();
    let (mut info, mut encrypted) = (None, false);
    let mut next = vec![first];
    let mut seen = Vec::new();
    while let Some(at) = next.pop() {
        if seen.contains(&at) {
            continue;
        }
        seen.push(at);
        if seen.len() > MAX_SECTIONS {
            break;
        }
        let Some(trailer) = file.section(at, &mut sections) else {
            // the first section must be read; an older one that cannot be is left out
            if sections.is_empty() {
                return None;
            }
            continue;
        };
        if info.is_none()
            && let Some(Obj::Ref(n, _)) = get(&trailer, b"Info")
        {
            info = Some(*n);
        }
        encrypted |= get(&trailer, b"Encrypt").is_some();
        // the section before this one is read after the stream of a hybrid file (`/XRefStm`), which stands for this section
        if let Some(prev) = get(&trailer, b"Prev").and_then(Obj::uint) {
            next.push(prev);
        }
        if let Some(stream) = get(&trailer, b"XRefStm").and_then(Obj::uint) {
            next.push(stream);
        }
    }
    if encrypted {
        return None;
    }
    let dict = match file.object(&sections, info?, 0)? {
        Obj::Dict(dict) => dict,
        _ => return None,
    };
    let mut text = |key: &[u8]| {
        let value = match get(&dict, key)? {
            Obj::Ref(n, _) => file.object(&sections, *n, 1)?,
            other => other.clone(),
        };
        match value {
            Obj::Str(bytes) => Some(crate::pdf::text_string(&bytes, MAX_TEXT)).filter(|t| !t.trim().is_empty()),
            _ => None,
        }
    };
    Some((text(b"Title"), text(b"Author")))
}

/// Where an object is, by a cross-reference section.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    Free,
    At(u64),
    /// In the object stream numbered so, at this index.
    InStream(u32, u32),
}

enum Section {
    /// Subsections: (first number, count, where their 20-byte entries begin in the file).
    Table(Vec<(u32, u32, u64)>),
    /// The unpacked entries of a cross-reference stream: the widths of their fields, the subsections, the entries.
    Stream {
        widths: [usize; 3],
        index: Vec<(u32, u32)>,
        data: Vec<u8>,
    },
}

struct Reader {
    file: File,
    len: u64,
}

impl Reader {
    fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Self { file, len })
    }

    /// At most `n` bytes from `at`.
    fn at(&mut self, at: u64, n: usize) -> Option<Vec<u8>> {
        if at >= self.len {
            return None;
        }
        self.file.seek(SeekFrom::Start(at)).ok()?;
        let mut out = Vec::with_capacity(n.min(CHUNK));
        (&mut self.file).take(n as u64).read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// Reads the cross-reference section at `at` into `sections` (after those read before, which are newer) and gives its trailer.
    fn section(&mut self, at: u64, sections: &mut Vec<Section>) -> Option<Vec<(Vec<u8>, Obj)>> {
        let head = self.at(at, CHUNK)?;
        let mut lexer = Lexer::new(&head);
        lexer.skip_space();
        if lexer.rest().starts_with(b"xref") {
            lexer.pos += 4;
            return self.table(at + lexer.pos as u64, sections);
        }
        // a cross-reference stream: `n g obj << ... >> stream`
        let (Obj::Dict(dict), Some(data_at)) = self.object_at(at)? else {
            return None;
        };
        if get(&dict, b"Type") != Some(&Obj::Name(b"XRef".to_vec())) {
            return None;
        }
        let data = self.stream(&dict, data_at, sections)?;
        let widths: Vec<usize> = match get(&dict, b"W")? {
            Obj::Array(items) => items.iter().filter_map(Obj::uint).map(|w| w as usize).collect(),
            _ => return None,
        };
        let [a, b, c] = widths[..] else { return None };
        if a > 8 || b > 8 || c > 8 || a + b + c == 0 {
            return None;
        }
        let index = match get(&dict, b"Index") {
            Some(Obj::Array(items)) => items
                .chunks(2)
                .filter_map(|pair| Some((pair.first()?.uint()? as u32, pair.get(1)?.uint()? as u32)))
                .collect(),
            _ => vec![(0, get(&dict, b"Size")?.uint()? as u32)],
        };
        sections.push(Section::Stream {
            widths: [a, b, c],
            index,
            data,
        });
        Some(dict)
    }

    /// A table whose first subsection begins at `at` (just after `xref`), and its trailer.
    fn table(&mut self, mut at: u64, sections: &mut Vec<Section>) -> Option<Vec<(Vec<u8>, Obj)>> {
        let mut subsections = Vec::new();
        for _ in 0..MAX_SUBSECTIONS {
            let head = self.at(at, 256)?;
            let mut lexer = Lexer::new(&head);
            lexer.skip_space();
            if lexer.rest().starts_with(b"trailer") {
                let trailer = self.at(at + (lexer.pos + 7) as u64, CHUNK)?;
                let Some(Obj::Dict(dict)) = Lexer::new(&trailer).object(0) else {
                    return None;
                };
                sections.push(Section::Table(subsections));
                return Some(dict);
            }
            let (first, count) = (lexer.uint()?, lexer.uint()?);
            lexer.skip_space();
            let entries = at + lexer.pos as u64;
            subsections.push((u32::try_from(first).ok()?, u32::try_from(count).ok()?, entries));
            at = entries.checked_add(count.checked_mul(20)?)?;
            if at >= self.len {
                return None;
            }
        }
        None
    }

    /// The object at `at` (`n g obj ...`), and where its stream's data begin when it has one.
    fn object_at(&mut self, at: u64) -> Option<(Obj, Option<u64>)> {
        let head = self.at(at, CHUNK)?;
        let mut lexer = Lexer::new(&head);
        lexer.uint()?;
        lexer.uint()?;
        lexer.skip_space();
        if !lexer.rest().starts_with(b"obj") {
            return None;
        }
        lexer.pos += 3;
        let object = lexer.object(0)?;
        lexer.skip_space();
        if !lexer.rest().starts_with(b"stream") {
            return Some((object, None));
        }
        lexer.pos += 6;
        // the data begin after the end of the line: CR LF or LF
        if lexer.rest().starts_with(b"\r\n") {
            lexer.pos += 2;
        } else if lexer.rest().starts_with(b"\n") || lexer.rest().starts_with(b"\r") {
            lexer.pos += 1;
        }
        Some((object, Some(at + lexer.pos as u64)))
    }

    /// The unpacked data of the stream whose dictionary is `dict` and whose data begin at `at`.
    fn stream(&mut self, dict: &[(Vec<u8>, Obj)], at: u64, sections: &[Section]) -> Option<Vec<u8>> {
        let length = match get(dict, b"Length")? {
            Obj::Ref(n, _) => self.object(sections, *n, 1)?.uint()?,
            other => other.uint()?,
        };
        let length = usize::try_from(length).ok().filter(|&n| n <= MAX_STREAM)?;
        let packed = self.at(at, length)?;
        let filter = match get(dict, b"Filter") {
            None => None,
            Some(Obj::Name(name)) => Some(name.as_slice()),
            Some(Obj::Array(items)) if items.len() == 1 => match &items[0] {
                Obj::Name(name) => Some(name.as_slice()),
                _ => return None,
            },
            Some(_) => return None,
        };
        let data = match filter {
            None => packed,
            Some(b"FlateDecode") => {
                let mut out = Vec::new();
                ZlibDecoder::new(packed.as_slice())
                    .take(MAX_UNPACKED)
                    .read_to_end(&mut out)
                    .ok()?;
                out
            }
            Some(_) => return None,
        };
        let parms = match get(dict, b"DecodeParms") {
            Some(Obj::Dict(parms)) => Some(parms.as_slice()),
            Some(Obj::Array(items)) => items.first().and_then(|p| match p {
                Obj::Dict(parms) => Some(parms.as_slice()),
                _ => None,
            }),
            _ => None,
        };
        let predictor = parms
            .and_then(|p| get(p, b"Predictor"))
            .and_then(Obj::uint)
            .unwrap_or(1);
        match predictor {
            1 => Some(data),
            10..=15 => {
                let columns = parms.and_then(|p| get(p, b"Columns")).and_then(Obj::uint).unwrap_or(1);
                unpredict(&data, usize::try_from(columns).ok().filter(|&c| (1..=64).contains(&c))?)
            }
            _ => None,
        }
    }

    /// Where object `number` is, by the newest section that has it.
    fn place(&mut self, sections: &[Section], number: u32) -> Option<Place> {
        for section in sections {
            match section {
                Section::Table(subsections) => {
                    let Some(&(first, _, at)) = subsections
                        .iter()
                        .find(|(first, count, _)| number >= *first && number - first < *count)
                    else {
                        continue;
                    };
                    let entry = self.at(at + u64::from(number - first) * 20, 20)?;
                    let mut lexer = Lexer::new(&entry);
                    let offset = lexer.uint()?;
                    lexer.uint()?;
                    lexer.skip_space();
                    return Some(if lexer.rest().starts_with(b"n") {
                        Place::At(offset)
                    } else {
                        Place::Free
                    });
                }
                Section::Stream { widths, index, data } => {
                    let mut before = 0usize;
                    for &(first, count) in index {
                        if number >= first && number - first < count {
                            let size = widths.iter().sum::<usize>();
                            let at = (before + (number - first) as usize) * size;
                            let entry = data.get(at..at + size)?;
                            let field = |from: usize, width: usize| {
                                entry[from..from + width]
                                    .iter()
                                    .fold(0u64, |n, &b| (n << 8) | u64::from(b))
                            };
                            let kind = if widths[0] == 0 { 1 } else { field(0, widths[0]) };
                            let (second, third) =
                                (field(widths[0], widths[1]), field(widths[0] + widths[1], widths[2]));
                            return Some(match kind {
                                1 => Place::At(second),
                                2 => Place::InStream(u32::try_from(second).ok()?, u32::try_from(third).ok()?),
                                _ => Place::Free,
                            });
                        }
                        before += count as usize;
                    }
                }
            }
        }
        None
    }

    /// Object `number`; `depth` counts the objects followed to reach it.
    fn object(&mut self, sections: &[Section], number: u32, depth: usize) -> Option<Obj> {
        if depth > 2 {
            return None;
        }
        match self.place(sections, number)? {
            Place::Free => None,
            Place::At(at) => self.object_at(at).map(|(object, _)| object),
            Place::InStream(stream, index) => {
                let Place::At(at) = self.place(sections, stream)? else {
                    return None;
                };
                let (Obj::Dict(dict), Some(data_at)) = self.object_at(at)? else {
                    return None;
                };
                let data = self.stream(&dict, data_at, sections)?;
                let count = get(&dict, b"N")?.uint()?;
                let first = usize::try_from(get(&dict, b"First")?.uint()?).ok()?;
                let mut lexer = Lexer::new(data.get(..first)?);
                let mut found = None;
                for i in 0..count {
                    let (n, offset) = (lexer.uint()?, lexer.uint()?);
                    if n == u64::from(number) && (found.is_none() || i == u64::from(index)) {
                        found = Some(usize::try_from(offset).ok()?);
                    }
                }
                Lexer::new(data.get(first.checked_add(found?)?..)?).object(0)
            }
        }
    }
}

/// An object of the file, as far as a trailer and an information dictionary need.
#[derive(Debug, Clone, PartialEq)]
enum Obj {
    Int(i64),
    Real,
    Name(Vec<u8>),
    Str(Vec<u8>),
    Ref(u32, u16),
    Array(Vec<Obj>),
    Dict(Vec<(Vec<u8>, Obj)>),
    Keyword(Vec<u8>),
}

impl Obj {
    fn uint(&self) -> Option<u64> {
        match self {
            Obj::Int(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    }
}

fn get<'a>(dict: &'a [(Vec<u8>, Obj)], key: &[u8]) -> Option<&'a Obj> {
    dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn rfind(data: &[u8], needle: &[u8]) -> Option<usize> {
    data.windows(needle.len()).rposition(|w| w == needle)
}

fn is_space(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or_default()
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while let Some(b) = self.peek() {
            if is_space(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.peek().is_some_and(|b| b != b'\n' && b != b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// A number with no sign or point.
    fn uint(&mut self) -> Option<u64> {
        self.skip_space();
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.data[start..self.pos]).ok()?.parse().ok()
    }

    fn object(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_space();
        let rest = self.rest();
        match *rest.first()? {
            b'<' if rest.get(1) == Some(&b'<') => {
                self.pos += 2;
                let mut dict = Vec::new();
                loop {
                    self.skip_space();
                    if self.rest().starts_with(b">>") {
                        self.pos += 2;
                        return Some(Obj::Dict(dict));
                    }
                    let Obj::Name(key) = self.object(depth + 1)? else {
                        return None;
                    };
                    let value = self.object(depth + 1)?;
                    dict.push((key, value));
                }
            }
            b'<' => {
                self.pos += 1;
                let mut digits = Vec::new();
                loop {
                    let b = self.peek()?;
                    self.pos += 1;
                    match b {
                        b'>' => break,
                        b if b.is_ascii_hexdigit() => digits.push(b),
                        b if is_space(b) => {}
                        _ => return None,
                    }
                }
                if digits.len() % 2 == 1 {
                    digits.push(b'0');
                }
                let hex = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                Some(Obj::Str(
                    digits.chunks(2).map(|p| (hex(p[0]) << 4) | hex(p[1])).collect(),
                ))
            }
            b'(' => {
                self.pos += 1;
                self.literal().map(Obj::Str)
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    if self.peek()? == b']' {
                        self.pos += 1;
                        return Some(Obj::Array(items));
                    }
                    items.push(self.object(depth + 1)?);
                }
            }
            b'/' => {
                self.pos += 1;
                let mut name = Vec::new();
                while let Some(b) = self.peek().filter(|&b| !is_space(b) && !is_delimiter(b)) {
                    self.pos += 1;
                    if b == b'#'
                        && let Some(code) = self
                            .data
                            .get(self.pos..self.pos + 2)
                            .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
                    {
                        name.push(code);
                        self.pos += 2;
                    } else {
                        name.push(b);
                    }
                }
                Some(Obj::Name(name))
            }
            b'0'..=b'9' => {
                let start = self.pos;
                let number = self.uint()?;
                if self.peek() == Some(b'.') {
                    self.pos = start;
                    return self.real();
                }
                // `n g R` is a reference
                let after = self.pos;
                if let Some(generation) = self.uint()
                    && {
                        self.skip_space();
                        self.peek() == Some(b'R')
                    }
                    && self
                        .data
                        .get(self.pos + 1)
                        .is_none_or(|&b| is_space(b) || is_delimiter(b))
                {
                    self.pos += 1;
                    return Some(Obj::Ref(u32::try_from(number).ok()?, u16::try_from(generation).ok()?));
                }
                self.pos = after;
                Some(Obj::Int(i64::try_from(number).ok()?))
            }
            b'+' | b'-' | b'.' => self.real(),
            b if b.is_ascii_alphabetic() => {
                let start = self.pos;
                while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
                    self.pos += 1;
                }
                Some(Obj::Keyword(self.data[start..self.pos].to_vec()))
            }
            _ => None,
        }
    }

    /// A number with a sign or a point: only skipped (no value here needs one).
    fn real(&mut self) -> Option<Obj> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
        {
            self.pos += 1;
        }
        (self.pos > start).then_some(Obj::Real)
    }

    /// A literal string after its `(`: balanced parentheses, escapes.
    fn literal(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut open = 0usize;
        loop {
            let b = self.peek()?;
            self.pos += 1;
            match b {
                b'(' => {
                    open += 1;
                    out.push(b);
                }
                b')' if open == 0 => return Some(out),
                b')' => {
                    open -= 1;
                    out.push(b);
                }
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut code = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        code = code * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((code & 0xFF) as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
    }
}

/// The rows of a stream drawn with the PNG predictors (one byte to a pixel), `columns` bytes a row.
fn unpredict(data: &[u8], columns: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut previous = vec![0u8; columns];
    for row in data.chunks_exact(columns + 1) {
        let (filter, row) = (row[0], &row[1..]);
        let mut current = row.to_vec();
        for i in 0..columns {
            let left = if i > 0 { current[i - 1] } else { 0 };
            let (up, corner) = (previous[i], if i > 0 { previous[i - 1] } else { 0 });
            let guess = match filter {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, corner),
                _ => return None,
            };
            current[i] = current[i].wrapping_add(guess);
        }
        out.extend_from_slice(&current);
        previous = current;
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("peekel-pdf-meta-{name}-{}.pdf", std::process::id()));
        std::fs::write(&path, bytes).expect("writes");
        path
    }

    fn read(name: &str, bytes: &[u8]) -> Option<(Option<String>, Option<String>)> {
        let path = write(name, bytes);
        let found = title_and_author(&path);
        let _ = std::fs::remove_file(&path);
        found
    }

    /// A PDF with a classic table: the objects as given (numbered from 1), the trailer's extra entries.
    fn classic(objects: &[&str], trailer: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} {trailer} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn deflate(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("writes");
        encoder.finish().expect("finishes")
    }

    #[test]
    fn a_classic_table_gives_the_title_and_the_author() {
        let pdf = classic(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>",
                "<< /Type /Pages /Kids [] /Count 0 >>",
                r"<< /Title (A Book \(with notes\)) /Author <FEFF0410043B044F> /Producer (x) >>",
            ],
            "/Root 1 0 R /Info 3 0 R",
        );
        assert_eq!(
            read("classic", &pdf),
            Some((Some("A Book (with notes)".to_string()), Some("Аля".to_string())))
        );
    }

    #[test]
    fn a_title_given_by_reference_and_no_author() {
        let pdf = classic(
            &["<< /Type /Catalog >>", "<< /Title 3 0 R >>", "(Far Away)"],
            "/Root 1 0 R /Info 2 0 R",
        );
        assert_eq!(read("reference", &pdf), Some((Some("Far Away".to_string()), None)));
    }

    #[test]
    fn an_encrypted_file_or_one_with_no_information_is_left_to_the_whole_reading() {
        let pdf = classic(
            &["<< /Type /Catalog >>", "<< /Title (x) >>", "<< /Filter /Standard >>"],
            "/Root 1 0 R /Info 2 0 R /Encrypt 3 0 R",
        );
        assert_eq!(read("encrypted", &pdf), None);
        let pdf = classic(&["<< /Type /Catalog >>"], "/Root 1 0 R");
        assert_eq!(read("no-info", &pdf), None);
    }

    #[test]
    fn a_later_update_is_read_before_the_first() {
        // the file as first written, then an update that gives a new information dictionary and points back with /Prev
        let mut pdf = classic(
            &["<< /Type /Catalog >>", "<< /Title (Old) >>"],
            "/Root 1 0 R /Info 2 0 R",
        );
        let first_xref = rfind(&pdf, b"xref").expect("an xref") as u64;
        let new_at = pdf.len();
        pdf.extend(b"3 0 obj\n<< /Title (New) /Author (Me) >>\nendobj\n");
        let xref = pdf.len();
        pdf.extend(format!("xref\n3 1\n{new_at:010} 00000 n \n").as_bytes());
        pdf.extend(
            format!("trailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R /Prev {first_xref} >>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        );
        assert_eq!(
            read("update", &pdf),
            Some((Some("New".to_string()), Some("Me".to_string())))
        );
    }

    #[test]
    fn a_cross_reference_stream_and_an_object_stream_with_a_predictor() {
        // object 1 the catalog, object 2 an object stream holding object 3 (the information), object 4 the cross-reference stream
        let mut pdf = b"%PDF-1.5\n".to_vec();
        let catalog = pdf.len();
        pdf.extend(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let inner = b"3 0 << /Title (Streamed) /Author (Someone) >>";
        let header_len = b"3 0 ".len();
        let packed = deflate(inner);
        let objstm = pdf.len();
        pdf.extend(
            format!(
                "2 0 obj\n<< /Type /ObjStm /N 1 /First {header_len} /Filter /FlateDecode /Length {} >>\nstream\n",
                packed.len()
            )
            .as_bytes(),
        );
        pdf.extend(&packed);
        pdf.extend(b"\nendstream\nendobj\n");
        let xref = pdf.len();
        // entries of 1 + 4 + 2 bytes, rows drawn with the "up" predictor
        let entries: Vec<[u8; 7]> = vec![
            [0, 0, 0, 0, 0, 0xFF, 0xFF],
            [1, 0, 0, 0, catalog as u8, 0, 0],
            [1, 0, 0, (objstm >> 8) as u8, objstm as u8, 0, 0],
            [2, 0, 0, 0, 2, 0, 0],
            [1, 0, 0, (xref >> 8) as u8, xref as u8, 0, 0],
        ];
        let mut rows = Vec::new();
        let mut previous = [0u8; 7];
        for entry in &entries {
            rows.push(2);
            rows.extend(entry.iter().zip(previous).map(|(b, p)| b.wrapping_sub(p)));
            previous = *entry;
        }
        let packed = deflate(&rows);
        pdf.extend(
            format!(
                "4 0 obj\n<< /Type /XRef /Size 5 /W [1 4 2] /Root 1 0 R /Info 3 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 7 >> /Length {} >>\nstream\r\n",
                packed.len()
            )
            .as_bytes(),
        );
        pdf.extend(&packed);
        pdf.extend(format!("\nendstream\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
        assert_eq!(
            read("stream", &pdf),
            Some((Some("Streamed".to_string()), Some("Someone".to_string())))
        );
    }

    #[test]
    fn spoilt_files_give_none_and_never_hang() {
        let good = classic(&["<< /Type /Catalog >>", "<< /Title (T) >>"], "/Root 1 0 R /Info 2 0 R");
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("no-startxref", b"%PDF-1.4\n1 0 obj << >> endobj\n".to_vec()),
            ("far-startxref", b"%PDF-1.4\nstartxref\n99999999\n%%EOF".to_vec()),
            ("cut", good[..good.len() / 2].to_vec()),
            (
                "loop",
                b"%PDF-1.4\nxref\n0 0\ntrailer\n<< /Prev 9 >>\nstartxref\n9\n%%EOF\n".to_vec(),
            ),
            (
                "deep",
                format!(
                    "%PDF-1.4\nxref\n0 0\ntrailer\n{}\nstartxref\n9\n%%EOF\n",
                    "[".repeat(10_000)
                )
                .into_bytes(),
            ),
        ] {
            assert_eq!(read(name, &bytes), None, "{name}");
        }
        // bytes cut short anywhere never panic
        for end in (0..good.len()).step_by(7) {
            let _ = read("prefix", &good[..end]);
        }
    }
}
