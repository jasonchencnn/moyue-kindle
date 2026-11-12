//! Local library: recursive book scan and bookshelf sorting.
//! Titles are file names for now; EPUB metadata is a later task.

use crate::hash::partial_md5;
use crate::progress::ProgressStore;
use std::cmp::Ordering;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Book container formats the engine layer can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookFormat {
    Epub,
    Fb2,
    Txt,
    Mobi,
    Azw3,
    Pdf,
}

impl BookFormat {
    /// Detects the format from the file name, case-insensitively.
    /// `.fb2.zip` is reported as [`BookFormat::Fb2`].
    ///
    /// File names are decoded with `to_string_lossy()`, so books copied onto
    /// FAT32 with non-UTF-8 (GBK/Latin-1) names are still recognised: the
    /// extension itself is ASCII, and replacement characters never form one.
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".fb2.zip") {
            return Some(Self::Fb2);
        }
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .as_deref()
            .unwrap_or_default()
        {
            "epub" => Some(Self::Epub),
            "fb2" => Some(Self::Fb2),
            "txt" => Some(Self::Txt),
            "mobi" => Some(Self::Mobi),
            "azw3" => Some(Self::Azw3),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

/// One book found by [`scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEntry {
    pub path: PathBuf,
    /// KOReader-compatible partial MD5, see [`crate::hash::partial_md5`].
    pub hash: String,
    pub format: BookFormat,
    /// File name without extension (`.fb2.zip` counts as one extension).
    pub title: String,
    pub size: u64,
    /// Unix timestamp in seconds; `0` when unavailable.
    pub modified: i64,
}

/// Shelf ordering. Ties fall back to case-insensitive title, then path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// Ascending by title.
    Title,
    /// Newest file modification first.
    Modified,
    /// Most recently read first, unread books last.
    RecentlyRead,
}

/// Recursively scans `root` for supported books.
///
/// Hidden entries (`.*`), Kindle `*.sdr` sidecar directories and unsupported
/// extensions are skipped. Files that fail to open or hash are skipped
/// individually; only an unreadable `root` itself is an error.
///
/// Symbolic links — to files and to directories alike — are skipped: following
/// them would allow directory loops on a FAT32 card and could scan outside
/// `root`. Books reachable only through a symlink are therefore not listed.
pub fn scan(root: &Path) -> io::Result<Vec<BookEntry>> {
    let mut books = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if dir.as_path() == root => return Err(e),
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if !name.to_ascii_lowercase().ends_with(".sdr") {
                    pending.push(path);
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            if let Some(book) = describe(&path, &metadata) {
                books.push(book);
            }
        }
    }
    Ok(books)
}

/// Sorts `entries` in place according to `key`; `progress` supplies
/// `updated_at` for [`SortKey::RecentlyRead`].
pub fn sort(entries: &mut [BookEntry], key: SortKey, progress: &ProgressStore) {
    match key {
        SortKey::Title => entries.sort_by(by_title),
        SortKey::Modified => {
            entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| by_title(a, b)))
        }
        SortKey::RecentlyRead => entries.sort_by(|a, b| {
            let read_at = |entry: &BookEntry| progress.get(&entry.hash).map(|p| p.updated_at);
            match (read_at(a), read_at(b)) {
                (Some(x), Some(y)) => y.cmp(&x).then_with(|| by_title(a, b)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => by_title(a, b),
            }
        }),
    }
}

fn by_title(a: &BookEntry, b: &BookEntry) -> Ordering {
    a.title
        .to_lowercase()
        .cmp(&b.title.to_lowercase())
        .then_with(|| a.path.cmp(&b.path))
}

/// Builds a [`BookEntry`], returning `None` when the file cannot be read.
fn describe(path: &Path, metadata: &fs::Metadata) -> Option<BookEntry> {
    let format = BookFormat::from_path(path)?;
    let mut file = File::open(path).ok()?;
    let hash = partial_md5(&mut file).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Some(BookEntry {
        path: path.to_path_buf(),
        hash,
        format,
        title: title_of(path),
        size: metadata.len(),
        modified,
    })
}

fn title_of(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.to_ascii_lowercase().ends_with(".fb2.zip") {
        return name[..name.len() - ".fb2.zip".len()].to_string();
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Progress;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "inkread-core-library-{tag}-{}-{nanos}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, contents).expect("write test file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn book(title: &str, hash: &str, modified: i64) -> BookEntry {
        BookEntry {
            path: PathBuf::from(format!("/books/{title}.epub")),
            hash: hash.to_string(),
            format: BookFormat::Epub,
            title: title.to_string(),
            size: 1,
            modified,
        }
    }

    fn titles(entries: &[BookEntry]) -> Vec<&str> {
        entries.iter().map(|b| b.title.as_str()).collect()
    }

    #[test]
    fn detects_formats_case_insensitively() {
        let cases = [
            ("a.epub", Some(BookFormat::Epub)),
            ("a.EPUB", Some(BookFormat::Epub)),
            ("a.Fb2", Some(BookFormat::Fb2)),
            ("a.fb2.zip", Some(BookFormat::Fb2)),
            ("a.FB2.ZIP", Some(BookFormat::Fb2)),
            ("a.txt", Some(BookFormat::Txt)),
            ("a.mobi", Some(BookFormat::Mobi)),
            ("a.azw3", Some(BookFormat::Azw3)),
            ("a.pdf", Some(BookFormat::Pdf)),
            ("a.azw", None),
            ("a.md", None),
            ("noext", None),
        ];
        for (name, expected) in cases {
            assert_eq!(BookFormat::from_path(Path::new(name)), expected, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_still_detect_format_and_keep_a_title() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // GBK-encoded "书.epub": 0xca 0xe9 are not valid UTF-8.
        let path = PathBuf::from(OsStr::from_bytes(&[
            0xca, 0xe9, b'.', b'e', b'p', b'u', b'b',
        ]));
        assert_eq!(BookFormat::from_path(&path), Some(BookFormat::Epub));
        assert_eq!(title_of(&path), "\u{FFFD}\u{FFFD}");
        assert!(!title_of(&path).is_empty());

        let zipped = PathBuf::from(OsStr::from_bytes(&[
            0xca, 0xe9, b'.', b'f', b'b', b'2', b'.', b'z', b'i', b'p',
        ]));
        assert_eq!(BookFormat::from_path(&zipped), Some(BookFormat::Fb2));
        assert_eq!(title_of(&zipped), "\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn scan_skips_hidden_sdr_and_unsupported() {
        let dir = TempDir::new("scan-skips");
        dir.write("visible.epub", b"epub");
        dir.write("nested/deep.PDF", b"pdf");
        dir.write(".hidden.epub", b"hidden");
        dir.write(".hidden-dir/book.epub", b"hidden dir");
        dir.write("book.sdr/content.epub", b"sdr");
        dir.write("book.sdr/metadata.epub.lua", b"lua");
        dir.write("notes.md", b"unsupported");
        dir.write("no-extension", b"none");

        let mut found = scan(&dir.path).unwrap();
        found.sort_by(by_title);
        assert_eq!(titles(&found), ["deep", "visible"]);
        assert_eq!(found[1].format, BookFormat::Epub);
        assert_eq!(found[0].format, BookFormat::Pdf);
    }

    #[test]
    fn scan_hashes_with_partial_md5_and_fills_metadata() {
        let dir = TempDir::new("scan-meta");
        let path = dir.write("hello.fb2.zip", b"hello");
        let entries = scan(&dir.path).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let expected = partial_md5(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(entry.hash, expected);
        assert_eq!(entry.size, 5);
        assert_eq!(entry.format, BookFormat::Fb2);
        assert_eq!(entry.title, "hello");
        assert_eq!(entry.path, path);
        let modified = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(entry.modified, modified);
    }

    #[test]
    fn scan_missing_root_is_error() {
        let dir = TempDir::new("scan-missing");
        let err = scan(&dir.path.join("nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn sorts_by_title_ascending() {
        let dir = TempDir::new("sort-title");
        let mut entries = vec![
            book("Zebra", "h1", 10),
            book("apple", "h2", 30),
            book("Banana", "h3", 20),
        ];
        let store = ProgressStore::load(dir.path.join("p.json")).unwrap();
        sort(&mut entries, SortKey::Title, &store);
        assert_eq!(titles(&entries), ["apple", "Banana", "Zebra"]);
    }

    #[test]
    fn sorts_by_modified_descending() {
        let dir = TempDir::new("sort-modified");
        let mut entries = vec![
            book("old", "h1", 10),
            book("new", "h2", 30),
            book("mid", "h3", 20),
        ];
        let store = ProgressStore::load(dir.path.join("p.json")).unwrap();
        sort(&mut entries, SortKey::Modified, &store);
        assert_eq!(titles(&entries), ["new", "mid", "old"]);
    }

    #[test]
    fn sorts_by_recently_read_with_unread_last() {
        let dir = TempDir::new("sort-recent");
        let mut entries = vec![
            book("Read Some Time Ago", "h1", 99),
            book("Never Opened", "h2", 99),
            book("Read Just Now", "h3", 99),
        ];
        let mut store = ProgressStore::load(dir.path.join("p.json")).unwrap();
        store.set(
            "h1",
            Progress {
                xpointer: None,
                page: 1,
                total: 2,
                updated_at: 100,
            },
        );
        store.set(
            "h3",
            Progress {
                xpointer: None,
                page: 2,
                total: 2,
                updated_at: 300,
            },
        );
        sort(&mut entries, SortKey::RecentlyRead, &store);
        assert_eq!(
            titles(&entries),
            ["Read Just Now", "Read Some Time Ago", "Never Opened"]
        );
    }

    #[test]
    fn unread_books_are_sorted_by_title() {
        let dir = TempDir::new("sort-unread");
        let mut entries = vec![book("Zulu", "h1", 1), book("alpha", "h2", 1)];
        let store = ProgressStore::load(dir.path.join("p.json")).unwrap();
        sort(&mut entries, SortKey::RecentlyRead, &store);
        assert_eq!(titles(&entries), ["alpha", "Zulu"]);
    }
}
