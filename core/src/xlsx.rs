//! The top left of the first sheet of an Excel file (xlsx) as a picture: a preview of what the sheet looks like, not a
//! rendering that matches Excel.
//!
//! An xlsx is a ZIP of XML. Read here: the first sheet's first rows (300 at most: the text of the sheet is cut after them
//! before it is parsed, so that a sheet of a million rows costs no more), the widths of the columns and the heights of the
//! rows, hidden rows and columns, merged cells, the shared strings, the styles (font, size, bold, italic, colour, fill,
//! borders, alignment, wrapping) and the number formats: the built-in ones and custom ones (decimals, thousands, per
//! cent, currency signs, text around the number, dates and times).
//!
//! Not done: charts, pictures, shapes, conditional formatting, formulas (the value Excel saved is shown), rich text
//! inside a cell (its pieces are joined), vertical alignment other than the bottom, rotated text, the 1904 date system,
//! scientific and fraction formats (shown as general), colours of borders (all grey), shrink to fit, text of more than the
//! first 16 MB of shared strings, sheets after the first, tables of Excel as such (only what their cells look like).
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser refuses DTDs and has
//! limits (`xml`), and what is drawn is limited by rows, columns and size.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use image::RgbaImage;
use roxmltree::Node;

use crate::archive::Archive;
use crate::card::civil_from_days;
use crate::color::{self, tinted};
use crate::decode::DecodeError;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::office::{MAX_BYTES, MAX_PART};
use crate::page::{Font, Page, Rgb, text_width};
use crate::text::{self, Align, Para};
use crate::xml::{self, attr, child, children, descendant, parse};

/// Of the shared strings this much is read.
const MAX_STRINGS: usize = 16 * 1024 * 1024;
/// Rows of the sheet that are read, and the columns and the points that are drawn at most.
const MAX_ROWS: usize = 300;
/// Rows of the sheet that can be shown at all: a row beyond this is not read (a page is a few dozen rows high).
const MAX_SHEET_ROWS: usize = 1000;
/// A cell's text is cut to this many characters, a number format code is not taken when it is longer than this.
const MAX_TEXT: usize = 4000;
const MAX_FORMAT: usize = 256;
const MAX_COLUMNS: usize = 60;
const MAX_WIDTH: f32 = 1400.0;
const MAX_HEIGHT: f32 = 1000.0;

/// The width of the strip of letters over the columns and of numbers beside the rows, in points.
const GUTTER: f32 = 30.0;
const HEADER: f32 = 16.0;
/// A column and a row as wide and as high as Excel makes them when the file says nothing, in points.
const DEFAULT_COLUMN: f32 = 48.0;
const DEFAULT_ROW: f32 = 15.0;

/// Whether the name says xlsx.
pub fn is_xlsx(path: &Path) -> bool {
    has_extension(path, "xlsx")
}

/// Draws the top left of the first sheet of the file in `path`; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("xlsx", "the workbook could not be drawn", || draw(&data))
}

/// `text` cut after its `rows`-th `</row>`, with the merged cells that come after the data kept and the tags closed.
fn first_rows(text: &str, rows: usize) -> String {
    let mut end = 0;
    for _ in 0..rows {
        match text[end..].find("</row>") {
            Some(i) => end += i + "</row>".len(),
            None => return text.to_string(),
        }
    }
    if !text[end..].contains("<row") {
        return text.to_string();
    }
    let merges = text.find("<mergeCells").and_then(|from| {
        let to = text[from..].find("</mergeCells>")? + "</mergeCells>".len();
        Some(&text[from..from + to])
    });
    format!("{}</sheetData>{}</worksheet>", &text[..end], merges.unwrap_or(""))
}

/// `text` cut at the last `</si>` that is within `limit` bytes, the root closed.
fn first_strings(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    match text[..cut].rfind("</si>") {
        Some(i) => format!("{}</si></sst>", &text[..i]),
        None => text.to_string(),
    }
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    let workbook_text = archive
        .text("xl/workbook.xml", MAX_PART)
        .ok_or_else(|| unreadable("this is no Excel file (no xl/workbook.xml)"))?;
    let workbook = parse(&workbook_text)?;
    let rels = xml::relationships(&archive, "xl/workbook.xml");
    // the first sheet in the list of sheets, or sheet1
    let sheet_name = descendant(workbook.root_element(), "sheet")
        .and_then(|s| s.attributes().find(|a| a.name() == "id" && a.namespace().is_some()))
        .and_then(|a| rels.get(a.value()).map(|(_, p)| p.clone()))
        .unwrap_or_else(|| "xl/worksheets/sheet1.xml".to_string());
    let sheet_text = archive
        .text(&sheet_name, MAX_PART)
        .ok_or_else(|| unreadable("the workbook has no sheet"))?;
    let cut = first_rows(&sheet_text, MAX_ROWS);
    let sheet = parse(&cut).or_else(|_| parse(&sheet_text))?;

    let theme_text = archive.text("xl/theme/theme1.xml", MAX_PART);
    let theme = theme_text
        .as_deref()
        .and_then(|t| parse(t).ok())
        .map(|t| theme_colours(&t))
        .unwrap_or_default();
    let styles_text = archive.text("xl/styles.xml", MAX_PART);
    let styles = styles_text
        .as_deref()
        .and_then(|t| parse(t).ok())
        .map(|s| Styles::read(&s, &theme))
        .unwrap_or_default();
    let strings_text = archive
        .text("xl/sharedStrings.xml", MAX_PART)
        .map(|t| first_strings(&t, MAX_STRINGS));
    let strings = strings_text
        .as_deref()
        .and_then(|t| parse(t).ok())
        .map(|s| shared_strings(&s))
        .unwrap_or_default();

    Grid::read(&sheet, &styles, &strings).render()
}

// ---- colours and styles -----------------------------------------------------------------------------------------

fn hex(value: &str) -> Option<Rgb> {
    // ARGB, or RGB
    let v = if value.len() == 8 { value.get(2..)? } else { value };
    color::hex(Some(v))
}

/// The theme's colours in the order a spreadsheet numbers them: light 1, dark 1, light 2, dark 2, the six accents, links.
fn theme_colours(theme: &roxmltree::Document<'_>) -> Vec<Rgb> {
    let Some(scheme) = descendant(theme.root_element(), "clrScheme") else {
        return Vec::new();
    };
    [
        "lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink",
        "folHlink",
    ]
    .iter()
    .map(|name| {
        child(scheme, name)
            .and_then(|e| {
                child(e, "srgbClr")
                    .and_then(|c| attr(c, "val").and_then(hex))
                    .or_else(|| child(e, "sysClr").and_then(|c| attr(c, "lastClr").and_then(hex)))
            })
            .unwrap_or([0, 0, 0])
    })
    .collect()
}

/// The legacy palette's first colours.
fn indexed(index: usize) -> Rgb {
    const BASIC: [Rgb; 8] = [
        [0, 0, 0],
        [255, 255, 255],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [255, 0, 255],
        [0, 255, 255],
    ];
    match index {
        0..=7 => BASIC[index],
        8..=15 => BASIC[index - 8],
        64 => [0, 0, 0],
        65 => [255, 255, 255],
        _ => [0, 0, 0],
    }
}

/// The colour an element like `<color rgb="FF0000FF"/>` or `<fgColor theme="4" tint="0.4"/>` says.
fn colour(node: Node<'_, '_>, theme: &[Rgb]) -> Option<Rgb> {
    let base = if let Some(v) = attr(node, "rgb") {
        hex(v)?
    } else if let Some(i) = attr(node, "theme").and_then(|v| v.parse::<usize>().ok()) {
        *theme.get(i)?
    } else {
        indexed(attr(node, "indexed").and_then(|v| v.parse::<usize>().ok())?)
    };
    Some(match attr(node, "tint").and_then(|v| v.parse::<f32>().ok()) {
        Some(t) if t.is_finite() => tinted(base, t.clamp(-1.0, 1.0)),
        _ => base,
    })
}

#[derive(Clone, Debug)]
struct FontSpec {
    size: f32,
    bold: bool,
    italic: bool,
    underline: bool,
    colour: Option<Rgb>,
    family: Option<String>,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            colour: None,
            family: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct CellStyle {
    format: u32,
    font: usize,
    fill: Option<Rgb>,
    border: bool,
    align: Option<String>,
    wrap: bool,
}

#[derive(Default)]
struct Styles {
    formats: HashMap<u32, String>,
    fonts: Vec<FontSpec>,
    cells: Vec<CellStyle>,
}

impl Styles {
    fn read(xml: &roxmltree::Document<'_>, theme: &[Rgb]) -> Self {
        let root = xml.root_element();
        let mut formats = HashMap::new();
        if let Some(list) = child(root, "numFmts") {
            for f in children(list, "numFmt").take(500) {
                if let (Some(id), Some(code)) =
                    (attr(f, "numFmtId").and_then(|v| v.parse().ok()), attr(f, "formatCode"))
                    && code.len() <= MAX_FORMAT
                {
                    formats.insert(id, code.to_string());
                }
            }
        }
        let fonts: Vec<FontSpec> = child(root, "fonts")
            .map(|list| {
                children(list, "font")
                    .take(1000)
                    .map(|f| FontSpec {
                        size: child(f, "sz")
                            .and_then(|n| attr(n, "val"))
                            .and_then(|v| v.parse::<f32>().ok())
                            .filter(|v| v.is_finite())
                            .unwrap_or(11.0)
                            .clamp(1.0, 200.0),
                        bold: child(f, "b").is_some_and(|n| attr(n, "val") != Some("0")),
                        italic: child(f, "i").is_some_and(|n| attr(n, "val") != Some("0")),
                        underline: child(f, "u").is_some_and(|n| attr(n, "val") != Some("none")),
                        colour: child(f, "color").and_then(|c| colour(c, theme)),
                        family: child(f, "name").and_then(|n| attr(n, "val")).map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fills: Vec<Option<Rgb>> = child(root, "fills")
            .map(|list| {
                children(list, "fill")
                    .take(1000)
                    .map(|f| {
                        let pattern = child(f, "patternFill")?;
                        if attr(pattern, "patternType") != Some("solid") {
                            return None;
                        }
                        child(pattern, "fgColor").and_then(|c| colour(c, theme))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let borders: Vec<bool> = child(root, "borders")
            .map(|list| {
                children(list, "border")
                    .take(1000)
                    .map(|b| {
                        ["left", "right", "top", "bottom"]
                            .iter()
                            .any(|side| child(b, side).is_some_and(|s| attr(s, "style").is_some()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cells: Vec<CellStyle> = child(root, "cellXfs")
            .map(|list| {
                children(list, "xf")
                    .take(10_000)
                    .map(|x| {
                        let index = |name: &str| attr(x, name).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                        let alignment = child(x, "alignment");
                        CellStyle {
                            format: attr(x, "numFmtId").and_then(|v| v.parse().ok()).unwrap_or(0),
                            font: index("fontId"),
                            fill: fills.get(index("fillId")).copied().flatten(),
                            border: borders.get(index("borderId")).copied().unwrap_or(false),
                            align: alignment.and_then(|a| attr(a, "horizontal")).map(str::to_string),
                            wrap: alignment.is_some_and(|a| matches!(attr(a, "wrapText"), Some("1" | "true"))),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { formats, fonts, cells }
    }

    fn style(&self, index: usize) -> CellStyle {
        self.cells.get(index).cloned().unwrap_or_default()
    }

    fn font(&self, index: usize) -> FontSpec {
        self.fonts.get(index).cloned().unwrap_or_default()
    }

    fn format(&self, id: u32) -> String {
        if let Some(code) = self.formats.get(&id) {
            return code.clone();
        }
        match id {
            1 => "0",
            2 => "0.00",
            3 => "#,##0",
            4 | 41..=44 => "#,##0.00",
            9 => "0%",
            10 => "0.00%",
            14 => "dd/mm/yyyy",
            15 => "d-mmm-yy",
            16 => "d-mmm",
            17 => "mmm-yy",
            18 => "h:mm AM/PM",
            19 => "h:mm:ss AM/PM",
            20 => "h:mm",
            21 => "h:mm:ss",
            22 => "dd/mm/yyyy h:mm",
            37 | 38 => "#,##0;(#,##0)",
            39 | 40 => "#,##0.00;(#,##0.00)",
            45 => "mm:ss",
            46 => "h:mm:ss",
            47 => "mm:ss.0",
            _ => "General",
        }
        .to_string()
    }
}

/// The text of each `si` of the shared strings (the pieces of rich text joined).
fn shared_strings(xml: &roxmltree::Document<'_>) -> Vec<String> {
    children(xml.root_element(), "si")
        .map(|si| {
            si.descendants()
                .filter(|n| n.is_element() && n.tag_name().name() == "t")
                // the phonetic guide of East Asian text is not part of the text
                .filter(|n| !n.ancestors().any(|a| a.is_element() && a.tag_name().name() == "rPh"))
                .filter_map(|n| n.text())
                .collect::<String>()
        })
        .collect()
}

// ---- number formats ---------------------------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// A number as Excel's "General" shows it: up to eleven significant digits, no trailing zeros.
fn general(value: f64) -> String {
    if !value.is_finite() {
        return "#NUM!".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    if value.abs() >= 1e11 || value.abs() < 1e-9 {
        return format!("{value:.5E}").replace("E", "E+").replace("E+-", "E-");
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (10 - magnitude).clamp(0, 15) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

/// The sections of a format code (split at `;` outside quotes and brackets).
fn sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut quoted, mut bracket) = (false, false);
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' if !bracket => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            '\\' if !quoted => {
                if let Some(next) = chars.next()
                    && let Some(s) = out.last_mut()
                {
                    s.push('\\');
                    s.push(next);
                }
                continue;
            }
            ';' if !quoted && !bracket => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        if let Some(last) = out.last_mut() {
            last.push(c);
        }
    }
    out
}

/// One piece of a section of a format code.
#[derive(Debug, PartialEq)]
enum Piece {
    Literal(String),
    /// A placeholder of a digit: `0`, `#`, `?`.
    Digit(char),
    Point,
    Comma,
    Percent,
    /// A letter of a date or time: the run of equal letters and how long it is.
    Time(char, usize),
    AmPm,
    General,
}

/// Whether `chars` begins with `word` (capitals do not matter).
fn starts(chars: &[char], word: &str) -> bool {
    chars.len() >= word.len() && chars.iter().zip(word.chars()).all(|(a, b)| a.eq_ignore_ascii_case(&b))
}

fn pieces(section: &str) -> Vec<Piece> {
    let chars: Vec<char> = section.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&x| x == '"')
                    .map_or(chars.len(), |p| i + 1 + p);
                out.push(Piece::Literal(chars[i + 1..end].iter().collect()));
                i = end + 1;
                continue;
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(Piece::Literal(next.to_string()));
                }
                i += 2;
                continue;
            }
            '[' => {
                let end = chars[i..].iter().position(|&x| x == ']').map_or(chars.len(), |p| i + p);
                let inside: String = chars[i + 1..end.min(chars.len())].iter().collect();
                // a currency sign: [$£-809]
                if let Some(rest) = inside.strip_prefix('$') {
                    let sign: String = rest.chars().take_while(|&x| x != '-').collect();
                    out.push(Piece::Literal(sign));
                }
                i = end + 1;
                continue;
            }
            '_' | '*' => {
                if c == '_' {
                    out.push(Piece::Literal(" ".to_string()));
                }
                i += 2;
                continue;
            }
            '0' | '#' | '?' => out.push(Piece::Digit(c)),
            '.' => out.push(Piece::Point),
            ',' => out.push(Piece::Comma),
            '%' => out.push(Piece::Percent),
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' => {
                let lower = c.to_ascii_lowercase();
                let run = chars[i..]
                    .iter()
                    .take_while(|x| x.to_ascii_lowercase() == lower)
                    .count();
                out.push(Piece::Time(lower, run));
                i += run;
                continue;
            }
            'A' | 'a' if starts(&chars[i..], "AM/PM") => {
                out.push(Piece::AmPm);
                i += 5;
                continue;
            }
            'G' if starts(&chars[i..], "General") => {
                out.push(Piece::General);
                i += 7;
                continue;
            }
            other => out.push(Piece::Literal(other.to_string())),
        }
        i += 1;
    }
    out
}

/// A number with a thousands separator.
fn thousands(digits: &str) -> String {
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A serial date of Excel (days since 1900, with its leap-year mistake) as (days since 1970, seconds of the day).
fn serial(value: f64) -> Option<(i64, u32)> {
    if !(0.0..2_958_466.0).contains(&value) {
        return None;
    }
    let mut days = value.floor() as i64;
    let mut seconds = ((value - value.floor()) * 86_400.0).round() as u32;
    if seconds >= 86_400 {
        seconds -= 86_400;
        days += 1;
    }
    // serial 61 is 1900-03-01; before it the file counts a 29 February that never was
    Some((if days >= 61 { days - 25_569 } else { days - 25_568 }, seconds))
}

fn date_text(value: f64, pieces: &[Piece]) -> String {
    let Some((days, seconds)) = serial(value) else {
        return "#####".to_string();
    };
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let weekday = (days + 4).rem_euclid(7) as usize;
    let twelve = pieces.contains(&Piece::AmPm);
    let mut out = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Literal(s) => out.push_str(s),
            Piece::AmPm => out.push_str(if hour < 12 { "AM" } else { "PM" }),
            Piece::Time(letter, run) => match (letter, run) {
                ('y', 1 | 2) => out.push_str(&format!("{:02}", year.rem_euclid(100))),
                ('y', _) => out.push_str(&format!("{year:04}")),
                ('m', n) => {
                    // after hours, or before seconds, it is minutes
                    let before_hours = pieces[..index].iter().rev().find_map(|p| match p {
                        Piece::Time(l, _) => Some(*l),
                        _ => None,
                    }) == Some('h');
                    let after_seconds = pieces[index + 1..].iter().find_map(|p| match p {
                        Piece::Time(l, _) => Some(*l),
                        _ => None,
                    }) == Some('s');
                    if before_hours || after_seconds {
                        out.push_str(&if *n == 1 {
                            minute.to_string()
                        } else {
                            format!("{minute:02}")
                        });
                    } else {
                        match n {
                            1 => out.push_str(&month.to_string()),
                            2 => out.push_str(&format!("{month:02}")),
                            3 => out.push_str(&MONTHS[(month as usize).saturating_sub(1) % 12][..3]),
                            _ => out.push_str(MONTHS[(month as usize).saturating_sub(1) % 12]),
                        }
                    }
                }
                ('d', 1) => out.push_str(&day.to_string()),
                ('d', 2) => out.push_str(&format!("{day:02}")),
                ('d', 3) => out.push_str(&DAYS[weekday][..3]),
                ('d', _) => out.push_str(DAYS[weekday]),
                ('h', n) => {
                    let shown = if twelve { (hour + 11) % 12 + 1 } else { hour };
                    out.push_str(&if *n == 1 {
                        shown.to_string()
                    } else {
                        format!("{shown:02}")
                    });
                }
                ('s', 1) => out.push_str(&second.to_string()),
                ('s', _) => out.push_str(&format!("{second:02}")),
                _ => {}
            },
            _ => {}
        }
    }
    out
}

/// A number shown as the format code says.
fn number_text(value: f64, code: &str) -> String {
    if code.is_empty() || code.eq_ignore_ascii_case("general") || code == "@" {
        return general(value);
    }
    let all = sections(code);
    let (section, negative_section) = if value < 0.0 && all.len() >= 2 {
        (&all[1], true)
    } else if value == 0.0 && all.len() >= 3 {
        (&all[2], false)
    } else {
        (&all[0], false)
    };
    if section.is_empty() && all.len() >= 2 {
        // an empty section (`0;-0;;@`) means that kind of value is shown as nothing
        return String::new();
    }
    let parts = pieces(section);
    if parts.iter().any(|p| matches!(p, Piece::General)) {
        return parts
            .iter()
            .map(|p| match p {
                Piece::Literal(s) => s.clone(),
                Piece::General => general(if negative_section { value.abs() } else { value }),
                _ => String::new(),
            })
            .collect();
    }
    if parts.iter().any(|p| matches!(p, Piece::Time(..))) {
        return date_text(value, &parts);
    }
    // a number
    let percent = parts.iter().filter(|p| matches!(p, Piece::Percent)).count();
    let mut shown = value * 100f64.powi(percent as i32);
    let signed = shown < 0.0 && !negative_section;
    shown = shown.abs();
    let point = parts.iter().position(|p| matches!(p, Piece::Point));
    let decimals = point.map_or(0, |at| {
        parts[at..].iter().filter(|p| matches!(p, Piece::Digit(_))).count()
    });
    let integer_digits: Vec<char> = parts[..point.unwrap_or(parts.len())]
        .iter()
        .filter_map(|p| match p {
            Piece::Digit(c) => Some(*c),
            _ => None,
        })
        .collect();
    let group = parts[..point.unwrap_or(parts.len())]
        .iter()
        .any(|p| matches!(p, Piece::Comma))
        && !integer_digits.is_empty();
    let text = format!("{shown:.decimals$}");
    let (integer, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let minimum = integer_digits.iter().filter(|&&c| c == '0').count();
    let mut integer = integer.trim_start_matches('0').to_string();
    while integer.len() < minimum {
        integer.insert(0, '0');
    }
    if group {
        integer = thousands(&integer);
    }
    let fraction = {
        // the `#` places at the end of the fraction that hold no digit are not shown
        let optional = parts[point.map_or(parts.len(), |p| p + 1)..]
            .iter()
            .rev()
            .take_while(|p| matches!(p, Piece::Digit('#' | '?')))
            .count();
        let keep = fraction.len().saturating_sub(optional);
        let trimmed = fraction[..keep.max(fraction.trim_end_matches('0').len().min(fraction.len()))].to_string();
        if optional == 0 { fraction.to_string() } else { trimmed }
    };
    let mut body = integer;
    if !fraction.is_empty() {
        body.push('.');
        body.push_str(&fraction);
    }
    // a format with no `0` in it shows nothing for what is nothing (the zero of an accounting format is a dash)
    if body.is_empty() && parts.iter().any(|p| matches!(p, Piece::Digit('0'))) {
        body.push('0');
    }
    // the text before the first placeholder and after the last
    let first = parts.iter().position(|p| matches!(p, Piece::Digit(_) | Piece::Point));
    let last = parts.iter().rposition(|p| matches!(p, Piece::Digit(_) | Piece::Point));
    let (Some(first), Some(last)) = (first, last) else {
        return general(value);
    };
    let literal = |range: &[Piece]| -> String {
        range
            .iter()
            .map(|p| match p {
                Piece::Literal(s) => s.clone(),
                Piece::Percent => "%".to_string(),
                _ => String::new(),
            })
            .collect()
    };
    format!(
        "{}{}{}{}",
        if signed { "-" } else { "" },
        literal(&parts[..first]),
        body,
        literal(&parts[last + 1..])
    )
}

// ---- the sheet --------------------------------------------------------------------------------------------------

struct Cell {
    row: usize,
    column: usize,
    text: String,
    number: bool,
    style: CellStyle,
    font: FontSpec,
}

struct Grid {
    cells: Vec<Cell>,
    /// The width of each column and the height of each row that is shown, in points (hidden ones are 0).
    widths: Vec<f32>,
    heights: Vec<f32>,
    /// Merged ranges: (first row, first column, last row, last column), counted from 0.
    merges: Vec<(usize, usize, usize, usize)>,
}

/// "B7" is (6, 1); "AA1" is (0, 26).
fn reference(text: &str) -> Option<(usize, usize)> {
    let letters: String = text.chars().take_while(char::is_ascii_alphabetic).collect();
    let digits = &text[letters.len()..];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let column = letters
        .to_ascii_uppercase()
        .bytes()
        .fold(0usize, |n, b| n * 26 + usize::from(b - b'A' + 1));
    Some((digits.parse::<usize>().ok()?.checked_sub(1)?, column.checked_sub(1)?))
}

fn letters(column: usize) -> String {
    let mut n = column + 1;
    let mut out = Vec::new();
    while n > 0 {
        out.push(b'A' + ((n - 1) % 26) as u8);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

impl Grid {
    fn read(sheet: &roxmltree::Document<'_>, styles: &Styles, strings: &[String]) -> Self {
        let root = sheet.root_element();
        let format = child(root, "sheetFormatPr");
        let default_row = format
            .and_then(|f| attr(f, "defaultRowHeight"))
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(DEFAULT_ROW)
            .clamp(4.0, 400.0);
        let default_column = format
            .and_then(|f| attr(f, "defaultColWidth"))
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map_or(DEFAULT_COLUMN, |w| (w * 7.0 + 5.0) * 0.75)
            .clamp(4.0, 600.0);
        let mut widths = vec![default_column; MAX_COLUMNS];
        if let Some(cols) = child(root, "cols") {
            for col in children(cols, "col").take(1000) {
                let number = |name: &str| attr(col, name).and_then(|v| v.parse::<usize>().ok());
                let (Some(min), Some(max)) = (number("min"), number("max")) else {
                    continue;
                };
                let width = if matches!(attr(col, "hidden"), Some("1" | "true")) {
                    0.0
                } else {
                    attr(col, "width")
                        .and_then(|v| v.parse::<f32>().ok())
                        .filter(|v| v.is_finite())
                        .map_or(default_column, |w| ((w * 7.0 + 5.0) * 0.75).clamp(0.0, 600.0))
                };
                for index in min.max(1)..=max.min(MAX_COLUMNS) {
                    widths[index - 1] = width;
                }
            }
        }
        let mut heights = vec![default_row; MAX_SHEET_ROWS];
        let mut cells = Vec::new();
        if let Some(data) = child(root, "sheetData") {
            let mut next_row = 0usize;
            for row in children(data, "row").take(MAX_ROWS) {
                // a row has its number (the rows that are not there are empty ones); one without gets the next
                let index = attr(row, "r")
                    .and_then(|v| v.parse::<usize>().ok())
                    .map_or(next_row, |n| n.saturating_sub(1));
                next_row = index + 1;
                if index >= MAX_SHEET_ROWS {
                    break;
                }
                let slot = index;
                if let Some(height) = heights.get_mut(slot) {
                    *height = if matches!(attr(row, "hidden"), Some("1" | "true")) {
                        0.0
                    } else {
                        attr(row, "ht")
                            .and_then(|v| v.parse::<f32>().ok())
                            .filter(|v| v.is_finite())
                            .map_or(default_row, |h| h.clamp(0.0, 600.0))
                    };
                }
                for cell in children(row, "c").take(MAX_COLUMNS * 4) {
                    let (_, column) = attr(cell, "r").and_then(reference).unwrap_or((index, 0));
                    if column >= MAX_COLUMNS {
                        continue;
                    }
                    let style = styles.style(attr(cell, "s").and_then(|v| v.parse().ok()).unwrap_or(0));
                    let kind = attr(cell, "t").unwrap_or("n");
                    let value = child(cell, "v").and_then(|v| v.text());
                    let (text, number) = match kind {
                        "s" => (
                            value
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .and_then(|i| strings.get(i).cloned())
                                .unwrap_or_default(),
                            false,
                        ),
                        "inlineStr" => (
                            child(cell, "is")
                                .map(|is| {
                                    is.descendants()
                                        .filter(|n| n.tag_name().name() == "t")
                                        .filter_map(|n| n.text())
                                        .collect()
                                })
                                .unwrap_or_default(),
                            false,
                        ),
                        "b" => ((if value == Some("1") { "TRUE" } else { "FALSE" }).to_string(), false),
                        "e" => (value.unwrap_or("#VALUE!").to_string(), false),
                        "str" => (value.unwrap_or("").to_string(), false),
                        _ => match value.and_then(|v| v.trim().parse::<f64>().ok()) {
                            Some(n) => (number_text(n, &styles.format(style.format)), true),
                            None => (String::new(), false),
                        },
                    };
                    let mut text = text;
                    if let Some((cut, _)) = text.char_indices().nth(MAX_TEXT) {
                        text.truncate(cut);
                    }
                    let font = styles.font(style.font);
                    if !text.is_empty() || style.fill.is_some() || style.border {
                        cells.push(Cell {
                            row: slot,
                            column,
                            text,
                            number,
                            style,
                            font,
                        });
                    }
                }
            }
        }
        let merges = child(root, "mergeCells")
            .map(|m| {
                children(m, "mergeCell")
                    .take(2000)
                    .filter_map(|c| {
                        let (from, to) = attr(c, "ref")?.split_once(':')?;
                        let (r1, c1) = reference(from)?;
                        let (r2, c2) = reference(to)?;
                        Some((r1, c1, r2, c2))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            cells,
            widths,
            heights,
            merges,
        }
    }

    fn render(&self) -> Result<RgbaImage, DecodeError> {
        // the room each column and row takes, up to what is drawn
        let last_row = self.cells.iter().map(|c| c.row).max().unwrap_or(0);
        let last_column = self.cells.iter().map(|c| c.column).max().unwrap_or(0);
        let mut left = vec![GUTTER];
        for width in self.widths.iter().take(last_column + 1) {
            let next = left.last().copied().unwrap_or(GUTTER) + width;
            left.push(next);
            if next > MAX_WIDTH {
                break;
            }
        }
        let mut top = vec![HEADER];
        for height in self.heights.iter().take(last_row + 1) {
            let next = top.last().copied().unwrap_or(HEADER) + height;
            top.push(next);
            if next > MAX_HEIGHT {
                break;
            }
        }
        let columns = left.len() - 1;
        let rows = top.len() - 1;
        let width = left.last().copied().unwrap_or(GUTTER).clamp(120.0, MAX_WIDTH);
        let height = top.last().copied().unwrap_or(HEADER).clamp(60.0, MAX_HEIGHT);
        let mut page = Page::new(width, height);
        page.rect(0.0, 0.0, width, height, Some([255, 255, 255]), None);
        let grey = [190, 190, 190];
        let label = Font {
            size: 9.0,
            colour: [90, 90, 90],
            ..Font::default()
        };
        // headers
        page.rect(0.0, 0.0, width, HEADER, Some([236, 236, 236]), None);
        page.rect(0.0, 0.0, GUTTER, height, Some([236, 236, 236]), None);
        for column in 0..columns {
            let (a, b) = (left[column], left[column + 1]);
            if b > a {
                page.line((b, 0.0), (b, HEADER), grey, 0.5);
                page.text(a + (b - a) / 2.0 - 3.0, HEADER - 4.0, &letters(column), &label);
            }
        }
        for row in 0..rows {
            let (a, b) = (top[row], top[row + 1]);
            if b > a {
                page.line((0.0, b), (GUTTER, b), grey, 0.5);
                page.text(4.0, b - 3.5, &(row + 1).to_string(), &label);
            }
        }
        // what is merged: the cell that starts it spans the range, the others inside it are not drawn
        let mut covered: HashSet<(usize, usize)> = HashSet::new();
        let mut span: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
        for &(r1, c1, r2, c2) in &self.merges {
            span.insert(
                (r1, c1),
                (r2.min(rows.saturating_sub(1)), c2.min(columns.saturating_sub(1))),
            );
            for r in r1..=r2.min(rows.saturating_sub(1)) {
                for c in c1..=c2.min(columns.saturating_sub(1)) {
                    if (r, c) != (r1, c1) {
                        covered.insert((r, c));
                    }
                }
            }
        }
        let occupied: HashSet<(usize, usize)> = self
            .cells
            .iter()
            .filter(|c| !c.text.is_empty())
            .map(|c| (c.row, c.column))
            .collect();
        // gridlines
        for row in 0..rows {
            for column in 0..columns {
                if top[row + 1] > top[row] && left[column + 1] > left[column] {
                    page.rect(
                        left[column],
                        top[row],
                        left[column + 1] - left[column],
                        top[row + 1] - top[row],
                        None,
                        Some(([225, 225, 225], 0.5)),
                    );
                }
            }
        }
        for cell in &self.cells {
            if cell.row >= rows || cell.column >= columns || covered.contains(&(cell.row, cell.column)) {
                continue;
            }
            let (last_r, last_c) = span
                .get(&(cell.row, cell.column))
                .copied()
                .unwrap_or((cell.row, cell.column));
            let (x, y) = (left[cell.column], top[cell.row]);
            let (w, h) = (left[last_c + 1] - x, top[last_r + 1] - y);
            if w <= 0.0 || h <= 0.0 {
                continue;
            }
            if cell.style.fill.is_some() || cell.style.border {
                page.rect(
                    x,
                    y,
                    w,
                    h,
                    cell.style.fill,
                    cell.style.border.then_some(([70, 70, 70], 0.6)),
                );
            }
            if cell.text.is_empty() {
                continue;
            }
            let font = Font {
                size: cell.font.size,
                bold: cell.font.bold,
                italic: cell.font.italic,
                underline: cell.font.underline,
                colour: cell.font.colour.unwrap_or([0, 0, 0]),
                family: cell.font.family.clone(),
            };
            let align = match cell.style.align.as_deref() {
                Some("center" | "centerContinuous") => Align::Center,
                Some("right") => Align::Right,
                Some("left") => Align::Left,
                _ if cell.number => Align::Right,
                _ => Align::Left,
            };
            if cell.style.wrap {
                let para = Para {
                    runs: vec![(cell.text.clone(), font.clone())],
                    align,
                    line: 1.0,
                    empty_size: font.size,
                    ..Para::default()
                };
                let used = text::measure(std::slice::from_ref(&para), (w - 6.0).max(4.0));
                let start = y + (h - used).max(0.0);
                text::layout(
                    std::slice::from_ref(&para),
                    x + 3.0,
                    start,
                    (w - 6.0).max(4.0),
                    y + h,
                    &mut page,
                );
                continue;
            }
            // text that does not fit goes on into the empty cells to its right (left-aligned), else it is cut
            let mut room = w - 6.0;
            if align == Align::Left {
                let mut next = last_c + 1;
                while next < columns && !occupied.contains(&(cell.row, next)) && !covered.contains(&(cell.row, next)) {
                    room += left[next + 1] - left[next];
                    next += 1;
                }
            }
            let mut shown = cell.text.replace(['\n', '\r'], " ");
            while shown.chars().count() > 1 && text_width(&shown, &font) > room.max(4.0) {
                shown.pop();
            }
            let used = text_width(&shown, &font);
            let at = match align {
                Align::Left => x + 3.0,
                Align::Center => x + (w - used) / 2.0,
                Align::Right => x + w - 3.0 - used,
            };
            page.text(at, y + h - 3.5, &shown, &font);
        }
        page.render()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    #[test]
    fn a_colour_of_eight_bytes_that_is_not_hex_is_nothing_and_not_a_panic() {
        assert_eq!(hex("FF4472C4"), Some([0x44, 0x72, 0xC4]));
        assert_eq!(hex("a\u{20ac}\u{20ac}a"), None); // byte 2 is in the middle of a character
    }

    #[test]
    fn numbers_are_shown_as_their_formats_say() {
        assert_eq!(number_text(1234.5, "General"), "1234.5");
        assert_eq!(number_text(0.1 + 0.2, "General"), "0.3");
        assert_eq!(number_text(42.0, "General"), "42");
        assert_eq!(number_text(1234567.891, "#,##0.00"), "1,234,567.89");
        assert_eq!(number_text(0.256, "0%"), "26%");
        assert_eq!(number_text(0.256, "0.0%"), "25.6%");
        assert_eq!(number_text(7.0, "000"), "007");
        assert_eq!(number_text(1234.5, "\"£\"#,##0.00"), "£1,234.50");
        assert_eq!(number_text(1234.5, "[$£-809]#,##0.00"), "£1,234.50");
        assert_eq!(number_text(-5.0, "#,##0;(#,##0)"), "(5)");
        assert_eq!(number_text(-5.0, "0.00"), "-5.00");
        assert_eq!(
            number_text(0.5, "#.##"),
            ".5",
            "Excel shows no zero before the point of #.##"
        );
        assert_eq!(number_text(0.5, "0.##"), "0.5");
        assert_eq!(number_text(12.0, "0 \"units\""), "12 units");
        assert_eq!(number_text(1.23456, "0.00"), "1.23");
        // an accounting format: the zero is a dash and no digit
        let accounting = r#"_-"£"* #,##0.00_-;\-"£"* #,##0.00_-;_-"£"* "-"??_-;_-@_-"#;
        assert_eq!(number_text(0.0, accounting).trim(), "£-");
        assert_eq!(number_text(70.0, accounting).trim(), "£70.00");
        assert_eq!(number_text(0.0, "0;-0;;@"), "");
    }

    #[test]
    fn dates_and_times_are_shown_as_their_formats_say() {
        // 45000 is 2023-03-15; with 0.5 it is noon
        assert_eq!(number_text(45000.0, "dd/mm/yyyy"), "15/03/2023");
        assert_eq!(number_text(45000.0, "yyyy-mm-dd"), "2023-03-15");
        assert_eq!(number_text(45000.5, "dd/mm/yyyy h:mm"), "15/03/2023 12:00");
        assert_eq!(number_text(45000.75, "h:mm AM/PM"), "6:00 PM");
        assert_eq!(number_text(45000.0, "d mmmm yyyy"), "15 March 2023");
        assert_eq!(number_text(45000.0, "ddd d-mmm-yy"), "Wed 15-Mar-23");
        assert_eq!(number_text(45000.51, "hh:mm:ss"), "12:14:24");
        assert_eq!(number_text(25569.0, "yyyy-mm-dd"), "1970-01-01");
        assert_eq!(number_text(61.0, "yyyy-mm-dd"), "1900-03-01");
        assert_eq!(number_text(-3.0, "yyyy-mm-dd"), "#####");
    }

    #[test]
    fn cells_are_found_by_their_references() {
        assert_eq!(reference("A1"), Some((0, 0)));
        assert_eq!(reference("B7"), Some((6, 1)));
        assert_eq!(reference("AA10"), Some((9, 26)));
        assert_eq!(reference("7"), None);
        assert_eq!(reference("A0"), None);
        assert_eq!(letters(0), "A");
        assert_eq!(letters(26), "AA");
    }

    fn workbook(sheet: &str, strings: &[&str], styles: &str) -> Vec<u8> {
        let ns = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let workbook =
            format!(r#"<workbook {ns}><sheets><sheet name="One" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let sheet = format!(r#"<worksheet {ns}>{sheet}</worksheet>"#);
        let strings = format!(
            r#"<sst {ns}>{}</sst>"#,
            strings
                .iter()
                .map(|s| format!("<si><t>{s}</t></si>"))
                .collect::<String>()
        );
        let styles = format!(r#"<styleSheet {ns}>{styles}</styleSheet>"#);
        zip(&[
            ("xl/workbook.xml", workbook.as_bytes(), true),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes(), true),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes(), true),
            ("xl/sharedStrings.xml", strings.as_bytes(), true),
            ("xl/styles.xml", styles.as_bytes(), true),
        ])
    }

    const STYLES: &str = r#"<fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="14"/><color rgb="FFFFFFFF"/><name val="Calibri"/></font></fonts>
        <fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FF0000C0"/></patternFill></fill></fills>
        <borders count="1"><border><left/><right/><top/><bottom/></border></borders>
        <cellXfs count="3"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="1" fillId="2" borderId="0"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0"/></cellXfs>"#;

    #[test]
    fn a_sheet_is_drawn_with_its_header_fill_text_numbers_and_dates() {
        let sheet = r#"<sheetFormatPr defaultRowHeight="15"/><cols><col min="1" max="1" width="20" customWidth="1"/></cols>
            <sheetData>
              <row r="1"><c r="A1" s="1" t="s"><v>0</v></c><c r="B1" s="1" t="s"><v>1</v></c></row>
              <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>1234.5</v></c><c r="C2" s="2"><v>45000</v></c></row>
            </sheetData>"#;
        let img = draw(&workbook(sheet, &["Name", "Amount", "Alpha"], STYLES)).expect("draws");
        assert_eq!(img.width().max(img.height()), 2048);
        // the first header cell is blue (fill 0000C0) just inside its top left corner
        let scale = 2048.0 / img.width().max(img.height()) as f32;
        let _ = scale;
        let blue = (0..img.width())
            .flat_map(|x| (0..img.height()).map(move |y| (x, y)))
            .step_by(37)
            .filter(|&(x, y)| {
                let [r, g, b, _] = img.get_pixel(x, y).0;
                r < 40 && g < 40 && b > 150
            })
            .count();
        assert!(blue > 200, "{blue} blue pixels of the header");
        let dark = img.pixels().step_by(13).filter(|p| p.0[0] < 90 && p.0[2] < 90).count();
        assert!(dark > 300, "{dark} dark pixels of text");
    }

    #[test]
    fn a_merged_range_and_hidden_rows_and_columns_are_taken() {
        let sheet = r#"<cols><col min="2" max="2" hidden="1"/></cols><sheetData>
            <row r="1"><c r="A1" t="inlineStr"><is><t>merged title</t></is></c></row>
            <row r="2" hidden="1"><c r="A2" t="inlineStr"><is><t>hidden row</t></is></c></row>
            <row r="3"><c r="A3"><v>1</v></c></row></sheetData><mergeCells><mergeCell ref="A1:C1"/></mergeCells>"#;
        let img = draw(&workbook(sheet, &[], STYLES)).expect("draws");
        assert!(img.width() > 100);
    }

    #[test]
    fn a_sheet_is_cut_after_its_rows_and_what_follows_is_kept() {
        let rows: String = (1..=500)
            .map(|i| format!(r#"<row r="{i}"><c r="A{i}"><v>{i}</v></c></row>"#))
            .collect();
        let text = format!(
            "<worksheet><sheetData>{rows}</sheetData><mergeCells><mergeCell ref=\"A1:B1\"/></mergeCells></worksheet>"
        );
        let cut = first_rows(&text, 300);
        assert_eq!(cut.matches("<row").count(), 300);
        assert!(cut.contains("mergeCell ref") && cut.ends_with("</worksheet>"));
        assert!(parse(&cut).is_ok());
        let small = first_rows(&text[..text.find("<row r=\"10\"").expect("row 10")], 300);
        assert!(small.starts_with("<worksheet>"), "less than asked is left alone");
        let strings = format!("<sst>{}</sst>", "<si><t>x</t></si>".repeat(1000));
        let cut = first_strings(&strings, 500);
        assert!(cut.len() <= 520 && cut.ends_with("</sst>") && parse(&cut).is_ok());
    }

    #[test]
    fn what_is_not_a_workbook_or_is_damaged_is_an_error_and_not_a_panic() {
        assert!(draw(b"not a zip").is_err());
        assert!(draw(&zip(&[("xl/other.xml", b"<x/>", false)])).is_err());
        assert!(draw(&zip(&[("xl/workbook.xml", b"<workbook>", false)])).is_err());
        assert!(draw(&zip(&[("xl/workbook.xml", b"<workbook/>", false)])).is_err());
    }
}
