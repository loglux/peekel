//! The terminal side of Peekel: raw mode, the window's size, and reading what the terminal sends.
//!
//! [`reply`] and [`input`] are plain safe Rust. The console's mode and size are reachable only through the system's
//! own calls, which is what [`sys`] holds (the one module here that allows `unsafe`).

pub mod clipboard;
pub mod input;
pub mod reply;
pub mod win32keys;

#[allow(unsafe_code)]
// the console's mode and size: SetConsoleMode and the like on Windows, termios and ioctl elsewhere
mod sys;

pub use sys::{RawMode, window_size};

pub mod realcell;

/// The window in character cells and, where the system says so, in pixels (0 when it does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSize {
    pub cols: u16,
    pub rows: u16,
    pub width_px: u16,
    pub height_px: u16,
}
