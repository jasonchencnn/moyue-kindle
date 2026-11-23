//! WeRead coordinates ↔ rebuilt EPUB coordinates (contract §14.6).
//!
//! WeRead records a position as `(chapterUid, chapterOffset)`; the rebuilt
//! EPUB lays the same chapters out as spine items. [`PositionMap`] is the
//! chapter table the download pipeline keeps, and [`AnchorMap`] turns a
//! character offset inside a chapter into a crengine XPointer and back.
//!
//! Both directions are pure: a map built once from a download never reads the
//! network or a clock. The only I/O is [`AnchorMap::from_epub`], which reads
//! the EPUB the download just wrote so the anchor paths are taken from the
//! bytes crengine will actually see.
//!
//! The character offsets are Unicode scalar values, the unit crengine uses
//! for `text().N` XPointer offsets. Whitespace-only text nodes between
//! elements do not advance the chapter offset: they are formatting, not
//! text. A chapter longer than [`MAX_ANCHORLESS_CHAPTER_CHARS`] needs an
//! anchor map before the reader is moved inside it (the MiuRead #114 rescue
//! rule); its start can always be named.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::weread::epub::SpineEntry;
use crate::weread::model::ChapterInfo;

/// Longest chapter whose in-chapter offset may be named without verified
/// anchors. Above this, jumping to the chapter start is the safe fallback.
pub const MAX_ANCHORLESS_CHAPTER_CHARS: i64 = 200_000;

/// A chapter that survived filtering and made it into the rebuilt EPUB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterMapping {
    /// WeRead `chapterUid`.
    pub chapter_uid: i64,
    /// WeRead `chapterIdx`; kept for the reverse conversion and diagnostics.
    pub chapter_idx: i64,
    /// Zero-based position in the rebuilt EPUB spine.
    pub spine_index: u32,
    /// Chapter document relative to the OPF (`Text/...`).
    pub href: String,
    /// TOC `wordCount`, when the server sent one. It is only an estimate of
    /// the chapter's length, so it never clamps an offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_count: Option<i64>,
    /// Visible characters in the rebuilt chapter, filled by [`AnchorMap`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_len: Option<i64>,
}

/// `chapterUid`/`chapterIdx` ↔ rebuilt-EPUB spine item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionMap {
    /// WeRead book id.
    pub book_id: String,
    /// `bookVersion` used for the chapter downloads.
    pub book_version: String,
    /// Chapters in spine order.
    pub chapters: Vec<ChapterMapping>,
}

/// A position inside the rebuilt EPUB: spine item and chapter character
/// offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpubPosition {
    /// Zero-based spine index.
    pub spine_index: u32,
    /// Unicode scalar offset inside the chapter.
    pub char_offset: i64,
}

/// A WeRead cloud coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeReadCoordinate {
    /// `chapterUid`.
    pub chapter_uid: i64,
    /// `chapterIdx`; used only when the uid is absent from the map.
    pub chapter_idx: i64,
    /// `chapterOffset`.
    pub chapter_offset: i64,
}

/// How exactly a coordinate was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    /// Within a known chapter length and anchored to a text node.
    Exact,
    /// The offset was past the chapter end and was clamped to it.
    Clamped,
    /// Only the chapter start is named; the in-chapter offset is unknown.
    ChapterStart,
    /// The EPUB did not provide a chapter length, so the offset is trusted.
    Unverified,
}

/// A position plus the precision it was obtained with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedPosition {
    /// The mapped position.
    pub position: EpubPosition,
    /// How exact it is.
    pub precision: Precision,
}

/// A WeRead coordinate plus the precision it was obtained with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedCoordinate {
    /// The mapped coordinate.
    pub coordinate: WeReadCoordinate,
    /// How exact it is.
    pub precision: Precision,
}

/// An XPointer and the precision of the page jump it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XPointerTarget {
    /// A crengine XPointer, absolute from the document root.
    pub xpointer: String,
    /// How exact the target is.
    pub precision: Precision,
}

/// One text run of a chapter and where it starts in the chapter's character
/// space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextAnchor {
    /// Chapter character offset of the run's first character.
    pub char_start: i64,
    /// Number of Unicode scalar values in the run.
    pub char_len: i64,
    /// XPointer of the text node, without the trailing `.offset`.
    pub path: String,
}

/// All text anchors of one chapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterAnchors {
    /// WeRead `chapterUid`.
    pub chapter_uid: i64,
    /// Zero-based spine index.
    pub spine_index: u32,
    /// Visible characters in the chapter.
    pub total_chars: i64,
    /// Text runs in document order.
    pub anchors: Vec<TextAnchor>,
}

/// Text anchors for the chapters of one book.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorMap {
    /// WeRead book id.
    pub book_id: String,
    /// Chapters with verified anchors.
    pub chapters: Vec<ChapterAnchors>,
}

/// The persisted pair of a book's coordinate map and its anchors.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookPositions {
    /// WeRead `chapterUid` ↔ EPUB spine index.
    pub map: PositionMap,
    /// `chapterUid` → text anchors.
    pub anchors: AnchorMap,
}

/// Failures of the coordinate conversion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PositionError {
    /// `chapterUid` is not in the downloaded spine.
    #[error("chapter {0} is not in the downloaded spine")]
    UnknownChapter(i64),
    /// The spine index is not in the downloaded map.
    #[error("spine item {0} is not in the downloaded spine")]
    UnknownSpine(u32),
    /// A negative offset cannot name a position.
    #[error("chapter {chapter_uid} has an invalid offset {offset}")]
    InvalidOffset {
        /// The chapter the offset belongs to.
        chapter_uid: i64,
        /// The rejected offset.
        offset: i64,
    },
    /// A long chapter needs anchors before an in-chapter jump is trusted.
    #[error(
        "chapter {chapter_uid} is {chars} characters long; its anchor must be verified before jumping"
    )]
    LongChapterNeedsAnchor {
        /// The chapter being jumped into.
        chapter_uid: i64,
        /// Its known or estimated length.
        chars: i64,
    },
    /// The XPointer is not a `DocFragment` pointer this map can place.
    #[error("invalid XPointer: {0}")]
    InvalidXPointer(String),
    /// A chapter document is not well-formed XHTML.
    #[error("chapter XHTML is invalid: {0}")]
    Xhtml(String),
    /// The rebuilt EPUB could not be read.
    #[error("EPUB is invalid: {0}")]
    Epub(String),
}

impl PositionMap {
    /// Build the chapter table from a completed download.
    pub fn from_spine(
        book_id: impl Into<String>,
        book_version: impl Into<String>,
        spine: &[SpineEntry],
        toc: &[ChapterInfo],
    ) -> Self {
        let mut chapters: Vec<ChapterMapping> = spine
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let word_count = toc
                    .iter()
                    .find(|chapter| chapter.chapter_uid == Some(entry.chapter_uid))
                    .and_then(|chapter| chapter.word_count)
                    .filter(|count| *count >= 0);
                ChapterMapping {
                    chapter_uid: entry.chapter_uid,
                    chapter_idx: entry.chapter_idx,
                    spine_index: index as u32,
                    href: entry.href.clone(),
                    word_count,
                    char_len: None,
                }
            })
            .collect();
        // When multiple TOC chapters share one deduplicated spine XHTML file,
        // retain alias mappings for secondary `chapterUid`s pointing to the
        // same `spine_index` so progress and notes on them resolve cleanly.
        let mut seen_uids: std::collections::BTreeSet<i64> =
            chapters.iter().map(|c| c.chapter_uid).collect();
        for toc_chapter in toc {
            let Some(uid) = toc_chapter.chapter_uid.filter(|&u| u != 0) else {
                continue;
            };
            if seen_uids.contains(&uid) {
                continue;
            }
            let matched = toc_chapter.files.iter().find_map(|raw| {
                let norm = crate::weread::epub::normalize_archive_path(raw).ok()?;
                let under_text = if norm.starts_with("Text/") {
                    norm.clone()
                } else {
                    format!("Text/{norm}")
                };
                spine
                    .iter()
                    .enumerate()
                    .find(|(_, entry)| entry.href == norm || entry.href == under_text)
            });
            if let Some((index, entry)) = matched {
                seen_uids.insert(uid);
                chapters.push(ChapterMapping {
                    chapter_uid: uid,
                    chapter_idx: toc_chapter.chapter_idx.unwrap_or(entry.chapter_idx),
                    spine_index: index as u32,
                    href: entry.href.clone(),
                    word_count: toc_chapter.word_count.filter(|count| *count >= 0),
                    char_len: None,
                });
            }
        }
        chapters.sort_by_key(|chapter| (chapter.spine_index, chapter.chapter_idx));
        Self {
            book_id: book_id.into(),
            book_version: book_version.into(),
            chapters,
        }
    }

    /// The chapter row for a WeRead uid.
    pub fn chapter_by_uid(&self, chapter_uid: i64) -> Option<&ChapterMapping> {
        self.chapters
            .iter()
            .find(|chapter| chapter.chapter_uid == chapter_uid)
    }

    /// The chapter row for a spine index.
    pub fn chapter_by_spine(&self, spine_index: u32) -> Option<&ChapterMapping> {
        self.chapters
            .iter()
            .find(|chapter| chapter.spine_index == spine_index)
    }

    /// Fill the chapter lengths from a freshly built anchor map.
    pub fn apply_anchor_lengths(&mut self, anchors: &AnchorMap) {
        for chapter in &mut self.chapters {
            if let Some(anchors) = anchors.chapter(chapter.chapter_uid).or_else(|| {
                anchors
                    .chapters
                    .iter()
                    .find(|a| a.spine_index == chapter.spine_index)
            }) {
                chapter.char_len = Some(anchors.total_chars);
            }
        }
    }

    /// WeRead coordinate → rebuilt EPUB position.
    ///
    /// The uid is authoritative: a coordinate whose uid is not in the
    /// downloaded spine is refused rather than matched to another chapter by
    /// index. A known chapter length clamps a past-the-end offset and reports
    /// [`Precision::Clamped`].
    pub fn to_epub(&self, coordinate: WeReadCoordinate) -> Result<MappedPosition, PositionError> {
        if coordinate.chapter_offset < 0 {
            return Err(PositionError::InvalidOffset {
                chapter_uid: coordinate.chapter_uid,
                offset: coordinate.chapter_offset,
            });
        }
        let chapter = self
            .chapter_by_uid(coordinate.chapter_uid)
            .ok_or(PositionError::UnknownChapter(coordinate.chapter_uid))?;
        let (char_offset, precision) = match chapter.char_len {
            Some(len) if coordinate.chapter_offset > len => (len, Precision::Clamped),
            Some(_) => (coordinate.chapter_offset, Precision::Exact),
            None => (coordinate.chapter_offset, Precision::Unverified),
        };
        Ok(MappedPosition {
            position: EpubPosition {
                spine_index: chapter.spine_index,
                char_offset,
            },
            precision,
        })
    }

    /// Rebuilt EPUB position → WeRead coordinate.
    pub fn to_weread(&self, position: EpubPosition) -> Result<MappedCoordinate, PositionError> {
        if position.char_offset < 0 {
            return Err(PositionError::InvalidOffset {
                chapter_uid: 0,
                offset: position.char_offset,
            });
        }
        let chapter = self
            .chapter_by_spine(position.spine_index)
            .ok_or(PositionError::UnknownSpine(position.spine_index))?;
        let (chapter_offset, precision) = match chapter.char_len {
            Some(len) if position.char_offset > len => (len, Precision::Clamped),
            Some(_) => (position.char_offset, Precision::Exact),
            None => (position.char_offset, Precision::Unverified),
        };
        Ok(MappedCoordinate {
            coordinate: WeReadCoordinate {
                chapter_uid: chapter.chapter_uid,
                chapter_idx: chapter.chapter_idx,
                chapter_offset,
            },
            precision,
        })
    }

    /// The chapter-start XPointer of a spine item.
    pub fn chapter_xpointer(&self, spine_index: u32) -> Result<String, PositionError> {
        self.chapter_by_spine(spine_index)
            .ok_or(PositionError::UnknownSpine(spine_index))
            .map(|_| format!("/body/DocFragment[{}]/body", spine_index + 1))
    }

    /// The best XPointer this map can name for `coordinate`.
    ///
    /// With anchors, the exact text node. Without them, the chapter start;
    /// an in-chapter offset of a chapter longer than
    /// [`MAX_ANCHORLESS_CHAPTER_CHARS`] is refused instead of guessed.
    pub fn xpointer_for(
        &self,
        coordinate: WeReadCoordinate,
        anchors: Option<&AnchorMap>,
    ) -> Result<XPointerTarget, PositionError> {
        let mapped = self.to_epub(coordinate)?;
        if let Some(target) = anchors.and_then(|anchors| anchors.xpointer_at(mapped.position)) {
            return Ok(target);
        }
        if mapped.position.char_offset == 0 {
            return Ok(XPointerTarget {
                xpointer: self.chapter_xpointer(mapped.position.spine_index)?,
                precision: Precision::Exact,
            });
        }
        let chapter = self
            .chapter_by_spine(mapped.position.spine_index)
            .ok_or(PositionError::UnknownSpine(mapped.position.spine_index))?;
        let length = chapter.char_len.or(chapter.word_count);
        if length.is_some_and(|chars| chars > MAX_ANCHORLESS_CHAPTER_CHARS) {
            return Err(PositionError::LongChapterNeedsAnchor {
                chapter_uid: chapter.chapter_uid,
                chars: length.unwrap_or(0),
            });
        }
        Ok(XPointerTarget {
            xpointer: self.chapter_xpointer(mapped.position.spine_index)?,
            precision: Precision::ChapterStart,
        })
    }

    /// Place a crengine XPointer with this map, using anchors when present.
    pub fn from_xpointer(
        &self,
        xpointer: &str,
        anchors: Option<&AnchorMap>,
    ) -> Result<MappedPosition, PositionError> {
        let spine_index = doc_fragment_index(xpointer)?;
        let chapter = self
            .chapter_by_spine(spine_index)
            .ok_or(PositionError::UnknownSpine(spine_index))?;
        if let Some(position) = anchors.and_then(|anchors| anchors.char_offset_of(xpointer)) {
            return Ok(MappedPosition {
                position,
                precision: Precision::Exact,
            });
        }
        Ok(MappedPosition {
            position: EpubPosition {
                spine_index: chapter.spine_index,
                char_offset: 0,
            },
            precision: Precision::ChapterStart,
        })
    }
}

impl AnchorMap {
    /// The anchors of a chapter.
    pub fn chapter(&self, chapter_uid: i64) -> Option<&ChapterAnchors> {
        self.chapters
            .iter()
            .find(|chapter| chapter.chapter_uid == chapter_uid)
    }

    /// Insert or replace one chapter's anchors, keeping spine order.
    pub fn set_chapter(&mut self, anchors: ChapterAnchors) {
        self.chapters
            .retain(|chapter| chapter.chapter_uid != anchors.chapter_uid);
        self.chapters.push(anchors);
        self.chapters.sort_by_key(|chapter| chapter.spine_index);
    }

    /// Build anchors for one chapter from its XHTML bytes.
    pub fn parse_chapter(
        chapter_uid: i64,
        spine_index: u32,
        xhtml: &[u8],
    ) -> Result<ChapterAnchors, PositionError> {
        let source =
            std::str::from_utf8(xhtml).map_err(|error| PositionError::Xhtml(error.to_string()))?;
        let document = roxmltree::Document::parse_with_options(
            source,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..roxmltree::ParsingOptions::default()
            },
        )
        .map_err(|error| PositionError::Xhtml(error.to_string()))?;
        let body = document
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case("body"))
            .unwrap_or_else(|| document.root_element());

        let prefix = format!("/body/DocFragment[{}]", spine_index + 1);
        let mut anchors = Vec::new();
        let mut char_start = 0i64;
        for node in body.descendants() {
            if !node.is_text() {
                continue;
            }
            if node.ancestors().any(|ancestor| {
                ancestor.is_element()
                    && matches!(
                        ancestor.tag_name().name().to_ascii_lowercase().as_str(),
                        "head" | "script" | "style" | "title" | "noscript"
                    )
            }) {
                continue;
            }
            let Some(text) = node.text() else {
                continue;
            };
            if text.is_empty() || text.chars().all(char::is_whitespace) {
                continue;
            }
            let char_len = text.chars().count() as i64;
            let path = format!(
                "{}/{}{}",
                prefix,
                element_path(body, node.parent_element()),
                text_step(node)
            );
            anchors.push(TextAnchor {
                char_start,
                char_len,
                path,
            });
            char_start += char_len;
        }

        if anchors.is_empty() {
            anchors.push(TextAnchor {
                char_start: 0,
                char_len: 0,
                path: format!("{}/body", prefix),
            });
        }

        Ok(ChapterAnchors {
            chapter_uid,
            spine_index,
            total_chars: char_start,
            anchors,
        })
    }

    /// Build anchors for every chapter of a rebuilt EPUB.
    pub fn from_epub(path: &Path, map: &PositionMap) -> Result<Self, PositionError> {
        let file = File::open(path)
            .map_err(|error| PositionError::Epub(format!("{}: {error}", path.display())))?;
        let mut zip =
            zip::ZipArchive::new(file).map_err(|error| PositionError::Epub(error.to_string()))?;
        let mut anchors = AnchorMap {
            book_id: map.book_id.clone(),
            chapters: Vec::new(),
        };
        let mut seen_spine = std::collections::BTreeSet::new();
        for chapter in &map.chapters {
            if !seen_spine.insert(chapter.spine_index) {
                continue;
            }
            let entry = format!("OEBPS/{}", chapter.href);
            let mut file = zip
                .by_name(&entry)
                .map_err(|error| PositionError::Epub(format!("{entry}: {error}")))?;
            let mut bytes = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut bytes)
                .map_err(|error| PositionError::Epub(format!("{entry}: {error}")))?;
            anchors.set_chapter(Self::parse_chapter(
                chapter.chapter_uid,
                chapter.spine_index,
                &bytes,
            )?);
        }
        Ok(anchors)
    }

    /// The exact XPointer of a chapter character offset.
    pub fn xpointer_at(&self, position: EpubPosition) -> Option<XPointerTarget> {
        let chapter = self
            .chapters
            .iter()
            .find(|chapter| chapter.spine_index == position.spine_index)?;
        let (char_offset, precision) = if position.char_offset > chapter.total_chars {
            (chapter.total_chars, Precision::Clamped)
        } else if position.char_offset < 0 {
            (0, Precision::Clamped)
        } else {
            (position.char_offset, Precision::Exact)
        };
        let anchor = chapter
            .anchors
            .iter()
            .find(|anchor| {
                anchor.char_len > 0
                    && char_offset >= anchor.char_start
                    && char_offset < anchor.char_start + anchor.char_len
            })
            .or_else(|| chapter.anchors.last())?;
        if anchor.char_len <= 0 {
            return Some(XPointerTarget {
                xpointer: anchor.path.clone(),
                precision: Precision::ChapterStart,
            });
        }
        // A text-node offset names a character, so `char_len` itself is one
        // past the node. Clamp an out-of-range offset to the last character.
        let local = (char_offset - anchor.char_start).clamp(0, anchor.char_len - 1);
        Some(XPointerTarget {
            xpointer: format!("{}.{}", anchor.path, local),
            precision,
        })
    }

    /// The chapter position an XPointer names.
    pub fn char_offset_of(&self, xpointer: &str) -> Option<EpubPosition> {
        let xpointer = xpointer.strip_prefix("xpointer:").unwrap_or(xpointer);
        let spine_index = doc_fragment_index(xpointer).ok()?;
        let chapter = self
            .chapters
            .iter()
            .find(|chapter| chapter.spine_index == spine_index)?;
        let element_base = strip_trailing_numeric_offset(xpointer);
        let mut matching: Option<(&TextAnchor, i64)> = None;
        for anchor in &chapter.anchors {
            if xpointer == anchor.path {
                matching = Some((anchor, 0));
                break;
            }
            if let Some(local) = xpointer_offset_after(xpointer, &anchor.path) {
                if let Ok(len) = local.parse::<i64>() {
                    if anchor.char_len > 0 && len >= 0 {
                        // crengine counts raw DOM text characters including
                        // leading/trailing whitespace trimmed during anchor
                        // indexing, so clamp instead of rejecting.
                        matching = Some((anchor, len.min(anchor.char_len)));
                        break;
                    }
                }
            }
            // An element pointer (with or without a trailing `.0`) names that
            // element's start. Its own path is a prefix of every text anchor
            // inside it, so the first anchor in document order is the offset.
            if anchor.path.starts_with(&format!("{element_base}/")) {
                matching.get_or_insert((anchor, 0));
            }
        }
        let (anchor, local) = matching?;
        Some(EpubPosition {
            spine_index,
            char_offset: anchor.char_start + local,
        })
    }
}

/// Strip a trailing `.N` offset from an element XPointer (e.g. `.../p[2].0` -> `.../p[2]`).
fn strip_trailing_numeric_offset(xpointer: &str) -> &str {
    match xpointer.rsplit_once('.') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => xpointer,
    }
}

/// Parse the one-based `DocFragment[N]` of an XPointer.
fn doc_fragment_index(xpointer: &str) -> Result<u32, PositionError> {
    let xpointer = xpointer.strip_prefix("xpointer:").unwrap_or(xpointer);
    let rest = xpointer
        .strip_prefix("/body/DocFragment[")
        .ok_or_else(|| PositionError::InvalidXPointer(xpointer.to_string()))?;
    let (number, _) = rest
        .split_once(']')
        .ok_or_else(|| PositionError::InvalidXPointer(xpointer.to_string()))?;
    let value: u32 = number
        .parse()
        .map_err(|_| PositionError::InvalidXPointer(xpointer.to_string()))?;
    if value == 0 {
        return Err(PositionError::InvalidXPointer(xpointer.to_string()));
    }
    Ok(value - 1)
}

/// The element path from `body` down to `parent`, inclusive.
fn element_path(body: roxmltree::Node<'_, '_>, parent: Option<roxmltree::Node<'_, '_>>) -> String {
    let mut steps = Vec::new();
    let mut current = parent;
    while let Some(node) = current {
        if !node.is_element() {
            current = node.parent();
            continue;
        }
        steps.push(element_step(node));
        if node == body {
            break;
        }
        current = node.parent_element();
    }
    steps.reverse();
    steps.join("/")
}

/// One element step, with an index only when the name repeats among siblings.
fn element_step(node: roxmltree::Node<'_, '_>) -> String {
    let name = node.tag_name().name().to_ascii_lowercase();
    let parent = node.parent_element();
    let same = parent.map_or(1, |parent| {
        parent
            .children()
            .filter(|child| {
                child.is_element() && child.tag_name().name().eq_ignore_ascii_case(&name)
            })
            .count()
    });
    if same > 1 {
        let index = parent.map_or(1, |parent| {
            parent
                .children()
                .filter(|child| child.is_element())
                .filter(|child| child.tag_name().name().eq_ignore_ascii_case(&name))
                .position(|child| child == node)
                .map_or(1, |zero| zero + 1)
        });
        format!("{name}[{index}]")
    } else {
        name
    }
}

/// One text-node step, with an index only when there are several.
fn text_step(node: roxmltree::Node<'_, '_>) -> String {
    let parent = node.parent_element();
    let same = parent.map_or(1, |parent| {
        parent.children().filter(|child| child.is_text()).count()
    });
    if same > 1 {
        let index = parent.map_or(1, |parent| {
            parent
                .children()
                .filter(|child| child.is_text())
                .position(|child| child == node)
                .map_or(1, |zero| zero + 1)
        });
        format!("/text()[{index}]")
    } else {
        "/text()".to_string()
    }
}

/// The `.N` offset of `xpointer` when `path` is its text node, else `None`.
fn xpointer_offset_after<'a>(xpointer: &'a str, path: &str) -> Option<&'a str> {
    let rest = xpointer.strip_prefix(path)?;
    rest.strip_prefix('.')
}

/// The number of characters a chapter's XHTML contributes to its anchors.
///
/// Used by the app before it has an [`AnchorMap`], for diagnostics only.
pub fn xhtml_char_len(xhtml: &[u8]) -> Result<i64, PositionError> {
    let source =
        std::str::from_utf8(xhtml).map_err(|error| PositionError::Xhtml(error.to_string()))?;
    let document = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..roxmltree::ParsingOptions::default()
        },
    )
    .map_err(|error| PositionError::Xhtml(error.to_string()))?;
    let mut total = 0i64;
    for node in document.descendants().filter(|node| node.is_text()) {
        if node.ancestors().any(|ancestor| {
            ancestor.is_element()
                && matches!(
                    ancestor.tag_name().name().to_ascii_lowercase().as_str(),
                    "head" | "script" | "style" | "title" | "noscript"
                )
        }) {
            continue;
        }
        if let Some(text) = node.text() {
            if !text.is_empty() && !text.chars().all(char::is_whitespace) {
                total += text.chars().count() as i64;
            }
        }
    }
    Ok(total)
}

/// One book's role in the persisted store: the app keys this map by
/// `bookId`, outside the core crate.
pub type PositionBookId = String;

/// Look up a book in an in-memory position table.
pub fn find_book<'a>(
    books: &'a BTreeMap<PositionBookId, BookPositions>,
    book_id: &str,
) -> Option<&'a BookPositions> {
    books.get(book_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weread::epub::{build_epub, EpubChapter, EpubSource};
    use crate::weread::model::ChapterInfo;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wr004a-position-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn toc(rows: &[(i64, i64, Option<i64>)]) -> Vec<ChapterInfo> {
        rows.iter()
            .map(|(uid, idx, words)| ChapterInfo {
                chapter_uid: Some(*uid),
                chapter_idx: Some(*idx),
                title: Some(format!("Chapter {uid}")),
                word_count: *words,
                price: Some(0.0),
                paid: Some(0),
                tar: None,
                files: Vec::new(),
            })
            .collect()
    }

    fn map() -> PositionMap {
        let spine = [
            SpineEntry {
                chapter_uid: 10,
                chapter_idx: 1,
                title: "One".into(),
                href: "Text/ch1.xhtml".into(),
            },
            SpineEntry {
                chapter_uid: 20,
                chapter_idx: 2,
                title: "Two".into(),
                href: "Text/ch2.xhtml".into(),
            },
            SpineEntry {
                chapter_uid: 30,
                chapter_idx: 3,
                title: "Three".into(),
                href: "Text/ch3.xhtml".into(),
            },
        ];
        let mut map = PositionMap::from_spine(
            "book-1",
            "42",
            &spine,
            &toc(&[(10, 1, Some(100)), (20, 2, Some(50)), (30, 3, Some(0))]),
        );
        map.chapters[0].char_len = Some(10);
        map.chapters[1].char_len = Some(5);
        map.chapters[2].char_len = Some(0);
        map
    }

    #[test]
    fn coordinates_round_trip_between_weread_and_the_spine() {
        let map = map();
        for coordinate in [
            WeReadCoordinate {
                chapter_uid: 10,
                chapter_idx: 1,
                chapter_offset: 0,
            },
            WeReadCoordinate {
                chapter_uid: 20,
                chapter_idx: 2,
                chapter_offset: 5,
            },
            WeReadCoordinate {
                chapter_uid: 30,
                chapter_idx: 3,
                chapter_offset: 0,
            },
        ] {
            let mapped = map.to_epub(coordinate).unwrap();
            assert_eq!(mapped.precision, Precision::Exact);
            let back = map.to_weread(mapped.position).unwrap();
            assert_eq!(back.coordinate.chapter_uid, coordinate.chapter_uid);
            assert_eq!(back.coordinate.chapter_offset, coordinate.chapter_offset);
        }
    }

    #[test]
    fn an_unknown_uid_is_refused_rather_than_matched_by_index() {
        let map = map();
        let error = map
            .to_epub(WeReadCoordinate {
                chapter_uid: 999,
                chapter_idx: 2,
                chapter_offset: 3,
            })
            .unwrap_err();
        assert_eq!(error, PositionError::UnknownChapter(999));
    }

    #[test]
    fn an_offset_past_the_chapter_is_clamped_to_its_end() {
        let map = map();
        let mapped = map
            .to_epub(WeReadCoordinate {
                chapter_uid: 10,
                chapter_idx: 1,
                chapter_offset: 999,
            })
            .unwrap();
        assert_eq!(mapped.position.char_offset, 10);
        assert_eq!(mapped.precision, Precision::Clamped);
        let back = map
            .to_weread(EpubPosition {
                spine_index: 0,
                char_offset: 999,
            })
            .unwrap();
        assert_eq!(back.coordinate.chapter_offset, 10);
        assert_eq!(back.precision, Precision::Clamped);
    }

    #[test]
    fn an_unknown_length_trusts_the_offset_and_says_so() {
        let mut map = map();
        map.chapters[0].char_len = None;
        let mapped = map
            .to_epub(WeReadCoordinate {
                chapter_uid: 10,
                chapter_idx: 1,
                chapter_offset: 999,
            })
            .unwrap();
        assert_eq!(mapped.position.char_offset, 999);
        assert_eq!(mapped.precision, Precision::Unverified);
    }

    #[test]
    fn chapter_start_xpointer_is_one_based_and_parses_back() {
        let map = map();
        let xpointer = map.chapter_xpointer(0).unwrap();
        assert_eq!(xpointer, "/body/DocFragment[1]/body");
        let mapped = map.from_xpointer(&xpointer, None).unwrap();
        assert_eq!(mapped.position.spine_index, 0);
        assert_eq!(mapped.position.char_offset, 0);
        assert_eq!(mapped.precision, Precision::ChapterStart);
        assert!(matches!(
            map.from_xpointer("/body/DocFragment[0]/body", None),
            Err(PositionError::InvalidXPointer(_))
        ));
        assert!(matches!(
            map.from_xpointer("/body/DocFragment[99]/body", None),
            Err(PositionError::UnknownSpine(98))
        ));
    }

    #[test]
    fn a_long_chapter_without_anchors_refuses_an_in_chapter_jump() {
        let mut map = map();
        map.chapters[0].char_len = Some(MAX_ANCHORLESS_CHAPTER_CHARS + 1);
        let error = map
            .xpointer_for(
                WeReadCoordinate {
                    chapter_uid: 10,
                    chapter_idx: 1,
                    chapter_offset: 50,
                },
                None,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            PositionError::LongChapterNeedsAnchor {
                chapter_uid: 10,
                chars
            } if chars == MAX_ANCHORLESS_CHAPTER_CHARS + 1
        ));

        let start = map
            .xpointer_for(
                WeReadCoordinate {
                    chapter_uid: 10,
                    chapter_idx: 1,
                    chapter_offset: 0,
                },
                None,
            )
            .unwrap();
        assert_eq!(start.precision, Precision::Exact);
        assert_eq!(start.xpointer, "/body/DocFragment[1]/body");
    }

    #[test]
    fn a_long_chapter_with_anchors_can_jump_inside_it() {
        let mut map = map();
        map.chapters[0].char_len = Some(MAX_ANCHORLESS_CHAPTER_CHARS + 1);
        map.chapters[0].word_count = Some(MAX_ANCHORLESS_CHAPTER_CHARS + 1);
        let mut anchors = AnchorMap {
            book_id: "book-1".into(),
            chapters: Vec::new(),
        };
        anchors.set_chapter(ChapterAnchors {
            chapter_uid: 10,
            spine_index: 0,
            total_chars: MAX_ANCHORLESS_CHAPTER_CHARS + 1,
            anchors: vec![TextAnchor {
                char_start: 0,
                char_len: MAX_ANCHORLESS_CHAPTER_CHARS + 1,
                path: "/body/DocFragment[1]/body/p[1]/text()".into(),
            }],
        });
        let target = map
            .xpointer_for(
                WeReadCoordinate {
                    chapter_uid: 10,
                    chapter_idx: 1,
                    chapter_offset: 50,
                },
                Some(&anchors),
            )
            .unwrap();
        assert_eq!(target.xpointer, "/body/DocFragment[1]/body/p[1]/text().50");
        assert_eq!(target.precision, Precision::Exact);
    }

    #[test]
    fn anchors_count_unicode_scalars_and_skip_formatting_whitespace() {
        let xhtml = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>T</title></head><body>
<p>你好世界</p>
<p>second</p>
</body></html>"#
            .as_bytes();
        let chapter = AnchorMap::parse_chapter(7, 0, xhtml).unwrap();
        assert_eq!(chapter.total_chars, 10);
        assert_eq!(chapter.anchors.len(), 2);
        assert_eq!(
            chapter.anchors[0].path,
            "/body/DocFragment[1]/body/p[1]/text()"
        );
        assert_eq!(chapter.anchors[0].char_len, 4);
        assert_eq!(chapter.anchors[1].char_start, 4);
        assert_eq!(
            chapter.anchors[1].path,
            "/body/DocFragment[1]/body/p[2]/text()"
        );
        assert_eq!(xhtml_char_len(xhtml).unwrap(), 10);
    }

    #[test]
    fn an_anchor_map_round_trips_exact_xpointers() {
        let xhtml = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><div><p>one</p><p>two<span>three</span></p></div></body></html>"#;
        let anchors = AnchorMap {
            book_id: "book-1".into(),
            chapters: vec![AnchorMap::parse_chapter(10, 0, xhtml).unwrap()],
        };
        let target = anchors
            .xpointer_at(EpubPosition {
                spine_index: 0,
                char_offset: 7,
            })
            .unwrap();
        assert_eq!(
            target.xpointer,
            "/body/DocFragment[1]/body/div/p[2]/span/text().1"
        );
        let back = anchors.char_offset_of(&target.xpointer).unwrap();
        assert_eq!(
            back,
            EpubPosition {
                spine_index: 0,
                char_offset: 7
            }
        );

        let start = anchors
            .xpointer_at(EpubPosition {
                spine_index: 0,
                char_offset: 0,
            })
            .unwrap();
        assert_eq!(
            start.xpointer,
            "/body/DocFragment[1]/body/div/p[1]/text().0"
        );
        let start_back = anchors.char_offset_of(&start.xpointer).unwrap();
        assert_eq!(start_back.char_offset, 0);
    }

    #[test]
    fn element_names_are_written_lowercase_like_crengine() {
        let xhtml = br#"<html xmlns="http://www.w3.org/1999/xhtml"><BODY><P>Hi</P></BODY></html>"#;
        let chapter = AnchorMap::parse_chapter(1, 0, xhtml).unwrap();
        assert_eq!(
            chapter.anchors[0].path,
            "/body/DocFragment[1]/body/p/text()"
        );
        assert_eq!(chapter.total_chars, 2);
    }

    #[test]
    fn multiple_text_nodes_get_an_index() {
        let xhtml =
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>a<b>b</b>c</p></body></html>"#;
        let chapter = AnchorMap::parse_chapter(1, 0, xhtml).unwrap();
        assert_eq!(
            chapter.anchors[0].path,
            "/body/DocFragment[1]/body/p/text()[1]"
        );
        assert_eq!(
            chapter.anchors[1].path,
            "/body/DocFragment[1]/body/p/b/text()"
        );
        assert_eq!(
            chapter.anchors[2].path,
            "/body/DocFragment[1]/body/p/text()[2]"
        );
        assert_eq!(chapter.total_chars, 3);
    }

    #[test]
    fn anchors_clamp_an_offset_past_the_end() {
        let xhtml = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>abc</p></body></html>"#;
        let anchors = AnchorMap {
            book_id: "book-1".into(),
            chapters: vec![AnchorMap::parse_chapter(1, 0, xhtml).unwrap()],
        };
        let target = anchors
            .xpointer_at(EpubPosition {
                spine_index: 0,
                char_offset: 99,
            })
            .unwrap();
        assert_eq!(target.xpointer, "/body/DocFragment[1]/body/p/text().2");
        assert_eq!(target.precision, Precision::Clamped);
    }

    #[test]
    fn an_offset_at_the_chapter_end_names_the_last_character() {
        let anchors = |xhtml: &[u8]| AnchorMap {
            book_id: "book-1".into(),
            chapters: vec![AnchorMap::parse_chapter(1, 0, xhtml).unwrap()],
        };
        let target = |anchors: &AnchorMap, offset| {
            anchors
                .xpointer_at(EpubPosition {
                    spine_index: 0,
                    char_offset: offset,
                })
                .unwrap()
                .xpointer
        };
        let chapter = anchors(br#"<html><body><p>abc</p><p>de</p></body></html>"#);
        assert_eq!(
            target(&chapter, 2),
            "/body/DocFragment[1]/body/p[1]/text().2"
        );
        assert_eq!(
            target(&chapter, 3),
            "/body/DocFragment[1]/body/p[2]/text().0"
        );
        // 5 is the chapter end, one past the last character; 6 is beyond it.
        assert_eq!(
            target(&chapter, 5),
            "/body/DocFragment[1]/body/p[2]/text().1"
        );
        assert_eq!(
            target(&chapter, 6),
            "/body/DocFragment[1]/body/p[2]/text().1"
        );
        let one = anchors(br#"<html><body><p>a</p></body></html>"#);
        for offset in [1, 2] {
            assert_eq!(target(&one, offset), "/body/DocFragment[1]/body/p/text().0");
        }
    }

    #[test]
    fn an_element_pointer_resolves_to_the_element_start() {
        let xhtml = br#"<html><body><p>one</p><p>two</p></body></html>"#;
        let anchors = AnchorMap {
            book_id: "book-1".into(),
            chapters: vec![AnchorMap::parse_chapter(10, 0, xhtml).unwrap()],
        };
        let offset = |pointer: &str| anchors.char_offset_of(pointer).unwrap().char_offset;
        assert_eq!(offset("/body/DocFragment[1]/body/p[1]"), 0);
        assert_eq!(offset("/body/DocFragment[1]/body/p[2]"), 3);
        assert_eq!(offset("/body/DocFragment[1]/body"), 0);
        let mapped = map()
            .from_xpointer("/body/DocFragment[1]/body/p[2]", Some(&anchors))
            .unwrap();
        assert_eq!(mapped.position.spine_index, 0);
        assert_eq!(mapped.position.char_offset, 3);
        assert_eq!(mapped.precision, Precision::Exact);
    }

    #[test]
    fn an_epub_with_the_spine_files_yields_anchors() {
        let dir = tempdir("epub");
        let mut texts = BTreeMap::new();
        texts.insert(
            "Text/ch1.xhtml".to_string(),
            br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>hello</p></body></html>"#
                .to_vec(),
        );
        let source = EpubSource {
            book_id: "book-1".into(),
            title: "Book".into(),
            chapters: vec![EpubChapter {
                chapter_uid: 10,
                chapter_idx: 1,
                title: "One".into(),
                files: vec!["Text/ch1.xhtml".into()],
            }],
            texts,
            assets: BTreeMap::new(),
        };
        let built = build_epub(&source).unwrap();
        let path = dir.join("book.epub");
        let mut file = File::create(&path).unwrap();
        file.write_all(&built.bytes).unwrap();
        drop(file);

        let map = PositionMap {
            book_id: "book-1".into(),
            book_version: "1".into(),
            chapters: vec![ChapterMapping {
                chapter_uid: 10,
                chapter_idx: 1,
                spine_index: 0,
                href: "Text/ch1.xhtml".into(),
                word_count: Some(5),
                char_len: None,
            }],
        };
        let anchors = AnchorMap::from_epub(&path, &map).unwrap();
        let chapter = anchors.chapter(10).expect("chapter anchors");
        assert_eq!(chapter.total_chars, 5);
        let target = anchors
            .xpointer_at(EpubPosition {
                spine_index: 0,
                char_offset: 1,
            })
            .unwrap();
        assert_eq!(target.xpointer, "/body/DocFragment[1]/body/p/text().1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_chapter_document_is_a_typed_error() {
        assert!(matches!(
            AnchorMap::parse_chapter(1, 0, b"<html><body><p>broken"),
            Err(PositionError::Xhtml(_))
        ));
        assert!(matches!(
            AnchorMap::parse_chapter(1, 0, &[0xff, 0xfe]),
            Err(PositionError::Xhtml(_))
        ));
    }

    #[test]
    fn serialization_keeps_the_spine_and_anchor_order() {
        let xhtml = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>abc</p></body></html>"#;
        let anchors = AnchorMap {
            book_id: "book-1".into(),
            chapters: vec![AnchorMap::parse_chapter(10, 0, xhtml).unwrap()],
        };
        let positions = BookPositions {
            map: map(),
            anchors,
        };
        let json = serde_json::to_string(&positions).unwrap();
        let back: BookPositions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, positions);
    }
}
