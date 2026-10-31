//! A log for the experiment: lines appended to the file named by `PEEKEL_FAR_LOG`, if it is set.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn line(text: &str) {
    let Some(path) = std::env::var_os("PEEKEL_FAR_LOG") else {
        return;
    };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() % 1_000_000);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ms:06} {text}");
    }
}
