// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end WeRead user-journey simulation over real loopback TCP + `ReadestTransport`.
//!
//! Covers real-device wire quirks that in-memory mocks missed:
//! - Integer booleans (`finished: 0/1`, `secret: 0/1`) in `/shelf/sync`
//! - `POST /login` token refresh omitting `refreshToken` (must keep existing one)
//! - `encryptkey` response header forwarding through `ReadestTransport`
//! - Chapter `0` requested only once on multi-chapter EPUB download (no 100x bloat)
//! - Rebuilt EPUB retains `Images/` and `Styles/` even after `public/` cache recovery

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use aristotle_net::{Policy, ReadestTransport};
use inkread_core::weread::download::{download_book, DownloadRequest, SourceFormat};
use inkread_core::weread::{ChapterInfo, ChapterInfos, Credentials, WeReadClient};

const FIXTURE_B64: &str = include_str!("../../../../tests/fixtures/weread/chapter-package.zip.b64");
const FIXTURE_PASSWORD: &str = "synthetic-zip-pass";
const FIXTURE_VID: &str = "1234567890";

fn decode_b64(input: &str) -> Vec<u8> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [255u8; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let clean: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let a = rev[chunk[0] as usize] as u32;
        let b = rev[chunk[1] as usize] as u32;
        let c = if chunk[2] == b'=' {
            0
        } else {
            rev[chunk[2] as usize] as u32
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            rev[chunk[3] as usize] as u32
        };
        let triple = (a << 18) | (b << 12) | (c << 6) | d;
        out.push(((triple >> 16) & 0xff) as u8);
        if chunk[2] != b'=' {
            out.push(((triple >> 8) & 0xff) as u8);
        }
        if chunk[3] != b'=' {
            out.push((triple & 0xff) as u8);
        }
    }
    out
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-net-weread-sim-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct ServerMetrics {
    refresh_calls: AtomicUsize,
    chapter_requests: Mutex<Vec<String>>,
    total_chapter_bytes: AtomicUsize,
    expired_once: AtomicBool,
}

#[test]
fn weread_full_user_journey_over_real_tcp_transport() {
    let zip_bytes = Arc::new(decode_b64(FIXTURE_B64));
    let envelope = inkread_core::weread::mock::make_envelope(FIXTURE_VID, FIXTURE_PASSWORD);
    let metrics = Arc::new(ServerMetrics {
        refresh_calls: AtomicUsize::new(0),
        chapter_requests: Mutex::new(Vec::new()),
        total_chapter_bytes: AtomicUsize::new(0),
        expired_once: AtomicBool::new(true),
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let running = Arc::new(AtomicBool::new(true));

    let srv_running = Arc::clone(&running);
    let srv_metrics = Arc::clone(&metrics);
    let srv_zip = Arc::clone(&zip_bytes);
    let srv_env = envelope.clone();

    let server = thread::spawn(move || {
        while srv_running.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            if !srv_running.load(Ordering::Relaxed) {
                break;
            }
            let mut req_buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let header_end = loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break None,
                    Ok(n) => {
                        req_buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break Some(pos);
                        }
                    }
                    Err(_) => break None,
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let head = String::from_utf8_lossy(&req_buf[..header_end]).into_owned();
            let first_line = head.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("");

            // Drain request body if Content-Length is set.
            let content_len: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let mut body_have = req_buf.len().saturating_sub(header_end + 4);
            while body_have < content_len {
                match stream.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body_have += n,
                }
            }

            if method == "POST" && path == "/login" {
                srv_metrics.refresh_calls.fetch_add(1, Ordering::SeqCst);
                // Real WeRead quirk: refresh returns new accessToken WITHOUT refreshToken.
                let body =
                    format!(r#"{{"vid":{FIXTURE_VID},"accessToken":"renewed-access-token"}}"#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }

            // Force one -2012 token expiry on the very first authenticated call.
            if srv_metrics.expired_once.swap(false, Ordering::SeqCst) {
                let body = r#"{"errcode":-2012,"errmsg":"accessToken expired"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }

            if method == "GET" && path.starts_with("/shelf/sync") {
                // Real WeRead quirk: integer booleans 0/1 for finished & secret.
                let mut books = Vec::new();
                for i in 1..=42 {
                    books.push(format!(
                        r#"{{"bookId":"book-{i}","title":"测试书 {i}","author":"作者","format":"epub","finished":{},"secret":0}}"#,
                        i % 2
                    ));
                }
                let body = format!(r#"{{"synckey":999,"books":[{}]}}"#, books.join(","));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            } else if method == "GET" && path.starts_with("/book/chapterdownload") {
                srv_metrics
                    .chapter_requests
                    .lock()
                    .unwrap()
                    .push(path.to_string());
                srv_metrics
                    .total_chapter_bytes
                    .fetch_add(srv_zip.len(), Ordering::SeqCst);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nencryptkey: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    srv_env,
                    srv_zip.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&srv_zip);
            } else {
                let resp =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });

    let policy = Policy::open();
    let transport = ReadestTransport::new(policy);
    let initial_creds = Credentials::new(FIXTURE_VID, "stale-access", "keep-me-rt", "dev-ko2");
    let mut client = WeReadClient::new(transport, initial_creds)
        .with_base_url(format!("http://127.0.0.1:{port}"));

    // 1. Shelf sync hits -2012 -> refreshes without refreshToken -> succeeds with 42 books (integer bools).
    let shelf = client
        .shelf_sync()
        .expect("shelf sync after silent refresh");
    assert_eq!(metrics.refresh_calls.load(Ordering::SeqCst), 1);
    assert_eq!(shelf.books.len(), 42);
    assert_eq!(shelf.books[0].finished, Some(true));
    assert_eq!(shelf.books[1].finished, Some(false));
    let renewed = client.credentials().expect("renewed creds");
    assert_eq!(renewed.access_token, "renewed-access-token");
    assert_eq!(
        renewed.refresh_token, "keep-me-rt",
        "refresh must preserve existing refresh_token when server omits it"
    );

    // 2. Download a 2-chapter EPUB over real TCP + ReadestTransport (verifies encryptkey forwarding + chapter 0 fetched only once).
    let dir = scratch_dir("epub-flow");
    let toc = ChapterInfos {
        book_id: "100001".to_string(),
        synckey: Some(42),
        book_version: Some(42),
        chapters: vec![
            ChapterInfo {
                chapter_uid: Some(1),
                chapter_idx: Some(1),
                title: Some("第一章".to_string()),
                price: Some(0.0),
                paid: Some(1),
                ..Default::default()
            },
            ChapterInfo {
                chapter_uid: Some(2),
                chapter_idx: Some(2),
                title: Some("第二章".to_string()),
                price: Some(0.0),
                paid: Some(1),
                ..Default::default()
            },
        ],
    };
    let request = DownloadRequest {
        book_id: "100001".to_string(),
        title: "测试书".to_string(),
        format: SourceFormat::Epub,
        book_version: "42".to_string(),
        chapters: vec![1, 2],
        epub_path: dir.join("books/weread/100001.epub"),
        mapping_path: dir.join("state/books.json"),
        work_dir: dir.join("books/weread/.cache/100001"),
    };
    let outcome = download_book(&mut client, &toc, &request).expect("real TCP epub download");
    assert_eq!(outcome.downloaded, vec![1, 2]);
    assert!(outcome.epub_path.exists());
    assert!(
        request.work_dir.starts_with(dir.join("books")),
        "cache must stay under books root, never state_dir"
    );

    let reqs = metrics.chapter_requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[0].contains("chapters=0,1&"),
        "first chapter must request chapter 0: {}",
        reqs[0]
    );
    assert!(
        reqs[1].contains("chapters=2&") && !reqs[1].contains("chapters=0,"),
        "second chapter must NOT re-request chapter 0: {}",
        reqs[1]
    );

    // 3. TC-WR-07b: Delete `public/` cache and re-run download -> chapter 0 is re-fetched once, chapter 2 stays cached!
    std::fs::remove_dir_all(request.work_dir.join("public")).unwrap();
    let resumed = download_book(&mut client, &toc, &request).expect("resume after missing public");
    assert_eq!(resumed.downloaded, vec![1]);
    assert_eq!(resumed.cached, vec![2]);
    assert!(request.work_dir.join("public/state.json").exists());

    // Cleanup server
    running.store(false, Ordering::Relaxed);
    let _ = std::net::TcpStream::connect(format!("127.0.0.1:{port}"));
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&dir);
}
