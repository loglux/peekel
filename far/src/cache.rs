//! The pictures already made: a Sixel stream per picture, kept for the ones next to the cursor so that moving to
//! them costs nothing but the writing.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// What a stream was made from. A file that changed, a panel of another size or another background colour is
/// another key.
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct Key {
    pub path: PathBuf,
    /// The pixels the picture had to fit.
    pub room: (usize, usize),
    pub background: [u8; 3],
    /// Whether it is the card of the file (kind, size, EXIF) and not the picture.
    pub info: bool,
    pub length: u64,
    pub modified: Option<SystemTime>,
}

/// A picture made into Sixel, whole cells wide and high.
#[derive(Debug)]
pub struct Rendered {
    pub stream: String,
    pub width: usize,
    pub height: usize,
}

/// The most recently used pictures, as many as fit in `limit` bytes of Sixel (the last one stays whatever its size).
#[derive(Debug)]
pub struct Cache {
    entries: VecDeque<(Key, Arc<Rendered>)>,
    bytes: usize,
    limit: usize,
}

impl Cache {
    pub const fn new(limit: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn get(&mut self, key: &Key) -> Option<Arc<Rendered>> {
        let at = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(at)?;
        let found = Arc::clone(&entry.1);
        self.entries.push_front(entry);
        Some(found)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn insert(&mut self, key: Key, picture: Arc<Rendered>) {
        if let Some(at) = self.entries.iter().position(|(k, _)| *k == key)
            && let Some((_, old)) = self.entries.remove(at)
        {
            self.bytes -= old.stream.len();
        }
        self.bytes += picture.stream.len();
        self.entries.push_front((key, picture));
        while self.bytes > self.limit && self.entries.len() > 1 {
            if let Some((_, gone)) = self.entries.pop_back() {
                self.bytes -= gone.stream.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key {
        Key {
            path: PathBuf::from(name),
            room: (100, 50),
            background: [0, 0, 128],
            info: false,
            length: 1,
            modified: None,
        }
    }

    fn picture(bytes: usize) -> Arc<Rendered> {
        Arc::new(Rendered {
            stream: "x".repeat(bytes),
            width: 10,
            height: 10,
        })
    }

    #[test]
    fn a_picture_is_found_by_what_it_was_made_from_and_not_by_anything_less() {
        let mut cache = Cache::new(1000);
        cache.insert(key("a"), picture(10));
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("b")).is_none());
        for changed in [
            Key {
                room: (101, 50),
                ..key("a")
            },
            Key {
                background: [0, 0, 0],
                ..key("a")
            },
            Key { info: true, ..key("a") },
            Key { length: 2, ..key("a") },
            Key {
                modified: Some(SystemTime::UNIX_EPOCH),
                ..key("a")
            },
        ] {
            assert!(cache.get(&changed).is_none(), "{changed:?}");
        }
    }

    #[test]
    fn the_least_recently_used_goes_first_when_the_bytes_run_out() {
        let mut cache = Cache::new(25);
        cache.insert(key("a"), picture(10));
        cache.insert(key("b"), picture(10));
        assert!(
            cache.get(&key("a")).is_some(),
            "a is used again, so b is now the oldest"
        );
        cache.insert(key("c"), picture(10));
        assert!(cache.contains(&key("a")) && cache.contains(&key("c")));
        assert!(!cache.contains(&key("b")));
    }

    #[test]
    fn one_picture_larger_than_the_limit_is_kept_alone() {
        let mut cache = Cache::new(5);
        cache.insert(key("a"), picture(10));
        assert!(cache.contains(&key("a")));
        cache.insert(key("b"), picture(10));
        assert!(!cache.contains(&key("a")) && cache.contains(&key("b")));
    }

    #[test]
    fn inserting_a_key_again_replaces_it_and_counts_its_bytes_once() {
        let mut cache = Cache::new(25);
        cache.insert(key("a"), picture(10));
        cache.insert(key("a"), picture(12));
        cache.insert(key("b"), picture(12));
        assert!(
            cache.contains(&key("a")) && cache.contains(&key("b")),
            "24 bytes fit in 25"
        );
    }
}
