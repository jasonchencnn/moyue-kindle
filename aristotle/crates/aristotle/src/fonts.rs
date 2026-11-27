// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reader's own fonts: TTF and OTF files in `fonts/` at the root of the
//! USB drive (`/mnt/us/fonts`, the folder the stock reader takes its custom
//! fonts from).
//!
//! - **Families.** Files are grouped by the family and style names FreeType
//!   reads from their name tables, the same names Plato matches when it
//!   loads a family. A style name the table gives but Plato's list does not
//!   know (as "Normal" or "Oblique") is read from the file name instead
//!   (`Family-Regular.ttf`, `Family-BoldItalic.otf`). Files FreeType cannot
//!   open, files over [`MAX_BYTES`] and families without a regular face
//!   are left out.
//! - **Lazily, cached.** The folder is read when the font list opens, not at
//!   start. The list is kept in `fonts.json` in the library data directory
//!   with the folder's mtime and file names, and read again only when they
//!   change.
//! - **A folder per family.** Plato loads a family by opening every font
//!   file in a directory and keeping the ones whose family name matches.
//!   Pointed at the user's folder it would open all of them for every book,
//!   on every thread that lays out. So the chosen family's faces (four at
//!   most) are copied to `reading-fonts/<name>-<stamp>/` in the library data
//!   directory and Plato is pointed there. FAT has no links, so a copy it
//!   is. The stamp hashes the sources' paths, sizes and mtimes: a changed
//!   font gets a new folder, and the folder's path is part of the style, so
//!   saved page numbers and snapshots of the old one are not used.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use aristotle_i18n::{tr, Msg};
use aristotle_render::plato_core::anyhow::Error;
use aristotle_render::plato_core::font::FontOpener;
use aristotle_ui::status::Loading;
use aristotle_ui::text::FamilyId;
use aristotle_ui::tree::Refresh;
use serde::{Deserialize, Serialize};

use crate::reading::Overlay;
use crate::{perf, App, Screen};

/// A font load slower than this (in milliseconds) is worth a word in the
/// status bar: the reader's own fonts are registered before every layout.
pub const FONT_LOAD_SLOW_MS: u64 = 300;

/// The built-in reading font.
pub const DEFAULT: &str = "Literata";
/// The built-in reading font for Latin text.
pub const DEFAULT_LATIN: &str = "Literata";
/// The built-in reading font for CJK text.
pub const DEFAULT_CJK: &str = "Noto Sans SC";
/// Where the stock reader and Aristotle look for the user's fonts.
pub const DEFAULT_DIR: &str = "/mnt/us/fonts";
/// Larger files are skipped: most CJK reading fonts (including Source Han,
/// Founder, LXGW WenKai and TTC collections) fit well under 64 MiB.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The family list, in the library data directory.
const LIST_FILE: &str = "fonts.json";
/// Copies of chosen families, in the library data directory.
pub const COPIES_DIR: &str = "reading-fonts";

/// Face slots, in the order Plato and the UI use: regular, italic, bold,
/// bold italic.
pub const SLOTS: [&str; 4] = ["Regular", "Italic", "Bold", "BoldItalic"];

/// Style names Plato's `FontFamily::from_name` takes for each slot, in its
/// order of preference.
const PLATO_STYLES: [&[&str]; 4] = [
    &["Regular", "Roman", "Book"],
    &["Italic", "Book Italic", "Regular Italic"],
    &["Bold", "Semibold", "SemiBold", "Medium"],
    &["Bold Italic", "SemiBold Italic", "Medium Italic"],
];

/// One font file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Face {
    pub path: PathBuf,
    /// The style name from the name table, as FreeType reports it.
    pub style: String,
    pub size: u64,
    pub mtime: i64,
}

/// A family with its faces by slot ([`SLOTS`]); the regular one is always
/// there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Family {
    pub name: String,
    pub faces: [Option<Face>; 4],
}

impl Family {
    pub fn regular(&self) -> &Face {
        self.faces[0].as_ref().expect("a family has a regular face")
    }

    /// The faces copied for Plato. Plato needs a face named as in its list
    /// for each style it should use, and a regular one among them, unless
    /// the folder holds one file, which then serves every style. So a
    /// regular face with another name goes alone.
    fn for_plato(&self) -> Vec<(usize, &Face)> {
        let named = |slot: usize, f: &Face| PLATO_STYLES[slot].contains(&f.style.as_str());
        if !named(0, self.regular()) {
            return vec![(0, self.regular())];
        }
        self.faces
            .iter()
            .enumerate()
            .filter_map(|(slot, f)| f.as_ref().map(|f| (slot, f)))
            .filter(|(slot, f)| named(*slot, f))
            .collect()
    }

    /// A hash of the sources, for the copy's folder name.
    fn stamp(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.name.hash(&mut h);
        for (slot, f) in self.for_plato() {
            slot.hash(&mut h);
            f.path.hash(&mut h);
            f.size.hash(&mut h);
            f.mtime.hash(&mut h);
        }
        h.finish()
    }
}

/// What a scan learnt about one file.
#[derive(Clone, Debug)]
pub struct Scanned {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: i64,
    pub family: String,
    pub style: String,
}

/// The slot a style name stands for: Plato's names first, then common
/// variants.
pub fn slot_of_style(style: &str) -> Option<usize> {
    if let Some(slot) = PLATO_STYLES.iter().position(|names| names.contains(&style)) {
        return Some(slot);
    }
    let s: String = style
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    match s.as_str() {
        "regular" | "roman" | "book" | "normal" | "plain" | "text" | "" => Some(0),
        "italic" | "oblique" | "bookitalic" | "regularitalic" | "textitalic" => Some(1),
        "bold" | "semibold" | "demibold" | "medium" => Some(2),
        "bolditalic" | "boldoblique" | "semibolditalic" | "demibolditalic" | "mediumitalic" => {
            Some(3)
        }
        _ => None,
    }
}

/// The slot a file name gives, as in `Family-BoldItalic.ttf`.
pub fn slot_of_file_name(path: &Path) -> Option<usize> {
    let stem = path.file_stem()?.to_str()?;
    let (_, style) = stem.rsplit_once(['-', '_'])?;
    slot_of_style(style)
}

/// Group scanned files into families: by family name, each style to its
/// slot (from the style name, else the file name), the first of Plato's
/// names winning where two files claim a slot. A family keeps a regular
/// face, or is a single file of any style, which then serves as regular.
/// Sorted by name.
pub fn group(files: Vec<Scanned>) -> Vec<Family> {
    let mut by_name: std::collections::BTreeMap<String, Vec<Scanned>> = Default::default();
    for f in files {
        by_name.entry(f.family.clone()).or_default().push(f);
    }
    let mut out = Vec::new();
    for (name, mut files) in by_name {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut faces: [Option<(usize, Face)>; 4] = Default::default();
        let single = files.len() == 1;
        for f in files {
            let rank = |slot: usize| {
                PLATO_STYLES[slot]
                    .iter()
                    .position(|s| *s == f.style)
                    .unwrap_or(PLATO_STYLES[slot].len())
            };
            let slot = slot_of_style(&f.style)
                .or_else(|| slot_of_file_name(&f.path))
                .or(single.then_some(0));
            let Some(slot) = slot else { continue };
            let r = rank(slot);
            let face = Face {
                path: f.path,
                style: f.style,
                size: f.size,
                mtime: f.mtime,
            };
            match &faces[slot] {
                Some((best, _)) if *best <= r => {}
                _ => faces[slot] = Some((r, face)),
            }
        }
        let faces = faces.map(|f| f.map(|(_, f)| f));
        if faces[0].is_some() {
            out.push(Family { name, faces });
        }
    }
    out
}

/// Whether `path` looks like a font file (`.ttf`, `.otf`, `.ttc`).
pub fn is_font(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        e.eq_ignore_ascii_case("ttf")
            || e.eq_ignore_ascii_case("otf")
            || e.eq_ignore_ascii_case("ttc")
    })
}

/// Format label (`TTF`, `OTF`, `TTC`) for a font path.
pub fn font_format(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_uppercase())
        .unwrap_or_else(|| "FONT".to_string())
}

/// Whether a top-level store folder should be skipped when browsing or
/// scanning for fonts. Unlike book scanning, the root `fonts` folder is
/// always allowed so users can inspect or keep it as their font directory.
pub fn excluded_font_store_folder(name: &str) -> bool {
    if name.eq_ignore_ascii_case("fonts") || name.eq_ignore_ascii_case("koreader") {
        return false;
    }
    aristotle_library::library::excluded_store_folder(name)
}

/// Resolves a designated fonts folder under `root`, allowing `/mnt/us/fonts`
/// while still blocking symlinks escaping `root`, hidden segments, and system
/// folders.
pub fn resolve_fonts_destination(root: &Path, destination: &Path) -> std::io::Result<PathBuf> {
    if !destination.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "fonts folder must be absolute",
        ));
    }
    let root = std::fs::canonicalize(root)?;
    let mut existing = destination.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let resolved = loop {
        match std::fs::canonicalize(&existing) {
            Ok(path) => {
                break missing
                    .iter()
                    .rev()
                    .fold(path, |path, part| path.join(part));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match (existing.file_name(), existing.parent()) {
                    (Some(name), Some(parent)) if parent != existing => {
                        missing.push(name.to_os_string());
                        existing = parent.to_path_buf();
                    }
                    _ => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "fonts folder cannot be resolved",
                        ));
                    }
                }
            }
            Err(e) => return Err(e),
        }
    };
    if !resolved.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "fonts folder must be inside the scan root",
        ));
    }
    let relative = resolved.strip_prefix(&root).unwrap_or(Path::new(""));
    for part in relative.components() {
        if let std::path::Component::Normal(name) = part {
            if name.to_string_lossy().starts_with('.') {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fonts folder cannot be hidden",
                ));
            }
        }
    }
    if root == Path::new(aristotle_library::library::KINDLE_STORE) {
        if let Some(std::path::Component::Normal(first)) = relative.components().next() {
            if excluded_font_store_folder(&first.to_string_lossy()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fonts folder cannot be a system folder",
                ));
            }
        }
    }
    Ok(resolved)
}

/// Effectiveness status of a discovered font file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontEffectStatus {
    /// Inside the designated fonts folder and registered with the renderer.
    Effective,
    /// Valid font file outside the designated fonts folder (ready to migrate).
    ValidPendingMigration,
    /// Inside the designated fonts folder and readable, but not yet registered.
    ValidNotRegistered,
    /// File exceeds [`MAX_BYTES`] or is empty.
    TooLarge,
    /// File cannot be parsed by FreeType or lacks a family name.
    Unreadable,
}

impl FontEffectStatus {
    /// Whether this font can be migrated into the designated fonts folder.
    pub fn migratable(self) -> bool {
        matches!(self, FontEffectStatus::ValidPendingMigration)
    }

    /// Short localized status label for the scan review list.
    pub fn label(self) -> &'static str {
        match self {
            FontEffectStatus::Effective => tr(Msg::FontStatusEffective),
            FontEffectStatus::ValidPendingMigration => tr(Msg::FontStatusPending),
            FontEffectStatus::ValidNotRegistered => tr(Msg::FontStatusNotRegistered),
            FontEffectStatus::TooLarge => tr(Msg::FontStatusTooLarge),
            FontEffectStatus::Unreadable => tr(Msg::FontStatusUnreadable),
        }
    }
}

/// A font file discovered during a unified book + font scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredFont {
    /// Path relative to the scan root.
    pub path: String,
    /// Display title: FreeType family name when valid, else file stem.
    pub title: String,
    /// Uppercase format (`TTF`, `OTF`, `TTC`).
    pub format: String,
    /// Whether the file already lives inside the designated fonts folder.
    pub in_destination: bool,
    /// Whether the font is currently effective / valid / why not.
    pub status: FontEffectStatus,
}

/// Face names currently registered in crengine's process-wide font manager.
pub fn registered_crengine_faces() -> Vec<String> {
    #[cfg(feature = "crengine")]
    {
        aristotle_render::crengine::registered_faces()
    }
    #[cfg(not(feature = "crengine"))]
    {
        Vec::new()
    }
}

/// Walks `sources` (or the whole `root` when `sources` is empty) for `.ttf`,
/// `.otf`, and `.ttc` files and inspects each with FreeType to report whether
/// it is effective, ready to migrate, or invalid.
pub fn discover_fonts_in_sources(
    root: &Path,
    sources: &[PathBuf],
    destination: &Path,
    registered_faces: &[String],
) -> Vec<DiscoveredFont> {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return Vec::new();
    };
    let dest_rel = crate::scan_scope::relative_path(
        &canonical_root,
        &std::fs::canonicalize(destination).unwrap_or_else(|_| destination.to_path_buf()),
    );
    let in_dest = |rel: &str| {
        !dest_rel.is_empty()
            && (rel == dest_rel
                || rel
                    .strip_prefix(&dest_rel)
                    .is_some_and(|rest| rest.starts_with('/')))
    };

    let mut roots: Vec<PathBuf> = if sources.is_empty() {
        vec![canonical_root.clone()]
    } else {
        sources.to_vec()
    };
    // Always include the designated fonts folder in whole-store scans so users
    // can inspect whether fonts already inside it are effective.
    if sources.is_empty() && destination.is_dir() {
        if let Ok(canon_dest) = std::fs::canonicalize(destination) {
            if !roots.iter().any(|r| canon_dest.starts_with(r)) {
                roots.push(canon_dest);
            }
        }
    }

    let mut found_paths = Vec::new();
    let mut visited_dirs = std::collections::HashSet::new();
    let is_kindle_store = canonical_root == Path::new(aristotle_library::library::KINDLE_STORE);
    for start in roots {
        collect_font_files(
            &canonical_root,
            &start,
            is_kindle_store,
            &mut visited_dirs,
            &mut found_paths,
        );
    }
    found_paths.sort();
    found_paths.dedup();

    let opener = FontOpener::new().ok();
    let mut out = Vec::new();
    for path in found_paths {
        let rel = crate::scan_scope::relative_path(&canonical_root, &path);
        if rel.is_empty() {
            continue;
        }
        let format = font_format(&path);
        let fallback_title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&rel)
            .to_string();
        let meta = match std::fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        let in_destination = in_dest(&rel);
        let (title, status) = if meta.len() == 0 || meta.len() > MAX_BYTES {
            (fallback_title, FontEffectStatus::TooLarge)
        } else if let Some(family) =
            opener
                .as_ref()
                .and_then(|o| o.open(&path).ok())
                .and_then(|f| {
                    f.family_name()
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                        .map(str::to_string)
                })
        {
            let status = if in_destination {
                if registered_faces.is_empty()
                    || registered_faces
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(&family))
                {
                    FontEffectStatus::Effective
                } else {
                    FontEffectStatus::ValidNotRegistered
                }
            } else {
                FontEffectStatus::ValidPendingMigration
            };
            (family, status)
        } else {
            (fallback_title, FontEffectStatus::Unreadable)
        };
        out.push(DiscoveredFont {
            path: rel,
            title,
            format,
            in_destination,
            status,
        });
    }
    out
}

fn collect_font_files(
    root: &Path,
    dir: &Path,
    is_kindle_store: bool,
    visited: &mut std::collections::HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) {
    let Ok(canon) = std::fs::canonicalize(dir) else {
        return;
    };
    if !canon.starts_with(root) || !visited.insert(canon.clone()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&canon) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            if name_str.to_lowercase().ends_with(".sdr") {
                continue;
            }
            if is_kindle_store && canon == root && excluded_font_store_folder(&name_str) {
                continue;
            }
            collect_font_files(root, &path, is_kindle_store, visited, out);
        } else if ft.is_file() && is_font(&path) {
            out.push(path);
        }
    }
}

/// Safely migrates one font file (`relative` under `root`) into `destination`.
///
/// Copies through a synced temporary file, verifies size and byte equality,
/// installs into a unique name with `create_new`, syncs the destination
/// directory, and only then removes the original file.
pub fn migrate_font(root: &Path, relative: &str, destination: &Path) -> std::io::Result<PathBuf> {
    use std::io::{Read, Write};

    let canon_root = std::fs::canonicalize(root)?;
    let source = canon_root.join(relative);
    let canon_source = std::fs::canonicalize(&source)?;
    if !canon_source.starts_with(&canon_root) || !canon_source.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "font source is outside the scan root",
        ));
    }
    let dest = resolve_fonts_destination(&canon_root, destination)?;
    std::fs::create_dir_all(&dest)?;
    let canon_dest = std::fs::canonicalize(&dest)?;
    if canon_source.parent() == Some(canon_dest.as_path()) {
        return Ok(canon_source);
    }

    let raw_name = canon_source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "font has no file name")
        })?;

    let target = {
        let mut attempt = 0usize;
        loop {
            let candidate_name = if attempt == 0 {
                raw_name.clone()
            } else {
                let n = attempt + 1;
                match raw_name.rsplit_once('.') {
                    Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
                    _ => format!("{raw_name} ({n})"),
                }
            };
            let candidate = canon_dest.join(&candidate_name);
            if !candidate.exists() {
                break candidate;
            }
            attempt += 1;
            if attempt > 9999 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "no free filename in fonts destination",
                ));
            }
        }
    };

    let temp = canon_dest.join(format!(
        ".{}.part",
        target
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    let copy_result = (|| -> std::io::Result<()> {
        let mut src = std::fs::File::open(&canon_source)?;
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
        }
        dst.flush()?;
        dst.sync_all()?;
        Ok(())
    })();
    if let Err(e) = copy_result {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }

    // Verify byte-for-byte before renaming and removing source.
    let same = (|| -> std::io::Result<bool> {
        let m1 = std::fs::metadata(&canon_source)?;
        let m2 = std::fs::metadata(&temp)?;
        if m1.len() != m2.len() {
            return Ok(false);
        }
        let mut f1 = std::fs::File::open(&canon_source)?;
        let mut f2 = std::fs::File::open(&temp)?;
        let mut b1 = [0u8; 64 * 1024];
        let mut b2 = [0u8; 64 * 1024];
        loop {
            let n1 = f1.read(&mut b1)?;
            let n2 = f2.read(&mut b2)?;
            if n1 != n2 || b1[..n1] != b2[..n2] {
                return Ok(false);
            }
            if n1 == 0 {
                return Ok(true);
            }
        }
    })()?;
    if !same {
        let _ = std::fs::remove_file(&temp);
        return Err(std::io::Error::other("font copy verification failed"));
    }

    std::fs::rename(&temp, &target)?;
    if let Ok(dir) = std::fs::File::open(&canon_dest) {
        let _ = dir.sync_all();
    }
    std::fs::remove_file(&canon_source)?;
    if let Some(parent) = canon_source.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(target)
}

/// The font files in `dir` (not its subfolders), sorted, with their
/// metadata. Hidden files (macOS leaves `._Name.ttf` next to each) are
/// skipped.
fn list(dir: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| is_font(p))
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?;
            m.is_file().then_some((p, m))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Read every font file in `dir` with FreeType.
pub fn scan(dir: &Path) -> Vec<Family> {
    let Ok(opener) = FontOpener::new() else {
        return Vec::new();
    };
    let mut scanned = Vec::new();
    for (path, meta) in list(dir) {
        if meta.len() > MAX_BYTES || meta.len() == 0 {
            continue;
        }
        let Ok(font) = opener.open(&path) else {
            continue;
        };
        // Plato matches families by this name; a face without one could
        // not be loaded.
        let Some(family) = font.family_name().filter(|n| !n.trim().is_empty()) else {
            continue;
        };
        scanned.push(Scanned {
            family: family.to_string(),
            style: font.style_name().unwrap_or("Regular").to_string(),
            size: meta.len(),
            mtime: meta.mtime(),
            path,
        });
    }
    group(scanned)
}

/// The cached list: the folder's mtime and file names it was made from.
#[derive(Serialize, Deserialize)]
struct List {
    dir: PathBuf,
    key: u64,
    families: Vec<Family>,
}

/// What decides whether a cached list is still good: the folder's mtime
/// and its font files' names, sizes and mtimes (FAT keeps mtimes to 2 s,
/// and replacing a file need not touch the folder).
fn folder_key(dir: &Path) -> Option<u64> {
    let meta = std::fs::metadata(dir).ok()?;
    let mut h = DefaultHasher::new();
    (meta.mtime(), meta.mtime_nsec()).hash(&mut h);
    for (p, m) in list(dir) {
        (p, m.len(), m.mtime()).hash(&mut h);
    }
    Some(h.finish())
}

/// The families in `dir`, from the cache in `data_dir` when the folder has
/// not changed. An absent folder has none.
pub fn families(data_dir: &Path, dir: &Path) -> Vec<Family> {
    let Some(key) = folder_key(dir) else {
        return Vec::new();
    };
    let path = data_dir.join(LIST_FILE);
    if let Some(cached) = std::fs::read(&path)
        .ok()
        .and_then(|d| serde_json::from_slice::<List>(&d).ok())
        .filter(|l| l.dir == dir && l.key == key)
    {
        return cached.families;
    }
    let families = scan(dir);
    let list = List {
        dir: dir.to_path_buf(),
        key,
        families,
    };
    if let Ok(data) = serde_json::to_vec_pretty(&list) {
        if let Err(e) = aristotle_library::index::write_atomic(&path, &data) {
            eprintln!("aristotle: can't save the font list: {e}");
        }
    }
    list.families
}

/// A file-name-safe form of a family name.
fn slug(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.trim_matches('-').to_string()
}

/// The folder Plato loads `family` from: its copy in `data_dir`, made now if
/// missing. Older copies of the same family are removed.
pub fn prepare(data_dir: &Path, family: &Family) -> std::io::Result<PathBuf> {
    let root = data_dir.join(COPIES_DIR);
    let base = slug(&family.name);
    let name = format!("{base}-{:016x}", family.stamp());
    let dir = root.join(&name);
    if dir.is_dir() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&root)?;
    // Copy into a fresh folder and rename it into place, so a copy cut
    // short by a crash or a full drive is never used.
    let part = root.join(format!("{name}.part"));
    if part.exists() {
        std::fs::remove_dir_all(&part)?;
    }
    std::fs::create_dir(&part)?;
    for (slot, f) in family.for_plato() {
        let ext = f.path.extension().and_then(|e| e.to_str()).unwrap_or("ttf");
        let to = part.join(format!(
            "{base}-{}.{}",
            SLOTS[slot],
            ext.to_ascii_lowercase()
        ));
        std::fs::copy(&f.path, &to)?;
    }
    std::fs::rename(&part, &dir)?;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let stale = n != name
                && n.strip_prefix(&base)
                    .and_then(|r| r.strip_prefix('-'))
                    .is_some_and(|r| {
                        r.len() >= 16 && r[..16].bytes().all(|b| b.is_ascii_hexdigit())
                    });
            if stale {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(dir)
}

/// The folder for the family called `name` in `dir`, prepared; `None` if
/// it is not there (any more) or cannot be copied.
#[cfg(test)]
fn resolve(data_dir: &Path, dir: &Path, name: &str) -> Option<PathBuf> {
    let families = families(data_dir, dir);
    let family = families.iter().find(|f| f.name == name)?;
    match prepare(data_dir, family) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("aristotle: can't prepare the font {name}: {e}");
            None
        }
    }
}

/// Script group a reading font belongs to (Chinese/CJK vs Western/Latin).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontScript {
    /// Chinese / Japanese / Korean body font.
    Cjk,
    /// Western / Latin body font.
    Latin,
}

/// Known Western / Latin font families whose names may contain ambiguous words
/// (such as "Gothic" in "Century Gothic" or "Franklin Gothic").
const KNOWN_LATIN_PATTERNS: &[&str] = &[
    "literata",
    "charis",
    "gentium",
    "bookerly",
    "caecilia",
    "amazon ember",
    "baskerville",
    "palatino",
    "garamond",
    "georgia",
    "helvetica",
    "times new roman",
    "arial",
    "verdana",
    "tahoma",
    "trebuchet",
    "century gothic",
    "franklin gothic",
    "trade gothic",
    "news gothic",
    "copperplate gothic",
    "bank gothic",
    "didot",
    "bodoni",
    "caslon",
    "minion",
    "myriad",
    "frutiger",
    "futura",
    "gill sans",
    "optima",
    "avenir",
    "sabon",
    "bembo",
    "janson",
    "rockwell",
    "clarendon",
    "cambria",
    "calibri",
    "candara",
    "consolas",
    "constantia",
    "corbel",
    "segoe",
    "roboto",
    "open sans",
    "lato",
    "montserrat",
    "merriweather",
    "lora",
    "crimson",
    "fira",
    "inter",
    "jetbrains",
    "cascadia",
    "ubuntu",
    "dejavu",
    "liberation",
    "atkinson",
    "spectral",
    "alegreya",
    "vollkorn",
    "bitter",
    "ibm plex",
    "pt serif",
    "pt sans",
    "stix",
    "linux libertine",
    "linux biolinum",
    "tex gyre",
    "charter",
    "iowan",
    "new york",
    "san francisco",
    "sf pro",
    "sf mono",
    "sf compact",
    "cochin",
    "hoefler",
    "american typewriter",
    "courier",
    "monaco",
    "menlo",
];

/// Known Chinese / CJK font families, foundries, and romanized style names.
///
/// Matched case-insensitively against the FreeType family name so CJK fonts
/// with English metadata (such as `LXGW WenKai Lite`, `Source Han Serif CN`,
/// `Zhuque Fangsong`, `FZShuSong`, `HYQiHei`, `PingFang`, `SimSun`, `MiSans`)
/// are classified into the Chinese font row rather than the Western row.
const KNOWN_CJK_PATTERNS: &[&str] = &[
    // Open-source & popular e-ink Chinese reading fonts
    "lxgw",
    "wenkai",
    "xihei",
    "source han",
    "siyuan",
    "zhuque",
    "huiwen",
    "kinghwa",
    "jinghua",
    "wenyue",
    "canger",
    "tsanger",
    "smiley sans",
    "zcool",
    "wenquanyi",
    "fandol",
    "sarasa",
    "genwan",
    "genyo",
    "genryu",
    "genseki",
    "gensen",
    "genjyuu",
    "cwtex",
    "ar pl",
    "i.ming",
    "oradano",
    "hanazono",
    "babelstone han",
    "plangothic",
    "maple mono cn",
    "ma shan zheng",
    "zhi mang xing",
    "liu jian mao cao",
    "long cang",
    "jiangcheng",
    "jinling",
    "chillround",
    "chillkai",
    "yozai",
    "hanyi",
    "founder",
    "fangzheng",
    "zhongyi",
    "changfangsong",
    // System & commercial CJK fonts (Windows, macOS, Android, OEM)
    "simsun",
    "nsimsun",
    "simhei",
    "simkai",
    "simfang",
    "simli",
    "simyou",
    "kaiti",
    "fangsong",
    "songti",
    "heiti",
    "yahei",
    "jhenghei",
    "dengxian",
    "mingliu",
    "pmingliu",
    "dfkai",
    "pingfang",
    "stsong",
    "stkaiti",
    "stfangsong",
    "stheiti",
    "stxihei",
    "stzhongsong",
    "stxinwei",
    "stxingkai",
    "stcaiyun",
    "sthupo",
    "stliti",
    "lisu",
    "youyuan",
    "lantinghei",
    "lanting",
    "baoli",
    "hannotate",
    "hanzipen",
    "libian",
    "lingwai",
    "wawati",
    "weibei",
    "xingkai",
    "yuanti",
    "yuppy",
    "biaukai",
    "hiragino sans gb",
    "hiragino mincho",
    "hiragino kaku",
    "hiragino maru",
    "misans",
    "harmonyos sans sc",
    "harmonyos sans tc",
    "puhuiti",
    "alibaba sans",
    "oppo sans",
    "vivo sans",
    "honor sans",
    "droid sans fallback",
    // Common Chinese romanized style suffixes/words
    "shusong",
    "biaosong",
    "xinsong",
    "cusong",
    "zhongsong",
    "yansong",
    "xiaobiaosong",
    "mingti",
    "yuansong",
    "keshu",
    // Japanese / Korean CJK reading families
    "kozuka",
    "yumincho",
    "yugothic",
    "meiryo",
    "ms mincho",
    "ms gothic",
    "ms pmincho",
    "ms pgothic",
    "ipaex",
    "ipamincho",
    "ipagothic",
    "shippori",
    "klee",
    "biz ud",
    "nanum",
    "malgun",
    "batang",
    "dotum",
    "gulim",
    "gungsuh",
    "pretendard",
    "spoqa",
];

/// Foundry prefixes commonly used in Chinese commercial/free font family names
/// (Founder `FZ*`, Hanyi `HY*`, DynaFont `DF*`, Arphic `AR*`, Taiwan `TW-*`,
/// WeRead/QQ `WR*` / `DK-*`).
const KNOWN_CJK_PREFIXES: &[&str] = &["fz", "hy", "df", "tw-", "han "];

/// Regional/script tokens that identify a CJK variant when appearing as a
/// standalone word (e.g. `Noto Serif SC`, `Source Han Serif CN`, `Noto Sans HK`).
const CJK_REGION_TOKENS: &[&str] = &["cjk", "sc", "tc", "cn", "hk", "tw", "jp", "kr", "gb", "gbk"];

fn is_cjk_char(c: char) -> bool {
    matches!(
        c as u32,
        0x2E80..=0x2EFF
            | 0x3000..=0x30FF
            | 0x3100..=0x312F
            | 0x31A0..=0x31BF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2FA1F
    )
}

/// Classify a font family name using the built-in Chinese / Western font
/// knowledge base. Returns `Some(FontScript)` when the name matches a known
/// rule, or `None` when the name is unrecognized and file-level inspection
/// should be consulted.
pub fn classify_font_name_known(name: &str) -> Option<FontScript> {
    if name.chars().any(is_cjk_char) {
        return Some(FontScript::Cjk);
    }
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }

    // Explicit CJK knowledge base matches take precedence before generic Latin
    // prefixes (e.g. `Source Han Serif CN` vs `Source Serif`, `Noto Sans SC` vs `Noto Sans`).
    if KNOWN_CJK_PATTERNS.iter().any(|pat| lower.contains(pat)) {
        return Some(FontScript::Cjk);
    }

    // Standalone regional/script tokens such as `SC`, `TC`, `CN`, `CJK`, `HK`, `JP`, `KR`.
    if lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| CJK_REGION_TOKENS.contains(&tok))
    {
        return Some(FontScript::Cjk);
    }

    // Foundry prefixes like `FZShuSong-Z01`, `HYQiHei-55S`, `DFKai-SB`, `TW-Kai`.
    if KNOWN_CJK_PREFIXES.iter().any(|prefix| {
        lower.starts_with(prefix)
            && (prefix.ends_with('-')
                || prefix.ends_with(' ')
                || name.len() > prefix.len()
                    && name
                        .as_bytes()
                        .get(prefix.len())
                        .is_some_and(|b| b.is_ascii_uppercase() || *b == b'-' || *b == b'_'))
    }) {
        return Some(FontScript::Cjk);
    }

    if KNOWN_LATIN_PATTERNS.iter().any(|pat| lower.contains(pat)) {
        return Some(FontScript::Latin);
    }

    None
}

/// Inspect a TTF/OTF/TTC font file's SFNT `OS/2` table to determine whether it
/// declares CJK Unicode or codepage coverage.
///
/// Reads only the table directory and the 86-byte `OS/2` header, so it stays
/// fast even on large 20+ MiB CJK fonts.
fn sfnt_declares_cjk(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 16];
    file.read_exact(&mut header).ok()?;

    let sfnt_offset: u64 = if &header[0..4] == b"ttcf" {
        let num_fonts = u32::from_be_bytes(header[8..12].try_into().ok()?);
        if num_fonts == 0 {
            return None;
        }
        u32::from_be_bytes(header[12..16].try_into().ok()?) as u64
    } else {
        0
    };

    file.seek(SeekFrom::Start(sfnt_offset)).ok()?;
    let mut table_dir_hdr = [0u8; 12];
    file.read_exact(&mut table_dir_hdr).ok()?;
    let sfnt_tag = &table_dir_hdr[0..4];
    if sfnt_tag != [0x00, 0x01, 0x00, 0x00] && sfnt_tag != b"OTTO" && sfnt_tag != b"true" {
        return None;
    }
    let num_tables = u16::from_be_bytes(table_dir_hdr[4..6].try_into().ok()?) as usize;
    let mut os2_offset_len: Option<(u64, u32)> = None;
    let mut rec = [0u8; 16];
    for _ in 0..num_tables.min(128) {
        file.read_exact(&mut rec).ok()?;
        if &rec[0..4] == b"OS/2" {
            let offset = u32::from_be_bytes(rec[8..12].try_into().ok()?) as u64;
            let length = u32::from_be_bytes(rec[12..16].try_into().ok()?);
            os2_offset_len = Some((offset, length));
            break;
        }
    }
    let (offset, length) = os2_offset_len?;
    if length < 58 {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let read_len = (length as usize).min(86);
    let mut os2 = [0u8; 86];
    file.read_exact(&mut os2[..read_len]).ok()?;

    let version = u16::from_be_bytes([os2[0], os2[1]]);
    // ulUnicodeRange2 is at byte offset 46..50:
    // bit 27 (0x0800_0000) = CJK Unified Ideographs (U+4E00..U+9FFF)
    let unicode_range_2 = u32::from_be_bytes([os2[46], os2[47], os2[48], os2[49]]);
    let has_cjk_ideographs = (unicode_range_2 & (1 << 27)) != 0;

    // For OS/2 version >= 1, ulCodePageRange1 is at byte offset 78..82:
    // bit 17 = JIS/Japan, bit 18 = Chinese Simplified, bit 19 = Korean, bit 20 = Chinese Traditional
    let has_cjk_codepage = if version >= 1 && read_len >= 82 {
        let cp1 = u32::from_be_bytes([os2[78], os2[79], os2[80], os2[81]]);
        (cp1 & ((1 << 17) | (1 << 18) | (1 << 19) | (1 << 20))) != 0
    } else {
        false
    };

    Some(has_cjk_ideographs || has_cjk_codepage)
}

/// Classify a font family name into [`FontScript::Cjk`] or [`FontScript::Latin`]
/// using the font knowledge base alone.
#[cfg(test)]
pub fn classify_font_name(name: &str) -> FontScript {
    classify_font_name_known(name).unwrap_or(FontScript::Latin)
}

/// Classify a scanned [`Family`] into [`FontScript::Cjk`] or [`FontScript::Latin`]:
/// checks the font knowledge base first, and falls back to inspecting the
/// regular font file's SFNT `OS/2` CJK coverage bits for unrecognized names.
pub fn classify_family(family: &Family) -> FontScript {
    if let Some(known) = classify_font_name_known(&family.name) {
        return known;
    }
    if let Some(true) = sfnt_declares_cjk(&family.regular().path) {
        return FontScript::Cjk;
    }
    FontScript::Latin
}

/// Which font group is being configured in the reader menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontTarget {
    Latin,
    Cjk,
}

/// A row of the font list.
pub struct FontRow {
    pub name: String,
    pub meta: Option<String>,
    /// The UI family the name is set in; `None` for the hint.
    pub ui: Option<FamilyId>,
    /// What picking the row chooses; `None` for the hint.
    pub pick: Option<Pick>,
}

pub enum Pick {
    Default,
    Family(Box<Family>),
}

/// Regular faces up to this size are read to show the family's name in its
/// own face; larger ones show it in the UI font.
const PREVIEW_BYTES: u64 = 2 * 1024 * 1024;

impl App {
    /// At start: find the folders of the reading fonts in the preferences.
    /// A family no longer there reads in default, and comes back when its
    /// files do.
    pub(crate) fn load_font(&mut self) {
        let t = std::time::Instant::now();
        self.prefs.font_dir = None;
        self.prefs.font_ui = 0;
        self.prefs.font_cjk_dir = None;
        self.prefs.font_cjk_ui = 0;

        let data = self.lib.data_dir().to_path_buf();
        let families = families(&data, &self.fonts_dir);

        if self.prefs.font_latin != DEFAULT_LATIN {
            if let Some(family) = families.iter().find(|f| f.name == self.prefs.font_latin) {
                match prepare(&data, family) {
                    Ok(dir) => {
                        self.prefs.font_dir = Some(dir.to_string_lossy().into_owned());
                        self.prefs.font_ui = self.ui_family(family).0 as usize;
                    }
                    Err(e) => eprintln!("aristotle: can't prepare the font {}: {e}", family.name),
                }
            } else {
                perf!(
                    "fonts: Latin {} is not in {}",
                    self.prefs.font_latin,
                    self.fonts_dir.display()
                );
            }
        }

        if self.prefs.font_cjk != DEFAULT_CJK {
            if let Some(family) = families.iter().find(|f| f.name == self.prefs.font_cjk) {
                match prepare(&data, family) {
                    Ok(dir) => {
                        self.prefs.font_cjk_dir = Some(dir.to_string_lossy().into_owned());
                        self.prefs.font_cjk_ui = self.ui_family(family).0 as usize;
                    }
                    Err(e) => {
                        eprintln!("aristotle: can't prepare the CJK font {}: {e}", family.name)
                    }
                }
            } else {
                perf!(
                    "fonts: CJK {} is not in {}",
                    self.prefs.font_cjk,
                    self.fonts_dir.display()
                );
            }
        }

        self.register_user_fonts_for_crengine();

        perf!(
            "fonts: Latin '{}', CJK '{}' ready in {:.0} ms",
            self.prefs.font_latin,
            self.prefs.font_cjk,
            crate::ms(t.elapsed())
        );
    }

    /// Register all TTF/OTF fonts in user's `fonts/` directory to crengine.
    pub(crate) fn register_user_fonts_for_crengine(&self) {
        #[cfg(feature = "crengine")]
        {
            let _ = aristotle_render::crengine::register_font_dir(&self.fonts_dir);
        }
    }

    /// Register the user's fonts for crengine, saying so in the status bar
    /// when it takes long. The work is synchronous, so the bar can only
    /// say it after the fact: the time it took is remembered, and the next
    /// book opens with the word on the bar before the work starts.
    pub(crate) fn register_fonts_for_crengine(&mut self) -> Result<(), Error> {
        let slow = self.font_load_ms >= FONT_LOAD_SLOW_MS;
        if slow {
            self.set_loading(
                crate::LoadingSource::Fonts,
                Some(Loading::without_percent(tr(Msg::StatusFonts).to_string())),
            );
            self.render_ui()?;
        }
        let t = std::time::Instant::now();
        self.register_user_fonts_for_crengine();
        self.font_load_ms = crate::ms(t.elapsed()) as u64;
        if slow {
            self.set_loading(crate::LoadingSource::Fonts, None);
        }
        Ok(())
    }

    /// The UI family showing `family`'s name: its regular face, read once,
    /// or the UI font when that is large or unreadable.
    pub(crate) fn ui_family(&mut self, family: &Family) -> FamilyId {
        let regular = family.regular();
        if let Some(id) = self.font_faces.get(&regular.path) {
            return *id;
        }
        let id = (regular.size <= PREVIEW_BYTES)
            .then(|| std::fs::read(&regular.path).ok())
            .flatten()
            .and_then(|bytes| {
                let fonts = &mut self.ui.text.fonts;
                let face = fonts
                    .add_face(aristotle_ui::text::FontData::Shared(std::sync::Arc::new(
                        bytes,
                    )))
                    .ok()?;
                fonts.add_family(&family.name, [Some(face), None, None, None])
            })
            .unwrap_or(FamilyId(0));
        self.font_faces.insert(regular.path.clone(), id);
        id
    }

    /// Applies a reading font chosen on a list that is not the reader's own
    /// (the settings typeset panel). The page is laid out again only when
    /// the book next opens; nothing on the panel needs a live preview.
    pub(crate) fn apply_reading_font(&mut self, target: FontTarget, name: &str) {
        let data = self.lib.data_dir().to_path_buf();
        let default = match target {
            FontTarget::Latin => DEFAULT_LATIN,
            FontTarget::Cjk => DEFAULT_CJK,
        };
        let chosen = if name == default {
            Some((default.to_string(), None, 0))
        } else {
            let all = families(&data, &self.fonts_dir);
            all.iter().find(|f| f.name == name).and_then(|family| {
                let dir = prepare(&data, family).ok()?;
                let ui = self.ui_family(family).0 as usize;
                Some((
                    family.name.clone(),
                    Some(dir.to_string_lossy().into_owned()),
                    ui,
                ))
            })
        };
        let Some((name, dir, ui)) = chosen else {
            return;
        };
        match target {
            FontTarget::Latin => {
                self.prefs.font_latin = name;
                self.prefs.font_dir = dir;
                self.prefs.font_ui = ui;
            }
            FontTarget::Cjk => {
                self.prefs.font_cjk = name;
                self.prefs.font_cjk_dir = dir;
                self.prefs.font_cjk_ui = ui;
            }
        }
        self.save_prefs();
        self.register_user_fonts_for_crengine();
    }

    /// The list of fonts, from the menu's font row: default, then the
    /// user's families. One full update.
    pub(crate) fn open_fonts(&mut self, target: FontTarget) -> Result<(), Error> {
        let t = std::time::Instant::now();
        let data = self.lib.data_dir().to_path_buf();
        let default_name = match target {
            FontTarget::Latin => DEFAULT_LATIN,
            FontTarget::Cjk => DEFAULT_CJK,
        };
        let families: Vec<Family> = families(&data, &self.fonts_dir)
            .into_iter()
            .filter(|f| f.name != default_name)
            .collect();
        let in_use = match target {
            FontTarget::Latin => self.prefs.font_latin_in_use().to_string(),
            FontTarget::Cjk => self.prefs.font_cjk_in_use().to_string(),
        };
        let mark = |name: &str, meta: Option<&str>| {
            let used = (name == in_use).then(|| tr(Msg::ReaderFontInUse));
            let parts: Vec<&str> = meta.into_iter().chain(used).collect();
            (!parts.is_empty()).then(|| parts.join(" · "))
        };
        let mut rows = vec![FontRow {
            name: default_name.into(),
            meta: mark(default_name, Some(tr(Msg::ReaderFontBuiltIn))),
            ui: Some(FamilyId(0)),
            pick: Some(Pick::Default),
        }];
        let registered = registered_crengine_faces();
        for f in &families {
            let ui = self.ui_family(f);
            let status = if registered.is_empty()
                || registered.iter().any(|r| r.eq_ignore_ascii_case(&f.name))
            {
                Some(tr(Msg::FontStatusEffective))
            } else {
                Some(tr(Msg::FontStatusNotRegistered))
            };
            rows.push(FontRow {
                name: f.name.clone(),
                meta: mark(&f.name, status),
                ui: Some(ui),
                pick: Some(Pick::Family(Box::new(f.clone()))),
            });
        }
        if families.is_empty() {
            rows.push(FontRow {
                name: tr(Msg::ReaderFontsHint).into(),
                meta: None,
                ui: None,
                pick: None,
            });
        }
        perf!(
            "fonts: {} families listed in {:.0} ms for {:?}",
            families.len(),
            crate::ms(t.elapsed()),
            target
        );
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.overlay = Overlay::Fonts {
            target,
            rows,
            page: 0,
        };
        r.menu_open = false;
        self.rebuild_reader(Refresh::Auto)?;
        self.present_full()
    }

    /// A row of the font list tapped: lay the book out in that font and show
    /// it under the menu, in one full update. The tapped row's outline is
    /// the acknowledgement meanwhile.
    pub(crate) fn pick_font(&mut self, i: usize) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let Overlay::Fonts { target, rows, .. } = &r.overlay else {
            return Ok(());
        };
        let target = *target;
        let Some(pick) = rows.get(i).and_then(|row| row.pick.as_ref()) else {
            return Ok(());
        };
        let default_name = match target {
            FontTarget::Latin => DEFAULT_LATIN,
            FontTarget::Cjk => DEFAULT_CJK,
        };
        let data = self.lib.data_dir().to_path_buf();
        let (name, dir, ui) = match pick {
            Pick::Default => (default_name.to_string(), None, FamilyId(0)),
            Pick::Family(f) => {
                let f = (**f).clone();
                match prepare(&data, &f) {
                    Ok(dir) => {
                        let ui = self.ui_family(&f);
                        (f.name, Some(dir.to_string_lossy().into_owned()), ui)
                    }
                    Err(e) => {
                        eprintln!("aristotle: can't prepare the font {}: {e}", f.name);
                        return self.fonts_back();
                    }
                }
            }
        };
        let already_in_use = match target {
            FontTarget::Latin => name == self.prefs.font_latin_in_use(),
            FontTarget::Cjk => name == self.prefs.font_cjk_in_use(),
        };
        if already_in_use {
            return self.fonts_back();
        }
        match target {
            FontTarget::Latin => {
                self.prefs.font_latin = name;
                self.prefs.font_dir = dir;
                self.prefs.font_ui = ui.0 as usize;
            }
            FontTarget::Cjk => {
                self.prefs.font_cjk = name;
                self.prefs.font_cjk_dir = dir;
                self.prefs.font_cjk_ui = ui.0 as usize;
            }
        }
        self.save_prefs();
        self.register_user_fonts_for_crengine();
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.overlay = Overlay::None;
        r.nodes = Default::default();
        r.menu_open = true;
        r.gestures.reset();
        let mut settings = r.settings;
        settings.family = self.prefs.font_ui;
        settings.cjk_family = self.prefs.font_cjk_ui;
        self.request_restyle(settings)
    }

    /// "Back" from the font list: the menu over the page, in one full
    /// update.
    pub(crate) fn fonts_back(&mut self) -> Result<(), Error> {
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        r.overlay = Overlay::None;
        r.menu_open = true;
        r.gestures.reset();
        self.rebuild_reader(Refresh::Auto)?;
        let Screen::Reader(r) = &mut self.screen else {
            return Ok(());
        };
        let page = r.session.page(r.session.current)?;
        crate::fit_menu(
            &mut self.ui,
            &self.styles,
            &r.session.title,
            &r.settings,
            &page,
            &mut r.menu,
            &mut r.page_node,
            &mut r.nav,
            &mut r.progress,
        );
        crate::update_menu_labels(&mut self.ui, r, self.readest.manual_message.as_deref());
        self.present_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../aristotle-ui/assets/fonts");

    fn scanned(path: &str, family: &str, style: &str) -> Scanned {
        Scanned {
            path: PathBuf::from(path),
            size: 1,
            mtime: 0,
            family: family.into(),
            style: style.into(),
        }
    }

    fn slots(f: &Family) -> Vec<Option<&str>> {
        f.faces
            .iter()
            .map(|f| f.as_ref().map(|f| f.path.to_str().unwrap()))
            .collect()
    }

    #[test]
    fn styles_and_file_names_give_slots() {
        assert_eq!(slot_of_style("Regular"), Some(0));
        assert_eq!(slot_of_style("Bold Italic"), Some(3));
        assert_eq!(slot_of_style("Oblique"), Some(1));
        assert_eq!(slot_of_style("Light"), None);
        assert_eq!(
            slot_of_file_name(Path::new("a/Foo-BoldItalic.otf")),
            Some(3)
        );
        assert_eq!(slot_of_file_name(Path::new("Foo_Italic.ttf")), Some(1));
        assert_eq!(slot_of_file_name(Path::new("Foo.ttf")), None);
    }

    #[test]
    fn grouping_by_name_table() {
        let fams = group(vec![
            scanned("x/b.ttf", "Foo", "Bold"),
            scanned("x/a.ttf", "Foo", "Regular"),
            scanned("x/m.ttf", "Foo", "Medium"),
            scanned("x/i.ttf", "Foo", "Italic"),
            scanned("x/l.ttf", "Foo", "Light"),
            // A style name only the file name explains.
            scanned("x/Foo-BoldItalic.ttf", "Foo", "Heavy Slanted"),
            // No regular face among several: left out.
            scanned("x/bar-i.ttf", "Bar", "Italic"),
            scanned("x/bar-b.ttf", "Bar", "Bold"),
            // A single file of any style serves as regular.
            scanned("x/one.ttf", "Solo", "Light"),
        ]);
        let names: Vec<_> = fams.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Foo", "Solo"]);
        assert_eq!(
            slots(&fams[0]),
            [
                Some("x/a.ttf"),
                Some("x/i.ttf"),
                Some("x/b.ttf"),
                Some("x/Foo-BoldItalic.ttf")
            ]
        );
        // Plato would not take "Heavy Slanted" as bold italic: not copied.
        let plato: Vec<usize> = fams[0].for_plato().iter().map(|(s, _)| *s).collect();
        assert_eq!(plato, [0, 1, 2]);
        // A regular Plato would not find goes alone.
        let normal = group(vec![
            scanned("y/n.ttf", "Baz", "Normal"),
            scanned("y/i.ttf", "Baz", "Italic"),
        ]);
        assert_eq!(normal[0].for_plato().len(), 1);
    }

    /// The OFL fonts in the repository, copied under other names, a broken
    /// file, an italic without its regular and a file that is too large.
    #[test]
    fn scanning_a_folder_of_real_fonts() {
        let dir = crate::testdir("fonts-scan");
        let user = dir.join("fonts");
        std::fs::create_dir_all(&user).unwrap();
        let assets = Path::new(ASSETS);
        for (from, to) in [
            ("Charis-Regular.ttf", "c1.ttf"),
            ("Charis-Italic.ttf", "c2.TTF"),
            ("GentiumBook-Italic.ttf", "gentium.otf"),
            ("Literata-Regular.ttf", "lit.ttf"),
            ("Literata-Bold.ttf", "lit-b.ttf"),
            ("Literata-BoldItalic.ttf", "lit-bi.ttf"),
        ] {
            std::fs::copy(assets.join(from), user.join(to)).unwrap();
        }
        std::fs::write(user.join("broken.ttf"), b"not a font").unwrap();
        std::fs::write(user.join("notes.txt"), b"x").unwrap();
        std::fs::copy(assets.join("Charis-Regular.ttf"), user.join("._c1.ttf")).unwrap();
        let big = std::fs::File::create(user.join("huge.ttf")).unwrap();
        big.set_len(MAX_BYTES + 1).unwrap();

        let fams = families(&dir, &user);
        let names: Vec<_> = fams.iter().map(|f| f.name.as_str()).collect();
        // Gentium Book has only its italic here.
        assert_eq!(names, ["Charis", "Literata"]);
        let charis = &fams[0];
        assert!(charis.faces[0].as_ref().unwrap().path.ends_with("c1.ttf"));
        assert!(charis.faces[1].as_ref().unwrap().path.ends_with("c2.TTF"));
        assert!(charis.faces[2].is_none());
        let lit = &fams[1];
        assert!(lit.faces[1].is_none() && lit.faces[2].is_some() && lit.faces[3].is_some());

        // The list is cached until the folder changes.
        assert!(dir.join(LIST_FILE).exists());
        assert_eq!(families(&dir, &user), fams);
        std::fs::copy(
            assets.join("GentiumBook-Regular.ttf"),
            user.join("gentium-r.ttf"),
        )
        .unwrap();
        let names: Vec<_> = families(&dir, &user).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["Charis", "Gentium Book", "Literata"]);
        assert!(families(&dir, &dir.join("absent")).is_empty());
    }

    /// The copy holds the faces under fixed names; Plato loads the family
    /// from it with its italic; a changed source gets a new folder and the
    /// old one goes.
    #[test]
    fn prepared_folder_loads_in_plato() {
        use aristotle_render::plato_core::font::FontFamily;
        let dir = crate::testdir("fonts-prepare");
        let user = dir.join("fonts");
        std::fs::create_dir_all(&user).unwrap();
        let assets = Path::new(ASSETS);
        std::fs::copy(assets.join("Charis-Regular.ttf"), user.join("one.ttf")).unwrap();
        std::fs::copy(assets.join("Charis-Italic.ttf"), user.join("two.ttf")).unwrap();
        let p = resolve(&dir, &user, "Charis").unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&p)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["Charis-Italic.ttf", "Charis-Regular.ttf"]);
        let fam = FontFamily::from_name("Charis", &p).expect("Plato loads the copy");
        assert_eq!(fam.italic.style_name(), Some("Italic"));
        assert_eq!(resolve(&dir, &user, "Charis").unwrap(), p, "made once");
        assert!(resolve(&dir, &user, "Nothing").is_none());

        // Replace the italic: a new folder, the old one removed.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::copy(assets.join("Charis-Regular.ttf"), user.join("two.ttf")).unwrap();
        let q = resolve(&dir, &user, "Charis").unwrap();
        assert_ne!(p, q);
        assert!(!p.exists());
    }

    #[test]
    fn font_knowledge_base_classifies_chinese_and_western_fonts() {
        for cjk in [
            "LXGW WenKai Lite",
            "LXGW WenKai Screen",
            "Source Han Serif CN",
            "Source Han Sans CN",
            "Zhuque Fangsong",
            "Noto Serif SC",
            "Noto Sans CJK TC",
            "FZShuSong-Z01",
            "HYQiHei-55S",
            "SimSun",
            "PingFang SC",
            "MiSans",
            "Alibaba PuHuiTi 3.0",
            "KingHwa_OldSong",
            "霞鹜文楷",
            "方正书宋",
            "Sarasa Gothic SC",
        ] {
            assert_eq!(
                classify_font_name(cjk),
                FontScript::Cjk,
                "{cjk} should be classified as CJK"
            );
        }
        for latin in [
            "Literata",
            "Charis",
            "Gentium Book",
            "Bookerly",
            "Georgia",
            "Baskerville",
            "Century Gothic",
            "Franklin Gothic",
            "EB Garamond",
            "Merriweather",
            "Source Serif 4",
            "Noto Serif",
            "Inter",
            "Roboto",
        ] {
            assert_eq!(
                classify_font_name(latin),
                FontScript::Latin,
                "{latin} should be classified as Latin"
            );
        }
    }
}
