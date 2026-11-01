//! Far's C structures and calls, as `plugin.hpp` (Far 3, x64) lays them out; only what the plugin uses.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::{ADV_CONTROL, GUID, PANEL_CONTROL, SEND_DLG_MESSAGE, TEXT, VIEWER_CONTROL};

pub const PF_PRELOAD: u64 = 1;
const PANEL_ACTIVE: isize = -1;
const PANEL_PASSIVE: isize = -2;
const FCTL_GETPANELINFO: i32 = 1;
const FCTL_GETPANELITEM: i32 = 21;
const FCTL_GETCURRENTPANELITEM: i32 = 23;
const FCTL_GETPANELDIRECTORY: i32 = 24;
const ACTL_GETCOLOR: i32 = 3;
const ACTL_COMMIT: i32 = 9;
const ACTL_SYNCHRO: i32 = 20;
const FCTL_REDRAWPANEL: i32 = 3;
/// `COL_PANELTEXT` of `farcolor.hpp`: the sixth colour of the menus, the four of the horizontal menu, then this.
const COL_PANELTEXT: isize = 10;
const FCF_FG_4BIT: u64 = 1;
const FCF_BG_4BIT: u64 = 2;
pub const PTYPE_QVIEWPANEL: i32 = 2;
pub const PFLAGS_PLUGIN: u64 = 0x800;
const FILE_ATTRIBUTE_DIRECTORY: usize = 0x10;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VersionInfo {
    major: u32,
    minor: u32,
    revision: u32,
    build: u32,
    stage: i32,
}

impl VersionInfo {
    pub const fn new(major: u32, minor: u32, revision: u32, build: u32) -> Self {
        Self {
            major,
            minor,
            revision,
            build,
            stage: 0,
        }
    }
}

#[repr(C)]
pub struct GlobalInfo {
    pub struct_size: usize,
    pub min_far_version: VersionInfo,
    pub version: VersionInfo,
    pub guid: [u8; 16],
    pub title: *const u16,
    pub description: *const u16,
    pub author: *const u16,
    pub instance: *mut c_void,
}

#[repr(C)]
pub struct PluginMenuItem {
    guids: *const c_void,
    strings: *const c_void,
    count: usize,
}

#[repr(C)]
pub struct PluginInfo {
    pub struct_size: usize,
    pub flags: u64,
    disk_menu: PluginMenuItem,
    plugin_menu: PluginMenuItem,
    plugin_config: PluginMenuItem,
    command_prefix: *const u16,
    instance: *mut c_void,
}

impl Default for PluginInfo {
    fn default() -> Self {
        // SAFETY: every field is an integer or a pointer; all-zero is "nothing" for each.
        unsafe { std::mem::zeroed() }
    }
}

/// The start of Far's `PluginStartupInfo`: the calls are kept as plain addresses, in Far's order.
#[repr(C)]
pub struct PluginStartupInfo {
    pub struct_size: usize,
    pub module_name: *const u16,
    menu: usize,
    message: usize,
    get_msg: usize,
    pub panel_control: usize,
    save_screen: usize,
    restore_screen: usize,
    get_dir_list: usize,
    get_plugin_dir_list: usize,
    free_dir_list: usize,
    free_plugin_dir_list: usize,
    viewer: usize,
    editor: usize,
    pub text: usize,
    editor_control: usize,
    fsf: usize,
    show_help: usize,
    pub adv_control: usize,
    input_box: usize,
    color_dialog: usize,
    dialog_init: usize,
    dialog_run: usize,
    dialog_free: usize,
    pub send_dlg_message: usize,
    def_dlg_proc: usize,
    pub viewer_control: usize,
}

const ACTL_SETPROGRESSSTATE: i32 = 21;
/// `TBPS_INDETERMINATE` of `TASKBARPROGRESSTATE`: a progress with no end in sight; `TBPS_NOPROGRESS` is none.
const TBPS_INDETERMINATE: isize = 1;
const TBPS_NOPROGRESS: isize = 0;

/// Shows progress of no known length on the taskbar button (and the tab, in a terminal that shows it) while a picture is made,
/// or takes it away. Far's own call: it must be made on Far's thread (the tick is).
pub fn set_progress(busy: bool) {
    adv(
        ACTL_SETPROGRESSSTATE,
        if busy { TBPS_INDETERMINATE } else { TBPS_NOPROGRESS },
        std::ptr::null_mut(),
    );
}

/// `DE_DLGPROCINIT` of `FARDIALOGEVENTS`: Far is about to give the dialog's own procedure a message.
pub const DE_DLGPROCINIT: isize = 0;
/// `DN_DRAWDIALOG`: the dialog is about to be painted.
pub const DN_DRAWDIALOG: isize = 4101;
/// `DM_GETDLGRECT`: where the dialog is on the screen.
const DM_GETDLGRECT: isize = 6;

/// What Far gives `ProcessDialogEventW`.
#[repr(C)]
pub struct ProcessDialogEventInfo {
    pub struct_size: usize,
    pub event: isize,
    pub param: *const FarDialogEvent,
    pub instance: *mut c_void,
}

/// The message of a dialog (`FarDialogEvent`).
#[repr(C)]
pub struct FarDialogEvent {
    pub struct_size: usize,
    pub dialog: isize,
    pub msg: isize,
    pub param1: isize,
    pub param2: *mut c_void,
    pub result: isize,
}

/// `SMALL_RECT`: left, top, right and bottom, from 0, the last row and column inside.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
}

type SendDlgMessageFn = unsafe extern "system" fn(isize, isize, isize, *mut c_void) -> isize;

/// Where a dialog is on the screen (not its shadow), as it is about to be painted.
pub fn dialog_rect(dialog: isize) -> Option<ScreenRect> {
    let f = SEND_DLG_MESSAGE.load(Ordering::SeqCst);
    if f == 0 {
        return None;
    }
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature; DM_GETDLGRECT fills a SMALL_RECT.
    let f = unsafe { std::mem::transmute::<usize, SendDlgMessageFn>(f) };
    let mut rect = ScreenRect::default();
    // SAFETY: Far's own call, with the handle it gave and a place for a SMALL_RECT.
    let ok = unsafe { f(dialog, DM_GETDLGRECT, 0, (&raw mut rect).cast()) };
    (ok != 0).then_some(rect)
}

/// `KEY_EVENT` of `INPUT_RECORD`.
pub const KEY_EVENT: u16 = 1;
/// `WINDOW_BUFFER_SIZE_EVENT` of `INPUT_RECORD`: the console (the terminal's window) was resized.
pub const WINDOW_BUFFER_SIZE_EVENT: u16 = 4;

/// The Windows `INPUT_RECORD` as a key event has it (the other kinds are not looked into).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct InputRecord {
    pub event_type: u16,
    pub key_down: i32,
    pub repeat: u16,
    pub virtual_key: u16,
    pub scan: u16,
    pub ch: u16,
    pub control_keys: u32,
}

#[repr(C)]
pub struct ProcessConsoleInputInfo {
    pub struct_size: usize,
    pub flags: u64,
    pub record: InputRecord,
    pub instance: *mut c_void,
}

#[repr(C)]
pub struct ProcessViewerEventInfo {
    pub struct_size: usize,
    pub event: isize,
    pub param: *mut c_void,
    pub viewer_id: isize,
    pub instance: *mut c_void,
}

#[repr(C)]
pub struct ProcessSynchroEventInfo {
    pub struct_size: usize,
    pub event: isize,
    pub param: *mut c_void,
    pub instance: *mut c_void,
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
struct PanelInfoRaw {
    struct_size: usize,
    plugin_handle: *mut c_void,
    owner_guid: [u8; 16],
    flags: u64,
    items_number: usize,
    selected_items_number: usize,
    rect: Rect,
    current_item: usize,
    top_panel_item: usize,
    view_mode: isize,
    panel_type: i32,
    sort_mode: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Panel {
    pub kind: i32,
    pub flags: u64,
    pub rect: Rect,
    /// The place of the cursor in the list, and the number of items (the ".." is the first).
    pub current: usize,
    pub count: usize,
}

#[repr(C)]
struct PluginPanelItem {
    times: [u64; 4],
    file_size: u64,
    allocation_size: u64,
    file_name: *const u16,
    alternate_file_name: *const u16,
    description: *const u16,
    owner: *const u16,
    custom_column_data: *const c_void,
    custom_column_number: usize,
    flags: u64,
    user_data: [usize; 2],
    file_attributes: usize,
    number_of_links: usize,
    crc32: usize,
    reserved: [isize; 2],
}

#[repr(C)]
struct FarGetPluginPanelItem {
    struct_size: usize,
    size: usize,
    item: *mut PluginPanelItem,
}

#[repr(C)]
struct FarPanelDirectory {
    struct_size: usize,
    name: *const u16,
    param: *const u16,
    plugin_id: [u8; 16],
    file: *const u16,
}

type PanelControlFn = unsafe extern "system" fn(isize, i32, isize, *mut c_void) -> isize;
type AdvControlFn = unsafe extern "system" fn(*const [u8; 16], i32, isize, *mut c_void) -> isize;

/// Far's `FCTL_*` call; 0 (a failure) while Far has not given its address yet.
///
/// # Safety
/// `param` is what `command` takes, valid for the call.
unsafe fn panel_call(panel: isize, command: i32, index: isize, param: *mut c_void) -> isize {
    let address = PANEL_CONTROL.load(Ordering::SeqCst);
    if address == 0 {
        return 0;
    }
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature.
    let f = unsafe { std::mem::transmute::<usize, PanelControlFn>(address) };
    unsafe { f(panel, command, index, param) }
}

/// Asks Far to call `ProcessSynchroEventW` on its own thread.
pub fn request_synchro() {
    let f = ADV_CONTROL.load(Ordering::SeqCst);
    if f == 0 {
        return;
    }
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature.
    let f = unsafe { std::mem::transmute::<usize, AdvControlFn>(f) };
    // SAFETY: ACTL_SYNCHRO takes any pointer as its parameter and hands it back; none is used.
    unsafe { f(&GUID, ACTL_SYNCHRO, 0, std::ptr::null_mut()) };
}

/// Far's `FarColor`: flags say whether each colour is a 4-bit index (the console's 16) or `0xAABBGGRR`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FarColor {
    flags: u64,
    foreground: u32,
    background: u32,
    underline: u32,
    reserved: u32,
}

// Far's palette is an array of these: a wrong size reads the wrong element (the last field is a DWORD in plugin.hpp)
const _: () = assert!(size_of::<FarColor>() == 24);

/// The console's 16 colours as Windows draws them by default (Far's indexes follow the console attribute bits).
const CONSOLE_COLOURS: [[u8; 3]; 16] = [
    [0, 0, 0],
    [0, 0, 128],
    [0, 128, 0],
    [0, 128, 128],
    [128, 0, 0],
    [128, 0, 128],
    [128, 128, 0],
    [192, 192, 192],
    [128, 128, 128],
    [0, 0, 255],
    [0, 255, 0],
    [0, 255, 255],
    [255, 0, 0],
    [255, 0, 255],
    [255, 255, 0],
    [255, 255, 255],
];

impl FarColor {
    /// The background as the console's colour number (0 to 15), when Far gives it as one.
    pub fn background_index(&self) -> Option<u32> {
        (self.flags & FCF_BG_4BIT != 0).then_some(self.background & 15)
    }

    /// The background as red, green, blue. For an index it is what the terminal said that colour is, or until it has
    /// said, the console's default palette (a guess: the terminal may have changed it).
    pub fn background_rgb(&self) -> [u8; 3] {
        if self.flags & FCF_BG_4BIT != 0 {
            crate::cell::palette_colour(self.background).unwrap_or(CONSOLE_COLOURS[(self.background & 15) as usize])
        } else {
            let c = self.background;
            [c as u8, (c >> 8) as u8, (c >> 16) as u8]
        }
    }

    /// The escape sequence that sets this colour: the 16 colours of an index, or 24-bit ones.
    pub fn sgr(&self) -> String {
        let code = |index: bool, value: u32, base: u32| {
            if index {
                let i = value & 15;
                let ansi = (i >> 2 & 1) + (i >> 1 & 1) * 2 + (i & 1) * 4;
                format!("{}", base + if i >= 8 { 60 } else { 0 } + ansi)
            } else {
                format!(
                    "{};2;{};{};{}",
                    base + 8,
                    value & 255,
                    value >> 8 & 255,
                    value >> 16 & 255
                )
            }
        };
        format!(
            "\x1b[0;{};{}m",
            code(self.flags & FCF_FG_4BIT != 0, self.foreground, 30),
            code(self.flags & FCF_BG_4BIT != 0, self.background, 40)
        )
    }

    /// The same background as the foreground too: what is written in it cannot be seen.
    pub fn hidden(&self) -> Self {
        let both = if self.flags & FCF_BG_4BIT != 0 {
            FCF_FG_4BIT | FCF_BG_4BIT
        } else {
            0
        };
        Self {
            flags: (self.flags & !(FCF_FG_4BIT | FCF_BG_4BIT)) | both,
            foreground: self.background,
            ..*self
        }
    }
}

type TextFn = unsafe extern "system" fn(isize, isize, *const FarColor, *const u16);

/// Far's `Text`: `s` into Far's own screen buffer at column `x`, row `y` (0-based, the screen's); `ACTL_COMMIT` shows it.
pub fn text(x: i32, y: i32, colour: &FarColor, s: &str) {
    let f = TEXT.load(Ordering::SeqCst);
    if f == 0 {
        return;
    }
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature.
    let f = unsafe { std::mem::transmute::<usize, TextFn>(f) };
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0);
    // SAFETY: a colour and a NUL-ended string that outlive the call.
    unsafe { f(x as isize, y as isize, colour, wide.as_ptr()) };
}

/// Makes Far write what `text` put in its buffer to the console now.
pub fn commit() {
    adv(ACTL_COMMIT, 0, std::ptr::null_mut());
}

/// The colour of the panels' text (cyan on blue by default), None when Far does not say.
pub fn panel_colour() -> Option<FarColor> {
    let mut colour = FarColor::default();
    (adv(ACTL_GETCOLOR, COL_PANELTEXT, (&raw mut colour).cast()) != 0).then_some(colour)
}

pub const VE_READ: isize = 0;
pub const VE_CLOSE: isize = 1;
const VCTL_GETINFO: i32 = 0;
const VCTL_GETFILENAME: i32 = 7;
const VCTL_SETMODE: i32 = 6;
/// `VMT_TEXT` and `VMT_HEX` of `VIEWER_MODE_TYPE`.
const VMT_TEXT: isize = 0;
const VMT_HEX: isize = 1;
const VCTL_SETPOSITION: i32 = 4;
const VSP_NOREDRAW: u64 = 1;
const VSP_NORETNEWPOS: u64 = 8;
const VOPT_QUICKVIEW: u64 = 0x20;
const ACTL_GETARRAYCOLOR: i32 = 4;
const ACTL_SETARRAYCOLOR: i32 = 16;
/// `COL_VIEWERTEXT` and `COL_VIEWERARROWS` of `farcolor.hpp` (counted from `COL_MENUTEXT` = 0).
pub const COL_VIEWERTEXT: usize = 50;
pub const COL_VIEWERARROWS: usize = 70;
const ACTL_GETWINDOWINFO: i32 = 6;
/// `WTYPE_PANELS` of `WINDOWINFO_TYPE`: the panels are the window on top (no dialog, menu or viewer over them).
pub const WTYPE_PANELS: i32 = 1;

type ViewerControlFn = unsafe extern "system" fn(isize, i32, isize, *mut c_void) -> isize;

#[repr(C)]
struct ViewerInfoRaw {
    struct_size: usize,
    viewer_id: isize,
    tab_size: isize,
    code_page: usize,
    mode_flags: u64,
    view_mode: i32,
    file_size: i64,
    file_pos: i64,
    left_pos: i64,
    options: u64,
    window_size_x: isize,
    window_size_y: isize,
}

#[repr(C)]
struct ViewerSetMode {
    struct_size: usize,
    /// `VSMT_VIEWMODE` (0)
    kind: i32,
    param: isize,
    flags: u64,
}

#[repr(C)]
struct ViewerSetPosition {
    struct_size: usize,
    flags: u64,
    start_pos: i64,
    left_pos: i64,
}

#[repr(C)]
struct FarSetColors {
    struct_size: usize,
    flags: u64,
    start_index: usize,
    colors_count: usize,
    colors: *const FarColor,
}

fn viewer_control() -> Option<ViewerControlFn> {
    let f = VIEWER_CONTROL.load(Ordering::SeqCst);
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature.
    (f != 0).then(|| unsafe { std::mem::transmute::<usize, ViewerControlFn>(f) })
}

/// Whether the viewer is the quick view's, and the file it shows.
pub fn viewer_file(viewer: isize) -> Option<(bool, std::path::PathBuf)> {
    let control = viewer_control()?;
    // SAFETY: an all-zero ViewerInfoRaw is plain integers; Far fills it in.
    let mut info: ViewerInfoRaw = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<ViewerInfoRaw>();
    // SAFETY: `info` is the structure VCTL_GETINFO fills.
    if unsafe { control(viewer, VCTL_GETINFO, 0, (&raw mut info).cast()) } == 0 {
        return None;
    }
    // SAFETY: a null buffer asks for the length (with the NUL).
    let size = unsafe { control(viewer, VCTL_GETFILENAME, 0, std::ptr::null_mut()) };
    let size = usize::try_from(size).ok().filter(|&s| s > 1)?;
    let mut name = vec![0u16; size];
    // SAFETY: a buffer of `size` units, as asked for.
    if unsafe { control(viewer, VCTL_GETFILENAME, size as isize, name.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    // SAFETY: the buffer is NUL-ended (Far wrote the NUL, and the last unit is 0 anyway after the zero fill).
    let name = unsafe { wide(name.as_ptr()) };
    Some((info.options & VOPT_QUICKVIEW != 0, std::path::PathBuf::from(name)))
}

/// Sets how the viewer shows its file (`VMT_TEXT` or `VMT_HEX`), without painting it again.
fn set_viewer_view_mode(viewer: isize, view: isize) -> bool {
    let Some(control) = viewer_control() else {
        return false;
    };
    let mut mode = ViewerSetMode {
        struct_size: size_of::<ViewerSetMode>(),
        kind: 0,
        param: view,
        flags: 0,
    };
    // SAFETY: `mode` is the structure VCTL_SETMODE reads.
    unsafe { control(viewer, VCTL_SETMODE, 0, (&raw mut mode).cast()) != 0 }
}

/// Makes the viewer show its file as hex (digits and letters only).
pub fn set_viewer_hex(viewer: isize) -> bool {
    set_viewer_view_mode(viewer, VMT_HEX)
}

/// Makes the viewer show nothing of its file: as text, scrolled to the right of the end of every line, so that it
/// prints blanks and not one sign of the file.
pub fn set_viewer_blank(viewer: isize) -> bool {
    let Some(control) = viewer_control() else {
        return false;
    };
    if !set_viewer_view_mode(viewer, VMT_TEXT) {
        return false;
    }
    let mut position = ViewerSetPosition {
        struct_size: size_of::<ViewerSetPosition>(),
        flags: VSP_NOREDRAW | VSP_NORETNEWPOS,
        start_pos: 0,
        left_pos: 1 << 24,
    };
    // SAFETY: `position` is the structure VCTL_SETPOSITION reads.
    unsafe { control(viewer, VCTL_SETPOSITION, 0, (&raw mut position).cast()) != 0 }
}

/// The colour Far has now at `index` of its palette.
pub fn palette_colour(index: usize) -> Option<FarColor> {
    let count = usize::try_from(adv(ACTL_GETARRAYCOLOR, 0, std::ptr::null_mut())).ok()?;
    if count <= index {
        return None;
    }
    let mut all = vec![FarColor::default(); count];
    adv(ACTL_GETARRAYCOLOR, count as isize, all.as_mut_ptr().cast());
    Some(all[index])
}

#[repr(C)]
struct WindowInfoRaw {
    struct_size: usize,
    id: isize,
    type_name: *mut u16,
    name: *mut u16,
    type_name_size: isize,
    name_size: isize,
    pos: isize,
    kind: i32,
    flags: u64,
}

/// The window on top in Far (its id and type): the panels, or a dialog, a menu, a viewer, an editor over them.
pub fn current_window() -> Option<(isize, i32)> {
    // SAFETY: an all-zero WindowInfoRaw is integers and null pointers; null name buffers ask for no names.
    let mut info: WindowInfoRaw = unsafe { std::mem::zeroed() };
    info.struct_size = size_of::<WindowInfoRaw>();
    info.pos = -1;
    (adv(ACTL_GETWINDOWINFO, 0, (&raw mut info).cast()) != 0).then_some((info.id, info.kind))
}

/// Sets the colour of `index` in Far's palette (no repaint).
pub fn set_palette_colour(index: usize, colour: &FarColor) -> bool {
    let mut set = FarSetColors {
        struct_size: size_of::<FarSetColors>(),
        flags: 0,
        start_index: index,
        colors_count: 1,
        colors: colour,
    };
    adv(ACTL_SETARRAYCOLOR, 0, (&raw mut set).cast()) != 0
}

/// Asks Far to paint the other panel again.
pub fn redraw_passive_panel() {
    // SAFETY: FCTL_REDRAWPANEL takes no structure for a null parameter (a panel redrawn as it is).
    unsafe { panel_call(PANEL_PASSIVE, FCTL_REDRAWPANEL, 0, std::ptr::null_mut()) };
}

fn adv(command: i32, param1: isize, param2: *mut c_void) -> isize {
    let f = ADV_CONTROL.load(Ordering::SeqCst);
    if f == 0 {
        return 0;
    }
    // SAFETY: the address Far gave in `SetStartupInfoW`, a function of this signature.
    let f = unsafe { std::mem::transmute::<usize, AdvControlFn>(f) };
    // SAFETY: the parameters of each command used here are what the callers pass.
    unsafe { f(&GUID, command, param1, param2) }
}

pub fn active_panel() -> Option<Panel> {
    panel_info(PANEL_ACTIVE)
}

pub fn passive_panel() -> Option<Panel> {
    panel_info(PANEL_PASSIVE)
}

fn panel_info(which: isize) -> Option<Panel> {
    // SAFETY: an all-zero PanelInfoRaw is plain integers and null pointers; Far fills it in.
    let mut raw: PanelInfoRaw = unsafe { std::mem::zeroed() };
    raw.struct_size = size_of::<PanelInfoRaw>();
    // SAFETY: `raw` is the structure FCTL_GETPANELINFO fills.
    let ok = unsafe { panel_call(which, FCTL_GETPANELINFO, 0, (&raw mut raw).cast()) };
    (ok != 0).then_some(Panel {
        kind: raw.panel_type,
        flags: raw.flags,
        rect: raw.rect,
        current: raw.current_item,
        count: raw.items_number,
    })
}

/// The name of an item of the active panel and whether it is a folder: `command` is `FCTL_GETPANELITEM` (item number
/// `index`) or `FCTL_GETCURRENTPANELITEM` (`index` is ignored).
fn item(command: i32, index: usize) -> Option<(String, bool)> {
    let which = PANEL_ACTIVE;
    let index = index as isize;
    // SAFETY: a null buffer asks for the size.
    let size = unsafe { panel_call(which, command, index, std::ptr::null_mut()) };
    let size = usize::try_from(size)
        .ok()
        .filter(|&s| s >= size_of::<PluginPanelItem>())?;
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let mut request = FarGetPluginPanelItem {
        struct_size: size_of::<FarGetPluginPanelItem>(),
        size,
        item: buffer.as_mut_ptr().cast(),
    };
    // SAFETY: the buffer is `size` bytes, as asked for, aligned for the structure.
    let ok = unsafe { panel_call(which, command, index, (&raw mut request).cast()) };
    if ok == 0 {
        return None;
    }
    // SAFETY: Far filled the item; its strings live in the same buffer.
    let item = unsafe { &*request.item };
    // SAFETY: a NUL-ended string inside the buffer.
    let name = unsafe { wide(item.file_name) };
    Some((name, item.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0))
}

/// The folder the active panel shows.
fn directory() -> Option<std::path::PathBuf> {
    let which = PANEL_ACTIVE;
    // SAFETY: a null buffer asks for the size.
    let size = unsafe { panel_call(which, FCTL_GETPANELDIRECTORY, 0, std::ptr::null_mut()) };
    let size = usize::try_from(size)
        .ok()
        .filter(|&s| s >= size_of::<FarPanelDirectory>())?;
    let mut buffer = vec![0u64; size.div_ceil(8)];
    buffer[0] = size_of::<FarPanelDirectory>() as u64;
    // SAFETY: the buffer is `size` bytes, as asked for.
    let ok = unsafe { panel_call(which, FCTL_GETPANELDIRECTORY, size as isize, buffer.as_mut_ptr().cast()) };
    if ok == 0 {
        return None;
    }
    // SAFETY: Far filled the structure; the name lives in the same buffer.
    let dir = unsafe { wide((*buffer.as_ptr().cast::<FarPanelDirectory>()).name) };
    Some(std::path::PathBuf::from(dir))
}

/// The path of the file under the cursor in the active panel (None for a folder or when Far does not say).
pub fn current_file() -> Option<std::path::PathBuf> {
    let (name, folder) = item(FCTL_GETCURRENTPANELITEM, 0)?;
    (!folder).then(|| directory().map(|d| d.join(name)))?
}

/// The files that are `offsets` places from the cursor in the active panel (negative: before it), in the order of
/// the panel as it is sorted now; folders and places beyond the list are left out.
pub fn files_around(offsets: &[isize]) -> Vec<std::path::PathBuf> {
    let Some(panel) = active_panel() else {
        return Vec::new();
    };
    let Some(dir) = directory() else {
        return Vec::new();
    };
    offsets
        .iter()
        .filter_map(|&offset| {
            let index = panel.current.checked_add_signed(offset).filter(|&i| i < panel.count)?;
            let (name, folder) = item(FCTL_GETPANELITEM, index)?;
            (!folder).then(|| dir.join(name))
        })
        .collect()
}

/// A NUL-ended UTF-16 string from Far.
///
/// # Safety
/// `p` is null or points to a NUL-ended string.
unsafe fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    // SAFETY: the caller promises the NUL.
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    // SAFETY: `n` units were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}
