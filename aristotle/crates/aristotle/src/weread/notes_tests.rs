// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests of the app's cloud-note glue (WR-006).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use inkread_core::sync::transport::{HttpResponse, TransportError};
use inkread_core::weread::{
    AnchorMap, BookPositions, ChapterAnchors, ChapterMapping, PositionMap, TextAnchor,
};

fn tempdir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aristotle-weread-notes-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn credentials() -> Credentials {
    Credentials::new("1", "token", "refresh", "device")
}

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

fn positions() -> BookPositions {
    BookPositions {
        map: map(),
        anchors: anchors(),
    }
}

#[derive(Clone)]
struct NotesTransport {
    routes: Arc<Mutex<Vec<(&'static str, Vec<u8>)>>>,
    urls: Arc<Mutex<Vec<String>>>,
}

impl NotesTransport {
    fn new(routes: Vec<(&'static str, Vec<u8>)>) -> Self {
        Self {
            routes: Arc::new(Mutex::new(routes)),
            urls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn urls(&self) -> Vec<String> {
        self.urls.lock().unwrap().clone()
    }
}

impl HttpTransport for NotesTransport {
    fn send(
        &self,
        request: inkread_core::sync::transport::HttpRequest,
    ) -> Result<HttpResponse, TransportError> {
        let url = request.url.clone();
        self.urls.lock().unwrap().push(url.clone());
        let mut routes = self.routes.lock().unwrap();
        let found = routes
            .iter()
            .position(|(pattern, _)| url.contains(pattern))
            .map(|index| routes.remove(index));
        match found {
            Some((_, body)) => Ok(HttpResponse::new(200, body)),
            None => Ok(HttpResponse::new(500, b"{}".to_vec())),
        }
    }
}

const BOOKMARKS: &str = r#"{"synckey":7,"updated":[{"bookmarkId":"b1","chapterUid":11,"range":"25-45","markText":"underlined","createTime":5}],"chapters":[{"chapterUid":11,"chapterIdx":1,"title":"One"}]}"#;
const REVIEWS_PAGE_1: &str = r#"{"synckey":2,"hasMore":1,"reviews":[{"review":{"reviewId":"r1","content":"first thought","abstract":"underlined","range":"25-45","chapterUid":11,"createTime":6}}]}"#;
const REVIEWS_PAGE_2: &str = r#"{"synckey":3,"hasMore":0,"reviews":[{"review":{"reviewId":"r2","content":"whole book","chapterUid":0,"createTime":7}}]}"#;

fn pull(cache: &mut WereadNotes, transport: NotesTransport, notes: BookNotes) -> NotesEvent {
    let (outcome, _) = fetch_notes_with(transport, credentials(), "100001", notes, 100, None);
    cache.apply(outcome)
}

fn full_pull(cache: &mut WereadNotes) -> (NotesTransport, NotesEvent) {
    let transport = NotesTransport::new(vec![
        ("/book/bookmarklist", BOOKMARKS.as_bytes().to_vec()),
        ("/review/list", REVIEWS_PAGE_1.as_bytes().to_vec()),
        ("/review/list", REVIEWS_PAGE_2.as_bytes().to_vec()),
    ]);
    let notes = cache.books.get("100001").cloned().unwrap_or_default();
    let event = pull(cache, transport.clone(), notes);
    (transport, event)
}

#[test]
fn a_pull_merges_the_cloud_notes_and_writes_the_cache_once() {
    let dir = tempdir("pull");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (transport, event) = full_pull(&mut cache);
    assert_eq!(cache.summary(), (1, 1, 2));
    assert_eq!(cache.writes, 1);
    assert_eq!(event.highlights, 1);
    assert_eq!(event.thoughts, 2);
    assert!(event.message.is_none());

    let urls = transport.urls();
    assert!(
        urls.iter()
            .any(|url| url.contains("/book/bookmarklist?bookId=100001&synckey=0")),
        "{urls:?}"
    );
    assert!(
        urls.iter()
            .any(|url| url.contains("/review/list?bookId=100001")
                && url.contains("mine=1")
                && url.contains("synckey=2")),
        "{urls:?}"
    );

    let path = dir.join(NOTES_FILE);
    assert!(path.is_file());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!super::sibling(&path, ".part").exists());

    // A second process reads the same notes.
    let mut reloaded = WereadNotes::new();
    reloaded.ensure_loaded(&dir);
    assert_eq!(reloaded.summary(), (1, 1, 2));
    assert_eq!(reloaded.books["100001"].bookmark_synckey, 7);
    assert_eq!(reloaded.books["100001"].review_synckey, 3);
}

#[test]
fn an_incremental_pull_sends_the_stored_cursors() {
    let dir = tempdir("cursor");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_first, _) = full_pull(&mut cache);

    let transport = NotesTransport::new(vec![
        ("/book/bookmarklist", br#"{"synckey":8}"#.to_vec()),
        (
            "/review/list",
            br#"{"synckey":4,"hasMore":0,"reviews":[]}"#.to_vec(),
        ),
    ]);
    let notes = cache.books["100001"].clone();
    let event = pull(&mut cache, transport.clone(), notes);
    assert!(event.message.is_none());
    let urls = transport.urls();
    assert!(
        urls[0].contains("/book/bookmarklist?bookId=100001&synckey=7"),
        "{urls:?}"
    );
    assert!(urls[1].contains("synckey=3"), "{urls:?}");
    assert_eq!(cache.summary(), (1, 1, 2));
}

#[test]
fn a_broken_response_keeps_the_cached_notes() {
    let dir = tempdir("broken");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_good, _) = full_pull(&mut cache);
    let writes = cache.writes;

    let transport = NotesTransport::new(vec![("/book/bookmarklist", b"not json".to_vec())]);
    let notes = cache.books["100001"].clone();
    let event = pull(&mut cache, transport, notes);
    assert!(event.message.is_some());
    assert_eq!(cache.summary(), (1, 1, 2));
    assert_eq!(
        cache.writes, writes,
        "a failed pull must not rewrite the cache"
    );
}

#[test]
fn a_removed_highlight_leaves_the_cache() {
    let dir = tempdir("removed");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_good, _) = full_pull(&mut cache);
    let transport = NotesTransport::new(vec![
        (
            "/book/bookmarklist",
            br#"{"synckey":9,"removed":["b1"],"updated":[]}"#.to_vec(),
        ),
        (
            "/review/list",
            br#"{"synckey":4,"hasMore":0,"reviews":[]}"#.to_vec(),
        ),
    ]);
    let notes = cache.books["100001"].clone();
    let event = pull(&mut cache, transport, notes);
    assert!(event.message.is_none());
    let notes = &cache.books["100001"];
    assert_eq!(notes.bookmark_count(), 0);
    assert_eq!(notes.review_count(), 2);
}

#[test]
fn the_cache_recovers_from_the_backup_generation() {
    let dir = tempdir("backup");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_good, _) = full_pull(&mut cache);
    // A second save rotates the first generation to `.bak`.
    let transport = NotesTransport::new(vec![
        ("/book/bookmarklist", BOOKMARKS.as_bytes().to_vec()),
        (
            "/review/list",
            br#"{"synckey":4,"hasMore":0,"reviews":[]}"#.to_vec(),
        ),
    ]);
    let notes = cache.books["100001"].clone();
    pull(&mut cache, transport, notes);
    let path = dir.join(NOTES_FILE);
    assert!(super::sibling(&path, ".bak").is_file());

    std::fs::write(&path, b"{ torn").unwrap();
    let mut reloaded = WereadNotes::new();
    reloaded.ensure_loaded(&dir);
    assert_eq!(reloaded.books["100001"].bookmark_count(), 1);
    assert!(super::sibling(&path, ".unread").is_file());
}

#[test]
fn a_cache_schema_this_build_cannot_read_is_kept_aside() {
    let dir = tempdir("schema");
    std::fs::write(dir.join(NOTES_FILE), br#"{"version":99,"books":{}}"#).unwrap();
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    assert!(cache.books.is_empty());
    assert!(super::sibling(&dir.join(NOTES_FILE), ".unread-v99").is_file());
}

#[test]
fn display_reads_memory_only() {
    let dir = tempdir("display");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (transport, _) = full_pull(&mut cache);
    let requests = transport.urls().len();
    let bytes = std::fs::read(dir.join(NOTES_FILE)).unwrap();
    let writes = cache.writes;
    let fetches = cache.fetches;

    for _ in 0..32 {
        let rows = cache.rows("100001", &positions());
        assert!(!rows.is_empty());
        let pairs = cache.positions_for("100001", &positions());
        assert_eq!(pairs.len(), 1);
        assert!(cache.mapping("100001", &positions()).is_some());
    }
    assert_eq!(
        transport.urls().len(),
        requests,
        "display must not touch the network"
    );
    assert_eq!(std::fs::read(dir.join(NOTES_FILE)).unwrap(), bytes);
    assert_eq!(cache.writes, writes);
    assert_eq!(cache.fetches, fetches);
}

#[test]
fn the_rows_show_the_thought_and_disclose_degraded_notes() {
    let dir = tempdir("rows");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (transport, _) = full_pull(&mut cache);
    // A note whose chapter is not in this rebuilt edition.
    let mut notes = cache.books["100001"].clone();
    notes.reviews.insert(
        "r3".to_string(),
        inkread_core::weread::WeReadReview {
            review_id: "r3".to_string(),
            content: "unplaced thought".to_string(),
            chapter_uid: 99,
            create_time: 8,
            ..inkread_core::weread::WeReadReview::default()
        },
    );
    cache.books.insert("100001".to_string(), notes);
    let rows = cache.rows("100001", &positions());
    assert_eq!(rows.len(), 4, "{rows:#?}");
    let highlight = rows
        .iter()
        .find(|row| row.text.contains("underlined"))
        .expect("the highlight row");
    assert!(highlight.target.is_some());
    assert!(
        highlight
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("first thought")),
        "{highlight:#?}"
    );
    let disclosure = rows.last().expect("the disclosure row");
    assert!(disclosure.target.is_none());
    assert!(disclosure.detail.is_some());
    assert_eq!(
        transport.urls().len(),
        3,
        "reading the rows must not refetch"
    );
}

#[test]
fn clearing_forgets_the_memory_and_the_files() {
    let dir = tempdir("clear");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_transport, _) = full_pull(&mut cache);
    let path = dir.join(NOTES_FILE);
    assert!(path.is_file());
    cache.clear().unwrap();
    assert!(cache.books.is_empty());
    assert_eq!(cache.summary(), (0, 0, 0));
    assert!(!path.exists());
    assert!(!super::sibling(&path, ".bak").exists());
}

#[test]
fn the_positions_are_the_xpointer_interval_of_the_range() {
    let dir = tempdir("positions");
    let mut cache = WereadNotes::new();
    cache.ensure_loaded(&dir);
    let (_transport, _) = full_pull(&mut cache);
    let pairs = cache.positions_for("100001", &positions());
    assert_eq!(pairs.len(), 1);
    let (_, start, end) = &pairs[0];
    assert_eq!(
        start,
        &Position::XPointer {
            xpointer: "/body/DocFragment[1]/body/p[2]/text().5".to_string()
        }
    );
    assert_eq!(
        end,
        &Position::XPointer {
            xpointer: "/body/DocFragment[1]/body/p[2]/text().24".to_string()
        }
    );
    // The reader resolves the same pointers with the stored anchor map: the
    // mark covers exactly the cloud range.
    let Position::XPointer { xpointer } = start else {
        panic!("not an XPointer");
    };
    assert_eq!(
        positions().anchors.char_offset_of(xpointer),
        Some(inkread_core::weread::EpubPosition {
            spine_index: 0,
            char_offset: 25
        })
    );
}
