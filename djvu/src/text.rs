//! The text layer of a page (spec 8.3.4): the characters of the page in reading order, kept in a `TXTz` chunk (BZZ coded) or a `TXTa`
//! chunk (not coded). What follows the characters, the zones that say where each word stands on the page, is not read here yet.

use std::borrow::Cow;

use crate::{Error, bzz};

/// The most text one page may have once it is decoded (a page of print has a few thousand characters).
pub const MAX_TEXT: usize = 16 * 1024 * 1024;

/// The text of a page from a `TXTz` or a `TXTa` chunk: a length of 24 bits, then that many bytes of UTF-8. Bytes that are not
/// valid UTF-8 become U+FFFD; the bytes after the text (the zones) are ignored.
///
/// # Errors
/// [`Error::BadBzz`] for a `TXTz` that cannot be decoded or has too much in it; [`Error::BadText`] for data too short for its length.
pub fn parse(id: [u8; 4], data: &[u8]) -> Result<String, Error> {
    let raw = decoded(id, data)?;
    let (text, _) = split(&raw)?;
    // the writers end the text of a page with a NUL (DjVuLibre does; `djvutxt` prints a line break in its place)
    Ok(String::from_utf8_lossy(text).trim_end_matches('\0').to_owned())
}

/// The data of a chunk as it is once the BZZ coding of a `TXTz` is taken off.
fn decoded(id: [u8; 4], data: &[u8]) -> Result<Cow<'_, [u8]>, Error> {
    Ok(if &id == b"TXTz" {
        Cow::Owned(bzz::decode(data, MAX_TEXT)?)
    } else {
        Cow::Borrowed(data)
    })
}

/// The bytes of the text and what follows them.
fn split(raw: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let [a, b, c, rest @ ..] = raw else {
        return Err(Error::BadText("shorter than the length of the text"));
    };
    let length = usize::from(*a) << 16 | usize::from(*b) << 8 | usize::from(*c);
    if rest.len() < length {
        return Err(Error::BadText("the text is longer than the data"));
    }
    Ok(rest.split_at(length))
}

/// A rectangle of a page in its pixels, the origin at the top left and y growing down: `right` and `bottom` are one past the last pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A word of the text layer (or, for a page whose zones go no deeper, a line) and where it stands on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub rect: Rect,
}

/// The zone types that matter here (the spec's numbers: page 1, column 2, region 3, paragraph 4, line 5, word 6, character 7).
const PARAGRAPH: u8 = 4;
const LINE: u8 = 5;
const WORD: u8 = 6;
const CHARACTER: u8 = 7;
/// The most zones one page may have (a record is at least 17 bytes, and a page of print has some thousands of words).
const MAX_ZONES: usize = 1 << 20;

/// A zone with its place made absolute: the rectangle in the origin the file uses (the bottom left, y growing up).
#[derive(Clone, Copy)]
struct Zone {
    kind: u8,
    xmin: i32,
    ymin: i32,
    xmax: i32,
    ymax: i32,
    start: i64,
    length: i64,
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, Error> {
        let byte = *self.data.get(self.at).ok_or(Error::BadText("the zones end too soon"))?;
        self.at += 1;
        Ok(byte)
    }

    fn number16(&mut self) -> Result<i32, Error> {
        let (a, b) = (i32::from(self.byte()?), i32::from(self.byte()?));
        Ok((a << 8 | b) - 0x8000)
    }

    fn number24(&mut self) -> Result<usize, Error> {
        let (a, b, c) = (
            usize::from(self.byte()?),
            usize::from(self.byte()?),
            usize::from(self.byte()?),
        );
        Ok(a << 16 | b << 8 | c)
    }
}

/// The words of a page with where each stands, from a `TXTz` or `TXTa` chunk (spec 8.3.5 gives the text and says the zones follow it;
/// the zones' own coding is not in the spec's text: see `docs/internal/djvu.md` step 11). `height` is the page's, to turn the
/// bottom-up y of the file to the top-down of a picture. `Ok(None)` when the chunk has no zones. A zone that does not fit the text
/// or has no area is left out; a structure that cannot be followed is an error.
///
/// # Errors
/// The errors of [`parse`]; [`Error::BadText`] for zones that end too soon, a version other than 1, a type out of order, or too many.
pub fn words(id: [u8; 4], data: &[u8], height: i32) -> Result<Option<Vec<Word>>, Error> {
    let raw = decoded(id, data)?;
    let (text, rest) = split(&raw)?;
    let Some((&version, zones)) = rest.split_first() else {
        return Ok(None);
    };
    if version != 1 {
        return Err(Error::BadText("an unknown version of the zones"));
    }
    let mut reader = Reader { data: zones, at: 0 };
    let mut found: Vec<Zone> = Vec::new();
    let mut count = 0;
    zone(&mut reader, None, None, &mut found, &mut count)?;
    let make = |kind: u8| -> Vec<Word> {
        found
            .iter()
            .filter(|z| z.kind == kind && z.xmax > z.xmin && z.ymax > z.ymin)
            .filter_map(|z| {
                let from = usize::try_from(z.start).ok()?;
                let to = from.checked_add(usize::try_from(z.length).ok()?)?;
                Some(Word {
                    text: String::from_utf8_lossy(text.get(from..to)?).into_owned(),
                    rect: Rect {
                        left: z.xmin,
                        top: height - z.ymax,
                        right: z.xmax,
                        bottom: height - z.ymin,
                    },
                })
            })
            .collect()
    };
    let words = make(WORD);
    Ok(Some(if words.is_empty() { make(LINE) } else { words }))
}

/// Reads one zone and its children, their places made absolute from the parent and the previous sibling; the words and lines go to
/// `found`, in reading order.
fn zone(
    reader: &mut Reader<'_>,
    parent: Option<&Zone>,
    previous: Option<&Zone>,
    found: &mut Vec<Zone>,
    count: &mut usize,
) -> Result<Zone, Error> {
    *count += 1;
    if *count > MAX_ZONES {
        return Err(Error::BadText("too many zones"));
    }
    let kind = reader.byte()?;
    if !(1..=CHARACTER).contains(&kind) || parent.is_some_and(|p| kind <= p.kind) {
        return Err(Error::BadText("a zone of a type that cannot be there"));
    }
    let (x, y) = (reader.number16()?, reader.number16()?);
    let (width, height) = (reader.number16()?, reader.number16()?);
    let start = reader.number16()?;
    let length = reader.number24()?;
    let children = reader.number24()?;
    // each child takes at least 17 bytes: a count the rest of the data cannot hold is a broken file, not a loop to run
    if children > reader.data.len().saturating_sub(reader.at) / 17 {
        return Err(Error::BadText("more zones than the data can hold"));
    }
    let length = i64::try_from(length).unwrap_or(i64::MAX);
    let (x, y, start) = match (previous, parent) {
        (Some(p), _) => {
            let (x, y) = if matches!(kind, 1 | PARAGRAPH | LINE) {
                (x + p.xmin, p.ymin - (y + height))
            } else {
                (x + p.xmax, y + p.ymin)
            };
            (x, y, i64::from(start) + p.start + p.length)
        }
        (None, Some(p)) => (x + p.xmin, p.ymax - (y + height), i64::from(start) + p.start),
        (None, None) => (x, y, i64::from(start)),
    };
    let this = Zone {
        kind,
        xmin: x,
        ymin: y,
        xmax: x + width,
        ymax: y + height,
        start,
        length,
    };
    let mut before: Option<Zone> = None;
    for _ in 0..children {
        before = Some(zone(reader, Some(&this), before.as_ref(), found, count)?);
    }
    if kind == WORD || kind == LINE {
        found.push(this);
    }
    Ok(this)
}

/// `text` as a search sees it: lower case, one space for each run of white space or of the separators that DjVu puts between
/// columns and paragraphs (US, GS, VT, FF).
pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The rectangles of the words of a page that the text `needle` (made by [`normalise`]) is in, in the order of the words; a hit that
/// runs over several words takes each of them.
pub fn locate(words: &[Word], needle: &str) -> Vec<Rect> {
    if needle.is_empty() {
        return Vec::new();
    }
    // the words joined by one space, and where each stands in that
    let mut joined = String::new();
    let mut spans: Vec<(usize, usize, usize)> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        let text = normalise(&word.text);
        if text.is_empty() {
            continue;
        }
        if !joined.is_empty() {
            joined.push(' ');
        }
        spans.push((joined.len(), joined.len() + text.len(), index));
        joined.push_str(&text);
    }
    let mut hit: Vec<usize> = Vec::new();
    for (at, found) in joined.match_indices(needle) {
        let end = at + found.len();
        hit.extend(
            spans
                .iter()
                .filter(|(from, to, _)| *from < end && at < *to)
                .map(|(_, _, index)| *index),
        );
    }
    hit.dedup();
    hit.into_iter()
        .filter_map(|index| words.get(index))
        .map(|word| word.rect)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)] // a test that cannot make its data should stop at once
mod tests {
    use super::*;

    fn text_chunk(text: &[u8], zones: &[u8]) -> Vec<u8> {
        let mut data = (text.len() as u32).to_be_bytes()[1..].to_vec();
        data.extend(text);
        data.extend(zones);
        data
    }

    #[test]
    fn a_plain_text_chunk_gives_its_characters_and_ignores_the_zones_after_them() {
        let data = text_chunk("Привет, мир\nline two".as_bytes(), &[1, 0, 0, 9, 9, 9]);
        assert_eq!(parse(*b"TXTa", &data).as_deref(), Ok("Привет, мир\nline two"));
    }

    #[test]
    fn the_nul_that_ends_the_text_of_a_page_is_not_part_of_it() {
        let data = text_chunk(b"one two\x0b\0\0", &[]);
        assert_eq!(parse(*b"TXTa", &data).as_deref(), Ok("one two\x0b"));
    }

    #[test]
    fn a_chunk_cut_short_or_with_a_wrong_length_is_an_error() {
        assert_eq!(
            parse(*b"TXTa", &[0, 1]),
            Err(Error::BadText("shorter than the length of the text"))
        );
        assert_eq!(
            parse(*b"TXTa", &[0, 0, 5, b'a', b'b']),
            Err(Error::BadText("the text is longer than the data"))
        );
        assert_eq!(parse(*b"TXTa", &[0, 0, 0]).as_deref(), Ok(""), "an empty page");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_replaced_and_not_an_error() {
        let data = text_chunk(&[b'a', 0xFF, b'b'], &[]);
        assert_eq!(parse(*b"TXTa", &data).as_deref(), Ok("a\u{FFFD}b"));
    }

    #[test]
    fn data_of_any_kind_is_an_error_or_text_and_never_a_panic() {
        let mut seed = 0x1234_5678_u32;
        for round in 0..500 {
            let len = (round * 7) % 200;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            let _ = parse(*b"TXTa", &data);
            let _ = parse(*b"TXTz", &data);
            let _ = words(*b"TXTa", &data, 500);
            let _ = words(*b"TXTz", &data, 500);
        }
    }

    /// One zone record: type, x, y, width, height and text start (biased 16 bits), text length and children count (24 bits).
    fn record(kind: u8, x: i32, y: i32, w: i32, h: i32, start: i32, length: usize, children: usize) -> Vec<u8> {
        let mut out = vec![kind];
        for n in [x, y, w, h, start] {
            out.extend(u16::try_from(n + 0x8000).unwrap().to_be_bytes());
        }
        for n in [length, children] {
            out.extend(&n.to_be_bytes()[5..]);
        }
        out
    }

    /// The text "ab cd", a page 1000 x 500 with one line and in it two words, in the file's way: the first child of a parent is
    /// counted from the parent's top edge (a line, a paragraph) or its left and bottom edge (a word), a later child from the previous
    /// sibling's right edge (a word) or its bottom (a line). In the bottom-up pixels of the file the words stand at x 100..160, y
    /// 410..440 and x 180..250, y 415..445.
    fn two_words() -> Vec<u8> {
        let mut zones = vec![1];
        zones.extend(record(1, 0, 0, 1000, 500, 0, 5, 1));
        zones.extend(record(5, 100, 50, 150, 50, 0, 5, 2));
        zones.extend(record(6, 0, 10, 60, 30, 0, 2, 0));
        zones.extend(record(6, 20, 5, 70, 30, 1, 2, 0));
        text_chunk(b"ab cd", &zones)
    }

    #[test]
    fn the_words_stand_where_the_zones_say_with_y_turned_to_grow_down() {
        let found = words(*b"TXTa", &two_words(), 500).unwrap().unwrap();
        let texts: Vec<&str> = found.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["ab", "cd"]);
        assert_eq!(
            found[0].rect,
            Rect {
                left: 100,
                top: 60,
                right: 160,
                bottom: 90
            }
        );
        assert_eq!(
            found[1].rect,
            Rect {
                left: 180,
                top: 55,
                right: 250,
                bottom: 85
            }
        );
    }

    #[test]
    fn a_hit_that_runs_over_two_words_takes_both_and_the_case_does_not_matter() {
        let found = words(*b"TXTa", &two_words(), 500).unwrap().unwrap();
        assert_eq!(locate(&found, &normalise("B C")).len(), 2);
        assert_eq!(locate(&found, "cd"), vec![found[1].rect]);
        assert_eq!(locate(&found, "x"), Vec::<Rect>::new());
        assert_eq!(locate(&found, ""), Vec::<Rect>::new());
    }

    #[test]
    fn a_page_whose_zones_stop_at_lines_gives_its_lines() {
        let mut zones = vec![1];
        zones.extend(record(1, 0, 0, 1000, 500, 0, 5, 1));
        zones.extend(record(5, 100, 50, 150, 50, 0, 5, 0));
        let found = words(*b"TXTa", &text_chunk(b"ab cd", &zones), 500).unwrap().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].text.as_str(), found[0].rect.left, found[0].rect.top),
            ("ab cd", 100, 50)
        );
    }

    #[test]
    fn text_without_zones_has_no_words_and_broken_zones_are_an_error() {
        assert_eq!(words(*b"TXTa", &text_chunk(b"ab cd", &[]), 500), Ok(None));
        assert!(
            words(*b"TXTa", &text_chunk(b"ab", &[2]), 500).is_err(),
            "a version other than 1"
        );
        let mut zones = vec![1];
        zones.extend(record(1, 0, 0, 1000, 500, 0, 2, 9));
        assert!(
            words(*b"TXTa", &text_chunk(b"ab", &zones), 500).is_err(),
            "more children than the data holds"
        );
        let mut zones = vec![1];
        zones.extend(record(6, 0, 0, 10, 10, 0, 2, 1));
        zones.extend(record(5, 0, 0, 10, 10, 0, 2, 0));
        assert!(
            words(*b"TXTa", &text_chunk(b"ab", &zones), 500).is_err(),
            "a line inside a word"
        );
    }

    #[test]
    fn a_zone_that_does_not_fit_the_text_is_left_out() {
        let mut zones = vec![1];
        zones.extend(record(1, 0, 0, 1000, 500, 0, 5, 1));
        zones.extend(record(6, 0, 0, 10, 10, 3, 9, 0));
        let found = words(*b"TXTa", &text_chunk(b"ab cd", &zones), 500).unwrap().unwrap();
        assert!(found.is_empty());
    }
}
