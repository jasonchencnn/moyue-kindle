//! WebDAV client for the Jianguoyun channel (contract §15.1–§15.5).
//!
//! The client builds [`HttpRequest`]s and hands them to any [`HttpTransport`]; the platform keeps
//! its network policy and TLS stack, and this module stays free of I/O and global state.

use std::fmt;

use crate::jianguoyun::merge::{decide_progress, merge_book_sync, ProgressDecision};
use crate::jianguoyun::model::{
    JianguoyunBookSync, JianguoyunConfig, JianguoyunError, RemoteEntry, SYNC_SCHEMA_VERSION,
};
use crate::jianguoyun::xml::{parse_propfind_response, PROPFIND_BODY};
use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport};

/// `User-Agent` sent with every WebDAV request.
pub const USER_AGENT: &str = "Moyue-Kindle/0.1 (+https://github.com/moyue-reader)";
/// `Content-Type` of the `PROPFIND` body.
pub const PROPFIND_CONTENT_TYPE: &str = "application/xml; charset=utf-8";
/// `Content-Type` of a sync record.
pub const SYNC_CONTENT_TYPE: &str = "application/json";

/// Parse the delta-seconds form of `Retry-After`; HTTP-date values yield `None`.
pub fn parse_retry_after(value: &str) -> Option<u64> {
    value.trim().parse::<u64>().ok()
}

/// Synchronous WebDAV client for one Jianguoyun account.
pub struct JianguoyunClient<T: HttpTransport> {
    config: JianguoyunConfig,
    transport: T,
}

impl<T: HttpTransport + Clone> Clone for JianguoyunClient<T> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            transport: self.transport.clone(),
        }
    }
}

impl<T: HttpTransport> fmt::Debug for JianguoyunClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JianguoyunClient")
            .field("config", &self.config)
            .field("authorization", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl<T: HttpTransport> JianguoyunClient<T> {
    /// Create a client around an injected transport.
    pub fn new(config: JianguoyunConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// The configuration the client was built with.
    pub fn config(&self) -> &JianguoyunConfig {
        &self.config
    }

    /// The injected transport, e.g. to inspect a mock in tests.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Call `PROPFIND Depth: 0` on the endpoint to verify credentials and WebDAV access.
    pub fn test_connection(&self) -> Result<(), JianguoyunError> {
        let url = self.config.normalized_base_url()?;
        let response = self.send(self.propfind_request(&url, "0"))?;
        if is_success(response.status) {
            Ok(())
        } else {
            Err(status_error(&response))
        }
    }

    /// List one collection with `PROPFIND Depth: 1`.
    ///
    /// `rel_path` is a decoded path relative to the configured endpoint (`""` lists the root).
    /// Hidden entries, `*.sdr` folders and non-ebook files are filtered, and directories sort
    /// before books (contract §15.2).
    pub fn list_dir(&self, rel_path: &str) -> Result<Vec<RemoteEntry>, JianguoyunError> {
        let url = self.config.join_remote_path(rel_path)?;
        let target = if url.ends_with('/') {
            url
        } else {
            format!("{url}/")
        };
        let response = self.send(self.propfind_request(&target, "1"))?;
        if !is_success(response.status) {
            return Err(status_error(&response));
        }
        let body = String::from_utf8_lossy(&response.body);
        parse_propfind_response(&body, &target)
    }

    /// Report whether a remote path exists, using `HEAD`.
    pub fn exists(&self, rel_path: &str) -> Result<bool, JianguoyunError> {
        let url = self.config.join_remote_path(rel_path)?;
        let response = self.send(self.build_request("HEAD", &url, None))?;
        match response.status {
            404 => Ok(false),
            status if is_success(status) => Ok(true),
            _ => Err(status_error(&response)),
        }
    }

    /// Download and parse one book's sync record.
    ///
    /// A missing file is not an error: `404` returns `Ok(None)` so the caller can seed the remote
    /// copy. A record that names another book or a newer schema is rejected instead of being
    /// merged, because rewriting it would silently drop data this client cannot represent.
    pub fn pull_book_sync(
        &self,
        book_hash: &str,
    ) -> Result<Option<JianguoyunBookSync>, JianguoyunError> {
        let url = self.config.sync_file_path(book_hash)?;
        let response = self.send(self.build_request("GET", &url, None))?;
        if response.status == 404 {
            return Ok(None);
        }
        if !is_success(response.status) {
            return Err(status_error(&response));
        }
        let record: JianguoyunBookSync = serde_json::from_slice(&response.body)
            .map_err(|error| JianguoyunError::JsonParse(error.to_string()))?;
        if !record.book_hash.eq_ignore_ascii_case(book_hash.trim()) {
            return Err(JianguoyunError::JsonParse(format!(
                "record belongs to `{}`, not `{book_hash}`",
                record.book_hash
            )));
        }
        if record.version > SYNC_SCHEMA_VERSION {
            return Err(JianguoyunError::JsonParse(format!(
                "record schema version {} is newer than {SYNC_SCHEMA_VERSION}",
                record.version
            )));
        }
        Ok(Some(record))
    }

    /// Upload one book's sync record, creating the `.moyue/sync/` collections when needed.
    ///
    /// A `404`/`409` on the first `PUT` triggers `MKCOL .moyue/`, `MKCOL .moyue/sync/` and
    /// exactly one retry; `405`/`301` on `MKCOL` means the collection already exists (contract
    /// §15.4). `429`/`503` are returned as [`JianguoyunError::RateLimited`] without any retry, so
    /// the free-tier quota is never burned twice.
    pub fn push_book_sync(&self, record: &JianguoyunBookSync) -> Result<(), JianguoyunError> {
        let url = self.config.sync_file_path(&record.book_hash)?;
        let body = serde_json::to_vec(record)
            .map_err(|error| JianguoyunError::JsonParse(error.to_string()))?;
        let response = self.send(self.put_request(&url, &body))?;
        if is_success(response.status) {
            return Ok(());
        }
        if !matches!(response.status, 404 | 409) {
            return Err(status_error(&response));
        }
        self.ensure_sync_collections()?;
        let retry = self.send(self.put_request(&url, &body))?;
        if is_success(retry.status) {
            Ok(())
        } else {
            Err(status_error(&retry))
        }
    }

    /// Pull, merge and conditionally push one book, returning the converged record.
    ///
    /// The returned [`ProgressDecision`] tells the reader whether it should jump to a remote
    /// position (`PullRemote`) or upload its own (`PushLocal`); `InSync` means both devices
    /// already agree. A notes-only difference still pushes, because deletions must propagate.
    pub fn sync_book(
        &self,
        local: &JianguoyunBookSync,
    ) -> Result<(JianguoyunBookSync, ProgressDecision), JianguoyunError> {
        let remote = self.pull_book_sync(&local.book_hash)?;
        let decision = decide_progress(
            local.progress.as_ref(),
            remote.as_ref().and_then(|record| record.progress.as_ref()),
        );
        let merged = match remote.as_ref() {
            Some(remote) => merge_book_sync(local, remote),
            None => local.clone(),
        };
        let should_push = match remote.as_ref() {
            None => true,
            Some(remote) => merged != *remote,
        };
        if should_push {
            self.push_book_sync(&merged)?;
        }
        Ok((merged, decision))
    }

    fn ensure_sync_collections(&self) -> Result<(), JianguoyunError> {
        for url in [self.config.sync_dir_url()?, self.config.sync_subdir_url()?] {
            let response = self.send(self.build_request("MKCOL", &url, None))?;
            match response.status {
                200 | 201 | 204 | 301 | 405 => {}
                _ => return Err(status_error(&response)),
            }
        }
        Ok(())
    }

    fn propfind_request(&self, url: &str, depth: &str) -> HttpRequest {
        self.build_request("PROPFIND", url, Some(depth))
            .with_header("Content-Type", PROPFIND_CONTENT_TYPE)
            .with_body(PROPFIND_BODY.as_bytes())
    }

    fn put_request(&self, url: &str, body: &[u8]) -> HttpRequest {
        self.build_request("PUT", url, None)
            .with_header("Content-Type", SYNC_CONTENT_TYPE)
            .with_body(body)
    }

    fn build_request(&self, method: &str, url: &str, depth: Option<&str>) -> HttpRequest {
        let mut request = HttpRequest::new(method, url)
            .with_header("Authorization", self.config.basic_auth_header())
            .with_header("User-Agent", USER_AGENT);
        if let Some(depth) = depth {
            request = request.with_header("Depth", depth);
        }
        request
    }

    fn send(&self, request: HttpRequest) -> Result<HttpResponse, JianguoyunError> {
        self.transport.send(request).map_err(JianguoyunError::from)
    }
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn status_error(response: &HttpResponse) -> JianguoyunError {
    let excerpt_len = response.body.len().min(512);
    let body = String::from_utf8_lossy(&response.body[..excerpt_len]);
    JianguoyunError::from_status(response.status, &body, retry_after_secs(response))
}

fn retry_after_secs(response: &HttpResponse) -> Option<u64> {
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| parse_retry_after(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jianguoyun::mock::MockServer;
    use crate::jianguoyun::model::{JianguoyunNote, JianguoyunProgress};

    const HASH: &str = "0123456789abcdef0123456789abcdef";

    fn progress(fraction: f64, updated_at_ms: i64, device: &str) -> JianguoyunProgress {
        JianguoyunProgress {
            xpointer: Some("/body/p[1]/text().0".to_string()),
            cfi: None,
            fraction,
            page: Some(10),
            total_pages: Some(100),
            chapter_title: Some("第一章".to_string()),
            updated_at_ms,
            device_id: device.to_string(),
        }
    }

    fn record(device: &str, updated_at_ms: i64, fraction: f64) -> JianguoyunBookSync {
        JianguoyunBookSync {
            version: SYNC_SCHEMA_VERSION,
            book_hash: HASH.to_string(),
            title: "三体".to_string(),
            author: "刘慈欣".to_string(),
            remote_path: "/Books/三体.epub".to_string(),
            progress: Some(progress(fraction, updated_at_ms, device)),
            notes: vec![JianguoyunNote::highlight("n-1", "a", "b", "text", 1)],
        }
    }

    fn client(server: &MockServer) -> JianguoyunClient<MockServer> {
        JianguoyunClient::new(server.config(), server.clone())
    }

    #[test]
    fn test_connection_uses_depth_zero_and_basic_auth() {
        let server = MockServer::new();
        client(&server).test_connection().unwrap();
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "PROPFIND");
        assert_eq!(requests[0].path, "/dav/");
        assert_eq!(requests[0].depth.as_deref(), Some("0"));
        assert!(requests[0]
            .authorization
            .as_deref()
            .unwrap()
            .starts_with("Basic "));
    }

    #[test]
    fn list_dir_sends_depth_one_and_parses_entries() {
        let server = MockServer::new();
        server.add_dir("Books");
        server.add_file("Books/三体.epub", b"epub-bytes".to_vec());
        server.add_file("Books/readme.md", b"nope".to_vec());
        let entries = client(&server).list_dir("Books").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "三体.epub");
        assert_eq!(entries[0].format.as_deref(), Some("epub"));
        assert_eq!(entries[0].path, "/dav/Books/三体.epub");
        assert_eq!(server.requests()[0].depth.as_deref(), Some("1"));
    }

    #[test]
    fn list_dir_maps_missing_collection_to_not_found() {
        let server = MockServer::new();
        assert_eq!(
            client(&server).list_dir("missing").unwrap_err(),
            JianguoyunError::NotFound
        );
    }

    #[test]
    fn list_dir_rejects_traversal_before_sending() {
        let server = MockServer::new();
        let error = client(&server).list_dir("../secret").unwrap_err();
        assert_eq!(
            error.kind(),
            crate::jianguoyun::model::JianguoyunErrorKind::InvalidInput
        );
        assert!(server.requests().is_empty());
    }

    #[test]
    fn pull_maps_rate_limiting_without_retry() {
        let server = MockServer::new();
        server.inject_status("GET", 429, Some(30), 5);
        assert_eq!(
            client(&server).pull_book_sync(HASH).unwrap_err(),
            JianguoyunError::RateLimited {
                retry_after_secs: Some(30)
            }
        );
        assert_eq!(server.request_count("GET"), 1);
    }

    #[test]
    fn pull_returns_none_for_missing_record() {
        let server = MockServer::new();
        assert!(client(&server).pull_book_sync(HASH).unwrap().is_none());
    }

    #[test]
    fn pull_parses_a_pushed_record() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let expected = record("KINDLE", 1_000, 0.25);
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&expected).unwrap(),
        );
        let pulled = client(&server).pull_book_sync(HASH).unwrap().unwrap();
        assert_eq!(pulled, expected);
    }

    #[test]
    fn pull_rejects_a_record_for_another_book() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let mut other = record("KINDLE", 1_000, 0.25);
        other.book_hash = "ffffffffffffffffffffffffffffffff".to_string();
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&other).unwrap(),
        );
        assert!(matches!(
            client(&server).pull_book_sync(HASH),
            Err(JianguoyunError::JsonParse(_))
        ));
    }

    #[test]
    fn pull_rejects_a_newer_schema() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let mut newer = record("KINDLE", 1_000, 0.25);
        newer.version = SYNC_SCHEMA_VERSION + 1;
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&newer).unwrap(),
        );
        assert!(matches!(
            client(&server).pull_book_sync(HASH),
            Err(JianguoyunError::JsonParse(_))
        ));
    }

    #[test]
    fn push_creates_collections_and_retries_once() {
        let server = MockServer::new();
        let expected = record("KINDLE", 1_000, 0.25);
        client(&server).push_book_sync(&expected).unwrap();

        let methods: Vec<String> = server
            .requests()
            .into_iter()
            .map(|request| format!("{} {}", request.method, request.path))
            .collect();
        assert_eq!(
            methods,
            [
                format!("PUT /dav/.moyue/sync/{HASH}.json"),
                "MKCOL /dav/.moyue/".to_string(),
                "MKCOL /dav/.moyue/sync/".to_string(),
                format!("PUT /dav/.moyue/sync/{HASH}.json"),
            ]
        );

        let stored = server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .unwrap();
        let stored: JianguoyunBookSync = serde_json::from_slice(&stored).unwrap();
        assert_eq!(stored, expected);
    }

    #[test]
    fn push_retries_on_put_404_and_creates_collections() {
        let server = MockServer::new();
        server.inject_status("PUT", 404, None, 1);
        client(&server)
            .push_book_sync(&record("KINDLE", 1, 0.1))
            .unwrap();
        assert_eq!(server.request_count("PUT"), 2);
        assert_eq!(server.request_count("MKCOL"), 2);
        assert!(server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .is_some());
    }

    #[test]
    fn push_accepts_mkcol_405_as_existing_directory() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        server.inject_status("MKCOL", 405, None, 2);
        client(&server)
            .push_book_sync(&record("KINDLE", 1, 0.1))
            .unwrap();
        assert!(server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .is_some());
    }

    #[test]
    fn push_accepts_mkcol_301_as_existing_directory() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        server.inject_status("MKCOL", 301, None, 2);
        client(&server)
            .push_book_sync(&record("KINDLE", 1, 0.1))
            .unwrap();
        assert!(server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .is_some());
    }

    #[test]
    fn push_returns_rate_limited_without_retry() {
        let server = MockServer::new();
        server.inject_status("PUT", 429, Some(120), 10);
        let error = client(&server)
            .push_book_sync(&record("KINDLE", 1, 0.1))
            .unwrap_err();
        assert_eq!(
            error,
            JianguoyunError::RateLimited {
                retry_after_secs: Some(120)
            }
        );
        assert_eq!(server.request_count("PUT"), 1);
        assert_eq!(server.request_count("MKCOL"), 0);
    }

    #[test]
    fn push_returns_rate_limited_on_503_without_retry_after() {
        let server = MockServer::new();
        server.inject_status("PUT", 503, None, 1);
        assert_eq!(
            client(&server)
                .push_book_sync(&record("KINDLE", 1, 0.1))
                .unwrap_err(),
            JianguoyunError::RateLimited {
                retry_after_secs: None
            }
        );
        assert_eq!(server.request_count("PUT"), 1);
    }

    #[test]
    fn push_maps_auth_and_permission_failures() {
        let server = MockServer::new();
        let mut config = server.config();
        config.password = "wrong".to_string();
        let unauthorized = JianguoyunClient::new(config, server.clone());
        assert_eq!(
            unauthorized
                .push_book_sync(&record("KINDLE", 1, 0.1))
                .unwrap_err(),
            JianguoyunError::AuthFailed
        );

        let server = MockServer::new();
        server.inject_status("PUT", 403, None, 1);
        assert_eq!(
            client(&server)
                .push_book_sync(&record("KINDLE", 1, 0.1))
                .unwrap_err(),
            JianguoyunError::Forbidden
        );
    }

    #[test]
    fn push_surfaces_transport_failures() {
        let server = MockServer::new();
        server.set_network_error(true);
        assert!(matches!(
            client(&server)
                .push_book_sync(&record("KINDLE", 1, 0.1))
                .unwrap_err(),
            JianguoyunError::Transport(_)
        ));
    }

    #[test]
    fn exists_distinguishes_present_and_missing_files() {
        let server = MockServer::new();
        server.add_file("Books/dune.epub", b"x".to_vec());
        let client = client(&server);
        assert!(client.exists("Books/dune.epub").unwrap());
        assert!(!client.exists("Books/missing.epub").unwrap());
    }

    #[test]
    fn sync_book_pushes_when_remote_is_absent() {
        let server = MockServer::new();
        let (merged, decision) = client(&server)
            .sync_book(&record("KINDLE", 1_000, 0.2))
            .unwrap();
        assert_eq!(decision, ProgressDecision::PushLocal);
        assert_eq!(merged.progress.as_ref().unwrap().device_id, "KINDLE");
        assert!(server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .is_some());
    }

    #[test]
    fn sync_book_pulls_a_newer_remote_position_without_pushing() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let remote = record("PHONE", 2_000, 0.8);
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&remote).unwrap(),
        );

        let local = record("KINDLE", 1_000, 0.2);
        let (merged, decision) = client(&server).sync_book(&local).unwrap();
        assert_eq!(decision, ProgressDecision::PullRemote);
        assert_eq!(merged.progress.as_ref().unwrap().device_id, "PHONE");
        assert_eq!(server.request_count("PUT"), 0);
    }

    #[test]
    fn sync_book_pushes_a_newer_local_position() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let remote = record("PHONE", 1_000, 0.2);
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&remote).unwrap(),
        );

        let local = record("KINDLE", 2_000, 0.6);
        let (merged, decision) = client(&server).sync_book(&local).unwrap();
        assert_eq!(decision, ProgressDecision::PushLocal);
        assert_eq!(merged.progress.as_ref().unwrap().device_id, "KINDLE");
        let stored = server
            .file_bytes(&format!(".moyue/sync/{HASH}.json"))
            .unwrap();
        let stored: JianguoyunBookSync = serde_json::from_slice(&stored).unwrap();
        assert_eq!(stored.progress.as_ref().unwrap().device_id, "KINDLE");
    }

    #[test]
    fn sync_book_pushes_a_notes_only_change() {
        let server = MockServer::new();
        server.add_dir(".moyue/sync");
        let mut remote = record("PHONE", 1_000, 0.2);
        remote.notes.clear();
        server.add_file(
            &format!(".moyue/sync/{HASH}.json"),
            serde_json::to_vec(&remote).unwrap(),
        );

        let (merged, decision) = client(&server)
            .sync_book(&record("KINDLE", 1_000, 0.2))
            .unwrap();
        assert_eq!(decision, ProgressDecision::InSync);
        assert_eq!(merged.notes.len(), 1);
        assert_eq!(server.request_count("PUT"), 1);
    }

    #[test]
    fn debug_redacts_password_and_authorization() {
        let server = MockServer::new();
        let client = client(&server);
        let debug = format!("{client:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&server.config().password));
        assert!(!debug.contains("Basic "));
    }

    #[test]
    fn retry_after_parses_delta_seconds_only() {
        assert_eq!(parse_retry_after(" 90 "), Some(90));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
    }
}
