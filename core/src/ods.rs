//! The top left of the first sheet of an OpenDocument spreadsheet (ods, LibreOffice Calc) as a picture: a preview of what the sheet
//! looks like, not a rendering that matches Calc. The picture is drawn by the same code as the sheet of an xlsx (`xlsx::Grid`); this
//! module reads an ods into that grid.
//!
//! An ods is a ZIP of XML (`content.xml`, `styles.xml`). Read here: the first table's first rows (300 at most: the text of the file is
//! cut after them before it is parsed, so that a sheet of a million rows costs no more), the widths of the columns and the heights of
//! the rows (from their styles), hidden (collapsed) rows and columns, rows and columns that repeat, merged cells, the text each cell
//! shows (an ods stores the text as Calc formatted it, so no number format is needed), and the cell styles: font, size, bold, italic,
//! underline, colour, fill, borders, alignment, wrapping.
//!
//! Not done: charts, pictures, shapes, conditional formatting, formulas (the value Calc saved is shown), rich text inside a cell (its
//! pieces are joined), vertical alignment other than the bottom, rotated text, the colour of a border (all grey), the styles that a
//! column or a row gives to the cells it holds, sheets after the first.
//!
//! Made for files made to hurt as well: the archive is read with limits (`archive`), the XML parser refuses DTDs and has limits
//! (`xml`), and what is drawn is limited by rows, columns and size; a row or a column that repeats a billion times is counted, not
//! made.

use std::path::Path;

use image::RgbaImage;
use roxmltree::Node;

use crate::archive::Archive;
use crate::decode::DecodeError;
use crate::flow::Align;
use crate::guard::{draw_on_thread, has_extension, read_limited, unreadable};
use crate::odt::{Props, Size, Styles};
use crate::office::{MAX_BYTES, MAX_PART, is_encrypted};
use crate::xlsx::{Cell, CellStyle, FontSpec, Grid};
use crate::xml::{attr, child, children, parse};

/// Rows of the sheet that are read, and the rows and the columns that are kept at most.
const MAX_ROWS: usize = 300;
const MAX_SHEET_ROWS: usize = 1000;
const MAX_COLUMNS: usize = 60;
/// A cell's text is cut to this many characters.
const MAX_TEXT: usize = 4000;
/// A row that repeats and has something in it is copied this many times at most (an empty one is only counted).
const MAX_REPEAT: usize = 50;
/// The width of a column and the height of a row that say nothing, in points (Calc's own: 2.26 cm and 0.45 cm).
const DEFAULT_COLUMN: f32 = 64.0;
const DEFAULT_ROW: f32 = 12.8;
/// The size of text that says nothing.
const DEFAULT_SIZE: f32 = 10.0;
/// How deep the groups of rows and columns are looked into.
const MAX_GROUPS: usize = 4;

/// Whether the name says ods.
pub fn is_ods(path: &Path) -> bool {
    has_extension(path, "ods")
}

/// Draws the top left of the first sheet of the file in `path`; a panic of the layout is an error.
pub fn render_first_page(path: &Path) -> Result<RgbaImage, DecodeError> {
    let data = read_limited(path, MAX_BYTES)?;
    draw_on_thread("ods", "the spreadsheet could not be drawn", || draw(&data))
}

/// `text` cut after its `rows`-th end of a row, and the tags that are open there closed; `None` when it has fewer rows (or the prefix
/// of the namespace is not the usual one), and then it is taken whole.
fn first_rows(text: &str, rows: usize) -> Option<String> {
    const END: &str = "</table:table-row>";
    let (at, _) = text.match_indices(END).nth(rows.saturating_sub(1))?;
    let kept = &text[..at + END.len()];
    // the names of the elements still open: a start tag pushes, an end tag pops, an empty element does nothing
    let mut open: Vec<&str> = Vec::new();
    let mut rest = kept;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>')?;
        let tag = &rest[..end];
        rest = &rest[end + 1..];
        if tag.starts_with('?') || tag.starts_with('!') || tag.ends_with('/') {
            continue;
        }
        if let Some(name) = tag.strip_prefix('/') {
            open.pop().filter(|o| *o == name.trim())?;
        } else {
            open.push(tag.split_whitespace().next()?);
        }
    }
    let mut cut = kept.to_string();
    for name in open.iter().rev() {
        cut.push_str("</");
        cut.push_str(name);
        cut.push('>');
    }
    Some(cut)
}

fn draw(data: &[u8]) -> Result<RgbaImage, DecodeError> {
    let archive = Archive::new(data)?;
    if is_encrypted(&archive) {
        return Err(unreadable("the document is protected with a password"));
    }
    let content = archive.read("content.xml", MAX_PART).map_err(|e| match e {
        DecodeError::Unreadable(_) => unreadable("this is no OpenDocument spreadsheet (no content.xml)"),
        other => other,
    })?;
    let text = String::from_utf8_lossy(&content);
    let cut = first_rows(&text, MAX_ROWS);
    let xml = cut
        .as_deref()
        .and_then(|c| parse(c).ok())
        .map_or_else(|| parse(&text), Ok)?;
    let styles_text = archive.text("styles.xml", MAX_PART);
    let styles_xml = styles_text.as_deref().and_then(|t| parse(t).ok());

    let root = xml.root_element();
    let table = child(root, "body")
        .and_then(|b| child(b, "spreadsheet"))
        .and_then(|s| children(s, "table").next())
        .ok_or_else(|| unreadable("the document has no sheet"))?;

    let mut styles = Styles::default();
    if let Some(xml) = &styles_xml {
        styles.read(xml.root_element());
    }
    styles.read(root);
    read_table(table, &styles).render()
}

/// The elements with this name among the children of `node` and of the groups they may stand in (`table-row-group`, the header rows...).
fn members<'a, 'i>(node: Node<'a, 'i>, name: &str, depth: usize, out: &mut Vec<Node<'a, 'i>>, limit: usize) {
    for item in node.children().filter(|c| c.is_element()) {
        if out.len() >= limit {
            return;
        }
        let found = item.tag_name().name();
        if found == name {
            out.push(item);
        } else if depth < MAX_GROUPS
            && matches!(
                found,
                "table-row-group"
                    | "table-header-rows"
                    | "table-rows"
                    | "table-column-group"
                    | "table-header-columns"
                    | "table-columns"
            )
        {
            members(item, name, depth + 1, out, limit);
        }
    }
}

fn count(node: Node<'_, '_>, name: &str) -> usize {
    attr(node, name)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

fn hidden(node: Node<'_, '_>) -> bool {
    matches!(attr(node, "visibility"), Some("collapse" | "filter"))
}

/// What a cell shows: its paragraphs, one to a line; a number with no text takes its value.
fn shown(cell: Node<'_, '_>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for paragraph in children(cell, "p") {
        let mut line = String::new();
        for node in paragraph.descendants() {
            if node.is_text() {
                line.push_str(node.text().unwrap_or(""));
            } else if node.is_element() {
                match node.tag_name().name() {
                    "s" => line.push_str(&" ".repeat(count(node, "c").min(50))),
                    "tab" => line.push(' '),
                    "line-break" => line.push('\n'),
                    _ => {}
                }
            }
            if line.len() > MAX_TEXT * 4 {
                break;
            }
        }
        lines.push(line);
    }
    let mut text = lines.join("\n");
    if text.is_empty()
        && let Some(value) = attr(cell, "value").or_else(|| attr(cell, "string-value"))
    {
        text = value.to_string();
    }
    if let Some((cut, _)) = text.char_indices().nth(MAX_TEXT) {
        text.truncate(cut);
    }
    text
}

fn font_of(props: &Props) -> FontSpec {
    let mut font = props.font();
    // the size that the style does not say is Calc's, not the text's
    let size = match props.size {
        Some(Size::Points(s)) => s,
        Some(Size::Part(p)) => DEFAULT_SIZE * p,
        None => DEFAULT_SIZE,
    };
    font.size = size.clamp(1.0, 400.0);
    FontSpec {
        size: font.size,
        bold: font.bold,
        italic: font.italic,
        underline: font.underline,
        colour: (font.colour != [0, 0, 0]).then_some(font.colour),
        family: font.family,
    }
}

fn read_table(table: Node<'_, '_>, styles: &Styles) -> Grid {
    let mut widths = vec![DEFAULT_COLUMN; MAX_COLUMNS];
    let mut column_nodes = Vec::new();
    members(table, "table-column", 0, &mut column_nodes, 1000);
    let mut at = 0usize;
    for column in column_nodes {
        let width = if hidden(column) {
            0.0
        } else {
            attr(column, "style-name")
                .and_then(|n| styles.columns.get(n))
                .copied()
                .unwrap_or(DEFAULT_COLUMN)
                .clamp(0.0, 600.0)
        };
        for _ in 0..count(column, "number-columns-repeated").min(MAX_COLUMNS) {
            if at >= MAX_COLUMNS {
                break;
            }
            widths[at] = width;
            at += 1;
        }
    }

    let mut heights = vec![DEFAULT_ROW; MAX_SHEET_ROWS];
    let mut cells: Vec<Cell> = Vec::new();
    let mut merges = Vec::new();
    let mut row_nodes = Vec::new();
    members(table, "table-row", 0, &mut row_nodes, MAX_ROWS * 4);
    let mut index = 0usize;
    for row in row_nodes {
        if index >= MAX_SHEET_ROWS {
            break;
        }
        let repeat = count(row, "number-rows-repeated");
        let height = if hidden(row) {
            0.0
        } else {
            attr(row, "style-name")
                .and_then(|n| styles.rows.get(n))
                .copied()
                .unwrap_or(DEFAULT_ROW)
                .clamp(0.0, 600.0)
        };
        let mut cell_nodes: Vec<Node<'_, '_>> = Vec::new();
        for cell in row.children().filter(|c| c.is_element()) {
            if cell_nodes.len() >= MAX_COLUMNS * 4 {
                break;
            }
            if matches!(cell.tag_name().name(), "table-cell" | "covered-table-cell") {
                cell_nodes.push(cell);
            }
        }
        // the cells of one copy of the row: (column, cell, how many columns and rows it spans)
        let mut one: Vec<(usize, Cell, usize, usize)> = Vec::new();
        let mut column = 0usize;
        for node in cell_nodes {
            if column >= MAX_COLUMNS {
                break;
            }
            let across = count(node, "number-columns-repeated").min(MAX_COLUMNS);
            if node.tag_name().name() == "table-cell" {
                let text = shown(node);
                let props = styles.resolve("table-cell", attr(node, "style-name"));
                if !text.is_empty() || props.fill.is_some() || props.border == Some(true) {
                    let align = props.align.map(|a| {
                        match a {
                            Align::Left => "left",
                            Align::Center => "center",
                            Align::Right => "right",
                        }
                        .to_string()
                    });
                    let number = matches!(
                        attr(node, "value-type"),
                        Some("float" | "percentage" | "currency" | "date" | "time")
                    );
                    for copy in 0..across {
                        if column + copy >= MAX_COLUMNS {
                            break;
                        }
                        one.push((
                            column + copy,
                            Cell {
                                row: 0,
                                column: column + copy,
                                text: text.clone(),
                                number,
                                style: CellStyle {
                                    fill: props.fill,
                                    border: props.border == Some(true),
                                    align: align.clone(),
                                    wrap: props.wrap == Some(true),
                                    ..CellStyle::default()
                                },
                                font: font_of(&props),
                            },
                            count(node, "number-columns-spanned"),
                            count(node, "number-rows-spanned"),
                        ));
                    }
                }
            }
            column += across;
        }
        // an empty row that repeats is only counted; one with something in it is copied (a few times at most)
        let copies = if one.is_empty() { repeat } else { repeat.min(MAX_REPEAT) };
        for copy in 0..copies {
            let row_index = index + copy;
            if row_index >= MAX_SHEET_ROWS {
                break;
            }
            heights[row_index] = height;
            for (column, cell, across, down) in &one {
                cells.push(Cell {
                    row: row_index,
                    column: *column,
                    text: cell.text.clone(),
                    number: cell.number,
                    style: cell.style.clone(),
                    font: cell.font.clone(),
                });
                if (*across > 1 || *down > 1) && merges.len() < 2000 {
                    merges.push((row_index, *column, row_index + down - 1, column + across - 1));
                }
            }
        }
        index += repeat;
    }
    Grid {
        cells,
        widths,
        heights,
        merges,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;
    use crate::archive::tests::zip;

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0""#;

    fn ods(table: &str, automatic: &str) -> Vec<u8> {
        let content = format!(
            r#"<?xml version="1.0"?><office:document-content {NS}><office:automatic-styles>{automatic}</office:automatic-styles><office:body><office:spreadsheet>{table}</office:spreadsheet></office:body></office:document-content>"#
        );
        zip(&[
            ("mimetype", b"application/vnd.oasis.opendocument.spreadsheet", false),
            ("content.xml", content.as_bytes(), true),
        ])
    }

    fn cell(text: &str) -> String {
        format!(r#"<table:table-cell office:value-type="string"><text:p>{text}</text:p></table:table-cell>"#)
    }

    #[test]
    fn the_cut_closes_what_is_open() {
        let text = r#"<?xml version="1.0"?><a:x><b:t><b:r></b:r><b:r></b:r><b:r></b:r></b:t></a:x>"#;
        let cut = first_rows(&text.replace("b:r", "table:table-row"), 2).expect("cuts");
        assert!(cut.ends_with("</table:table-row></b:t></a:x>"), "{cut}");
        assert!(
            first_rows(text, 2).is_none(),
            "a sheet with no such rows is taken whole"
        );
    }

    #[test]
    fn a_small_sheet_is_drawn_with_its_fill_and_its_text() {
        let table = format!(
            r#"<table:table><table:table-column table:number-columns-repeated="3"/><table:table-row>{a}{b}</table:table-row><table:table-row><table:table-cell table:style-name="Y"/>{a}</table:table-row></table:table>"#,
            a = cell("alpha"),
            b = cell("beta")
        );
        let data = ods(
            &table,
            r##"<style:style style:name="Y" style:family="table-cell"><style:table-cell-properties fo:background-color="#ffcc00"/></style:style>"##,
        );
        let image = draw(&data).expect("draws");
        let yellow = image
            .pixels()
            .filter(|p| p.0[0] > 240 && p.0[1] > 190 && p.0[1] < 215 && p.0[2] < 30)
            .count();
        assert!(yellow > 100, "the fill of the cell is there");
        let dark = image.pixels().filter(|p| p.0[0] < 100 && p.0[1] < 100).count();
        assert!(dark > 100, "there is text");
    }

    #[test]
    fn a_row_and_a_column_that_repeat_a_billion_times_are_counted_not_made() {
        let table = format!(
            r#"<table:table><table:table-column table:number-columns-repeated="1000000000"/><table:table-row table:number-rows-repeated="1000000000">{}</table:table-row><table:table-row>{}</table:table-row></table:table>"#,
            r#"<table:table-cell table:number-columns-repeated="1000000000"/>"#,
            cell("last")
        );
        let started = std::time::Instant::now();
        assert!(draw(&ods(&table, "")).is_ok());
        assert!(started.elapsed().as_secs() < 10);
    }

    #[test]
    fn a_document_with_a_password_says_so() {
        let manifest = br#"<manifest><file-entry><encryption-data/></file-entry></manifest>"#;
        let data = zip(&[("META-INF/manifest.xml", manifest, true), ("content.xml", b"x", false)]);
        let message = draw(&data).expect_err("refused").to_string();
        assert!(message.contains("password"), "{message}");
    }
    #[test]
    fn a_file_with_no_content_is_an_error_not_a_panic() {
        let data = zip(&[("mimetype", b"application/vnd.oasis.opendocument.spreadsheet", false)]);
        assert!(draw(&data).is_err());
    }
}
