//! The Windows console: virtual-terminal input on (the terminal's answers to our questions arrive as characters; with it
//! off the console drops them, measured in BlueEdit), line editing, echo and Ctrl+C handling off, virtual-terminal output
//! on (what Windows Terminal has on already, and the old console host needs).

use std::io;
use std::mem::zeroed;

use windows_sys::Win32::Foundation::{HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetConsoleScreenBufferInfo,
    GetConsoleWindow, GetStdHandle, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};

use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GA_ROOTOWNER, GetAncestor, GetClassNameW, GetClientRect,
};
use windows_sys::core::BOOL;

use crate::WindowSize;

fn std_handle(which: STD_HANDLE) -> io::Result<HANDLE> {
    // SAFETY: GetStdHandle only looks up one of the process's standard handles.
    let handle = unsafe { GetStdHandle(which) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

fn mode_of(handle: HANDLE) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: `mode` is a valid place for the call to write one u32 to.
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

fn set_mode(handle: HANDLE, mode: u32) -> io::Result<()> {
    // SAFETY: SetConsoleMode takes the handle and a mode by value; a wrong handle is reported as an error.
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The console put in raw mode; the modes it had are set again when this is dropped.
#[derive(Debug)]
pub struct RawMode {
    input: HANDLE,
    output: HANDLE,
    saved_input: u32,
    saved_output: u32,
}

impl RawMode {
    /// Fails when standard input or output is not a console (a pipe, a file).
    pub fn enter() -> io::Result<Self> {
        let input = std_handle(STD_INPUT_HANDLE)?;
        let output = std_handle(STD_OUTPUT_HANDLE)?;
        let (saved_input, saved_output) = (mode_of(input)?, mode_of(output)?);
        let raw_input = (saved_input & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        set_mode(input, raw_input)?;
        if let Err(e) = set_mode(output, saved_output | ENABLE_VIRTUAL_TERMINAL_PROCESSING) {
            let _ = set_mode(input, saved_input);
            return Err(e);
        }
        Ok(Self {
            input,
            output,
            saved_input,
            saved_output,
        })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = set_mode(self.input, self.saved_input);
        let _ = set_mode(self.output, self.saved_output);
    }
}

/// The console window's size in cells (its pixels the console does not say).
pub fn window_size() -> io::Result<WindowSize> {
    let output = std_handle(STD_OUTPUT_HANDLE)?;
    // SAFETY: the struct is plain numbers, for which all zeros is a valid value.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { zeroed() };
    // SAFETY: `info` is a valid place for the call to fill in.
    if unsafe { GetConsoleScreenBufferInfo(output, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let window = info.srWindow;
    let side = |far: i16, near: i16| u16::try_from((far - near + 1).max(0)).unwrap_or(0);
    Ok(WindowSize {
        cols: side(window.Right, window.Left),
        rows: side(window.Bottom, window.Top),
        width_px: 0,
        height_px: 0,
    })
}

/// The name of the class of a window.
fn class_of(window: HWND) -> String {
    let mut name = [0u16; 128];
    // SAFETY: a buffer of its own, and its length is what the call is told.
    let n = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0).min(name.len())])
}

/// The size of the client area of a window in pixels.
fn client_size(window: HWND) -> Option<(i32, i32)> {
    // SAFETY: a rectangle of ours for the call to fill in; all zeros is a valid value of it.
    let mut rect: RECT = unsafe { zeroed() };
    // SAFETY: `rect` is a valid place for the call to write to.
    (unsafe { GetClientRect(window, &mut rect) } != 0).then_some((rect.right - rect.left, rect.bottom - rect.top))
}

/// What Windows says about the window of Windows Terminal that this console belongs to: the client area in pixels, the scale in
/// dpi, and the height of its title bar (the child window that holds the tabs; 0 if there is none). None when the console is
/// not that of Windows Terminal or the window cannot be read.
pub fn terminal_window() -> Option<((i32, i32), u32, i32)> {
    // A program that does not say it knows about the scale of the screen is told the sizes of other programs' windows as they would
    // be at 96 dpi (a 3840 pixel wide window as 2560 at 150 %), which no whole cell fits; the measuring is done as one that does
    // know (seen: `peekel.exe`, which has no such manifest, got 2560 x 1392 for a window of 291 columns of Consolas at 150 %).
    // SAFETY: a call with a constant; what it gives back is put again after the measuring.
    let before = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let found = measure_window();
    if !before.is_null() {
        // SAFETY: the context the thread had a moment ago.
        unsafe { SetThreadDpiAwarenessContext(before) };
    }
    found
}

fn measure_window() -> Option<((i32, i32), u32, i32)> {
    // SAFETY: calls with no pointers of ours.
    let console = unsafe { GetConsoleWindow() };
    if console.is_null() {
        return None;
    }
    // SAFETY: as above; the window that the terminal made for the console is owned by the terminal's own window.
    let window = unsafe { GetAncestor(console, GA_ROOTOWNER) };
    if window.is_null() || class_of(window) != "CASCADIA_HOSTING_WINDOW_CLASS" {
        return None;
    }
    let client = client_size(window)?;
    // SAFETY: a call with the window by value.
    let dpi = unsafe { GetDpiForWindow(window) };
    unsafe extern "system" fn bar_height(child: HWND, found: LPARAM) -> BOOL {
        if class_of(child) == "DRAG_BAR_WINDOW_CLASS"
            && let Some((_, height)) = client_size(child)
        {
            // SAFETY: `found` is the address of an i32 of the caller, which is there while the enumeration runs.
            unsafe { *(found as *mut i32) = height };
        }
        1
    }
    let mut bar = 0i32;
    // SAFETY: the callback only writes to `bar`, which lives through the call.
    unsafe { EnumChildWindows(window, Some(bar_height), &mut bar as *mut i32 as LPARAM) };
    Some((client, dpi, bar))
}
