// SPDX-License-Identifier: AGPL-3.0-or-later
//! Choosing the folder a local scan looks at.
//!
//! A scan can look at the whole user store or at one folder in it. The
//! picker lists the folders under the scan root, lets the reader step into
//! one or back out, and keeps the choice for the next scan.
//!
//! The folder is checked against the root through the file system, so a
//! symlink pointing out of the store cannot be scanned, and the paths the
//! picker shows are the ones the index keeps: relative to the scan root.

use std::fs;
use std::path::{Path, PathBuf};

use aristotle_library::library::{excluded_store_folder, KINDLE_STORE};

/// One folder of the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeFolder {
    /// The name shown in the list.
    pub name: String,
    /// The path relative to the scan root, the form the index keeps.
    pub relative: String,
    /// The books the index holds below this folder.
    pub books: usize,
}

/// The folder a scoped scan looks at, and the root it must stay under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The scan root, already resolved.
    pub root: PathBuf,
    /// The folder the reader picked, already resolved.
    pub path: PathBuf,
}

impl Scope {
    /// The scope for `path` under `root`.
    ///
    /// Both go through the file system: the folder has to be there and has
    /// to be inside the root. A symlink that points out of the root resolves
    /// to its target and is refused.
    pub fn new(root: &Path, path: &Path) -> Option<Scope> {
        let root = fs::canonicalize(root).ok()?;
        let path = fs::canonicalize(path).ok()?;
        if !path.starts_with(&root) || !path.is_dir() {
            return None;
        }
        if excluded_relative(&root, &path) {
            return None;
        }
        Some(Scope { root, path })
    }

    /// The scope of the scan root itself: the whole store.
    pub fn whole(root: &Path) -> Option<Scope> {
        Scope::new(root, root)
    }

    /// The folder one level up, or `None` at the root.
    pub fn parent(&self) -> Option<PathBuf> {
        let parent = self.path.parent()?;
        if !parent.starts_with(&self.root) {
            return None;
        }
        Some(parent.to_path_buf())
    }

    /// Whether this scope is the whole root.
    pub fn is_whole(&self) -> bool {
        self.path == self.root
    }

    /// The path of the scope relative to the root, the form the index
    /// keeps. The root itself is the empty string.
    pub fn relative(&self) -> String {
        relative_of(&self.root, &self.path)
    }

    /// The folders below this scope, with the number of books the index
    /// holds in each of them.
    ///
    /// `books` is a list of the indexed paths, relative to the root. The
    /// app's own folders and the hidden ones are left out.
    pub fn folders(&self, books: &[String]) -> Vec<ScopeFolder> {
        let entries = match fs::read_dir(&self.path) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if !entry.file_type().is_ok_and(|kind| kind.is_dir())
                || name.to_lowercase().ends_with(".sdr")
            {
                continue;
            }
            // The app's own folders sit at the root of the store; deeper
            // ones with the same name are the reader's own.
            if self.is_whole() && excluded_relative(&self.root, &path) {
                continue;
            }
            let relative = relative_of(&self.root, &path);
            if relative.is_empty() {
                continue;
            }
            let count = books.iter().filter(|p| under(p, &relative)).count();
            out.push(ScopeFolder {
                name: name.to_string(),
                relative,
                books: count,
            });
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }
}

/// The path of `path` relative to `root`, with `/` separators.
fn relative_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The path of `path` relative to `root`, the form the index keeps.
pub fn relative_path(root: &Path, path: &Path) -> String {
    relative_of(root, path)
}

/// Whether the indexed path lies below the relative folder path.
fn under(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a folder belongs to the store's system or app folders, and so is
/// hidden from both the source and the destination picker.
///
/// Only the first folder below the root counts: a user folder called
/// `documents/system` is the reader's own. A custom root has no such
/// blacklist.
pub fn excluded_relative(root: &Path, path: &Path) -> bool {
    if root != Path::new(KINDLE_STORE) {
        return false;
    }
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|part| excluded_store_folder(&part.as_os_str().to_string_lossy()))
}

/// The canonical folders to scan for the reader's `selected` sources, given
/// as paths relative to the root.
///
/// A source that vanished or escapes the root is dropped; duplicates and
/// folders already inside another source collapse into the parent, so each
/// file is walked and reviewed once.
pub fn scan_sources(root: &Path, selected: &[String]) -> Vec<PathBuf> {
    let Ok(root) = fs::canonicalize(root) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for relative in selected {
        let Some(scope) = Scope::new(&root, &root.join(relative)) else {
            continue;
        };
        if !paths.contains(&scope.path) {
            paths.push(scope.path);
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if out.iter().any(|kept| path.starts_with(kept)) {
            continue;
        }
        out.retain(|kept| !kept.starts_with(&path));
        out.push(path);
    }
    out.sort();
    out
}

/// The reader's sources as `/`-separated paths relative to the root, for
/// prefs and the picker's checkmarks.
#[cfg(test)]
pub fn sources_relative(root: &Path, sources: &[PathBuf]) -> Vec<String> {
    sources
        .iter()
        .map(|path| relative_of(root, path))
        .filter(|path| !path.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("aristotle-scope-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn the_picker_lists_the_folders_below_the_scope() {
        let root = dir("folders");
        fs::create_dir_all(root.join("documents/novels")).unwrap();
        fs::create_dir_all(root.join("documents/poems")).unwrap();
        fs::write(root.join("documents/loose.txt"), b"x").unwrap();
        fs::create_dir_all(root.join("documents/book.sdr")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("documents/loop")).unwrap();
        let scope = Scope::new(&root, &root.join("documents")).unwrap();
        let books = vec!["documents/novels/a.epub".to_string()];
        let folders = scope.folders(&books);
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["novels", "poems"]);
        let novels = &folders[0];
        assert_eq!(novels.relative, "documents/novels");
        assert_eq!(novels.books, 1);
    }

    #[test]
    fn the_scope_walks_up_to_the_root_and_no_further() {
        let root = dir("parent");
        fs::create_dir_all(root.join("documents/novels")).unwrap();
        let scope = Scope::new(&root, &root.join("documents/novels")).unwrap();
        assert_eq!(scope.parent(), Some(root.join("documents")));
        assert_eq!(scope.relative(), "documents/novels");
        let docs = Scope::new(&root, &root.join("documents")).unwrap();
        assert_eq!(docs.parent(), Some(root.clone()));
        let whole = Scope::whole(&root).unwrap();
        assert!(whole.is_whole());
        assert_eq!(whole.parent(), None);
        assert_eq!(whole.relative(), "");
    }

    #[test]
    fn a_folder_outside_the_root_is_refused() {
        let root = dir("outside");
        let other = dir("outside-other");
        fs::create_dir_all(&other).unwrap();
        assert!(Scope::new(&root, &other).is_none());
        assert!(Scope::new(&root, &root.join("missing")).is_none());
    }

    #[test]
    fn the_store_blacklist_hides_system_and_app_folders() {
        let store = Path::new(KINDLE_STORE);
        for name in [
            "system",
            "koreader",
            "extensions",
            "usbnet",
            "linkss",
            "mkk",
            "mrpackages",
            "rp",
            "libkh",
            "inkread-lab",
            "aristotle-probe",
            "aristotle-pre-app007",
            "aristotle-v0.1.0",
        ] {
            assert!(
                excluded_relative(store, &store.join(name)),
                "{name} must be excluded"
            );
        }
        assert!(!excluded_relative(store, &store.join("documents")));
        assert!(!excluded_relative(store, &store.join("documents/system")));
        assert!(!excluded_relative(store, &store.join("books")));
        // A custom root keeps every folder.
        let root = Path::new("/home/reader/books");
        assert!(!excluded_relative(root, &root.join("system")));
        assert!(!excluded_relative(root, &root.join("aristotle-v1")));
    }

    #[test]
    fn overlapping_sources_collapse_to_the_parent_once() {
        let root = dir("sources");
        fs::create_dir_all(root.join("documents/novels")).unwrap();
        fs::create_dir_all(root.join("documents/novels/old")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        let selected = vec![
            "documents".to_string(),
            "documents/novels".to_string(),
            "documents/novels".to_string(),
            "docs".to_string(),
            "gone".to_string(),
        ];
        let sources = scan_sources(&root, &selected);
        let relative = sources_relative(&root, &sources);
        assert_eq!(relative, ["docs", "documents"]);
        assert_eq!(sources.len(), 2, "no file is walked twice");
    }
}
