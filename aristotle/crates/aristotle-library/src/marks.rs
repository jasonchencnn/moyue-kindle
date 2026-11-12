// SPDX-License-Identifier: AGPL-3.0-or-later
//! The list behind "My highlights and notes": one row per highlight or
//! note, newest first, with the books that left the library marked as
//! unreachable.
//!
//! The app only displays and pages this list; the resolution from an
//! annotation to a position stays with the reader ([`crate::index`] keeps
//! the saved position verbatim).

use crate::index::{Annotation, AnnotationKind, Index};

/// A highlight or a note of the library, with the book it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The book's ID.
    pub book_id: String,
    /// The book's title, as the library shows it.
    pub book_title: String,
    /// The book is in the library's "removed" list: the row cannot jump.
    pub missing: bool,
    /// The annotation as saved: the app resolves its position when the
    /// book opens.
    pub annotation: Annotation,
    /// The highlighted text, or the note's text, trimmed.
    pub text: String,
    /// The note attached to a highlight, trimmed, when there is one.
    pub note: Option<String>,
}

/// The highlights and notes of `index`, newest first. Bookmarks and
/// annotations without text are left out. Books that were deleted from the
/// library keep their rows, marked as missing, so the reader still sees
/// what was in them.
pub fn entries(index: &Index) -> Vec<Entry> {
    let mut out = Vec::new();
    for book in index.books.iter().chain(index.removed.iter()) {
        let missing = index.removed.iter().any(|r| r.id == book.id);
        for a in &book.annotations {
            if !matches!(a.kind, AnnotationKind::Highlight | AnnotationKind::Note) {
                continue;
            }
            let Some(text) = a.text.as_deref().map(str::trim) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let note = a
                .note
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(String::from);
            out.push(Entry {
                book_id: book.id.clone(),
                book_title: book.meta.title.clone(),
                missing,
                annotation: a.clone(),
                text: text.to_string(),
                note,
            });
        }
    }
    // "YYYY-MM-DD HH:MM:SS" orders as text; `None` sorts last, and the
    // sort is stable, so rows with the same time keep index order.
    out.sort_by(|a, b| b.annotation.created.cmp(&a.annotation.created));
    out
}

/// How many highlights and notes the list shows, counted as [`entries`]
/// builds it: a highlight with a note counts as a note too, and bookmarks
/// or annotations without text are left out.
pub fn counts(index: &Index) -> (usize, usize) {
    let mut highlights = 0;
    let mut notes = 0;
    for entry in entries(index) {
        match &entry.annotation.kind {
            AnnotationKind::Highlight => {
                highlights += 1;
                if entry.note.is_some() {
                    notes += 1;
                }
            }
            AnnotationKind::Note => notes += 1,
            AnnotationKind::Bookmark => {}
        }
    }
    (highlights, notes)
}

/// How many pages `entries` take with `per_page` rows on a page: at least
/// one, so an empty list still has a page for the empty state.
pub fn page_count(entries: usize, per_page: usize) -> usize {
    entries.div_ceil(per_page.max(1)).max(1)
}

/// A saved time as the list shows it: "YYYY-MM-DD HH:MM", without the
/// seconds. Anything shorter is returned unchanged.
pub fn display_time(created: &str) -> &str {
    let created = created.trim();
    match created.char_indices().nth(16) {
        Some((at, _)) => &created[..at],
        None => created,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Format;
    use crate::index::{FileStamp, Status};
    use crate::metadata::{Author, Metadata};

    fn book(id: &str, title: &str) -> crate::BookRecord {
        crate::BookRecord {
            id: id.into(),
            file: FileStamp {
                path: format!("{title}.epub"),
                size: 1,
                mtime: 1,
                mtime_ns: 0,
            },
            format: Format::Epub,
            meta: Metadata {
                title: title.into(),
                authors: vec![Author {
                    name: "Platon".into(),
                    sort: "Platon".into(),
                }],
                ..Metadata::default()
            },
            cover: None,
            added: 0,
            last_opened: None,
            status: Status::Reading,
            progress: None,
            settings: Default::default(),
            annotations: Vec::new(),
            annotation_tombstones: Vec::new(),
        }
    }

    fn highlight(text: &str, note: Option<&str>, created: Option<&str>) -> Annotation {
        Annotation {
            id: crate::index::generate_annotation_id(),
            kind: AnnotationKind::Highlight,
            text: Some(text.into()),
            page: None,
            location: None,
            position: None,
            created: created.map(String::from),
            modified: None,
            color: None,
            style: None,
            source: "aristotle".into(),
            chapter: None,
            note: note.map(String::from),
        }
    }

    #[test]
    fn newest_first_and_undated_last() {
        let mut a = book("a", "Fedon");
        a.annotations
            .push(highlight("first", None, Some("2026-10-01 10:00:00")));
        a.annotations
            .push(highlight("third", None, Some("2026-11-01 09:00:00")));
        let mut b = book("b", "Uczta");
        b.annotations
            .push(highlight("second", None, Some("2026-10-15 21:05:09")));
        b.annotations.push(highlight("undated", None, None));
        let index = Index {
            books: vec![a, b],
            ..Index::default()
        };
        let got: Vec<String> = entries(&index).into_iter().map(|e| e.text).collect();
        assert_eq!(got, ["third", "second", "first", "undated"]);
    }

    #[test]
    fn bookmarks_and_empty_text_are_left_out() {
        let mut a = book("a", "Fedon");
        let mut bookmark = highlight("", None, Some("2026-11-09 10:00:00"));
        bookmark.kind = AnnotationKind::Bookmark;
        bookmark.text = None;
        a.annotations.push(bookmark);
        a.annotations.push(highlight("   ", None, None));
        a.annotations.push(highlight(" real text ", None, None));
        let index = Index {
            books: vec![a],
            ..Index::default()
        };
        let got = entries(&index);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "real text");
    }

    #[test]
    fn notes_keep_the_highlight_and_attach_the_note() {
        let mut a = book("a", "Fedon");
        let mut imported = highlight("Zobacz też Gorgiasz", None, None);
        imported.kind = AnnotationKind::Note;
        imported.source = "kindle-sidecar".into();
        a.annotations.push(highlight(
            "Dusza jest nieśmiertelna.",
            Some("por. Fajdros"),
            None,
        ));
        a.annotations.push(imported);
        let index = Index {
            books: vec![a],
            ..Index::default()
        };
        let got = entries(&index);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "Dusza jest nieśmiertelna.");
        assert_eq!(got[0].note.as_deref(), Some("por. Fajdros"));
        assert_eq!(got[1].text, "Zobacz też Gorgiasz");
        assert_eq!(got[1].note, None);
    }

    #[test]
    fn counts_follow_the_list_and_include_highlight_notes() {
        let mut a = book("a", "Fedon");
        a.annotations.push(highlight("plain", None, None));
        a.annotations
            .push(highlight("with a note", Some("por. Fajdros"), None));
        let mut imported = highlight("Zobacz też Gorgiasz", None, None);
        imported.kind = AnnotationKind::Note;
        a.annotations.push(imported);
        let mut bookmark = highlight("", None, None);
        bookmark.kind = AnnotationKind::Bookmark;
        a.annotations.push(bookmark);
        a.annotations.push(highlight("   ", None, None));
        let index = Index {
            books: vec![a],
            ..Index::default()
        };
        // Two highlights, one of them with a note; the imported note is a
        // note of its own. The bookmark and the blank highlight are left
        // out, as the list leaves them out.
        assert_eq!(counts(&index), (2, 2));
    }

    #[test]
    fn removed_books_keep_their_rows_marked_missing() {
        let mut gone = book("gone", "Spalona książka");
        gone.annotations.push(highlight("zaginione", None, None));
        let index = Index {
            books: vec![book("here", "Fedon")],
            removed: vec![gone],
            ..Index::default()
        };
        let got = entries(&index);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].book_id, "gone");
        assert!(got[0].missing);
    }

    #[test]
    fn pages_and_display_time() {
        assert_eq!(page_count(0, 4), 1);
        assert_eq!(page_count(4, 4), 1);
        assert_eq!(page_count(5, 4), 2);
        assert_eq!(page_count(9, 0), 9);
        assert_eq!(display_time("2026-11-09 21:38:07"), "2026-11-09 21:38");
        assert_eq!(display_time("2026-11-09"), "2026-11-09");
        assert_eq!(display_time(""), "");
    }
}
