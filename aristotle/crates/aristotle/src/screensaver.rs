// SPDX-License-Identifier: AGPL-3.0-or-later
//! The screensaver's own pictures: the folder on the Kindle's root and
//! the pictures the app shows.
//!
//! The pictures live in `screensavers/` next to the `documents/` folder
//! the books are in, so the reader can put them there over USB. The
//! folder is read on demand: the screensaver is the only place that
//! needs it, and the picker page is opened rarely.
//!
//! The decoding and the cache are the library's
//! (`aristotle_library::screensaver`); this module only says where the
//! folder is and turns the library's pictures into the ones the screens
//! draw.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aristotle_library::screensaver::{self, Catalog, PictureCache};
use aristotle_ui::widgets::GrayImage;

pub use aristotle_library::screensaver::Picture;

/// The folder on the Kindle's root that holds the screensaver's
/// pictures.
///
/// On the device the books are in `/mnt/us/documents` and the pictures
/// in `/mnt/us/screensavers`.
pub fn dir_for(root: &Path) -> PathBuf {
    root.parent().unwrap_or(root).join(screensaver::DIR_NAME)
}

/// Validated screensaver sources and their content-keyed disk cache.
pub struct Pictures {
    cache: PictureCache,
    catalog: Catalog,
    loaded: Vec<Loaded>,
}

struct Loaded {
    path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
    size: (u32, u32),
    image: GrayImage,
}

impl Pictures {
    pub fn new(dir: PathBuf) -> Pictures {
        Pictures {
            cache: PictureCache::new(dir),
            catalog: Catalog::default(),
            loaded: Vec::new(),
        }
    }

    /// Revalidates sources after USB access or reopening the picker, including
    /// same-size replacements whose FAT modification time did not advance.
    pub fn invalidate(&mut self) {
        self.catalog = Catalog::default();
        self.loaded.clear();
    }

    /// Lists pictures, validating only new or modified source files.
    pub fn list(&mut self, dir: &Path) -> Vec<Picture> {
        self.catalog.list(dir)
    }

    /// The thumbnail of `picture` for the picker page.
    pub fn thumb(&mut self, picture: &Picture) -> Option<GrayImage> {
        self.at(picture, screensaver::THUMB)
    }

    /// The picture of `picture` at the size of the screen.
    pub fn screen(&mut self, picture: &Picture) -> Option<GrayImage> {
        self.at(picture, screensaver::SCREEN)
    }

    fn at(&mut self, picture: &Picture, size: (u32, u32)) -> Option<GrayImage> {
        let metadata = std::fs::metadata(&picture.path).ok()?;
        if !metadata.is_file() || metadata.len() > screensaver::MAX_BYTES {
            return None;
        }
        let modified = metadata.modified().ok();
        if let Some(loaded) = self.loaded.iter().find(|loaded| {
            modified.is_some()
                && loaded.path == picture.path
                && loaded.size == size
                && loaded.bytes == metadata.len()
                && loaded.modified == modified
        }) {
            return Some(loaded.image.clone());
        }
        let bytes = screensaver::read_picture(&picture.path).ok()?;
        let gray = self.cache.get(&bytes, size).ok()?;
        let image = GrayImage::new(gray.width, gray.height, gray.pixels);
        // Keep at most ten images, including only one full-screen picture.
        self.loaded.retain(|loaded| {
            !(loaded.path == picture.path && loaded.size == size
                || size == screensaver::SCREEN && loaded.size == size)
        });
        if self.loaded.len() == 10 {
            self.loaded.remove(0);
        }
        self.loaded.push(Loaded {
            path: picture.path.clone(),
            bytes: metadata.len(),
            modified,
            size,
            image: image.clone(),
        });
        Some(image)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_gray(path: &Path, value: u8) {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[value; 4])
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn the_folder_is_next_to_the_documents() {
        // On the device the books are in /mnt/us/documents and the
        // pictures in /mnt/us/screensavers.
        let dir = dir_for(Path::new("/mnt/us/documents"));
        assert_eq!(dir, PathBuf::from("/mnt/us/screensavers"));
        // A root without a parent is its own parent.
        let dir = dir_for(Path::new("/"));
        assert_eq!(dir, PathBuf::from("/screensavers"));
    }

    #[test]
    fn a_folder_that_is_not_there_holds_no_pictures() {
        let mut pictures = Pictures::new(crate::testdir("sleep-pictures-missing"));
        assert!(pictures.list(Path::new("/nowhere/screensavers")).is_empty());
    }

    #[test]
    fn replacing_the_same_name_updates_both_cached_sizes() {
        let dir = crate::testdir("sleep-pictures-replaced");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        write_gray(&path, 0);
        let mut pictures = Pictures::new(dir.join("cache"));
        let picture = pictures.list(&dir).remove(0);
        assert!(pictures
            .thumb(&picture)
            .unwrap()
            .pixels
            .iter()
            .all(|p| *p == 0));
        assert!(pictures
            .screen(&picture)
            .unwrap()
            .pixels
            .iter()
            .all(|p| *p == 0));
        write_gray(&path, 255);
        assert!(pictures
            .thumb(&picture)
            .unwrap()
            .pixels
            .iter()
            .all(|p| *p == 255));
        assert!(pictures
            .screen(&picture)
            .unwrap()
            .pixels
            .iter()
            .all(|p| *p == 255));
        std::fs::remove_file(path).unwrap();
        assert!(pictures.screen(&picture).is_none());
    }

    #[test]
    fn invalidation_detects_replacements_with_preserved_metadata() {
        let dir = crate::testdir("sleep-pictures-preserved-time");
        let path = dir.join("a.png");
        write_gray(&path, 0);
        let metadata = std::fs::metadata(&path).unwrap();
        let mut pictures = Pictures::new(dir.join("cache"));
        let picture = pictures.list(&dir).remove(0);
        pictures.thumb(&picture).unwrap();
        write_gray(&path, 255);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(metadata.modified().unwrap())
            .unwrap();
        pictures.invalidate();
        assert!(pictures
            .thumb(&picture)
            .unwrap()
            .pixels
            .iter()
            .all(|p| *p == 255));
    }

    #[test]
    fn only_one_full_screen_picture_and_one_page_stay_loaded() {
        let dir = crate::testdir("sleep-pictures-bounded");
        for i in 0..12 {
            write_gray(&dir.join(format!("{i:02}.png")), i * 17);
        }
        let mut pictures = Pictures::new(dir.join("cache"));
        for picture in pictures.list(&dir) {
            pictures.thumb(&picture).unwrap();
            pictures.screen(&picture).unwrap();
        }
        assert!(pictures.loaded.len() <= 10);
        assert_eq!(
            pictures
                .loaded
                .iter()
                .filter(|p| p.size == screensaver::SCREEN)
                .count(),
            1
        );
    }
}
