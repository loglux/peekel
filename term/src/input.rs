//! Standard input read on a thread of its own, so that the main thread can wait for a byte with a timeout (the terminal
//! may not answer a question at all, and the window's size has to be looked at while waiting for a key).

use std::io::Read;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

/// The bytes of standard input as they arrive. The thread ends when input does (or when the receiver is dropped, at its
/// next byte); it may stay blocked in `read` until the process exits, which is fine for a program that is about to.
pub fn spawn() -> Receiver<u8> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        read_into(|byte, _| sender.send(byte).is_ok());
    });
    receiver
}

/// As [`spawn`], each byte with the time it was read: a program that was busy reads what came meanwhile all at once, and only the
/// times tell an Escape pressed and then a key from Alt and the key (see `reply::Parser::push_at`).
pub fn spawn_timed() -> Receiver<(u8, Instant)> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        read_into(|byte, at| sender.send((byte, at)).is_ok());
    });
    receiver
}

/// Reads standard input and hands each byte to `send` with the time of its read, until input ends or `send` says to stop.
fn read_into(mut send: impl FnMut(u8, Instant) -> bool) {
    let mut stdin = std::io::stdin().lock();
    let mut buffer = [0u8; 256];
    loop {
        match stdin.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let at = Instant::now();
                for &byte in &buffer[..n] {
                    if !send(byte, at) {
                        return;
                    }
                }
            }
        }
    }
}
