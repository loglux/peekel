//! The system's calls, one file per family. Both give `RawMode` (put back when dropped) and `window_size`.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::{RawMode, terminal_window, window_size};
#[cfg(windows)]
pub use windows::{RawMode, terminal_window, window_size};
