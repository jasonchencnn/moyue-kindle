// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests of the app's WeRead glue (split from `weread.rs` to keep each
//! file under the task's 1200-line cap).

use super::download::WereadDownloads;
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use inkread_core::weread::mock::{MockTransport, MOCK_DEVICE_ID, MOCK_VID};

/// A transport that answers the QR long poll with a fixed body and
/// delegates everything else to the core mock.
struct ScriptedPoll {
    poll_body: &'static str,
    inner: MockTransport,
    sent: Mutex<Vec<String>>,
}

impl ScriptedPoll {
    fn new(poll_body: &'static str) -> Self {
        Self {
            poll_body,
            inner: MockTransport::new(),
            sent: Mutex::new(Vec::new()),
        }
    }
}

impl HttpTransport for ScriptedPoll {
    fn send(
        &self,
        request: HttpRequest,
    ) -> Result<inkread_core::sync::transport::HttpResponse, TransportError> {
        self.sent.lock().unwrap().push(request.url.clone());
        if request.url.contains("/connect/l/qrconnect") {
            return Ok(inkread_core::sync::transport::HttpResponse::new(
                200,
                self.poll_body.as_bytes().to_vec(),
            ));
        }
        self.inner.send(request)
    }
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-weread-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn login_with_mock(mock: MockTransport) -> Credentials {
    let driver = LoginDriver::new(mock).with_pause(Duration::ZERO);
    let ticket = driver.ticket().expect("ticket");
    let uuid = driver.qr_confirm(&ticket).expect("uuid");
    assert_eq!(
        driver.confirm_url(&uuid),
        "https://open.weixin.qq.com/connect/confirm?uuid=synthetic-uuid"
    );
    assert!(matches!(driver.poll(&uuid, 0), Ok(QrPollStatus::Continue)));
    let code = match driver.poll(&uuid, 408).expect("poll") {
        QrPollStatus::Confirmed(code) => code,
        other => panic!("expected a confirmation, got {other:?}"),
    };
    driver
        .exchange(
            &code,
            MOCK_DEVICE_ID,
            DEFAULT_DEVICE_NAME,
            "eink31000000000000000000000001",
        )
        .expect("exchange")
}

#[test]
fn the_qr_flow_reaches_credentials_on_the_core_mock() {
    let mock = MockTransport::new();
    let credentials = login_with_mock(mock.clone());
    assert_eq!(credentials.vid, MOCK_VID);
    assert!(credentials.access_token.starts_with("test-access-token"));
    let debug = format!("{credentials:?}");
    assert!(
        !debug.contains(&credentials.access_token),
        "leaked: {debug}"
    );
}

#[test]
fn an_expired_code_is_reported_and_not_retried_forever() {
    let transport = ScriptedPoll::new(r#"{"wx_errcode":402}"#);
    let driver = LoginDriver::new(transport).with_pause(Duration::ZERO);
    let ticket = driver.ticket().expect("ticket");
    let uuid = driver.qr_confirm(&ticket).expect("uuid");
    assert!(matches!(driver.poll(&uuid, 0), Ok(QrPollStatus::Expired)));

    let mut state = WereadState::new();
    state.login_generation = 1;
    for expected in [true, true, true] {
        assert_eq!(state.rotate_expired(), expected);
    }
    assert!(!state.rotate_expired(), "the fourth expiry stops the loop");
}

#[test]
fn manual_verification_and_rate_limit_are_not_retried() {
    for (status, code) in [(401, -2041), (200, -10102)] {
        let mock = MockTransport::new();
        mock.set_next_error(status, code, "injected");
        let (result, _) = fetch_shelf_with(mock.clone(), mock.credentials(), None);
        let error = result.expect_err("the injected error must surface");
        match code {
            -2041 => assert!(matches!(error, WeReadError::ManualVerification(-2041))),
            _ => assert!(matches!(error, WeReadError::RateLimited(-10102))),
        }
        assert_eq!(
            mock.request_count("/shelf/sync"),
            1,
            "code {code} must not be retried"
        );
        let expected = if code == -2041 {
            tr(Msg::WereadErrManual)
        } else {
            tr(Msg::WereadErrRate)
        };
        assert_eq!(error_message(&error), expected);
    }
}

#[test]
fn the_cursor_is_sent_and_kept() {
    let mock = MockTransport::new();
    let (result, _) = fetch_shelf_with(mock.clone(), mock.credentials(), Some(7));
    let shelf = result.expect("shelf");
    assert_eq!(shelf.synckey, Some(7));
    let request = mock
        .requests()
        .into_iter()
        .find(|request| request.url.contains("/shelf/sync"))
        .expect("shelf request");
    assert!(request.url.contains("synckey=7"), "{}", request.url);
}

#[test]
fn a_rotated_access_token_comes_back_with_the_shelf() {
    let mock = MockTransport::new();
    mock.invalidate_access_token();
    let old = mock.credentials().access_token;
    let (result, credentials) = fetch_shelf_with(mock.clone(), mock.credentials(), None);
    assert!(result.is_ok());
    assert_eq!(mock.refresh_count(), 1);
    assert_ne!(credentials.access_token, old);
}

#[test]
fn credentials_are_saved_owner_only_and_redacted() {
    let dir = tempdir("store");
    let mut state = WereadState::new();
    state.ensure_loaded(&dir);
    let credentials = Credentials::new("vid-secret", "access-secret", "refresh-secret", "dev-1");
    state.set_credentials(credentials);
    state.synckey = Some(11);
    state.save();

    let path = store_path(&dir);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the credentials file is owner-only");

    let mut loaded = WereadState::new();
    loaded.ensure_loaded(&dir);
    assert!(loaded.signed_in);
    assert_eq!(loaded.credentials().unwrap().vid, "vid-secret");
    assert_eq!(loaded.synckey, Some(11));

    let debug = format!("{state:?}");
    for secret in ["vid-secret", "access-secret", "refresh-secret", "dev-1"] {
        assert!(!debug.contains(secret), "debug leaked {secret}: {debug}");
    }
    assert!(debug.contains("[REDACTED]"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn terminal_auth_errors_drop_the_account_and_manual_checks_do_not() {
    assert!(auth_terminal(&WeReadError::AuthExpired));
    assert!(auth_terminal(&WeReadError::AuthRejected(-2012)));
    assert!(auth_terminal(&WeReadError::NotAuthenticated));
    assert!(auth_terminal(&WeReadError::HttpStatus(401, String::new())));
    assert!(auth_terminal(&WeReadError::ManualVerification(-2011)));
    assert!(!auth_terminal(&WeReadError::ManualVerification(-2041)));
    assert!(!auth_terminal(&WeReadError::RateLimited(-10102)));
    assert!(!auth_terminal(&WeReadError::Transport("timeout".into())));

    let dir = tempdir("forget");
    let mut state = WereadState::new();
    state.ensure_loaded(&dir);
    state.set_credentials(Credentials::new("vid", "access", "refresh", "dev"));
    state.save();
    state.forget_credentials();
    assert!(!state.signed_in);
    let mut loaded = WereadState::new();
    loaded.ensure_loaded(&dir);
    assert!(!loaded.signed_in);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn archive_groups_and_secrets_are_mapped() {
    use serde_json::json;
    let row = |archive: serde_json::Value| ShelfBook {
        book_id: "100001".to_string(),
        title: Some("Book".to_string()),
        format: Some("epub".to_string()),
        finished: Some(false),
        secret: Some(true),
        progress: Some(42),
        reading_time: Some(60),
        archive: Some(archive),
    };
    let books = vec![
        row(json!(["group"])),
        row(json!(0)),
        row(json!("")),
        row(json!(null)),
    ];
    let mapped = shelf_books(&books, &WereadDownloads::default());
    assert!(mapped[0].archive);
    assert!(!mapped[1].archive);
    assert!(!mapped[2].archive);
    assert!(!mapped[3].archive);
    assert!(mapped[0].secret);
    assert_eq!(mapped[0].progress, Some(42));
    assert_eq!(mapped[0].format, "epub");
}

// ---------------------------------------------------------------- download

use super::download::{
    choose_book_version, download_error_message, jump_annotation, run_download, DownloadFailure,
    DownloadParams, PositionStore,
};
use aristotle_i18n::with_language;
use aristotle_library::index::AnnotationKind;
use aristotle_ui::screens::weread_shelf::WereadDownloadState;
use inkread_core::sync::transport::HttpResponse;
use inkread_core::weread::{AnchorMap, BookPositions, PositionMap, WeReadCoordinate};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;

const FIXTURE_ZIP: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/fixtures/weread/chapter-package.zip.b64"
));

fn decode_base64(input: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
            other => panic!("invalid base64 byte {other}"),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    out
}

struct FixtureTransport {
    inner: MockTransport,
    zip: Vec<u8>,
    cancel: Arc<AtomicBool>,
    chapters: Arc<AtomicUsize>,
    cancel_after_first: bool,
}

impl HttpTransport for FixtureTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let url = request.url.clone();
        if url.contains("/book/chapterInfos") {
            let body = serde_json::json!({
                "data": [{
                    "bookId": "100001",
                    "synckey": 7,
                    "chapters": [
                        {"chapterUid": 1, "chapterIdx": 1, "title": "One",
                         "wordCount": 100, "price": 0, "paid": 0,
                         "files": ["Text/ch1.xhtml"]},
                        {"chapterUid": 2, "chapterIdx": 2, "title": "Two",
                         "wordCount": 100, "price": 0, "paid": 0,
                         "files": ["Text/ch2.xhtml"]}
                    ]
                }]
            });
            return Ok(HttpResponse::new(200, serde_json::to_vec(&body).unwrap()));
        }
        if url.contains("/book/getProgress") {
            let body = serde_json::json!({
                "bookId": "100001",
                "book": {
                    "appId": "synthetic-app",
                    "bookVersion": 42,
                    "chapterUid": 2,
                    "chapterIdx": 2,
                    "chapterOffset": 5,
                    "summary": "synthetic",
                    "progress": 50
                }
            });
            return Ok(HttpResponse::new(200, serde_json::to_vec(&body).unwrap()));
        }
        let chapter = url.contains("/book/chapterdownload");
        if chapter {
            let previous = self.chapters.fetch_add(1, Ordering::SeqCst);
            if self.cancel_after_first && previous == 0 {
                self.cancel.store(true, Ordering::SeqCst);
            }
        }
        let response = self.inner.send(request)?;
        if chapter {
            return Ok(HttpResponse::new(200, self.zip.clone())
                .with_header("encryptkey", self.inner.envelope()));
        }
        Ok(response)
    }
}

fn download_params(dir: &Path) -> DownloadParams {
    DownloadParams {
        book_id: "100001".to_string(),
        title: "Synthetic Book".to_string(),
        cache_dir: dir.join("state/weread/cache/100001"),
        epub_path: dir.join("root/WeRead/100001.epub"),
        mapping_path: dir.join("state/weread/books.json"),
    }
}

#[test]
fn the_mock_pipeline_cancels_resumes_and_maps_the_cloud_position() {
    let dir = tempdir("download");
    let params = download_params(&dir);
    std::fs::create_dir_all(params.epub_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(params.mapping_path.parent().unwrap()).unwrap();
    let zip = decode_base64(FIXTURE_ZIP);

    let cancel = Arc::new(AtomicBool::new(false));
    let chapters = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(AtomicUsize::new(0));
    let progress_seen = progress.clone();
    let transport = FixtureTransport {
        inner: MockTransport::new(),
        zip: zip.clone(),
        cancel: cancel.clone(),
        chapters: chapters.clone(),
        cancel_after_first: true,
    };
    let result = run_download(
        MockTransport::new().credentials(),
        &params,
        transport,
        cancel,
        move |_, _| {
            progress_seen.fetch_add(1, Ordering::SeqCst);
        },
    );
    assert!(matches!(result, Err(DownloadFailure::Cancelled)));
    assert!(params.cache_dir.join("packages/1").is_dir());
    assert!(!params.epub_path.exists(), "a cancel commits no book");
    assert_eq!(chapters.load(Ordering::SeqCst), 1);
    assert!(
        progress.load(Ordering::SeqCst) >= 1,
        "chapter progress was reported"
    );

    let cancel = Arc::new(AtomicBool::new(false));
    let chapters = Arc::new(AtomicUsize::new(0));
    let transport = FixtureTransport {
        inner: MockTransport::new(),
        zip,
        cancel: cancel.clone(),
        chapters: chapters.clone(),
        cancel_after_first: false,
    };
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_seen = progress.clone();
    let downloaded = run_download(
        MockTransport::new().credentials(),
        &params,
        transport,
        cancel,
        move |done, total| progress_seen.lock().unwrap().push((done, total)),
    )
    .expect("the resumed pipeline commits the book");
    assert_eq!(
        progress.lock().unwrap().last().copied(),
        Some((2, 2)),
        "a resumed download reaches 100% before the completion event"
    );
    assert_eq!(
        downloaded.outcome.cached,
        vec![1],
        "chapter 1 came from cache"
    );
    assert_eq!(
        downloaded.outcome.downloaded,
        vec![2],
        "only the missing chapter was requested"
    );
    assert_eq!(chapters.load(Ordering::SeqCst), 1);
    assert!(params.epub_path.is_file());
    assert!(params.mapping_path.is_file());

    let mapped = downloaded
        .positions
        .map
        .to_epub(WeReadCoordinate {
            chapter_uid: 2,
            chapter_idx: 2,
            chapter_offset: 5,
        })
        .unwrap();
    assert_eq!(mapped.position.spine_index, 1);
    assert_eq!(mapped.position.char_offset, 5);
    let target = downloaded
        .positions
        .map
        .xpointer_for(
            WeReadCoordinate {
                chapter_uid: 2,
                chapter_idx: 2,
                chapter_offset: 5,
            },
            Some(&downloaded.positions.anchors),
        )
        .unwrap();
    assert!(
        target.xpointer.contains("DocFragment[2]"),
        "{}",
        target.xpointer
    );
    assert!(target.xpointer.ends_with(".5"), "{}", target.xpointer);

    // The open path consumes exactly this jump: an annotation the existing
    // `Session::prepare` resolves through crengine's `location_of`.
    let annotation = jump_annotation(&target.xpointer);
    let jump = crate::marks::target(&annotation, aristotle_library::Format::Epub, None)
        .expect("the cloud XPointer is a valid open jump");
    assert_eq!(
        jump,
        aristotle_render::book::Position::XPointer {
            xpointer: target.xpointer
        }
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn sample_positions() -> BookPositions {
    BookPositions {
        map: PositionMap {
            book_id: "100001".into(),
            book_version: "42".into(),
            chapters: Vec::new(),
        },
        anchors: AnchorMap {
            book_id: "100001".into(),
            chapters: Vec::new(),
        },
    }
}

#[test]
fn the_position_store_is_atomic_and_recovers_its_backup() {
    let dir = tempdir("positions");
    let path = dir.join("positions.json");
    let mut store = PositionStore::load(path.clone());
    store.insert("100001", sample_positions());
    store.save().unwrap();
    store.save().unwrap();
    assert!(!dir.join("positions.json.part").exists());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    std::fs::write(&path, b"not json").unwrap();
    let loaded = PositionStore::load(path);
    assert!(
        loaded.get("100001").is_some(),
        "the .bak generation is used after a torn write"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn download_failures_have_words_in_both_languages() {
    with_language("en", || {
        assert!(download_error_message(&DownloadFailure::UnsupportedFormat).contains("format"));
        assert!(download_error_message(&DownloadFailure::Cancelled).contains("cancelled"));
        assert!(download_error_message(&DownloadFailure::NoEligibleChapters).contains("chapters"));
        assert!(download_error_message(&DownloadFailure::MissingBookVersion).contains("version"));
    });
    with_language("zh", || {
        assert!(download_error_message(&DownloadFailure::UnsupportedFormat).contains("格式"));
        assert!(download_error_message(&DownloadFailure::Cancelled).contains("取消"));
        assert!(download_error_message(&DownloadFailure::NoEligibleChapters).contains("章节"));
    });
}

#[test]
fn a_cloud_xpointer_becomes_an_open_jump_annotation() {
    let annotation = jump_annotation("/body/DocFragment[2]/body/p[1]/text().5");
    assert_eq!(annotation.kind, AnnotationKind::Bookmark);
    let target = crate::marks::target(&annotation, aristotle_library::Format::Epub, None)
        .expect("the annotation decodes");
    assert_eq!(
        target,
        aristotle_render::book::Position::XPointer {
            xpointer: "/body/DocFragment[2]/body/p[1]/text().5".into()
        }
    );
}

#[test]
fn opening_a_downloaded_card_decides_between_download_index_and_cloud() {
    use super::download::{OpenAction, WereadDownloads};
    use aristotle_net::Policy;
    use inkread_core::weread::{BookMapping, BookMappingStore};

    let dir = tempdir("open");
    let state = dir.join("state");
    let root = dir.join("root");
    std::fs::create_dir_all(state.join("weread")).unwrap();
    std::fs::create_dir_all(root.join("WeRead")).unwrap();
    let mapping_path = state.join("weread/books.json");
    let mut mapping = BookMappingStore::load(&mapping_path).unwrap();
    mapping.insert(
        "100001",
        BookMapping {
            hash: "hash-100001".into(),
            title: "Synthetic Book".into(),
            format: "epub".into(),
            synckey: None,
        },
    );
    mapping.save().unwrap();
    let mut positions = PositionStore::load(state.join("weread/positions.json"));
    positions.insert("100001", sample_positions());
    positions.save().unwrap();
    std::fs::write(root.join("WeRead/100001.epub"), b"not parsed here").unwrap();

    let mut downloads = WereadDownloads::default();
    downloads.ensure_loaded(&state);
    assert_eq!(downloads.state("100001"), WereadDownloadState::Downloaded);
    assert_eq!(downloads.hash_of("100001").as_deref(), Some("hash-100001"));

    assert!(matches!(
        downloads.open(&state, &root, "unknown", None, Policy::open(), true),
        OpenAction::NeedDownload
    ));
    assert!(matches!(
        downloads.open(&state, &root, "100001", None, Policy::open(), false),
        OpenAction::AwaitingIndex { .. }
    ));
    assert!(matches!(
        downloads.open(&state, &root, "100001", None, Policy::open(), true),
        OpenAction::NoCredentials
    ));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_map_reload_keeps_the_in_memory_hash() {
    use inkread_core::weread::{BookMapping, BookMappingStore};

    let dir = tempdir("map-reload");
    let state = dir.join("state");
    std::fs::create_dir_all(state.join("weread")).unwrap();
    let path = state.join("weread/books.json");
    let mut mapping = BookMappingStore::load(&path).unwrap();
    mapping.insert(
        "100001",
        BookMapping {
            hash: "hash-100001".into(),
            title: "Synthetic Book".into(),
            format: "epub".into(),
            synckey: None,
        },
    );
    mapping.save().unwrap();

    let mut downloads = WereadDownloads::default();
    downloads.ensure_loaded(&state);
    assert_eq!(downloads.hash_of("100001").as_deref(), Some("hash-100001"));

    // The reload after a download fails. The card still says "Downloaded",
    // so the old hash has to stay usable or the next open downloads again.
    std::fs::write(&path, b"{not json").unwrap();
    downloads.reload_mapping();
    assert_eq!(downloads.hash_of("100001").as_deref(), Some("hash-100001"));
    assert_eq!(downloads.state("100001"), WereadDownloadState::Downloaded);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_book_version_falls_back_to_the_toc_then_its_synckey() {
    use inkread_core::weread::ChapterInfos;

    let mut toc = ChapterInfos {
        book_id: "100001".into(),
        synckey: Some(9),
        book_version: Some(7),
        chapters: Vec::new(),
    };
    assert_eq!(
        choose_book_version(Some(42), &toc).as_deref(),
        Some("42"),
        "getProgress.book.bookVersion wins"
    );
    assert_eq!(
        choose_book_version(None, &toc).as_deref(),
        Some("7"),
        "chapterInfos.book.version is the first fallback"
    );
    toc.book_version = None;
    assert_eq!(
        choose_book_version(None, &toc).as_deref(),
        Some("9"),
        "chapterInfos.synckey is the last fallback"
    );
    toc.synckey = None;
    assert_eq!(choose_book_version(None, &toc), None);
}

#[test]
fn a_server_book_id_cannot_escape_the_download_directories() {
    use super::download::safe_book_id;
    assert_eq!(safe_book_id("100001"), "100001");
    assert_eq!(safe_book_id("../../etc/passwd"), "etcpasswd");
    assert_eq!(safe_book_id(".."), "book");
}
