//! Local reading progress keyed by book hash, persisted as a single JSON file.
//! The on-disk shape is `{ "<book_hash>": Progress }`.
//!
//! # Crash safety
//!
//! Kindle stores books on FAT32 (`/mnt/us`), where `rename` is not an atomic
//! metadata transaction and page-cache writeback can lag behind the syscall by
//! seconds. A hard power-off in that window can leave the file zero-length or
//! truncated, so the store keeps one previous generation around:
//!
//! - `save` writes `progress.json.tmp`, `fsync`s it, rotates the current file
//!   to `progress.json.bak`, renames the tmp file into place and finally
//!   `fsync`s the parent directory (best effort — vfat may not support it).
//! - `load` falls back to `progress.json.bak` when the main file is missing or
//!   unparsable, so a torn write costs at most the newest position.
//!
//! The `.bak` fallback is a mitigation, not a guarantee: FAT32 can still lose
//! both generations if the card is yanked mid-write.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Reading position of one book.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// crengine XPointer, e.g. `/body/DocFragment[11]/body/div/p[3]/text().0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xpointer: Option<String>,
    /// Current page, 1-based.
    #[serde(default)]
    pub page: u32,
    /// Total pages known at the time of writing; `0` when unknown.
    #[serde(default)]
    pub total: u32,
    /// Unix timestamp in seconds, used for `SortKey::RecentlyRead`.
    #[serde(default)]
    pub updated_at: i64,
}

/// In-memory progress table backed by a JSON file.
#[derive(Debug, Clone)]
pub struct ProgressStore {
    path: PathBuf,
    entries: BTreeMap<String, Progress>,
}

impl ProgressStore {
    /// Loads the store at `path`.
    ///
    /// A missing main file yields an empty store. When the main file exists but
    /// is unparsable, or is missing while a sibling `*.bak` exists, the backup
    /// is loaded instead so a torn write does not wipe every reading position.
    /// The returned store still writes back to `path`.
    ///
    /// # Errors
    /// Returns [`io::ErrorKind::InvalidData`] when the main file is not valid
    /// JSON and no readable backup is available; other I/O errors are returned
    /// as-is.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let primary = read_entries(&path);
        let entries = match primary {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let backup = read_entries(&bak_path(&path));
                match backup {
                    Ok(entries) => entries,
                    Err(_) => return Err(e),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                read_entries(&bak_path(&path)).unwrap_or_default()
            }
            Err(e) => return Err(e),
        };
        Ok(Self { path, entries })
    }

    /// Returns the stored progress for `hash`, if any.
    pub fn get(&self, hash: &str) -> Option<&Progress> {
        self.entries.get(hash)
    }

    /// Inserts or replaces the progress for `hash`.
    pub fn set(&mut self, hash: impl Into<String>, progress: Progress) {
        self.entries.insert(hash.into(), progress);
    }

    /// Persists the store, keeping the previous generation as `*.bak`.
    ///
    /// The data is written to `*.tmp` and `fsync`ed before it replaces the main
    /// file, so a crash leaves either the new file or the previous one plus its
    /// backup — never a half-written main file. See the module docs for the
    /// FAT32 caveats.
    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_vec_pretty(&self.entries)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = tmp_path(&self.path);
        if let Err(e) = self.replace_with(&tmp, &json) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }

    /// Writes `json` to `tmp`, rotates the current file to `*.bak` and renames
    /// `tmp` into place.
    fn replace_with(&self, tmp: &Path, json: &[u8]) -> io::Result<()> {
        let mut file = File::create(tmp)?;
        file.write_all(json)?;
        file.sync_all()?;
        drop(file);
        if self.path.exists() {
            let bak = bak_path(&self.path);
            match fs::remove_file(&bak) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            fs::rename(&self.path, &bak)?;
        }
        fs::rename(tmp, &self.path)?;
        sync_parent_dir(&self.path);
        Ok(())
    }
}

/// Reads and parses one progress file; a missing file is `NotFound`.
fn read_entries(path: &Path) -> io::Result<BTreeMap<String, Progress>> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// `progress.json` -> `progress.json.tmp`, in the same directory.
fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// `progress.json` -> `progress.json.bak`, in the same directory.
fn bak_path(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    PathBuf::from(bak)
}

/// Best-effort `fsync` of the directory holding `path`, so the rename itself is
/// durable. vfat may reject opening or syncing a directory; that is ignored.
#[cfg(unix)]
fn sync_parent_dir(path: &Path) {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
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
                "inkread-core-progress-{tag}-{}-{nanos}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn file(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn sample(xpointer: &str, page: u32, updated: i64) -> Progress {
        Progress {
            xpointer: Some(xpointer.to_string()),
            page,
            total: 100,
            updated_at: updated,
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = TempDir::new("missing");
        let store = ProgressStore::load(dir.file("progress.json")).unwrap();
        assert!(store.get("anything").is_none());
    }

    #[test]
    fn roundtrip_preserves_entries() {
        let dir = TempDir::new("roundtrip");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash-a", sample("/body/p[1]", 3, 100));
        store.set("hash-b", sample("/body/p[2]", 7, 200));
        store.save().unwrap();

        let reloaded = ProgressStore::load(&path).unwrap();
        assert_eq!(reloaded.get("hash-a"), Some(&sample("/body/p[1]", 3, 100)));
        assert_eq!(reloaded.get("hash-b"), Some(&sample("/body/p[2]", 7, 200)));
        assert!(reloaded.get("hash-c").is_none());
    }

    #[test]
    fn json_uses_camel_case() {
        let dir = TempDir::new("camel");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash", sample("/body/p[1]", 1, 42));
        store.save().unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"xpointer\""));
        assert!(text.contains("\"updatedAt\""));
        assert!(!text.contains("updated_at"));
    }

    #[test]
    fn save_is_atomic_and_leaves_no_tmp() {
        let dir = TempDir::new("atomic");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash", sample("/body/p[1]", 1, 42));
        store.save().unwrap();

        let leftovers: Vec<_> = fs::read_dir(&dir.path)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");
        assert_eq!(fs::read_dir(&dir.path).unwrap().count(), 1);
    }

    #[test]
    fn corrupt_json_is_invalid_data() {
        let dir = TempDir::new("corrupt");
        let path = dir.file("progress.json");
        fs::write(&path, b"{ this is not json").unwrap();
        let err = ProgressStore::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn save_creates_missing_parent_directory() {
        let dir = TempDir::new("nested");
        let path = dir.file("sub").join("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash", sample("/body/p[1]", 1, 42));
        store.save().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn save_rotates_previous_generation_to_bak() {
        let dir = TempDir::new("rotate");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash", sample("/body/p[1]", 1, 42));
        store.save().unwrap();
        store.set("hash", sample("/body/p[9]", 9, 99));
        store.save().unwrap();

        let main = ProgressStore::load(&path).unwrap();
        assert_eq!(main.get("hash"), Some(&sample("/body/p[9]", 9, 99)));
        let backup = ProgressStore::load(dir.file("progress.json.bak")).unwrap();
        assert_eq!(backup.get("hash"), Some(&sample("/body/p[1]", 1, 42)));
    }

    #[test]
    fn load_falls_back_to_bak_when_main_is_corrupt() {
        let dir = TempDir::new("fallback");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash-a", sample("/body/p[1]", 3, 100));
        store.save().unwrap();
        store.set("hash-b", sample("/body/p[2]", 7, 200));
        store.save().unwrap();

        fs::write(&path, b"").unwrap();
        let recovered = ProgressStore::load(&path).unwrap();
        assert_eq!(
            recovered.get("hash-a"),
            Some(&sample("/body/p[1]", 3, 100)),
            "torn main file must fall back to the previous generation"
        );
    }

    #[test]
    fn load_uses_bak_when_main_is_missing() {
        let dir = TempDir::new("bak-only");
        let mut store = ProgressStore::load(dir.file("progress.json")).unwrap();
        store.set("hash", sample("/body/p[1]", 4, 400));
        store.save().unwrap();
        let path = dir.file("progress.json");
        fs::rename(&path, dir.file("progress.json.bak")).unwrap();

        let recovered = ProgressStore::load(&path).unwrap();
        assert_eq!(recovered.get("hash"), Some(&sample("/body/p[1]", 4, 400)));
        assert_eq!(fs::read_dir(&dir.path).unwrap().count(), 1);
    }

    #[test]
    fn load_is_invalid_data_when_main_and_bak_are_corrupt() {
        let dir = TempDir::new("both-corrupt");
        let path = dir.file("progress.json");
        fs::write(&path, b"{ half").unwrap();
        fs::write(dir.file("progress.json.bak"), b"").unwrap();

        let err = ProgressStore::load(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn save_failure_leaves_no_tmp_behind() {
        let dir = TempDir::new("tmp-cleanup");
        let path = dir.file("progress.json");
        let mut store = ProgressStore::load(&path).unwrap();
        store.set("hash", sample("/body/p[1]", 1, 42));
        store.save().unwrap();

        // A directory where the rotated backup must go makes the rotation fail.
        fs::create_dir(dir.file("progress.json.bak")).unwrap();
        store.set("hash", sample("/body/p[2]", 2, 43));
        store.save().unwrap_err();

        assert!(!dir.file("progress.json.tmp").exists());
    }
}
