//! A page that a document format lays out (words, boxes, lines, pictures, in points from the top left) and that is drawn
//! into pixels here, so that every document format (docx, pptx, xlsx) has only its own reading and laying out to do.
//!
//! The text is drawn by `resvg` as SVG text with the installed fonts; the pictures are laid over afterwards (resvg here
//! reads no raster pictures). Words are measured by a rough table of widths, not by the font: enough for a preview, where
//! a line a little longer or shorter than in Word does not show.

use std::sync::Arc;

use image::{Rgba, RgbaImage, imageops};

use crate::decode::DecodeError;
use crate::svg;
use crate::xml::escape;

/// The longest side of the picture of a page, in pixels.
const SIDE: f32 = 2048.0;

/// A colour as red, green, blue.
pub type Rgb = [u8; 3];

/// How a piece of text looks.
#[derive(Clone, PartialEq, Debug)]
pub struct Font {
    /// Size in points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub colour: Rgb,
    /// The family the document asks for; the fallbacks are added when drawing.
    pub family: Option<String>,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            colour: [0, 0, 0],
            family: None,
        }
    }
}

#[derive(Debug)]
enum Item {
    /// A line of text: pieces in their own fonts, one after the other, set by the renderer with the real widths.
    Text {
        x: f32,
        baseline: f32,
        runs: Vec<(String, Font)>,
    },
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        fill: Option<Rgb>,
        stroke: Option<(Rgb, f32)>,
    },
    Ellipse {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        fill: Option<Rgb>,
        stroke: Option<(Rgb, f32)>,
    },
    Line {
        from: (f32, f32),
        to: (f32, f32),
        colour: Rgb,
        width: f32,
    },
    Picture {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        image: Arc<RgbaImage>,
    },
}

/// What is on one page, in points.
#[derive(Debug)]
pub struct Page {
    pub width: f32,
    pub height: f32,
    items: Vec<Item>,
}

/// The width of `text` set in `font`, roughly: a table of widths by kind of letter, in units of the size.
pub fn text_width(text: &str, font: &Font) -> f32 {
    let units: f32 =
        text.chars()
            .map(|c| match c {
                ' ' => 0.28,
                'i' | 'j' | 'l' | 't' | 'f' | 'I' | '.' | ',' | ';' | ':' | '\'' | '|' | '!' | '(' | ')' | '['
                | ']' => 0.29,
                'm' | 'w' | 'M' | 'W' | '@' | '%' => 0.86,
                'A'..='Z' => 0.66,
                '0'..='9' => 0.55,
                c if c as u32 >= 0x2E80 => 1.0,                      // CJK
                c if c as u32 >= 0x400 && c as u32 <= 0x4FF => 0.56, // Cyrillic
                _ => 0.5,
            })
            .sum();
    units * font.size * if font.bold { 1.07 } else { 1.0 }
}

fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

/// A family name made safe to put in an attribute (quotes and markup out), with the fallbacks.
fn family(name: Option<&str>) -> String {
    let clean: String = name
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        "Segoe UI, Arial, sans-serif".to_string()
    } else {
        format!("{clean}, Segoe UI, Arial, sans-serif")
    }
}

impl Page {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            items: Vec::new(),
        }
    }

    /// A piece of text whose baseline starts at (`x`, `baseline`).
    pub fn text(&mut self, x: f32, baseline: f32, text: &str, font: &Font) {
        self.runs(x, baseline, vec![(text.to_string(), font.clone())]);
    }

    /// A line whose baseline starts at (`x`, `baseline`) made of pieces, each in its own font.
    pub fn runs(&mut self, x: f32, baseline: f32, runs: Vec<(String, Font)>) {
        if runs.iter().any(|(text, _)| !text.trim().is_empty()) {
            self.items.push(Item::Text { x, baseline, runs });
        }
    }

    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, fill: Option<Rgb>, stroke: Option<(Rgb, f32)>) {
        self.items.push(Item::Rect {
            x,
            y,
            width,
            height,
            fill,
            stroke,
        });
    }

    /// An ellipse inside this box.
    pub fn ellipse(&mut self, x: f32, y: f32, width: f32, height: f32, fill: Option<Rgb>, stroke: Option<(Rgb, f32)>) {
        self.items.push(Item::Ellipse {
            x,
            y,
            width,
            height,
            fill,
            stroke,
        });
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), colour: Rgb, width: f32) {
        self.items.push(Item::Line {
            from,
            to,
            colour,
            width,
        });
    }

    /// A picture laid over the page in this box.
    pub fn picture(&mut self, x: f32, y: f32, width: f32, height: f32, image: Arc<RgbaImage>) {
        self.items.push(Item::Picture {
            x,
            y,
            width,
            height,
            image,
        });
    }

    /// Whether anything is on the page.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The page on white, its longest side `SIDE` pixels.
    pub fn render(&self) -> Result<RgbaImage, DecodeError> {
        let (width, height) = (self.width.max(1.0), self.height.max(1.0));
        let scale = SIDE / width.max(height);
        let (px_w, px_h) = ((width * scale).round().max(1.0), (height * scale).round().max(1.0));
        let mut body = String::new();
        for item in &self.items {
            match item {
                Item::Text { x, baseline, runs } => {
                    body += &format!(r#"<text x="{x:.2}" y="{baseline:.2}" xml:space="preserve">"#);
                    for (text, font) in runs {
                        body += &format!(
                            r#"<tspan font-size="{:.2}" fill="{}" font-family="{}"{}{}{}>{}</tspan>"#,
                            font.size,
                            hex(font.colour),
                            family(font.family.as_deref()),
                            if font.bold { r#" font-weight="bold""# } else { "" },
                            if font.italic { r#" font-style="italic""# } else { "" },
                            if font.underline {
                                r#" text-decoration="underline""#
                            } else {
                                ""
                            },
                            escape(text)
                        );
                    }
                    body += "</text>";
                }
                Item::Rect {
                    x,
                    y,
                    width,
                    height,
                    fill,
                    stroke,
                } => {
                    let fill = fill.map_or("none".to_string(), hex);
                    let stroke = stroke.map_or(String::new(), |(c, w)| {
                        format!(r#" stroke="{}" stroke-width="{w:.2}""#, hex(c))
                    });
                    body += &format!(
                        r#"<rect x="{x:.2}" y="{y:.2}" width="{:.2}" height="{:.2}" fill="{fill}"{stroke}/>"#,
                        width.max(0.0),
                        height.max(0.0)
                    );
                }
                Item::Ellipse {
                    x,
                    y,
                    width,
                    height,
                    fill,
                    stroke,
                } => {
                    let fill = fill.map_or("none".to_string(), hex);
                    let stroke = stroke.map_or(String::new(), |(c, w)| {
                        format!(r#" stroke="{}" stroke-width="{w:.2}""#, hex(c))
                    });
                    body += &format!(
                        r#"<ellipse cx="{:.2}" cy="{:.2}" rx="{:.2}" ry="{:.2}" fill="{fill}"{stroke}/>"#,
                        x + width / 2.0,
                        y + height / 2.0,
                        (width / 2.0).max(0.0),
                        (height / 2.0).max(0.0)
                    );
                }
                Item::Line {
                    from,
                    to,
                    colour,
                    width,
                } => {
                    body += &format!(
                        r#"<line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{width:.2}"/>"#,
                        from.0,
                        from.1,
                        to.0,
                        to.1,
                        hex(*colour)
                    );
                }
                Item::Picture { .. } => {}
            }
        }
        let markup = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{px_w}" height="{px_h}" viewBox="0 0 {width:.2} {height:.2}"><rect width="100%" height="100%" fill="#ffffff"/>{body}</svg>"##
        );
        let mut canvas = svg::render_exact(&markup)?;
        for item in &self.items {
            if let Item::Picture {
                x,
                y,
                width,
                height,
                image,
            } = item
            {
                let (w, h) = ((width * scale).round().max(1.0), (height * scale).round().max(1.0));
                if w > 8192.0 || h > 8192.0 {
                    continue;
                }
                let scaled = imageops::resize(&**image, w as u32, h as u32, imageops::FilterType::Triangle);
                // what is clear in the picture lets the white of the page through
                let (ox, oy) = ((x * scale).round() as i64, (y * scale).round() as i64);
                imageops::overlay(&mut canvas, &scaled, ox, oy);
            }
        }
        // the page is opaque
        for p in canvas.pixels_mut() {
            *p = Rgba([p.0[0], p.0[1], p.0[2], 255]);
        }
        Ok(canvas)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    #[test]
    fn words_are_measured_roughly_and_bold_is_wider() {
        let plain = Font::default();
        let bold = Font {
            bold: true,
            ..Font::default()
        };
        assert!(text_width("iiii", &plain) < text_width("MMMM", &plain));
        assert!(text_width("word", &bold) > text_width("word", &plain));
        assert_eq!(text_width("", &plain), 0.0);
    }

    #[test]
    fn a_page_is_drawn_with_its_box_text_and_picture() {
        let mut page = Page::new(200.0, 100.0);
        assert!(page.is_empty());
        page.rect(10.0, 10.0, 60.0, 30.0, Some([255, 0, 0]), None);
        page.text(
            80.0,
            40.0,
            "Hi <&> \"there\"",
            &Font {
                size: 18.0,
                ..Font::default()
            },
        );
        page.picture(
            150.0,
            60.0,
            30.0,
            30.0,
            Arc::new(RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]))),
        );
        let img = page.render().expect("renders");
        assert_eq!(img.dimensions(), (2048, 1024));
        assert_eq!(img.get_pixel(1000, 100).0, [255, 255, 255, 255]);
        let [r, g, b, _] = img.get_pixel(400, 200).0;
        assert!(r > 240 && g < 20 && b < 20, "red box: {r} {g} {b}");
        let [r, g, b, _] = img.get_pixel(1600, 750).0;
        assert!(b > 240 && r < 20 && g < 20, "picture: {r} {g} {b}");
        let dark = (800..1600)
            .flat_map(|x| (250..420).map(move |y| (x, y)))
            .filter(|&(x, y)| img.get_pixel(x, y).0[0] < 80)
            .count();
        assert!(dark > 1000, "{dark} dark pixels of text");
    }
}
