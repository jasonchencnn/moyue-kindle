//! Cloud highlights and thoughts of one book (contract §14.3, §14.6).
//!
//! `/book/bookmarklist` answers the highlight list with a `synckey` version
//! cursor and `/review/list` answers the reader's own thoughts. [`BookNotes`]
//! merges delta answers by id, [`BookNotes::compose`] joins a thought to the
//! highlight it was written on, and [`map_notes`] places every note on the
//! rebuilt EPUB through the chapter table and the text anchors
//! [`crate::weread::position`] keeps.
//!
//! A note the map cannot place is never dropped: it degrades to the chapter
//! start ([`NoteAnchor::Chapter`]) or to a list-only entry without a position
//! ([`NoteAnchor::Unplaced`]), and [`MappingStats`] counts both so the app can
//! disclose them.
//!
//! # Coordinates
//!
//! The server's `range` is `"start-end"` in the chapter's text coordinate
//! space, the end exclusive (Tencent's notes document shows `"2959-3007"`).
//! §14.6 fixes the rebuilt-EPUB side to Unicode scalar values; the server's
//! own unit is not in the contract, so a chapter with non-BMP characters
//! before the range can shift the placement by a few characters. That gap is
//! recorded in `docs/tasks/WR-006/notes.md`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::position::{AnchorMap, EpubPosition, PositionMap};
use super::WeReadError;

/// One cloud highlight (`/book/bookmarklist` `updated[]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeReadBookmark {
    /// `bookmarkId`; the merge key of the delta.
    #[serde(default)]
    pub bookmark_id: String,
    /// WeRead book id.
    #[serde(default)]
    pub book_id: String,
    /// Chapter the highlight is in.
    #[serde(default)]
    pub chapter_uid: i64,
    /// `chapterIdx`, when the server sends it.
    #[serde(default)]
    pub chapter_idx: Option<i64>,
    /// `"start-end"`, end exclusive.
    #[serde(default)]
    pub range: String,
    /// The highlighted text.
    #[serde(default)]
    pub mark_text: String,
    /// Unix seconds the highlight was made.
    #[serde(default)]
    pub create_time: i64,
    /// WeRead style/colour tags, kept for the future write-back.
    #[serde(default)]
    pub style: i64,
    /// WeRead colour tag.
    #[serde(default)]
    pub color_style: i64,
    /// WeRead `type` (`1` is a highlight).
    #[serde(rename = "type", default)]
    pub kind: i64,
}

/// One thought (`/review/list` `reviews[].review`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WeReadReview {
    /// `reviewId`; the merge key.
    #[serde(default)]
    pub review_id: String,
    /// The thought itself.
    #[serde(default)]
    pub content: String,
    /// The underlined text the thought was written on, when it has one.
    #[serde(rename = "abstract", default)]
    pub quote: String,
    /// `"start-end"` of that underlined text, when it has one.
    #[serde(default)]
    pub range: String,
    /// Chapter the thought belongs to; `0` for a whole-book review.
    #[serde(default)]
    pub chapter_uid: i64,
    /// `chapterIdx`, when the server sends it.
    #[serde(default)]
    pub chapter_idx: i64,
    /// Chapter name of a chapter-level thought.
    #[serde(default)]
    pub chapter_name: String,
    /// Unix seconds the thought was made.
    #[serde(default)]
    pub create_time: i64,
    /// Book rating of a whole-book review (`-1` when absent).
    #[serde(default)]
    pub star: i64,
    /// `isFinish` of a whole-book review.
    #[serde(default)]
    pub is_finish: i64,
}

/// A chapter entry of `/book/bookmarklist` (`chapters[]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteChapter {
    /// WeRead `chapterUid`.
    #[serde(default)]
    pub chapter_uid: i64,
    /// Zero-based chapter index.
    #[serde(default)]
    pub chapter_idx: i64,
    /// Chapter title.
    #[serde(default)]
    pub title: String,
}

/// `/book/bookmarklist` response: a delta since the request's `synckey`.
///
/// `updated` is `None` when the server sent a cursor without a change list;
/// that is a valid empty delta, while a body with neither field is refused by
/// [`parse_bookmark_list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookmarkList {
    /// Cursor for the next request.
    pub synckey: Option<i64>,
    /// Changed highlights.
    pub updated: Option<Vec<WeReadBookmark>>,
    /// Ids the server reported as removed, when it does.
    pub removed: Vec<String>,
    /// Chapter titles for this book.
    pub chapters: Vec<NoteChapter>,
}

/// `/review/list` response: the reader's own thoughts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewList {
    /// Cursor (page or version) for the next request.
    pub synckey: Option<i64>,
    /// Total thoughts on the book, when sent.
    pub total_count: Option<i64>,
    /// Whether the server has more entries for this cursor.
    pub has_more: bool,
    /// The thoughts of this page; `None` when the response has no list.
    pub reviews: Option<Vec<WeReadReview>>,
}

/// One highlight or thought, as the reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeReadNote {
    /// `bookmarkId` or `reviewId`.
    pub id: String,
    /// Where the note came from.
    pub kind: NoteKind,
    /// WeRead `chapterUid`; `0` for a whole-book review.
    pub chapter_uid: i64,
    /// Chapter order, when known.
    pub chapter_idx: Option<i64>,
    /// Chapter title, when known.
    pub chapter_title: String,
    /// `"start-end"`, empty when the note cannot name a range.
    pub range: String,
    /// The underlined text.
    pub quote: String,
    /// The thought, joined when several thoughts sit on one highlight.
    pub thought: Option<String>,
    /// Unix seconds.
    pub create_time: i64,
}

/// Whether a note is a highlight or a thought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// A cloud highlight (`/book/bookmarklist`).
    Highlight,
    /// A thought or a whole-book review (`/review/list`).
    Thought,
}

/// How exactly a note's range was placed on the rebuilt EPUB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteAnchor {
    /// Both ends were named by text anchors.
    Exact {
        /// XPointer of the first character.
        start: String,
        /// XPointer of the last character of the range.
        end: String,
    },
    /// Only the chapter start could be named.
    Chapter {
        /// The chapter's start XPointer.
        xpointer: String,
    },
    /// Nothing could be named; the note is shown in the list only.
    Unplaced,
}

/// Why a note degraded to [`NoteAnchor::Chapter`] or [`NoteAnchor::Unplaced`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeReason {
    /// The note names no range (a whole-book or chapter-level thought).
    NoRange,
    /// The range is not `"start-end"` with `end > start`.
    InvalidRange,
    /// The chapter is not in the rebuilt EPUB's spine.
    UnknownChapter,
    /// The chapter has no verified text anchors yet.
    MissingAnchor,
    /// The range starts past the chapter's last character.
    OutOfChapter,
}

/// A composed note plus where (if anywhere) it belongs on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedNote {
    /// The note.
    pub note: WeReadNote,
    /// The best anchor the map could name.
    pub anchor: NoteAnchor,
    /// The degradation, when the anchor is not exact.
    pub reason: Option<DegradeReason>,
    /// The range end was past the chapter and was clamped to it.
    pub clamped: bool,
    /// The range spans more than one text node.
    pub cross_segment: bool,
}

/// Counts of how the notes were placed, for the app's disclosure line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MappingStats {
    /// Notes considered.
    pub total: usize,
    /// Notes with both ends on text anchors.
    pub exact: usize,
    /// Notes degraded to the chapter start.
    pub chapter: usize,
    /// Notes with no place in this edition (list only).
    pub unplaced: usize,
    /// Exact notes whose end was past the chapter and clamped.
    pub clamped: usize,
    /// Exact notes spanning more than one text node.
    pub cross_segment: usize,
}

/// Every composed note of a book with its placement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesMapping {
    /// Notes in reading order (unplaced last).
    pub notes: Vec<MappedNote>,
    /// Placement counts.
    pub stats: MappingStats,
}

/// The raw cloud notes of one book, as they are cached on disk.
///
/// The values are keyed by their server ids, so a delta answer updates a note
/// instead of duplicating it. The structure is the on-disk schema: every field
/// has a `serde` default, so an older or newer cache still loads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookNotes {
    /// `bookmarkId` → highlight.
    #[serde(default)]
    pub bookmarks: BTreeMap<String, WeReadBookmark>,
    /// `reviewId` → thought.
    #[serde(default)]
    pub reviews: BTreeMap<String, WeReadReview>,
    /// `chapterUid` → title, from `bookmarklist.chapters[]`.
    #[serde(default)]
    pub chapters: BTreeMap<i64, String>,
    /// `chapterUid` → chapter index.
    #[serde(default)]
    pub chapter_idx: BTreeMap<i64, i64>,
    /// Cursor for the next `bookmarklist` request.
    #[serde(default)]
    pub bookmark_synckey: i64,
    /// Cursor for the next `review/list` request.
    #[serde(default)]
    pub review_synckey: i64,
    /// Unix seconds of the last successful merge.
    #[serde(default)]
    pub updated_at: i64,
}

impl BookNotes {
    /// Merge one `bookmarklist` delta. Returns whether anything changed.
    ///
    /// A response without `updated` changes nothing but may advance the
    /// cursor; `removed` deletes by id.
    pub fn merge_bookmarks(&mut self, list: &BookmarkList, now: i64) -> bool {
        // `synckey` 0 asks for the full snapshot: a note the server no longer
        // has must leave the cache, even without a `removed` entry.
        let snapshot = self.bookmark_synckey == 0 && list.updated.is_some();
        let mut keep = BTreeSet::new();
        let mut changed = false;
        if let Some(updated) = &list.updated {
            for bookmark in updated {
                let key = bookmark_key(bookmark);
                keep.insert(key.clone());
                if self.bookmarks.insert(key, bookmark.clone()) != Some(bookmark.clone()) {
                    changed = true;
                }
            }
        }
        for id in &list.removed {
            if self.bookmarks.remove(id).is_some() {
                changed = true;
            }
        }
        if snapshot {
            let before = self.bookmarks.len();
            self.bookmarks.retain(|key, _| keep.contains(key));
            changed |= self.bookmarks.len() != before;
        }
        for chapter in &list.chapters {
            if self
                .chapters
                .insert(chapter.chapter_uid, chapter.title.clone())
                .as_deref()
                != Some(chapter.title.as_str())
            {
                changed = true;
            }
            if self
                .chapter_idx
                .insert(chapter.chapter_uid, chapter.chapter_idx)
                != Some(chapter.chapter_idx)
            {
                changed = true;
            }
        }
        if let Some(synckey) = list.synckey {
            if self.bookmark_synckey != synckey {
                self.bookmark_synckey = synckey;
                changed = true;
            }
        }
        if changed {
            self.updated_at = now;
        }
        changed
    }

    /// Merge one `review/list` page. Returns whether anything changed.
    pub fn merge_reviews(&mut self, page: &ReviewList, now: i64) -> bool {
        let mut changed = false;
        if let Some(reviews) = &page.reviews {
            for review in reviews {
                let key = review_key(review);
                if self.reviews.insert(key, review.clone()) != Some(review.clone()) {
                    changed = true;
                }
            }
        }
        if let Some(synckey) = page.synckey {
            if self.review_synckey != synckey {
                self.review_synckey = synckey;
                changed = true;
            }
        }
        if changed {
            self.updated_at = now;
        }
        changed
    }

    /// Number of cached highlights.
    pub fn bookmark_count(&self) -> usize {
        self.bookmarks.len()
    }

    /// Number of cached thoughts.
    pub fn review_count(&self) -> usize {
        self.reviews.len()
    }

    /// Join highlights and thoughts into reading-order notes.
    ///
    /// A thought whose range matches a highlight's range in the same chapter
    /// is that highlight's thought; several thoughts on one highlight are
    /// joined with a blank line. Every other thought stays a note of its own
    /// (a chapter comment or a whole-book review).
    pub fn compose(&self) -> Vec<WeReadNote> {
        let mut notes: Vec<WeReadNote> = Vec::with_capacity(self.bookmarks.len());
        let mut by_range: BTreeMap<(i64, String), usize> = BTreeMap::new();
        for bookmark in self.bookmarks.values() {
            let chapter_idx = bookmark
                .chapter_idx
                .or_else(|| self.chapter_idx.get(&bookmark.chapter_uid).copied());
            let index = notes.len();
            notes.push(WeReadNote {
                id: bookmark.bookmark_id.clone(),
                kind: NoteKind::Highlight,
                chapter_uid: bookmark.chapter_uid,
                chapter_idx,
                chapter_title: self
                    .chapters
                    .get(&bookmark.chapter_uid)
                    .cloned()
                    .unwrap_or_default(),
                range: bookmark.range.clone(),
                quote: bookmark.mark_text.clone(),
                thought: None,
                create_time: bookmark.create_time,
            });
            if !bookmark.range.is_empty() {
                by_range
                    .entry((bookmark.chapter_uid, bookmark.range.clone()))
                    .or_insert(index);
            }
        }
        for review in self.reviews.values() {
            let attached = if review.range.is_empty() {
                None
            } else {
                by_range
                    .get(&(review.chapter_uid, review.range.clone()))
                    .copied()
            };
            if let Some(index) = attached {
                let thought = &mut notes[index].thought;
                match thought {
                    Some(text) if !review.content.is_empty() => {
                        text.push_str("\n\n");
                        text.push_str(&review.content);
                    }
                    Some(_) => {}
                    None => *thought = Some(review.content.clone()),
                }
                continue;
            }
            let chapter_title = if review.chapter_name.is_empty() {
                self.chapters
                    .get(&review.chapter_uid)
                    .cloned()
                    .unwrap_or_default()
            } else {
                review.chapter_name.clone()
            };
            let chapter_idx = (review.chapter_idx != 0)
                .then_some(review.chapter_idx)
                .or_else(|| self.chapter_idx.get(&review.chapter_uid).copied());
            notes.push(WeReadNote {
                id: review.review_id.clone(),
                kind: NoteKind::Thought,
                chapter_uid: review.chapter_uid,
                chapter_idx,
                chapter_title,
                range: review.range.clone(),
                quote: review.quote.clone(),
                thought: Some(review.content.clone()),
                create_time: review.create_time,
            });
        }
        notes.sort_by(|a, b| {
            let a_idx = a.chapter_idx.unwrap_or(i64::MAX);
            let b_idx = b.chapter_idx.unwrap_or(i64::MAX);
            a_idx
                .cmp(&b_idx)
                .then(a.chapter_uid.cmp(&b.chapter_uid))
                .then(a.create_time.cmp(&b.create_time))
                .then(a.id.cmp(&b.id))
        });
        notes
    }
}

fn bookmark_key(bookmark: &WeReadBookmark) -> String {
    if !bookmark.bookmark_id.is_empty() {
        return bookmark.bookmark_id.clone();
    }
    // A server that omits the id still lets the range identify the note.
    format!("c{}:{}", bookmark.chapter_uid, bookmark.range)
}

fn review_key(review: &WeReadReview) -> String {
    if !review.review_id.is_empty() {
        return review.review_id.clone();
    }
    format!(
        "c{}:{}:{}",
        review.chapter_uid, review.range, review.create_time
    )
}

/// Parse one `range` (`"start-end"`, or `"start,end"` from Web payloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRange {
    /// First character, inclusive.
    pub start: i64,
    /// Last character + 1, exclusive.
    pub end: i64,
}

/// Parse the server's `"start-end"` range; `None` when it is not a valid
/// interval.
pub fn parse_note_range(text: &str) -> Option<NoteRange> {
    let text = text.trim();
    let (start, end) = text.split_once(['-', ','])?;
    let start: i64 = start.trim().parse().ok()?;
    let end: i64 = end.trim().parse().ok()?;
    (start >= 0 && end > start).then_some(NoteRange { start, end })
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .or_else(|| value.as_f64().map(|number| number as i64))
}

fn json_true(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_i64().is_some_and(|number| number != 0),
        Value::String(text) => text == "1" || text.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn response_object(body: &[u8], what: &str) -> Result<serde_json::Map<String, Value>, WeReadError> {
    if body.is_empty() {
        return Err(WeReadError::InvalidResponse(format!(
            "{what}: empty response"
        )));
    }
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| WeReadError::InvalidResponse(format!("{what}: {error}")))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| WeReadError::InvalidResponse(format!("{what}: not a JSON object")))
}

/// Parse a `/book/bookmarklist` body.
///
/// A body with neither `synckey` nor `updated` is refused: it cannot be a
/// delta, and treating it as "nothing changed" would hide a broken server.
pub fn parse_bookmark_list(body: &[u8]) -> Result<BookmarkList, WeReadError> {
    const WHAT: &str = "bookmarklist";
    let object = response_object(body, WHAT)?;
    let synckey = object.get("synckey").and_then(json_i64);
    let updated = match object.get("updated") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => Some(
            serde_json::from_value::<Vec<WeReadBookmark>>(Value::Array(items.clone()))
                .map_err(|error| WeReadError::InvalidResponse(format!("{WHAT}: {error}")))?,
        ),
        Some(_) => {
            return Err(WeReadError::InvalidResponse(format!(
                "{WHAT}: updated is not an array"
            )))
        }
    };
    let removed = match object.get("removed") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Some(_) => Vec::new(),
    };
    let chapters = match object.get("chapters") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            serde_json::from_value::<Vec<NoteChapter>>(Value::Array(items.clone()))
                .map_err(|error| WeReadError::InvalidResponse(format!("{WHAT}: {error}")))?
        }
        Some(_) => {
            return Err(WeReadError::InvalidResponse(format!(
                "{WHAT}: chapters is not an array"
            )))
        }
    };
    if synckey.is_none() && updated.is_none() && removed.is_empty() && chapters.is_empty() {
        return Err(WeReadError::InvalidResponse(format!(
            "{WHAT}: response has neither synckey nor updated"
        )));
    }
    Ok(BookmarkList {
        synckey,
        updated,
        removed,
        chapters,
    })
}

/// Parse a `/review/list` body.
pub fn parse_review_list(body: &[u8]) -> Result<ReviewList, WeReadError> {
    const WHAT: &str = "review/list";
    let object = response_object(body, WHAT)?;
    let synckey = object.get("synckey").and_then(json_i64);
    let total_count = object.get("totalCount").and_then(json_i64);
    let has_more = object.get("hasMore").is_some_and(json_true);
    let reviews = match object.get("reviews") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                // The E-Ink answer nests the review (`reviews[].review`); the
                // Web answer sometimes flattens it. Accept both.
                let candidate = item.get("review").filter(|value| value.is_object());
                let value = candidate.unwrap_or(item);
                let review: WeReadReview = serde_json::from_value(value.clone())
                    .map_err(|error| WeReadError::InvalidResponse(format!("{WHAT}: {error}")))?;
                if !review.review_id.is_empty() || !review.content.is_empty() {
                    out.push(review);
                }
            }
            Some(out)
        }
        Some(_) => {
            return Err(WeReadError::InvalidResponse(format!(
                "{WHAT}: reviews is not an array"
            )))
        }
    };
    if synckey.is_none() && reviews.is_none() && total_count.is_none() {
        return Err(WeReadError::InvalidResponse(format!(
            "{WHAT}: response has neither synckey nor reviews"
        )));
    }
    Ok(ReviewList {
        synckey,
        total_count,
        has_more,
        reviews,
    })
}

/// Place every note of `notes` on the rebuilt EPUB.
///
/// The chapter is authoritative: a note whose `chapterUid` is not in the
/// spine is [`NoteAnchor::Unplaced`] rather than matched to another chapter
/// by index. A chapter whose anchors are missing gets the chapter-start
/// marker, and a range whose start is past the chapter degrades the same way;
/// the counts in [`MappingStats`] disclose every case.
pub fn map_notes(notes: &[WeReadNote], map: &PositionMap, anchors: &AnchorMap) -> NotesMapping {
    let mut mapping = NotesMapping::default();
    mapping.stats.total = notes.len();
    for note in notes {
        let Some(chapter) = map.chapter_by_uid(note.chapter_uid) else {
            mapping.stats.unplaced += 1;
            mapping.notes.push(MappedNote {
                note: note.clone(),
                anchor: NoteAnchor::Unplaced,
                reason: Some(DegradeReason::UnknownChapter),
                clamped: false,
                cross_segment: false,
            });
            continue;
        };
        let chapter_xpointer = map.chapter_xpointer(chapter.spine_index).ok();
        let chapter_anchor = |mapping: &mut NotesMapping, reason: DegradeReason| {
            let Some(xpointer) = chapter_xpointer.clone() else {
                mapping.stats.unplaced += 1;
                mapping.notes.push(MappedNote {
                    note: note.clone(),
                    anchor: NoteAnchor::Unplaced,
                    reason: Some(reason),
                    clamped: false,
                    cross_segment: false,
                });
                return;
            };
            mapping.stats.chapter += 1;
            mapping.notes.push(MappedNote {
                note: note.clone(),
                anchor: NoteAnchor::Chapter { xpointer },
                reason: Some(reason),
                clamped: false,
                cross_segment: false,
            });
        };
        if note.range.trim().is_empty() {
            chapter_anchor(&mut mapping, DegradeReason::NoRange);
            continue;
        }
        let Some(range) = parse_note_range(&note.range) else {
            chapter_anchor(&mut mapping, DegradeReason::InvalidRange);
            continue;
        };
        let chapter_length = chapter.char_len.or_else(|| {
            anchors
                .chapter(note.chapter_uid)
                .map(|chapter| chapter.total_chars)
        });
        if chapter_length.is_some_and(|length| range.start >= length) {
            chapter_anchor(&mut mapping, DegradeReason::OutOfChapter);
            continue;
        }
        if anchors.chapter(note.chapter_uid).is_none() {
            chapter_anchor(&mut mapping, DegradeReason::MissingAnchor);
            continue;
        }
        let (end, clamped) = match chapter_length {
            Some(length) if range.end > length => (length, true),
            _ => (range.end, false),
        };
        let start = anchors.xpointer_at(EpubPosition {
            spine_index: chapter.spine_index,
            char_offset: range.start,
        });
        let end = anchors.xpointer_at(EpubPosition {
            spine_index: chapter.spine_index,
            char_offset: (end - 1).max(range.start),
        });
        let (Some(start), Some(end)) = (start, end) else {
            chapter_anchor(&mut mapping, DegradeReason::MissingAnchor);
            continue;
        };
        let start_path = start.xpointer.rsplit_once('.').map(|(path, _)| path);
        let end_path = end.xpointer.rsplit_once('.').map(|(path, _)| path);
        let cross_segment = start_path != end_path;
        mapping.stats.exact += 1;
        if clamped {
            mapping.stats.clamped += 1;
        }
        if cross_segment {
            mapping.stats.cross_segment += 1;
        }
        mapping.notes.push(MappedNote {
            note: note.clone(),
            anchor: NoteAnchor::Exact {
                start: start.xpointer,
                end: end.xpointer,
            },
            reason: None,
            clamped,
            cross_segment,
        });
    }
    mapping
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
