// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deleting a book's local file, and the caches that belong to it.
//!
//! The file goes, the reading state stays: the index keeps the book in
//! its list of removed books, with its progress, its highlights and its
//! notes, so a book that comes back over USB resumes where it was left.
//! What goes with the file are the caches: they hold nothing but copies
//! of what the file itself holds.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use aristotle_library::cover::ThumbSpec;
use aristotle_library::index::BookRecord;
use aristotle_library::library::Library;
use aristotle_render::plato_core::anyhow::{format_err, Context, Error};

use crate::pagecache;
use crate::snapshot;

/// The crengine document cache, under the library's data directory.
const CRENGINE_CACHE: &str = "crengine-cache";

/// What was deleted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Deleted {
    /// The book file itself.
    pub file: bool,
    /// The cached cover.
    pub cover: bool,
    /// The cached page numbers.
    pub pages: usize,
    /// The cached snapshot of the last page.
    pub snapshot: bool,
    /// The EPUB a MOBI or TXT book was converted to.
    pub converted: usize,
    /// The crengine document cache.
    pub crengine: usize,
}

/// Deletes the file of `rec` and the caches that belong to it.
///
/// The file must be gone afterwards; the caches are best effort, for a
/// cache that cannot be deleted is only wasted space.
pub fn delete(lib: &Library, rec: &BookRecord, mobi_cache: &Path) -> Result<Deleted, Error> {
    let mut out = Deleted::default();
    let path = lib
        .absolute_path(rec)
        .ok_or_else(|| format_err!("the library has no root"))?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("can't delete {}", path.display()))?;
        out.file = true;
    }
    // Both thumbnail sizes: the shelf's and the screensaver's.
    for spec in [ThumbSpec::default(), ThumbSpec::large()] {
        if let Some(cover) = lib.cover_path_sized(rec, spec) {
            out.cover |= fs::remove_file(&cover).is_ok();
        }
    }
    let data = lib.data_dir();
    out.snapshot = fs::remove_file(snapshot::path(data, &rec.id)).is_ok();
    out.pages = remove_matching(
        &data.join(pagecache::PAGES_DIR),
        &format!("{}-", rec.id),
        ".txt",
    );
    // The EPUB a MOBI or TXT book was converted to, and the map of its
    // positions.
    let stem = cache_stem(&path);
    out.converted = remove_converted(mobi_cache, &stem);
    // The crengine document cache, whose file names carry the CRC of the
    // book file.
    if let Some(crc) = crc32_of(&path) {
        out.crengine = remove_matching(&data.join(CRENGINE_CACHE), &format!(".{crc:08x}."), ".cr3");
    }
    Ok(out)
}

/// Deletes the copies a MOBI or TXT book was converted to. The converter
/// names them `{stem}-{hash}.epub` (and `.posmap`); a file whose name leaves
/// no stem at all falls back to `book` for TXT and to an empty stem for
/// MOBI, so both names are tried then.
fn remove_converted(dir: &Path, stem: &str) -> usize {
    if stem.is_empty() {
        remove_matching(dir, "book-", ".epub")
            + remove_matching(dir, "book-", ".posmap")
            + remove_matching(dir, "-", ".epub")
            + remove_matching(dir, "-", ".posmap")
    } else {
        let prefix = format!("{stem}-");
        remove_matching(dir, &prefix, ".epub") + remove_matching(dir, &prefix, ".posmap")
    }
}

/// Deletes the files in `dir` whose name starts with `prefix` and ends with
/// `suffix`, and returns how many went.
fn remove_matching(dir: &Path, prefix: &str, suffix: &str) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut gone = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix)
            && name.ends_with(suffix)
            && fs::remove_file(entry.path()).is_ok()
        {
            gone += 1;
        }
    }
    gone
}

/// The name the converted EPUB of a MOBI or TXT book gets: the stem of the
/// file, without the characters the cache does not use.
fn cache_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(40)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The CRC-32 of the file, as zlib computes it: the crengine cache
/// names its files after it.
fn crc32_of(path: &Path) -> Option<u32> {
    let mut file = fs::File::open(path).ok()?;
    let table = crc_table();
    let mut crc = 0xffff_ffffu32;
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            crc = (crc >> 8) ^ table[((crc ^ u32::from(b)) & 0xff) as usize];
        }
    }
    Some(!crc)
}

/// The CRC-32 table, built once.
fn crc_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let mut crc = i as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
            *entry = crc;
        }
        table
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_zlib() {
        // The CRC-32 of "123456789", the value every CRC-32 test uses.
        let dir = std::env::temp_dir().join(format!("aristotle-crc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.epub");
        std::fs::write(&path, b"123456789").unwrap();
        assert_eq!(crc32_of(&path), Some(0xcbf4_3926));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_stem_is_cleaned_like_the_cache() {
        assert_eq!(cache_stem(Path::new("/books/红楼梦.mobi")), "红楼梦");
        assert_eq!(cache_stem(Path::new("/books/a b!.txt")), "a b");
        // A name that leaves nothing falls back to the converters' names.
        assert_eq!(cache_stem(Path::new("/books/《》.txt")), "");
    }

    #[test]
    fn only_this_books_converted_copies_go() {
        let dir = std::env::temp_dir().join(format!("aristotle-delete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = [
            "红楼梦-0123456789abcdef.epub",
            "红楼梦-0123456789abcdef.posmap",
            "红楼梦续-0123456789abcdef.epub",
            "book-0123456789abcdef.epub",
        ];
        for name in names {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        assert_eq!(remove_converted(&dir, "红楼梦"), 2);
        assert!(dir.join("红楼梦续-0123456789abcdef.epub").exists());
        assert!(dir.join("book-0123456789abcdef.epub").exists());
        // An empty stem takes the fallbacks the converters use and leaves
        // other books alone.
        std::fs::write(dir.join("-0123456789abcdef.epub"), b"x").unwrap();
        assert_eq!(remove_converted(&dir, ""), 2);
        assert!(!dir.join("book-0123456789abcdef.epub").exists());
        assert!(!dir.join("-0123456789abcdef.epub").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
