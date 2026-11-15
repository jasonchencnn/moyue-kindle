// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto chapter splitting and cached EPUB generation for plain text files.
//!
//! crengine-ng natively supports EPUB with full DOM fragmenting, accurate table
//! of contents, and multi-threaded partial rerendering. By converting TXT files
//! into cached uncompressed EPUB documents (following the same design as
//! `mobi.rs`), we achieve:
//! 1. Native handling of GBK and GB18030 (via `aristotle_library::encoding`).
//! 2. Automatic chapter splitting based on standard Chinese web novel heading patterns.
//! 3. TOC integration into reader's TOC page via standard EPUB `toc.ncx`.
//! 4. Fast opening times with on-disk caching (uncompressed ZIP stored entries).

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use plato_core::anyhow::{format_err, Error};
use regex::Regex;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

/// Current version of the TXT-to-EPUB converter. Bumping this invalidates cached EPUBs.
///
/// Version 2: chapter headings without a separator, full-width numerals,
/// English `Chapter` headings, blank pages for empty files, and the CSS that
/// starts every recognized chapter on a new page.
pub const CONVERTER_VERSION: u32 = 2;

/// The version stored in [`aristotle_library::Progress::source`] for a TXT
/// position, as `aristotle/txt<version>`.
///
/// The converter rewrites the book's document structure between versions, so
/// an XPointer only names the words it was saved against when the version
/// that generated it is known. Records written before this marker existed
/// have source `aristotle` and are converter 1, the only version shipped.
pub fn source_for(version: u32) -> String {
    format!("aristotle/txt{version}")
}

/// The converter version in a [`source_for`] string, if it is one.
pub fn version_from_source(source: Option<&str>) -> Option<u32> {
    source?.strip_prefix("aristotle/txt")?.parse().ok()
}

/// Regular expression matching common chapter headings:
/// - 第 x 章 / 回 / 卷 / 节 / 篇 / 话, with Arabic (including full-width),
///   Chinese numerals, or spaces around them; a title may follow with or
///   without a separator (`第一章新的开始`)
/// - 卷 x (e.g. 卷一, 卷三)
/// - 序章, 序言, 前言, 楔子, 后记, 尾声, 番外 (optionally numbered)
/// - English `Chapter` / `Part` / `Book` + numeral, and `Prologue`,
///   `Epilogue`, `Interlude`, `Afterword`, `Foreword`
pub const CHAPTER_PATTERN: &str = r"(?i)^[\s　]*((?:第[\s　]*[0-9０-９一二两三四五六七八九十百千万零〇廿卅]+[\s　]*[章回卷节篇话]|卷[\s　]*[0-9０-９一二两三四五六七八九十百千万零〇廿卅]+|序章|序言|前言|楔子|后记|尾声|番外(?:[\s　]*[0-9０-９一二两三四五六七八九十百千万零〇]+)?|(?:chapter|part|book)(?:[\s　]+(?:[0-9０-９]+|(?-i:[IVXLCDM]+)|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve))|prologue|epilogue|interlude|afterword|foreword))(?:[:：.\-—\s　]*(.*?))?[\s　]*$";

/// Maximum allowed length in characters for a chapter title line.
/// Headings longer than this are considered ordinary narrative sentences.
pub const MAX_CHAPTER_TITLE_LEN: usize = 40;

/// Represents a parsed chapter.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: Option<String>,
    pub paragraphs: Vec<String>,
}

/// Tests whether a line is a chapter heading. Returns the cleaned title if matched.
pub fn is_chapter_heading(line: &str, re: &Regex) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_CHAPTER_TITLE_LEN {
        return None;
    }
    if let Some(caps) = re.captures(trimmed) {
        // Group 1 is the chapter marker (`第一章`, `Chapter 1`, ...); group 2
        // is the optional title. A heading without a separator (`第一章新的
        // 开始`) is common in Chinese TXT files, so the separator is optional.
        // Without one, a title that starts with a grammatical particle is an
        // ordinary sentence (`第一章的内容`), not a heading.
        let marker = caps.get(1)?;
        let prefix = marker.as_str();
        let rest = &trimmed[marker.end()..];
        let separated = rest
            .chars()
            .next()
            .is_some_and(|c| matches!(c, ':' | '：' | '.' | '-' | '—' | ' ' | '\u{3000}' | '\t'));
        let particle = rest
            .chars()
            .next()
            .is_some_and(|c| "的是中里内所写讲说有在将把从对与和".contains(c));
        if !rest.is_empty() && !separated && (particle || prefix.is_ascii()) {
            return None;
        }
        let title = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let full = if title.trim().is_empty() {
            prefix.trim().to_string()
        } else {
            format!("{} {}", prefix.trim(), title.trim())
        };
        Some(full)
    } else {
        None
    }
}

/// The chapter pattern of converter 1, frozen.
///
/// A legacy XPointer was written against the EPUB that this pattern produced:
/// its `DocFragment` numbers are the old chapter numbers and its `p[M]` counts
/// the old paragraphs. Reproducing the old split from the same source text is
/// how a pointer is translated to the current split without guessing.
pub const LEGACY_CHAPTER_PATTERN: &str = r"^[\s　]*(?:(第[0-9一二两三四五六七八九十百千万零]+[章回卷节]|卷[0-9一二两三四五六七八九十百千万零]+|序章|序言|前言|楔子|后记|尾声|番外(?:[0-9一二两三四五六七八九十百千万零]+)?)(?:[:：\s　]+(.*?))?)[\s　]*$";

/// Why a stored TXT XPointer could not be translated exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// Not a pointer into a generated TXT EPUB.
    NotGenerated,
    /// The element path is not a direct `p`/`h2` of `body`.
    Unsupported,
    /// No such `DocFragment[N]`, or `N` is 0.
    Fragment,
    /// No such `p[M]`/`h2[M]` in that fragment.
    Element,
    /// The text offset is past the end of the paragraph it names.
    TextOffset,
}

impl MapError {
    /// A short explanation for the log and the notes.
    pub fn reason(self) -> &'static str {
        match self {
            MapError::NotGenerated => "not a generated TXT position",
            MapError::Unsupported => "unsupported element path",
            MapError::Fragment => "chapter (DocFragment) is gone",
            MapError::Element => "paragraph is gone",
            MapError::TextOffset => "text offset is out of range",
        }
    }
}

/// The paragraph layout of one converter version: where each spine file
/// (`DocFragment`) starts in the source text's non-empty lines.
#[derive(Debug)]
struct Layout {
    /// First item of every fragment, plus the item count.
    starts: Vec<usize>,
}

impl Layout {
    fn fragment_of(&self, item: usize) -> Option<usize> {
        let frag = self.starts.partition_point(|&s| s <= item).checked_sub(1)?;
        let start = *self.starts.get(frag)?;
        let end = *self.starts.get(frag + 1)?;
        (start <= item && item < end).then_some(frag)
    }
}

/// One source line classified by both converter versions.
///
/// `old` says converter 1 made it a chapter heading, `new` says converter 2
/// did. Every old heading is still a heading in version 2.
#[derive(Clone, Copy, Debug, Default)]
struct Item {
    old_heading: bool,
    new_heading: bool,
    /// Characters of the line, as the paragraph text of version 1.
    old_len: u32,
    /// Characters of the element text in version 2: the cleaned heading or
    /// the trimmed paragraph.
    new_len: u32,
}

/// A source TXT split by converter 1 and converter 2, for translating the
/// XPointers the old split left in progress and annotations.
#[derive(Debug)]
pub struct LegacyMap {
    items: Vec<Item>,
    old: Layout,
    new: Layout,
    heading_edits: HashMap<usize, (u32, u32, u32)>,
}

impl LegacyMap {
    fn from_text(text: &str) -> Result<LegacyMap, Error> {
        let old_re = Regex::new(LEGACY_CHAPTER_PATTERN)
            .map_err(|e| format_err!("invalid legacy regex: {e}"))?;
        let new_re =
            Regex::new(CHAPTER_PATTERN).map_err(|e| format_err!("invalid chapter regex: {e}"))?;
        let mut items = Vec::new();
        let mut heading_edits = HashMap::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let old_title = is_chapter_heading(trimmed, &old_re);
            let new_title = is_chapter_heading(trimmed, &new_re);
            let old_text = old_title.as_deref().unwrap_or(trimmed);
            let new_text = new_title.as_deref().unwrap_or(trimmed);
            if old_text != new_text {
                let before: Vec<char> = old_text.chars().collect();
                let after: Vec<char> = new_text.chars().collect();
                let prefix = before
                    .iter()
                    .zip(&after)
                    .take_while(|(a, b)| a == b)
                    .count();
                let suffix = before[prefix..]
                    .iter()
                    .rev()
                    .zip(after[prefix..].iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                heading_edits.insert(
                    items.len(),
                    (
                        prefix as u32,
                        (before.len() - suffix) as u32,
                        (after.len() - suffix) as u32,
                    ),
                );
            }
            items.push(Item {
                old_heading: old_title.is_some(),
                new_heading: new_title.is_some(),
                old_len: old_text.chars().count() as u32,
                new_len: new_text.chars().count() as u32,
            });
        }
        let old = build_layout(&items, |i| i.old_heading);
        let new = build_layout(&items, |i| i.new_heading);
        Ok(LegacyMap {
            items,
            old,
            new,
            heading_edits,
        })
    }

    /// The item a `p[M]`/`h2[M]` of `layout` names, zero-based.
    fn item_at(
        &self,
        old: bool,
        fragment: usize,
        heading: bool,
        index: usize,
    ) -> Result<usize, MapError> {
        let layout = if old { &self.old } else { &self.new };
        let (start, end) = (
            *layout.starts.get(fragment).ok_or(MapError::Fragment)?,
            *layout.starts.get(fragment + 1).ok_or(MapError::Fragment)?,
        );
        let mut seen = 0;
        for (i, item) in self.items.iter().enumerate().take(end).skip(start) {
            let is_heading = if old {
                item.old_heading
            } else {
                item.new_heading
            };
            if is_heading == heading {
                seen += 1;
                if seen == index {
                    return Ok(i);
                }
            }
        }
        Err(MapError::Element)
    }

    /// The element naming `item` in the current layout, one-based.
    fn new_address(&self, item: usize) -> Result<(usize, bool, usize), MapError> {
        let fragment = self.new.fragment_of(item).ok_or(MapError::Fragment)?;
        let start = self.new.starts[fragment];
        let new_heading = self.items[item].new_heading;
        let mut index = 0;
        for i in start..=item {
            let item = &self.items[i];
            if new_heading == item.new_heading {
                index += 1;
            }
        }
        Ok((fragment, new_heading, index))
    }

    /// Translate one XPointer into the current layout, keeping its text node
    /// and character offset.
    pub fn map_xpointer(&self, xpointer: &str) -> Result<String, MapError> {
        let parsed = parse_generated(xpointer)?;
        let fragment = parsed.fragment - 1;
        match parsed.element {
            None => {
                // A fragment start: name its first paragraph in the current
                // split, so a pointer at an old chunk boundary does not move
                // back to the start of a new chapter.
                let start = *self.old.starts.get(fragment).ok_or(MapError::Fragment)?;
                if start >= self.items.len() {
                    let new_fragment = self.new.fragment_of(start).unwrap_or(0);
                    return Ok(format!("/body/DocFragment[{}]/body", new_fragment + 1));
                }
                let (new_fragment, new_heading, new_index) = self.new_address(start)?;
                let name = if new_heading { "h2" } else { "p" };
                Ok(format!(
                    "/body/DocFragment[{}]/body/{name}[{new_index}]",
                    new_fragment + 1
                ))
            }
            Some((name, index)) => {
                let heading = name == "h2";
                let item = self.item_at(true, fragment, heading, index)?;
                let (new_fragment, new_heading, new_index) = self.new_address(item)?;
                let mut tail = parsed.tail.to_string();
                if let Some(offset) = text_offset(parsed.tail)? {
                    if offset > self.items[item].old_len {
                        return Err(MapError::TextOffset);
                    }
                    let mapped = match self.heading_edits.get(&item) {
                        Some(&(_, old_end, new_end)) if offset >= old_end => {
                            offset - old_end + new_end
                        }
                        Some(&(prefix, _, _)) if offset >= prefix => {
                            return Err(MapError::TextOffset);
                        }
                        _ => offset,
                    };
                    if mapped > self.items[item].new_len {
                        return Err(MapError::TextOffset);
                    }
                    let (node, _) = parsed.tail.rsplit_once('.').ok_or(MapError::Unsupported)?;
                    tail = format!("{node}.{mapped}");
                }
                let name = if new_heading { "h2" } else { "p" };
                Ok(format!(
                    "/body/DocFragment[{}]/body/{name}[{}]{}",
                    new_fragment + 1,
                    new_index,
                    tail
                ))
            }
        }
    }
}

impl LegacyMap {
    /// Translate a stored [`Position`], leaving non-XPointers unchanged.
    pub fn map_position(
        &self,
        position: &crate::book::Position,
    ) -> Result<crate::book::Position, MapError> {
        use crate::book::Position;
        match position {
            Position::XPointer { xpointer } => self
                .map_xpointer(xpointer)
                .map(|xpointer| Position::XPointer { xpointer }),
            other => Ok(other.clone()),
        }
    }
}

/// Split `items` the way a converter version does: at every heading of that
/// version, or, without headings, in 500-paragraph chunks.
fn build_layout(items: &[Item], is_heading: impl Fn(&Item) -> bool) -> Layout {
    let headings: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| is_heading(item))
        .map(|(i, _)| i)
        .collect();
    let mut starts = Vec::new();
    if headings.is_empty() {
        starts.push(0);
        let mut at = 500;
        while at < items.len() {
            starts.push(at);
            at += 500;
        }
        starts.push(items.len());
    } else {
        starts.push(0);
        for heading in headings {
            if starts.last() != Some(&heading) {
                starts.push(heading);
            }
        }
        if starts.last() != Some(&items.len()) {
            starts.push(items.len());
        }
    }
    if starts.len() < 2 {
        starts = vec![0, 0];
    }
    Layout { starts }
}

struct ParsedXPointer<'a> {
    /// One-based `DocFragment[N]`.
    fragment: usize,
    /// `p[M]` or `h2[M]`, one-based.
    element: Option<(&'a str, usize)>,
    /// The part after the element (`/text().0`), kept verbatim.
    tail: &'a str,
}

fn parse_generated(xpointer: &str) -> Result<ParsedXPointer<'_>, MapError> {
    let rest = xpointer
        .strip_prefix("/body/DocFragment")
        .ok_or(MapError::NotGenerated)?;
    let (fragment, rest) = if let Some(number) = rest.strip_prefix('[') {
        let (number, rest) = number.split_once(']').ok_or(MapError::Fragment)?;
        let fragment = number
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or(MapError::Fragment)?;
        (fragment, rest)
    } else {
        (1, rest)
    };
    let rest = if rest.is_empty() {
        rest
    } else {
        rest.strip_prefix("/body").ok_or(MapError::NotGenerated)?
    };
    if rest.is_empty() {
        return Ok(ParsedXPointer {
            fragment,
            element: None,
            tail: "",
        });
    }
    let rest = rest.strip_prefix('/').ok_or(MapError::NotGenerated)?;
    let (element, tail) = rest
        .split_once('/')
        .map_or((rest, ""), |(name, _)| (name, &rest[name.len()..]));
    let (name, index) = if let Some((name, number)) = element.split_once('[') {
        let number = number.strip_suffix(']').ok_or(MapError::Unsupported)?;
        let index = number
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or(MapError::Element)?;
        (name, index)
    } else {
        (element, 1)
    };
    if !matches!(name, "p" | "h2") || (!tail.is_empty() && !tail.starts_with("/text()")) {
        return Err(MapError::Unsupported);
    }
    Ok(ParsedXPointer {
        fragment,
        element: Some((name, index)),
        tail,
    })
}

/// The character offset of a `/text().K` or `/text()[T].K` tail, if any.
fn text_offset(tail: &str) -> Result<Option<u32>, MapError> {
    let Some(rest) = tail.strip_prefix("/text()") else {
        return Ok(None);
    };
    let rest = if let Some(rest) = rest.strip_prefix('[') {
        let close = rest.find(']').ok_or(MapError::Unsupported)?;
        if &rest[..close] != "1" {
            return Err(MapError::Unsupported);
        }
        rest[close + 1..]
            .strip_prefix('.')
            .ok_or(MapError::Unsupported)?
    } else if let Some(rest) = rest.strip_prefix('.') {
        rest
    } else {
        return Ok(None);
    };
    Ok(Some(rest.parse().map_err(|_| MapError::TextOffset)?))
}

/// Maximum number of source layouts kept: enough for the open book, its
/// prewarm and a neighbour without re-reading the text.
const MAP_CACHE: usize = 4;

/// One cached source split: the file's identity and the layout read from it.
struct CachedMap {
    path: PathBuf,
    len: u64,
    mtime: u128,
    map: Arc<LegacyMap>,
}

/// The legacy split of `src`, cached by path, size and modification time.
///
/// Reading and splitting the source is the expensive part; callers on the
/// open worker share the bounded cache, so opening the same book twice does
/// not read it twice.
pub fn legacy_map(src: &Path) -> Result<Arc<LegacyMap>, Error> {
    static CACHE: Mutex<Vec<CachedMap>> = Mutex::new(Vec::new());
    let meta = fs::metadata(src)
        .map_err(|e| format_err!("can't read metadata for {}: {e}", src.display()))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    if let Ok(cache) = CACHE.lock() {
        if let Some(entry) = cache
            .iter()
            .find(|e| e.path == src && e.len == meta.len() && e.mtime == modified)
        {
            return Ok(entry.map.clone());
        }
    }
    let bytes = fs::read(src).map_err(|e| format_err!("can't read {}: {e}", src.display()))?;
    let text = aristotle_library::encoding::decode_guess(&bytes);
    let map = Arc::new(LegacyMap::from_text(&text)?);
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|e| e.path != src);
        cache.insert(
            0,
            CachedMap {
                path: src.to_path_buf(),
                len: meta.len(),
                mtime: modified,
                map: map.clone(),
            },
        );
        cache.truncate(MAP_CACHE);
    }
    Ok(map)
}

/// Translate a converter-1 XPointer into the current converter's document.
///
/// The old and new splits are both recomputed from the original TXT, so the
/// pointer keeps its paragraph and character offset (`/text().K`) exactly.
pub fn map_legacy_xpointer(src: &Path, xpointer: &str) -> Result<String, MapError> {
    legacy_map(src)
        .map_err(|_| MapError::NotGenerated)?
        .map_xpointer(xpointer)
}

/// Translate a converter-1 [`Position`], leaving other kinds alone.
pub fn map_legacy_position(
    src: &Path,
    position: &crate::book::Position,
) -> Result<crate::book::Position, MapError> {
    use crate::book::Position;
    match position {
        Position::XPointer { xpointer } => {
            map_legacy_xpointer(src, xpointer).map(|xpointer| Position::XPointer { xpointer })
        }
        other => Ok(other.clone()),
    }
}

/// Escapes XML special characters and strips invalid XML 1.0 control codes.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // In XML 1.0, only \t (0x9), \n (0xA), \r (0xD) and >= 0x20 are valid
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Converts a TXT file to a cached EPUB file in `cache_dir`.
/// If a valid cached version already exists, returns its path immediately.
pub fn cached_epub(src: &Path, cache_dir: &Path) -> Result<PathBuf, Error> {
    cached_epub_with_progress(src, cache_dir, None)
}

/// [`cached_epub`], reporting conversion progress (0..=30) to `progress`.
///
/// The conversion reads and decodes the whole file and writes an
/// uncompressed EPUB, which on the device takes long enough to be worth a
/// progress line. It owns the first 30% of the open; crengine reports its
/// layout on the same counter afterwards, and the status bar ignores
/// decreases. The callback is only called on a cache miss.
pub fn cached_epub_with_progress(
    src: &Path,
    cache_dir: &Path,
    progress: Option<&AtomicU8>,
) -> Result<PathBuf, Error> {
    let meta = fs::metadata(src)
        .map_err(|e| format_err!("can't read metadata for {}: {e}", src.display()))?;
    let mut h = DefaultHasher::new();
    CONVERTER_VERSION.hash(&mut h);
    src.hash(&mut h);
    meta.len().hash(&mut h);
    // Nanoseconds, not seconds: a book replaced within the same second at
    // the same size must still miss the cache instead of showing the old
    // chapters and layout.
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .hash(&mut h);

    let stem: String = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .take(40)
        .collect();
    let stem_clean = stem.trim();
    let stem_name = if stem_clean.is_empty() {
        "book"
    } else {
        stem_clean
    };
    let out = cache_dir.join(format!("{}-{:016x}.epub", stem_name, h.finish()));
    if out.exists() {
        return Ok(out);
    }

    fs::create_dir_all(cache_dir)
        .map_err(|e| format_err!("can't create cache dir {}: {e}", cache_dir.display()))?;

    static CONVERSIONS: AtomicUsize = AtomicUsize::new(0);
    let n = CONVERSIONS.fetch_add(1, Ordering::Relaxed);
    let tmp = out.with_extension(format!("epub.{}-{n}.part", std::process::id()));

    if let Err(e) = convert_txt_to_epub_with_progress(src, &tmp, progress) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &out) {
        // Another open or prewarm may have won the race between the cache
        // check and the rename. The key covers the source bytes and the
        // converter version, so the file that appeared is the same book:
        // use it instead of falling back to MuPDF (FAT cannot overwrite on
        // rename).
        if out.exists() {
            let _ = fs::remove_file(&tmp);
            return Ok(out);
        }
        let _ = fs::remove_file(&tmp);
        return Err(format_err!("failed to rename temporary EPUB: {e}"));
    }
    report(progress, 30);
    Ok(out)
}

/// Converts `src` TXT file into uncompressed EPUB at `dst`.
pub fn convert_txt_to_epub(src: &Path, dst: &Path) -> Result<(), Error> {
    convert_txt_to_epub_with_progress(src, dst, None)
}

/// [`convert_txt_to_epub`], reporting progress (0..=30) to `progress`.
pub fn convert_txt_to_epub_with_progress(
    src: &Path,
    dst: &Path,
    progress: Option<&AtomicU8>,
) -> Result<(), Error> {
    report(progress, 5);
    let bytes = fs::read(src).map_err(|e| format_err!("can't read {}: {e}", src.display()))?;
    let text = aristotle_library::encoding::decode_guess(&bytes);
    report(progress, 10);

    // Extract title and author heuristics
    let extracted = aristotle_library::txt::extract(src).unwrap_or_default();
    let title = if !extracted.meta.title.is_empty() {
        extracted.meta.title
    } else {
        src.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".to_string())
    };
    let author = extracted
        .meta
        .authors
        .first()
        .map(|a| a.name.clone())
        .unwrap_or_default();

    let re = Regex::new(CHAPTER_PATTERN).map_err(|e| format_err!("invalid chapter regex: {e}"))?;

    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_paras: Vec<String> = Vec::new();
    let mut found_any_chapter = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(ch_title) = is_chapter_heading(trimmed, &re) {
            found_any_chapter = true;
            if current_title.is_some() || !current_paras.is_empty() {
                chapters.push(Chapter {
                    title: current_title.take(),
                    paragraphs: std::mem::take(&mut current_paras),
                });
            }
            current_title = Some(ch_title);
        } else {
            current_paras.push(trimmed.to_string());
        }
    }
    if current_title.is_some() || !current_paras.is_empty() {
        chapters.push(Chapter {
            title: current_title,
            paragraphs: current_paras,
        });
    }

    // If no chapters were found, chunk into groups of 500 paragraphs
    // so the EPUB spine has manageable XHTML files without creating false TOC entries.
    let chapters = if found_any_chapter {
        chapters
    } else {
        let all_paras = if chapters.is_empty() {
            Vec::new()
        } else {
            chapters.into_iter().flat_map(|c| c.paragraphs).collect()
        };
        if all_paras.is_empty() {
            vec![Chapter {
                title: None,
                paragraphs: Vec::new(),
            }]
        } else {
            all_paras
                .chunks(500)
                .map(|chunk| Chapter {
                    title: None,
                    paragraphs: chunk.to_vec(),
                })
                .collect()
        }
    };

    report(progress, 15);
    let file = File::create(dst).map_err(|e| format_err!("can't create {}: {e}", dst.display()))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    // 1. mimetype (must be first and uncompressed)
    zip.start_file("mimetype", opts)
        .map_err(|e| format_err!("{e}"))?;
    zip.write_all(b"application/epub+zip")
        .map_err(|e| format_err!("{e}"))?;

    // 2. META-INF/container.xml
    zip.start_file("META-INF/container.xml", opts)
        .map_err(|e| format_err!("{e}"))?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>
"#,
    )
    .map_err(|e| format_err!("{e}"))?;

    // 3. Chapter XHTML files
    let mut manifest_items = String::new();
    let mut spine_items = String::new();
    let mut nav_points = String::new();
    let mut play_order = 1;

    for (idx, ch) in chapters.iter().enumerate() {
        let file_name = format!("chapter_{:04}.xhtml", idx);
        zip.start_file(&file_name, opts)
            .map_err(|e| format_err!("{e}"))?;

        manifest_items.push_str(&format!(
            r#"    <item id="c{idx}" href="{file_name}" media-type="application/xhtml+xml"/>
"#
        ));
        spine_items.push_str(&format!(
            r#"    <itemref idref="c{idx}"/>
"#
        ));

        if let Some(t) = &ch.title {
            nav_points.push_str(&format!(
                r#"    <navPoint id="np_{idx}" playOrder="{play_order}">
      <navLabel><text>{}</text></navLabel>
      <content src="{file_name}"/>
    </navPoint>
"#,
                xml_escape(t)
            ));
            play_order += 1;
        }

        let mut content = String::with_capacity(ch.paragraphs.len() * 128 + 256);
        content.push_str(
            r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="zh">
<head>
  <title>"#,
        );
        content.push_str(&xml_escape(ch.title.as_deref().unwrap_or(&title)));
        content.push_str("</title>\n</head>\n<body>\n");

        if let Some(t) = &ch.title {
            content.push_str("  <h2>");
            content.push_str(&xml_escape(t));
            content.push_str("</h2>\n");
        }

        if ch.title.is_none() && ch.paragraphs.is_empty() {
            // An empty body makes crengine fail with "document has no
            // pages". A non-breaking space gives the reader a blank page to
            // show, the way an empty text file should open.
            content.push_str("  <p>&#160;</p>\n");
        }

        for p in &ch.paragraphs {
            content.push_str("  <p>");
            content.push_str(&xml_escape(p));
            content.push_str("</p>\n");
        }

        content.push_str("</body>\n</html>\n");
        zip.write_all(content.as_bytes())
            .map_err(|e| format_err!("{e}"))?;
        if !chapters.is_empty() {
            report(progress, 15 + (15 * (idx + 1) / chapters.len()) as u8);
        }
    }

    // 4. toc.ncx
    zip.start_file("toc.ncx", opts)
        .map_err(|e| format_err!("{e}"))?;
    let ncx = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
  <head>
    <meta name="dtb:uid" content="urn:uuid:txt-auto-convert"/>
    <meta name="dtb:depth" content="1"/>
    <meta name="dtb:totalPageCount" content="0"/>
    <meta name="dtb:maxPageNumber" content="0"/>
  </head>
  <docTitle><text>{}</text></docTitle>
  <navMap>
{nav_points}  </navMap>
</ncx>
"#,
        xml_escape(&title)
    );
    zip.write_all(ncx.as_bytes())
        .map_err(|e| format_err!("{e}"))?;

    // 5. content.opf
    zip.start_file("content.opf", opts)
        .map_err(|e| format_err!("{e}"))?;
    let creator_elem = if !author.is_empty() {
        format!("    <dc:creator>{}</dc:creator>\n", xml_escape(&author))
    } else {
        String::new()
    };
    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:txt-auto-convert</dc:identifier>
    <dc:title>{}</dc:title>
    <dc:language>zh</dc:language>
{creator_elem}  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
{manifest_items}  </manifest>
  <spine toc="ncx">
{spine_items}  </spine>
</package>
"#,
        xml_escape(&title)
    );
    zip.write_all(opf.as_bytes())
        .map_err(|e| format_err!("{e}"))?;

    zip.finish().map_err(|e| format_err!("{e}"))?;
    report(progress, 30);
    Ok(())
}

/// Stores `value` in `progress` when the caller asked for progress.
fn report(progress: Option<&AtomicU8>, value: u8) {
    if let Some(progress) = progress {
        progress.store(value, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plato_core::document::Document;

    #[test]
    fn chapter_regex_matches_various_formats() {
        let re = Regex::new(CHAPTER_PATTERN).unwrap();

        assert_eq!(
            is_chapter_heading("第一章 序幕", &re),
            Some("第一章 序幕".to_string())
        );
        assert_eq!(
            is_chapter_heading("第10章 决战之巅", &re),
            Some("第10章 决战之巅".to_string())
        );
        assert_eq!(
            is_chapter_heading("第十二回 甄士隐梦幻识通灵", &re),
            Some("第十二回 甄士隐梦幻识通灵".to_string())
        );
        assert_eq!(
            is_chapter_heading("卷三 风云再起", &re),
            Some("卷三 风云再起".to_string())
        );
        assert_eq!(
            is_chapter_heading("第4节 讨论与结论", &re),
            Some("第4节 讨论与结论".to_string())
        );
        assert_eq!(is_chapter_heading("序章", &re), Some("序章".to_string()));
        assert_eq!(is_chapter_heading("楔子", &re), Some("楔子".to_string()));
        assert_eq!(is_chapter_heading("后记", &re), Some("后记".to_string()));
        assert_eq!(is_chapter_heading("尾声", &re), Some("尾声".to_string()));
        assert_eq!(
            is_chapter_heading("番外 那些年", &re),
            Some("番外 那些年".to_string())
        );
        assert_eq!(
            is_chapter_heading("番外2 往事如烟", &re),
            Some("番外2 往事如烟".to_string())
        );
        assert_eq!(
            is_chapter_heading("　　第一章：新的开始　", &re),
            Some("第一章 新的开始".to_string())
        );
        // Common Chinese headings with no separator between marker and title.
        assert_eq!(
            is_chapter_heading("第一章新的开始", &re),
            Some("第一章 新的开始".to_string())
        );
        assert_eq!(
            is_chapter_heading("序章开始", &re),
            Some("序章 开始".to_string())
        );
        // Full-width numerals and spaces around the numeral.
        assert_eq!(
            is_chapter_heading("第１２章 风起云涌", &re),
            Some("第１２章 风起云涌".to_string())
        );
        assert_eq!(
            is_chapter_heading("第 3 章 归来", &re),
            Some("第 3 章 归来".to_string())
        );
        // Common English chapter headings, Arabic and Roman numerals.
        assert_eq!(
            is_chapter_heading("Chapter 5 The Return", &re),
            Some("Chapter 5 The Return".to_string())
        );
        assert_eq!(
            is_chapter_heading("CHAPTER XII: Dawn", &re),
            Some("CHAPTER XII Dawn".to_string())
        );
        assert_eq!(
            is_chapter_heading("Chapter One", &re),
            Some("Chapter One".to_string())
        );
        // `第一章的内容` is a sentence about chapter one, not a heading.
        assert_eq!(is_chapter_heading("第一章的内容", &re), None);
        assert_eq!(is_chapter_heading("第1章的内容", &re), None);
        for line in [
            "Books are on the shelf",
            "Part of the story",
            "Chapter summary",
            "Chapter did not say why.",
            "Book Inevitable",
        ] {
            assert_eq!(is_chapter_heading(line, &re), None, "{line}");
        }
    }

    #[test]
    fn ignores_overlong_and_empty_lines() {
        let re = Regex::new(CHAPTER_PATTERN).unwrap();

        assert_eq!(is_chapter_heading("", &re), None);
        assert_eq!(is_chapter_heading("   \t   ", &re), None);

        let overlong = format!("第一章 {}", "这是一段非常非常长的文字".repeat(10));
        assert!(overlong.chars().count() > MAX_CHAPTER_TITLE_LEN);
        assert_eq!(is_chapter_heading(&overlong, &re), None);

        // Ordinary sentence that happens to contain "第一章"
        assert_eq!(
            is_chapter_heading("他在第一章写道：人生若只如初见。", &re),
            None
        );
    }

    #[test]
    fn xml_escape_handles_entities_and_controls() {
        assert_eq!(
            xml_escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
        assert_eq!(
            xml_escape("valid\t\n\rtext\x00\x08null"),
            "valid\t\n\rtextnull"
        );
    }

    #[test]
    fn converts_and_caches_epub() {
        let temp = std::env::temp_dir().join("txt_test_convert_epub");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("test_book.txt");
        let content = "《测试小说》\n作者：测试者\n\n第一章 起步\n这是第一章的正文。\n\n第二章 发展\n这是第二章的内容。";
        fs::write(&src, content.as_bytes()).unwrap();

        let cache = temp.join("cache");
        let epub = cached_epub(&src, &cache).unwrap();
        assert!(epub.exists());

        // Read the generated EPUB using plato_core::document::epub::EpubDocument
        let doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        assert_eq!(doc.title().as_deref(), Some("测试小说"));
        assert_eq!(doc.author().as_deref(), Some("测试者"));

        // Second call should hit cache
        let epub_cached = cached_epub(&src, &cache).unwrap();
        assert_eq!(epub, epub_cached);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn plain_text_without_chapters_has_empty_nav_points() {
        let temp = std::env::temp_dir().join("txt_test_no_chapters_epub");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("plain.txt");
        let content = "这是纯文本短篇。\n第二段落没有章节标记。\n第三段落。";
        fs::write(&src, content.as_bytes()).unwrap();

        let cache = temp.join("cache");
        let epub = cached_epub(&src, &cache).unwrap();
        let mut doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        let toc = doc.toc().unwrap_or_default();
        assert!(toc.is_empty(), "TOC should be empty when no chapters exist");

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn crlf_gbk_chapters_are_recognized() {
        let temp = std::env::temp_dir().join("txt_test_crlf_gbk");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("gbk.txt");
        let text = "《测试》\r\n作者：某人\r\n\r\n第一章新的开始\r\n第一段。\r\n第二章：风起\r\n第二段。\r\n第１２章 云涌\r\n第三段。\r\nChapter 3: Dawn\r\n第四段。\r\n";
        let (encoded, _, _) = encoding_rs::GBK.encode(text);
        fs::write(&src, encoded).unwrap();

        let epub = cached_epub(&src, &temp.join("cache")).unwrap();
        let mut doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        let toc = doc.toc().unwrap_or_default();
        let titles: Vec<&str> = toc.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "第一章 新的开始",
                "第二章 风起",
                "第１２章 云涌",
                "Chapter 3 Dawn"
            ]
        );
    }

    #[test]
    fn empty_text_becomes_a_blank_page() {
        let temp = std::env::temp_dir().join("txt_test_empty");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("empty.txt");
        fs::write(&src, b"").unwrap();
        let epub = cached_epub(&src, &temp.join("cache")).unwrap();

        let file = fs::File::open(&epub).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut body = String::new();
        {
            use std::io::Read;
            zip.by_name("chapter_0000.xhtml")
                .unwrap()
                .read_to_string(&mut body)
                .unwrap();
        }
        assert!(
            body.contains("&#160;"),
            "an empty TXT must still produce a page: {body}"
        );
    }

    #[test]
    fn long_nonchapter_text_is_chunked_without_a_toc() {
        let temp = std::env::temp_dir().join("txt_test_long_plain");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("long.txt");
        let body = "这一行不是章节标题，只是一段普通的连续文本。\n";
        fs::write(&src, body.repeat(1200)).unwrap();
        let epub = cached_epub(&src, &temp.join("cache")).unwrap();

        let file = fs::File::open(&epub).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let chapters = (0..zip.len())
            .filter(|i| {
                zip.by_index(*i)
                    .map(|f| f.name().map(|n| n.starts_with("chapter_")).unwrap_or(false))
                    .unwrap_or(false)
            })
            .count();
        assert!(
            chapters > 1,
            "long non-chapter text must be split into several spine files"
        );

        let mut doc = plato_core::document::epub::EpubDocument::new(&epub).unwrap();
        assert!(doc.toc().unwrap_or_default().is_empty());
    }

    #[test]
    fn a_replaced_source_invalidates_the_cached_epub() {
        let temp = std::env::temp_dir().join("txt_test_cache_invalidation");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("book.txt");
        fs::write(&src, b"first version").unwrap();
        let cache = temp.join("cache");
        let first = cached_epub(&src, &cache).unwrap();
        assert!(first.exists());

        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&src, b"other version").unwrap();
        let second = cached_epub(&src, &cache).unwrap();
        assert_ne!(first, second, "a changed source must not reuse the cache");
        assert!(second.exists());
    }

    #[test]
    fn conversion_reports_progress() {
        let temp = std::env::temp_dir().join("txt_test_progress");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();

        let src = temp.join("book.txt");
        fs::write(&src, b"first line\nsecond line\n").unwrap();
        let progress = AtomicU8::new(0);
        let _ = cached_epub_with_progress(&src, &temp.join("cache"), Some(&progress)).unwrap();
        // Conversion owns the first 30%; the engine reports the rest of the
        // open. The status bar ignores decreases, so this stays monotonic.
        assert_eq!(progress.load(Ordering::Relaxed), 30);
    }
    fn fixture(name: &str, text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("book.txt");
        fs::write(&src, text.as_bytes()).unwrap();
        (dir, src)
    }

    #[test]
    fn legacy_canonical_singletons_and_heading_offsets_are_preserved() {
        let map = LegacyMap::from_text("第一章新的开始\n唯一段落").unwrap();
        assert_eq!(
            map.map_xpointer("/body/DocFragment/body/p[1]/text().3")
                .unwrap(),
            "/body/DocFragment[1]/body/h2[1]/text().4"
        );
        assert_eq!(
            map.map_xpointer("/body/DocFragment/body/p[2]/text()[1].1")
                .unwrap(),
            "/body/DocFragment[1]/body/p[1]/text()[1].1"
        );
        let map = LegacyMap::from_text("第一章 开始\n唯一段落").unwrap();
        assert_eq!(
            map.map_xpointer("/body/DocFragment/body/h2/text().4")
                .unwrap(),
            "/body/DocFragment[1]/body/h2[1]/text().4"
        );
        assert_eq!(
            map.map_xpointer("/body/DocFragment/body/p/text().1")
                .unwrap(),
            "/body/DocFragment[1]/body/p[1]/text().1"
        );
    }

    #[test]
    fn legacy_pointer_follows_inserted_chapters_and_a_heading() {
        let text =
            "第一章 开始\n一段。\n二段。\nChapter 5 New\n三段。\n第二章 继续\n四段。\n五段。";
        let (dir, src) = fixture("txt_migrate_inserted", text);
        // Version 1: one chapter, so `Chapter 5 New` was paragraph 3.
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[1]/body/p[3]/text().0").unwrap(),
            "/body/DocFragment[2]/body/h2[1]/text().0"
        );
        // A paragraph before the inserted chapter keeps its text offset.
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[1]/body/p[1]/text().2").unwrap(),
            "/body/DocFragment[1]/body/p[1]/text().2"
        );
        // Version 1's second chapter becomes the third fragment.
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[2]/body/p[2]/text().0").unwrap(),
            "/body/DocFragment[3]/body/p[2]/text().0"
        );
        // The old fragment start names the first paragraph of the new split.
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[2]").unwrap(),
            "/body/DocFragment[3]/body/h2[1]"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_pointer_from_the_500_paragraph_fallback() {
        let mut lines: Vec<String> = (0..700).map(|i| format!("段落{i}")).collect();
        lines[299] = "Chapter 5 插入".to_string();
        let text = lines.join("\n") + "\n";
        let (dir, src) = fixture("txt_migrate_chunks", &text);
        // Version 1 had no headings: fragment 2 starts at paragraph 501.
        // Saved at paragraph 10 of it (the 510th), before the new heading.
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[2]/body/p[10]/text().3").unwrap(),
            "/body/DocFragment[2]/body/p[210]/text().3"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unmappable_legacy_pointers_are_errors() {
        let (dir, src) = fixture("txt_migrate_bad", "第一章 开始\n一段。\n二段。");
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[9]/body/p[1]"),
            Err(MapError::Fragment)
        );
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[1]/body/p[9]"),
            Err(MapError::Element)
        );
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[1]/body/p[1]/text().999"),
            Err(MapError::TextOffset)
        );
        assert_eq!(
            map_legacy_xpointer(&src, "/body/DocFragment[1]/body/div[1]/p[1]"),
            Err(MapError::Unsupported)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn txt_sources_round_trip_the_converter_version() {
        assert_eq!(source_for(2), "aristotle/txt2");
        assert_eq!(version_from_source(Some("aristotle/txt2")), Some(2));
        assert_eq!(version_from_source(Some("aristotle")), None);
        assert_eq!(version_from_source(None), None);
    }
}
