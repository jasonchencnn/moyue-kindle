// SPDX-License-Identifier: AGPL-3.0-or-later
//! The library: an index plus a cover cache in one data directory, and the
//! incremental scanner that keeps them in step with the books on disk.
//!
//! ## Scanning
//!
//! A scan walks the root, skipping hidden entries, `.sdr` sidecar folders,
//! configured folders (`dictionaries` by default) and, directly under the
//! root, the Kindle user store's own system and app folders. On the device
//! the root is `/mnt/us`, the whole user store: books also live in `Books/`,
//! `Download/` and folders of the reader's own, not only in `documents`.
//! [`scan_root`] widens the stock `documents` root to the store; any other
//! root is scanned as given.
//!
//! Every candidate file is stat-ed. A file whose path, size and mtime match
//! the index is not opened at all, which makes a re-scan of an unchanged
//! library cost one `readdir` per folder and one `stat` per file. New and
//! changed files are sniffed, hashed (partial MD5, twelve 1 KiB reads),
//! parsed, and get a thumbnail.
//!
//! When a library recorded under `documents` is scanned under its parent for
//! the first time, the relative paths move down by one component and the
//! records keep their IDs and reading state, so nothing is read again.
//!
//! Reading state belongs to the book, not the path. When a new file has the
//! ID of a book whose path disappeared, it is a move or rename and inherits
//! the old record's state; when a known path changes content, the record at
//! that path passes its state on; when a removed book comes back, its state
//! is restored from the `removed` list.
//!
//! A scan can run while the reader keeps reading. It works on a copy of the
//! index and never writes it; the app hands that copy back through
//! [`Library::absorb_scan`], which keeps the progress, marks and settings
//! gained while the scan ran, so neither side loses a write.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::clippings::{title_key, Clipping};
use crate::cover::{CoverCache, ThumbSpec};
use crate::error::{Error, Result};
use crate::format::{self, Format};
use crate::index::{
    generate_annotation_id, Annotation, AnnotationTombstone, BookRecord, FileStamp, Index, Locator,
    Progress, Status, INDEX_FILE,
};
use crate::metadata;
use crate::query::{self, Query};
use crate::sidecar;
use crate::text;

pub const COVERS_DIR: &str = "covers";
/// Cap on remembered removed books; old entries are dropped first.
pub(crate) const MAX_REMOVED: usize = 500;

/// The Kindle user store, the whole storage the reader can see over USB.
pub const KINDLE_STORE: &str = "/mnt/us";

/// The stock `documents` folder: the default root and the base for the app's
/// own downloads, exports and screensavers.
pub const KINDLE_DOCUMENTS: &str = "/mnt/us/documents";

/// Folders directly under the scan root that belong to the system or to an
/// app, never to the reader's books. They are skipped only at the root:
/// a user folder with the same name deeper in the tree (`documents/system`)
/// is still scanned.
///
/// The list after `aristotle-notes` comes from the read-only listing of
/// `/mnt/us` on 2026-11-12 (`docs/tasks/UX-112/device-directories.md`):
/// USB networking, screensaver hacks, KUAL packages, toolchains and other
/// app folders. Versioned Aristotle bundles are matched by
/// [`excluded_store_folder`] instead, because their names carry the build.
pub const STORE_SKIP_DIRS: &[&str] = &[
    "aristotle",
    "system",
    "audible",
    "tts",
    "voice",
    "music",
    "koreader",
    "extensions",
    "fonts",
    "screensavers",
    "dictionaries",
    "dict",
    "aristotle-notes",
    "usbnet",
    "linkss",
    "mkk",
    "mrpackages",
    "rp",
    "libkh",
    "inkread-lab",
    "config",
    "configs",
    "log",
    "logs",
    "diagnostic_logs",
];

/// Prefix of the versioned Aristotle bundles that sit beside the running
/// app (`aristotle-pre-app007`, `aristotle-pre-ui-u1`, `aristotle-v0.1.0`).
const APP_BUNDLE_PREFIX: &str = "aristotle-";

/// Whether a top-level folder name belongs to the system, an app or a tool
/// rather than to the reader's books.
///
/// This is the one blacklist the scanner, the source and destination pickers
/// and the transfer use, so a folder excluded from a scan is also excluded
/// from the folder lists. It matches the whole directory name only: a book
/// named `system.txt` or a user folder `documents/system` is untouched. No
/// file format or file name is ever excluded.
pub fn excluded_store_folder(name: &str) -> bool {
    let lower = name.to_lowercase();
    STORE_SKIP_DIRS.contains(&lower.as_str()) || lower.starts_with(APP_BUNDLE_PREFIX)
}

/// The folder a scan walks: `root`, widened to the Kindle user store when
/// `root` is the stock `documents` folder.
///
/// Books reach a Kindle in `documents`, but also in folders beside it
/// (`Books`, `Download`, custom ones), so the default scan covers the whole
/// user store. `documents` stays the root for the app's own folders
/// (downloads, clippings, notes, screensavers), which is a different
/// concern. A custom `--root` is scanned as given.
pub fn scan_root(root: &Path) -> PathBuf {
    if root == Path::new(KINDLE_DOCUMENTS) {
        PathBuf::from(KINDLE_STORE)
    } else {
        root.to_path_buf()
    }
}

/// Resolves the folder explicitly imported books are moved into.
///
/// The destination must be a folder inside `root`. It does not have to exist
/// yet: the longest existing ancestor goes through the file system, so a
/// symlink that points out of the root is refused. Hidden and private path
/// components are rejected, as is a store's top-level system or app folder;
/// a nested user folder with the same name (`documents/system`) is the
/// reader's own and stays available. The app creates the folder only when a
/// confirmed import actually runs, and the root itself is a valid choice.
pub fn resolve_library_destination(root: &Path, destination: &Path) -> Result<PathBuf> {
    if !destination.is_absolute() {
        return Err(Error::scope("the library folder must be absolute"));
    }
    let root =
        fs::canonicalize(root).map_err(|e| Error::scope(format!("{}: {e}", root.display())))?;
    let mut existing = destination.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let resolved = loop {
        match fs::canonicalize(&existing) {
            Ok(path) => {
                break missing
                    .iter()
                    .rev()
                    .fold(path, |path, part| path.join(part))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match (existing.file_name(), existing.parent()) {
                    (Some(name), Some(parent)) if parent != existing => {
                        missing.push(name.to_os_string());
                        existing = parent.to_path_buf();
                    }
                    _ => {
                        return Err(Error::scope(format!(
                            "{} cannot be resolved",
                            destination.display()
                        )))
                    }
                }
            }
            // A permission error or a broken mount is not "the folder does
            // not exist yet": report it instead of guessing a path.
            Err(e) => return Err(Error::Io(e)),
        }
    };
    if !resolved.starts_with(&root) {
        return Err(Error::scope(
            "the library folder must be inside the scan root",
        ));
    }
    let relative = resolved.strip_prefix(&root).unwrap_or(Path::new(""));
    for part in relative.components() {
        if let Component::Normal(name) = part {
            if name.to_string_lossy().starts_with('.') {
                return Err(Error::scope("the library folder is a hidden folder"));
            }
        }
    }
    if root == Path::new(KINDLE_STORE)
        && relative.components().next().is_some_and(|part| {
            excluded_store_folder(&part.as_os_str().to_string_lossy().to_lowercase())
        })
    {
        return Err(Error::scope(
            "the library folder belongs to the system or an app",
        ));
    }
    Ok(resolved)
}

/// The folder a scoped scan looks at, checked against the scan root.
///
/// Both paths go through the file system, so a symlink inside the root that
/// points out of it is refused instead of followed, and a folder that is not
/// there is an error rather than an empty scan.
fn resolve_scope(root: &Path, scope: &Path) -> Result<PathBuf> {
    let scope =
        fs::canonicalize(scope).map_err(|e| Error::scope(format!("{}: {e}", scope.display())))?;
    if scope == root {
        return Ok(root.to_path_buf());
    }
    if !scope.starts_with(root) {
        return Err(Error::scope(format!(
            "{} is not under the scan root {}",
            scope.display(),
            root.display()
        )));
    }
    if !scope.is_dir() {
        return Err(Error::scope(format!("{} is not a folder", scope.display())));
    }
    Ok(scope)
}

/// What is left of a book after a push was acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushAck {
    /// A deletion of the submitted snapshot was forgotten.
    pub acknowledged: bool,
    /// The book still has something the push did not carry: a deletion made
    /// while it was in flight, a note added after it, a newer position.
    pub pending: bool,
}

pub struct Library {
    data_dir: PathBuf,
    index: Index,
    covers: CoverCache,
}

#[derive(Clone, Debug)]
pub struct ScanProgress {
    pub path: String,
    pub found: usize,
    pub current: usize,
    pub total: usize,
    pub new_book: Option<(String, String)>,
}

/// Which new books a scan may add to the index.
///
/// A manual scan first runs [`ScanAdmission::Staged`]: it finds the books and
/// leaves every supported file in [`ScanReport::candidates`], adding none.
/// The reader then confirms a selection, and a second scan with
/// [`ScanAdmission::Confirmed`] adds only those paths. Automatic scans (start,
/// background, upload) use `Confirmed` with an empty set unless the app has an
/// explicit path to admit, so a new local file never enters the shelf without
/// its reader choosing it. [`ScanAdmission::All`] keeps the historical
/// behaviour of the direct API for explicit callers (`cli`, `preview`,
/// benchmarks, tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ScanAdmission {
    /// Add every supported new book (the historical behaviour).
    #[default]
    All,
    /// Add no new book; report every supported file as a candidate.
    Staged,
    /// Add a new book only when its path, relative to the scan root, is in
    /// the set.
    Confirmed(HashSet<String>),
}

impl ScanAdmission {
    fn admits(&self, path: &str) -> bool {
        match self {
            ScanAdmission::All => true,
            ScanAdmission::Staged => false,
            ScanAdmission::Confirmed(paths) => paths.contains(path),
        }
    }

    fn is_staged(&self) -> bool {
        matches!(self, ScanAdmission::Staged)
    }
}

#[derive(Clone)]
pub struct ScanOptions {
    /// Extract covers and build thumbnails for new and changed books.
    pub covers: bool,
    pub thumb: ThumbSpec,
    /// Folder names (case-insensitive) skipped at any depth.
    pub skip_dirs: Vec<String>,
    /// Folder names (case-insensitive) skipped only directly under the scan
    /// root: the Kindle user store's system and app folders. A nested user
    /// folder with the same name still scans.
    pub root_skip_dirs: Vec<String>,
    /// Import stock Kindle and KOReader sidecars for books new to the index.
    pub import_sidecars: bool,
    /// Which new books may be added; see [`ScanAdmission`].
    pub admission: ScanAdmission,
    /// Cancellation flag.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Progress notification callback.
    pub on_progress: Option<Arc<dyn Fn(ScanProgress) + Send + Sync>>,
}

impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("covers", &self.covers)
            .field("thumb", &self.thumb)
            .field("skip_dirs", &self.skip_dirs)
            .field("root_skip_dirs", &self.root_skip_dirs)
            .field("import_sidecars", &self.import_sidecars)
            .field("admission", &self.admission)
            .field("cancel", &self.cancel.is_some())
            .field("on_progress", &self.on_progress.is_some())
            .finish()
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            covers: true,
            thumb: ThumbSpec::default(),
            skip_dirs: vec!["dictionaries".into()],
            root_skip_dirs: Vec::new(),
            import_sidecars: true,
            admission: ScanAdmission::All,
            cancel: None,
            on_progress: None,
        }
    }
}

impl ScanOptions {
    fn root_skips(&self, root: &Path) -> HashSet<String> {
        let mut names: HashSet<String> = self
            .root_skip_dirs
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        if root == Path::new(KINDLE_STORE) {
            names.extend(STORE_SKIP_DIRS.iter().map(|name| name.to_string()));
        }
        names
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub unchanged: usize,
    pub added: usize,
    pub updated: usize,
    pub moved: usize,
    pub restored: usize,
    pub removed: usize,
    pub ignored: usize,
    pub thumbnails: usize,
    pub sidecars_imported: usize,
    pub errors: Vec<(String, String)>,
    pub walk_time: Duration,
    pub parse_time: Duration,
    pub cover_time: Duration,
    pub total_time: Duration,
    pub cancelled: bool,
    /// The index was re-rooted to an ancestor of its old root (the stock
    /// `documents` folder expanded to the user store), so the caller has to
    /// save even when no book changed.
    pub migrated: bool,
    pub added_books: Vec<AddedBook>,
    /// Supported books a staged scan found, and any new book a confirmed
    /// scan left unconfirmed. A staged scan adds none of them.
    pub candidates: Vec<ScanCandidate>,
}

/// A supported book a scan found but did not add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanCandidate {
    /// The path relative to the scan root, the form the index keeps.
    pub path: String,
    /// A title to show before the book is parsed: from its metadata when the
    /// index already knows the file, else from the file name.
    pub title: String,
    /// The format label (`EPUB`, `TXT`, ...).
    pub format: String,
    /// The index record the file already has, when it is in the library
    /// (perhaps outside the destination, so the reader can move it).
    pub indexed: Option<String>,
}

impl ScanCandidate {
    /// Whether the file is new to the library and a confirmation would add
    /// it.
    pub fn is_new(&self) -> bool {
        self.indexed.is_none()
    }
}

/// A book a scan added, for the summary the scan screen shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedBook {
    pub id: String,
    pub title: String,
    pub authors: String,
    pub format: String,
}

#[derive(Debug, Default)]
pub struct ClippingsImport {
    pub matched: usize,
    pub added: usize,
    pub duplicates: usize,
    /// Titles of clippings that matched no book.
    pub unmatched: Vec<String>,
}

struct Found {
    abs: PathBuf,
    stamp: FileStamp,
    author_dir: Option<String>,
}

pub(crate) fn unix_secs(t: SystemTime) -> Option<i64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

pub fn now() -> i64 {
    unix_secs(SystemTime::now()).unwrap_or(0)
}

impl Library {
    /// Opens (or starts) the library stored in `data_dir`.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Library> {
        Library::open_with(data_dir, ThumbSpec::default())
    }

    pub fn open_with(data_dir: impl Into<PathBuf>, thumb: ThumbSpec) -> Result<Library> {
        let data_dir = data_dir.into();
        let index = Index::load(&data_dir.join(INDEX_FILE))?;
        let covers = CoverCache::new(data_dir.join(COVERS_DIR), thumb);
        Ok(Library {
            data_dir,
            index,
            covers,
        })
    }

    pub fn save(&self) -> Result<()> {
        self.index.save(&self.data_dir.join(INDEX_FILE))
    }

    /// Folds in the index of a scan that ran on its own copy, keeping the
    /// reading state this library gained while the scan was running. The
    /// caller saves afterwards; see [`Index::absorb_scan`].
    pub fn absorb_scan(&mut self, scanned: Index) {
        self.index.absorb_scan(scanned);
    }

    /// Folds in records an explicit import or migration prepared on a worker,
    /// without losing what the reader changed meanwhile.
    ///
    /// A record already in the index keeps its title, cover, progress, marks
    /// and settings; only the file stamp moves to the copy in the library
    /// folder. A new record is inserted. An ID the reader deleted while the
    /// worker ran is not resurrected from the stale scan. The caller saves
    /// `confirmed_removed` contains only the removed records and their root
    /// at confirmation, so an explicitly reselected old book can return while
    /// a later deletion keeps winning. The caller saves durably and then
    /// authorizes the worker to remove exactly the returned
    /// IDs; a skipped record keeps its source file. Returns the IDs whose live
    /// path now is the prepared destination.
    pub fn apply_imported(
        &mut self,
        root: &Path,
        records: Vec<BookRecord>,
        confirmed_removed: &Index,
    ) -> Vec<String> {
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let root_str = root.to_string_lossy().into_owned();
        let active_before_rebase: HashSet<String> = if self.index.root.as_deref() != Some(&root_str)
        {
            let active = self
                .index
                .books
                .iter()
                .map(|book| book.id.clone())
                .collect();
            self.adopt_root(&root_str);
            active
        } else {
            HashSet::new()
        };
        let mut confirmed = confirmed_removed.clone();
        confirmed.rebase_root(&root_str);
        let mut admitted = Vec::new();
        for mut rec in records {
            if let Some(live) = self.index.books.iter_mut().find(|book| book.id == rec.id) {
                live.file = rec.file;
                live.format = rec.format;
                admitted.push(rec.id);
                continue;
            }
            if let Some(i) = self.index.removed.iter().position(|old| old.id == rec.id) {
                let old = &self.index.removed[i];
                if !active_before_rebase.contains(&rec.id)
                    && !confirmed.removed.iter().any(|approved| approved == old)
                {
                    continue;
                }
                let old = self.index.removed.remove(i);
                rec.inherit_state(&old);
            }
            admitted.push(rec.id.clone());
            self.index.books.push(rec);
        }
        self.index
            .books
            .sort_by(|a, b| a.file.path.cmp(&b.file.path));
        admitted
    }

    /// Puts the index back to a snapshot taken before an import was merged.
    /// Used when the durable save failed: the reader's live changes stay in
    /// memory, and the import can be tried again.
    pub fn restore_index(&mut self, index: Index) {
        self.index = index;
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn root(&self) -> Option<PathBuf> {
        self.index.root.as_ref().map(PathBuf::from)
    }

    pub fn books(&self) -> &[BookRecord] {
        &self.index.books
    }

    pub fn removed(&self) -> &[BookRecord] {
        &self.index.removed
    }

    pub fn book(&self, id: &str) -> Option<&BookRecord> {
        self.index.books.iter().find(|b| b.id == id)
    }

    pub fn book_mut(&mut self, id: &str) -> Option<&mut BookRecord> {
        self.index.books.iter_mut().find(|b| b.id == id)
    }

    /// Looks a book up by full ID or by a unique ID prefix (handy in a shell).
    pub fn find(&self, id_or_prefix: &str) -> Option<&BookRecord> {
        if let Some(b) = self.book(id_or_prefix) {
            return Some(b);
        }
        let mut it = self
            .index
            .books
            .iter()
            .filter(|b| b.id.starts_with(id_or_prefix));
        match (it.next(), it.next()) {
            (Some(b), None) if !id_or_prefix.is_empty() => Some(b),
            _ => None,
        }
    }

    pub fn absolute_path(&self, rec: &BookRecord) -> Option<PathBuf> {
        self.root().map(|r| r.join(&rec.file.path))
    }

    /// The cached shelf thumbnail of `rec`, if there is one.
    pub fn cover_path(&self, rec: &BookRecord) -> Option<PathBuf> {
        self.cover_path_sized(rec, self.covers.spec())
    }

    /// The cached thumbnail of `rec` at `spec`, if there is one. The shelf
    /// and the screensaver keep their own sizes of the same cover.
    pub fn cover_path_sized(&self, rec: &BookRecord, spec: ThumbSpec) -> Option<PathBuf> {
        rec.cover
            .as_deref()
            .map(crate::cover::base_key)
            .map(|k| self.covers.path_for(k, spec))
            .filter(|p| p.exists())
    }

    /// The thumbnail of `rec` at `spec`, making it from the book's cover
    /// image when the cache does not have it yet. Returns `None` when the
    /// book has no cover the cache can use.
    ///
    /// This reads the book file again, so it is for the screens that show one
    /// book large (the screensaver), never for a list.
    pub fn ensure_cover_path(&self, rec: &BookRecord, spec: ThumbSpec) -> Option<PathBuf> {
        let key = rec.cover.as_deref().map(crate::cover::base_key)?;
        let path = self.covers.path_for(key, spec);
        if path.exists() {
            return Some(path);
        }
        let bytes = self.cover_image(rec)?;
        self.covers.store_with(&bytes, spec).ok()?;
        Some(path)
    }

    /// The cover image bytes of `rec`, read from the book file.
    pub fn cover_image(&self, rec: &BookRecord) -> Option<Vec<u8>> {
        let path = self.absolute_path(rec)?;
        let dir = rec
            .file
            .path
            .rsplit_once('/')
            .map(|(d, _)| d)
            .and_then(|d| d.rsplit('/').next());
        crate::metadata::extract(&path, rec.format, dir, true)
            .ok()?
            .cover
    }

    /// Points `rec` at the thumbnail with content key `key` (see
    /// [`crate::cover::CoverCache::key`]).
    pub fn set_cover(&mut self, id: &str, key: &str) -> bool {
        match self.book_mut(id) {
            Some(b) => {
                b.cover = Some(key.to_string());
                true
            }
            None => false,
        }
    }

    pub fn query(&self, q: &Query) -> Vec<&BookRecord> {
        query::run(&self.index.books, q)
    }

    pub fn mark_opened(&mut self, id: &str, when: i64) -> bool {
        let Some(b) = self.book_mut(id) else {
            return false;
        };
        b.last_opened = Some(when);
        if b.status == Status::Unread {
            b.status = Status::Reading;
        }
        true
    }

    pub fn set_progress(&mut self, id: &str, progress: Progress) -> bool {
        let Some(b) = self.book_mut(id) else {
            return false;
        };
        if b.status == Status::Unread {
            b.status = Status::Reading;
        }
        b.progress = Some(progress);
        true
    }

    /// Adds an annotation to a book unless an equal one is there already.
    /// Returns whether it was added.
    pub fn add_annotation(&mut self, id: &str, mut a: Annotation) -> bool {
        if a.id.is_empty() {
            a.id = generate_annotation_id();
        }
        let aid = a.id.clone();
        match self.book_mut(id) {
            Some(b) => {
                b.annotation_tombstones.retain(|t| t.id != aid);
                merge_annotation(b, a)
            }
            None => false,
        }
    }

    /// Removes the book's annotations for which `remove` is true. Returns
    /// how many went.
    pub fn remove_annotations(&mut self, id: &str, remove: impl Fn(&Annotation) -> bool) -> usize {
        let Some(b) = self.book_mut(id) else {
            return 0;
        };
        let now = now() * 1000;
        let mut removed_ids = Vec::new();
        let before = b.annotations.len();
        b.annotations.retain(|a| {
            if remove(a) {
                if !a.id.is_empty() {
                    removed_ids.push(a.id.clone());
                }
                false
            } else {
                true
            }
        });
        for rid in removed_ids {
            b.annotation_tombstones.retain(|t| t.id != rid);
            b.annotation_tombstones.push(AnnotationTombstone {
                id: rid,
                deleted_at: now,
            });
        }
        before - b.annotations.len()
    }

    /// Forget the deletion tombstones of `id` after they reached the server.
    /// Returns whether any were there.
    pub fn clear_annotation_tombstones(&mut self, id: &str) -> bool {
        match self.book_mut(id) {
            Some(b) => !std::mem::take(&mut b.annotation_tombstones).is_empty(),
            None => false,
        }
    }

    /// Forget every deletion tombstone after a full push reached the server.
    pub fn clear_all_annotation_tombstones(&mut self) {
        for b in &mut self.index.books {
            b.annotation_tombstones.clear();
        }
    }

    /// Acknowledge what a push submitted and say what is still unsent.
    ///
    /// Only the deletions of the submitted snapshot are forgotten: a deletion
    /// made while the push was in flight, a note added after it and a newer
    /// position stay, and the book keeps its place on the pending list.
    ///
    /// A tombstone with the same id but a newer timestamp is a new deletion,
    /// not the one that went up: it stays.
    pub fn acknowledge_push(&mut self, id: &str, submitted: &BookRecord) -> PushAck {
        let Some(b) = self.book_mut(id) else {
            return PushAck {
                acknowledged: false,
                pending: false,
            };
        };
        // Readest only receives metadata for TXT; its reading state stays local.
        if matches!(submitted.format, Format::Txt) {
            return PushAck {
                acknowledged: false,
                pending: false,
            };
        }
        let before = b.annotation_tombstones.len();
        b.annotation_tombstones.retain(|t| {
            !submitted
                .annotation_tombstones
                .iter()
                .any(|s| s.id == t.id && s.deleted_at == t.deleted_at)
        });
        let acknowledged = before != b.annotation_tombstones.len();
        // What is left is what the push did not carry.
        let pending = b.progress != submitted.progress
            || b.annotations != submitted.annotations
            || !b.annotation_tombstones.is_empty();
        PushAck {
            acknowledged,
            pending,
        }
    }

    pub fn set_status(&mut self, id: &str, status: Status) -> bool {
        let Some(b) = self.book_mut(id) else {
            return false;
        };
        b.status = status;
        true
    }

    /// Removes a book from active records and remembers it in `removed`
    /// so its state can still be restored if the book file comes back.
    pub fn remove_book(&mut self, id: &str) -> Option<BookRecord> {
        let i = self.index.books.iter().position(|b| b.id == id)?;
        let rec = self.index.books.remove(i);
        self.index.removed.retain(|r| r.id != id);
        self.index.removed.push(rec.clone());
        self.index.trim_removed(MAX_REMOVED);
        Some(rec)
    }

    /// Moves the index to a new root. When the new root is an ancestor of
    /// the old one, every relative path gains the prefix that leads back
    /// down, so the records keep their IDs (including duplicate `-2`
    /// suffixes), reading state, annotations, tombstones, settings and
    /// timestamps, and unchanged books are not parsed again. Any other new
    /// root makes the old paths meaningless and keeps the records as
    /// "removed", to be matched by ID.
    ///
    /// Returns whether the index was re-rooted to an ancestor.
    fn adopt_root(&mut self, new_root: &str) -> bool {
        let Some(old_root) = self.index.root.clone() else {
            self.index.root = Some(new_root.to_string());
            return false;
        };
        if old_root == new_root {
            return false;
        }
        let rebased = self.index.rebase_root(new_root);
        if !rebased {
            let old = std::mem::take(&mut self.index.books);
            self.index.removed.extend(old);
            self.index.ignored.clear();
        }
        self.index.root = Some(new_root.to_string());
        rebased
    }

    /// Brings the index in line with the files under `root`. See the module
    /// documentation for what is re-read and how state follows books.
    ///
    /// The stock `documents` root is widened to the Kindle user store (see
    /// [`scan_root`]); every other root is scanned as given.
    pub fn scan(&mut self, root: &Path, opts: &ScanOptions) -> Result<ScanReport> {
        self.scan_with(root, None, opts)
    }

    /// Brings the index in line with the files under `scope`, one folder
    /// under the scan root, leaving everything outside it alone.
    ///
    /// The books the index holds for folders outside `scope` keep their
    /// records, progress and annotations: a scoped scan does not remove what
    /// it did not look at. Only the books inside `scope` that are gone are
    /// reported as removed.
    ///
    /// `scope` must be a real folder under the scan root. It is resolved
    /// through the file system, so a symlink pointing out of the root is
    /// refused rather than followed.
    pub fn scan_scope(
        &mut self,
        root: &Path,
        scope: &Path,
        opts: &ScanOptions,
    ) -> Result<ScanReport> {
        self.scan_with(root, Some(scope), opts)
    }

    /// Scans several folders of `root` as one operation: the whole root when
    /// `scopes` is empty, else each folder in turn, with each call leaving
    /// the records outside its own folder alone.
    ///
    /// A cancelled run changes nothing at all: the index is the one the
    /// reader had before the first folder was walked.
    pub fn scan_sources(
        &mut self,
        root: &Path,
        scopes: &[PathBuf],
        opts: &ScanOptions,
    ) -> Result<ScanReport> {
        if scopes.is_empty() {
            return self.scan(root, opts);
        }
        let started = Instant::now();
        let original = self.index.clone();
        let mut total = ScanReport::default();
        for scope in scopes {
            let report = self.scan_scope(root, scope, opts)?;
            total.unchanged += report.unchanged;
            total.added += report.added;
            total.updated += report.updated;
            total.moved += report.moved;
            total.restored += report.restored;
            total.removed += report.removed;
            total.ignored += report.ignored;
            total.thumbnails += report.thumbnails;
            total.sidecars_imported += report.sidecars_imported;
            total.errors.extend(report.errors);
            total.walk_time += report.walk_time;
            total.parse_time += report.parse_time;
            total.cover_time += report.cover_time;
            total.migrated |= report.migrated;
            total.added_books.extend(report.added_books);
            total.candidates.extend(report.candidates);
            if report.cancelled {
                self.index = original;
                total.cancelled = true;
                total.migrated = false;
                total.total_time = started.elapsed();
                return Ok(total);
            }
        }
        total.added_books.sort_by(|a, b| a.id.cmp(&b.id));
        total.candidates.sort_by(|a, b| a.path.cmp(&b.path));
        total.candidates.dedup_by(|a, b| a.path == b.path);
        total.total_time = started.elapsed();
        Ok(total)
    }

    fn scan_with(
        &mut self,
        root: &Path,
        scope: Option<&Path>,
        opts: &ScanOptions,
    ) -> Result<ScanReport> {
        let original = opts.cancel.as_ref().map(|_| self.index.clone());
        let mut result = self.scan_inner(root, scope, opts);
        if let (Ok(report), Some(index)) = (&mut result, original) {
            if report.cancelled {
                self.index = index;
                report.migrated = false;
            }
        }
        result
    }

    fn scan_inner(
        &mut self,
        root: &Path,
        scope: Option<&Path>,
        opts: &ScanOptions,
    ) -> Result<ScanReport> {
        let started = Instant::now();
        let mut report = ScanReport::default();
        let root = fs::canonicalize(scan_root(root))?;
        let cancel = opts.cancel.as_deref();
        // A cancelled scan changes nothing at all, not even the root: the
        // index stays exactly as the reader left it.
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            report.cancelled = true;
            return Ok(report);
        }
        // The folder a scoped scan looks at. `None` means the whole root.
        let scope = match scope {
            Some(s) => Some(resolve_scope(&root, s)?),
            None => None,
        };
        let data_dir = fs::canonicalize(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        if scope.as_deref().is_some_and(|s| s.starts_with(&data_dir)) {
            return Err(Error::scope("the library data directory cannot be scanned"));
        }
        let root_skip = opts.root_skips(&root);
        if let Some(scope) = &scope {
            let relative = scope.strip_prefix(&root).unwrap();
            if scope_excluded(&root, relative, &root_skip) {
                return Err(Error::scope(
                    "the selected folder is excluded from scanning",
                ));
            }
        }
        let root_str = root.to_string_lossy().into_owned();
        if self.index.root.as_deref() != Some(&root_str) {
            report.migrated = self.adopt_root(&root_str);
        }
        if self.covers.spec() != opts.thumb {
            self.covers = CoverCache::new(self.data_dir.join(COVERS_DIR), opts.thumb);
        }

        let mut found = Vec::new();
        let mut unreadable = Vec::new();
        let skip: HashSet<String> = opts.skip_dirs.iter().map(|s| s.to_lowercase()).collect();
        // The library's own data directory can sit anywhere in the store
        // (`--library`); its covers, caches and index are not books.
        // The scope is walked, but the paths in the index stay relative to
        // the root, so the records of the other folders keep their place.
        let walk_root = scope.as_deref().unwrap_or(&root);
        let depth = walk_root.strip_prefix(&root).unwrap().components().count();
        walk(
            &root,
            walk_root,
            depth,
            &skip,
            &root_skip,
            &data_dir,
            cancel,
            &mut found,
            &mut unreadable,
            &mut report.errors,
        );
        report.walk_time = started.elapsed();
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            report.cancelled = true;
            return Ok(report);
        }

        let mut old_books: HashMap<String, BookRecord> = std::mem::take(&mut self.index.books)
            .into_iter()
            .map(|b| (b.file.path.clone(), b))
            .collect();
        let refresh_cbz = self.index.cbz_metadata_version < crate::cbz::METADATA_VERSION;
        // The prefix the paths inside the scope carry; `None` means the
        // whole root, which is every path.
        let scope_prefix = scope
            .as_deref()
            .map(|s| rel_string(&root, s))
            .filter(|p| !p.is_empty());
        // Whether a path lies inside the scope.
        let in_scope = |path: &str| match scope_prefix.as_deref() {
            Some(prefix) => under(path, prefix),
            None => true,
        };
        let mut old_ignored: HashMap<String, FileStamp> = std::mem::take(&mut self.index.ignored)
            .into_iter()
            .filter(|s| {
                // A stale CBZ or ZIP stamp is dropped so the file is looked
                // at again. A file outside the scope is not looked at, so
                // its stamp stays.
                !refresh_cbz
                    || !in_scope(&s.path)
                    || !Path::new(&s.path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| {
                            e.eq_ignore_ascii_case("cbz") || e.eq_ignore_ascii_case("zip")
                        })
            })
            .map(|s| (s.path.clone(), s))
            .collect();
        let mut books = Vec::new();
        let mut ignored = Vec::new();
        let mut pending: Vec<(Found, Option<BookRecord>)> = Vec::new();
        // A full scan does not walk the store's app and system folders. A
        // record the index already holds there (an earlier import the reader
        // has not decided about) is kept untouched rather than counted as
        // removed by a walk that never looked at it.
        if scope_prefix.is_none() {
            let kept: Vec<String> = old_books
                .keys()
                .filter(|path| under_excluded_root(&root, path, &root_skip))
                .cloned()
                .collect();
            for path in kept {
                if let Some(book) = old_books.remove(&path) {
                    books.push(book);
                }
            }
        }
        // A scoped scan looks at one folder. The records of the folders
        // around it are kept as they are: they were not looked at, so they
        // are neither changed nor gone.
        if let Some(prefix) = scope_prefix.as_deref() {
            let outside: Vec<String> = old_books
                .keys()
                .filter(|path| !under(path, prefix))
                .cloned()
                .collect();
            for path in outside {
                if let Some(b) = old_books.remove(&path) {
                    books.push(b);
                }
            }
            let outside: Vec<String> = old_ignored
                .keys()
                .filter(|path| !under(path, prefix))
                .cloned()
                .collect();
            for path in outside {
                if let Some(s) = old_ignored.remove(&path) {
                    ignored.push(s);
                }
            }
        }
        for f in found {
            if let Some(b) = old_books.remove(&f.stamp.path) {
                if b.file.same_file(&f.stamp) && !(refresh_cbz && b.format == Format::Cbz) {
                    if opts.admission.is_staged() {
                        report.candidates.push(ScanCandidate {
                            path: f.stamp.path.clone(),
                            title: b.meta.title.clone(),
                            format: b.format.label().to_string(),
                            indexed: Some(b.id.clone()),
                        });
                    }
                    books.push(b);
                    report.unchanged += 1;
                } else {
                    pending.push((f, Some(b)));
                }
                continue;
            }
            if let Some(s) = old_ignored
                .get(&f.stamp.path)
                .filter(|s| s.same_file(&f.stamp))
            {
                ignored.push(s.clone());
                report.ignored += 1;
                continue;
            }
            pending.push((f, None));
        }
        // A folder or file the walk could not read keeps what the index has
        // below it: a transient permission error, a card pulled at the wrong
        // moment, or a busy mount must not erase books from the shelf. The
        // errors are already in the report, so the reader is told.
        if !unreadable.is_empty() {
            let kept: Vec<String> = old_books
                .keys()
                .filter(|path| under_any(path, &unreadable))
                .cloned()
                .collect();
            for path in kept {
                if let Some(b) = old_books.remove(&path) {
                    books.push(b);
                    report.unchanged += 1;
                }
            }
        }
        // Whatever is left in old_books has vanished from its path: moved or deleted.
        let mut vanished: HashMap<String, BookRecord> =
            old_books.into_values().map(|b| (b.id.clone(), b)).collect();
        let mut taken_ids: HashSet<String> = books.iter().map(|b| b.id.clone()).collect();
        taken_ids.extend(pending.iter().filter_map(|(f, previous)| {
            previous
                .as_ref()
                .filter(|b| refresh_cbz && b.format == Format::Cbz && b.file.same_file(&f.stamp))
                .map(|b| b.id.clone())
        }));
        let now = now();
        let mut cbz_refresh_failed = false;

        // Reserve surviving records' IDs before assigning IDs to new copies.
        pending.sort_by_key(|(_, previous)| previous.is_none());
        let total_pending = pending.len();
        for (index, (f, prev)) in pending.into_iter().enumerate() {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                report.cancelled = true;
                if let Some(p) = prev {
                    books.push(p);
                }
                books.extend(vanished.into_values());
                books.sort_by(|a, b| a.file.path.cmp(&b.file.path));
                ignored.sort_by(|a, b| a.path.cmp(&b.path));
                self.index.books = books;
                self.index.ignored = ignored;
                report.total_time = started.elapsed();
                return Ok(report);
            }
            if let Some(cb) = &opts.on_progress {
                cb(ScanProgress {
                    path: f.stamp.path.clone(),
                    found: books.len(),
                    current: index + 1,
                    total: total_pending,
                    new_book: None,
                });
            }
            let t0 = Instant::now();
            let fmt = match format::sniff(&f.abs) {
                Ok(Some(fmt)) => fmt,
                Ok(None) => {
                    if let Some(previous) = prev {
                        if refresh_cbz
                            && previous.format == Format::Cbz
                            && previous.file.same_file(&f.stamp)
                        {
                            self.index.removed.push(previous);
                            report.removed += 1;
                        } else {
                            report.errors.push((
                                f.stamp.path.clone(),
                                "previously indexed book has unrecognized content".into(),
                            ));
                            taken_ids.insert(previous.id.clone());
                            books.push(previous);
                            continue;
                        }
                    }
                    ignored.push(f.stamp);
                    report.ignored += 1;
                    continue;
                }
                Err(e) => {
                    report.errors.push((f.stamp.path.clone(), e.to_string()));
                    if let Some(previous) = prev {
                        cbz_refresh_failed |= refresh_cbz && previous.format == Format::Cbz;
                        taken_ids.insert(previous.id.clone());
                        books.push(previous);
                    }
                    continue;
                }
            };
            // A staged scan only names what it found: no parsing, no
            // hashing and no cover, so a review list of a whole store stays
            // cheap. The confirm scan reads the books the reader picked.
            if opts.admission.is_staged() {
                let (title, indexed) = match &prev {
                    Some(previous) => (previous.meta.title.clone(), Some(previous.id.clone())),
                    None => (metadata::title_from_filename(&f.abs), None),
                };
                report.candidates.push(ScanCandidate {
                    path: f.stamp.path.clone(),
                    title,
                    format: fmt.label().to_string(),
                    indexed: indexed.clone(),
                });
                if let Some(previous) = prev {
                    taken_ids.insert(previous.id.clone());
                    books.push(previous);
                }
                continue;
            }
            let id = match crate::id::partial_md5(&f.abs) {
                Ok(id) => id,
                Err(e) => {
                    report.errors.push((f.stamp.path.clone(), e.to_string()));
                    if let Some(previous) = prev {
                        cbz_refresh_failed |= refresh_cbz && previous.format == Format::Cbz;
                        taken_ids.insert(previous.id.clone());
                        books.push(previous);
                    }
                    continue;
                }
            };
            // A book whose content the index knows (the same file changed,
            // or a move) is a refresh, not a new local file. A record the
            // reader deleted is *not* one: only an explicit confirmation may
            // bring it back (UX-112 review point 1).
            let known = prev.is_some() || vanished.contains_key(&id);
            if !known && !opts.admission.admits(&f.stamp.path) {
                report.candidates.push(ScanCandidate {
                    path: f.stamp.path.clone(),
                    title: metadata::title_from_filename(&f.abs),
                    format: fmt.label().to_string(),
                    indexed: None,
                });
                continue;
            }
            let ex = match metadata::extract(&f.abs, fmt, f.author_dir.as_deref(), opts.covers) {
                Ok(ex) => ex,
                Err(e) => {
                    report.errors.push((f.stamp.path.clone(), e.to_string()));
                    if let Some(previous) = prev {
                        cbz_refresh_failed |= refresh_cbz && previous.format == Format::Cbz;
                        taken_ids.insert(previous.id.clone());
                        books.push(previous);
                    }
                    continue;
                }
            };
            report.parse_time += t0.elapsed();
            let mut id = id;
            if ex.dictionary {
                ignored.push(f.stamp);
                report.ignored += 1;
                continue;
            }
            let mut cover = None;
            if let Some(bytes) = &ex.cover {
                let t1 = Instant::now();
                let key = CoverCache::key(bytes);
                let existed = self.covers.path(&key).exists();
                match self.covers.store_keyed(&key, bytes, self.covers.spec()) {
                    Ok(()) => {
                        cover = Some(key);
                        report.thumbnails += usize::from(!existed);
                    }
                    Err(e) => report
                        .errors
                        .push((f.stamp.path.clone(), format!("cover: {e}"))),
                }
                report.cover_time += t1.elapsed();
            }
            // Identical copies of one file share an ID; keep IDs unique.
            if scope_prefix.is_some() && taken_ids.contains(&id) {
                if let Some(moved) = books.iter().position(|book| {
                    book.id == id
                        && !in_scope(&book.file.path)
                        && fs::metadata(root.join(&book.file.path))
                            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                }) {
                    let previous = books.remove(moved);
                    taken_ids.remove(&id);
                    vanished.insert(id.clone(), previous);
                }
            }
            if let Some(previous) = prev
                .as_ref()
                .filter(|b| refresh_cbz && b.format == Format::Cbz && b.file.same_file(&f.stamp))
            {
                id = previous.id.clone();
            } else if taken_ids.contains(&id) {
                let base = id.clone();
                let mut n = 2;
                while taken_ids.contains(&id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
            }
            taken_ids.insert(id.clone());
            let mut rec = BookRecord {
                id: id.clone(),
                file: f.stamp,
                format: fmt,
                meta: ex.meta,
                cover,
                added: now,
                last_opened: None,
                status: Status::Unread,
                progress: None,
                settings: Default::default(),
                annotations: Vec::new(),
                annotation_tombstones: Vec::new(),
            };
            if let Some(p) = prev {
                rec.inherit_state(&p);
                report.updated += 1;
            } else if let Some(p) = vanished.remove(&id) {
                rec.inherit_state(&p);
                report.moved += 1;
            } else if let Some(i) = self.index.removed.iter().position(|r| r.id == id) {
                let p = self.index.removed.remove(i);
                rec.inherit_state(&p);
                report.restored += 1;
            } else {
                report.added += 1;
                let authors = rec
                    .meta
                    .authors
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                report.added_books.push(AddedBook {
                    id: rec.id.clone(),
                    title: rec.meta.title.clone(),
                    authors,
                    format: fmt.label().to_string(),
                });
                if let Some(cb) = &opts.on_progress {
                    cb(ScanProgress {
                        path: rec.file.path.clone(),
                        found: books.len() + 1,
                        current: index + 1,
                        total: total_pending,
                        new_book: Some((rec.meta.title.clone(), fmt.label().to_string())),
                    });
                }
                if opts.import_sidecars && apply_sidecars(&mut rec, &f.abs).changed() {
                    report.sidecars_imported += 1;
                }
            }
            books.push(rec);
        }
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            report.cancelled = true;
            return Ok(report);
        }
        report.removed += vanished.len();
        self.index.removed.extend(vanished.into_values());
        self.index.trim_removed(MAX_REMOVED);
        books.sort_by(|a, b| a.file.path.cmp(&b.file.path));
        ignored.sort_by(|a, b| a.path.cmp(&b.path));
        report.candidates.sort_by(|a, b| a.path.cmp(&b.path));
        self.index.books = books;
        self.index.ignored = ignored;
        // The version says that every CBZ in the index has current
        // metadata. A scoped scan looked at one folder only, so the CBZs
        // outside it may still be stale: the version stays where it was,
        // and the next full scan refreshes them.
        if !cbz_refresh_failed && scope_prefix.is_none() {
            self.index.cbz_metadata_version = crate::cbz::METADATA_VERSION;
        }
        report.total_time = started.elapsed();
        Ok(report)
    }

    /// Attaches clippings to books by title (diacritic-insensitive, then by
    /// prefix, since the Kindle sometimes shortens titles), using the author
    /// to break ties. Exact duplicates are skipped, so importing the same
    /// file twice is harmless.
    pub fn import_clippings(&mut self, clippings: &[Clipping]) -> ClippingsImport {
        let mut res = ClippingsImport::default();
        let keys: Vec<(String, String, String)> = self
            .index
            .books
            .iter()
            .map(|b| {
                let authors = text::fold(
                    &b.meta
                        .authors
                        .iter()
                        .map(|a| format!("{} {}", a.name, a.sort))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                (
                    title_key(&b.meta.title),
                    title_key(&metadata::title_from_filename(Path::new(&b.file.path))),
                    authors,
                )
            })
            .collect();
        for c in clippings {
            let key = title_key(&c.title);
            let author = c.author.as_deref().map(text::fold);
            let exact: Vec<usize> = (0..keys.len())
                .filter(|&i| keys[i].0 == key || keys[i].1 == key)
                .collect();
            let candidates = if exact.is_empty() {
                (0..keys.len())
                    .filter(|&i| {
                        let t = &keys[i].0;
                        t.len().min(key.len()) >= 8
                            && (t.starts_with(&key) || key.starts_with(t.as_str()))
                    })
                    .collect()
            } else {
                exact
            };
            let pick = match candidates.len() {
                0 => None,
                1 => Some(candidates[0]),
                _ => candidates
                    .iter()
                    .copied()
                    .find(|&i| {
                        author.as_deref().is_some_and(|a| {
                            a.split(|c: char| !c.is_alphanumeric())
                                .filter(|w| w.len() > 2)
                                .any(|w| keys[i].2.contains(w))
                        })
                    })
                    .or(Some(candidates[0])),
            };
            let Some(i) = pick else {
                if !res.unmatched.contains(&c.title) {
                    res.unmatched.push(c.title.clone());
                }
                continue;
            };
            res.matched += 1;
            if merge_annotation(&mut self.index.books[i], c.to_annotation()) {
                res.added += 1;
            } else {
                res.duplicates += 1;
            }
        }
        res
    }

    /// Imports stock Kindle and KOReader sidecars for every book. Returns
    /// how many books changed.
    pub fn import_sidecars(&mut self) -> SidecarImport {
        let mut res = SidecarImport::default();
        let Some(root) = self.root() else {
            return res;
        };
        for b in &mut self.index.books {
            let outcome = apply_sidecars(b, &root.join(&b.file.path));
            res.changed += usize::from(outcome.changed());
            res.with_sidecars += usize::from(outcome.found);
            res.errors.extend(outcome.errors);
        }
        res
    }
}

/// Result of [`Library::import_sidecars`].
#[derive(Debug, Default)]
pub struct SidecarImport {
    /// Books whose progress or annotations changed.
    pub changed: usize,
    /// Books with a readable stock or KOReader sidecar, changed or not. A
    /// scan already imports the sidecars of new books, so a later import
    /// usually finds them up to date.
    pub with_sidecars: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SidecarOutcome {
    /// A stock or KOReader sidecar was found and parsed.
    pub found: bool,
    pub progress: bool,
    pub annotations: usize,
    pub errors: Vec<String>,
}

impl SidecarOutcome {
    pub fn changed(&self) -> bool {
        self.progress || self.annotations > 0
    }
}

/// Merges what the `.sdr` folder knows into `rec`. Progress is taken only
/// if it is newer than what the record has, so a re-import never rewinds a
/// book that was read further in Aristotle.
pub fn apply_sidecars(rec: &mut BookRecord, abs: &Path) -> SidecarOutcome {
    let sc = sidecar::read_sidecars(abs);
    let mut out = SidecarOutcome {
        found: sc.kindle.is_some() || sc.koreader.is_some(),
        errors: sc.errors,
        ..Default::default()
    };
    let mut candidates: Vec<(Progress, Option<Status>)> = Vec::new();

    if let Some(k) = &sc.koreader {
        // KOReader stores the same partial MD5 we use; a mismatch means the
        // sidecar describes another version of the file.
        if k.partial_md5
            .as_deref()
            .is_none_or(|m| rec.id.starts_with(m))
        {
            let locator = match (&k.last_xpointer, k.last_page) {
                (Some(x), _) if rec.format == Format::Epub => Some(Locator::XPointer {
                    xpointer: x.clone(),
                }),
                (_, Some(p)) if matches!(rec.format, Format::Pdf | Format::Cbz) => {
                    Some(Locator::Page {
                        index: p.saturating_sub(1),
                    })
                }
                _ => None,
            };
            if let (Some(f), Some(locator)) = (k.percent_finished, locator) {
                let p = Progress {
                    fraction: f.clamp(0.0, 1.0) as f32,
                    locator,
                    updated: k.modified.unwrap_or(0),
                    source: Some("koreader".into()),
                    page: None,
                };
                candidates.push((p, k.status));
            }
        }
    }
    if let Some(st) = &sc.kindle {
        if let (Some((pos, ts)), Format::Mobi | Format::Azw3) = (&st.last_read, rec.format) {
            if let Some(position) = sidecar::kindle_position(pos) {
                let lens = crate::mobi::text_lengths(abs).ok();
                let kf8 = sc
                    .kindle_ext
                    .as_deref()
                    .is_some_and(|e| e.starts_with("azw3"));
                let len = lens
                    .and_then(|(m, k)| if kf8 { k.or(Some(m)) } else { Some(m) })
                    .filter(|&l| l > 0);
                // The text length is exact; the page map is the fallback
                // (its last page start is close to the end of the text).
                let fraction = match len {
                    Some(len) => Some(position as f64 / len as f64),
                    None => sc.page_map.as_ref().and_then(|m| m.fraction(position)),
                };
                if let Some(fraction) = fraction {
                    let page = sc
                        .page_map
                        .as_ref()
                        .and_then(|m| Some(m.label(m.page_index(position)?)));
                    let p = Progress {
                        fraction: fraction.clamp(0.0, 1.0) as f32,
                        locator: Locator::Kindle { position },
                        updated: ts.map(|t| t / 1000).unwrap_or(0),
                        source: Some("kindle".into()),
                        page,
                    };
                    candidates.push((p, None));
                }
            }
        }
        for a in &st.annotations {
            out.annotations += usize::from(merge_annotation(rec, a.clone()));
        }
    }
    if let Some((p, status)) = candidates.into_iter().max_by_key(|(p, _)| p.updated) {
        if rec
            .progress
            .as_ref()
            .is_none_or(|old| old.updated < p.updated)
        {
            rec.status = match status {
                Some(s) => s,
                None if p.fraction >= 0.99 => Status::Finished,
                None => Status::Reading,
            };
            rec.progress = Some(p);
            out.progress = true;
        }
    }
    out
}

fn merge_annotation(rec: &mut BookRecord, a: Annotation) -> bool {
    let dup = rec.annotations.iter().any(|x| {
        x.kind == a.kind
            && x.location == a.location
            && x.page == a.page
            && x.text == a.text
            && (x.position == a.position || x.position.is_none() || a.position.is_none())
    });
    if dup {
        return false;
    }
    rec.annotations.push(a);
    true
}

/// Whether a file is a log dump the Kindle writes into `documents`, such as
/// `Nov_09_14.14.09_2026.txt` (seen on an Oasis 2): an HTML summary of its
/// logs, not a book.
fn is_device_log_dump(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".txt") else {
        return false;
    };
    let parts: Vec<&str> = stem.split('_').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let [month, day, time, year] = parts[..] else {
        return false;
    };
    let clock: Vec<&str> = time.split('.').collect();
    MONTHS.contains(&month)
        && digits(day, 2)
        && digits(year, 4)
        && clock.len() == 3
        && clock.iter().all(|c| digits(c, 2))
}

/// `path` relative to `root` with `/` separators, the form the index keeps.
fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The folder used to fill in a missing author. On the device books live in
/// `documents/<Author>/`, so the author is the first folder below
/// `documents`, wherever that sits under the scan root. Books outside
/// `documents` use their first folder below the root, which keeps the same
/// grouping the shelf shows.
fn author_dir_for(rel: &Path) -> Option<String> {
    let names: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let names = match names.first() {
        Some(first) if first.eq_ignore_ascii_case("documents") => &names[1..],
        _ => &names[..],
    };
    (names.len() > 1).then(|| names[0].to_string())
}

/// `path` relative to `ancestor`, as the `/`-separated prefix that leads
/// back down, or `None` when `path` is not below `ancestor`.
pub(crate) fn path_prefix(ancestor: &str, path: &str) -> Option<String> {
    let rest = Path::new(path).strip_prefix(Path::new(ancestor)).ok()?;
    let mut parts = Vec::new();
    for c in rest.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// `prefix` + `/` + `path`, skipping the separator for an empty prefix.
pub(crate) fn with_prefix(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_string()
    } else if path.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}/{path}")
    }
}

/// Whether the indexed path is the given relative path or lies below it.
fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|r| r.starts_with('/'))
}

/// Whether an indexed path starts in a folder the walk does not enter. A
/// full scan keeps such a record as it is instead of treating a skipped
/// folder as a deletion.
fn under_excluded_root(root: &Path, path: &str, root_skip: &HashSet<String>) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    let name = first.to_lowercase();
    root_skip.contains(&name) || (root == Path::new(KINDLE_STORE) && excluded_store_folder(&name))
}

/// Whether an indexed path is the given relative path or lies below it.
fn under_any(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| under(path, p))
}

/// Whether an explicit scope under `root` starts in an excluded folder. A
/// folder the picker hides cannot be scanned by naming it directly either.
fn scope_excluded(root: &Path, relative: &Path, root_skip: &HashSet<String>) -> bool {
    relative.components().next().is_some_and(|part| {
        let name = part.as_os_str().to_string_lossy().to_lowercase();
        root_skip.contains(&name)
            || (root == Path::new(KINDLE_STORE) && excluded_store_folder(&name))
    })
}

/// Walks `dir`, collecting candidate books. `unreadable` gets the relative
/// paths of folders and files that could not be listed or stat-ed, so the
/// scan keeps the records the index already has there.
#[allow(clippy::too_many_arguments)]
fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    skip: &HashSet<String>,
    root_skip: &HashSet<String>,
    exclude: &Path,
    cancel: Option<&AtomicBool>,
    out: &mut Vec<Found>,
    unreadable: &mut Vec<String>,
    errors: &mut Vec<(String, String)>,
) {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push((dir.to_string_lossy().into_owned(), e.to_string()));
            unreadable.push(rel_string(root, dir));
            return;
        }
    };
    for entry in entries {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // The listing is incomplete: keep what the index has here.
                errors.push((dir.to_string_lossy().into_owned(), e.to_string()));
                unreadable.push(rel_string(root, dir));
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            errors.push((
                entry.path().to_string_lossy().into_owned(),
                "file name is not UTF-8".into(),
            ));
            unreadable.push(rel_string(root, &entry.path()));
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                errors.push((path.to_string_lossy().into_owned(), e.to_string()));
                unreadable.push(rel_string(root, &path));
                continue;
            }
        };
        let lower = name.to_lowercase();
        if ft.is_dir() {
            if lower.ends_with(".sdr")
                || skip.contains(&lower)
                || (depth == 0
                    && (root_skip.contains(&lower)
                        || (root == Path::new(KINDLE_STORE) && excluded_store_folder(&lower))))
                || path == exclude
            {
                continue;
            }
            walk(
                root,
                &path,
                depth + 1,
                skip,
                root_skip,
                exclude,
                cancel,
                out,
                unreadable,
                errors,
            );
            continue;
        }
        // Symlinks to files are followed, symlinks to folders are not (loops).
        if ft.is_symlink() && fs::canonicalize(&path).is_ok_and(|target| !target.starts_with(root))
        {
            let relative = rel_string(root, &path);
            errors.push((
                relative.clone(),
                "symbolic link target is outside the scan root".into(),
            ));
            unreadable.push(relative);
            continue;
        }
        let md = match fs::metadata(&path) {
            Ok(md) => md,
            Err(e) => {
                errors.push((path.to_string_lossy().into_owned(), e.to_string()));
                unreadable.push(rel_string(root, &path));
                continue;
            }
        };
        if !md.is_file() || !format::is_candidate(&path) || is_device_log_dump(name) {
            continue;
        }
        let rel = rel_string(root, &path);
        if matches!(
            rel.to_lowercase().as_str(),
            "quotes.txt" | "documents/quotes.txt" | "inkread-probe.txt"
        ) {
            continue;
        }
        let author_dir = author_dir_for(Path::new(&rel));
        let (mtime, mtime_ns) = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
            .unwrap_or((0, 0));
        out.push(Found {
            abs: path,
            stamp: FileStamp {
                path: rel,
                size: md.len(),
                mtime,
                mtime_ns,
            },
            author_dir,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_exclusions_apply_only_to_the_device_store() {
        let options = ScanOptions::default();
        let names = options.root_skips(Path::new(KINDLE_STORE));
        for name in [
            "aristotle",
            "koreader",
            "system",
            "extensions",
            "aristotle-notes",
            "usbnet",
            "linkss",
            "mkk",
            "mrpackages",
            "rp",
            "libkh",
            "inkread-lab",
        ] {
            assert!(names.contains(name), "{name} must keep the device list");
        }
        assert!(!names.contains("documents"));
        assert!(!names.contains("books"));
        assert!(!names.contains("download"));
        assert!(options.root_skips(Path::new("/custom/books")).is_empty());
        let options = ScanOptions {
            root_skip_dirs: vec!["CUSTOM".into()],
            ..Default::default()
        };
        assert!(options
            .root_skips(Path::new("/custom/books"))
            .contains("custom"));
    }

    /// The shared blacklist names the app and tool folders the store listing
    /// found on 2026-11-12, plus the versioned Aristotle bundles. It never
    /// matches a book file name or a nested user folder.
    #[test]
    fn the_shared_blacklist_matches_store_folders_not_books() {
        for name in [
            "SYSTEM",
            "aristotle",
            "Aristotle-Notes",
            "aristotle-probe",
            "aristotle-pre-app007",
            "aristotle-pre-ui-u1",
            "aristotle-v0.1.0",
            "usbnet",
            "libkh",
        ] {
            assert!(excluded_store_folder(name), "{name} is an app folder");
        }
        for name in ["documents", "books", "Download", "system.txt", "墨阅书库"] {
            assert!(!excluded_store_folder(name), "{name} is not a folder rule");
        }
        let skip: HashSet<String> = HashSet::new();
        assert!(scope_excluded(
            Path::new(KINDLE_STORE),
            Path::new("aristotle-v0.1.0/books"),
            &skip
        ));
        assert!(!scope_excluded(
            Path::new(KINDLE_STORE),
            Path::new("documents"),
            &skip
        ));
        assert!(!scope_excluded(
            Path::new("/home/reader/documents"),
            Path::new("aristotle-v0.1.0"),
            &skip
        ));
    }

    #[test]
    fn the_library_destination_must_be_inside_the_root_and_visible() {
        let dir = crate::testdir("destination");
        let root = dir.join("documents");
        fs::create_dir_all(&root).unwrap();
        let inside = root.join("墨阅书库");
        assert_eq!(
            resolve_library_destination(&root, &inside).unwrap(),
            fs::canonicalize(&root).unwrap().join("墨阅书库")
        );
        // The user store root itself is a valid explicit choice.
        assert_eq!(
            resolve_library_destination(&root, &root).unwrap(),
            fs::canonicalize(&root).unwrap()
        );
        assert!(resolve_library_destination(&root, &dir.join("outside")).is_err());
        assert!(resolve_library_destination(&root, Path::new("relative")).is_err());
        // A hidden saved setting cannot pass the public helper.
        assert!(resolve_library_destination(&root, &root.join(".aristotle")).is_err());
        assert!(resolve_library_destination(&root, &root.join(".hidden/Books")).is_err());
        // A nested user folder with a blacklisted name is the reader's own.
        let nested = root.join("documents/system/Books");
        assert!(resolve_library_destination(&root, &nested).is_ok());
    }

    /// A migration record updates the file of a live book without touching
    /// its progress or marks, and a book the reader deleted while the worker
    /// ran is not brought back by the stale record.
    #[test]
    fn imported_records_keep_live_state_and_never_resurrect_a_deleted_book() {
        let dir = crate::testdir("apply-imported");
        let root = dir.join("documents");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Lalka.txt"), b"Lalka\n").unwrap();
        fs::write(root.join("Dziady.txt"), b"Dziady\n").unwrap();
        let mut lib = Library::open(dir.join("data")).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let lalka = lib
            .books()
            .iter()
            .find(|b| b.file.path == "Lalka.txt")
            .unwrap()
            .clone();
        let dziady = lib
            .books()
            .iter()
            .find(|b| b.file.path == "Dziady.txt")
            .unwrap()
            .clone();
        lib.set_progress(
            &lalka.id,
            Progress {
                fraction: 0.42,
                locator: Locator::Text { offset: 0 },
                updated: 7,
                source: None,
                page: None,
            },
        );
        let mut moved = lalka.clone();
        moved.file.path = "墨阅书库/Lalka.txt".into();
        // The reader deleted Dziady after the worker's scan.
        lib.remove_book(&dziady.id);
        let mut stale = dziady.clone();
        stale.file.path = "墨阅书库/Dziady.txt".into();
        let admitted = lib.apply_imported(&root, vec![moved, stale], &Index::default());
        assert_eq!(admitted.len(), 1, "only the live book is authorized");
        assert_eq!(admitted[0], lalka.id);
        let rec = lib.book(&lalka.id).unwrap();
        assert_eq!(rec.file.path, "墨阅书库/Lalka.txt");
        assert_eq!(rec.progress.as_ref().unwrap().fraction, 0.42);
        assert!(lib.book(&dziady.id).is_none(), "deleted book stays deleted");
        assert_eq!(lib.books().len(), 1);
    }

    #[test]
    fn device_log_dumps_are_not_books() {
        assert!(is_device_log_dump("Nov_09_14.14.09_2026.txt"));
        assert!(!is_device_log_dump("Nov_09_2026.txt"));
        assert!(!is_device_log_dump("Nov_09_14.14.09_2026.epub"));
        assert!(!is_device_log_dump("红楼梦.txt"));
        assert!(!is_device_log_dump("My Clippings.txt"));
    }

    /// The stock documents folder is widened to the user store; any other
    /// root, including a custom one, is scanned as given.
    #[test]
    fn the_stock_documents_root_widens_to_the_user_store() {
        assert_eq!(
            scan_root(Path::new(KINDLE_DOCUMENTS)),
            PathBuf::from(KINDLE_STORE)
        );
        assert_eq!(
            scan_root(Path::new("/mnt/us/books")),
            PathBuf::from("/mnt/us/books")
        );
        assert_eq!(
            scan_root(Path::new("/home/reader/documents")),
            PathBuf::from("/home/reader/documents")
        );
    }

    /// The author fallback is the folder below `documents`, wherever
    /// `documents` sits under the scan root, and never the `documents`
    /// folder itself.
    #[test]
    fn the_author_fallback_skips_the_documents_folder() {
        let dir = |p: &str| author_dir_for(Path::new(p));
        assert_eq!(dir("documents/Prus/Lalka.mobi").as_deref(), Some("Prus"));
        assert_eq!(dir("documents/Lalka.mobi"), None);
        assert_eq!(
            dir("documents/My Books/Lalka.mobi").as_deref(),
            Some("My Books")
        );
        assert_eq!(dir("Books/Prus/Lalka.mobi").as_deref(), Some("Books"));
        assert_eq!(dir("Lalka.mobi"), None);
        // A custom root that is itself a documents folder keeps the old rule.
        assert_eq!(dir("Prus/Lalka.mobi").as_deref(), Some("Prus"));
    }

    #[test]
    fn ancestor_prefixes_rebase_relative_paths() {
        assert_eq!(
            path_prefix("/mnt/us", "/mnt/us/documents").as_deref(),
            Some("documents")
        );
        assert_eq!(
            path_prefix("/mnt/us", "/mnt/us/documents/Books").as_deref(),
            Some("documents/Books")
        );
        assert_eq!(path_prefix("/mnt/us", "/mnt/us"), None);
        assert_eq!(path_prefix("/mnt/us", "/mnt/used"), None);
        assert_eq!(path_prefix("/mnt/us", "/mnt"), None);
        assert_eq!(
            with_prefix("documents", "Platon/Uczta.epub"),
            "documents/Platon/Uczta.epub"
        );
        assert_eq!(with_prefix("", "Platon/Uczta.epub"), "Platon/Uczta.epub");
    }

    #[test]
    fn unreadable_prefixes_cover_their_whole_subtree() {
        let prefixes = vec!["documents/Kept".to_string(), String::new()];
        assert!(under_any("documents/Kept/Book.epub", &prefixes));
        assert!(under_any("documents/Kept/Nested/Book.epub", &prefixes));
        assert!(!under_any("documents/Keptish/Book.epub", &prefixes[..1]));
        assert!(!under_any("documents/Other/Book.epub", &prefixes[..1]));
        assert!(under_any("anything.epub", &prefixes[1..]));
    }
    use crate::index::AnnotationKind;

    /// The device case that once imported nothing: a MOBI under an author
    /// folder whose `.sdr` holds `.mbs`, `.mbp1` and `.apnx` in the layouts
    /// seen on firmware 5.13.6, with an `lpr` of "pos:pos:n:base64" form.
    #[test]
    fn device_layout_sidecars_import_position_and_percent() {
        let dir = crate::testdir("device-sdr");
        let root = dir.join("documents");
        let book = root.join("Adam Mickiewicz/Pan Tadeusz - Adam Mickiewicz.mobi");
        let sdr = root.join("Adam Mickiewicz/Pan Tadeusz - Adam Mickiewicz.sdr");
        fs::create_dir_all(&sdr).unwrap();
        // No text length in record 0, so the page map supplies the percent.
        fs::write(
            &book,
            crate::mobi::testutil::build(
                "Pan Tadeusz",
                &[(100, "Adam Mickiewicz".as_bytes())],
                &[],
                6,
            ),
        )
        .unwrap();
        let pages = sidecar::testutil::even_pages(180);
        let lpr = "7510:7510:11:REFUQQAAAHxFQkFSAAAAAQAAAABFQlZT";
        fs::write(
            sdr.join("Pan Tadeusz - Adam Mickiewicz.mbs"),
            sidecar::testutil::device_mbs(lpr, "84521", 1_792_229_945_807),
        )
        .unwrap();
        fs::write(
            sdr.join("Pan Tadeusz - Adam Mickiewicz.mbp1"),
            sidecar::testutil::device_mbp1(&pages),
        )
        .unwrap();
        fs::write(
            sdr.join("Pan Tadeusz - Adam Mickiewicz.apnx"),
            sidecar::testutil::apnx(&pages, "(1,a,1)"),
        )
        .unwrap();

        // Scan without the automatic import, then import explicitly, as the
        // CLI's import-sidecars does.
        let mut lib = Library::open(dir.join("data")).unwrap();
        let opts = ScanOptions {
            import_sidecars: false,
            covers: false,
            ..ScanOptions::default()
        };
        lib.scan(&root, &opts).unwrap();
        assert!(lib.books()[0].progress.is_none());
        let r = lib.import_sidecars();
        assert_eq!((r.changed, r.with_sidecars, r.errors.len()), (1, 1, 0));
        let b = &lib.books()[0];
        let p = b.progress.as_ref().unwrap();
        assert_eq!(p.locator, Locator::Kindle { position: 7510 });
        assert_eq!(
            (p.page.as_deref(), p.updated, b.status),
            (Some("4"), 1_792_229_945, Status::Reading)
        );
        assert_eq!((p.fraction * 100.0).round(), 2.0, "stock reader shows 2 %");

        // With the text length known, it is used instead (exact).
        let mut mobi = crate::mobi::testutil::build("Pan Tadeusz", &[], &[], 6);
        mobi[88 + 4..88 + 8].copy_from_slice(&375_500u32.to_be_bytes());
        fs::write(&book, mobi).unwrap();
        let mut lib = Library::open(dir.join("data2")).unwrap();
        lib.scan(
            &root,
            &ScanOptions {
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
        let p = lib.books()[0].progress.clone().unwrap();
        assert!((p.fraction - 0.02).abs() < 1e-6, "{}", p.fraction);
    }

    #[test]
    fn kindle_sidecar_progress_and_annotations() {
        let dir = crate::testdir("kindle-sdr");
        let root = dir.join("documents");
        fs::create_dir_all(root.join("Prus/Lalka.sdr")).unwrap();
        let mut mobi = crate::mobi::testutil::build("Lalka", &[], &[], 6);
        // Record 0 starts after the 78-byte header, one 8-byte entry and 2 pad
        // bytes; its bytes 4..8 hold the uncompressed text length.
        mobi[88 + 4..88 + 8].copy_from_slice(&30_000u32.to_be_bytes());
        fs::write(root.join("Prus/Lalka.mobi"), mobi).unwrap();
        fs::write(
            root.join("Prus/Lalka.sdr/Lalka.mbs"),
            sidecar::testutil::sample(),
        )
        .unwrap();

        let mut lib = Library::open(dir.join("data")).unwrap();
        let r = lib.scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!((r.added, r.sidecars_imported), (1, 1));
        let b = &lib.books()[0];
        assert_eq!(b.status, Status::Reading);
        let p = b.progress.as_ref().unwrap();
        assert_eq!(
            (p.fraction, &p.locator, p.updated),
            (0.5, &Locator::Kindle { position: 15_000 }, 1_702_585_665)
        );
        assert_eq!(
            b.annotations
                .iter()
                .map(|a| a.kind.clone())
                .collect::<Vec<_>>(),
            [AnnotationKind::Highlight, AnnotationKind::Note]
        );

        // Importing again adds nothing and never rewinds newer progress.
        let id = b.id.clone();
        lib.set_progress(
            &id,
            Progress {
                fraction: 0.9,
                locator: Locator::Kindle { position: 27_000 },
                updated: 2_000_000_000,
                source: None,
                page: None,
            },
        );
        assert_eq!(lib.import_sidecars().changed, 0);
        assert_eq!(
            lib.book(&id).unwrap().progress.as_ref().unwrap().fraction,
            0.9
        );
    }

    #[test]
    fn a_scanned_pdf_gets_its_page_image_and_a_large_thumbnail_on_demand() {
        let dir = crate::testdir("pdf-cover");
        let root = dir.join("documents");
        fs::create_dir_all(&root).unwrap();
        // Page one of a scanned book: a 400 × 600 image stored with Flate.
        let (w, h) = (400u32, 600u32);
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&vec![180u8; (w * h * 3) as usize], 6);
        fs::write(
            root.join("Skan.pdf"),
            crate::pdf::testutil::one_page_image("FlateDecode", w, h, "DeviceRGB", &z),
        )
        .unwrap();

        let mut lib = Library::open(dir.join("data")).unwrap();
        let report = lib.scan(&root, &ScanOptions::default()).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let rec = lib.books()[0].clone();
        let key = rec.cover.clone().expect("the page image is the cover");
        assert_eq!(key.len(), 32, "the key is the image's MD5");
        let shelf = crate::cover::read_pgm(&lib.cover_path(&rec).unwrap()).unwrap();
        assert_eq!((shelf.width, shelf.height), (200, 300));

        // The screensaver's size is made from the book when it is first
        // needed, and the shelf thumbnail is left alone.
        let large = crate::cover::ThumbSpec::large();
        assert!(lib.cover_path_sized(&rec, large).is_none());
        let path = lib.ensure_cover_path(&rec, large).expect("large thumbnail");
        assert_eq!(path, lib.cover_path_sized(&rec, large).unwrap());
        let hero = crate::cover::read_pgm(&path).unwrap();
        assert_eq!((hero.width, hero.height), (560, 840));
        assert!(hero.pixels.iter().all(|&v| v % 17 == 0));
    }

    /// A scoped scan looks at one folder. The books outside it keep their
    /// records, progress and annotations, and only the books inside it that
    /// are gone are removed.
    #[test]
    fn a_scoped_scan_keeps_the_books_outside_the_scope() {
        let dir = crate::testdir("scoped-scan");
        let root = dir.join("store");
        let inside = root.join("documents/novels");
        let outside = root.join("documents/essays");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let mobi = |title: &str| crate::mobi::testutil::build(title, &[], &[], 6);
        fs::write(inside.join("Lalka.mobi"), mobi("Lalka")).unwrap();
        fs::write(outside.join("Inny.mobi"), mobi("Inny")).unwrap();

        let mut lib = Library::open(dir.join("data")).unwrap();
        let report = lib.scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!((report.added, report.removed), (2, 0));
        let outside_id = lib
            .books()
            .iter()
            .find(|b| b.file.path == "documents/essays/Inny.mobi")
            .expect("the book outside the scope")
            .id
            .clone();
        lib.set_progress(
            &outside_id,
            Progress {
                fraction: 0.42,
                locator: Locator::Kindle { position: 1234 },
                updated: 1_700_000_000,
                source: None,
                page: None,
            },
        );

        // The book inside the scope is gone; the one outside it is not.
        fs::remove_file(inside.join("Lalka.mobi")).unwrap();
        let report = lib
            .scan_scope(&root, &inside, &ScanOptions::default())
            .unwrap();
        assert_eq!(report.removed, 1, "{:?}", report);
        assert_eq!(lib.books().len(), 1);
        let kept = &lib.books()[0];
        assert_eq!(kept.id, outside_id);
        assert_eq!(kept.file.path, "documents/essays/Inny.mobi");
        assert_eq!(
            kept.progress.as_ref().map(|p| p.fraction),
            Some(0.42),
            "the progress of a book outside the scope survives"
        );
        assert_eq!(lib.removed().len(), 1);
        assert_eq!(lib.removed()[0].file.path, "documents/novels/Lalka.mobi");
    }

    /// A scoped scan that adds a book leaves the books around it alone.
    #[test]
    fn a_scoped_scan_adds_only_what_is_inside_the_scope() {
        let dir = crate::testdir("scoped-scan-add");
        let root = dir.join("store");
        let inside = root.join("documents/novels");
        let outside = root.join("documents/essays");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let mobi = |title: &str| crate::mobi::testutil::build(title, &[], &[], 6);
        fs::write(outside.join("Inny.mobi"), mobi("Inny")).unwrap();

        let mut lib = Library::open(dir.join("data")).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        fs::write(inside.join("Lalka.mobi"), mobi("Lalka")).unwrap();
        let report = lib
            .scan_scope(&root, &inside, &ScanOptions::default())
            .unwrap();
        assert_eq!((report.added, report.removed, report.unchanged), (1, 0, 0));
        let paths: Vec<&str> = lib.books().iter().map(|b| b.file.path.as_str()).collect();
        assert_eq!(
            paths,
            ["documents/essays/Inny.mobi", "documents/novels/Lalka.mobi"]
        );
    }

    /// The scope has to be a real folder under the scan root: a symlink
    /// pointing out of it is refused, and so is a folder that is not there.
    #[test]
    fn a_scope_outside_the_root_is_refused() {
        let dir = crate::testdir("scoped-scan-refused");
        let root = dir.join("store");
        let other = dir.join("elsewhere");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&other).unwrap();
        let mut lib = Library::open(dir.join("data")).unwrap();
        let opts = ScanOptions::default();

        assert!(matches!(
            lib.scan_scope(&root, &other, &opts),
            Err(Error::Scope(_))
        ));
        assert!(matches!(
            lib.scan_scope(&root, &root.join("missing"), &opts),
            Err(Error::Scope(_))
        ));
        let data = root.join("reader-data");
        fs::create_dir_all(data.join("pages")).unwrap();
        let mut inside = Library::open(&data).unwrap();
        for scope in [&data, &data.join("pages")] {
            assert!(matches!(
                inside.scan_scope(&root, scope, &opts),
                Err(Error::Scope(_))
            ));
            assert!(inside.index().root.is_none());
        }

        // A symlink inside the root that points out of it is refused too.
        let link = root.join("escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, &link).unwrap();
        #[cfg(unix)]
        assert!(matches!(
            lib.scan_scope(&root, &link, &opts),
            Err(Error::Scope(_))
        ));
    }

    #[test]
    fn scoped_move_preserves_state_and_real_copies_keep_unique_ids() {
        let dir = crate::testdir("scoped-move");
        let root = dir.join("store");
        let selected = root.join("selected");
        fs::create_dir_all(&selected).unwrap();
        let old = root.join("original.mobi");
        fs::write(
            &old,
            crate::mobi::testutil::build("Scoped move", &[], &[], 6),
        )
        .unwrap();
        let mut lib = Library::open(dir.join("data")).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = lib.books()[0].id.clone();
        let record = lib.book_mut(&id).unwrap();
        record.progress = Some(Progress {
            fraction: 0.42,
            locator: Locator::Kindle { position: 42 },
            updated: 1,
            source: None,
            page: None,
        });
        record.annotations.push(
            serde_json::from_str(r#"{"id":"saved-mark","kind":"bookmark","source":"aristotle"}"#)
                .unwrap(),
        );
        let before = record.clone();
        let moved = selected.join("moved.mobi");
        fs::rename(&old, &moved).unwrap();
        let report = lib
            .scan_scope(&root, &selected, &ScanOptions::default())
            .unwrap();
        assert_eq!(report.moved, 1);
        assert_eq!(lib.books().len(), 1);
        let mut expected = before;
        expected.file = lib.books()[0].file.clone();
        expected.meta = lib.books()[0].meta.clone();
        expected.cover = lib.books()[0].cover.clone();
        assert_eq!(&expected, lib.book(&id).unwrap());
        fs::copy(&moved, selected.join("copy.mobi")).unwrap();
        lib.scan_scope(&root, &selected, &ScanOptions::default())
            .unwrap();
        assert_eq!(lib.books().len(), 2);
        assert_ne!(lib.books()[0].id, lib.books()[1].id);
        lib.scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(lib.book(&id).unwrap().progress, expected.progress);
        assert_eq!(lib.book(&id).unwrap().annotations, expected.annotations);
    }

    #[cfg(unix)]
    #[test]
    fn file_symlinks_stay_within_the_scan_root() {
        let dir = crate::testdir("scoped-file-symlink");
        let root = dir.join("store");
        let selected = root.join("selected");
        fs::create_dir_all(&selected).unwrap();
        let outside = dir.join("outside.mobi");
        fs::write(
            &outside,
            crate::mobi::testutil::build("Outside", &[], &[], 6),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, selected.join("escape.mobi")).unwrap();
        let mut lib = Library::open(dir.join("data")).unwrap();
        let report = lib
            .scan_scope(&root, &selected, &ScanOptions::default())
            .unwrap();
        assert!(lib.books().is_empty());
        assert_eq!(report.errors.len(), 1);
        fs::copy(&outside, root.join("inside.mobi")).unwrap();
        std::os::unix::fs::symlink(root.join("inside.mobi"), selected.join("allowed.mobi"))
            .unwrap();
        lib.scan_scope(&root, &selected, &ScanOptions::default())
            .unwrap();
        assert_eq!(lib.books().len(), 1);
        assert_eq!(lib.books()[0].file.path, "selected/allowed.mobi");
        let opts = ScanOptions {
            root_skip_dirs: vec!["selected".into()],
            ..Default::default()
        };
        assert!(matches!(
            lib.scan_scope(&root, &selected, &opts),
            Err(Error::Scope(_))
        ));
    }

    /// A scope that is the root itself scans like a full scan: the folders
    /// the store keeps for itself are left out.
    #[test]
    fn a_scope_that_is_the_root_skips_the_same_folders_as_a_full_scan() {
        let dir = crate::testdir("scoped-scan-root");
        let root = dir.join("store");
        fs::create_dir_all(root.join("system")).unwrap();
        fs::create_dir_all(root.join("documents")).unwrap();
        let mobi = |title: &str| crate::mobi::testutil::build(title, &[], &[], 6);
        fs::write(root.join("system/Hidden.mobi"), mobi("Hidden")).unwrap();
        fs::write(root.join("documents/Kept.mobi"), mobi("Kept")).unwrap();
        let opts = ScanOptions {
            root_skip_dirs: vec!["system".into()],
            ..ScanOptions::default()
        };

        let mut full = Library::open(dir.join("data-full")).unwrap();
        let report = full.scan(&root, &opts).unwrap();
        assert_eq!(report.added, 1, "{:?}", report);
        assert_eq!(full.books()[0].file.path, "documents/Kept.mobi");

        let mut scoped = Library::open(dir.join("data-scoped")).unwrap();
        let report = scoped.scan_scope(&root, &root, &opts).unwrap();
        assert_eq!(report.added, 1, "{:?}", report);
        assert_eq!(scoped.books().len(), 1);
        assert_eq!(scoped.books()[0].file.path, "documents/Kept.mobi");
    }

    /// A scoped scan that leaves stale CBZ files outside the scope alone
    /// does not say that the whole index has current metadata, and it keeps
    /// the stamps of the files it did not look at.
    #[test]
    fn a_scoped_scan_keeps_the_cbz_version_and_the_stamps_outside_the_scope() {
        let dir = crate::testdir("scoped-scan-cbz");
        let root = dir.join("store");
        let inside = root.join("documents/novels");
        let outside = root.join("documents/essays");
        fs::create_dir_all(&inside).unwrap();
        fs::create_dir_all(&outside).unwrap();
        // A file that looks like a book but is not: the scan ignores it and
        // keeps its stamp.
        fs::write(outside.join("broken.cbz"), b"not a book").unwrap();
        fs::write(
            inside.join("Lalka.mobi"),
            crate::mobi::testutil::build("Lalka", &[], &[], 6),
        )
        .unwrap();

        let mut lib = Library::open(dir.join("data")).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(lib.index().ignored.len(), 1);
        assert_eq!(lib.index().ignored[0].path, "documents/essays/broken.cbz");
        // The index is older than the CBZ parser, so the next scan refreshes
        // the CBZ files.
        lib.index.cbz_metadata_version = lib.index().cbz_metadata_version.saturating_sub(1);
        let outdated = lib.index().cbz_metadata_version;

        lib.scan_scope(&root, &inside, &ScanOptions::default())
            .unwrap();
        assert_eq!(
            lib.index().cbz_metadata_version,
            outdated,
            "a scoped scan must not say that every CBZ has current metadata"
        );
        assert_eq!(
            lib.index().ignored.len(),
            1,
            "the stamp of the file outside the scope is kept"
        );
        assert_eq!(lib.index().ignored[0].path, "documents/essays/broken.cbz");
    }

    #[test]
    fn an_explicitly_reselected_removed_book_can_return_with_its_history() {
        let dir = crate::testdir("confirmed-restore");
        let root = dir.join("books");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("book.txt"), b"Original history restore text.").unwrap();
        let mut lib = Library::open(dir.join("data")).unwrap();
        lib.scan(&root, &ScanOptions::default()).unwrap();
        let id = lib.books()[0].id.clone();
        lib.set_progress(
            &id,
            Progress {
                fraction: 0.4,
                locator: Locator::Text { offset: 4 },
                updated: 4,
                source: None,
                page: None,
            },
        );
        let mut moved = lib.remove_book(&id).unwrap();
        let confirmed = Index {
            root: lib.index().root.clone(),
            removed: lib.removed().to_vec(),
            ..Index::default()
        };
        moved.file.path = "library/book.txt".into();
        assert_eq!(
            lib.apply_imported(&root, vec![moved], &confirmed),
            vec![id.clone()]
        );
        assert_eq!(lib.book(&id).unwrap().file.path, "library/book.txt");
        assert_eq!(
            lib.book(&id).unwrap().progress.as_ref().unwrap().fraction,
            0.4
        );
        assert!(!lib.removed().iter().any(|book| book.id == id));
    }
}
