// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moving selected books into the physical library folder.
//!
//! The flow has two halves, deliberately separated:
//!
//! - [`prepare`] copies the selected files into the destination and verifies
//!   every copy. It never removes anything and never touches the index: the
//!   caller decides what to do with the returned records and saves the index
//!   itself.
//! - [`finish`] removes the original file, and only once the destination
//!   still holds the same bytes. It must run *after* the index was saved
//!   durably, so a crash between the two steps leaves the original in place
//!   rather than a book the library no longer knows about.
//!
//! Every file is copied through a temporary file created with `create_new`,
//! synced, and compared against the source with bounded buffers, so a
//! truncated or corrupted copy never becomes the destination file. The
//! destination file is then written with `create_new` as well and synced, so
//! no name is ever taken away from somebody else and no store has to support
//! renaming over an existing file (the Kindle's FAT userstore does not).
//!
//! The source path is derived from [`BookRecord::file`] under the canonical
//! root and must stay inside it; the destination must stay under the root as
//! well and outside the hidden, private and application folders.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, Result};
use crate::index::{BookRecord, FileStamp};

/// Buffer size for copying and comparing files. Books are read in chunks, so
/// the peak memory does not depend on the file size.
const CHUNK: usize = 64 * 1024;

/// Length of a KOReader partial MD5, the base every book id is built from.
const BASE_ID_LEN: usize = 32;

/// One book that was copied into the destination, ready to be merged into the
/// index by the caller.
#[derive(Clone, Debug)]
pub struct PreparedBook {
    /// The canonical scan root used to recheck paths before source cleanup.
    pub root: PathBuf,
    /// The record with its path and stamp updated for the new location. The
    /// reading state the record already carried is untouched.
    pub record: BookRecord,
    /// The file the book was copied from. Still there after [`prepare`].
    pub source: PathBuf,
    /// The copy inside the destination folder.
    pub destination: PathBuf,
}

/// The result of a [`prepare`] run: what was copied, what failed and why, and
/// whether the run was cancelled.
#[derive(Clone, Debug, Default)]
pub struct PreparedBatch {
    /// The books that were copied successfully.
    pub books: Vec<PreparedBook>,
    /// Per-file failures, as `(path, reason)`.
    pub errors: Vec<(String, String)>,
    /// Whether the run stopped early because the cancel flag was set.
    pub cancelled: bool,
}

/// Copies the selected files into `destination`.
///
/// `root` is the canonical root the records' paths are relative to;
/// `destination` is the physical library folder, which must live under that
/// root and outside the hidden, private and application folders. The
/// destination folder is created only when a file is really copied into it.
///
/// A book that already sits in the destination is a no-op: its record comes
/// back with the canonical path and stamp of the file, and no copy is made. A
/// book whose source no longer matches the record it came from (different
/// size, different content) is an error for that file only; the other books
/// still migrate.
///
/// The source files are never removed here, and the index is never touched.
pub fn prepare(
    root: &Path,
    destination: &Path,
    records: &[BookRecord],
    cancel: &AtomicBool,
) -> Result<PreparedBatch> {
    let root = fs::canonicalize(root)?;
    let destination = resolve_destination(&root, destination)?;
    let mut batch = PreparedBatch::default();

    for record in records {
        if cancel.load(Ordering::Relaxed) {
            batch.cancelled = true;
            break;
        }
        match prepare_one(&root, &destination, record, cancel) {
            Ok(book) => batch.books.push(book),
            Err(e) => {
                if cancel.load(Ordering::Relaxed) {
                    batch.cancelled = true;
                }
                batch.errors.push((record.file.path.clone(), e.to_string()));
            }
        }
    }
    Ok(batch)
}

/// Removes the source of a prepared book, once the destination is known to
/// hold the same bytes.
///
/// Must only be called after the index was saved durably. When the source is
/// gone already the book is complete, so this returns `Ok`. When the source
/// still differs from the destination it is kept and an error is returned, so
/// a file edited during the migration is never thrown away.
///
/// The destination has to be a regular file of its own: a symlink, even one
/// pointing at the source, is refused, because removing the source would
/// leave the library pointing at a file that is no longer there.
pub fn finish(book: &PreparedBook) -> Result<()> {
    let source = &book.source;
    let destination = &book.destination;
    if source == destination {
        return Ok(());
    }
    let source_meta = match fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if source_meta.is_dir() {
        return Err(Error::scope(format!(
            "refusing to remove the directory {}",
            source.display()
        )));
    }
    let destination_link = fs::symlink_metadata(destination)
        .map_err(|e| Error::scope(format!("{}: {e}", destination.display())))?;
    if destination_link.file_type().is_symlink() {
        return Err(Error::scope(format!(
            "{} is a symlink",
            destination.display()
        )));
    }
    if !destination_link.is_file() {
        return Err(Error::scope(format!(
            "{} is not a regular file",
            destination.display()
        )));
    }
    // The two paths must be two different files: a destination that resolves
    // to the source is the same book twice.
    let source_real = fs::canonicalize(source)?;
    let destination_real = fs::canonicalize(destination)?;
    if fs::canonicalize(&book.root)? != book.root
        || !source.starts_with(&book.root)
        || !destination.starts_with(&book.root)
        || !source_real.starts_with(&book.root)
        || !destination_real.starts_with(&book.root)
    {
        return Err(Error::scope("a migration path left the scan root"));
    }
    if source_real == destination_real {
        return Err(Error::scope(format!(
            "{} and {} are the same file",
            source.display(),
            destination.display()
        )));
    }
    if !files_match(source, destination)? {
        return Err(Error::format(format!(
            "{} differs from {}",
            source.display(),
            destination.display()
        )));
    }
    fs::remove_file(source)?;
    crate::index::sync_parent_dir(source);
    Ok(())
}

/// Copies one record into the destination.
fn prepare_one(
    root: &Path,
    destination: &Path,
    record: &BookRecord,
    cancel: &AtomicBool,
) -> Result<PreparedBook> {
    let source = source_path(root, &record.file.path)?;
    let resolved_source = fs::canonicalize(&source)?;
    if !resolved_source.starts_with(root) {
        return Err(Error::scope(format!(
            "{} is outside the library root",
            source.display()
        )));
    }
    let meta = fs::metadata(&resolved_source)
        .map_err(|e| Error::scope(format!("{}: {e}", source.display())))?;
    if !meta.is_file() {
        return Err(Error::scope(format!("{} is not a file", source.display())));
    }
    check_unchanged(record, &resolved_source, meta.len())?;

    // The destination folder is only created when a file is really copied
    // into it.
    let destination = match fs::canonicalize(destination) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => destination.to_path_buf(),
        Err(e) => return Err(e.into()),
    };

    // Already in the destination: nothing to copy, nothing to remove. The
    // record gets the canonical path and stamp of the file, so a symlinked
    // source still yields the path the index should hold.
    if source == resolved_source && source.parent() == Some(destination.as_path()) {
        let meta = fs::metadata(&source)?;
        let mut new_record = record.clone();
        new_record.file = stamp_of(&meta, relative_to(root, &source));
        return Ok(PreparedBook {
            root: root.to_path_buf(),
            record: new_record,
            source: source.clone(),
            destination: source,
        });
    }

    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    fs::create_dir_all(&destination)?;
    let target = place_book(&resolved_source, &destination, &name, cancel)?;

    // The copy is checked once more before it counts as prepared: the file
    // has to hold the record's content and size, and it has to still match
    // the source it was made from.
    let target_meta = fs::metadata(&target)?;
    if target_meta.len() != record.file.size {
        let _ = fs::remove_file(&target);
        return Err(Error::format(format!(
            "{} copied with {} bytes, expected {}",
            target.display(),
            target_meta.len(),
            record.file.size
        )));
    }
    let target_id = crate::id::partial_md5(&target)?;
    if target_id != content_base(&record.id) {
        let _ = fs::remove_file(&target);
        return Err(Error::format(format!(
            "{} does not hold the book's content",
            target.display()
        )));
    }
    if !files_match(&resolved_source, &target)? {
        let _ = fs::remove_file(&target);
        return Err(Error::format(format!(
            "{} differs from {}",
            target.display(),
            source.display()
        )));
    }

    crate::index::sync_parent_dir(&target);
    let mut new_record = record.clone();
    new_record.file = stamp_of(&target_meta, relative_to(root, &target));
    Ok(PreparedBook {
        root: root.to_path_buf(),
        record: new_record,
        source,
        destination: target,
    })
}

/// Copies the source into the destination folder under a name that is free.
fn place_book(
    source: &Path,
    destination: &Path,
    name: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    for attempt in 0..usize::MAX {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::scope("cancelled"));
        }
        let target = destination.join(suffixed_name(name, attempt));
        if target.try_exists()? {
            continue;
        }
        let temp = temp_path(destination);
        copy_to_temp(source, &temp, cancel)?;
        match files_match(source, &temp) {
            Ok(true) => (),
            verification => {
                let _ = fs::remove_file(&temp);
                return match verification {
                    Err(error) => Err(error),
                    _ => Err(Error::format(format!(
                        "{} did not copy correctly",
                        source.display()
                    ))),
                };
            }
        }
        let installed = install(&temp, &target, cancel);
        let cleanup = fs::remove_file(&temp);
        match installed {
            Ok(()) => {
                cleanup?;
                crate::index::sync_parent_dir(&target);
                return Ok(target);
            }
            Err(Install::Exists) => {
                // Somebody took the name between the check and the write.
                cleanup?;
                continue;
            }
            Err(Install::Other(e)) => return Err(e),
        }
    }
    Err(Error::scope(format!(
        "no free name for {} in {}",
        name,
        destination.display()
    )))
}

/// What [`install`] can report.
enum Install {
    /// The target name was taken; the caller tries the next one.
    Exists,
    Other(Error),
}

/// Puts the temporary file in place without ever overwriting a file that
/// belongs to somebody else.
///
/// The name is taken with `create_new`, so it is ours alone and no store has
/// to support renaming over an existing file. The bytes are then written into
/// that file with bounded buffers, synced and compared with the source. A
/// failure removes the file we created; the source and the temporary file are
/// left for the caller to clean up.
fn install(temp: &Path, target: &Path, cancel: &AtomicBool) -> std::result::Result<(), Install> {
    let mut output = match OpenOptions::new().write(true).create_new(true).open(target) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(Install::Exists);
        }
        Err(e) => return Err(Install::Other(e.into())),
    };
    let mut input = match File::open(temp) {
        Ok(f) => f,
        Err(e) => {
            drop(output);
            let _ = fs::remove_file(target);
            return Err(Install::Other(e.into()));
        }
    };
    let written = copy_into(&mut input, &mut output, cancel).and_then(|()| {
        output.flush()?;
        output.sync_all()?;
        Ok(())
    });
    if let Err(e) = written {
        drop(output);
        let _ = fs::remove_file(target);
        return Err(Install::Other(e));
    }
    drop(output);
    match files_match(temp, target) {
        Ok(true) => Ok(()),
        Ok(false) => {
            let _ = fs::remove_file(target);
            Err(Install::Other(Error::format(format!(
                "{} did not copy correctly",
                target.display()
            ))))
        }
        Err(e) => {
            let _ = fs::remove_file(target);
            Err(Install::Other(e))
        }
    }
}

/// `name` for the first attempt, `name (2).ext` for the second, and so on.
fn suffixed_name(name: &str, attempt: usize) -> String {
    if attempt == 0 {
        return name.to_string();
    }
    let n = attempt + 1;
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// A temporary name next to the destination, unique per process and attempt.
fn temp_path(destination: &Path) -> PathBuf {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    destination.join(format!(".moyue-{pid}-{n}.tmp"))
}

/// Copies the source into the temporary file, syncing it before it is used.
fn copy_to_temp(source: &Path, temp: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(temp)?;
    let result = copy_into(&mut input, &mut output, cancel).and_then(|()| {
        output.flush()?;
        output.sync_all()?;
        Ok(())
    });
    drop(output);
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

/// Copies a file into an open file with bounded buffers.
fn copy_into(input: &mut File, output: &mut File, cancel: &AtomicBool) -> Result<()> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::scope("cancelled"));
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read])?;
    }
    Ok(())
}

/// Compares two files byte by byte with bounded buffers.
fn files_match(a: &Path, b: &Path) -> Result<bool> {
    let mut left = File::open(a)?;
    let mut right = File::open(b)?;
    let mut left_buf = vec![0u8; CHUNK];
    let mut right_buf = vec![0u8; CHUNK];
    loop {
        let n = left.read(&mut left_buf)?;
        let m = right.read(&mut right_buf)?;
        if n != m {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        if left_buf[..n] != right_buf[..m] {
            return Ok(false);
        }
    }
}

/// The absolute path of a record's file, under the canonical root.
fn source_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() {
        return Err(Error::scope("the record has no path"));
    }
    let mut path = root.to_path_buf();
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(Error::scope(format!("{relative} leaves the library root")));
        }
        path.push(part);
    }
    Ok(path)
}

/// The content hash a record's id is built from.
///
/// The scanner keeps ids unique when two files hold the same content, by
/// adding a numeric suffix (`<partial-md5>-2`, `-3`, …). The content itself
/// is still the base hash, so a duplicate id is accepted here.
fn content_base(id: &str) -> &str {
    if id.len() <= BASE_ID_LEN {
        return id;
    }
    let Some(base) = id.get(..BASE_ID_LEN) else {
        return id;
    };
    let suffix = &id[BASE_ID_LEN..];
    let Some(number) = suffix.strip_prefix('-') else {
        return id;
    };
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return id;
    }
    match number.parse::<u32>() {
        Ok(n) if n >= 2 => base,
        _ => id,
    }
}

/// Checks that the file on disk is still the file the record describes.
fn check_unchanged(record: &BookRecord, source: &Path, size: u64) -> Result<()> {
    if record.file.size != size {
        return Err(Error::format(format!(
            "{} changed since the scan ({} bytes, expected {})",
            source.display(),
            size,
            record.file.size
        )));
    }
    if !record.id.is_empty() {
        let id = crate::id::partial_md5(source)?;
        if id != content_base(&record.id) {
            return Err(Error::format(format!(
                "{} changed since the scan (different content)",
                source.display()
            )));
        }
    }
    Ok(())
}

/// The destination folder, checked against the root.
///
/// The path itself is resolved by the shared helper, so the destination
/// follows the same rules as the folder picker: it lives inside the root and
/// is not one of the store's top-level system or app folders. Hidden
/// components are also refused to keep private application data separate.
///
/// The folder is not created here: it is created when a file is really
/// copied into it.
fn resolve_destination(root: &Path, destination: &Path) -> Result<PathBuf> {
    let wanted = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        root.join(destination)
    };
    let resolved = crate::library::resolve_library_destination(root, &wanted)?;
    if let Ok(relative) = resolved.strip_prefix(root) {
        for part in relative.components() {
            if let Component::Normal(name) = part {
                check_folder_name(&name.to_string_lossy())?;
            }
        }
    }
    Ok(resolved)
}

/// Rejects hidden private folders.
fn check_folder_name(name: &str) -> Result<()> {
    if name.starts_with('.') {
        return Err(Error::scope(format!("{name} is a hidden folder")));
    }
    Ok(())
}

/// The path of a file relative to the root, in the `/`-separated form the
/// index keeps.
fn relative_to(root: &Path, path: &Path) -> String {
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

/// The stamp of a file, as the scanner keeps it.
fn stamp_of(meta: &Metadata, path: String) -> FileStamp {
    let (mtime, mtime_ns) = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
        .unwrap_or((0, 0));
    FileStamp {
        path,
        size: meta.len(),
        mtime,
        mtime_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format;
    use crate::index::Status;

    /// A synthetic plain-text book.
    fn txt_bytes() -> Vec<u8> {
        b"Chapter one\nThe quick brown fox jumps over the lazy dog.\n".to_vec()
    }

    /// A synthetic EPUB: a ZIP with the OCF mimetype first.
    fn epub_bytes() -> Vec<u8> {
        crate::zip::testutil::build(&[
            ("mimetype", b"application/epub+zip", false),
            ("META-INF/container.xml", b"<container/>", false),
            ("OEBPS/ch1.xhtml", b"<html><body>Hello</body></html>", true),
        ])
    }

    /// A record for a file that is already on disk, with the size, stamp and
    /// content id the scan would have produced.
    fn record_for(path: &Path, root: &Path, format: Format) -> BookRecord {
        let meta = fs::metadata(path).unwrap();
        BookRecord {
            id: crate::id::partial_md5(path).unwrap(),
            file: stamp_of(&meta, relative_to(root, path)),
            format,
            meta: Default::default(),
            cover: None,
            added: 1,
            last_opened: None,
            status: Status::Unread,
            progress: None,
            settings: Default::default(),
            annotations: Vec::new(),
            annotation_tombstones: Vec::new(),
        }
    }

    #[test]
    fn txt_book_moves_into_the_destination() {
        let root = fs::canonicalize(crate::testdir("transfer-txt")).unwrap();
        let source_dir = root.join("Author");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let destination = root.join("墨阅书库");

        let batch = prepare(
            &root,
            &destination,
            std::slice::from_ref(&record),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.books.len(), 1);
        let prepared = &batch.books[0];

        assert!(source.exists(), "prepare must not remove the source");
        let target = destination.join("book.txt");
        assert_eq!(prepared.destination, target);
        assert_eq!(fs::read(&target).unwrap(), txt_bytes());
        assert_eq!(prepared.record.file.path, "墨阅书库/book.txt");
        assert_eq!(prepared.record.file.size, txt_bytes().len() as u64);
        assert_eq!(prepared.record.id, record.id);
        assert_eq!(
            fs::read_dir(&destination).unwrap().count(),
            1,
            "successful preparation must remove its temporary copy"
        );

        // The index is the caller's business; the record is ready for it.
        finish(prepared).unwrap();
        assert!(!source.exists());
        assert!(target.exists());
    }

    #[test]
    fn epub_book_moves_into_the_destination() {
        let root = fs::canonicalize(crate::testdir("transfer-epub")).unwrap();
        let source = root.join("books").join("novel.epub");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, epub_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Epub);
        let destination = root.join("墨阅书库");

        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let prepared = &batch.books[0];
        assert_eq!(prepared.destination, destination.join("novel.epub"));
        assert_eq!(fs::read(&prepared.destination).unwrap(), epub_bytes());
        assert!(source.exists());
    }

    #[test]
    fn every_format_is_allowed() {
        let root = fs::canonicalize(crate::testdir("transfer-formats")).unwrap();
        let source = root.join("notes.txt");
        fs::write(&source, txt_bytes()).unwrap();
        // The format in the record does not gate the migration.
        let record = record_for(&source, &root, Format::Pdf);
        let destination = root.join("墨阅书库");
        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].destination, destination.join("notes.txt"));
    }

    #[test]
    fn a_name_collision_does_not_overwrite() {
        let root = fs::canonicalize(crate::testdir("transfer-collision")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let destination = root.join("墨阅书库");
        fs::create_dir_all(&destination).unwrap();
        let taken = destination.join("book.txt");
        fs::write(&taken, b"an older book").unwrap();

        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let prepared = &batch.books[0];
        assert_eq!(prepared.destination, destination.join("book (2).txt"));
        assert_eq!(fs::read(&taken).unwrap(), b"an older book");
        assert_eq!(fs::read(&prepared.destination).unwrap(), txt_bytes());
        assert!(source.exists());
    }

    #[test]
    fn a_cancelled_run_keeps_every_source() {
        let root = fs::canonicalize(crate::testdir("transfer-cancel")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let cancel = AtomicBool::new(true);

        let batch = prepare(&root, &root.join("墨阅书库"), &[record], &cancel).unwrap();
        assert!(batch.cancelled);
        assert!(batch.books.is_empty());
        assert!(source.exists());
        // A cancelled run creates nothing.
        assert!(!root.join("墨阅书库").exists());
    }

    #[test]
    fn a_changed_source_is_refused() {
        let root = fs::canonicalize(crate::testdir("transfer-changed")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        fs::write(&source, b"something else entirely").unwrap();

        let batch = prepare(
            &root,
            &root.join("墨阅书库"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.books.is_empty());
        assert_eq!(batch.errors.len(), 1);
        assert!(source.exists(), "the changed source must be kept");
        assert!(!root.join("墨阅书库").join("book.txt").exists());
    }

    #[test]
    fn a_missing_source_is_reported_and_the_others_still_move() {
        let root = fs::canonicalize(crate::testdir("transfer-missing-source")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let mut missing = record.clone();
        fs::remove_file(&source).unwrap();
        missing.file.path = "gone.txt".to_string();

        // A second book, still there, has to move.
        let other = root.join("other.txt");
        fs::write(&other, txt_bytes()).unwrap();
        let other_record = record_for(&other, &root, Format::Txt);
        let destination = root.join("墨阅书库");

        let batch = prepare(
            &root,
            &destination,
            &[missing.clone(), other_record.clone()],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].destination, destination.join("other.txt"));
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.errors[0].0, missing.file.path);
        assert!(!destination.join("gone.txt").exists());
    }

    #[test]
    fn a_duplicate_id_is_still_a_valid_book() {
        let root = fs::canonicalize(crate::testdir("transfer-duplicate-id")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let mut record = record_for(&source, &root, Format::Txt);
        // The scanner keeps ids unique by adding a numeric suffix.
        let base = record.id.clone();
        record.id = format!("{base}-2");
        let destination = root.join("墨阅书库");

        let batch = prepare(
            &root,
            &destination,
            &[record.clone()],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.books.len(), 1);
        let prepared = &batch.books[0];
        assert_eq!(prepared.record.id, format!("{base}-2"));
        assert_eq!(fs::read(&prepared.destination).unwrap(), txt_bytes());
        finish(prepared).unwrap();
        assert!(!source.exists());
    }

    #[test]
    fn a_book_already_in_the_destination_is_a_no_op() {
        let root = fs::canonicalize(crate::testdir("transfer-noop")).unwrap();
        let destination = root.join("墨阅书库");
        fs::create_dir_all(&destination).unwrap();
        let source = destination.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);

        let batch = prepare(
            &root,
            &destination,
            std::slice::from_ref(&record),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let prepared = &batch.books[0];
        assert_eq!(prepared.source, prepared.destination);
        assert_eq!(prepared.record.file.path, record.file.path);

        // Finishing such a book must not remove it.
        finish(prepared).unwrap();
        assert!(source.exists());
    }

    #[test]
    fn migrating_a_symlink_keeps_its_unselected_target() {
        let root = fs::canonicalize(crate::testdir("transfer-symlink-noop")).unwrap();
        let destination = root.join("墨阅书库");
        fs::create_dir_all(&destination).unwrap();
        let real = destination.join("book.txt");
        fs::write(&real, txt_bytes()).unwrap();
        let link = root.join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut record = record_for(&link, &root, Format::Txt);
        // The scanner saw the link, not the file it points at.
        record.file.path = relative_to(&root, &link);
        let destination = destination.clone();

        let batch = prepare(
            &root,
            &destination,
            std::slice::from_ref(&record),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        let prepared = &batch.books[0];
        assert_eq!(prepared.record.file.path, "墨阅书库/link.txt");
        assert_eq!(prepared.record.file.size, txt_bytes().len() as u64);
        assert_eq!(prepared.source, link);
        finish(prepared).unwrap();
        assert!(!link.exists());
        assert!(real.exists(), "the unselected target stays in place");
        assert_eq!(fs::read(&prepared.destination).unwrap(), txt_bytes());
    }

    #[test]
    fn finish_only_removes_a_matching_source() {
        let root = fs::canonicalize(crate::testdir("transfer-finish")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let destination = root.join("墨阅书库");

        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        let prepared = &batch.books[0];

        // The source was edited after the copy: it must stay.
        fs::write(&source, b"edited after the migration").unwrap();
        assert!(finish(prepared).is_err());
        assert!(source.exists());
        assert!(prepared.destination.exists());
    }

    #[test]
    fn a_missing_source_is_already_complete() {
        let root = fs::canonicalize(crate::testdir("transfer-missing")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let destination = root.join("墨阅书库");

        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        let prepared = &batch.books[0];
        fs::remove_file(&source).unwrap();
        finish(prepared).unwrap();
        assert!(prepared.destination.exists());
    }

    #[cfg(unix)]
    #[test]
    fn finish_refuses_a_destination_that_is_a_symlink_to_the_source() {
        let root = fs::canonicalize(crate::testdir("transfer-symlink-dest")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let destination = root.join("墨阅书库");

        let batch = prepare(&root, &destination, &[record], &AtomicBool::new(false)).unwrap();
        let prepared = &batch.books[0];

        // Somebody replaced the copy with a link to the original.
        fs::remove_file(&prepared.destination).unwrap();
        std::os::unix::fs::symlink(&source, &prepared.destination).unwrap();

        assert!(finish(prepared).is_err());
        assert!(source.exists(), "the source must be kept");
        assert!(prepared.destination.exists());
    }

    #[test]
    fn the_destination_must_stay_under_the_root() {
        let root = fs::canonicalize(crate::testdir("transfer-outside")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let outside = root.parent().unwrap().join("墨阅书库-outside");

        let result = prepare(&root, &outside, &[record], &AtomicBool::new(false));
        assert!(result.is_err());
        assert!(source.exists());
        assert!(!outside.exists());
    }

    #[test]
    fn private_folders_are_refused_as_destination() {
        let root = fs::canonicalize(crate::testdir("transfer-appfolder")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);

        for folder in [".hidden", "documents/.private"] {
            let result = prepare(
                &root,
                &root.join(folder),
                std::slice::from_ref(&record),
                &AtomicBool::new(false),
            );
            assert!(result.is_err(), "{folder} should be refused");
        }
        assert!(source.exists());
    }

    #[test]
    fn malformed_unicode_ids_are_reported_without_panicking() {
        let root = fs::canonicalize(crate::testdir("transfer-unicode-id")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let mut record = record_for(&source, &root, Format::Txt);
        record.id = "书".repeat(20);
        let batch = prepare(
            &root,
            &root.join("library"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.books.is_empty());
        assert_eq!(batch.errors.len(), 1);
        assert!(source.exists());
    }

    #[test]
    fn a_long_source_name_uses_a_short_temporary_name() {
        let root = fs::canonicalize(crate::testdir("transfer-long-name")).unwrap();
        let source = root.join(format!("{}.txt", "b".repeat(240)));
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let batch = prepare(
            &root,
            &root.join("library"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty(), "{:?}", batch.errors);
        assert_eq!(batch.books.len(), 1);
        assert_eq!(fs::read(&batch.books[0].destination).unwrap(), txt_bytes());
        assert!(source.exists());
    }

    #[test]
    fn an_existing_temporary_file_is_preserved() {
        let root = fs::canonicalize(crate::testdir("transfer-temp-collision")).unwrap();
        let source = root.join("book.txt");
        let temp = root.join(".moyue-existing.tmp");
        fs::write(&source, txt_bytes()).unwrap();
        fs::write(&temp, b"another operation's file").unwrap();
        assert!(copy_to_temp(&source, &temp, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(&temp).unwrap(), b"another operation's file");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_into_another_source_folder_moves_only_the_selected_link() {
        let root = fs::canonicalize(crate::testdir("transfer-link-selection")).unwrap();
        fs::create_dir_all(root.join("unselected")).unwrap();
        let real = root.join("unselected/book.txt");
        fs::write(&real, txt_bytes()).unwrap();
        let link = root.join("selected.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut record = record_for(&link, &root, Format::Txt);
        record.file.path = "selected.txt".into();
        let batch = prepare(
            &root,
            &root.join("library"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty());
        finish(&batch.books[0]).unwrap();
        assert!(!link.exists());
        assert_eq!(fs::read(&real).unwrap(), txt_bytes());
        assert_eq!(batch.books[0].record.file.path, "library/selected.txt");
    }

    #[cfg(unix)]
    #[test]
    fn finish_keeps_files_outside_the_root_after_a_parent_link_changes() {
        let root = fs::canonicalize(crate::testdir("transfer-source-parent")).unwrap();
        let outside = fs::canonicalize(crate::testdir("transfer-source-outside")).unwrap();
        fs::create_dir_all(root.join("sources")).unwrap();
        let source = root.join("sources/book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let batch = prepare(
            &root,
            &root.join("library"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty());
        fs::rename(root.join("sources"), root.join("originals")).unwrap();
        fs::write(outside.join("book.txt"), txt_bytes()).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sources")).unwrap();
        assert!(finish(&batch.books[0]).is_err());
        assert_eq!(fs::read(outside.join("book.txt")).unwrap(), txt_bytes());
        assert!(root.join("originals/book.txt").exists());
    }

    #[test]
    fn an_ordinary_nested_system_folder_is_a_valid_destination() {
        let root = fs::canonicalize(crate::testdir("transfer-nested-system")).unwrap();
        let source = root.join("book.txt");
        fs::write(&source, txt_bytes()).unwrap();
        let record = record_for(&source, &root, Format::Txt);
        let batch = prepare(
            &root,
            &root.join("documents/system"),
            &[record],
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(batch.errors.is_empty());
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].record.file.path, "documents/system/book.txt");
        finish(&batch.books[0]).unwrap();
        assert!(!source.exists());
    }
}
