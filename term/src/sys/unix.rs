//! A Unix terminal: termios in raw mode (no echo, no line editing, no signals from Ctrl+C: it arrives as a byte), and the
//! window's size from the kernel, pixels included where the terminal fills them in.

use std::io;
use std::mem::{MaybeUninit, zeroed};

use crate::WindowSize;

/// The terminal put in raw mode; the settings it had are set again when this is dropped.
#[derive(Debug)]
pub struct RawMode {
    saved: libc::termios,
}

impl RawMode {
    /// Fails when standard input is not a terminal.
    pub fn enter() -> io::Result<Self> {
        let mut current = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: on success tcgetattr has filled the struct in; on failure it is not used.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, current.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: filled in by the successful call above.
        let saved = unsafe { current.assume_init() };
        let mut raw = saved;
        // SAFETY: `raw` is a valid termios, which cfmakeraw only changes.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: `raw` is a valid termios for the call to read.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: `saved` is the termios tcgetattr gave; a failure here cannot be helped.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved) };
    }
}

/// The window's size, from whichever of the standard streams is the terminal.
pub fn window_size() -> io::Result<WindowSize> {
    for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO] {
        // SAFETY: the struct is plain numbers, for which all zeros is a valid value.
        let mut size: libc::winsize = unsafe { zeroed() };
        // SAFETY: TIOCGWINSZ writes one winsize through the pointer.
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size) } == 0 {
            return Ok(WindowSize {
                cols: size.ws_col,
                rows: size.ws_row,
                width_px: size.ws_xpixel,
                height_px: size.ws_ypixel,
            });
        }
    }
    Err(io::Error::last_os_error())
}

/// Only Windows Terminal has a window of this kind to measure.
pub fn terminal_window() -> Option<((i32, i32), u32, i32)> {
    None
}
