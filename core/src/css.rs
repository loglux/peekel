//! A book's style sheets as a cascade (`docs/internal/css-cascade.md`): the sheets of a part of an EPUB or of a page of HTML parsed
//! (`simplecss`, Apache-2.0 OR MIT), their at-rules dealt with here (`@media` for the screen kept, `@import` gathered, `@font-face`
//! kept for later), and for each element the declarations that match it, in the order of the cascade (our sheet, the book's, the
//! book's `!important`, with the element's own `style` above the book's sheets), made into its computed style: what it inherits from
//! its parent and what is its own, the lengths in points.
//!
//! A file may be made to hurt: the number of rules, the length of a selector and the work of matching are bounded; past the bound an
//! element gets what it inherits, and the book is still read.

use std::cell::Cell;
use std::collections::HashMap;

use roxmltree::Node;
use simplecss::{AttributeOperator, Element, PseudoClass, Selector};

use crate::book::{Align, SIZE};

/// The most rules of a part taken, all its sheets together.
const MAX_RULES: usize = 20_000;
/// The longest selector taken (as `simplecss` writes it), and the most combinators in one: a longer one is dropped.
const MAX_SELECTOR: usize = 512;
const MAX_COMBINATORS: usize = 8;
/// The steps of matching (a move to a parent or a sibling) one part may take; past them no rule of a book matches any more. Our 9
/// EPUB take at most 23,670 in a part (24 November 2026); 20 million took 15 s in a test on the CI's runner.
const MAX_STEPS: u64 = 1_000_000;
/// How deep `@media` and `@supports` blocks are read inside one another.
const MAX_NESTING: usize = 8;

/// Our sheet for a part that brings style sheets of its own: HTML's suggested rendering, as EPUB Reading Systems 3.3 asks, under
/// the book's rules.
pub(crate) const HTML_SHEET: &str = "
html, body { display: block; text-align: left; text-indent: 0 }
p { margin: 1em 0 }
blockquote, figure { margin: 1em 40px }
ul, ol { margin: 1em 0; padding-left: 40px }
ul ul, ul ol, ol ul, ol ol { margin: 0 }
dl { margin: 1em 0 }
dd { margin-left: 40px }
pre { margin: 1em 0; font-family: monospace }
code, kbd, samp, tt { font-family: monospace }
h1 { font-size: 2em; margin: 0.67em 0 }
h2 { font-size: 1.5em; margin: 0.83em 0 }
h3 { font-size: 1.17em; margin: 1em 0 }
h4 { margin: 1.33em 0 }
h5 { font-size: 0.83em; margin: 1.67em 0 }
h6 { font-size: 0.67em; margin: 2.33em 0 }
h1, h2, h3, h4, h5, h6, b, strong, th { font-weight: bold }
i, em, cite, dfn, var, address { font-style: italic }
small { font-size: smaller }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle }
ul ul ul, ol ul ul, ul ol ul { list-style-type: square }
hr { margin: 0.5em 0 }
big { font-size: larger }
caption, center { text-align: center }
[hidden] { display: none }
";

/// Our sheet for a part with no style sheet at all: the look such books had before their sheets were read (justified, the first line
/// indented, no space between paragraphs, headings centred), as Readium CSS gives its own look only to a book with no styles.
pub(crate) const PLAIN_SHEET: &str = "
html, body { display: block; text-align: justify; text-indent: 1.6em }
h1, h2, h3, h4, h5, h6 { text-align: center; text-indent: 0; margin: 6.9pt 0; font-weight: bold }
h1 { font-size: 1.35em }
h2 { font-size: 1.18em }
h3, h4, h5, h6 { font-size: 1.05em }
b, strong, th { font-weight: bold }
i, em, cite, dfn, var { font-style: italic }
li, dt, dd, figcaption, caption, pre, td, th, center { text-indent: 0 }
figcaption, caption, center { text-align: center }
figcaption, caption { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
ol { list-style-type: decimal }
ul, ol { margin-left: 1.5em; text-align: left }
blockquote { margin: 0.4em 2em }
pre, code, kbd, samp, tt { font-family: monospace }
[hidden] { display: none }
";

/// A sheet's text with its at-rules dealt with: the rules to parse (those of an `@media` for the screen taken out of it), the sheets
/// it imports (to be read before it), and the bodies of its `@font-face` rules.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Flat {
    pub(crate) text: String,
    pub(crate) imports: Vec<String>,
    pub(crate) fonts: Vec<String>,
}

/// `css` with its comments out (not those inside a string).
fn without_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let (mut i, mut from) = (0, 0);
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(_) if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'/' && bytes.get(i + 1) == Some(&b'*') => {
                out.push_str(&css[from..i]);
                let end = css[i + 2..].find("*/").map_or(bytes.len(), |e| i + 2 + e + 2);
                out.push(' ');
                i = end;
                from = end;
                continue;
            }
            None => {}
        }
        i += 1;
    }
    out.push_str(&css[from.min(css.len())..]);
    out
}

/// The first of `stops` at `from` or after it outside strings and parentheses, and where; `None` at the end of the text.
fn next_stop(text: &str, from: usize, stops: &[u8]) -> Option<(usize, u8)> {
    let bytes = text.as_bytes();
    let (mut quote, mut parens) = (None::<u8>, 0usize);
    let mut i = from;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(_) if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => parens += 1,
                b')' => parens = parens.saturating_sub(1),
                _ if parens == 0 && stops.contains(&b) => return Some((i, b)),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Where the block that opens at `open` (a `{`) closes: its `}`, or the end of the text for one left open.
fn block_end(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut at = open;
    while let Some((i, b)) = next_stop(text, at, b"{}") {
        if b == b'{' {
            depth += 1;
        } else {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return i;
            }
        }
        at = i + 1;
    }
    text.len()
}

/// Whether an `@media` rule's list of media (`screen`, `print`, `screen and (min-width: 30em)`, `not print`, `amzn-kf8`) is for a
/// reader's screen: `all`, `screen` or features alone; not `print`, `speech` or Kindle's own. Features are taken as met.
fn media_applies(list: &str) -> bool {
    let list = list.trim().to_ascii_lowercase();
    if list.is_empty() {
        return true;
    }
    list.split(',').any(|query| {
        let query = query.trim();
        let (negated, query) = match query.strip_prefix("not ") {
            Some(rest) => (true, rest.trim_start()),
            None => (false, query.strip_prefix("only ").unwrap_or(query).trim_start()),
        };
        let kind = if query.starts_with('(') {
            "all"
        } else {
            query.split_whitespace().next().unwrap_or("all")
        };
        let ours = matches!(kind, "all" | "screen");
        ours != negated
    })
}

/// The file an `@import` names (`url("a.css")`, `"a.css"`), when it is one of the book's (not on the network).
fn import_of(prelude: &str) -> Option<String> {
    let rest = prelude.trim();
    let named = if let Some(inside) = rest.strip_prefix("url(") {
        inside.split(')').next().unwrap_or("")
    } else {
        rest.split_whitespace().next().unwrap_or("")
    };
    let named = named.trim().trim_matches(['"', '\'']);
    (!named.is_empty() && !named.contains("://") && !named.starts_with("data:")).then(|| named.to_string())
}

/// A sheet with its at-rules dealt with (`Flat`).
pub(crate) fn flatten(css: &str) -> Flat {
    let text = without_comments(css);
    let mut out = Flat::default();
    flatten_into(&text, &mut out, 0);
    out
}

fn flatten_into(text: &str, out: &mut Flat, nesting: usize) {
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let skipped = rest.len() - rest.trim_start().len();
        i += skipped;
        if i >= text.len() {
            break;
        }
        if text[i..].starts_with('@') {
            let name: String = text[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect::<String>()
                .to_ascii_lowercase();
            let prelude_from = i + 1 + name.len();
            match next_stop(text, prelude_from, b";{") {
                Some((at, b';')) => {
                    if name == "import" && nesting == 0 {
                        out.imports.extend(import_of(&text[prelude_from..at]));
                    }
                    i = at + 1;
                }
                Some((open, _)) => {
                    let close = block_end(text, open);
                    let body = &text[open + 1..close.min(text.len())];
                    let prelude = &text[prelude_from..open];
                    match name.as_str() {
                        "media" if nesting < MAX_NESTING && media_applies(prelude) => {
                            flatten_into(body, out, nesting + 1)
                        }
                        "supports" if nesting < MAX_NESTING => flatten_into(body, out, nesting + 1),
                        "font-face" => out.fonts.push(body.trim().to_string()),
                        _ => {}
                    }
                    i = close + 1;
                }
                None => break,
            }
            continue;
        }
        // a rule: its selectors and its block
        let Some((open, b)) = next_stop(text, i, b"{}") else {
            break;
        };
        if b == b'}' {
            // a stray `}`
            i = open + 1;
            continue;
        }
        let close = block_end(text, open);
        out.text.push_str(&text[i..close.min(text.len())]);
        out.text.push_str("}\n");
        i = close + 1;
    }
}

/// How an element is laid out, as `display` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Display {
    None,
    Inline,
    Block,
    ListItem,
    Table,
}

/// The computed style of an element: what it inherits (the indent of the first line, the alignment, the size of the text, a
/// monospace font) and what is its own (`display`, its margins and padding, its background).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Computed {
    /// As a sheet (ours or the book's) sets it; `None`: the element's own kind decides.
    pub(crate) display: Option<Display>,
    /// Top, right, bottom, left, in points.
    pub(crate) margin: [f32; 4],
    pub(crate) padding: [f32; 4],
    pub(crate) indent: Option<f32>,
    pub(crate) align: Option<Align>,
    /// The size of the text, in points.
    pub(crate) size: f32,
    /// Set in a monospace font: inherited, and by the element's own declarations (a listing a class marks so).
    pub(crate) mono: bool,
    pub(crate) mono_own: bool,
    /// Set in a sans-serif font (the generic family its `font-family` ends in, or its first name is one).
    pub(crate) sans: bool,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) small_caps: bool,
    pub(crate) background: Option<[u8; 3]>,
    /// The colour of the text (inherited), when the book gives one we can read and that shows on a white page.
    pub(crate) colour: Option<[u8; 3]>,
    pub(crate) underline: bool,
    pub(crate) strike: bool,
    pub(crate) transform: Transform,
    /// Raised (`vertical-align: super`) or lowered (`sub`): the element's own, not inherited.
    pub(crate) shift: Shift,
    /// The mark of a list's items (inherited).
    pub(crate) marker: Marker,
    /// A page break before or after the element (`page-break-before: always`, `break-after: page`).
    pub(crate) break_before: bool,
    pub(crate) break_after: bool,
}

/// `text-transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

impl Transform {
    /// `text` as this transform makes it; `word_start` whether the text before it ended a word (for `Capitalize`).
    pub(crate) fn apply(self, text: &str, word_start: bool) -> String {
        match self {
            Transform::None => text.to_string(),
            Transform::Upper => text.to_uppercase(),
            Transform::Lower => text.to_lowercase(),
            Transform::Capitalize => {
                let mut out = String::with_capacity(text.len());
                let mut start = word_start;
                for c in text.chars() {
                    if start && c.is_alphabetic() {
                        out.extend(c.to_uppercase());
                    } else {
                        out.push(c);
                    }
                    start = c.is_whitespace() || (start && !c.is_alphanumeric());
                }
                out
            }
        }
    }
}

/// `vertical-align` as the core draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shift {
    Base,
    Super,
    Sub,
}

/// `list-style-type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    None,
}

impl Marker {
    /// The mark of the item numbered `n` (from 1), with the space after it.
    pub(crate) fn of(self, n: i64) -> String {
        // 1 is a, 26 z, 27 aa
        let letters = |n: i64, alphabet: &str| -> String {
            let alphabet: Vec<char> = alphabet.chars().collect();
            let count = alphabet.len() as i64;
            let mut n = n.max(1);
            let mut out = Vec::new();
            while n > 0 {
                n -= 1;
                out.push(alphabet[(n % count) as usize]);
                n /= count;
            }
            out.iter().rev().collect()
        };
        const LATIN: &str = "abcdefghijklmnopqrstuvwxyz";
        // the Greek letters CSS counts with: no final sigma
        const GREEK: &str = "αβγδεζηθικλμνξοπρστυφχψω";
        match self {
            Marker::Disc => "\u{2022} ".to_string(),
            Marker::Circle => "\u{25E6} ".to_string(),
            Marker::Square => "\u{25AA} ".to_string(),
            Marker::Decimal => format!("{n}. "),
            Marker::LowerAlpha => format!("{}. ", letters(n, LATIN)),
            Marker::UpperAlpha => format!("{}. ", letters(n, LATIN).to_uppercase()),
            Marker::LowerRoman => format!("{}. ", roman(n).to_lowercase()),
            Marker::UpperRoman => format!("{}. ", roman(n)),
            Marker::LowerGreek => format!("{}. ", letters(n, GREEK)),
            Marker::None => String::new(),
        }
    }
}

/// A number in Roman numerals (1 to 3999; others as Arabic ones).
fn roman(n: i64) -> String {
    if !(1..4000).contains(&n) {
        return n.to_string();
    }
    let mut n = n;
    let mut out = String::new();
    for (value, letters) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while n >= value {
            out.push_str(letters);
            n -= value;
        }
    }
    out
}

impl Computed {
    /// The style above the root of a part.
    pub(crate) fn root() -> Self {
        Self {
            display: None,
            margin: [0.0; 4],
            padding: [0.0; 4],
            indent: None,
            align: None,
            size: SIZE,
            mono: false,
            mono_own: false,
            sans: false,
            bold: false,
            italic: false,
            small_caps: false,
            background: None,
            colour: None,
            underline: false,
            strike: false,
            transform: Transform::None,
            shift: Shift::Base,
            marker: Marker::Disc,
            break_before: false,
            break_after: false,
        }
    }
}

/// Where a rule comes from, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Origin {
    Ours,
    Book,
}

struct Entry<'a> {
    selector: Selector<'a>,
    declarations: Vec<Declaration<'a>>,
    origin: Origin,
    specificity: [u8; 3],
    order: usize,
}

/// A declaration: the property's name, its value, and whether it is `!important`.
type Declaration<'a> = (&'a str, &'a str, bool);

/// A declaration that matches an element, with its place in the cascade (the level of its origin and importance, its rule's
/// specificity and order), its name and value, and where it comes from.
type Found<'a> = ((u8, [u8; 3], usize), &'a str, &'a str, Origin);

/// The declarations of a block (`margin: 1em; font: 12px/1.5 "PT Serif", serif !important`), parted at `;` outside strings and
/// parentheses. (`simplecss` reads a value only up to a `/`, and loses the rest of the rule after it.)
fn declarations(block: &str) -> Vec<Declaration<'_>> {
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let end = next_stop(block, from, b";").map_or(block.len(), |(at, _)| at);
        if let Some((name, value)) = block[from..end].split_once(':') {
            let name = name.trim();
            let mut value = value.trim();
            let mut important = false;
            if let Some(bang) = value.rfind('!')
                && value[bang + 1..].trim().eq_ignore_ascii_case("important")
            {
                important = true;
                value = value[..bang].trim();
            }
            if !name.is_empty() && !value.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                out.push((name, value, important));
            }
        }
        if end >= block.len() {
            return out;
        }
        from = end + 1;
    }
}

/// The rules of a flattened sheet (`flatten`): the text of each one's selectors and of its block.
fn rules_of(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((open, _)) = next_stop(text, from, b"{") {
        let close = block_end(text, open);
        out.push((&text[from..open], &text[open + 1..close.min(text.len())]));
        from = close + 1;
        if from >= text.len() {
            break;
        }
    }
    out
}

/// Whether a selector is one `simplecss` reads whole: elements, `*`, classes, ids, attributes (`[x]`, `=`, `~=`, `|=`), the
/// combinators ` `, `>`, `+`, and the pseudo-classes it knows. A pseudo-element (`::first-letter`), `~`, `:not()`, `:nth-child()` and
/// the like are not, and their rule is dropped, as a browser drops a rule it does not understand.
fn supported(selector: &str) -> bool {
    if selector.contains("::")
        || selector.contains(['~', '\\', '(', ')']) && !selector.contains(":lang(")
        || ["^=", "$=", "*="].iter().any(|op| selector.contains(op))
    {
        return false;
    }
    selector.split(':').skip(1).all(|after| {
        ["first-child", "link", "visited", "hover", "active", "focus", "lang("]
            .iter()
            .any(|known| after.to_ascii_lowercase().starts_with(known))
    })
}

/// The rules of a part, put in buckets by what the rightmost part of their selector needs (an id, a class, an element), so that an
/// element tries only those that may match it.
pub(crate) struct Cascade<'a> {
    entries: Vec<Entry<'a>>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_name: HashMap<String, Vec<usize>>,
    any: Vec<usize>,
    steps: Cell<u64>,
    /// The size of the text of the part's root (`html`), which `rem` is of.
    root: Cell<f32>,
}

/// What the rightmost part of a selector (as `simplecss` writes it: `div > p[class~='x']`) needs: an id, a class, an element, or
/// nothing it can be told by.
enum Need {
    Id(String),
    Class(String),
    Name(String),
    Any,
}

fn need(written: &str) -> Need {
    let last = written.rsplit([' ', '>', '+']).next().unwrap_or("");
    let quoted = |key: &str| -> Option<String> {
        let at = last.find(key)? + key.len();
        let rest = &last[at..];
        Some(rest.split('\'').next().unwrap_or("").to_string())
    };
    if let Some(id) = quoted("[id='") {
        return Need::Id(id);
    }
    if let Some(class) = quoted("[class~='") {
        return Need::Class(class);
    }
    let name: String = last
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    if name.is_empty() {
        Need::Any
    } else {
        Need::Name(name.to_ascii_lowercase())
    }
}

impl<'a> Cascade<'a> {
    /// The rules of `sheets` (each flattened, `flatten`), in their order.
    pub(crate) fn new(sheets: &[(Origin, &'a str)]) -> Self {
        let mut cascade = Cascade {
            entries: Vec::new(),
            by_id: HashMap::new(),
            by_class: HashMap::new(),
            by_name: HashMap::new(),
            any: Vec::new(),
            steps: Cell::new(MAX_STEPS),
            root: Cell::new(SIZE),
        };
        for (origin, text) in sheets {
            for (selectors, block) in rules_of(text) {
                let declared = declarations(block);
                if declared.is_empty() {
                    continue;
                }
                let mut from = 0;
                loop {
                    let end = next_stop(selectors, from, b",").map_or(selectors.len(), |(at, _)| at);
                    let one = selectors[from..end].trim();
                    if cascade.entries.len() >= MAX_RULES {
                        return cascade;
                    }
                    if !one.is_empty() && one.len() <= MAX_SELECTOR && supported(one) {
                        cascade.add(one, &declared, *origin);
                    }
                    if end >= selectors.len() {
                        break;
                    }
                    from = end + 1;
                }
            }
        }
        cascade
    }

    /// One selector of a rule and the rule's declarations.
    fn add(&mut self, text: &'a str, declarations: &[Declaration<'a>], origin: Origin) {
        let Some(selector) = Selector::parse(text) else {
            return;
        };
        let written = selector.to_string();
        let compounds = written.split([' ', '>', '+']).filter(|p| !p.is_empty()).count();
        // `simplecss` asserts that a selector has a part and starts with no combinator: such a one is never handed to it
        if written.is_empty() || written.starts_with([' ', '>', '+']) || compounds > MAX_COMBINATORS + 1 {
            return;
        }
        let index = self.entries.len();
        match need(&written) {
            Need::Id(id) => self.by_id.entry(id).or_default().push(index),
            Need::Class(class) => self.by_class.entry(class).or_default().push(index),
            Need::Name(name) => self.by_name.entry(name).or_default().push(index),
            Need::Any => self.any.push(index),
        }
        self.entries.push(Entry {
            specificity: selector.specificity(),
            selector,
            declarations: declarations.to_vec(),
            origin,
            order: index,
        });
    }

    /// The rules that may match `node`, by its id, its classes and its name.
    fn candidates(&self, node: Node<'_, '_>) -> Vec<usize> {
        let mut out: Vec<usize> = self.any.clone();
        if let Some(id) = node.attribute("id")
            && let Some(found) = self.by_id.get(id)
        {
            out.extend(found);
        }
        for class in node.attribute("class").unwrap_or("").split_whitespace() {
            if let Some(found) = self.by_class.get(class) {
                out.extend(found);
            }
        }
        if let Some(found) = self.by_name.get(&node.tag_name().name().to_ascii_lowercase()) {
            out.extend(found);
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The computed style of `node`, whose parent's is `parent`.
    pub(crate) fn compute(&self, node: Node<'_, '_>, parent: &Computed) -> Computed {
        // the declarations that match, with their place in the cascade: our normal ones, the book's, the element's own `style`, the
        // book's important ones, the element's own important ones
        let mut found: Vec<Found<'_>> = Vec::new();
        for index in self.candidates(node) {
            if self.steps.get() == 0 {
                break;
            }
            let entry = &self.entries[index];
            let element = Visited {
                node,
                steps: &self.steps,
            };
            if entry.selector.matches(&element) {
                for (name, value, important) in &entry.declarations {
                    let level = match (entry.origin, important) {
                        (Origin::Ours, _) => 0,
                        (Origin::Book, false) => 1,
                        (Origin::Book, true) => 3,
                    };
                    found.push(((level, entry.specificity, entry.order), name, value, entry.origin));
                }
            }
        }
        let own = node.attribute("style").unwrap_or("");
        for (name, value, important) in declarations(own) {
            let level = if important { 4 } else { 2 };
            found.push(((level, [0; 3], 0), name, value, Origin::Book));
        }
        found.sort_by_key(|(key, ..)| *key);
        let mut declared = Declared::default();
        for (_, name, value, origin) in found {
            declared.set(&name.to_ascii_lowercase(), value.trim(), origin);
        }
        // `rem` is of the root's size; the root's own `rem` of the initial size
        let is_root = node.parent_element().is_none();
        let computed = declared.compute(parent, if is_root { SIZE } else { self.root.get() });
        if is_root {
            self.root.set(computed.size);
        }
        computed
    }
}

/// An element as `simplecss` asks after it; every step to a parent or a sibling is counted against the part's bound (`MAX_STEPS`), so
/// that a selector made to try every way up a deep tree cannot take long.
#[derive(Clone, Copy)]
struct Visited<'n, 'i, 's> {
    node: Node<'n, 'i>,
    steps: &'s Cell<u64>,
}

impl Visited<'_, '_, '_> {
    fn step(&self) -> bool {
        let left = self.steps.get();
        if left == 0 {
            return false;
        }
        self.steps.set(left - 1);
        true
    }
}

/// The language an element is in, from its own `xml:lang` or `lang` or its nearest ancestor's.
fn language(node: Node<'_, '_>) -> Option<String> {
    node.ancestors().filter(Node::is_element).find_map(|n| {
        n.attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
            .or_else(|| n.attribute("lang"))
            .map(str::to_ascii_lowercase)
    })
}

impl Element for Visited<'_, '_, '_> {
    fn parent_element(&self) -> Option<Self> {
        if !self.step() {
            return None;
        }
        self.node.parent_element().map(|node| Visited { node, ..*self })
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        if !self.step() {
            return None;
        }
        self.node.prev_sibling_element().map(|node| Visited { node, ..*self })
    }

    fn has_local_name(&self, name: &str) -> bool {
        self.node.tag_name().name().eq_ignore_ascii_case(name)
    }

    fn attribute_matches(&self, local_name: &str, operator: AttributeOperator<'_>) -> bool {
        let Some(attribute) = self
            .node
            .attributes()
            .find(|a| a.name().eq_ignore_ascii_case(local_name))
        else {
            return false;
        };
        if local_name.eq_ignore_ascii_case("class") {
            // classes are parted by any white space, not by spaces alone
            let classes: Vec<&str> = attribute.value().split_whitespace().collect();
            operator.matches(&classes.join(" "))
        } else {
            operator.matches(attribute.value())
        }
    }

    fn pseudo_class_matches(&self, class: PseudoClass<'_>) -> bool {
        match class {
            PseudoClass::FirstChild => self.node.prev_sibling_element().is_none(),
            PseudoClass::Link => self.node.tag_name().name() == "a" && self.node.has_attribute("href"),
            PseudoClass::Lang(wanted) => language(self.node).is_some_and(|lang| {
                let wanted = wanted.to_ascii_lowercase();
                lang == wanted || lang.strip_prefix(&wanted).is_some_and(|rest| rest.starts_with('-'))
            }),
            _ => false,
        }
    }
}

/// The values declared for an element, the winner of each property (a shorthand sets its parts), as written.
#[derive(Default)]
struct Declared<'v> {
    display: Option<&'v str>,
    margin: [Option<&'v str>; 4],
    padding: [Option<&'v str>; 4],
    indent: Option<&'v str>,
    align: Option<&'v str>,
    size: Option<&'v str>,
    family: Option<(&'v str, Origin)>,
    weight: Option<&'v str>,
    style: Option<&'v str>,
    variant: Option<&'v str>,
    background: Option<&'v str>,
    colour: Option<&'v str>,
    decoration: Option<&'v str>,
    transform: Option<&'v str>,
    shift: Option<&'v str>,
    marker: Option<&'v str>,
    break_before: Option<&'v str>,
    break_after: Option<&'v str>,
}

/// The four sides of a shorthand (`1em`, `1em 2em`, `1em 2em 3em`, `1em 2em 3em 4em`): top, right, bottom, left.
fn sides(value: &str) -> Option<[&str; 4]> {
    let values: Vec<&str> = value.split_whitespace().collect();
    Some(match values.as_slice() {
        [all] => [*all; 4],
        [vertical, sides] => [*vertical, *sides, *vertical, *sides],
        [top, sides, bottom] => [*top, *sides, *bottom, *sides],
        [top, right, bottom, left] => [*top, *right, *bottom, *left],
        _ => return None,
    })
}

impl<'v> Declared<'v> {
    fn set(&mut self, name: &str, value: &'v str, origin: Origin) {
        let side = |name: &str| match name {
            "top" => Some(0),
            "right" => Some(1),
            "bottom" => Some(2),
            "left" => Some(3),
            _ => None,
        };
        match name {
            "display" => self.display = Some(value),
            "margin" => {
                if let Some(all) = sides(value) {
                    self.margin = all.map(Some);
                }
            }
            "padding" => {
                if let Some(all) = sides(value) {
                    self.padding = all.map(Some);
                }
            }
            "text-indent" => self.indent = Some(value),
            "text-align" => self.align = Some(value),
            "font-size" => self.size = Some(value),
            "font-family" => self.family = Some((value, origin)),
            "font-weight" => self.weight = Some(value),
            "font-style" => self.style = Some(value),
            "font-variant" | "font-variant-caps" => self.variant = Some(value),
            "font" => {
                // `italic small-caps bold 12px/1.5 "PT Mono", monospace`: the size is the first word that is one, the style, variant
                // and weight come before it (each back to normal when not given, as the shorthand sets them), the families after it
                let words: Vec<&str> = value.split_whitespace().collect();
                if let Some(at) = words
                    .iter()
                    .position(|w| font_size(w.split('/').next().unwrap_or(""), SIZE, SIZE).is_some())
                {
                    let word = words[at];
                    let size = word.split('/').next().unwrap_or("");
                    if let Some(start) = value.find(word) {
                        self.size = Some(&value[start..start + size.len()]);
                        let families = value[start + word.len()..].trim();
                        if !families.is_empty() {
                            self.family = Some((families, origin));
                        }
                        self.weight = Some("normal");
                        self.style = Some("normal");
                        self.variant = Some("normal");
                        for before in &words[..at] {
                            match before.to_ascii_lowercase().as_str() {
                                "italic" | "oblique" => self.style = Some(before),
                                "small-caps" => self.variant = Some(before),
                                "normal" => {}
                                _ => self.weight = Some(before),
                            }
                        }
                    }
                }
            }
            "background-color" | "background" => self.background = Some(value),
            "color" => self.colour = Some(value),
            "text-decoration" | "text-decoration-line" => self.decoration = Some(value),
            "text-transform" => self.transform = Some(value),
            "vertical-align" => self.shift = Some(value),
            "list-style-type" | "list-style" => self.marker = Some(value),
            "page-break-before" | "break-before" => self.break_before = Some(value),
            "page-break-after" | "break-after" => self.break_after = Some(value),
            _ => {
                if let Some(which) = name.strip_prefix("margin-").and_then(side) {
                    self.margin[which] = Some(value);
                } else if let Some(which) = name.strip_prefix("padding-").and_then(side) {
                    self.padding[which] = Some(value);
                }
            }
        }
    }

    fn compute(&self, parent: &Computed, root: f32) -> Computed {
        let lower = |v: &str| v.trim().to_ascii_lowercase();
        let size = match self.size.map(lower).as_deref() {
            Some("inherit") | None => parent.size,
            Some("initial") => SIZE,
            Some(value) => font_size(value, parent.size, root).unwrap_or(parent.size),
        };
        let box_side = |declared: Option<&str>, inherited: f32, negative: bool| -> f32 {
            match declared.map(lower).as_deref() {
                None | Some("initial" | "auto") => 0.0,
                Some("inherit") => inherited,
                Some(value) => length(value, size, root)
                    .filter(|l| negative || *l >= 0.0)
                    .unwrap_or(0.0),
            }
        };
        let mut margin = [0.0; 4];
        let mut padding = [0.0; 4];
        for i in 0..4 {
            margin[i] = box_side(self.margin[i], parent.margin[i], false);
            padding[i] = box_side(self.padding[i], parent.padding[i], false);
        }
        let display = match self.display.map(lower).as_deref() {
            None | Some("initial") => None,
            Some("inherit") => parent.display,
            Some("none") => Some(Display::None),
            Some("inline" | "inline-block" | "inline-flex" | "inline-grid" | "contents" | "ruby") => {
                Some(Display::Inline)
            }
            Some("list-item") => Some(Display::ListItem),
            Some(value) if value.starts_with("table") || value == "inline-table" => Some(Display::Table),
            Some(_) => Some(Display::Block),
        };
        let indent = match self.indent.map(lower).as_deref() {
            None | Some("inherit") => parent.indent,
            Some("initial") => Some(0.0),
            Some(value) => length(value, size, root).or(parent.indent),
        };
        let align = match self.align.map(lower).as_deref() {
            None | Some("inherit") => parent.align,
            Some("left" | "start" | "initial" | "-webkit-left" | "-moz-left") => Some(Align::Left),
            Some("right" | "end" | "-webkit-right" | "-moz-right") => Some(Align::Right),
            Some("center" | "-webkit-center" | "-moz-center") => Some(Align::Center),
            Some("justify" | "justify-all") => Some(Align::Justify),
            Some(_) => parent.align,
        };
        let (mono, mono_own, sans) = match self.family {
            None => (parent.mono, false, parent.sans),
            Some((value, origin)) => {
                let value = lower(value);
                if value == "inherit" {
                    (parent.mono, false, parent.sans)
                } else {
                    let mono = monospace(&value);
                    (mono, mono && origin == Origin::Book, !mono && sans_serif(&value))
                }
            }
        };
        let bold = match self.weight.map(lower).as_deref() {
            None | Some("inherit") => parent.bold,
            Some("bold" | "bolder") => true,
            Some("normal" | "lighter" | "initial") => false,
            Some(value) => value.parse::<f32>().map_or(parent.bold, |w| w >= 600.0),
        };
        let italic = match self.style.map(lower).as_deref() {
            None | Some("inherit") => parent.italic,
            Some(value) => value == "italic" || value.starts_with("oblique"),
        };
        let small_caps = match self.variant.map(lower).as_deref() {
            None | Some("inherit") => parent.small_caps,
            Some(value) => value
                .split_whitespace()
                .any(|w| w == "small-caps" || w == "all-small-caps"),
        };
        let background = match self.background.map(lower).as_deref() {
            None => None,
            Some("inherit") => parent.background,
            Some(value) => {
                // `rgb(...)` has spaces inside it; the other forms are a word of the value
                let rgb = value
                    .find("rgb(")
                    .and_then(|at| value[at..].find(')').map(|end| &value[at..=at + end]));
                rgb.and_then(colour)
                    .or_else(|| value.split_whitespace().find_map(colour))
            }
        };
        let colour = match self.colour.map(lower).as_deref() {
            None | Some("inherit") => parent.colour,
            Some("initial" | "currentcolor") => None,
            Some(value) => text_colour(value).or(parent.colour),
        };
        // a decoration is drawn under or through the text of the element's children too (as CSS propagates it)
        let (underline, strike) = match self.decoration.map(lower).as_deref() {
            None | Some("inherit") => (parent.underline, parent.strike),
            Some(value) if value.split_whitespace().any(|w| w == "none") => (false, false),
            Some(value) => (
                parent.underline || value.contains("underline"),
                parent.strike || value.contains("line-through"),
            ),
        };
        let transform = match self.transform.map(lower).as_deref() {
            None | Some("inherit") => parent.transform,
            Some("uppercase") => Transform::Upper,
            Some("lowercase") => Transform::Lower,
            Some("capitalize") => Transform::Capitalize,
            Some(_) => Transform::None,
        };
        let shift = match self.shift.map(lower).as_deref() {
            Some("super" | "top" | "text-top") => Shift::Super,
            Some("sub" | "bottom" | "text-bottom") => Shift::Sub,
            _ => Shift::Base,
        };
        let marker = match self.marker.map(lower).as_deref() {
            None | Some("inherit") => parent.marker,
            Some(value) => value.split_whitespace().find_map(marker_of).unwrap_or(parent.marker),
        };
        let breaks = |value: Option<&str>| {
            matches!(
                value.map(lower).as_deref(),
                Some("always" | "page" | "left" | "right" | "recto" | "verso")
            )
        };
        Computed {
            display,
            margin,
            padding,
            indent,
            align,
            size,
            mono,
            mono_own,
            sans,
            bold,
            italic,
            small_caps,
            background,
            colour,
            underline,
            strike,
            transform,
            shift,
            marker,
            break_before: breaks(self.break_before),
            break_after: breaks(self.break_after),
        }
    }
}

/// The mark a `list-style-type` (or a word of `list-style`) names.
fn marker_of(word: &str) -> Option<Marker> {
    Some(match word {
        "disc" => Marker::Disc,
        "circle" => Marker::Circle,
        "square" => Marker::Square,
        "decimal" | "decimal-leading-zero" => Marker::Decimal,
        "lower-alpha" | "lower-latin" => Marker::LowerAlpha,
        "upper-alpha" | "upper-latin" => Marker::UpperAlpha,
        "lower-roman" => Marker::LowerRoman,
        "upper-roman" => Marker::UpperRoman,
        "lower-greek" => Marker::LowerGreek,
        "none" => Marker::None,
        _ => return None,
    })
}

/// The colour of a text (`#333`, `rgb(0, 0, 128)`, a name of the basic ones): `None` for one we cannot read, and for one so light
/// that it would not show on the white page (a book made for a dark one).
fn text_colour(value: &str) -> Option<[u8; 3]> {
    let rgb = match value {
        "black" => [0, 0, 0],
        "gray" | "grey" => [128, 128, 128],
        "dimgray" | "dimgrey" => [105, 105, 105],
        "darkgray" | "darkgrey" => [169, 169, 169],
        "red" => [255, 0, 0],
        "darkred" | "maroon" => [128, 0, 0],
        "green" => [0, 128, 0],
        "darkgreen" => [0, 100, 0],
        "blue" => [0, 0, 255],
        "navy" | "darkblue" => [0, 0, 128],
        "purple" => [128, 0, 128],
        "teal" => [0, 128, 128],
        "olive" => [128, 128, 0],
        "brown" => [165, 42, 42],
        _ => colour(value)?,
    };
    let light = 0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2]);
    (light < 200.0).then_some(rgb)
}

/// A length in points, `size` the size of the element's text (for `em`) and `root` the root's (for `rem`): `2em`, `1.5rem`, `12pt`,
/// `16px`, `1cm`, `5mm`, `0.5in`, `0`; `None` for one that is not (`auto`, a share of the width `%`, which is not known when the blocks
/// are read, a word).
pub(crate) fn length(value: &str, size: f32, root: f32) -> Option<f32> {
    let value = value.trim();
    if value == "0" {
        return Some(0.0);
    }
    let split = value.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))?;
    let (number, unit) = value.split_at(split);
    let number: f32 = number.parse().ok()?;
    let points = match unit.to_ascii_lowercase().as_str() {
        "em" => number * size,
        "rem" => number * root,
        "ex" | "ch" => number * size * 0.5,
        "pt" => number,
        "px" => number * 0.75,
        "pc" => number * 12.0,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        _ => return None,
    };
    // a length no page could hold is not one
    (points.is_finite() && points.abs() <= 2000.0).then_some(points)
}

/// The size of the text a `font-size` gives, in points, `parent` the size it is in: a keyword (`small`, `larger`), a share of the
/// parent's (`1.2em`, `120%`), or an absolute size taken against 12 points as the book's base, so that the reader's size of the text
/// still scales it. Kept between half and three times the book's size.
pub(crate) fn font_size(value: &str, parent: f32, root: f32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    let points = match value.as_str() {
        "xx-small" => SIZE * 0.6,
        "x-small" => SIZE * 0.75,
        "small" => SIZE * 0.89,
        "medium" => SIZE,
        "large" => SIZE * 1.2,
        "x-large" => SIZE * 1.5,
        "xx-large" => SIZE * 2.0,
        "xxx-large" => SIZE * 3.0,
        "larger" => parent * 1.2,
        "smaller" => parent / 1.2,
        _ => {
            if let Some(share) = value.strip_suffix('%') {
                parent * share.trim().parse::<f32>().ok()? / 100.0
            } else if value.ends_with("rem") || value.ends_with("em") || value.ends_with("ex") || value.ends_with("ch")
            {
                length(&value, parent, root)?
            } else {
                length(&value, parent, root)? * SIZE / 12.0
            }
        }
    };
    (points.is_finite() && points > 0.0).then(|| points.clamp(SIZE * 0.5, SIZE * 3.0))
}

/// The kind of face a font family is, as far as we can tell it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Serif,
    Sans,
    Mono,
}

/// What one name of a `font-family` list (lower case, without its quotes) is: a generic family, or a face known by a word of its
/// name (a word that ends in `mono`, `Courier`, `Consolas`; `Sans`, `Arial`, `Verdana`; `Serif`, `Times`, `Georgia`); `None` for
/// one we cannot tell (`Monomakh Unicode`, a serif face of a book of Wikisource, only begins with "mono").
fn kind_of(name: &str) -> Option<Kind> {
    match name {
        "monospace" => return Some(Kind::Mono),
        "sans-serif" | "system-ui" => return Some(Kind::Sans),
        "serif" | "cursive" | "fantasy" => return Some(Kind::Serif),
        _ => {}
    }
    let words: Vec<&str> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words
        .iter()
        .any(|w| w.ends_with("mono") || w.starts_with("courier") || *w == "consolas")
    {
        return Some(Kind::Mono);
    }
    let sans = [
        "sans",
        "arial",
        "helvetica",
        "verdana",
        "tahoma",
        "calibri",
        "segoe",
        "roboto",
        "trebuchet",
        "ubuntu",
        "candara",
        "frutiger",
        "myriad",
        "futura",
        "geneva",
    ];
    if words.iter().any(|w| sans.contains(w)) {
        return Some(Kind::Sans);
    }
    let serif = [
        "serif",
        "times",
        "georgia",
        "cambria",
        "garamond",
        "palatino",
        "baskerville",
        "bookman",
        "charter",
        "minion",
    ];
    words
        .iter()
        .any(|w| serif.contains(w) || w.ends_with("serif"))
        .then_some(Kind::Serif)
}

/// The kind of face a `font-family` list (lower case) asks for: that of its first name we can tell (a reader uses the first face it
/// has, and a generic family is always had), serif when none.
fn kind(value: &str) -> Kind {
    value
        .split(',')
        .map(|n| n.trim().trim_matches(['"', '\'']).trim())
        .find_map(kind_of)
        .unwrap_or(Kind::Serif)
}

/// Whether a `font-family` list (lower case) asks for a monospace face (`kind`): `"PT Mono", monospace`, `Consolas`; not
/// `Georgia, Courier`, nor a list that names a serif face before a monospace one.
pub(crate) fn monospace(value: &str) -> bool {
    kind(value) == Kind::Mono
}

/// Whether a `font-family` list (lower case) asks for a sans-serif face (`kind`): `"PT Sans", sans-serif`, `Arial`; not `Cambria,
/// Times New Roman, serif, sans-serif` (the list of a book of Wikisource, whose first face is a serif one).
pub(crate) fn sans_serif(value: &str) -> bool {
    kind(value) == Kind::Sans
}

/// A colour (`#eee`, `#f4f4f4`, `rgb(244, 244, 244)`); `None` for any other, and for white, which sets nothing off on a white page.
pub(crate) fn colour(value: &str) -> Option<[u8; 3]> {
    let value = value.trim().trim_end_matches(',');
    let rgb = if let Some(hex) = value.strip_prefix('#') {
        let digit = |i: usize| u8::from_str_radix(hex.get(i..=i)?, 16).ok();
        match hex.len() {
            3 => [digit(0)? * 17, digit(1)? * 17, digit(2)? * 17],
            6 => {
                let pair = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
                [pair(0)?, pair(2)?, pair(4)?]
            }
            _ => return None,
        }
    } else {
        let inside = value.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')'))?;
        let parts: Vec<u8> = inside.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        match parts.as_slice() {
            [r, g, b] => [*r, *g, *b],
            _ => return None,
        }
    };
    (rgb != [255, 255, 255]).then_some(rgb)
}

#[cfg(test)]
#[allow(clippy::expect_used)] // a test that cannot make its fixture should stop at once
mod tests {
    use super::*;

    fn walk(cascade: &Cascade<'_>, node: Node<'_, '_>, parent: &Computed, out: &mut HashMap<String, Computed>) {
        let computed = cascade.compute(node, parent);
        if let Some(id) = node.attribute("id") {
            out.insert(id.to_string(), computed.clone());
        }
        for child in node.children().filter(Node::is_element) {
            walk(cascade, child, &computed, out);
        }
    }

    /// The computed style of each element of `xml` that has an id, under our sheet for a styled book and `css` (the book's).
    fn styles(css: &str, xml: &str) -> HashMap<String, Computed> {
        let flat = flatten(css);
        let sources = [(Origin::Ours, HTML_SHEET), (Origin::Book, flat.text.as_str())];
        let cascade = Cascade::new(&sources);
        let document = roxmltree::Document::parse(xml).expect("XML");
        let mut out = HashMap::new();
        walk(&cascade, document.root_element(), &Computed::root(), &mut out);
        out
    }

    fn get<'m>(styles: &'m HashMap<String, Computed>, id: &str) -> &'m Computed {
        styles.get(id).expect("an element of that id")
    }

    #[test]
    fn at_rules_are_dealt_with_and_comments_are_left_out() {
        let flat = flatten(
            r#"@charset "utf-8"; @import url("base.css"); @import 'more.css' screen; @import url(http://x.org/a.css);
/* p { margin: 9em } */ p { content: "/* not a comment */"; margin: 1em }
@media screen { .s { margin: 2em } } @media print { .p { margin: 3em } } @media amzn-kf8 { .k { margin: 4em } }
@media not print { .n { margin: 5em } } @media (min-width: 30em) { .f { margin: 6em } }
@supports (display: grid) { @media all { .g { margin: 7em } } }
@font-face { font-family: "PT Mono"; src: url(../fonts/mono.ttf) } @page { margin: 1cm }"#,
        );
        assert_eq!(flat.imports, ["base.css", "more.css"]);
        assert_eq!(flat.fonts.len(), 1);
        assert!(flat.fonts[0].contains("PT Mono"));
        for kept in [".s", ".n", ".f", ".g", "not a comment"] {
            assert!(flat.text.contains(kept), "{kept} in {}", flat.text);
        }
        for dropped in [".p", ".k", "9em", "@page", "@font-face", "@media"] {
            assert!(!flat.text.contains(dropped), "{dropped} in {}", flat.text);
        }
        // a sheet cut off or of nonsense makes what it can and does not stop
        for nonsense in [
            "p.x { margin: 1em /* never closed",
            "}}}{{{ .y { margin: 1em } {",
            "@media screen { .z {",
            "@",
            "",
            "\"",
            "@import",
            "p { margin: 1em } ) ( } p:not( { x }",
        ] {
            let flat = flatten(nonsense);
            let _ = Cascade::new(&[(Origin::Book, flat.text.as_str())]);
        }
    }

    #[test]
    fn a_monospace_family_is_told_by_its_generic_family_or_its_first_name() {
        // the list of a book of Wikisource: its text was set as code for "Monomakh Unicode"
        assert!(!monospace(
            "cambria, times new roman, times, freeserif, noto serif, serif, monomakh unicode, sans-serif, calibri, segoe ui"
        ));
        assert!(!monospace("monomakh unicode, serif"));
        assert!(!monospace("georgia, courier"));
        for mono in [
            "'pt mono'",
            "\"sfmono-regular\", menlo, monospace",
            "\"courier new\"",
            "dejavu sans mono",
            "consolas",
        ] {
            assert!(monospace(mono), "{mono}");
        }
    }

    #[test]
    fn the_cascade_orders_rules_by_importance_specificity_and_place() {
        let styles = styles(
            r#"#a { margin-left: 3em } .c { margin-left: 2em } p { margin-left: 1em }
p.later { margin-left: 4em } p.later { margin-left: 5em }
.imp { margin-left: 6em !important } .imp2 { margin-left: 7em !important }
div > p.child { margin-left: 8em } div p.deep { margin-left: 9em } span + p.next { margin-left: 10em }
p::first-letter { margin-right: 5em } p:not(.x) { margin-right: 6em } p:first-child { margin-top: 2pt }"#,
            r#"<html><body><div><p id="a" class="c">x</p><p id="later" class="later">x</p>
<p id="imp" class="imp" style="margin-left: 1pt">x</p><p id="imp2" class="imp2" style="margin-left: 1pt !important">x</p>
<p id="own" class="c" style="margin-left: 11pt">x</p><p id="child" class="child">x</p><section><p id="deep" class="deep">x</p></section>
<span>s</span><p id="next" class="next">x</p></div></body></html>"#,
        );
        let left = |id: &str| get(&styles, id).margin[3];
        // an id over a class over an element, whatever their order; the later of two of the same weight
        assert_eq!(left("a"), 3.0 * SIZE);
        assert_eq!(left("later"), 5.0 * SIZE);
        // the book's important over the element's own; the element's own important over that; its own over the sheets
        assert_eq!(left("imp"), 6.0 * SIZE);
        assert_eq!(left("imp2"), 1.0);
        assert_eq!(left("own"), 11.0);
        // a child, a descendant, a next sibling
        assert_eq!(left("child"), 8.0 * SIZE);
        assert_eq!(left("deep"), 9.0 * SIZE);
        assert_eq!(left("next"), 10.0 * SIZE);
        // a pseudo-element and a selector of CSS 3 are not taken; `:first-child` is
        assert_eq!(get(&styles, "a").margin[1], 0.0);
        assert_eq!(get(&styles, "a").margin[0], 2.0);
        assert_eq!(get(&styles, "later").margin[0], SIZE);
    }

    #[test]
    fn values_are_inherited_and_resolved() {
        let styles = styles(
            r#"body { text-indent: 1.5em; text-align: justify; font-family: Georgia, serif }
.big { font-size: 2em; margin: 1em 2em 3em } .half { font-size: 50% } .abs { font-size: 24pt } .huge { font-size: 90em }
.in { margin-left: inherit } .center { text-align: center } .none { display: none } .blockish { display: block }
.code { font: 10pt/1.2 "Courier New", monospace } .bad { margin: -1em 10% auto 1e999em; background: #ffffff }
.shade { background: rgb(240, 240, 240) url(x.png) } .zero { text-indent: 0 } .tabbed { margin-left: 4pt }"#,
            "<html><body><div id=\"big\" class=\"big\"><p id=\"in\" class=\"in\">x</p><p id=\"plain\">x</p>\
             <p id=\"half\" class=\"half\">x</p></div><p id=\"abs\" class=\"abs\">x</p><p id=\"huge\" class=\"huge\">x</p>\
             <p id=\"center\" class=\"center\">x</p><p id=\"none\" class=\"none\">x</p><span id=\"blockish\" class=\"blockish\">x</span>\
             <p id=\"code\" class=\"code\">x</p><p id=\"bad\" class=\"bad\">x</p><p id=\"shade\" class=\"shade\">x</p>\
             <p id=\"zero\" class=\"zero\">x</p><p id=\"tabbed\" class=\"a\ttabbed\">x</p></body></html>",
        );
        // the em of a margin is the element's own size; margins are not inherited unless asked for
        let big = get(&styles, "big");
        assert_eq!(big.size, 2.0 * SIZE);
        assert_eq!(big.margin, [2.0 * SIZE, 4.0 * SIZE, 6.0 * SIZE, 4.0 * SIZE]);
        assert_eq!(get(&styles, "in").margin[3], 4.0 * SIZE);
        // `p` of our sheet has 1em above and below, of its own size, which it inherits
        assert_eq!(get(&styles, "plain").margin, [2.0 * SIZE, 0.0, 2.0 * SIZE, 0.0]);
        assert_eq!(get(&styles, "half").size, SIZE);
        // an absolute size against 12 points; kept within three times the book's
        assert_eq!(get(&styles, "abs").size, 2.0 * SIZE);
        assert_eq!(get(&styles, "huge").size, 3.0 * SIZE);
        // the indent (computed where it is set, of the body's size) and the alignment inherited
        let plain = get(&styles, "plain");
        assert_eq!(
            (plain.indent, plain.align, plain.mono),
            (Some(1.5 * SIZE), Some(Align::Justify), false)
        );
        assert_eq!(get(&styles, "center").align, Some(Align::Center));
        assert_eq!(get(&styles, "zero").indent, Some(0.0));
        // a monospace font and its size from `font`
        let code = get(&styles, "code");
        assert!(
            code.mono && code.mono_own && (code.size - 10.0 * SIZE / 12.0).abs() < 0.01,
            "{code:?}"
        );
        assert_eq!(get(&styles, "none").display, Some(Display::None));
        assert_eq!(get(&styles, "blockish").display, Some(Display::Block));
        // a negative margin, a share of the width, `auto`, a length no page holds: none; white sets nothing off
        let bad = get(&styles, "bad");
        assert_eq!((bad.margin, bad.background), ([0.0; 4], None));
        assert_eq!(get(&styles, "shade").background, Some([240, 240, 240]));
        // classes parted by a tab
        assert_eq!(get(&styles, "tabbed").margin[3], 4.0);
    }

    #[test]
    fn the_font_properties_are_computed_and_inherited() {
        let styles = styles(
            r#"body { font-family: Georgia, serif } .caption { font-family: "PT Sans", sans-serif; font-weight: 700 }
.thin { font-weight: 400 } .light { font-weight: lighter } .oblique { font-style: oblique 10deg } .upright { font-style: normal }
.sc { font-variant: small-caps } .short { font: italic small-caps bold 9pt/1.2 Verdana } .plain-font { font: 12pt serif }
.arial { font-family: Arial } .mono { font-family: monospace }"#,
            "<html><body><p id=\"caption\" class=\"caption\">x<span id=\"inner\">y</span><b id=\"b\" class=\"thin\">z</b></p>\
             <p id=\"plain\">x<b id=\"bold\">y</b><em id=\"em\">z<i id=\"upright\" class=\"upright\">w</i></em></p>\
             <p id=\"oblique\" class=\"oblique\">x</p><p id=\"sc\" class=\"sc\">x</p><p id=\"short\" class=\"short\">x</p>\
             <h2 id=\"h2\">x<span id=\"plainfont\" class=\"plain-font\">y</span></h2><p id=\"arial\" class=\"arial\">x</p>\
             <p id=\"mono\" class=\"mono\">x</p><b id=\"light\" class=\"light\">x</b></body></html>",
        );
        let look = |id: &str| {
            let c = get(&styles, id);
            (c.bold, c.italic, c.sans, c.small_caps)
        };
        // a sans caption bold by number, inherited by its span; `b` made normal by its class
        assert_eq!(look("caption"), (true, false, true, false));
        assert_eq!(look("inner"), (true, false, true, false));
        assert_eq!(look("b"), (false, false, true, false));
        // our sheet: `b` bold, `em` italic, `font-style: normal` upright inside it, a heading bold
        assert_eq!(look("plain"), (false, false, false, false));
        assert_eq!(look("bold"), (true, false, false, false));
        assert_eq!(look("em"), (false, true, false, false));
        assert_eq!(look("upright"), (false, false, false, false));
        assert_eq!(look("h2"), (true, false, false, false));
        assert_eq!(look("oblique"), (false, true, false, false));
        assert_eq!(look("sc"), (false, false, false, true));
        assert_eq!(look("light"), (false, false, false, false));
        // `font` sets them all, and back to normal those it does not name (bold in a heading no more)
        assert_eq!(look("short"), (true, true, true, true));
        assert!((get(&styles, "short").size - 9.0 * SIZE / 12.0).abs() < 0.01);
        assert_eq!(look("plainfont"), (false, false, false, false));
        // a sans face known by its name; monospace is not sans
        assert!(get(&styles, "arial").sans);
        let mono = get(&styles, "mono");
        assert!(mono.mono && !mono.sans);
        assert!(sans_serif("\"pt sans\", arial") && !sans_serif("georgia, serif") && !sans_serif("times new roman"));
        // the first face a reader would have decides: a serif one before a generic sans-serif (Wikisource), Arial before `serif`
        assert!(!sans_serif(
            "cambria, times new roman, times, freeserif, noto serif, serif, monomakh unicode, sans-serif, calibri, segoe ui"
        ));
        assert!(sans_serif("arial, serif") && !sans_serif("freeserif") && !sans_serif("monomakh unicode, serif"));
    }

    #[test]
    fn colour_decoration_transform_shift_lists_and_breaks_are_computed() {
        let styles = styles(
            r#".navy { color: navy } .pale { color: #fafafa } .hex { color: #333 } .u { text-decoration: underline }
.plain { text-decoration: none } .up { text-transform: uppercase } .sup { vertical-align: super } .roman { list-style: upper-roman inside }
.brk { page-break-before: always } .after { break-after: page } .avoid { page-break-before: avoid }"#,
            "<html><body><div id=\"navy\" class=\"navy u up\"><p id=\"inner\">x<span id=\"pale\" class=\"pale plain\">y</span></p>\
             <span id=\"hex\" class=\"hex\">z</span></div><span id=\"sup\" class=\"sup\">1</span><del id=\"del\">d</del>\
             <ol id=\"ol\"><li id=\"li\">a</li></ol><ul id=\"ul\" class=\"roman\"><li id=\"li2\">b</li></ul>\
             <h2 id=\"brk\" class=\"brk\">h</h2><p id=\"after\" class=\"after\">p</p><p id=\"avoid\" class=\"avoid\">p</p></body></html>",
        );
        let navy = get(&styles, "navy");
        assert_eq!(
            (navy.colour, navy.underline, navy.transform),
            (Some([0, 0, 128]), true, Transform::Upper)
        );
        // inherited by a child; a colour too light for a white page is not taken; `none` takes the decoration off
        let inner = get(&styles, "inner");
        assert_eq!(
            (inner.colour, inner.underline, inner.transform),
            (Some([0, 0, 128]), true, Transform::Upper)
        );
        let pale = get(&styles, "pale");
        assert_eq!((pale.colour, pale.underline), (Some([0, 0, 128]), false));
        assert_eq!(get(&styles, "hex").colour, Some([0x33, 0x33, 0x33]));
        assert_eq!(get(&styles, "sup").shift, Shift::Super);
        assert!(get(&styles, "del").strike);
        // a list's mark inherited by its items; `list-style` names it among other words
        assert_eq!(get(&styles, "li").marker, Marker::Decimal);
        assert_eq!(get(&styles, "li2").marker, Marker::UpperRoman);
        assert!(get(&styles, "brk").break_before && get(&styles, "after").break_after);
        assert!(!get(&styles, "avoid").break_before && !get(&styles, "inner").break_before);
    }

    #[test]
    fn marks_and_transforms_are_written_as_css_has_them() {
        let marks: Vec<String> = [
            (Marker::Disc, 1),
            (Marker::Decimal, 12),
            (Marker::LowerAlpha, 27),
            (Marker::UpperAlpha, 2),
            (Marker::LowerRoman, 4),
            (Marker::UpperRoman, 1994),
            (Marker::LowerGreek, 18),
            (Marker::UpperRoman, 0),
            (Marker::None, 3),
        ]
        .iter()
        .map(|(m, n)| m.of(*n))
        .collect();
        assert_eq!(
            marks,
            [
                "\u{2022} ",
                "12. ",
                "aa. ",
                "B. ",
                "iv. ",
                "MCMXCIV. ",
                "σ. ",
                "0. ",
                ""
            ]
        );
        assert_eq!(
            Transform::Capitalize.apply("the «old» man-of-war", true),
            "The «Old» Man-of-war"
        );
        assert_eq!(Transform::Capitalize.apply("tail of a word", false), "tail Of A Word");
        assert_eq!(Transform::Upper.apply("straße", true), "STRASSE");
    }

    #[test]
    fn rem_is_of_the_size_of_the_root() {
        // a book that sets its root smaller and its paragraphs in `rem` (its text was drawn 1.2 times too large when `rem` was taken
        // as the book's base size)
        let styles = styles(
            "html { font-size: 0.7rem } p { font-size: 1.2rem; text-indent: 1rem; margin-top: 2rem }",
            "<html id=\"html\"><body><p id=\"p\">x</p></body></html>",
        );
        let root = get(&styles, "html").size;
        let p = get(&styles, "p");
        assert!((root - 0.7 * SIZE).abs() < 0.01, "{root}");
        assert!((p.size - 1.2 * root).abs() < 0.01, "{}", p.size);
        assert!((p.indent.unwrap_or(0.0) - root).abs() < 0.01 && (p.margin[0] - 2.0 * root).abs() < 0.01);
    }

    #[test]
    fn a_selector_made_to_try_every_way_up_a_deep_tree_is_bounded() {
        let depth = 60;
        let xml = format!(
            "<html><body>{}<p id=\"end\">x</p>{}</body></html>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let mut css = String::new();
        // no `section` above the `p`: each rule tries every way of choosing its `div`s among the sixty before it fails
        for i in 0..2000 {
            css.push_str(&format!(
                "section div div div div div div div p {{ margin-left: {i}em }}\n"
            ));
        }
        css.push_str(&format!("{} {{ margin-left: 1em }}", "div ".repeat(200) + "p"));
        css.push_str(&".a".repeat(1000));
        css.push_str(" { margin: 1em }");
        let start = std::time::Instant::now();
        let styles = styles(&css, &xml);
        assert!(start.elapsed().as_secs() < 10, "{:?}", start.elapsed());
        assert_eq!(get(&styles, "end").margin[3], 0.0);
    }
}
