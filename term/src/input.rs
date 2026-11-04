//! Standard input read on a thread of its own, so that the main thread can wait for a byte with a timeout (the terminal
//! may not answer a question at all, and the window's size has to be looked at while waiting for a key).

use std::io::Read;
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// The bytes of standard input as they arrive. The thread ends when input does (or when the receiver is dropped, at its
/// next byte); it may stay blocked in `read` until the process exits, which is fine for a program that is about to.
pub fn spawn() -> Receiver<u8> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0u8; 256];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    for &byte in &buffer[..n] {
                        if sender.send(byte).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
    receiver
}
