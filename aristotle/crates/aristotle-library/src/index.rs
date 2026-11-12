// SPDX-License-Identifier: AGPL-3.0-or-later
//! The persistent library index.
//!
//! One JSON file, `library.json`, in the data directory. JSON over a binary
//! format such as postcard because:
//!
//! - fields can be added later with `#[serde(default)]` and old files still
//!   load; postcard has no field names, so any schema change breaks them;
//! - it can be read and repaired with a text editor over SSH;
//! - size and speed do not matter at this scale: 31 books are about 17 KB
//!   and load in well under a millisecond on the host.
//!
//! # Crash safety
//!
//! The library lives on the Kindle's FAT userstore, where `rename` is not an
//! atomic metadata transaction and writeback can lag behind the syscall. A
//! power cut mid-write can therefore leave `library.json` empty or truncated,
//! which used to abort startup (FIX-109). So the index keeps one previous
//! generation and never gives up on a damaged file:
//!
//! - [`Index::save`] writes `library.json.tmp`, syncs it, rotates a *readable*
//!   current file to `library.json.bak`, renames the temporary file into place
//!   and syncs the directory (best effort; FAT may not support it). A damaged
//!   current file is not rotated, so it cannot displace a good backup.
//! - [`Index::load`] falls back to `library.json.bak` when the main file is
//!   missing, empty or unparsable, writes the recovered copy back, and only
//!   when that also fails moves the damaged file to `library.json.corrupt` and
//!   starts with an empty index. The next scan rebuilds the library.
//!
//! FAT can still lose both generations if the card is yanked mid-write; the
//! backup is a mitigation, not a guarantee.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::format::Format;
use crate::metadata::Metadata;

pub const INDEX_VERSION: u32 = 1;
pub const INDEX_FILE: &str = "library.json";
/// The previous generation, kept beside the index by [`Index::save`].
const BACKUP_SUFFIX: &str = ".bak";
/// Where [`Index::load`] moves an index it cannot read and has no backup for.
const CORRUPT_SUFFIX: &str = ".corrupt";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    /// CBZ parser version used by the last completed scan.
    #[serde(default)]
    pub cbz_metadata_version: u32,
    /// Library root the paths are relative to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default)]
    pub books: Vec<BookRecord>,
    /// Files that were sniffed and found not to be books, so that the next
    /// scan does not open them again while they are unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored: Vec<FileStamp>,
    /// Books whose file disappeared. Their reading state is kept so that a
    /// book deleted and copied back over USB resumes where it was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<BookRecord>,
}

/// Size and modification time: the cheap "has this file changed" test. Both
/// are compared because FAT timestamps have 2-second resolution, so a quick
/// rewrite can keep the mtime but rarely keeps the size too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    #[serde(default)]
    pub mtime_ns: u32,
}

impl FileStamp {
    pub fn same_file(&self, other: &FileStamp) -> bool {
        self.size == other.size && self.mtime == other.mtime && self.mtime_ns == other.mtime_ns
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookRecord {
    /// KOReader-compatible partial MD5 of the content (see [`crate::id`]).
    pub id: String,
    #[serde(flatten)]
    pub file: FileStamp,
    pub format: Format,
    pub meta: Metadata,
    /// Content key of the cached cover thumbnail (see
    /// [`crate::cover::CoverCache`]): the MD5 of the cover image. The
    /// thumbnail's size is part of its file name, so one key serves the
    /// shelf and the screensaver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    /// Unix seconds when the book first appeared in the library.
    pub added: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened: Option<i64>,
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(default, skip_serializing_if = "ReaderSettings::is_empty")]
    pub settings: ReaderSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotation_tombstones: Vec<AnnotationTombstone>,
}

impl BookRecord {
    /// Copies everything the reader owns (as opposed to what the scanner
    /// derives from the file) from `old`. Used when a file is replaced,
    /// moved, or comes back after removal.
    pub fn inherit_state(&mut self, old: &BookRecord) {
        self.added = old.added;
        self.last_opened = old.last_opened;
        self.status = old.status;
        self.progress = old.progress.clone();
        self.settings = old.settings.clone();
        self.annotations = old.annotations.clone();
        self.annotation_tombstones = old.annotation_tombstones.clone();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Unread,
    Reading,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Position in the book from 0.0 to 1.0, for progress bars and sorting.
    /// Format-independent but approximate; the locator is authoritative.
    pub fraction: f32,
    pub locator: Locator,
    /// Unix seconds of the last update, to pick the newer of two sources.
    pub updated: i64,
    /// Where this progress came from: "aristotle", "kindle", "koreader".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Printed page label at the position, when a page map is known
    /// (Kindle `.apnx`), for display such as "page 4 of 180".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A format-specific position. Each variant is what the corresponding
/// renderer can seek to exactly; `fraction` alone drifts when fonts change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Locator {
    /// Reflowable EPUB: spine item and character offset in its text.
    Epub { spine: u32, offset: u32 },
    /// MOBI/AZW3: the Kindle "position", a byte offset into the
    /// uncompressed text, which is what the stock reader stores.
    Kindle { position: u64 },
    /// Fixed layout (PDF, CBZ): zero-based page index.
    Page { index: u32 },
    /// Plain text: byte offset.
    Text { offset: u64 },
    /// KOReader's CRE XPointer, kept verbatim so it can be converted once
    /// the EPUB renderer exists.
    XPointer { xpointer: String },
}

/// Per-book overrides of the global reader settings. Every field is
/// optional: `None` means "use the global default".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReaderSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justify: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hyphenation: Option<bool>,
    /// Fixed layout zoom (1.0 = fit width).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u16>,
    /// Forward-compatible bag for settings not modelled yet.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, String>,
}

impl ReaderSettings {
    pub fn is_empty(&self) -> bool {
        *self == ReaderSettings::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnotationKind {
    Highlight,
    Note,
    Bookmark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationTombstone {
    pub id: String,
    pub deleted_at: i64,
}

/// Generates a persistent random UUID v4 string for annotations.
pub fn generate_annotation_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(1);

    let mut bytes = [0u8; 16];
    let mut filled = false;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut bytes).is_ok() {
            filled = true;
        }
    }
    if !filled {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let t_bytes = now.to_le_bytes();
        let c_bytes = count.to_le_bytes();
        bytes[..8].copy_from_slice(&t_bytes[..8]);
        bytes[8..].copy_from_slice(&c_bytes[..8]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // RFC 4122 version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant 1
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// Stable persistent ID (UUIDv4).
    #[serde(default)]
    pub id: String,
    pub kind: AnnotationKind,
    /// Highlighted text, or the note's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Printed page label ("12", "xii") when the book has page numbers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// Kindle location range (1 location = 150 bytes of MOBI text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<(u32, u32)>,
    /// Format-specific position when known exactly (from sidecars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(String, String)>,
    /// Local time as written by the source, "YYYY-MM-DD HH:MM:SS".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Local time when the annotation was last modified, "YYYY-MM-DD HH:MM:SS".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// Color code (e.g. "yellow", "green", "blue", "purple", "pink").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Style code (e.g. "highlight", "underline").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// "kindle-clippings", "kindle-sidecar", "koreader", "aristotle".
    pub source: String,
    /// Chapter title at the start of the annotation, when the reader knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    /// The reader's note attached to a highlight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Index {
    /// Loads the index at `path`, recovering from a torn write.
    ///
    /// A missing file yields an empty index, unless `library.json.bak` is
    /// readable; an empty or unparsable file is replaced by that backup when
    /// it is readable (and the recovered copy is written back). Otherwise the
    /// damaged file is moved aside to `library.json.corrupt` and an empty index
    /// is returned, so a damaged index costs the reading state but not the
    /// whole application.
    ///
    /// # Errors
    /// Returns [`Error::Index`] when the file declares a newer version than
    /// this build understands; that is not damage and is left untouched.
    pub fn load(path: &Path) -> Result<Index> {
        let primary = match fs::read(path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let index = match primary {
            Some(data) => match decode(&data) {
                Ok(index) => index,
                Err(DecodeError::TooNew(version)) => return Err(too_new(version)),
                Err(DecodeError::Invalid(err)) => match read_backup(path) {
                    Ok(index) => {
                        eprintln!(
                            "aristotle: {}: {err}; recovered {}",
                            path.display(),
                            backup_path(path).display()
                        );
                        // Write the recovered copy back so the next start does
                        // not need the backup, and so the next save rotates a
                        // readable file instead of the damaged one.
                        let _ = index.save(path);
                        index
                    }
                    Err(DecodeError::TooNew(version)) => return Err(too_new(version)),
                    Err(DecodeError::Invalid(backup_err)) => {
                        eprintln!(
                            "aristotle: {}: {err}; no usable backup ({backup_err}); \
                             starting with an empty index",
                            path.display()
                        );
                        quarantine(path);
                        return Ok(empty());
                    }
                },
            },
            None => match read_backup(path) {
                Ok(index) => {
                    eprintln!(
                        "aristotle: {}: missing; recovered {}",
                        path.display(),
                        backup_path(path).display()
                    );
                    let _ = index.save(path);
                    index
                }
                Err(DecodeError::TooNew(version)) => return Err(too_new(version)),
                Err(DecodeError::Invalid(_)) => return Ok(empty()),
            },
        };
        let mut index = index;
        let mut dirty = false;
        for book in &mut index.books {
            for a in &mut book.annotations {
                if a.id.is_empty() {
                    a.id = generate_annotation_id();
                    dirty = true;
                }
            }
            // Covers used to carry their size ("<md5>-200x320"); the key is
            // now the image's MD5 alone, and the size lives in the file name.
            if let Some(cover) = &book.cover {
                let base = crate::cover::base_key(cover);
                if base.len() != cover.len() {
                    book.cover = Some(base.to_string());
                    dirty = true;
                }
            }
        }
        if dirty {
            let _ = index.save(path);
        }
        Ok(index)
    }

    /// Writes the index, keeping the previous readable generation as
    /// `library.json.bak`. A current file that does not parse (empty or
    /// truncated by power loss) is not kept: it would displace a good backup.
    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_json::to_vec(self).map_err(|e| Error::Index(e.to_string()))?;
        save_index(path, &data)
    }

    /// Drops the oldest remembered removed books beyond `max`, newest and
    /// most recently opened kept first.
    pub fn trim_removed(&mut self, max: usize) {
        if self.removed.len() > max {
            self.removed
                .sort_by_key(|r| std::cmp::Reverse(r.last_opened.unwrap_or(r.added)));
            self.removed.truncate(max);
        }
    }

    /// Preserves file identity when the scan root expands to an ancestor.
    pub(crate) fn rebase_root(&mut self, new_root: &str) -> bool {
        if self
            .root
            .as_deref()
            .is_some_and(|old| Path::new(old) == Path::new(new_root))
        {
            self.root = Some(new_root.to_string());
            return true;
        }
        let Some(prefix) = self
            .root
            .as_deref()
            .and_then(|old| crate::library::path_prefix(new_root, old))
        else {
            return false;
        };
        for stamp in self
            .books
            .iter_mut()
            .chain(self.removed.iter_mut())
            .map(|b| &mut b.file)
            .chain(self.ignored.iter_mut())
        {
            stamp.path = crate::library::with_prefix(&prefix, &stamp.path);
        }
        self.root = Some(new_root.to_string());
        true
    }

    /// Folds the index of a scan that ran on its own copy into this one,
    /// which the app has been changing meanwhile (FIX-106).
    ///
    /// The scan is the truth about the files: which books exist, their
    /// format, metadata, cover, and the lists of ignored and removed books.
    /// The reader is the truth about its own state: progress, status, marks,
    /// settings and last-opened are kept even when they changed while the
    /// scan ran, because the scan started from an older snapshot of them.
    /// A book the reader deleted during the scan stays deleted, even though
    /// the scan's copy still lists it; its file is gone, which tells the two
    /// out from a book the scan restored.
    pub fn absorb_scan(&mut self, mut scanned: Index) {
        let root = scanned.root.clone();
        if let Some(new_root) = root.as_deref() {
            self.rebase_root(new_root);
        }
        let file_exists = |rec: &BookRecord| {
            root.as_deref()
                .is_some_and(|r| Path::new(r).join(&rec.file.path).exists())
        };
        let mut books = Vec::with_capacity(scanned.books.len());
        for mut rec in std::mem::take(&mut scanned.books) {
            if let Some(old) = self.books.iter().find(|b| b.id == rec.id) {
                rec.inherit_state(old);
                if rec.cover.is_none() {
                    rec.cover = old.cover.clone();
                }
                books.push(rec);
            } else if let Some(old) = self.removed.iter().find(|b| b.id == rec.id) {
                // In `removed` and in the scan's copy: the file either came
                // back (the scan restored it) or went away while the scan ran,
                // after the scan had read it.
                if file_exists(&rec) {
                    rec.inherit_state(old);
                    if rec.cover.is_none() {
                        rec.cover = old.cover.clone();
                    }
                    books.push(rec);
                }
            } else {
                books.push(rec);
            }
        }
        // Deletions made during the scan win, and this copy of a record is
        // newer than the one the scan started from.
        let mut removed = scanned.removed;
        for old in &self.removed {
            if books.iter().any(|b| b.id == old.id) {
                continue;
            }
            match removed.iter_mut().find(|r| r.id == old.id) {
                Some(r) => *r = old.clone(),
                None => removed.push(old.clone()),
            }
        }
        // A book scanned on this side after the scan's copy was opened (a
        // catalogue or Wi-Fi download) is in neither list of the scan; it
        // stays. A different root makes the old paths meaningless, though.
        if self.root.as_deref() == root.as_deref() {
            for b in &self.books {
                if !books.iter().any(|x| x.id == b.id) && !removed.iter().any(|x| x.id == b.id) {
                    books.push(b.clone());
                }
            }
        }
        self.books = books;
        self.ignored = scanned.ignored;
        self.removed = removed;
        self.cbz_metadata_version = scanned.cbz_metadata_version;
        if root.is_some() {
            self.root = root;
        }
        self.trim_removed(crate::library::MAX_REMOVED);
    }
}

/// Writes `data` to `path` via a temporary file and a rename, with an fsync
/// in between and a best-effort fsync of the directory afterwards, so a crash
/// never exposes an empty or half-written file.
///
/// This keeps no backup: callers such as the cover cache and page cache write
/// files that are cheap to rebuild and would only collect `.bak` debris. The
/// library index, whose loss is user-visible, uses [`Index::save`] instead.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic_inner(path, data, false)
}

/// Writes serialized index bytes the way [`Index::save`] does: the current
/// file is kept as `.bak` when it still parses. The app serialises the index on
/// the caller's thread and writes it from its save thread, so it cannot use
/// [`Index::save`] directly.
pub fn save_index(path: &Path, data: &[u8]) -> Result<()> {
    let keep_previous = parses_as_index(path);
    write_atomic_inner(path, data, keep_previous)
}

/// [`write_atomic`], optionally rotating the current file to `<name>.bak`
/// before replacing it.
fn write_atomic_inner(path: &Path, data: &[u8], keep_previous: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let tmp = with_suffix(path, ".tmp");
    let written = fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(data)?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    // The old file is moved only after the new one is safely on disk. If the
    // rotation fails, the current file stays where it is and the temporary one
    // is removed, so nothing readable is lost.
    if keep_previous && path.exists() {
        if let Err(e) = rotate_to_backup(path) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_parent_dir(path);
    Ok(())
}

/// `path` -> `path<suffix>`, in the same directory.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, BACKUP_SUFFIX)
}

/// Moves `path` to `<name>.bak`, replacing an older backup if there is one.
fn rotate_to_backup(path: &Path) -> std::io::Result<()> {
    let bak = backup_path(path);
    match fs::remove_file(&bak) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::rename(path, &bak)
}

/// Moves an unreadable index to `<name>.corrupt`, replacing an older one.
/// Failures are logged and swallowed: startup continues either way.
fn quarantine(path: &Path) {
    let corrupt = with_suffix(path, CORRUPT_SUFFIX);
    if fs::rename(path, &corrupt).is_ok() {
        return;
    }
    // FAT can refuse to rename over an existing name; drop the old one first.
    let _ = fs::remove_file(&corrupt);
    if let Err(e) = fs::rename(path, &corrupt) {
        eprintln!(
            "aristotle: can't move {} to {}: {e}",
            path.display(),
            corrupt.display()
        );
    }
}

/// Best-effort `fsync` of the directory holding `path`, so the rename itself
/// is durable. vfat may reject opening or syncing a directory; that is ignored.
#[cfg(unix)]
pub(crate) fn sync_parent_dir(path: &Path) {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_dir(_path: &Path) {}

/// Why a file did not decode into an index.
enum DecodeError {
    /// Not JSON, or not the index shape.
    Invalid(String),
    /// Valid JSON from a build newer than this one.
    TooNew(u32),
}

fn decode(data: &[u8]) -> std::result::Result<Index, DecodeError> {
    let index: Index =
        serde_json::from_slice(data).map_err(|e| DecodeError::Invalid(e.to_string()))?;
    if index.version > INDEX_VERSION {
        return Err(DecodeError::TooNew(index.version));
    }
    Ok(index)
}

fn read_backup(path: &Path) -> std::result::Result<Index, DecodeError> {
    let data = fs::read(backup_path(path)).map_err(|e| DecodeError::Invalid(e.to_string()))?;
    decode(&data)
}

/// Whether `path` holds a decodable index, whatever its version. A damaged
/// file must not be rotated over the last good backup.
fn parses_as_index(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice::<Index>(&data).ok())
        .is_some()
}

fn empty() -> Index {
    Index {
        version: INDEX_VERSION,
        ..Index::default()
    }
}

fn too_new(version: u32) -> Error {
    Error::Index(format!(
        "version {version} is newer than this build ({INDEX_VERSION})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_index() {
        let dir = crate::testdir("index");
        let idx = Index::load(&dir.join("nope.json")).unwrap();
        assert_eq!(idx.version, INDEX_VERSION);
        assert!(idx.books.is_empty());
    }

    #[test]
    fn roundtrip_and_unknown_fields() {
        let dir = crate::testdir("index");
        let p = dir.join("library.json");
        let mut idx = Index {
            version: 1,
            ..Index::default()
        };
        idx.books.push(BookRecord {
            id: "abc".into(),
            file: FileStamp {
                path: "Platon/Uczta.epub".into(),
                size: 10,
                mtime: 5,
                mtime_ns: 0,
            },
            format: Format::Epub,
            meta: Metadata {
                title: "Uczta".into(),
                ..Metadata::default()
            },
            cover: None,
            added: 1,
            last_opened: None,
            status: Status::Reading,
            progress: Some(Progress {
                fraction: 0.5,
                locator: Locator::Epub {
                    spine: 3,
                    offset: 120,
                },
                updated: 2,
                source: None,
                page: None,
            }),
            settings: ReaderSettings {
                font_size: Some(11.5),
                ..Default::default()
            },
            annotations: vec![],
            annotation_tombstones: vec![],
        });
        idx.save(&p).unwrap();
        let back = Index::load(&p).unwrap();
        assert_eq!(back.books, idx.books);

        // Files written by a future minor version with extra fields still load.
        let json = String::from_utf8(fs::read(&p).unwrap()).unwrap().replacen(
            "\"added\":1",
            "\"added\":1,\"future\":true",
            1,
        );
        fs::write(&p, json).unwrap();
        assert_eq!(Index::load(&p).unwrap().books.len(), 1);
    }

    #[test]
    fn annotation_id_backfilled_and_persisted_for_old_indices() {
        let dir = crate::testdir("index_backfill");
        let p = dir.join("library.json");
        // Simulate an old index without "id" in annotation
        let old_json = r#"{
            "version": 1,
            "books": [
                {
                    "id": "book-1",
                    "path": "test.epub",
                    "size": 100,
                    "mtime": 1000,
                    "format": "epub",
                    "meta": { "title": "Test" },
                    "added": 1000,
                    "annotations": [
                        {
                            "kind": "highlight",
                            "text": "old quote",
                            "source": "aristotle"
                        }
                    ]
                }
            ]
        }"#;
        fs::write(&p, old_json).unwrap();
        let loaded = Index::load(&p).unwrap();
        assert_eq!(loaded.books.len(), 1);
        let id1 = loaded.books[0].annotations[0].id.clone();
        assert!(!id1.is_empty(), "id must be backfilled");
        assert_eq!(id1.len(), 36, "id must be uuid format");

        // Reload to verify it was saved to disk and remained stable
        let reloaded = Index::load(&p).unwrap();
        assert_eq!(reloaded.books[0].annotations[0].id, id1);
    }

    /// Saves an empty index with a distinguishing `root`.
    fn save_root(path: &Path, root: &str) {
        Index {
            version: INDEX_VERSION,
            root: Some(root.into()),
            ..Index::default()
        }
        .save(path)
        .unwrap();
    }

    #[test]
    fn empty_index_falls_back_to_the_backup() {
        let dir = crate::testdir("index-empty-bak");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "previous");
        save_root(&p, "current");
        fs::write(&p, b"").unwrap();

        let loaded = Index::load(&p).unwrap();
        assert_eq!(loaded.root.as_deref(), Some("previous"));
        assert_eq!(
            fs::read(&p).unwrap(),
            fs::read(backup_path(&p)).unwrap(),
            "the recovered copy is written back to the main file"
        );
    }

    #[test]
    fn empty_index_without_a_backup_is_quarantined() {
        let dir = crate::testdir("index-empty-corrupt");
        let p = dir.join(INDEX_FILE);
        fs::write(&p, b"").unwrap();
        // An older quarantine is replaced, not kept beside the new one.
        fs::write(with_suffix(&p, CORRUPT_SUFFIX), b"older").unwrap();

        let loaded = Index::load(&p).unwrap();
        assert_eq!(loaded.version, INDEX_VERSION);
        assert!(loaded.books.is_empty());
        assert!(!p.exists());
        assert_eq!(
            fs::read(with_suffix(&p, CORRUPT_SUFFIX)).unwrap(),
            b"".to_vec()
        );
    }

    #[test]
    fn truncated_index_falls_back_to_the_backup() {
        let dir = crate::testdir("index-truncated-bak");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "previous");
        save_root(&p, "current");
        let data = fs::read(&p).unwrap();
        fs::write(&p, &data[..data.len() / 2]).unwrap();

        assert_eq!(
            Index::load(&p).unwrap().root.as_deref(),
            Some("previous"),
            "a truncated index must fall back to the previous generation"
        );
    }

    #[test]
    fn truncated_index_without_a_backup_starts_empty() {
        let dir = crate::testdir("index-truncated-corrupt");
        let p = dir.join(INDEX_FILE);
        fs::write(&p, br#"{"version":1,"books":["#).unwrap();

        let loaded = Index::load(&p).unwrap();
        assert!(loaded.books.is_empty());
        assert!(!p.exists());
        assert!(with_suffix(&p, CORRUPT_SUFFIX).exists());
    }

    #[test]
    fn missing_index_is_restored_from_the_backup() {
        let dir = crate::testdir("index-missing-bak");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "only");
        fs::rename(&p, backup_path(&p)).unwrap();

        let loaded = Index::load(&p).unwrap();
        assert_eq!(loaded.root.as_deref(), Some("only"));
        assert!(p.exists(), "the recovered copy is written back");
    }

    #[test]
    fn every_save_keeps_the_previous_generation() {
        let dir = crate::testdir("index-rotate");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "first");
        save_root(&p, "second");
        assert_eq!(Index::load(&p).unwrap().root.as_deref(), Some("second"));
        assert_eq!(
            Index::load(&backup_path(&p)).unwrap().root.as_deref(),
            Some("first")
        );

        save_root(&p, "third");
        assert_eq!(
            Index::load(&backup_path(&p)).unwrap().root.as_deref(),
            Some("second"),
            "the backup is always the generation before the current file"
        );
    }

    #[test]
    fn save_does_not_displace_a_good_backup_with_a_damaged_index() {
        let dir = crate::testdir("index-damaged-rotate");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "first");
        save_root(&p, "second");
        fs::write(&p, b"").unwrap();

        save_root(&p, "third");
        assert_eq!(Index::load(&p).unwrap().root.as_deref(), Some("third"));
        assert_eq!(
            Index::load(&backup_path(&p)).unwrap().root.as_deref(),
            Some("first"),
            "the damaged main file must not become the backup"
        );
    }

    #[test]
    fn newer_version_still_fails_without_touching_the_files() {
        let dir = crate::testdir("index-newer");
        let p = dir.join(INDEX_FILE);
        save_root(&p, "previous");
        save_root(&p, "current");
        fs::write(&p, br#"{"version":2}"#).unwrap();

        let err = Index::load(&p).unwrap_err();
        assert!(
            matches!(&err, Error::Index(m) if m.contains("newer")),
            "unexpected error: {err:?}"
        );
        assert!(!with_suffix(&p, CORRUPT_SUFFIX).exists(), "not quarantined");
        assert_eq!(fs::read(&p).unwrap(), br#"{"version":2}"#.to_vec());
        assert_eq!(
            Index::load(&backup_path(&p)).unwrap().root.as_deref(),
            Some("previous"),
            "no fallback happened, the backup was not read or changed"
        );
    }
}
