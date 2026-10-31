//! What the readers of files that may be made to hurt do the same way: one error for "this cannot be read", a limit on the size
//! that is read, little-endian numbers read without a panic, and a thread of its own with a large stack on which a panic is an
//! error and not the end of the caller.

use std::path::Path;

use image::RgbaImage;

use crate::decode::DecodeError;

/// The stack of the thread a drawing runs on (a table inside a table inside ... as deep as the parser lets).
pub const STACK: usize = 128 * 1024 * 1024;

/// An error that says what could not be read.
pub fn unreadable(e: impl std::fmt::Display) -> DecodeError {
    DecodeError::Unreadable(e.to_string())
}

/// Whether the name of `path` ends in `.ext`, in any case.
pub fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// The bytes of `path`; a file bigger than `max` is refused before it is read.
pub fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, DecodeError> {
    if std::fs::metadata(path).map_err(unreadable)?.len() > max {
        return Err(DecodeError::TooLarge);
    }
    std::fs::read(path).map_err(unreadable)
}

/// Runs `work` on a thread called `name` with a large stack; a panic of it is the error `failure`.
pub fn draw_on_thread<F>(name: &str, failure: &str, work: F) -> Result<RgbaImage, DecodeError>
where
    F: FnOnce() -> Result<RgbaImage, DecodeError> + Send,
{
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(name.to_string())
            .stack_size(STACK)
            .spawn_scoped(scope, work)
            .map_err(unreadable)?
            .join()
            .unwrap_or_else(|_| Err(unreadable(failure)))
    })
}

/// The little-endian `u16` at `at`, if the data reaches that far.
pub fn le_u16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

/// The little-endian `u32` at `at`, if the data reaches that far.
pub fn le_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at.checked_add(4)?)?.try_into().ok()?))
}
