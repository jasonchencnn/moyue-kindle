// SPDX-License-Identifier: AGPL-3.0-or-later
//! The screensaver's own pictures: the `screensavers/` folder on the
//! Kindle's root.
//!
//! The reader puts the pictures there (over USB, or through the Wi-Fi
//! transfer page) and picks one in the settings. The screensaver then
//! shows it, or rotates through the folder when "all pictures" is picked.
//!
//! This module is the pure part of that: which files count, in which
//! order they come, how they are put on the pages of the picker, and
//! which one the screensaver shows this time. Nothing here draws or
//! reads the screen; the app does that with what this module decides.
//!
//! The rules come from `docs/ui/prototype-v2/mcode/DESIGN-SLEEP-IMAGES.md`
//! (the section the product owner confirmed):
//!
//! - Only `.jpg`, `.jpeg` and `.png`, whatever the case of the letters.
//! - The pictures are sorted by file name, byte by byte, so the order
//!   does not change with the locale.
//! - A picture bigger than 4000 × 6000 pixels or than 5 MB, and one that
//!   cannot be decoded, is left out of the list and of the rotation.
//! - Page one holds the "all pictures" cell and eight pictures; the
//!   other pages hold nine pictures each.
//! - The rotation follows the file names. A picture that was deleted
//!   falls back to the rotation; an empty folder falls back to the last
//!   screen.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cover::{self, Gray};
use crate::error::{Error, Result};

/// The folder on the Kindle's root that holds the screensaver's
/// pictures.
pub const DIR_NAME: &str = "screensavers";

/// The size of the thumbnails of the picker page: 3:4, like the cells.
pub const THUMB: (u32, u32) = (270, 360);
/// The size of the screensaver's picture: the whole screen.
pub const SCREEN: (u32, u32) = (1264, 1680);

/// The file names the screensaver takes: JPEG and PNG, in any case.
pub const EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// A picture wider than this is left out: decoding it costs more memory
/// than the device should spend on a thumbnail.
pub const MAX_WIDTH: u32 = 4000;
/// A picture taller than this is left out, for the same reason.
pub const MAX_HEIGHT: u32 = 6000;
/// A picture bigger than this is left out: the file itself is too heavy
/// to read on the device.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// How many pictures the first page of the picker holds. The "all
/// pictures" cell takes the place of one of them.
pub const FIRST_PAGE_PICTURES: usize = 8;
/// How many pictures the other pages of the picker hold.
pub const PAGE_PICTURES: usize = 9;

/// The name of a file in the screensaver's folder, with the size and the
/// pixel size of the picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    /// The file name, e.g. "sunset.jpg".
    pub name: String,
    /// The full path of the file.
    pub path: PathBuf,
    /// The size of the file in bytes.
    pub bytes: u64,
    /// The width of the picture in pixels.
    pub width: u32,
    /// The height of the picture in pixels.
    pub height: u32,
}

/// Whether the file name is one the screensaver takes: a JPEG or a PNG,
/// whatever the case of the letters.
pub fn is_picture_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    EXTENSIONS.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

/// Whether a picture is too big to be used: too many pixels, or a file
/// too heavy to read.
pub fn is_oversized(width: u32, height: u32, bytes: u64) -> bool {
    width > MAX_WIDTH || height > MAX_HEIGHT || bytes > MAX_BYTES
}

/// The pictures of `dir`, sorted by file name byte by byte.
///
/// The files that are too big are left out, and so are the ones that
/// cannot be decoded: the reader must not be able to pick a picture
/// that cannot be shown. A folder that is not there holds no pictures.
pub fn list(dir: &Path) -> Vec<Picture> {
    Catalog::default().list(dir)
}

struct Inspected {
    bytes: u64,
    modified: Option<SystemTime>,
    picture: Option<Picture>,
}

/// Validated directory entries, refreshed when a file's size or time changes.
/// Each scan still checks metadata so additions and deletions are visible.
#[derive(Default)]
pub struct Catalog {
    entries: HashMap<PathBuf, Inspected>,
}

impl Catalog {
    /// Lists usable pictures without decoding unchanged files again.
    pub fn list(&mut self, dir: &Path) -> Vec<Picture> {
        let mut previous = std::mem::take(&mut self.entries);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !is_picture_name(name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_BYTES {
                continue;
            }
            let modified = meta.modified().ok();
            let inspected = previous.remove(&path).filter(|cached| {
                modified.is_some() && cached.modified == modified && cached.bytes == meta.len()
            });
            let inspected = inspected.unwrap_or_else(|| Inspected {
                bytes: meta.len(),
                modified,
                picture: inspect(&path, meta.len()),
            });
            if let Some(picture) = &inspected.picture {
                out.push(picture.clone());
            }
            self.entries.insert(path, inspected);
        }
        out.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        out
    }
}

fn inspect(path: &Path, bytes: u64) -> Option<Picture> {
    let data = read_picture(path).ok()?;
    let (width, height) = picture_size(&data).ok()?;
    if is_oversized(width, height, data.len() as u64) || width == 0 || height == 0 {
        return None;
    }
    cover::decode_gray(&data, THUMB.0).ok()?;
    Some(Picture {
        name: path.file_name()?.to_str()?.to_string(),
        path: path.to_path_buf(),
        bytes,
        width,
        height,
    })
}

/// Reads one source with the same byte limit used by the picker.
pub fn read_picture(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::format("screensaver: file too large"));
    }
    Ok(bytes)
}

/// The pixel size of a JPEG or a PNG, read from its header alone: the
/// picture is not decoded, so this is cheap even for a large file.
pub fn picture_size(data: &[u8]) -> Result<(u32, u32)> {
    match crate::cover::sniff_image(data) {
        Some(crate::cover::ImageKind::Jpeg) => jpeg_size(data),
        Some(crate::cover::ImageKind::Png) => png_size(data),
        Some(k) => Err(Error::format(format!(
            "screensaver: {k:?} is not supported"
        ))),
        None => Err(Error::format("screensaver: not an image")),
    }
}

/// The pixel size of a JPEG, from its start-of-frame marker.
fn jpeg_size(data: &[u8]) -> Result<(u32, u32)> {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        // The start-of-frame markers, except the ones that are not
        // frames (DHT, JPG and DAC).
        let is_frame = matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        );
        if is_frame {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Ok((w, h));
        }
        // The markers without a payload.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if len < 2 {
            return Err(Error::format("screensaver: broken jpeg header"));
        }
        i += 2 + len;
    }
    Err(Error::format("screensaver: no frame in the jpeg"))
}

/// The pixel size of a PNG, from its IHDR chunk.
fn png_size(data: &[u8]) -> Result<(u32, u32)> {
    if data.len() < 24 || &data[12..16] != b"IHDR" {
        return Err(Error::format("screensaver: broken png header"));
    }
    let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Ok((w, h))
}

/// The pictures of one page of the picker.
///
/// The first page holds the "all pictures" cell and eight pictures; the
/// other pages hold nine pictures each. `page` counts from zero.
pub fn page_pictures(pictures: &[Picture], page: usize) -> &[Picture] {
    if pictures.is_empty() {
        return &[];
    }
    if page == 0 {
        let end = FIRST_PAGE_PICTURES.min(pictures.len());
        return &pictures[..end];
    }
    // The first page took the first eight pictures; the rest follow.
    let start = page_offset(page);
    if start >= pictures.len() {
        return &[];
    }
    let end = (start + PAGE_PICTURES).min(pictures.len());
    &pictures[start..end]
}

/// Where the page of the picker starts in the list of the pictures.
///
/// The first page holds the first eight pictures, the other pages nine
/// each. The number tells where the i-th picture of a page sits in the
/// whole list.
pub fn page_offset(page: usize) -> usize {
    if page == 0 {
        0
    } else {
        FIRST_PAGE_PICTURES + (page - 1) * PAGE_PICTURES
    }
}

/// How many pages the picker shows for `count` pictures.
///
/// The first page holds eight; the other pages hold nine.
pub fn page_count(count: usize) -> usize {
    if count == 0 {
        return 1;
    }
    if count <= FIRST_PAGE_PICTURES {
        return 1;
    }
    let rest = count - FIRST_PAGE_PICTURES;
    1 + rest.div_ceil(PAGE_PICTURES)
}

/// What the screensaver shows this time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// One picture, the one the reader picked.
    One(Picture),
    /// The next picture of the rotation.
    Next(Picture),
    /// Nothing to show: the folder is empty. The screensaver keeps the
    /// last screen.
    None,
}

/// Picks the picture the screensaver shows this time.
///
/// `picked` is the file name the reader chose, or `None` for the
/// rotation. `turn` is the position of the rotation, which moves on by
/// one for every picture of the rotation.
///
/// The rules: a picture that was picked and is still there is shown;
/// a picture that was picked and is gone falls back to the rotation;
/// an empty folder shows nothing.
pub fn choose(pictures: &[Picture], picked: Option<&str>, turn: usize) -> Choice {
    if pictures.is_empty() {
        return Choice::None;
    }
    if let Some(name) = picked {
        if let Some(p) = pictures.iter().find(|p| p.name == name) {
            return Choice::One(p.clone());
        }
        // The picture was deleted: the rotation takes over.
    }
    let index = turn % pictures.len();
    Choice::Next(pictures[index].clone())
}

/// The picture at `size`: decoded, cropped in the middle to the shape
/// of the size and dithered to the 16 gray levels the panel shows.
///
/// The picture covers the size and the overflow is cropped away in the
/// middle, like the prototype's `object-fit: cover`.
pub fn make_picture(bytes: &[u8], size: (u32, u32)) -> Result<Gray> {
    let source_size = picture_size(bytes)?;
    if is_oversized(source_size.0, source_size.1, bytes.len() as u64)
        || source_size.0 == 0
        || source_size.1 == 0
        || size.0 == 0
        || size.1 == 0
        || is_oversized(size.0, size.1, 0)
    {
        return Err(Error::format("screensaver: invalid picture size"));
    }
    let src = cover::decode_gray(bytes, size.0)?;
    // Crop before resizing so a narrow source never allocates a huge scaled image.
    let (width, height) =
        if u64::from(src.width) * u64::from(size.1) > u64::from(src.height) * u64::from(size.0) {
            (
                ((u64::from(src.height) * u64::from(size.0) / u64::from(size.1)) as u32).max(1),
                src.height,
            )
        } else {
            (
                src.width,
                ((u64::from(src.width) * u64::from(size.1) / u64::from(size.0)) as u32).max(1),
            )
        };
    let ox = (src.width - width) / 2;
    let oy = (src.height - height) / 2;
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in oy..oy + height {
        let start = (row * src.width + ox) as usize;
        pixels.extend_from_slice(&src.pixels[start..start + width as usize]);
    }
    let cropped = Gray {
        width,
        height,
        pixels,
    };
    let resized = cover::resize(&cropped, size.0, size.1);
    let mut out = Gray {
        width: size.0,
        height: size.1,
        pixels: resized.pixels,
    };
    cover::dither16(&mut out);
    Ok(out)
}

/// The cache of the screensaver's pictures.
///
/// The way the cover cache works: the key is the MD5 of the file's
/// bytes, the name carries the size, and the picture is kept as a
/// binary PGM. A picture that was decoded once is not decoded again,
/// and a picture that was changed gets a new file.
pub struct PictureCache {
    dir: PathBuf,
}

impl PictureCache {
    pub fn new(dir: PathBuf) -> PictureCache {
        PictureCache { dir }
    }

    /// The content key of a picture: the MD5 of its bytes.
    pub fn key(bytes: &[u8]) -> String {
        crate::id::md5_hex(bytes)
    }

    /// Where the picture of `key` at `size` is cached. The size is part
    /// of the file name, so changing the geometry never serves a stale
    /// file.
    pub fn path_for(&self, key: &str, size: (u32, u32)) -> PathBuf {
        self.dir.join(format!("{key}-{}x{}.pgm", size.0, size.1))
    }

    /// The picture of `key` at `size`, if it is in the cache.
    pub fn load(&self, key: &str, size: (u32, u32)) -> Option<Gray> {
        if size.0 == 0 || size.1 == 0 || is_oversized(size.0, size.1, 0) {
            return None;
        }
        let mut data = Vec::new();
        std::fs::File::open(self.path_for(key, size))
            .ok()?
            .take(u64::from(size.0) * u64::from(size.1) + 65)
            .read_to_end(&mut data)
            .ok()?;
        let header = format!("P5\n{} {}\n255\n", size.0, size.1);
        let pixels = data.strip_prefix(header.as_bytes())?;
        if pixels.len() != size.0 as usize * size.1 as usize {
            return None;
        }
        Some(Gray {
            width: size.0,
            height: size.1,
            pixels: pixels.to_vec(),
        })
    }

    /// The picture of `bytes` at `size`, made from them when the cache
    /// does not have it yet.
    pub fn get(&self, bytes: &[u8], size: (u32, u32)) -> Result<Gray> {
        let key = Self::key(bytes);
        if let Some(g) = self.load(&key, size) {
            return Ok(g);
        }
        let picture = make_picture(bytes, size)?;
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = cover::write_pgm(&self.path_for(&key, size), &picture);
        Ok(picture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cover::testutil::gradient_png;

    fn dir(name: &str) -> PathBuf {
        crate::testdir(name)
    }

    fn write(dir: &Path, name: &str, data: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), data).unwrap();
    }

    fn picture(name: &str) -> Picture {
        Picture {
            name: name.to_string(),
            path: PathBuf::from(name),
            bytes: 10,
            width: 100,
            height: 200,
        }
    }

    #[test]
    fn only_jpegs_and_pngs_count() {
        assert!(is_picture_name("a.jpg"));
        assert!(is_picture_name("a.JPG"));
        assert!(is_picture_name("a.jpeg"));
        assert!(is_picture_name("a.JPEG"));
        assert!(is_picture_name("a.png"));
        assert!(is_picture_name("a.PNG"));
        assert!(!is_picture_name("a.gif"));
        assert!(!is_picture_name("a.webp"));
        assert!(!is_picture_name("a.txt"));
        assert!(!is_picture_name("jpg"));
        assert!(!is_picture_name(""));
    }

    #[test]
    fn the_folder_is_listed_by_file_name() {
        let d = dir("screensaver-list");
        write(&d, "b.jpg", &gradient_png(20, 30));
        write(&d, "A.jpg", &gradient_png(20, 30));
        write(&d, "c.png", &gradient_png(20, 30));
        write(&d, "notes.txt", b"not a picture");
        write(&d, "d.gif", b"GIF89a not a picture");
        let list = list(&d);
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        // Byte order, not the locale's: the capital comes first.
        assert_eq!(names, ["A.jpg", "b.jpg", "c.png"]);
        assert_eq!((list[0].width, list[0].height), (20, 30));
    }

    #[test]
    fn a_folder_that_is_not_there_holds_no_pictures() {
        assert!(list(&dir("screensaver-missing").join("nowhere")).is_empty());
    }

    #[test]
    fn broken_and_oversized_pictures_are_left_out() {
        let d = dir("screensaver-broken");
        write(&d, "good.png", &gradient_png(20, 30));
        // A PNG header followed by rubbish.
        let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
        broken.extend_from_slice(&[0u8; 64]);
        write(&d, "broken.png", &broken);
        // Not an image at all.
        write(&d, "text.jpg", b"this is not a picture");
        // Too many pixels: the header says 5000 × 7000.
        let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
        big.extend_from_slice(&(13u32).to_be_bytes());
        big.extend_from_slice(b"IHDR");
        big.extend_from_slice(&5000u32.to_be_bytes());
        big.extend_from_slice(&7000u32.to_be_bytes());
        big.extend_from_slice(&[8, 0, 0, 0, 0]);
        write(&d, "big.png", &big);
        let names: Vec<String> = list(&d).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["good.png"]);
    }

    #[test]
    fn a_picture_that_is_too_big_is_left_out() {
        // The limit is "bigger than", so the size itself is still fine.
        assert!(!is_oversized(MAX_WIDTH, MAX_HEIGHT, MAX_BYTES));
        assert!(is_oversized(MAX_WIDTH + 1, 100, 10));
        assert!(is_oversized(100, MAX_HEIGHT + 1, 10));
        assert!(is_oversized(100, 100, MAX_BYTES + 1));
    }

    #[test]
    fn the_first_page_holds_the_rotation_cell_and_eight_pictures() {
        let pictures: Vec<Picture> = (0..20).map(|i| picture(&format!("{i:02}.jpg"))).collect();
        let first = page_pictures(&pictures, 0);
        assert_eq!(first.len(), 8);
        assert_eq!(first[0].name, "00.jpg");
        let second = page_pictures(&pictures, 1);
        assert_eq!(second.len(), 9);
        assert_eq!(second[0].name, "08.jpg");
        let third = page_pictures(&pictures, 2);
        assert_eq!(third.len(), 3);
        assert_eq!(third[0].name, "17.jpg");
        // Past the end there is nothing.
        assert!(page_pictures(&pictures, 3).is_empty());
    }

    #[test]
    fn a_short_list_fits_on_one_page() {
        let pictures: Vec<Picture> = (0..5).map(|i| picture(&format!("{i}.jpg"))).collect();
        assert_eq!(page_count(0), 1);
        assert_eq!(page_count(1), 1);
        assert_eq!(page_count(8), 1);
        assert_eq!(page_count(9), 2);
        assert_eq!(page_count(17), 2);
        assert_eq!(page_count(18), 3);
        assert_eq!(page_count(26), 3);
        assert_eq!(page_count(27), 4);
        assert_eq!(page_pictures(&pictures, 0).len(), 5);
        assert!(page_pictures(&pictures, 1).is_empty());
    }

    #[test]
    fn a_picked_picture_is_shown() {
        let pictures = vec![picture("a.jpg"), picture("b.jpg")];
        match choose(&pictures, Some("b.jpg"), 0) {
            Choice::One(p) => assert_eq!(p.name, "b.jpg"),
            other => panic!("expected the picked picture, got {other:?}"),
        }
    }

    #[test]
    fn the_rotation_moves_on_by_one() {
        let pictures = vec![picture("a.jpg"), picture("b.jpg"), picture("c.jpg")];
        match choose(&pictures, None, 0) {
            Choice::Next(p) => assert_eq!(p.name, "a.jpg"),
            other => panic!("expected the rotation, got {other:?}"),
        }
        match choose(&pictures, None, 1) {
            Choice::Next(p) => assert_eq!(p.name, "b.jpg"),
            other => panic!("expected the rotation, got {other:?}"),
        }
        // The turn is taken modulo the number of pictures, so a turn
        // from an older, longer list still lands on a picture.
        match choose(&pictures, None, 4) {
            Choice::Next(p) => assert_eq!(p.name, "b.jpg"),
            other => panic!("expected the rotation, got {other:?}"),
        }
    }

    #[test]
    fn a_picture_that_was_deleted_falls_back_to_the_rotation() {
        let pictures = vec![picture("a.jpg"), picture("b.jpg")];
        match choose(&pictures, Some("gone.jpg"), 1) {
            Choice::Next(p) => assert_eq!(p.name, "b.jpg"),
            other => panic!("expected the rotation, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_folder_shows_nothing() {
        assert_eq!(choose(&[], Some("a.jpg"), 0), Choice::None);
        assert_eq!(choose(&[], None, 0), Choice::None);
    }

    #[test]
    fn the_pixel_size_is_read_from_the_header() {
        let png = gradient_png(37, 91);
        assert_eq!(picture_size(&png).unwrap(), (37, 91));
        assert!(picture_size(b"not an image").is_err());
    }

    #[test]
    fn a_picture_is_cut_to_the_size_and_cached() {
        let png = gradient_png(80, 60);
        let picture = make_picture(&png, SCREEN).unwrap();
        assert_eq!((picture.width, picture.height), SCREEN);
        // The panel shows 16 levels: every value is a multiple of 17.
        assert!(picture.pixels.iter().all(|p| p % 17 == 0));

        let cache = PictureCache::new(dir("screensaver-cache").join("cache"));
        let first = cache.get(&png, THUMB).unwrap();
        assert_eq!((first.width, first.height), THUMB);
        // The second time it comes from the cache on disk.
        let second = cache.get(&png, THUMB).unwrap();
        assert_eq!(first, second);
        let key = PictureCache::key(&png);
        assert!(cache.path_for(&key, THUMB).exists(), "the cache is written");
        // The two sizes live in two files.
        assert_ne!(
            cache.path_for(&key, THUMB),
            cache.path_for(&key, SCREEN),
            "the sizes do not share a file"
        );
    }

    #[test]
    fn a_picture_that_cannot_be_read_is_not_cached() {
        let cache = PictureCache::new(dir("screensaver-cache-bad").join("cache"));
        assert!(cache.get(b"not an image", THUMB).is_err());
    }

    #[test]
    fn a_valid_header_with_truncated_pixels_is_left_out() {
        let d = dir("screensaver-truncated");
        let png = gradient_png(20, 30);
        write(&d, "good.png", &png);
        write(&d, "truncated.png", &png[..33]);
        let names: Vec<_> = list(&d).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["good.png"]);
    }

    #[test]
    fn a_cache_file_with_the_wrong_size_is_rebuilt() {
        let png = gradient_png(20, 30);
        let cache = PictureCache::new(dir("screensaver-cache-size"));
        std::fs::create_dir_all(&cache.dir).unwrap();
        let path = cache.path_for(&PictureCache::key(&png), THUMB);
        cover::write_pgm(
            &path,
            &Gray {
                width: 1,
                height: 1,
                pixels: vec![0],
            },
        )
        .unwrap();
        let image = cache.get(&png, THUMB).unwrap();
        assert_eq!((image.width, image.height), THUMB);
    }

    #[test]
    fn extreme_aspect_ratios_stay_within_the_output_size() {
        for (width, height) in [(1, MAX_HEIGHT), (MAX_WIDTH, 1)] {
            let png = gradient_png(width, height);
            let image = make_picture(&png, SCREEN).unwrap();
            assert_eq!((image.width, image.height), SCREEN);
            assert_eq!(image.pixels.len(), (SCREEN.0 * SCREEN.1) as usize);
            assert!(image.pixels.iter().all(|pixel| pixel % 17 == 0));
        }
        assert!(make_picture(&gradient_png(2, 2), (0, 1)).is_err());
    }

    #[test]
    fn the_catalog_refreshes_changed_added_and_deleted_sources() {
        let d = dir("screensaver-catalog");
        let mut catalog = Catalog::default();
        assert!(catalog.list(&d).is_empty());
        write(&d, "a.png", &gradient_png(20, 30));
        assert_eq!(catalog.list(&d)[0].width, 20);
        assert_eq!(catalog.list(&d)[0].width, 20);
        write(&d, "a.png", b"broken");
        assert!(catalog.list(&d).is_empty());
        write(&d, "b.png", &gradient_png(30, 20));
        assert_eq!(catalog.list(&d)[0].name, "b.png");
        std::fs::remove_file(d.join("b.png")).unwrap();
        assert!(catalog.list(&d).is_empty());
        assert_eq!(catalog.entries.len(), 1);
    }

    #[test]
    fn corrupt_cache_headers_are_ignored_without_parsing_their_dimensions() {
        let png = gradient_png(20, 30);
        let cache = PictureCache::new(dir("screensaver-cache-corrupt"));
        std::fs::create_dir_all(&cache.dir).unwrap();
        let path = cache.path_for(&PictureCache::key(&png), THUMB);
        std::fs::write(path, b"P5\n4294967295 4294967295\n255\n").unwrap();
        let image = cache.get(&png, THUMB).unwrap();
        assert_eq!((image.width, image.height), THUMB);
    }
}
