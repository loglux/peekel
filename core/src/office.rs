//! What the readers of Office files (docx, pptx, xlsx) share: the limits on what is read from an archive, and the pictures of a
//! document, decoded once each and counted.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;

use crate::archive::Archive;
use crate::decode;

/// A bigger file is refused before it is read.
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The largest XML part that is unpacked.
pub const MAX_PART: usize = 64 * 1024 * 1024;
/// The largest picture that is unpacked from the archive.
const MAX_MEDIA: usize = 32 * 1024 * 1024;
/// Pictures of a document that are decoded at most, and their pixels all together.
const MAX_IMAGES: usize = 64;
const MAX_IMAGE_PIXELS: u64 = 60_000_000;
/// One picture of a document may have this many pixels at most (a photo of 20 MP is 80 MB once decoded).
const MAX_ONE_IMAGE: u64 = 40_000_000;

/// The pictures of one document.
pub struct Pictures<'a> {
    archive: &'a Archive<'a>,
    /// The pictures decoded (`None` for one that could not be), by the path in the archive.
    images: RefCell<HashMap<String, Option<Arc<RgbaImage>>>>,
    pixels: Cell<u64>,
}

impl<'a> Pictures<'a> {
    pub fn new(archive: &'a Archive<'a>) -> Self {
        Self {
            archive,
            images: RefCell::new(HashMap::new()),
            pixels: Cell::new(0),
        }
    }

    /// The picture at `path` of the archive, decoded once; `None` when it cannot be (a format that is not read, a damaged
    /// file, too many pictures).
    pub fn get(&self, path: &str) -> Option<Arc<RgbaImage>> {
        if let Some(found) = self.images.borrow().get(path) {
            return found.clone();
        }
        let made = (self.images.borrow().len() < MAX_IMAGES && self.pixels.get() < MAX_IMAGE_PIXELS)
            .then(|| self.archive.read(path, MAX_MEDIA).ok())
            .flatten()
            .and_then(|bytes| {
                let left = MAX_IMAGE_PIXELS.saturating_sub(self.pixels.get());
                decode::decode_bytes(&bytes, left.min(MAX_ONE_IMAGE)).ok()
            })
            .map(|image| {
                self.pixels
                    .set(self.pixels.get() + u64::from(image.width()) * u64::from(image.height()));
                Arc::new(image)
            });
        self.images.borrow_mut().insert(path.to_string(), made.clone());
        made
    }
}
