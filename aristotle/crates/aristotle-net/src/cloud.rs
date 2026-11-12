// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming a Readest cloud book into the library folder.
//!
//! `GET /api/storage/download?fileKey=...` gives a signed URL; the bytes are
//! streamed by [`crate::http::Client::download`] into a hidden `.part` file
//! beside the destination, never into memory. `core::cloud` checks the
//! partial MD5 before the file is renamed into place, and every failure
//! removes the partial file. An expired ticket (HTTP 403 or 410) is renewed
//! and the download retried, twice. A signed URL that [`crate::endpoint`]
//! refuses (plain HTTP outside loopback and the private IPv4 ranges), or a
//! `book_hash` that is not a plain name, is refused before anything is
//! requested.
//!
//! The signed URL's host is not known until the account answers, so the
//! download allows it on a copy of the device policy for this request only.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use inkread_core::cloud::{
    format_to_extension, request_download_ticket, resolve_book_file_key, verify_download_hash,
    CloudBook, CloudError, DownloadTicket, StorageBackend,
};

use crate::download;
use crate::http::{Client, Error as HttpError, Request, Timeouts};
use crate::policy::Policy;
use crate::readest::ReadestTransport;
use crate::url::Url;

/// The largest cloud book downloaded.
pub const MAX_BOOK: u64 = download::MAX_BOOK;

/// A failed cloud download.
#[derive(Debug)]
pub enum DownloadError {
    Cloud(CloudError),
    Http(HttpError),
    /// The server's `book_hash` is not a plain name: it is used in a path.
    UnsafeHash,
    /// The signed URL is not an endpoint the device may talk to: plain HTTP
    /// outside loopback and the private IPv4 ranges.
    InsecureUrl,
}

impl From<CloudError> for DownloadError {
    fn from(e: CloudError) -> Self {
        DownloadError::Cloud(e)
    }
}

impl From<HttpError> for DownloadError {
    fn from(e: HttpError) -> Self {
        DownloadError::Http(e)
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        DownloadError::Cloud(CloudError::Io(e))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Stream `book` into `dir`, verify `book_hash`, and rename it into place.
///
/// `progress` receives `(bytes so far, total if known)`. The returned path is
/// the final, unique file name in `dir`; no `.part` file remains.
#[allow(clippy::too_many_arguments)]
pub fn download_book(
    policy: Policy,
    api_base: &str,
    token: &str,
    user_id: &str,
    book: &CloudBook,
    dir: &Path,
    limit: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, DownloadError> {
    download(
        policy, api_base, token, user_id, book, dir, limit, cancel, progress,
    )
}

/// The server's `book_hash` as a file-name tag: letters and digits only, at
/// most 64 of them. The value comes from the account's answer and ends up in
/// a path, so a slash, `..` or anything else is refused before any byte is
/// requested.
fn hash_tag(hash: &str) -> Result<String, DownloadError> {
    if hash.is_empty() || hash.len() > 64 || !hash.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(DownloadError::UnsafeHash);
    }
    Ok(hash.to_ascii_lowercase())
}

#[allow(clippy::too_many_arguments)]
fn download(
    policy: Policy,
    api_base: &str,
    token: &str,
    user_id: &str,
    book: &CloudBook,
    dir: &Path,
    limit: u64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, DownloadError> {
    // Before the account is asked for a ticket: nothing about this book can
    // name a file outside the library folder.
    let hash = hash_tag(&book.book_hash)?;
    let transport = ReadestTransport::new(policy.clone());
    let file_key = resolve_book_file_key(
        &transport,
        api_base,
        token,
        user_id,
        book,
        StorageBackend::R2,
    );
    let ext = format_to_extension(&book.format);
    let stem = download::safe_name(&book.title, 120, &hash);
    let part = dir.join(format!(".{hash}.{ext}.part"));

    fs::create_dir_all(dir)?;

    let mut ticket: Option<DownloadTicket> = None;
    let mut renewals = 0u8;
    loop {
        let now = now_ms();
        if ticket
            .as_ref()
            .is_none_or(|t| t.is_expired(now, DownloadTicket::DEFAULT_MARGIN_MS))
        {
            ticket = Some(request_download_ticket(
                &transport, api_base, token, &file_key, now,
            )?);
        }
        let signed = ticket.as_ref().expect("ticket requested above");

        let url = Url::parse(&signed.url).map_err(HttpError::Url)?;
        if !crate::endpoint::is_allowed(&url) {
            return Err(DownloadError::InsecureUrl);
        }
        let mut signed_policy = policy.clone();
        signed_policy.allow(&url);
        let client = Client::new(
            signed_policy,
            Timeouts {
                connect: Duration::from_secs(15),
                response: Duration::from_secs(30),
                body: Duration::from_secs(300),
            },
        );

        let mut file = File::create(&part)?;
        let result = client
            .download(&Request::get(url), &mut file, limit, cancel, progress)
            .and_then(|_| {
                file.flush()?;
                file.sync_all()?;
                Ok(())
            });
        drop(file);

        match result {
            Ok(()) => {
                verify_download_hash(&part, &book.book_hash)?;
                let path = download::unique_path(dir, &stem, ext);
                if let Err(e) = fs::rename(&part, &path) {
                    let _ = fs::remove_file(&part);
                    return Err(e.into());
                }
                return Ok(path);
            }
            Err(HttpError::Status(403 | 410, _)) if renewals < 2 => {
                let _ = fs::remove_file(&part);
                renewals += 1;
                ticket = None;
            }
            Err(e) => {
                let _ = fs::remove_file(&part);
                return Err(e.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, Server};
    use inkread_core::hash::partial_md5;
    use std::io::Cursor;
    use std::sync::atomic::Ordering;

    fn temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join("aristotle-net-cloud-tests")
            .join(format!("{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn book_for(bytes: &[u8]) -> (CloudBook, String) {
        let hash = partial_md5(&mut Cursor::new(bytes.to_vec())).unwrap();
        let book = CloudBook {
            book_hash: hash.clone(),
            title: "Test Book".into(),
            source_title: None,
            author: "Tester".into(),
            format: "EPUB".into(),
            progress: None,
            cover_hash: None,
            uploaded_at: Some("2026-11-09T00:00:00Z".into()),
            deleted_at: None,
            file_key: format!("user-1/Readest/Books/{hash}/Test Book.epub"),
            downloaded: false,
            meta_hash: None,
            updated_at: None,
        };
        (book, hash)
    }

    fn ticket_server(body: Vec<u8>, expired_first: bool) -> Server {
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Server::start(move |r| {
            if r.path.starts_with("/api/storage/list") {
                return Reply::text(
                    200,
                    r#"{"files":[],"total":0,"page":1,"pageSize":100,"totalPages":0}"#,
                );
            }
            if r.path.starts_with("/api/storage/download") {
                let host = r.header("host").unwrap_or("127.0.0.1");
                return Reply::text(
                    200,
                    &format!(r#"{{"downloadUrl":"http://{host}/book.epub"}}"#),
                );
            }
            if r.path == "/book.epub" {
                if expired_first && hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Reply::text(403, "expired");
                }
                return Reply::typed(200, "application/epub+zip", &body);
            }
            Reply::text(404, "gone")
        })
    }

    fn ticket_server_with_signed_url(signed_url: &'static str) -> Server {
        Server::start(move |r| {
            if r.path.starts_with("/api/storage/list") {
                return Reply::text(
                    200,
                    r#"{"files":[],"total":0,"page":1,"pageSize":100,"totalPages":0}"#,
                );
            }
            if r.path.starts_with("/api/storage/download") {
                return Reply::text(200, &format!(r#"{{"downloadUrl":"{signed_url}"}}"#));
            }
            Reply::text(404, "gone")
        })
    }

    #[test]
    fn streams_verifies_and_renames_without_a_part_file() {
        let bytes = b"an epub that is streamed and checked".repeat(200);
        let (book, _) = book_for(&bytes);
        // A loopback `http` signed URL is the same allowance a device uses for a
        // mock server on its own network: the download succeeds.
        let server = ticket_server(bytes.clone(), false);
        let dir = temp_dir("stream");
        let mut reported = 0u64;
        let path = download_book(
            Policy::open(),
            &format!("http://127.0.0.1:{}/api", server.url("/").port()),
            "token",
            "user-1",
            &book,
            &dir,
            MAX_BOOK,
            &AtomicBool::new(false),
            &mut |bytes, _| reported = bytes,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(path, dir.join("Test Book.epub"));
        assert_eq!(reported as usize, bytes.len());
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["Test Book.epub".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hash_mismatch_removes_the_partial_file() {
        let bytes = b"downloaded bytes".to_vec();
        let (mut book, _) = book_for(&bytes);
        book.book_hash = "00000000000000000000000000000000".into();
        let server = ticket_server(bytes, false);
        let dir = temp_dir("mismatch");
        let err = download_book(
            Policy::open(),
            &format!("http://127.0.0.1:{}/api", server.url("/").port()),
            "token",
            "user-1",
            &book,
            &dir,
            MAX_BOOK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(
            err,
            DownloadError::Cloud(CloudError::HashMismatch { .. })
        ));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hash_that_names_a_path_is_refused_before_any_request() {
        let bytes = b"never downloaded".to_vec();
        let (mut book, _) = book_for(&bytes);
        book.book_hash = "../../x".into();
        let server = ticket_server(bytes, false);
        let dir = temp_dir("unsafe-hash");
        let err = download_book(
            Policy::open(),
            &format!("http://127.0.0.1:{}/api", server.url("/").port()),
            "token",
            "user-1",
            &book,
            &dir,
            MAX_BOOK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, DownloadError::UnsafeHash));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        assert_eq!(server.hits("/api/storage/download"), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_tags_keep_only_plain_names() {
        assert_eq!(hash_tag("AbC123").unwrap(), "abc123");
        for bad in [
            "",
            "../../x",
            "a/b",
            "a.b",
            "hash with space",
            &"a".repeat(65),
        ] {
            assert!(
                matches!(hash_tag(bad), Err(DownloadError::UnsafeHash)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_public_http_signed_url_is_refused() {
        let bytes = b"plain transport".to_vec();
        let (book, _) = book_for(&bytes);
        let server = ticket_server_with_signed_url("http://example.com/book.epub");
        let dir = temp_dir("insecure");
        let err = download_book(
            Policy::open(),
            &format!("http://127.0.0.1:{}/api", server.url("/").port()),
            "token",
            "user-1",
            &book,
            &dir,
            MAX_BOOK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(matches!(err, DownloadError::InsecureUrl));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_ticket_is_renewed_and_the_download_retried() {
        let bytes = b"retry after a 403".to_vec();
        let (book, _) = book_for(&bytes);
        let server = ticket_server(bytes.clone(), true);
        let dir = temp_dir("renew");
        let path = download_book(
            Policy::open(),
            &format!("http://127.0.0.1:{}/api", server.url("/").port()),
            "token",
            "user-1",
            &book,
            &dir,
            MAX_BOOK,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(server.hits("/book.epub"), 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
