//! The size of a cell in pixels, asked of the terminal while Far runs.
//!
//! The terminal answers the question `CSI 16 t` by typing `CSI 6 ; height ; width t` into the input, which Far would
//! take for key presses. But a plugin sees every input record before Far handles it (`ProcessConsoleInputW`), so while
//! an answer is awaited the records that make up escape sequences are taken out there and never reach Far.
//!
//! Like Far's own queries, the question is put between two questions every terminal answers (`CSI 0 c`, the device
//! attributes): when the second of those answers has come, the answer to the question between them has come too or never
//! will, and the waiting is over. The terminal answers only after it has dealt with what was written before (a large
//! Sixel picture, say), so the answers may be late; the keys of the user (anything that is not an escape sequence) pass.
//!
//! The question is asked at the start, when the layout of the panels changes (the window was resized or the font
//! zoomed), and now and then while the user is not typing. `PEEKEL_CELL` (`10x20`), when set, is used and nothing is
//! asked.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::api::{InputRecord, KEY_EVENT, WINDOW_BUFFER_SIZE_EVENT};
use crate::draw;
use crate::log;
use peekel_term::realcell;

/// What a cell is taken as when nothing is known: the default of Windows Terminal's font at 100 % scaling.
const DEFAULT: (usize, usize) = (10, 20);
/// The longest an answer is awaited.
const WAIT: Duration = Duration::from_secs(8);
/// The user must not have pressed a key for this long before a question is asked.
const QUIET: Duration = Duration::from_millis(800);
/// A question is asked again after this long, if the layout has not changed.
const EVERY: Duration = Duration::from_secs(15);

/// The size last told by the terminal, width << 16 | height; 0 while unknown.
static MEASURED: AtomicU32 = AtomicU32::new(0);
/// The real size of a cell, width << 16 | height, when it is another shape than the terminal tells (`realcell`); 0 when it is not
/// known, or is the same shape.
static REAL: AtomicU32 = AtomicU32::new(0);
/// The console was resized since the last question about the cell.
static RESIZED: AtomicBool = AtomicBool::new(false);
/// With `PEEKEL_CELL` given: the real cell has been looked for at least once (see `real` and `look_when_due`).
static LOOKED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<State> = Mutex::new(State {
    waiting_since: None,
    reader: Reader::new(),
    last_key: None,
    last_asked: None,
    layout: None,
});

struct State {
    /// A question is out and its answer is awaited.
    waiting_since: Option<Instant>,
    reader: Reader,
    /// When the user last pressed a key (an answer is not counted).
    last_key: Option<Instant>,
    last_asked: Option<Instant>,
    /// The layout of the panels at the last look: when it changes, the cell may have.
    layout: Option<[i32; 8]>,
}

/// What reading one more character of the input comes to.
#[derive(Debug, PartialEq)]
enum Feed {
    /// Not part of an answer: it goes on to Far.
    Pass,
    /// Part of an escape sequence: taken out.
    Taken,
    /// The second device-attributes answer came: the waiting is over, with the size if it was told.
    Done(Option<(usize, usize)>),
}

/// Takes escape sequences apart as their characters come: `ESC [ parameters final-character`, and the answers about
/// colours, `ESC ] 4 ; index ; rgb:rr/gg/bb` ended by `BEL` or `ESC \`.
struct Reader {
    sequence: String,
    attributes_seen: u8,
    cell: Option<(usize, usize)>,
    /// The terminal's 16 colours as told, by the index of the escape codes (not the console's).
    palette: [Option<[u8; 3]>; 16],
}

impl Reader {
    const fn new() -> Self {
        Self {
            sequence: String::new(),
            attributes_seen: 0,
            cell: None,
            palette: [None; 16],
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn feed(&mut self, ch: char) -> Feed {
        if self.sequence.is_empty() {
            if ch == '\x1b' {
                self.sequence.push(ch);
                return Feed::Taken;
            }
            return Feed::Pass;
        }
        if self.sequence.len() == 1 && ch != '[' && ch != ']' {
            // a lone Escape of the user's, not the start of an answer (it is lost; what follows is theirs)
            self.sequence.clear();
            return Feed::Pass;
        }
        self.sequence.push(ch);
        if self.sequence.starts_with("\x1b]") {
            if ch == '\x07' || self.sequence.ends_with("\x1b\\") {
                let sequence = std::mem::take(&mut self.sequence);
                if let Some((index, rgb)) = parse_palette_answer(&sequence) {
                    self.palette[index] = Some(rgb);
                }
            } else if self.sequence.len() > 64 {
                self.sequence.clear();
            }
            return Feed::Taken;
        }
        if self.sequence.len() > 2 && ('@'..='~').contains(&ch) {
            let sequence = std::mem::take(&mut self.sequence);
            if sequence.starts_with("\x1b[?") && ch == 'c' {
                self.attributes_seen += 1;
            } else if let Some(cell) = parse_answer(&sequence) {
                self.cell = Some(cell);
            }
            if self.attributes_seen >= 2 {
                return Feed::Done(self.cell);
            }
        } else if self.sequence.len() > 64 {
            // no sequence of an answer is this long: not one
            self.sequence.clear();
        }
        Feed::Taken
    }
}

/// From `PEEKEL_CELL` (`10x20`).
fn given() -> Option<(usize, usize)> {
    let v = std::env::var("PEEKEL_CELL").ok()?;
    let (w, h) = v.split_once('x')?;
    plausible(w.parse().ok()?, h.parse().ok()?)
}

/// The cell as the plugin takes it: given by hand, else what the terminal said, else 10 x 20.
pub fn current() -> (usize, usize) {
    if let Some(cell) = given() {
        return cell;
    }
    match MEASURED.load(Ordering::SeqCst) {
        0 => DEFAULT,
        packed => ((packed >> 16) as usize, (packed & 0xFFFF) as usize),
    }
}

/// From `PEEKEL_REAL_CELL` (`13x28`): the real size of a cell given by hand, where it cannot be worked out.
fn given_real() -> Option<(usize, usize)> {
    let v = std::env::var("PEEKEL_REAL_CELL").ok()?;
    let (w, h) = v.split_once('x')?;
    plausible(w.parse().ok()?, h.parse().ok()?)
}

/// The real size of a cell, when it is another shape than the one the terminal tells: a picture made for the cells of the terminal's
/// size is stretched by Windows Terminal over real cells, so for a font whose cell is not 1 : 2 it is made for the real shape and
/// drawn for the told one (`draw.rs`). None when the two are the same shape, or the real one is not known (`PEEKEL_OFF=aspect`
/// switches this off).
pub fn real() -> Option<(usize, usize)> {
    if crate::draw::off("aspect") {
        return None;
    }
    // with `PEEKEL_CELL` nothing is asked of the terminal, so no answer comes to start the look at the window: the first use does
    if given().is_some() && !LOOKED.swap(true, Ordering::SeqCst) {
        look_at_the_window();
    }
    match REAL.load(Ordering::SeqCst) {
        0 => None,
        packed => Some(((packed >> 16) as usize, (packed & 0xFFFF) as usize)),
    }
}

/// Works the real size of a cell out again (the window, the font or the scale may have changed).
fn look_at_the_window() {
    let told = current();
    let real = given_real()
        .or_else(realcell::measure)
        .filter(|&real| realcell::differs(told, real));
    let packed = real.map_or(0, |(w, h)| (w as u32) << 16 | h as u32);
    if REAL.swap(packed, Ordering::SeqCst) != packed {
        match real {
            Some((w, h)) => log::line(&format!(
                "the real cell is {w}x{h} px, not the shape of {}x{} that the terminal tells: pictures are made for it",
                told.0, told.1
            )),
            None => log::line("the real cell is the shape the terminal tells, or is not known"),
        }
    }
}

/// The colour the pictures are laid on (the panel's background), as `PEEKEL_BG` gives it by hand: `#0037DA`, `0037DA` or `0,55,218`.
/// For the one who stopped the questions to the terminal with `PEEKEL_CELL`, and so has no palette from it: the colour the terminal
/// shows Far's blue in is in the settings of the terminal (the colour schemes). None when the variable is not set or is not a colour.
pub fn given_background() -> Option<[u8; 3]> {
    parse_background(&std::env::var("PEEKEL_BG").ok()?)
}

fn parse_background(text: &str) -> Option<[u8; 3]> {
    let text = text.trim();
    if text.contains(',') {
        let mut channels = text.split(',').map(|c| c.trim().parse::<u8>().ok());
        let colour = [channels.next()??, channels.next()??, channels.next()??];
        return channels.next().is_none().then_some(colour);
    }
    let hex = text.trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// The size an answer `ESC [ 6 ; height ; width t` tells, as (width, height), if it is a whole sane one.
fn parse_answer(text: &str) -> Option<(usize, usize)> {
    let inner = text.strip_prefix("\x1b[6;")?.strip_suffix('t')?;
    let (height, width) = inner.split_once(';')?;
    plausible(width.parse().ok()?, height.parse().ok()?)
}

/// The colour an answer `ESC ] 4 ; index ; rgb:rrrr/gggg/bbbb` tells, as (index, red, green, blue); each channel has one
/// to four hex digits and only its first two count.
fn parse_palette_answer(text: &str) -> Option<(usize, [u8; 3])> {
    let inner = text.strip_prefix("\x1b]4;")?;
    let inner = inner.strip_suffix('\x07').or_else(|| inner.strip_suffix("\x1b\\"))?;
    let (index, colour) = inner.split_once(';')?;
    let index: usize = index.parse().ok().filter(|i| *i < 16)?;
    let mut channels = colour.strip_prefix("rgb:")?.split('/');
    let mut channel = || {
        let digits = channels.next()?;
        u8::from_str_radix(digits.get(..2.min(digits.len()))?, 16)
            .ok()
            .map(|v| if digits.len() == 1 { v * 17 } else { v })
    };
    let rgb = [channel()?, channel()?, channel()?];
    channels.next().is_none().then_some((index, rgb))
}

/// The terminal's palette colours, by the index of the escape codes; unknown until the terminal has answered.
static PALETTE: Mutex<[Option<[u8; 3]>; 16]> = Mutex::new([None; 16]);

/// What the terminal says the console colour `index` (0..16, Far's, which follow the console's attribute bits: bit 0 is
/// blue, bit 2 red) looks like, if it has said.
pub fn palette_colour(index: u32) -> Option<[u8; 3]> {
    let i = index & 15;
    let ansi = (i >> 2 & 1) + (i >> 1 & 1) * 2 + (i & 1) * 4 + (i & 8);
    PALETTE.lock().ok()?[ansi as usize]
}

/// The questions about the 16 colours, one after the other.
fn palette_question() -> String {
    (0..16).map(|i| format!("\x1b]4;{i};?\x1b\\")).collect()
}

/// A cell of this size is believable (a zero, or a size that would make a picture of gigabytes, is not).
fn plausible(width: usize, height: usize) -> Option<(usize, usize)> {
    ((4..=64).contains(&width) && (8..=128).contains(&height)).then_some((width, height))
}

/// Every input record, before Far has it: true when it is a part of an answer awaited, and is to be taken out.
pub fn takes(record: &InputRecord) -> bool {
    if record.event_type == WINDOW_BUFFER_SIZE_EVENT {
        // the window was resized (or the font zoomed): the cell may be another size, so it is asked about at once, not when
        // the next look at the panels' frames notices
        RESIZED.store(true, Ordering::SeqCst);
        log::line("the console was resized");
        return false;
    }
    if record.event_type != KEY_EVENT {
        return false;
    }
    let Ok(mut state) = STATE.lock() else {
        return false;
    };
    let ch = char::from_u32(u32::from(record.ch)).unwrap_or('\0');
    let key_down = record.key_down != 0;
    if std::env::var_os("PEEKEL_TRACE_INPUT").is_some() {
        log::line(&format!(
            "input: down {key_down} char {:#x} key {:#x} control {:#x}, awaited: {}",
            record.ch,
            record.virtual_key,
            record.control_keys,
            state.waiting_since.is_some()
        ));
    }
    if state.waiting_since.is_none() {
        if key_down {
            state.last_key = Some(Instant::now());
        }
        return false;
    }
    if !key_down {
        // the release of a key: taken when it is in the middle of a sequence
        return !state.reader.sequence.is_empty();
    }
    match state.reader.feed(ch) {
        Feed::Pass => {
            state.last_key = Some(Instant::now());
            false
        }
        Feed::Taken => true,
        Feed::Done(cell) => {
            state.waiting_since = None;
            let palette = state.reader.palette;
            state.reader.reset();
            drop(state);
            if palette.iter().any(Option::is_some)
                && let Ok(mut known) = PALETTE.lock()
                && *known != palette
            {
                *known = palette;
                log::line(&format!("the terminal says its colours are {palette:?}"));
            }
            match cell {
                Some((w, h)) => {
                    let packed = (w as u32) << 16 | h as u32;
                    let changed = MEASURED.swap(packed, Ordering::SeqCst) != packed;
                    look_at_the_window();
                    if changed {
                        log::line(&format!("the terminal says a cell is {w}x{h} px"));
                    }
                }
                None => log::line("the terminal answered, but not the question about the cell"),
            }
            true
        }
    }
}

/// With `PEEKEL_CELL` given: looks at the window again when the layout of the panels or the size of the console changed (a font or a
/// scale that is changed does that), and every so often, as the question about the cell is asked in the other mode. Nothing is written.
fn look_when_due(layout: [i32; 8]) {
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let now = Instant::now();
    let resized = RESIZED.swap(false, Ordering::SeqCst);
    let changed = resized || state.layout != Some(layout);
    state.layout = Some(layout);
    let due = state.last_asked.is_none_or(|at| now.duration_since(at) >= EVERY);
    if changed || due {
        state.last_asked = Some(now);
        drop(state);
        LOOKED.store(true, Ordering::SeqCst);
        look_at_the_window();
    }
}

/// Asks the question, if a layout change or the time calls for it and the user is not typing. `layout` is the frames of
/// the panels (left, top, right, bottom of both).
pub fn tick(layout: [i32; 8]) {
    if given().is_some() {
        // nothing is asked of the terminal (the answers begin with an Escape that a Far macro on Esc may take for a key), but the real
        // cell is worked out from the window as before: the answer of the terminal was never needed for that
        look_when_due(layout);
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let now = Instant::now();
    if let Some(since) = state.waiting_since {
        if now.duration_since(since) >= WAIT {
            log::line("the terminal did not finish answering the question about the cell");
            state.waiting_since = None;
            state.reader.reset();
        }
        return;
    }
    let resized = RESIZED.swap(false, Ordering::SeqCst);
    let changed = resized || state.layout != Some(layout);
    state.layout = Some(layout);
    let due = state.last_asked.is_none_or(|at| now.duration_since(at) >= EVERY);
    let quiet = state.last_key.is_none_or(|at| now.duration_since(at) >= QUIET);
    if (changed || due) && quiet {
        state.waiting_since = Some(now);
        state.last_asked = Some(now);
        state.reader.reset();
        drop(state);
        // the questions between two that are always answered
        if let Err(e) = draw::write_console(&format!("\x1b[0c\x1b[16t{}\x1b[0c", palette_question())) {
            log::line(&format!("cannot ask about the cell: {e}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(reader: &mut Reader, text: &str) -> Vec<Feed> {
        text.chars().map(|c| reader.feed(c)).collect()
    }

    const ATTRIBUTES: &str = "\x1b[?61;4;6;7;14;21;22;23;24;28;32;42c";

    #[test]
    fn the_size_is_read_between_the_two_attribute_answers() {
        let mut reader = Reader::new();
        let text = format!("{ATTRIBUTES}\x1b[6;28;14t{ATTRIBUTES}");
        let fed = feed_all(&mut reader, &text);
        assert!(fed[..fed.len() - 1].iter().all(|f| *f == Feed::Taken));
        assert_eq!(fed.last(), Some(&Feed::Done(Some((14, 28)))));
    }

    #[test]
    fn a_terminal_that_does_not_know_the_question_ends_it_with_no_size() {
        let mut reader = Reader::new();
        let fed = feed_all(&mut reader, &format!("{ATTRIBUTES}{ATTRIBUTES}"));
        assert_eq!(fed.last(), Some(&Feed::Done(None)));
    }

    #[test]
    fn the_keys_of_the_user_pass_even_in_the_middle_of_an_answer() {
        let mut reader = Reader::new();
        assert_eq!(reader.feed('a'), Feed::Pass);
        assert_eq!(reader.feed('\x1b'), Feed::Taken);
        assert_eq!(reader.feed('['), Feed::Taken);
        // a lone Escape then an ordinary key: not an answer
        let mut lone = Reader::new();
        assert_eq!(lone.feed('\x1b'), Feed::Taken);
        assert_eq!(lone.feed('x'), Feed::Pass);
        assert_eq!(lone.feed('y'), Feed::Pass);
    }

    #[test]
    fn the_colours_are_read_between_the_attribute_answers_with_either_ending() {
        let mut reader = Reader::new();
        let text = format!(
            "{ATTRIBUTES}\x1b[6;28;14t\x1b]4;4;rgb:0000/3737/dada\x1b\\\x1b]4;1;rgb:c5c5/0f0f/1f1f\x07{ATTRIBUTES}"
        );
        let fed = feed_all(&mut reader, &text);
        assert!(fed[..fed.len() - 1].iter().all(|f| *f == Feed::Taken));
        assert_eq!(fed.last(), Some(&Feed::Done(Some((14, 28)))));
        assert_eq!(reader.palette[4], Some([0x00, 0x37, 0xda]));
        assert_eq!(reader.palette[1], Some([0xc5, 0x0f, 0x1f]));
        assert_eq!(reader.palette[0], None);
    }

    #[test]
    fn a_colour_answer_that_is_not_whole_is_not_taken() {
        assert_eq!(
            parse_palette_answer("\x1b]4;4;rgb:00/37/da\x07"),
            Some((4, [0, 0x37, 0xda]))
        );
        assert_eq!(parse_palette_answer("\x1b]4;16;rgb:00/37/da\x07"), None);
        assert_eq!(parse_palette_answer("\x1b]4;4;rgb:00/37\x07"), None);
        assert_eq!(parse_palette_answer("\x1b]4;4;rgb:00/37/da/ff\x07"), None);
        assert_eq!(parse_palette_answer("\x1b]4;4;rgb:zz/37/da\x07"), None);
        assert_eq!(parse_palette_answer("\x1b]4;4;rgb:00/37/da"), None);
        assert_eq!(parse_palette_answer("\x1b]4;4;rgb://\x07"), None);
    }

    fn set_palette(colours: [Option<[u8; 3]>; 16]) {
        if let Ok(mut known) = PALETTE.lock() {
            *known = colours;
        }
    }

    #[test]
    fn the_console_colours_are_asked_for_by_the_codes_of_the_escape_sequences() {
        let mut known = [None; 16];
        for (ansi, slot) in known.iter_mut().enumerate() {
            *slot = Some([ansi as u8, 0, 0]);
        }
        set_palette(known);
        // the console's blue (1) is the escape codes' 4, its red (4) their 1, and the bright ones follow
        assert_eq!(palette_colour(1), Some([4, 0, 0]));
        assert_eq!(palette_colour(4), Some([1, 0, 0]));
        assert_eq!(palette_colour(9), Some([12, 0, 0]));
        assert_eq!(palette_colour(7), Some([7, 0, 0]));
        set_palette([None; 16]);
        assert_eq!(palette_colour(1), None);
    }

    #[test]
    fn a_colour_given_by_hand_is_read_in_hex_or_as_three_numbers() {
        assert_eq!(parse_background("#0037DA"), Some([0, 55, 218]));
        assert_eq!(parse_background("0037da"), Some([0, 55, 218]));
        assert_eq!(parse_background(" 0, 55 ,218 "), Some([0, 55, 218]));
        assert_eq!(parse_background("#0037D"), None);
        assert_eq!(parse_background("0,55"), None);
        assert_eq!(parse_background("0,55,218,1"), None);
        assert_eq!(parse_background("0,55,256"), None);
        assert_eq!(parse_background("blue"), None);
        assert_eq!(parse_background(""), None);
        assert_eq!(
            parse_background("#00é7DA"),
            None,
            "no panic on a letter that is more than one byte"
        );
    }

    /// The variables of the environment are the process's, and these tests change them: one at a time.
    static ENVIRONMENT: Mutex<()> = Mutex::new(());

    #[test]
    fn with_the_cell_given_the_real_cell_given_by_hand_is_taken_though_the_terminal_is_never_asked() {
        let _alone = ENVIRONMENT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: the tests that change the environment hold `ENVIRONMENT`, and no other test reads these variables
        unsafe {
            std::env::set_var("PEEKEL_CELL", "10x20");
            std::env::set_var("PEEKEL_REAL_CELL", "13x28");
        }
        REAL.store(0, Ordering::SeqCst);
        LOOKED.store(false, Ordering::SeqCst);
        assert_eq!(
            real(),
            Some((13, 28)),
            "the first use looks at the window, and the size given by hand wins"
        );
        // the same cell as the terminal tells: nothing to correct
        unsafe { std::env::set_var("PEEKEL_REAL_CELL", "10x20") };
        look_when_due([1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(real(), None);
        unsafe {
            std::env::remove_var("PEEKEL_CELL");
            std::env::remove_var("PEEKEL_REAL_CELL");
        }
        REAL.store(0, Ordering::SeqCst);
        LOOKED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_size_out_of_reason_is_not_taken() {
        assert_eq!(parse_answer("\x1b[6;0;0t"), None);
        assert_eq!(parse_answer("\x1b[6;20t"), None);
        assert_eq!(parse_answer("\x1b[6;2000;1000t"), None);
        assert_eq!(parse_answer("\x1b[6;20;10t"), Some((10, 20)));
        // the same for a size given by hand (`PEEKEL_CELL=0x0` divided by zero, a huge one made huge pictures)
        assert_eq!(plausible(0, 0), None);
        assert_eq!(plausible(100_000, 100_000), None);
        assert_eq!(plausible(10, 20), Some((10, 20)));
    }
}
