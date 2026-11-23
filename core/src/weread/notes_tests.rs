use std::sync::Mutex;

use super::*;
use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};
use crate::weread::auth::Credentials;
use crate::weread::client::WeReadClient;
use crate::weread::position::{ChapterAnchors, ChapterMapping, TextAnchor};

fn map() -> PositionMap {
    PositionMap {
        book_id: "100001".to_string(),
        book_version: "42".to_string(),
        chapters: vec![ChapterMapping {
            chapter_uid: 11,
            chapter_idx: 1,
            spine_index: 0,
            href: "Text/ch1.xhtml".to_string(),
            word_count: Some(100),
            char_len: Some(100),
        }],
    }
}

fn anchors() -> AnchorMap {
    AnchorMap {
        book_id: "100001".to_string(),
        chapters: vec![ChapterAnchors {
            chapter_uid: 11,
            spine_index: 0,
            total_chars: 100,
            anchors: vec![
                TextAnchor {
                    char_start: 0,
                    char_len: 20,
                    path: "/body/DocFragment[1]/body/p[1]/text()".to_string(),
                },
                TextAnchor {
                    char_start: 20,
                    char_len: 30,
                    path: "/body/DocFragment[1]/body/p[2]/text()".to_string(),
                },
                TextAnchor {
                    char_start: 50,
                    char_len: 50,
                    path: "/body/DocFragment[1]/body/p[3]/text()".to_string(),
                },
            ],
        }],
    }
}

fn note(id: &str, kind: NoteKind, chapter_uid: i64, range: &str) -> WeReadNote {
    WeReadNote {
        id: id.to_string(),
        kind,
        chapter_uid,
        chapter_idx: Some(1),
        chapter_title: "Chapter".to_string(),
        range: range.to_string(),
        quote: "quoted".to_string(),
        thought: None,
        create_time: 1,
    }
}

#[test]
fn a_range_is_a_half_open_interval() {
    assert_eq!(
        parse_note_range("2959-3007"),
        Some(NoteRange {
            start: 2959,
            end: 3007
        })
    );
    assert_eq!(
        parse_note_range(" 12, 20 "),
        Some(NoteRange { start: 12, end: 20 })
    );
    assert_eq!(parse_note_range("20-20"), None);
    assert_eq!(parse_note_range("30-20"), None);
    assert_eq!(parse_note_range("abc"), None);
    assert_eq!(parse_note_range("12"), None);
}

#[test]
fn bookmark_list_parses_a_delta_and_refuses_a_body_with_nothing() {
    let delta = parse_bookmark_list(
            br#"{"synckey":7,"updated":[{"bookmarkId":"b1","chapterUid":11,"range":"25-45","markText":"hi","createTime":5}],"chapters":[{"chapterUid":11,"chapterIdx":1,"title":"One"}]}"#,
        )
        .expect("delta");
    assert_eq!(delta.synckey, Some(7));
    assert_eq!(delta.updated.as_ref().map(Vec::len), Some(1));
    assert_eq!(delta.chapters[0].title, "One");

    // A cursor without changes is a valid empty delta.
    let empty = parse_bookmark_list(br#"{"synckey":8}"#).expect("empty delta");
    assert!(empty.updated.is_none());

    for body in [&b""[..], b"{}", b"[]", b"not json", br#"{"updated":"x"}"#] {
        let error = parse_bookmark_list(body).expect_err("bad body");
        assert!(matches!(error, WeReadError::InvalidResponse(_)), "{body:?}");
    }
}

#[test]
fn review_list_parses_nested_and_flat_entries() {
    let page = parse_review_list(
            br#"{"synckey":3,"totalCount":2,"hasMore":1,"reviews":[{"review":{"reviewId":"r1","content":"thought","abstract":"quote","range":"25-45","chapterUid":11,"createTime":9}},{"reviewId":"r2","content":"flat","chapterUid":11}]}"#,
        )
        .expect("page");
    assert_eq!(page.synckey, Some(3));
    assert_eq!(page.total_count, Some(2));
    assert!(page.has_more);
    let reviews = page.reviews.expect("reviews");
    assert_eq!(reviews.len(), 2);
    assert_eq!(reviews[0].quote, "quote");
    assert_eq!(reviews[1].content, "flat");

    for body in [&b""[..], b"{}", b"null", br#"{"reviews":"x"}"#] {
        let error = parse_review_list(body).expect_err("bad body");
        assert!(matches!(error, WeReadError::InvalidResponse(_)), "{body:?}");
    }
}

#[test]
fn a_thought_joins_the_highlight_with_the_same_range() {
    let mut notes = BookNotes::default();
    notes.merge_bookmarks(
        &BookmarkList {
            synckey: Some(5),
            updated: Some(vec![WeReadBookmark {
                bookmark_id: "b1".to_string(),
                chapter_uid: 11,
                range: "25-45".to_string(),
                mark_text: "underlined".to_string(),
                create_time: 1,
                ..WeReadBookmark::default()
            }]),
            removed: Vec::new(),
            chapters: vec![NoteChapter {
                chapter_uid: 11,
                chapter_idx: 1,
                title: "One".to_string(),
            }],
        },
        10,
    );
    notes.merge_reviews(
        &ReviewList {
            synckey: Some(6),
            total_count: Some(2),
            has_more: false,
            reviews: Some(vec![
                WeReadReview {
                    review_id: "r1".to_string(),
                    content: "first".to_string(),
                    range: "25-45".to_string(),
                    chapter_uid: 11,
                    create_time: 2,
                    ..WeReadReview::default()
                },
                WeReadReview {
                    review_id: "r2".to_string(),
                    content: "second".to_string(),
                    range: "25-45".to_string(),
                    chapter_uid: 11,
                    create_time: 3,
                    ..WeReadReview::default()
                },
                WeReadReview {
                    review_id: "r3".to_string(),
                    content: "whole book".to_string(),
                    create_time: 4,
                    ..WeReadReview::default()
                },
            ]),
        },
        11,
    );
    let composed = notes.compose();
    assert_eq!(composed.len(), 2, "{composed:?}");
    assert_eq!(composed[0].kind, NoteKind::Highlight);
    assert_eq!(composed[0].thought.as_deref(), Some("first\n\nsecond"));
    assert_eq!(composed[1].kind, NoteKind::Thought);
    assert_eq!(composed[1].content(), "whole book");
}

impl WeReadNote {
    fn content(&self) -> String {
        self.thought.clone().unwrap_or_else(|| self.quote.clone())
    }
}

#[test]
fn a_delta_updates_by_id_and_removed_ids_leave() {
    let mut notes = BookNotes::default();
    let first = BookmarkList {
        synckey: Some(1),
        updated: Some(vec![WeReadBookmark {
            bookmark_id: "b1".to_string(),
            chapter_uid: 11,
            range: "1-2".to_string(),
            mark_text: "old".to_string(),
            ..WeReadBookmark::default()
        }]),
        removed: Vec::new(),
        chapters: Vec::new(),
    };
    assert!(notes.merge_bookmarks(&first, 1));
    let second = BookmarkList {
        synckey: Some(2),
        updated: Some(vec![WeReadBookmark {
            bookmark_id: "b1".to_string(),
            chapter_uid: 11,
            range: "1-2".to_string(),
            mark_text: "new".to_string(),
            ..WeReadBookmark::default()
        }]),
        removed: Vec::new(),
        chapters: Vec::new(),
    };
    assert!(notes.merge_bookmarks(&second, 2));
    assert_eq!(notes.bookmark_count(), 1);
    assert_eq!(
        notes.bookmarks.get("b1").map(|b| b.mark_text.as_str()),
        Some("new")
    );
    assert_eq!(notes.bookmark_synckey, 2);
    let removal = BookmarkList {
        synckey: Some(3),
        updated: None,
        removed: vec!["b1".to_string()],
        chapters: Vec::new(),
    };
    assert!(notes.merge_bookmarks(&removal, 3));
    assert_eq!(notes.bookmark_count(), 0);
    assert_eq!(notes.bookmark_synckey, 3);
}

#[test]
fn an_exact_range_maps_to_the_text_nodes() {
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "25-45")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.stats.exact, 1);
    assert_eq!(mapping.stats.chapter, 0);
    let MappedNote {
        anchor,
        cross_segment,
        ..
    } = &mapping.notes[0];
    assert!(!cross_segment);
    assert_eq!(
        anchor,
        &NoteAnchor::Exact {
            start: "/body/DocFragment[1]/body/p[2]/text().5".to_string(),
            end: "/body/DocFragment[1]/body/p[2]/text().24".to_string(),
        }
    );
}

#[test]
fn a_range_across_text_nodes_keeps_both_ends() {
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "10-30")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.stats.cross_segment, 1);
    assert_eq!(
        mapping.notes[0].anchor,
        NoteAnchor::Exact {
            start: "/body/DocFragment[1]/body/p[1]/text().10".to_string(),
            end: "/body/DocFragment[1]/body/p[2]/text().9".to_string(),
        }
    );
}

#[test]
fn an_unmapped_chapter_is_listed_and_counted_not_dropped() {
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 99, "1-2")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.stats.total, 1);
    assert_eq!(mapping.stats.unplaced, 1);
    assert_eq!(mapping.notes.len(), 1);
    assert_eq!(mapping.notes[0].anchor, NoteAnchor::Unplaced);
    assert_eq!(mapping.notes[0].reason, Some(DegradeReason::UnknownChapter));
}

#[test]
fn a_range_beyond_the_chapter_degrades_to_the_chapter_start() {
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "150-160")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.stats.chapter, 1);
    assert_eq!(
        mapping.notes[0].anchor,
        NoteAnchor::Chapter {
            xpointer: "/body/DocFragment[1]/body".to_string()
        }
    );
    assert_eq!(mapping.notes[0].reason, Some(DegradeReason::OutOfChapter));
}

#[test]
fn a_missing_anchor_or_range_degrades_to_the_chapter_start() {
    let mut no_anchors = anchors();
    no_anchors.chapters.clear();
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "25-45")],
        &map(),
        &no_anchors,
    );
    assert_eq!(mapping.stats.chapter, 1);
    assert_eq!(mapping.notes[0].reason, Some(DegradeReason::MissingAnchor));

    let mapping = map_notes(&[note("n1", NoteKind::Thought, 11, "")], &map(), &anchors());
    assert_eq!(mapping.stats.chapter, 1);
    assert_eq!(mapping.notes[0].reason, Some(DegradeReason::NoRange));

    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "bad")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.notes[0].reason, Some(DegradeReason::InvalidRange));
}

#[test]
fn an_end_past_the_chapter_is_clamped_and_counted() {
    let mapping = map_notes(
        &[note("n1", NoteKind::Highlight, 11, "90-200")],
        &map(),
        &anchors(),
    );
    assert_eq!(mapping.stats.exact, 1);
    assert_eq!(mapping.stats.clamped, 1);
    assert!(mapping.notes[0].clamped);
    assert_eq!(
        mapping.notes[0].anchor,
        NoteAnchor::Exact {
            start: "/body/DocFragment[1]/body/p[3]/text().40".to_string(),
            end: "/body/DocFragment[1]/body/p[3]/text().49".to_string(),
        }
    );
}

#[derive(Clone)]
struct Fixture {
    body: std::sync::Arc<Vec<u8>>,
    sent: std::sync::Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new(body: &str) -> Self {
        Self {
            body: std::sync::Arc::new(body.as_bytes().to_vec()),
            sent: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl HttpTransport for Fixture {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.sent.lock().unwrap().push(request.url);
        Ok(HttpResponse::new(200, (*self.body).clone()))
    }
}

fn credentials() -> Credentials {
    Credentials::new("1", "token", "refresh", "device")
}

#[test]
fn the_client_asks_the_two_note_endpoints_with_the_cursor() {
    let transport = Fixture::new(
        r#"{"synckey":9,"updated":[{"bookmarkId":"b1","chapterUid":11,"range":"1-2"}]}"#,
    );
    let mut client =
        WeReadClient::new(transport, credentials()).with_base_url("https://i.weread.qq.com");
    let list = client.bookmark_list("100001", 7).expect("bookmark list");
    assert_eq!(list.synckey, Some(9));

    let transport = Fixture::new(r#"{"synckey":4,"hasMore":1,"reviews":[]}"#);
    let mut client =
        WeReadClient::new(transport, credentials()).with_base_url("https://i.weread.qq.com");
    let page = client.review_list("100001", 3, 20).expect("review list");
    assert!(page.has_more);
    assert_eq!(page.reviews.as_deref(), Some(&[][..]));
}

#[test]
fn the_client_sends_the_documented_queries() {
    let transport = Fixture::new(r#"{"synckey":9}"#);
    let mut client = WeReadClient::new(transport.clone(), credentials())
        .with_base_url("https://i.weread.qq.com");
    client.bookmark_list("100001", 7).expect("bookmark list");
    let urls = transport.urls();
    assert!(
        urls[0].contains("/book/bookmarklist?bookId=100001&synckey=7"),
        "{urls:?}"
    );
}

#[test]
fn a_broken_notes_response_surfaces_as_an_invalid_response() {
    let transport = Fixture::new("");
    let mut client =
        WeReadClient::new(transport, credentials()).with_base_url("https://i.weread.qq.com");
    let error = client.bookmark_list("100001", 0).expect_err("empty body");
    assert!(matches!(error, WeReadError::InvalidResponse(_)));

    let transport = Fixture::new("{ broken");
    let mut client =
        WeReadClient::new(transport, credentials()).with_base_url("https://i.weread.qq.com");
    let error = client
        .review_list("100001", 0, 20)
        .expect_err("broken body");
    assert!(matches!(error, WeReadError::InvalidResponse(_)));
}

#[test]
fn a_full_snapshot_drops_a_highlight_the_server_no_longer_has() {
    let mut notes = BookNotes::default();
    let full = BookmarkList {
        synckey: Some(5),
        updated: Some(vec![WeReadBookmark {
            bookmark_id: "b1".to_string(),
            chapter_uid: 11,
            range: "1-2".to_string(),
            ..WeReadBookmark::default()
        }]),
        removed: Vec::new(),
        chapters: Vec::new(),
    };
    notes.merge_bookmarks(&full, 1);
    notes.bookmarks.insert(
        "stale".to_string(),
        WeReadBookmark {
            bookmark_id: "stale".to_string(),
            chapter_uid: 11,
            range: "3-4".to_string(),
            ..WeReadBookmark::default()
        },
    );
    let after_clear = BookmarkList {
        synckey: Some(6),
        updated: Some(vec![WeReadBookmark {
            bookmark_id: "b1".to_string(),
            chapter_uid: 11,
            range: "1-2".to_string(),
            ..WeReadBookmark::default()
        }]),
        removed: Vec::new(),
        chapters: Vec::new(),
    };
    // `synckey` 0 is the full snapshot request: only what it answers stays.
    let mut snapshot_notes = BookNotes {
        bookmark_synckey: 0,
        ..notes.clone()
    };
    assert!(snapshot_notes.merge_bookmarks(&after_clear, 2));
    assert_eq!(snapshot_notes.bookmark_count(), 1);
    assert!(snapshot_notes.bookmarks.contains_key("b1"));

    // A delta (`synckey` non-zero) keeps the notes it did not mention.
    assert!(notes.merge_bookmarks(&after_clear, 2));
    assert_eq!(notes.bookmark_count(), 2);
}
