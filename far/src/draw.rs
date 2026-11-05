//! What the plugin does each time Far lets it run: look at the panels, and draw the picture in the quick view.
//!
//! Far's thread only looks, wipes and writes: the picture is read, shrunk and encoded on a thread of its own, so that
//! Far stays alive while a large photo is made (0.1 to 0.5 s). The order, for each new picture under the cursor:
//! 1. at once, the place is wiped (the text Far printed for the file would be on show otherwise);
//! 2. the worker makes the Sixel;
//! 3. the next tick after it is ready (and at least `HOLD` after the cursor moved: Far prints the file's text late,
//!    and a picture already made must not be written before that) writes it, and half a second later writes it once
//!    more, because Far may have printed the file's text over it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peekel_core::{card, decode, photo, sixel};

use crate::api::{self, PFLAGS_PLUGIN, PTYPE_QVIEWPANEL, Rect};
use crate::cache::{Cache, Key, Rendered};
use crate::log;

const PICTURES: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff"];

/// The least time between the cursor moving to a picture and the picture being written, even when it is made already:
/// written sooner, Far's own painting of the quick view comes after it and leaves the file's text around the picture.
const HOLD: Duration = Duration::from_millis(150);

/// The least wait before a picture is written, and how long the panel's cells must stay as they are for Far to count as done
/// painting (see `show_when_ready`).
const HOLD_LEAST: Duration = Duration::from_millis(80);
const QUIET: Duration = Duration::from_millis(40);

/// How long after a dialog or menu came or went the picture is written again.
const REDRAW: Duration = Duration::from_millis(100);

/// How long after a picture is written it is written once more.
const SETTLE: Duration = Duration::from_millis(500);

/// How long after a dialog or menu came the rest of the picture is wiped round it a second time (it is wiped at once too).
const HIDE: Duration = Duration::from_millis(150);

unsafe extern "system" {
    fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    fn ReadConsoleOutputW(
        handle: isize,
        buffer: *mut CharInfo,
        size: Coord,
        origin: Coord,
        region: *mut SmallRect,
    ) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

/// A cell of the console's screen: the character and the colours (the low four bits of the high byte are the background).
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CharInfo {
    ch: u16,
    attributes: u16,
}

/// A picture to show and where: the file and the other panel's frame (left, top, right, bottom).
#[derive(PartialEq, Eq, Clone, Debug)]
struct Target {
    path: PathBuf,
    rect: (i32, i32, i32, i32),
    /// The cell the picture is made for: when the terminal tells another, the picture is made again.
    cell: (usize, usize),
    /// The real cell, when it is another shape than that (see `cell::real`): the picture is made again when it changes.
    real: Option<(usize, usize)>,
    /// Whether the file is a copy Far took out of a plugin's panel (an archive): the panel's neighbours are then no files.
    from_plugin: bool,
    /// The card of the file (kind, size, EXIF) is shown in place of the picture (Ctrl+Alt+I).
    info: bool,
    /// The panel's colour as it is known now: until the terminal has said what Far's blue is, a guess, and the picture is made
    /// again when the answer comes (a picture made on the guess would have a strip of the wrong colour under it).
    background: [u8; 3],
}

impl Target {
    fn rect(&self) -> Rect {
        let (left, top, right, bottom) = self.rect;
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }
}

/// What the worker made: the Sixel for a target, and which request it answers.
struct Made {
    generation: u64,
    picture: Arc<Rendered>,
}

#[derive(Default)]
struct State {
    /// The picture the cursor is on (its place was wiped; it may not be on the screen yet).
    target: Option<Target>,
    /// Bytes of the picture as last written, and when.
    written: Option<(String, Instant)>,
    /// Whether that was written a second time.
    settled: bool,
    /// When the cursor moved to it.
    began: Option<Instant>,
    /// The window on top in Far at the last look (id and type): when it changes, a dialog or menu came or went, and Far
    /// repainted rows of the quick view, so the picture is written again.
    window: Option<(isize, i32)>,
    /// When what is left of the picture is to be wiped round a dialog or menu that came (see `hide_round_dialog`).
    hide_at: Option<Instant>,
    /// The wiping is to be done once more after `HIDE`.
    hide_again: bool,
    /// The dialog the picture was wiped round, when Far was about to paint it (so that it is done once for a dialog).
    hidden_dialog: Option<isize>,
    /// The progress on the taskbar button is on (a picture is being made and it takes long).
    progress: bool,
    /// The cells of the panel's inside at the last look while the picture waits, and when they last changed.
    quiet: Option<(Vec<CharInfo>, Instant)>,
    /// The picture that is on the screen now: it stays while the next one is made, and rows of it that Far paints over are
    /// written again (see `repair`).
    shown: Option<Shown>,
    /// A window of synchronized output is open (see `key_seen`).
    sync: Option<SyncWindow>,
    /// A new picture whose Sixel waits after its clearing (see `stage_step`).
    staged: Option<Staged>,
    /// The hint about "ClearType friendly redraw" stands under a picture in this place, and when its cells were last looked at
    /// (see `hint_after_picture`, `hint_step`).
    hint_looked: Option<Instant>,
    hint_at: Option<(Rect, usize)>,
    /// A measurement: the rows repaired at the end of a window are repaired again at these times (see `echo_step`).
    echo: Vec<(Instant, Vec<(usize, usize)>)>,
}

/// A picture on the screen: what it is made of, where it stands (row and column from 1, in cells) and the rows of the console
/// (from the picture's first row, the whole width of the panels) as they were at the last look.
struct Shown {
    picture: Arc<Rendered>,
    row: usize,
    col: usize,
    rows: usize,
    /// The width of the rows looked at (the columns from the screen's left edge to the panel's right edge).
    width: usize,
    seen: Option<Vec<CharInfo>>,
}

/// The newest request that the worker could not make a picture for (a file that does not decode): the picture on the screen is
/// wiped then.
static FAILED: AtomicU64 = AtomicU64::new(0);

static STATE: Mutex<State> = Mutex::new(State {
    sync: None,
    staged: None,
    hint_looked: None,
    hint_at: None,
    echo: Vec::new(),
    shown: None,
    quiet: None,
    target: None,
    written: None,
    settled: false,
    began: None,
    window: None,
    hide_at: None,
    hide_again: false,
    hidden_dialog: None,
    progress: false,
});
/// The worker is making the picture the cursor is on (not one for a neighbour made ahead).
static MAKING: AtomicBool = AtomicBool::new(false);
/// How long a picture has to take before the taskbar shows progress: a quick one would only make it flash.
/// `PEEKEL_PROGRESS_MS` sets it (for trying it with a quick picture).
fn progress_after() -> Duration {
    let ms = std::env::var("PEEKEL_PROGRESS_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    Duration::from_millis(ms)
}
static MADE: Mutex<Option<Made>> = Mutex::new(None);
/// The pictures made, the ones next to the cursor too (64 MB of Sixel at most).
static CACHE: Mutex<Cache> = Mutex::new(Cache::new(64 << 20));
/// Counts the requests to the worker; an answer to an older one is dropped.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Set when the panel was filled with markers and Far is to paint it over them at the next tick.
static REPAINT: AtomicBool = AtomicBool::new(false);
/// The viewer whose text was made invisible, and the colours (palette index, colour) it had: they go back when that
/// viewer closes.
static HIDDEN: Mutex<Option<Hidden>> = Mutex::new(None);

struct Hidden {
    viewer: isize,
    colours: Vec<(usize, api::FarColor)>,
}
/// The file whose card is asked for in place of its picture (Ctrl+Alt+I); the cursor moving to another file, or the quick
/// view closing, ends it (`card_wanted`, `forget_card`).
static INFO_FOR: Mutex<Option<PathBuf>> = Mutex::new(None);
static LAST_NOTE: Mutex<String> = Mutex::new(String::new());
/// The viewer of the quick view and the file it has open. For a file in an archive (or any other plugin's panel) that is
/// the copy Far took out into a temporary place: the panel's own item is no path, but this is.
static VIEWER_FILE: Mutex<Option<(isize, PathBuf)>> = Mutex::new(None);

/// Logs a line only when it differs from the last one (the plugin ticks many times a second).
fn note(text: String) {
    if let Ok(mut last) = LAST_NOTE.lock()
        && *last != text
    {
        log::line(&text);
        *last = text;
    }
}

/// Whether a part of what the plugin does is switched off, for finding which part spoils what: `PEEKEL_OFF` is a list
/// separated by commas of `wipe` (all the wiping), `draw` (no picture is written), `settle` (no second write) and
/// `hide` (the text of Far's quick view is left visible), `tick` (the plugin does nothing at its 20 ms calls), `hook`
/// (it does not look at the viewer's events), `ahead` (no picture is made ahead) and `hex` (a picture or another
/// binary file is not shown as hex in the quick view), `blank` (hex and not nothing) and `redraw` (the picture is not
/// written again after a dialog or menu).
pub(crate) fn off(part: &str) -> bool {
    std::env::var("PEEKEL_OFF").is_ok_and(|v| v.split(',').any(|p| p.trim().eq_ignore_ascii_case(part)))
}

/// Whether the start of the file has a NUL or many control characters, which text does not.
fn looks_binary(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 4096];
    let Ok(n) = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) else {
        return false;
    };
    let head = &head[..n];
    let control = head.iter().filter(|&&b| b < 9 || (13 < b && b < 32 && b != 27)).count();
    head.contains(&0) || control * 100 > n
}

/// A picture, or a binary file (which gets a card): the file is looked into once, not at each of the plugin's ticks.
fn shows_something(path: &Path) -> bool {
    static LAST: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);
    if is_picture(path) {
        return true;
    }
    let Ok(mut last) = LAST.lock() else {
        return false;
    };
    match last.as_ref() {
        Some((seen, binary)) if seen == path => *binary,
        _ => {
            let binary = looks_binary(path);
            *last = Some((path.to_path_buf(), binary));
            binary
        }
    }
}

/// A picture to draw: one that `core` reads, or one that a decoder of this Windows opens (HEIC, AVIF, JPEG XL, ... where the
/// codec is installed; `wic.rs`).
fn is_picture(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p)) || crate::wic::supports(e))
        || peekel_core::decode::is_special(path)
}

/// Whether `core` reads files with this extension by itself (the plain pictures, SVG, PDF, the Office files, RAW by its preview).
pub fn core_reads(extension: &str) -> bool {
    let extension = extension.trim_start_matches('.');
    PICTURES.iter().any(|p| extension.eq_ignore_ascii_case(p))
        || peekel_core::decode::is_special(Path::new(&format!("x.{extension}")))
}

/// What a decoder of this Windows makes of a file that `core` did not read, if the extension is one of those: `first` is why
/// `core` did not. A plain picture `core` knows is not offered to Windows (a damaged PNG is damaged for both); a RAW file with no
/// preview is.
fn read_by_windows(
    path: &Path,
    room: (usize, usize),
    first: decode::DecodeError,
) -> Result<image::RgbaImage, decode::DecodeError> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let plain = PICTURES.iter().any(|p| extension.eq_ignore_ascii_case(p));
    if plain || !crate::wic::supports(extension) {
        return Err(first);
    }
    let started = Instant::now();
    match crate::wic::decode(path, Some(room)) {
        Ok(image) => {
            log::line(&format!(
                "{}: opened by Windows' own codec in {:?}",
                path.display(),
                started.elapsed()
            ));
            Ok(image)
        }
        Err(why) => {
            if crate::wic::is_component_failure(&why) {
                crate::wic::mark_failed(extension);
            }
            Err(decode::DecodeError::Unreadable(format!("{first}; Windows: {why}")))
        }
    }
}

/// What the plugin knows about its surroundings, for the first line of the log.
pub fn describe() -> String {
    let (w, h) = cell();
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| "-".into());
    format!(
        "cell {w}x{h} px, real cell {:?}, PEEKEL_CELL={}, PEEKEL_REAL_CELL={}, PEEKEL_BG={} (read as {:?}), PEEKEL_OFF={}, WT_SESSION set: {}, TERM_PROGRAM={}",
        crate::cell::real(),
        var("PEEKEL_CELL"),
        var("PEEKEL_REAL_CELL"),
        var("PEEKEL_BG"),
        crate::cell::given_background(),
        var("PEEKEL_OFF"),
        std::env::var_os("WT_SESSION").is_some(),
        var("TERM_PROGRAM")
    )
}

/// The pixels of one console cell: `PEEKEL_CELL` (`10x20`), else what the terminal said, else 10 x 20.
fn cell() -> (usize, usize) {
    crate::cell::current()
}

/// The inside of a frame, in cells: the width and the height without the two last rows (Far's line with the file's name,
/// and above it the line with the kind of file, which Far paints again when it likes).
fn inside(rect: Rect) -> Option<(usize, usize)> {
    let cols = usize::try_from(rect.right - rect.left - 1).ok()?;
    let rows = usize::try_from(rect.bottom - rect.top - 3).ok()?;
    (cols > 0 && rows > 0).then_some((cols, rows))
}

/// The quick view is about to paint a file: when it is a picture, its text is made invisible first (the colour of the
/// text is that of the background), so that Far's bytes-as-characters are never on the screen. The palette is Far's,
/// and Far saves a changed one to the profile when it quits, so `show_text` puts the colour back whenever the viewer
/// closes or another file is opened, and when Far exits. `PEEKEL_KEEP_TEXT` switches this off.
pub fn viewer_opened(viewer: isize) {
    if off("hook") {
        return;
    }
    log::line(&format!("viewer {viewer} opened"));
    show_text();
    let Some((quick, path)) = api::viewer_file(viewer) else {
        return log::line("the viewer's file is unknown");
    };
    log::line(&format!("viewer file {} (quick view: {quick})", path.display()));
    if !quick {
        return;
    }
    let picture = is_picture(&path);
    if let Ok(mut open) = VIEWER_FILE.lock() {
        *open = Some((viewer, path.clone()));
    }
    // The bytes of a picture, a document or a program shown as text are control characters and signs of odd width:
    // Far and Windows Terminal do not agree on how wide they are, and the rows of the panels next to it slide. Hex is
    // digits and letters only.
    let mut blank = false;
    if !off("hex") && (picture || looks_binary(&path)) {
        // nothing of the file is printed (hex, with `-Off blank`, still prints some signs of odd width)
        if !off("blank") && api::set_viewer_blank(viewer) {
            blank = true;
            log::line("the quick view shows nothing of this file");
        } else if api::set_viewer_hex(viewer) {
            log::line("the quick view shows this file as hex");
        }
    }
    if std::env::var_os("PEEKEL_KEEP_TEXT").is_some() || off("hide") {
        return;
    }
    // the text of a picture (hex or text), and the arrows Far prints when it repaints the rows of a blank view
    let mut indexes = Vec::new();
    if picture {
        indexes.push(api::COL_VIEWERTEXT);
    }
    if blank {
        indexes.push(api::COL_VIEWERARROWS);
    }
    hide(viewer, &indexes, &path);
}

/// Makes the palette colours at `indexes` invisible (the foreground the colour of the background), remembering them.
fn hide(viewer: isize, indexes: &[usize], path: &Path) {
    let mut kept = Vec::new();
    for &index in indexes {
        let Some(original) = api::palette_colour(index) else {
            log::line(&format!("palette colour {index} is unknown: not hidden"));
            continue;
        };
        if api::set_palette_colour(index, &original.hidden()) {
            kept.push((index, original));
        }
    }
    if !kept.is_empty() {
        log::line(&format!(
            "{} colours of the quick view hidden for {}",
            kept.len(),
            path.display()
        ));
        if let Ok(mut hidden) = HIDDEN.lock() {
            *hidden = Some(Hidden { viewer, colours: kept });
        }
    }
}

pub fn viewer_closed(viewer: isize) {
    if off("hook") {
        return;
    }
    if let Ok(mut open) = VIEWER_FILE.lock()
        && open.as_ref().is_some_and(|(v, _)| *v == viewer)
    {
        *open = None;
    }
    if HIDDEN
        .lock()
        .is_ok_and(|h| h.as_ref().is_some_and(|hidden| hidden.viewer == viewer))
    {
        show_text();
    }
}

/// Puts the colours back, if they were hidden.
pub fn show_text() {
    let Some(hidden) = HIDDEN.lock().ok().and_then(|mut h| h.take()) else {
        return;
    };
    for (index, original) in hidden.colours {
        if !api::set_palette_colour(index, &original) {
            log::line(&format!("palette colour {index} could not be put back"));
        }
        log::line(&format!(
            "palette colour {index} put back: {:?}",
            api::palette_colour(index)
        ));
    }
}

/// Ctrl+Alt+`letter`: that virtual key with a Ctrl (left or right) and an Alt (left or right) held, and no Shift.
fn is_chord(virtual_key: u16, control_keys: u32, letter: u8) -> bool {
    const RIGHT_ALT: u32 = 0x1;
    const LEFT_ALT: u32 = 0x2;
    const RIGHT_CTRL: u32 = 0x4;
    const LEFT_CTRL: u32 = 0x8;
    const SHIFT: u32 = 0x10;
    virtual_key == u16::from(letter)
        && control_keys & (RIGHT_ALT | LEFT_ALT) != 0
        && control_keys & (RIGHT_CTRL | LEFT_CTRL) != 0
        && control_keys & SHIFT == 0
}

/// Ctrl+Alt+I: the card of the file in place of its picture.
fn is_info_key(virtual_key: u16, control_keys: u32) -> bool {
    is_chord(virtual_key, control_keys, b'I')
}

/// Ctrl+Alt+S: the Microsoft Store, for the codec that the file shown lacks.
fn is_store_key(virtual_key: u16, control_keys: u32) -> bool {
    is_chord(virtual_key, control_keys, b'S')
}

/// Every input record, before Far has it: Ctrl+Alt+S, while the quick view shows a file whose Store codec Windows lacks (its card
/// says so), opens the Store at the search for that extension. True when the record is that key and there is such a file (the
/// press and the release both); with none the key is Far's.
pub fn takes_store_key(record: &api::InputRecord) -> bool {
    if record.event_type != api::KEY_EVENT || !is_store_key(record.virtual_key, record.control_keys) {
        return false;
    }
    let Ok(state) = STATE.lock() else {
        return false;
    };
    let Some(url) = state.target.as_ref().and_then(|t| crate::formats::store_url(&t.path)) else {
        return false;
    };
    if record.key_down != 0 {
        log::line(&format!("Ctrl+Alt+S: {url}"));
        crate::formats::open_url(&url);
    }
    true
}

/// Every input record, before Far has it: Ctrl+Alt+I, while a picture is shown in the quick view, turns the card of the
/// file on and off in place of the picture. True when the record is that key (the press and the release both: Far is not
/// to see half of it).
pub fn takes_info_key(record: &api::InputRecord) -> bool {
    if record.event_type != api::KEY_EVENT || !is_info_key(record.virtual_key, record.control_keys) {
        return false;
    }
    let Ok(state) = STATE.lock() else {
        return false;
    };
    let Some(target) = state.target.as_ref() else {
        return false; // no picture in the quick view: the key is Far's
    };
    if record.key_down != 0
        && let Ok(mut asked) = INFO_FOR.lock()
    {
        *asked = if asked.as_deref() == Some(target.path.as_path()) {
            None
        } else {
            Some(target.path.clone())
        };
        log::line(&format!(
            "Ctrl+Alt+I: the card is {}",
            if asked.is_some() { "on" } else { "off" }
        ));
    }
    true
}

/// Whether the card of `path` is asked for. The card is asked for one file at a time: when the file shown is another one,
/// the question is forgotten, and coming back to the first file shows its picture.
fn card_wanted(path: &Path) -> bool {
    let Ok(mut asked) = INFO_FOR.lock() else {
        return false;
    };
    if asked.as_deref() == Some(path) {
        return true;
    }
    *asked = None;
    false
}

/// The quick view is closed: the next time it is open, a file shows its picture.
fn forget_card() {
    if let Ok(mut asked) = INFO_FOR.lock() {
        *asked = None;
    }
}

/// Far's "ClearType friendly redraw" (`Interface.ClearType`), as read once: 0 not read yet, 1 off, 2 on, 3 could not be read. With
/// it on, Far writes a row of the screen whole when any cell of it changed, and a picture in the quick view is cut by the rows
/// of the list's cursor bar (found on a capture of what Far writes, 4 November 2026).
static CLEAR_TYPE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Whether the picture is wiped round a dialog or menu that comes over the quick view. With "ClearType friendly redraw" on, Far
/// paints the dialog's rows across the whole screen and a torn picture would be left on both sides of it; with it off, Far paints
/// only the dialog's own box (seen on a capture of F10's box, 4 November 2026), so the picture stays round it and its part under
/// the dialog is written again when the dialog goes. `PEEKEL_OFF=hide` never wipes.
fn wipe_round_dialogs() -> bool {
    !off("hide") && clear_type_on() != Some(false)
}

/// Whether Far's "ClearType friendly redraw" is on; None when it could not be read (no LuaMacro). Read once, then kept.
pub fn clear_type_on() -> Option<bool> {
    match CLEAR_TYPE.load(Ordering::SeqCst) {
        0 => read_clear_type(),
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Reads Far's "ClearType friendly redraw" again (it may have been changed since) and keeps it.
pub fn read_clear_type() -> Option<bool> {
    let read = api::far_config_bool("Interface", "ClearType");
    let new = read.map_or(3, |on| if on { 2 } else { 1 });
    if CLEAR_TYPE.swap(new, Ordering::SeqCst) != new {
        log::line(&format!("Far's ClearType friendly redraw: {read:?}"));
    }
    read
}

/// How often the option is read again while the plugin runs, so that the hint goes soon after the option is turned off.
const CLEAR_TYPE_AGAIN: Duration = Duration::from_secs(10);
static CLEAR_TYPE_READ: Mutex<Option<Instant>> = Mutex::new(None);

/// How often the hint's cells are looked at, to write it again when Far has painted over it.
const HINT_LOOK: Duration = Duration::from_millis(200);

/// The hint's place: the row above the last of the quick view's inside (row, first column, width, as the screen counts from 1) and
/// its text, when there is room for it below the picture.
fn hint_place(rect: Rect, picture_end: usize) -> Option<(usize, usize, usize, &'static str)> {
    let (cols, rows) = inside(rect)?;
    let row = usize::try_from(rect.top).ok()? + rows;
    if picture_end >= row {
        return None;
    }
    let text = [crate::formats::CLEAR_TYPE_HINT, crate::formats::CLEAR_TYPE_HINT_SHORT]
        .into_iter()
        .find(|t| t.chars().count() + 2 <= cols)?;
    let width = text.chars().count();
    let col = usize::try_from(rect.left).ok()? + 2 + (cols - width) / 2;
    Some((row, col, width, text))
}

/// Writes `text` (or blanks as wide as the hint, with None) at the hint's place, in the panel's colours.
fn write_hint(row: usize, col: usize, width: usize, text: Option<&str>) {
    let sgr = api::panel_colour()
        .as_ref()
        .map_or_else(|| "\x1b[0m".to_string(), api::FarColor::sgr);
    let shown = text.map_or_else(|| " ".repeat(width), str::to_string);
    let _ = write_console(&format!("\x1b7{sgr}\x1b[{row};{col}H{shown}\x1b[0m\x1b8"));
}

/// Writes the hint about "ClearType friendly redraw" under a picture just written, while the option is on (see the README, "Turn
/// off ClearType friendly redraw"); a picture that leaves no row under it gets none.
fn hint_after_picture(state: &mut State, rect: Rect, picture_end: usize) {
    state.hint_at = None;
    if off("hint") || clear_type_on() != Some(true) {
        return;
    }
    let Some((row, col, width, text)) = hint_place(rect, picture_end) else {
        return;
    };
    state.hint_at = Some((rect, picture_end));
    state.hint_looked = Some(Instant::now());
    write_hint(row, col, width, Some(text));
}

/// While the hint is shown: it goes when the option was turned off, and it is written again when Far has painted over it (with the
/// option on, Far writes whole rows of the screen, the hint's among them).
fn hint_step(state: &mut State) {
    let Some((rect, picture_end)) = state.hint_at else {
        return;
    };
    let Some((row, col, width, text)) = hint_place(rect, picture_end) else {
        state.hint_at = None;
        return;
    };
    if clear_type_on() != Some(true) {
        state.hint_at = None;
        write_hint(row, col, width, None);
        return log::line("the hint goes: ClearType friendly redraw is off now");
    }
    if state.hint_looked.is_some_and(|at| at.elapsed() < HINT_LOOK)
        || state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS)
    {
        return;
    }
    state.hint_looked = Some(Instant::now());
    let Some(cells) = read_cells(col - 1, row - 1, width, 1) else {
        return;
    };
    let there: String = cells
        .iter()
        .map(|c| char::from_u32(u32::from(c.ch)).unwrap_or(' '))
        .collect();
    if there != text {
        write_hint(row, col, width, Some(text));
    }
}

pub fn tick() {
    // the option is read at the first tick and again every `CLEAR_TYPE_AGAIN`
    let due = CLEAR_TYPE_READ.lock().is_ok_and(|mut at| {
        let due = at.is_none_or(|t| t.elapsed() >= CLEAR_TYPE_AGAIN);
        if due {
            *at = Some(Instant::now());
        }
        due
    });
    if due {
        let _ = read_clear_type();
    }
    if off("tick") {
        return;
    }
    if REPAINT.swap(false, Ordering::SeqCst) {
        api::redraw_passive_panel();
    }
    let Some(other) = api::passive_panel() else {
        return note("no panel info".into());
    };
    if other.kind != PTYPE_QVIEWPANEL {
        note(format!("the other panel is type {}, not the quick view", other.kind));
        forget_card();
        return release();
    }
    let Some(this) = api::active_panel() else {
        return;
    };
    // In a plugin's panel (an archive) the item under the cursor is no file on the disk: Far takes the file out into a
    // temporary one and the quick view's viewer opens that, so the file is the one the viewer has.
    let from_plugin = this.flags & PFLAGS_PLUGIN != 0;
    let path = if from_plugin {
        VIEWER_FILE
            .lock()
            .ok()
            .and_then(|open| open.as_ref().map(|(_, p)| p.clone()))
            .filter(|p| p.is_file() && shows_something(p))
    } else {
        api::current_file().filter(|p| shows_something(p))
    };
    let Some(path) = path else {
        note("quick view, but the file shown is no picture and no binary file".into());
        return release();
    };
    let Rect {
        left,
        top,
        right,
        bottom,
    } = other.rect;
    let info = card_wanted(&path);
    let want = Target {
        path,
        rect: (left, top, right, bottom),
        cell: cell(),
        real: crate::cell::real(),
        from_plugin,
        info,
        // `PEEKEL_BG` by hand first (for the one who stopped the questions to the terminal), else the panel's colour as the terminal said it
        background: crate::cell::given_background()
            .unwrap_or_else(|| api::panel_colour().map_or([0, 0, 0], |c| c.background_rgb())),
    };
    crate::cell::tick([
        this.rect.left,
        this.rect.top,
        this.rect.right,
        this.rect.bottom,
        left,
        top,
        right,
        bottom,
    ]);
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.target.as_ref() != Some(&want) {
        begin(&mut state, want);
    }
    let window = api::current_window();
    if state.window != window {
        // a dialog or menu came or went: Far paints rows of the quick view again, which cuts the picture. It is written
        // again, `REDRAW` after, and only when the panels are on top (a picture written over a dialog would hide it).
        if state.window.is_some()
            && !off("redraw")
            && let Some((_, at)) = state.written.as_mut()
        {
            *at = Instant::now().checked_sub(SETTLE - REDRAW).unwrap_or_else(Instant::now);
            state.settled = false;
            log::line(&format!("the window on top went from {:?} to {window:?}", state.window));
        }
        // a dialog or menu is over the panels: what is left of the picture round it is wiped (the picture comes back
        // when the panels are on top again, by the writing above)
        // (at once: this runs on Far's thread, after Far has painted the dialog; and again `HIDE` later, in case Far
        // painted more)
        let over = window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS);
        state.hide_at = over.then(Instant::now);
        state.hide_again = over;
        if !over {
            state.hidden_dialog = None;
        }
        state.window = window;
    }
    if state.hide_at.is_some_and(|at| Instant::now() >= at) {
        state.hide_at = None;
        if wipe_round_dialogs() {
            hide_round_dialog(&state);
        } else if !off("hide") {
            patch_round_dialog(&state);
        }
        if state.hide_again {
            state.hide_again = false;
            state.hide_at = Some(Instant::now() + HIDE);
        }
    }
    show_when_ready(&mut state);
    stage_step(&mut state);
    late_painting(&mut state);
    echo_step(&mut state);
    hint_step(&mut state);
    sync_step(&mut state, false);
    repair(&mut state);
    // a picture that takes long (a big photo, a PDF) shows progress of no known length on the taskbar button; it goes when the
    // picture is made, or no longer wanted, or the cursor has left
    let waiting = !off("progress")
        && MAKING.load(Ordering::SeqCst)
        && state.target.is_some()
        && state.began.is_some_and(|at| at.elapsed() >= progress_after());
    if waiting != state.progress {
        api::set_progress(waiting);
        state.progress = waiting;
        log::line(&format!("progress on the taskbar button: {waiting}"));
    }
}

/// A dialog is about to be painted (Far tells the plugin before it paints): the rest of the picture of the quick view is wiped
/// round the dialog's box at once, so that Far paints the dialog on a blank panel and no torn picture is ever on the screen.
/// Done once for a dialog. The menus are not dialogs; the look at the screen in `hide_round_dialog` catches those.
pub fn dialog_drawn(dialog: isize) {
    if !wipe_round_dialogs() {
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.hidden_dialog == Some(dialog) {
        return;
    }
    let (Some(target), Some(_)) = (state.target.as_ref(), state.written.as_ref()) else {
        return;
    };
    let quick_view = target.rect();
    let Some(rect) = api::dialog_rect(dialog) else {
        return log::line("a dialog is about to be painted, but its place is not told");
    };
    let Some(keep) = dialog_keep(rect) else {
        return;
    };
    state.hidden_dialog = Some(dialog);
    log::line(&format!(
        "a dialog {rect:?} is about to be painted: the picture is wiped round it"
    ));
    if let Err(e) = write_console(&erase(quick_view, api::panel_colour().as_ref(), Some(keep))) {
        note(format!("cannot wipe round the dialog: {e}"));
    }
}

/// A dialog's box with its shadow (two columns to the right, a row below) as `erase` takes a place to keep: row, column,
/// rows and columns, counted from 1.
fn dialog_keep(rect: api::ScreenRect) -> Option<(usize, usize, usize, usize)> {
    let (left, top) = (usize::try_from(rect.left).ok()?, usize::try_from(rect.top).ok()?);
    let (right, bottom) = (usize::try_from(rect.right).ok()?, usize::try_from(rect.bottom).ok()?);
    (right >= left && bottom >= top).then(|| (top + 1, left + 1, bottom - top + 2, right - left + 3))
}

/// The rectangle that is not the panel's colour among the cells of the picture's place: a dialog or menu, and its shadow.
/// `backgrounds` are the background colours of the cells, `cols` to a row; the result is (row, column, rows, columns)
/// counted from 0 in that grid. A dialog that covers most of the grid is not told from the panel: `panel` is the colour
/// the panel has.
fn dialog_box(backgrounds: &[u8], cols: usize, panel: u8) -> Option<(usize, usize, usize, usize)> {
    if cols == 0 {
        return None;
    }
    let (mut top, mut left, mut bottom, mut right) = (usize::MAX, usize::MAX, 0, 0);
    for (i, &colour) in backgrounds.iter().enumerate() {
        if colour == panel {
            continue;
        }
        let (row, col) = (i / cols, i % cols);
        top = top.min(row);
        left = left.min(col);
        bottom = bottom.max(row);
        right = right.max(col);
    }
    (top != usize::MAX).then(|| (top, left, bottom - top + 1, right - left + 1))
}

/// The background colour (0..16) of each cell of a rectangle of the screen (`left` and `top` from 0), row after row, as Far has
/// it in the console's own buffer: the pictures are not in it, only what Far wrote.
fn read_backgrounds(left: usize, top: usize, cols: usize, rows: usize) -> Option<Vec<u8>> {
    let cells = read_cells(left, top, cols, rows)?;
    Some(cells.iter().map(|c| ((c.attributes >> 4) & 15) as u8).collect())
}

/// The cells (character and colours) of a rectangle of the screen, row after row, as the console's own buffer has them.
fn read_cells(left: usize, top: usize, cols: usize, rows: usize) -> Option<Vec<CharInfo>> {
    use std::os::windows::io::AsRawHandle;
    let console = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .ok()?;
    let mut cells = vec![CharInfo::default(); cols * rows];
    let (left, top) = (i16::try_from(left).ok()?, i16::try_from(top).ok()?);
    let (width, height) = (i16::try_from(cols).ok()?, i16::try_from(rows).ok()?);
    let mut region = SmallRect {
        left,
        top,
        right: left + width - 1,
        bottom: top + height - 1,
    };
    // SAFETY: a console handle, a buffer of `cols * rows` cells and the region it holds.
    let read = unsafe {
        ReadConsoleOutputW(
            console.as_raw_handle() as isize,
            cells.as_mut_ptr(),
            Coord { x: width, y: height },
            Coord::default(),
            &raw mut region,
        )
    };
    (read != 0).then_some(cells)
}

/// Whether the cells of the panel's inside have stayed as they are for `QUIET`: Far paints with the cells it changes, and the
/// console's buffer has them. Looked at each tick while a picture waits; a failed look counts as not quiet.
fn far_is_quiet(state: &mut State) -> bool {
    let Some(target) = state.target.as_ref() else {
        return false;
    };
    let rect = target.rect();
    let Some((cols, rows)) = inside(rect) else {
        return false;
    };
    let (Ok(left), Ok(top)) = (usize::try_from(rect.left + 1), usize::try_from(rect.top + 1)) else {
        return false;
    };
    let Some(now) = read_cells(left, top, cols, rows) else {
        return false;
    };
    match state.quiet.as_mut() {
        Some((before, since)) if *before == now => since.elapsed() >= QUIET,
        _ => {
            state.quiet = Some((now, Instant::now()));
            false
        }
    }
}

/// The rows of the console that the picture stands on, the whole width from the screen's left edge to the panel's right edge
/// (`PEEKEL_OFF=repair` turns the repair off).
fn look(row: usize, rows: usize, width: usize) -> Option<Vec<CharInfo>> {
    read_cells(0, row.checked_sub(1)?, width, rows)
}

/// The Sixel of the rows `runs` (first, last; counted from the picture's first row) of a picture on the screen, each at its place,
/// from the picture's own pixels (a few kilobytes, not the whole picture).
fn slices(shown: &Shown, runs: &[(usize, usize)]) -> String {
    let picture = &shown.picture;
    let per_row = picture.height / shown.rows.max(1);
    let mut out = String::from("\x1b7");
    for &(first, last) in runs {
        out += &format!("\x1b[{};{}H", shown.row + first, shown.col);
        out += &sixel::encode_indexed(
            &picture.index,
            &picture.palette,
            picture.width,
            first * per_row,
            (last - first + 1) * per_row,
            0,
        );
    }
    out + "\x1b8"
}

/// The rows of `dirty` that are set, as runs of (first, last).
fn runs_of(dirty: &[bool]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (r, _) in dirty.iter().enumerate().filter(|(_, d)| **d) {
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == r => *last = r,
            _ => runs.push((r, r)),
        }
    }
    runs
}

/// The rows that differ between two looks (`look`), `width` cells to a row.
fn rows_changed(before: &[CharInfo], now: &[CharInfo], width: usize, rows: usize) -> Vec<bool> {
    (0..rows)
        .map(|r| before[r * width..(r + 1) * width] != now[r * width..(r + 1) * width])
        .collect()
}

/// Far painted a row of the screen, and the terminal drops the part of a Sixel picture that is under the cells written to (seen
/// as a stripe of two rows across the picture, on the rows of the cursor bar of the list; the recordings of 4 November 2026).
/// ConPTY writes a whole row to the terminal when any cell of it changed (seen on a capture of what Far writes in a ConPTY), so the
/// row is cut across the whole width. Each tick the rows the picture stands on are read from the console's buffer, and the rows
/// that changed since the last look are written again from the picture's own pixels. An experiment, off unless `PEEKEL_REPAIR`
/// is set: ConPTY draws a row some milliseconds after Far wrote it, and a repair that comes sooner is cut again.
fn repair(state: &mut State) {
    if std::env::var_os("PEEKEL_REPAIR").is_none() || state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS) {
        return;
    }
    let Some(shown) = state.shown.as_mut() else {
        return;
    };
    let Some(now) = look(shown.row, shown.rows, shown.width) else {
        return;
    };
    let Some(before) = shown.seen.replace(now.clone()) else {
        return;
    };
    let runs = runs_of(&rows_changed(&before, &now, shown.width, shown.rows));
    if runs.is_empty() {
        return;
    }
    let out = slices(shown, &runs);
    match write_console(&out) {
        Ok(()) => log::line(&format!(
            "Far painted rows {runs:?} of the picture's place: written again, {} bytes",
            out.len()
        )),
        Err(e) => note(format!("cannot write: {e}")),
    }
}

/// Begins and ends the synchronized output of the terminal (DEC mode 2026, Windows Terminal 1.23 and newer): what is written
/// between the two is shown at once when the second comes, not piece by piece.
const BEGIN_SYNC: &str = "\x1b[?2026h";
const END_SYNC: &str = "\x1b[?2026l";
/// The longest a synchronized window is held open (Windows Terminal shows what it holds when a window lasts much more than 50 ms),
/// and how long the panel's rows must stay as they are, after Far began painting, for the window to end.
const SYNC_MAX: Duration = Duration::from_millis(45);
const SYNC_QUIET: Duration = Duration::from_millis(20);
const SYNC_LEAST: Duration = Duration::from_millis(25);

/// How long after the clearing of a new picture's place the picture is written (see `show_when_ready`, `stage_step`).
const STAGE: Duration = Duration::from_millis(30);

/// A new picture whose clearing is written and whose Sixel waits, inside a window of synchronized output.
struct Staged {
    at: Instant,
    generation: u64,
    picture: Arc<Rendered>,
    clearing: String,
    row: usize,
    col: usize,
    rows: usize,
    width: usize,
}

/// Writes the Sixel of a staged picture when `STAGE` has passed since its clearing, and ends the window; drops it (and ends the
/// window) when the cursor has gone to another file meanwhile.
fn stage_step(state: &mut State) {
    let Some(staged) = state.staged.as_ref() else {
        return;
    };
    let current = staged.generation == GENERATION.load(Ordering::SeqCst);
    let over_a_dialog = state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS);
    if !current || over_a_dialog {
        state.staged = None;
        let _ = write_console(END_SYNC);
        return;
    }
    if staged.at.elapsed() < STAGE {
        return;
    }
    let Some(staged) = state.staged.take() else {
        return;
    };
    let sixel = format!("\x1b7\x1b[{};{}H{}\x1b8", staged.row, staged.col, staged.picture.stream);
    match write_console(&format!("{sixel}{END_SYNC}")) {
        Ok(()) => {
            log::line(&format!(
                "{}: the picture is written, {} bytes, {:?} after the cursor moved",
                staged.generation,
                sixel.len(),
                state.began.map(|at| at.elapsed())
            ));
            state.written = Some((format!("\x1b7{}\x1b8{sixel}", staged.clearing), Instant::now()));
            state.shown = Some(Shown {
                seen: look(staged.row, staged.rows, staged.width),
                picture: staged.picture,
                row: staged.row,
                col: staged.col,
                rows: staged.rows,
                width: staged.width,
            });
        }
        Err(e) => note(format!("cannot write: {e}")),
    }
}

/// A window of synchronized output that began with a key that may move the cursor.
struct SyncWindow {
    began: Instant,
    last_change: Instant,
    /// The rows of the picture that Far painted since the window began.
    dirty: Vec<bool>,
    prev: Option<Vec<CharInfo>>,
}

/// The keys that may take the cursor to another file: PgUp, PgDn, End, Home, Left, Up, Right, Down.
fn moves_cursor(virtual_key: u16) -> bool {
    (0x21..=0x28).contains(&virtual_key)
}

/// Every input record, before Far has it: a key that may move the cursor while a picture is shown opens a window of synchronized
/// output, so that what Far and ConPTY write to the terminal for the rows of the picture (and our repair of them) is not seen piece
/// by piece, and no stripe is seen across the picture. `sync_step` ends the window. An experiment, off unless `PEEKEL_SYNC` is set.
pub fn key_seen(record: &api::InputRecord) {
    if std::env::var_os("PEEKEL_SYNC").is_none()
        || record.event_type != api::KEY_EVENT
        || record.key_down == 0
        || !moves_cursor(record.virtual_key)
    {
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.sync.is_some() || state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS) {
        return;
    }
    let Some(shown) = state.shown.as_ref() else {
        return;
    };
    let rows = shown.rows;
    // the rows as they are before Far handles the key: Far paints at once, and a look at the next tick would see them painted
    let before = look(shown.row, shown.rows, shown.width);
    if write_console(BEGIN_SYNC).is_ok() {
        let now = Instant::now();
        state.sync = Some(SyncWindow {
            began: now,
            last_change: now,
            dirty: vec![false; rows],
            prev: before,
        });
        log::line("a key that may move the cursor: the synchronized window begins");
    }
}

/// Writes the repairs of `PEEKEL_ECHO` that are due (on the picture that is shown; a new picture drops them).
fn echo_step(state: &mut State) {
    if state.echo.is_empty() {
        return;
    }
    if state.staged.is_some() || state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS) {
        state.echo.clear();
        return;
    }
    let Some(shown) = state.shown.as_ref() else {
        state.echo.clear();
        return;
    };
    let now = Instant::now();
    let (due, later): (Vec<_>, Vec<_>) = state.echo.drain(..).partition(|(at, _)| *at <= now);
    for (_, runs) in due {
        match write_console(&slices(shown, &runs)) {
            Ok(()) => log::line(&format!("echo: rows {runs:?} written again")),
            Err(e) => note(format!("cannot write: {e}")),
        }
    }
    state.echo = later;
}

/// Far painted rows of the picture's place with no key of ours before it (late: after the file's viewer had read it, or on a timer):
/// a window of synchronized output is opened at once, with those rows as dirty, so that `sync_step` writes them again when
/// ConPTY has had its say. Only in synchronized mode (`PEEKEL_SYNC`).
fn late_painting(state: &mut State) {
    if std::env::var_os("PEEKEL_SYNC").is_none()
        || state.sync.is_some()
        || state.staged.is_some()
        || state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS)
    {
        return;
    }
    let Some(shown) = state.shown.as_mut() else {
        return;
    };
    let Some(now) = look(shown.row, shown.rows, shown.width) else {
        return;
    };
    let Some(before) = shown.seen.as_ref() else {
        shown.seen = Some(now);
        return;
    };
    let changed = rows_changed(before, &now, shown.width, shown.rows);
    if !changed.iter().any(|c| *c) {
        return;
    }
    if write_console(BEGIN_SYNC).is_ok() {
        let at = Instant::now();
        log::line(&format!(
            "Far painted rows {:?} of the picture's place with no key before: the synchronized window begins",
            runs_of(&changed)
        ));
        state.sync = Some(SyncWindow {
            began: at,
            last_change: at,
            dirty: changed,
            prev: Some(now),
        });
    }
}

/// Ends the window of synchronized output: when it has been quiet (Far painted nothing in the picture's rows for `SYNC_QUIET`, and
/// at least `SYNC_LEAST` have passed), or the longest time is over, the rows that Far painted are written again and the window
/// ends, so that the terminal shows the picture whole. A new picture written meanwhile ends it too (`show_when_ready`).
fn sync_step(state: &mut State, now_or_never: bool) {
    let Some(mut sync) = state.sync.take() else {
        return;
    };
    let over_a_dialog = state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS);
    let Some(shown) = state.shown.as_mut() else {
        // the picture is gone (wiped): nothing to keep whole
        let _ = write_console(END_SYNC);
        return;
    };
    if over_a_dialog {
        let _ = write_console(END_SYNC);
        return;
    }
    if let Some(now) = look(shown.row, shown.rows, shown.width) {
        if let Some(prev) = sync.prev.as_ref() {
            let changed = rows_changed(prev, &now, shown.width, shown.rows);
            if changed.iter().any(|c| *c) {
                sync.last_change = Instant::now();
                for (d, c) in sync.dirty.iter_mut().zip(&changed) {
                    *d |= *c;
                }
            }
        }
        sync.prev = Some(now);
    }
    let elapsed = sync.began.elapsed();
    let quiet = sync.last_change.elapsed() >= SYNC_QUIET && elapsed >= SYNC_LEAST;
    if !now_or_never && !quiet && elapsed < SYNC_MAX {
        state.sync = Some(sync);
        return;
    }
    let runs = runs_of(&sync.dirty);
    // a measurement (`PEEKEL_ECHO`): the same rows are written again 100 and 250 ms later, to see on a recording whether a stripe
    // that the first repair did not take away goes then (ConPTY's rows came after the repair) or never (the repair is elsewhere)
    // (`PEEKEL_ECHO=every`: every 20 ms for 400 ms)
    if let Some(echo) = std::env::var_os("PEEKEL_ECHO")
        && !runs.is_empty()
    {
        let at = Instant::now();
        let delays: Vec<u64> = if echo == "every" {
            (1..=20).map(|i| i * 20).collect()
        } else {
            vec![100, 250]
        };
        state.echo = delays
            .into_iter()
            .map(|ms| (at + Duration::from_millis(ms), runs.clone()))
            .collect();
    }
    let Some(shown) = state.shown.as_mut() else {
        return;
    };
    let mut out = if runs.is_empty() {
        String::new()
    } else {
        slices(shown, &runs)
    };
    out += END_SYNC;
    shown.seen = sync.prev.take();
    match write_console(&out) {
        Ok(()) => log::line(&format!(
            "the synchronized window ends after {elapsed:?}: rows {runs:?} written again, {} bytes",
            out.len()
        )),
        Err(e) => note(format!("cannot write: {e}")),
    }
}

/// The Sixel of a part of a picture: `cols` x `rows` cells from the cell (`x`, `y`) of the picture (cells of `cw` x `ch` pixels).
fn crop(picture: &Rendered, x: usize, y: usize, cols: usize, rows: usize, cw: usize, ch: usize) -> String {
    let (px, py, pw, ph) = (x * cw, y * ch, cols * cw, rows * ch);
    if px + pw > picture.width || py + ph > picture.height || pw == 0 || ph == 0 {
        return String::new();
    }
    let mut index = Vec::with_capacity(pw * ph);
    for row in py..py + ph {
        index.extend_from_slice(&picture.index[row * picture.width + px..row * picture.width + px + pw]);
    }
    sixel::encode_indexed(&index, &picture.palette, pw, 0, ph, 0)
}

/// A dialog or menu is over the picture and "ClearType friendly redraw" is off: the picture stays round it, but Far paints the
/// cells of the dialog's box that its shadow leaves out (the top right and the bottom left corners) in the panel's colour, and a
/// picture under them is gone there. Those cells (the panel's colour inside the box of the dialog and its shadow, over the
/// picture) are written again from the picture's pixels.
fn patch_round_dialog(state: &State) {
    let (Some(target), Some(shown)) = (state.target.as_ref(), state.shown.as_ref()) else {
        return;
    };
    let rect = target.rect();
    let Some((cols, rows)) = inside(rect) else {
        return;
    };
    let Some(panel) = api::panel_colour().and_then(|c| u8::try_from(c.background_index()?).ok()) else {
        return;
    };
    let (Ok(left), Ok(top)) = (usize::try_from(rect.left + 1), usize::try_from(rect.top + 1)) else {
        return;
    };
    let Some(backgrounds) = read_backgrounds(left, top, cols, rows) else {
        return;
    };
    let Some((box_row, box_col, box_rows, box_cols)) = dialog_box(&backgrounds, cols, panel) else {
        return;
    };
    let (cw, ch) = cell();
    let pic_cols = shown.picture.width / cw.max(1);
    let mut out = String::new();
    for r in box_row..box_row + box_rows {
        // the screen's row and column (from 1) of the grid's cell (r, c) are top + 1 + r and left + 1 + c
        let screen_row = top + 1 + r;
        if screen_row < shown.row || screen_row >= shown.row + shown.rows {
            continue;
        }
        let mut c = box_col;
        while c < box_col + box_cols {
            if backgrounds[r * cols + c] != panel {
                c += 1;
                continue;
            }
            let start = c;
            while c < box_col + box_cols && backgrounds[r * cols + c] == panel {
                c += 1;
            }
            // the run of the panel's colour, cut to the picture's columns
            let (first, last) = (left + 1 + start, left + 1 + c);
            let (from, to) = (first.max(shown.col), last.min(shown.col + pic_cols));
            if from < to {
                out += &format!("\x1b[{screen_row};{from}H");
                out += &crop(
                    &shown.picture,
                    from - shown.col,
                    screen_row - shown.row,
                    to - from,
                    1,
                    cw,
                    ch,
                );
            }
        }
    }
    if out.is_empty() {
        return;
    }
    match write_console(&format!("\x1b7{out}\x1b8")) {
        Ok(()) => log::line(&format!(
            "the picture's cells round the dialog's shadow written again, {} bytes",
            out.len()
        )),
        Err(e) => note(format!("cannot write: {e}")),
    }
}

/// A dialog or menu is over the panels, and Far has painted its rows of the quick view, which left a torn picture (a band
/// cut out of it). The panel is wiped round the dialog's rectangle, so that the dialog stands on a blank panel and the
/// picture comes back, written again, when the panels are on top.
fn hide_round_dialog(state: &State) {
    let (Some(target), Some(_)) = (state.target.as_ref(), state.written.as_ref()) else {
        return;
    };
    let rect = target.rect();
    let Some((cols, rows)) = inside(rect) else {
        return;
    };
    let colour = api::panel_colour();
    // the panel's colour as the console has it: Far's index follows the console's attribute bits
    let Some(panel) = colour.as_ref().and_then(|c| u8::try_from(c.background_index()?).ok()) else {
        return log::line("the panel's colour is no index of the console: the picture is left as it is");
    };
    let (Ok(left), Ok(top)) = (usize::try_from(rect.left + 1), usize::try_from(rect.top + 1)) else {
        return;
    };
    let Some(backgrounds) = read_backgrounds(left, top, cols, rows) else {
        return log::line("cannot read the screen: the picture is left as it is");
    };
    let dialog = dialog_box(&backgrounds, cols, panel);
    log::line(&format!("a dialog over the picture's place: {dialog:?}"));
    // `erase` counts rows and columns from 1, and the inside of the panel starts at the second row and column
    let keep = dialog.map(|(row, col, height, width)| (top + row + 1, left + col + 1, height, width));
    if let Err(e) = write_console(&erase(rect, colour.as_ref(), keep)) {
        note(format!("cannot wipe round the dialog: {e}"));
    }
}

/// A new picture is under the cursor: wipe its place at once, and have the worker make it (unless it was made already
/// as a neighbour of the last one) and then the ones next to it.
fn begin(state: &mut State, want: Target) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    note(format!(
        "{generation}: quick view at {:?}: {}",
        want.rect,
        want.path.display()
    ));
    let Some((cols, rows)) = inside(want.rect()) else {
        return note("the panel is too small".into());
    };
    let colour = api::panel_colour();
    // The picture that is on the screen stays, in the same place, until the new one comes (the panel was blank for 150 ms and
    // more between two pictures); the rows of it that Far paints over are written again (`repair`). It is wiped at once when
    // the panel is another place, or with `PEEKEL_OFF=keep`.
    let same_place = state.target.as_ref().is_some_and(|t| t.rect == want.rect);
    if !(same_place && state.shown.is_some() && !off("keep")) {
        state.shown = None;
        if let Err(e) = write_console(&erase(want.rect(), colour.as_ref(), None)) {
            note(format!("cannot wipe: {e}"));
        }
    }
    state.quiet = None;
    let background = want.background;
    // the room in the pixels of the real cell, when that is another shape than the one the terminal tells: the terminal stretches
    // a Sixel picture over real cells
    let cell = crate::cell::real().unwrap_or_else(cell);
    // one column less than the inside: Far writes into the inside's first column when the kind of file shown changes (seen on a capture,
    // 4 November 2026), which would cut a column of a picture that begins there
    let room = (cols.saturating_sub(1) * cell.0, rows * cell.1);
    let wanted = key_of(&want.path, room, background, want.info);
    let hit = CACHE.lock().ok().and_then(|mut cache| cache.get(&wanted));
    // the files next to the cursor, nearest first: the next one is the likeliest to be wanted
    let around = if want.from_plugin {
        Vec::new()
    } else {
        api::files_around(&[1, -1, 2])
    };
    let neighbours: Vec<Key> = around
        .into_iter()
        .filter(|p| is_picture(p))
        .map(|p| key_of(&p, room, background, false))
        .collect();
    let neighbours = if off("ahead") { Vec::new() } else { neighbours };
    state.began = Some(Instant::now());
    state.target = Some(want);
    state.written = None;
    state.settled = false;
    if let Ok(mut made) = MADE.lock() {
        *made = hit.clone().map(|picture| Made { generation, picture });
    }
    if hit.is_some() {
        log::line(&format!("{generation}: already made"));
    }
    let to_make = hit.is_none().then_some(wanted);
    hand_over(Job {
        generation,
        wanted: to_make,
        neighbours,
        background,
    });
}

/// What the worker is asked to make.
struct Job {
    generation: u64,
    wanted: Option<Key>,
    neighbours: Vec<Key>,
    background: [u8; 3],
}

/// The newest job waiting for the worker; an older one that was never started is dropped (nobody wants it now).
static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// A worker thread is running. Only one at a time: a key held down over files made to hurt must not pile decodes up (each
/// may take hundreds of megabytes), and it runs on a thread with a large stack because the cards and the drawings are made
/// on it.
static WORKING: AtomicBool = AtomicBool::new(false);
const WORKER_STACK: usize = 128 * 1024 * 1024;

/// Gives `job` to the worker, and starts it if none is running.
fn hand_over(job: Job) {
    if let Ok(mut waiting) = JOB.lock() {
        *waiting = Some(job);
    }
    if WORKING.swap(true, Ordering::SeqCst) {
        return; // the running worker takes it when it is done with its own
    }
    let started = std::thread::Builder::new()
        .name("peekel-worker".to_string())
        .stack_size(WORKER_STACK)
        .spawn(|| {
            loop {
                let next = JOB.lock().ok().and_then(|mut waiting| waiting.take());
                let Some(job) = next else {
                    WORKING.store(false, Ordering::SeqCst);
                    // a job that came between the look and the store is not left behind
                    if JOB.lock().is_ok_and(|waiting| waiting.is_some()) && !WORKING.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    return;
                };
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(job.generation, job.wanted, &job.neighbours, job.background);
                }));
                if done.is_err() {
                    log::line("the worker panicked on a file; it goes on with the next");
                }
            }
        });
    if let Err(e) = started {
        WORKING.store(false, Ordering::SeqCst);
        note(format!("cannot start the worker: {e}"));
    }
}

fn key_of(path: &Path, room: (usize, usize), background: [u8; 3], info: bool) -> Key {
    let meta = std::fs::metadata(path).ok();
    Key {
        path: path.to_path_buf(),
        room,
        background,
        info,
        length: meta.as_ref().map_or(0, std::fs::Metadata::len),
        modified: meta.and_then(|m| m.modified().ok()),
    }
}

/// On the worker's thread: the picture of this request, if it is not made yet, and then the neighbours.
fn work(generation: u64, wanted: Option<Key>, neighbours: &[Key], background: [u8; 3]) {
    let current = || GENERATION.load(Ordering::SeqCst) == generation;
    if let Some(key) = wanted {
        let started = Instant::now();
        MAKING.store(true, Ordering::SeqCst);
        let made = make(&key, background, &current);
        MAKING.store(false, Ordering::SeqCst);
        match made {
            Ok(picture) if current() => {
                log::line(&format!(
                    "{generation}: made {}x{} px in {:?}",
                    picture.width,
                    picture.height,
                    started.elapsed()
                ));
                if let Ok(mut slot) = MADE.lock() {
                    *slot = Some(Made { generation, picture });
                }
            }
            Ok(_) => {}
            Err(e) if current() => {
                FAILED.store(generation, Ordering::SeqCst);
                log::line(&format!("{generation}: cannot make it: {e}"));
            }
            Err(_) => {}
        }
    }
    for key in neighbours {
        if !current() {
            return;
        }
        if CACHE.lock().is_ok_and(|cache| cache.contains(key)) {
            continue;
        }
        let started = Instant::now();
        if make(key, background, &current).is_ok() {
            log::line(&format!(
                "{generation}: made ahead in {:?}: {}",
                started.elapsed(),
                key.path.display()
            ));
        }
    }
}

/// The picture, made for the real cells, made again for the cells the terminal tells (when the real ones are another shape): the
/// terminal stretches it back over the real cells, so each side is changed by the ratio of the two sizes and what is seen has the
/// proportions of the picture, whatever the font.
fn squeeze(image: image::RgbaImage) -> image::RgbaImage {
    let Some((real_w, real_h)) = crate::cell::real() else {
        return image;
    };
    let (told_w, told_h) = cell();
    let side = |n: u32, told: usize, real: usize| ((n as usize * told + real / 2) / real).max(1) as u32;
    let (w, h) = (
        side(image.width(), told_w, real_w),
        side(image.height(), told_h, real_h),
    );
    image::imageops::resize(&image, w, h, image::imageops::FilterType::Triangle)
}

/// The picture of `key`, from the cache or made now (and kept). `current` says whether anyone still wants it.
fn make(key: &Key, background: [u8; 3], current: &dyn Fn() -> bool) -> Result<Arc<Rendered>, String> {
    if let Some(found) = CACHE.lock().ok().and_then(|mut cache| cache.get(key)) {
        return Ok(found);
    }
    // a file that is no picture, or one that cannot be read as one (a cut PNG, an encrypted PDF), gets a card
    let decoded = if is_picture(&key.path) && !key.info {
        decode::decode_for_room(&key.path, key.room.0, key.room.1)
            .or_else(|first| read_by_windows(&key.path, key.room, first))
            .map_err(|e| {
                log::line(&format!("{}: {e}: a card instead", key.path.display()));
                e
            })
    } else {
        Err(decode::DecodeError::NoRoom)
    };
    let image = match decoded {
        Ok(image) => image,
        Err(why) => {
            let note = crate::formats::codec_hint(&key.path, &why.to_string());
            card::render_with_note(&key.path, key.room.0, key.room.1, background, note.as_deref())
                .map_err(|e| e.to_string())?
        }
    };
    if !current() {
        return Err("not wanted any more".to_string());
    }
    let mut picture = decode::to_picture(&squeeze(decode::fit(image, key.room.0, key.room.1)));
    // what is clear is shown in the panel's own colour, not black
    let [br, bg, bb] = background;
    decode::composite_over(&mut picture.argb, background);
    // a whole number of cells each way, the rest in the panel's colour: no strip of a cell is left uncovered,
    // where whatever is under the picture would show
    let (cw, ch) = cell();
    let (padded_width, padded_height) = (picture.width.div_ceil(cw) * cw, picture.height.div_ceil(ch) * ch);
    if (padded_width, padded_height) != (picture.width, picture.height) {
        let back = 0xFF00_0000 | u32::from(br) << 16 | u32::from(bg) << 8 | u32::from(bb);
        let mut canvas = vec![back; padded_width * padded_height];
        for (y, row) in picture.argb.chunks(picture.width).enumerate() {
            canvas[y * padded_width..y * padded_width + picture.width].copy_from_slice(row);
        }
        picture.argb = canvas;
        picture.width = padded_width;
        picture.height = padded_height;
    }
    let indexed = photo::quantize(&picture.argb, picture.width);
    let stream = sixel::encode_indexed(&indexed.index, &indexed.palette, picture.width, 0, picture.height, 0);
    let made = Arc::new(Rendered {
        stream,
        width: picture.width,
        height: picture.height,
        index: indexed.index,
        palette: indexed.palette,
    });
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(key.clone(), Arc::clone(&made));
    }
    Ok(made)
}

/// Writes what the worker made, once for the current picture, and once more after `SETTLE`.
fn show_when_ready(state: &mut State) {
    let Some(target) = state.target.clone() else {
        return;
    };
    // Far paints the quick view some time after the cursor moved, and a picture written before that is cut by the painting.
    // `HOLD` is the longest wait. Sooner, the picture is written when Far has painted nothing in the panel for `QUIET`
    // (the cells of the console's buffer did not change) and at least `HOLD_LEAST` have passed. `PEEKEL_OFF=quiet` waits `HOLD`.
    let waited = state.began.map(|at| at.elapsed());
    let held = waited.is_some_and(|w| {
        if off("quiet") {
            w < HOLD
        } else {
            let quiet = far_is_quiet(state);
            w < HOLD_LEAST || (w < HOLD && !quiet)
        }
    });
    let ready = if held {
        None
    } else {
        MADE.lock()
            .ok()
            .and_then(|mut slot| slot.take_if(|m| m.generation == GENERATION.load(Ordering::SeqCst)))
    };
    if let Some(made) = ready.filter(|_| !off("draw")) {
        let (cw, ch) = cell();
        let Some((cols, rows)) = inside(target.rect()) else {
            return;
        };
        // centred in the frame's inside
        let pic_cols = made.picture.width.div_ceil(cw);
        let pic_rows = made.picture.height.div_ceil(ch);
        let row = usize::try_from(target.rect().top).unwrap_or(0) + 2 + rows.saturating_sub(pic_rows) / 2;
        // (centred in the inside less its first column, see `begin`)
        let col =
            usize::try_from(target.rect().left).unwrap_or(0) + 3 + cols.saturating_sub(1).saturating_sub(pic_cols) / 2;
        // a measurement (`PEEKEL_ONE`): only the first picture is written, and it stays while the cursor moves, so that what Far
        // and ConPTY do to it, and what the repairs do, can be seen with no new picture covering it
        if std::env::var_os("PEEKEL_ONE").is_some() && state.shown.is_some() {
            return;
        }
        if std::env::var_os("PEEKEL_SYNC").is_some() {
            // what changes cells goes first, and the picture a little later, inside one window of synchronized output: ConPTY
            // writes the rows whose cells changed some milliseconds after our write, and the picture written together with the
            // clearing would be cut by the rows that come after it (seen on a capture: the clearing's rows came after the Sixel)
            let clearing = erase(
                target.rect(),
                api::panel_colour().as_ref(),
                Some((row, col, pic_rows, pic_cols)),
            );
            // Windows Terminal shows what it holds when a window lasts much more than 50 ms (seen with a test of windows of 50 to
            // 1000 ms, 4 November 2026), so the key's window ends now and the new picture has a short window of its own
            if state.sync.is_some() {
                sync_step(state, true);
            }
            match write_console(&format!("{BEGIN_SYNC}\x1b7{clearing}\x1b8")) {
                Ok(()) => {
                    let width = usize::try_from(target.rect().right).unwrap_or(0) + 1;
                    state.staged = Some(Staged {
                        at: Instant::now(),
                        generation: made.generation,
                        picture: made.picture.clone(),
                        clearing,
                        row,
                        col,
                        rows: pic_rows,
                        width,
                    });
                    log::line(&format!(
                        "{}: the clearing is written, the picture follows in {STAGE:?}",
                        made.generation
                    ));
                }
                Err(e) => note(format!("cannot write: {e}")),
            }
            return;
        }
        // save the cursor; clear what is around the picture; go to its corner, draw, put the cursor back; a window of
        // synchronized output that a key opened ends with the new picture (it covers what Far painted)
        let sync_ends = if state.sync.take().is_some() { END_SYNC } else { "" };
        let bytes = format!(
            "\x1b7{}\x1b[{row};{col}H{}\x1b8{sync_ends}",
            erase(
                target.rect(),
                api::panel_colour().as_ref(),
                Some((row, col, pic_rows, pic_cols))
            ),
            made.picture.stream
        );
        let started = Instant::now();
        match write_console(&bytes) {
            Ok(()) => {
                log::line(&format!(
                    "{}: wrote {} bytes at row {row} col {col} in {:?}; the picture is {}x{} px = {pic_cols}x{pic_rows} \
                     cells (a cell taken as {cw}x{ch} px) in a place of {cols}x{rows} cells",
                    made.generation,
                    bytes.len(),
                    started.elapsed(),
                    made.picture.width,
                    made.picture.height
                ));
                log::line(&format!(
                    "{}: written {:?} after the cursor moved",
                    made.generation,
                    state.began.map(|at| at.elapsed())
                ));
                state.written = Some((bytes, Instant::now()));
                // what Far has on these rows now is the first look for `repair`
                let width = usize::try_from(target.rect().right).unwrap_or(0) + 1;
                state.shown = Some(Shown {
                    seen: look(row, pic_rows, width),
                    picture: made.picture.clone(),
                    row,
                    col,
                    rows: pic_rows,
                    width,
                });
                hint_after_picture(state, target.rect(), row + pic_rows - 1);
            }
            Err(e) => note(format!("cannot write: {e}")),
        }
    } else if state.written.is_none()
        && state.shown.is_some()
        && FAILED.load(Ordering::SeqCst) == GENERATION.load(Ordering::SeqCst)
    {
        // no picture for this file: the one on the screen must not stay for another file
        state.shown = None;
        if let Err(e) = write_console(&erase(target.rect(), api::panel_colour().as_ref(), None)) {
            note(format!("cannot wipe: {e}"));
        }
    } else if !state.settled
        && !off("settle")
        && std::env::var_os("PEEKEL_SYNC").is_none()
        && !state.window.is_some_and(|(_, kind)| kind != api::WTYPE_PANELS)
        && let Some((bytes, at)) = &state.written
        && at.elapsed() >= SETTLE
    {
        state.settled = true;
        match write_console(bytes) {
            Ok(()) => log::line("written once more (Far may have painted over it)"),
            Err(e) => note(format!("cannot write: {e}")),
        }
    }
}

/// The cursor left the pictures (another kind of file, or the quick view is gone).
///
/// A terminal drops a picture only from the cells that are written to, and Far writes only the cells it thinks have
/// changed, so a blank cell under the picture is never touched. So the inside of the panel is filled, through Far's
/// own screen buffer, with a character nobody can see (the panel's background colour on itself): to Far that is a
/// change, and it reaches the terminal. Far puts its buffer on the screen only after the plugin's call has returned,
/// so painting the panel again, which turns the markers back into what belongs there, is left to the next tick.
fn release() {
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    if state.progress {
        api::set_progress(false);
        state.progress = false;
    }
    let Some(old) = state.target.take() else {
        return;
    };
    state.written = None;
    state.shown = None;
    state.hint_at = None;
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if off("wipe") {
        return;
    }
    let Some(colour) = api::panel_colour() else {
        return log::line("Far gave no colour for the panel: nothing wiped");
    };
    let Some((cols, rows)) = inside(old.rect()) else {
        return;
    };
    log::line("the picture goes: markers in its panel, Far paints it at the next tick");
    let (left, top, _, _) = old.rect;
    for i in 0..rows {
        api::text(left + 1, top + 1 + i as i32, &colour.hidden(), &".".repeat(cols));
    }
    api::commit();
    REPAINT.store(true, Ordering::SeqCst);
}

/// What clears the inside of the panel's frame (not its two last rows, where Far writes the file's name and kind) in the panel's
/// colour, written straight to the console: the text Far printed for a file we show a picture of, and the picture
/// before. `keep` is a picture's place (row, column, rows, columns, as the screen counts them from 1) that is not
/// cleared: written over, it does not blink.
fn erase(rect: Rect, colour: Option<&api::FarColor>, keep: Option<(usize, usize, usize, usize)>) -> String {
    if off("wipe") {
        return String::new();
    }
    let Some((cols, rows)) = inside(rect) else {
        return String::new();
    };
    let mut out = String::from("\x1b7");
    out += &colour.map_or_else(|| "\x1b[0m".to_string(), api::FarColor::sgr);
    let first_col = usize::try_from(rect.left + 2).unwrap_or(1);
    let first_row = usize::try_from(rect.top + 2).unwrap_or(1);
    for row in first_row..first_row + rows {
        // the stretches of this row outside the picture, as (first column, width)
        let spans = match keep.filter(|&(r, _, n, _)| (r..r + n).contains(&row)) {
            Some((_, c, _, w)) => vec![
                (first_col, c.saturating_sub(first_col)),
                (c + w, (first_col + cols).saturating_sub(c + w)),
            ],
            None => vec![(first_col, cols)],
        };
        for (col, width) in spans.into_iter().filter(|&(_, width)| width > 0) {
            out += &format!("\x1b[{row};{col}H{}", " ".repeat(width));
        }
    }
    out + "\x1b[0m\x1b8"
}

pub fn write_console(bytes: &str) -> Result<(), String> {
    let mut console = std::fs::OpenOptions::new()
        .write(true)
        .open("CONOUT$")
        .map_err(|e| e.to_string())?;
    vt_on(&console);
    console.write_all(bytes.as_bytes()).map_err(|e| e.to_string())
}

/// Makes sure the console reads escape sequences in what is written to it; the log says if it had to be switched on.
fn vt_on(console: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    let handle = console.as_raw_handle() as isize;
    let mut mode = 0u32;
    // SAFETY: a console handle and a place for the mode.
    if unsafe { GetConsoleMode(handle, &mut mode) } != 0 && mode & 0x4 == 0 {
        log::line(&format!(
            "console output mode {mode:#x} lacked virtual terminal processing: switched on"
        ));
        // SAFETY: the same handle.
        unsafe { SetConsoleMode(handle, mode | 0x4) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_is_kept_with_its_shadow_in_screen_cells_from_1() {
        // a dialog at columns 20 to 59 and rows 5 to 12 (from 0): it starts at row 6, column 21 (from 1), is 8 rows and 40
        // columns, and its shadow adds two columns and a row
        let rect = api::ScreenRect {
            left: 20,
            top: 5,
            right: 59,
            bottom: 12,
        };
        assert_eq!(dialog_keep(rect), Some((6, 21, 9, 42)));
        let nothing = api::ScreenRect {
            left: 5,
            top: 5,
            right: 4,
            bottom: 5,
        };
        assert_eq!(dialog_keep(nothing), None);
    }

    #[test]
    fn a_dialog_is_the_box_of_the_cells_that_are_not_the_panels_colour() {
        // a grid of 6 columns and 4 rows in colour 1 (blue); a dialog of 3 x 2 cells in colour 7 at row 1, column 2
        let mut grid = vec![1u8; 24];
        for row in 1..3 {
            for col in 2..5 {
                grid[row * 6 + col] = 7;
            }
        }
        assert_eq!(dialog_box(&grid, 6, 1), Some((1, 2, 2, 3)));
        // its shadow (colour 0) to the right and below is part of the box
        grid[3 * 6 + 3] = 0;
        grid[2 * 6 + 5] = 0;
        assert_eq!(dialog_box(&grid, 6, 1), Some((1, 2, 3, 4)));
        // nothing over the panel
        assert_eq!(dialog_box(&[1; 24], 6, 1), None);
        assert_eq!(dialog_box(&[], 0, 1), None);
    }

    fn ask_card_for(path: Option<&str>) {
        if let Ok(mut asked) = INFO_FOR.lock() {
            *asked = path.map(PathBuf::from);
        }
    }

    #[test]
    fn the_card_is_asked_for_one_file_and_forgotten_when_another_is_shown_or_the_quick_view_closes() {
        ask_card_for(Some("a.jpg"));
        assert!(card_wanted(Path::new("a.jpg")));
        assert!(!card_wanted(Path::new("b.jpg")), "another file shows its picture");
        assert!(
            !card_wanted(Path::new("a.jpg")),
            "and coming back to the first one does not bring the card back"
        );
        ask_card_for(Some("a.jpg"));
        forget_card();
        assert!(!card_wanted(Path::new("a.jpg")), "a closed quick view forgets it");
    }

    #[test]
    fn erasing_leaves_the_pictures_place_alone() {
        // a frame of 9 x 6 cells: the inside is 8 columns from column 2, 4 rows from row 2
        let rect = Rect {
            left: 0,
            top: 0,
            right: 9,
            bottom: 7,
        };
        assert_eq!(inside(rect), Some((8, 4)));
        let all = erase(rect, None, None);
        for row in 2..6 {
            assert!(all.contains(&format!("\x1b[{row};2H        ")), "row {row}: {all:?}");
        }
        // a picture of 2 rows x 3 columns at row 3, column 4: the two rows it is on are cleared left and right of it
        let around = erase(rect, None, Some((3, 4, 2, 3)));
        for row in [2, 5] {
            assert!(around.contains(&format!("\x1b[{row};2H        ")), "row {row}");
        }
        for row in [3, 4] {
            assert!(
                around.contains(&format!("\x1b[{row};2H  \x1b")),
                "left of the picture, row {row}"
            );
            assert!(
                around.contains(&format!("\x1b[{row};7H   \x1b")) || around.contains(&format!("\x1b[{row};7H   ")),
                "right of it, row {row}"
            );
            assert!(!around.contains(&format!("\x1b[{row};4H")), "none on it, row {row}");
        }
    }

    #[test]
    fn ctrl_alt_i_is_the_key_for_the_card_and_nothing_less() {
        let (ctrl, alt, shift) = (0x8, 0x2, 0x10);
        assert!(is_info_key(u16::from(b'I'), ctrl | alt));
        assert!(is_info_key(u16::from(b'I'), 0x4 | 0x1), "the right Ctrl and Alt too");
        assert!(!is_info_key(u16::from(b'I'), ctrl), "Ctrl alone is Far's");
        assert!(!is_info_key(u16::from(b'I'), alt));
        assert!(!is_info_key(u16::from(b'I'), 0), "so is a plain i");
        assert!(!is_info_key(u16::from(b'I'), ctrl | alt | shift));
        assert!(!is_info_key(u16::from(b'O'), ctrl | alt));
    }

    #[test]
    fn ctrl_alt_s_is_the_key_for_the_store_and_nothing_less() {
        let (ctrl, alt, shift) = (0x8, 0x2, 0x10);
        assert!(is_store_key(u16::from(b'S'), ctrl | alt));
        assert!(is_store_key(u16::from(b'S'), 0x4 | 0x1), "the right Ctrl and Alt too");
        assert!(!is_store_key(u16::from(b'S'), ctrl), "Ctrl+S alone is Far's");
        assert!(!is_store_key(u16::from(b'S'), alt));
        assert!(!is_store_key(u16::from(b'S'), ctrl | alt | shift));
        assert!(
            !is_store_key(u16::from(b'I'), ctrl | alt),
            "the card's key is not the Store's"
        );
        assert!(!is_info_key(u16::from(b'S'), ctrl | alt));
    }

    #[test]
    fn a_panel_too_small_has_no_inside() {
        let tiny = Rect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 2,
        };
        assert_eq!(inside(tiny), None);
        assert_eq!(erase(tiny, None, None), "");
    }
}
