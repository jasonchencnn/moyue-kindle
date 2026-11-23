//! In-memory WebDAV server for the Jianguoyun unit tests and the two-device simulation.
//!
//! It implements [`HttpTransport`], so a [`JianguoyunClient`] cannot tell it apart from a real
//! server: the same `PROPFIND`/`GET`/`PUT`/`MKCOL`/`HEAD` requests go in, real `207` multistatus
//! XML comes back. Failures can be injected per method and path to exercise §15.5.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::cloud::url_encode;
use crate::jianguoyun::model::{percent_decode, JianguoyunConfig, DEFAULT_WEBDAV_BASE};
use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

/// Path prefix of [`DEFAULT_WEBDAV_BASE`] that every request is matched against.
const BASE_PATH: &str = "/dav";
/// Static `getlastmodified` value used by the mock.
const MODIFIED: &str = "Mon, 01 Jan 2026 00:00:00 GMT";

/// One recorded request, enough to assert method, path, `Depth` and authentication.
#[derive(Clone, PartialEq, Eq)]
pub struct MockRequest {
    /// HTTP method.
    pub method: String,
    /// Percent-decoded absolute path without a trailing slash.
    pub path: String,
    /// `Depth` header, when present.
    pub depth: Option<String>,
    /// `Authorization` header, when present.
    pub authorization: Option<String>,
    /// Request body.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for MockRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockRequest")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("depth", &self.depth)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "[REDACTED]"),
            )
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

#[derive(Clone)]
struct MockNode {
    is_dir: bool,
    data: Vec<u8>,
    etag: String,
}

#[derive(Clone)]
struct Fault {
    method: String,
    path: Option<String>,
    status: u16,
    retry_after: Option<u64>,
    remaining: usize,
}

struct Inner {
    nodes: BTreeMap<String, MockNode>,
    username: String,
    password: String,
    expected_auth: String,
    requests: Vec<MockRequest>,
    faults: Vec<Fault>,
    network_error: bool,
    next_etag: u64,
}

impl Inner {
    fn store_file(&mut self, path: &str, data: Vec<u8>) {
        self.next_etag += 1;
        self.nodes.insert(
            path.to_string(),
            MockNode {
                is_dir: false,
                data,
                etag: format!("\"{}\"", self.next_etag),
            },
        );
    }

    fn ensure_dir(&mut self, path: &str) {
        if path.is_empty() || path == "/" || self.nodes.contains_key(path) {
            return;
        }
        if path != BASE_PATH {
            let parent = parent_path(path);
            self.ensure_dir(&parent);
        }
        self.nodes.insert(
            path.to_string(),
            MockNode {
                is_dir: true,
                data: Vec::new(),
                etag: String::from("\"dir\""),
            },
        );
    }

    fn take_fault(&mut self, method: &str, path: &str) -> Option<Fault> {
        let index = self.faults.iter().position(|fault| {
            fault.remaining > 0
                && fault.method.eq_ignore_ascii_case(method)
                && match &fault.path {
                    Some(target) => target == path,
                    None => true,
                }
        })?;
        let fault = self.faults.get_mut(index)?;
        fault.remaining -= 1;
        Some(fault.clone())
    }
}

/// In-memory Jianguoyun WebDAV server.
#[derive(Clone)]
pub struct MockServer {
    state: Arc<Mutex<Inner>>,
}

impl Default for MockServer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockServer {
    /// A server accepting `reader@example.com` / `app-password`.
    pub fn new() -> Self {
        Self::with_credentials("reader@example.com", "app-password")
    }

    /// A server accepting the given Basic credentials.
    pub fn with_credentials(username: &str, password: &str) -> Self {
        let expected_auth =
            JianguoyunConfig::new(DEFAULT_WEBDAV_BASE, username, password, "").basic_auth_header();
        let mut nodes = BTreeMap::new();
        nodes.insert(
            BASE_PATH.to_string(),
            MockNode {
                is_dir: true,
                data: Vec::new(),
                etag: String::from("\"root\""),
            },
        );
        Self {
            state: Arc::new(Mutex::new(Inner {
                nodes,
                username: username.to_string(),
                password: password.to_string(),
                expected_auth,
                requests: Vec::new(),
                faults: Vec::new(),
                network_error: false,
                next_etag: 0,
            })),
        }
    }

    /// A configuration that authenticates against this server.
    pub fn config(&self) -> JianguoyunConfig {
        let state = self.state.lock().unwrap();
        JianguoyunConfig::new(
            DEFAULT_WEBDAV_BASE,
            state.username.clone(),
            state.password.clone(),
            "",
        )
    }

    /// Add a collection, creating its parents.
    pub fn add_dir(&self, rel_path: &str) {
        let mut state = self.state.lock().unwrap();
        let path = absolute_path(rel_path);
        state.ensure_dir(&path);
    }

    /// Add a file with the given bytes, creating its parents.
    pub fn add_file(&self, rel_path: &str, data: Vec<u8>) {
        let mut state = self.state.lock().unwrap();
        let path = absolute_path(rel_path);
        let parent = parent_path(&path);
        state.ensure_dir(&parent);
        state.store_file(&path, data);
    }

    /// Read a stored file's bytes.
    pub fn file_bytes(&self, rel_path: &str) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state
            .nodes
            .get(&absolute_path(rel_path))
            .filter(|node| !node.is_dir)
            .map(|node| node.data.clone())
    }

    /// Whether a path exists, file or collection.
    pub fn contains(&self, rel_path: &str) -> bool {
        let state = self.state.lock().unwrap();
        state.nodes.contains_key(&absolute_path(rel_path))
    }

    /// Every recorded request, in order.
    pub fn requests(&self) -> Vec<MockRequest> {
        self.state.lock().unwrap().requests.clone()
    }

    /// How many requests used `method`.
    pub fn request_count(&self, method: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|request| request.method.eq_ignore_ascii_case(method))
            .count()
    }

    /// Fail the next `times` requests of `method` with `status`, on any path.
    pub fn inject_status(&self, method: &str, status: u16, retry_after: Option<u64>, times: usize) {
        self.state.lock().unwrap().faults.push(Fault {
            method: method.to_string(),
            path: None,
            status,
            retry_after,
            remaining: times,
        });
    }

    /// Fail the next `times` requests of `method` on `rel_path` with `status`.
    pub fn inject_status_for(
        &self,
        method: &str,
        rel_path: &str,
        status: u16,
        retry_after: Option<u64>,
        times: usize,
    ) {
        self.state.lock().unwrap().faults.push(Fault {
            method: method.to_string(),
            path: Some(absolute_path(rel_path)),
            status,
            retry_after,
            remaining: times,
        });
    }

    /// Drop every injected fault.
    pub fn clear_faults(&self) {
        self.state.lock().unwrap().faults.clear();
    }

    /// Make every request fail at the transport layer.
    pub fn set_network_error(&self, enabled: bool) {
        self.state.lock().unwrap().network_error = enabled;
    }
}

impl HttpTransport for MockServer {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut state = self.state.lock().unwrap();
        let path = request_path(&request.url);
        let key = lookup_path(&path);
        let depth = header_value(&request, "depth").map(str::to_string);
        let authorization = header_value(&request, "authorization").map(str::to_string);
        state.requests.push(MockRequest {
            method: request.method.clone(),
            path: path.clone(),
            depth: depth.clone(),
            authorization: authorization.clone(),
            body: request.body.clone(),
        });

        if state.network_error {
            return Err(TransportError::Network("mock network failure".to_string()));
        }
        if authorization.as_deref() != Some(state.expected_auth.as_str()) {
            return Ok(HttpResponse::new(401, b"unauthorized".to_vec())
                .with_header("WWW-Authenticate", "Basic realm=\"jianguoyun\""));
        }
        if let Some(fault) = state.take_fault(&request.method, &key) {
            let mut response = HttpResponse::new(fault.status, b"injected failure".to_vec());
            if let Some(seconds) = fault.retry_after {
                response = response.with_header("Retry-After", seconds.to_string());
            }
            return Ok(response);
        }

        Ok(match request.method.as_str() {
            "PROPFIND" => propfind_response(&state, &key, depth.as_deref()),
            "GET" => body_response(&state, &key, true),
            "HEAD" => body_response(&state, &key, false),
            "PUT" => put_response(&mut state, &key, &request.body),
            "MKCOL" => mkcol_response(&mut state, &key),
            _ => HttpResponse::new(405, b"method not allowed".to_vec()),
        })
    }
}

fn header_value<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn propfind_response(state: &Inner, path: &str, depth: Option<&str>) -> HttpResponse {
    match state.nodes.get(path) {
        Some(node) if !node.is_dir => {
            return HttpResponse::new(405, b"not a collection".to_vec());
        }
        Some(_) => {}
        None => return HttpResponse::new(404, b"not found".to_vec()),
    }

    let mut targets = vec![path.to_string()];
    if depth != Some("0") {
        targets.extend(
            state
                .nodes
                .keys()
                .filter(|child| parent_path(child) == path)
                .cloned(),
        );
    }
    targets.sort();
    let xml = build_multistatus(state, &targets);
    HttpResponse::new(207, xml.into_bytes())
        .with_header("Content-Type", "application/xml; charset=utf-8")
}

fn body_response(state: &Inner, path: &str, with_body: bool) -> HttpResponse {
    let Some(node) = state.nodes.get(path) else {
        return HttpResponse::new(404, b"not found".to_vec());
    };
    if node.is_dir {
        return if with_body {
            HttpResponse::new(405, b"target is a collection".to_vec())
        } else {
            HttpResponse::new(200, Vec::new())
        };
    }
    let body = if with_body {
        node.data.clone()
    } else {
        Vec::new()
    };
    HttpResponse::new(200, body)
        .with_header("ETag", node.etag.clone())
        .with_header("Content-Type", "application/json")
}

fn put_response(state: &mut Inner, path: &str, body: &[u8]) -> HttpResponse {
    match state.nodes.get(&parent_path(path)) {
        Some(parent) if parent.is_dir => {}
        _ => return HttpResponse::new(409, b"parent collection missing".to_vec()),
    }
    if state.nodes.get(path).is_some_and(|node| node.is_dir) {
        return HttpResponse::new(405, b"target is a collection".to_vec());
    }
    let existed = state.nodes.contains_key(path);
    state.store_file(path, body.to_vec());
    HttpResponse::new(if existed { 204 } else { 201 }, Vec::new())
}

fn mkcol_response(state: &mut Inner, path: &str) -> HttpResponse {
    if state.nodes.contains_key(path) {
        return HttpResponse::new(405, b"already exists".to_vec());
    }
    match state.nodes.get(&parent_path(path)) {
        Some(parent) if parent.is_dir => {}
        _ => return HttpResponse::new(409, b"parent collection missing".to_vec()),
    }
    state.ensure_dir(path);
    HttpResponse::new(201, Vec::new())
}

fn build_multistatus(state: &Inner, paths: &[String]) -> String {
    let mut xml =
        String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">");
    for path in paths {
        let Some(node) = state.nodes.get(path) else {
            continue;
        };
        xml.push_str("<D:response><D:href>");
        xml.push_str(&xml_escape(&encode_href(path, node.is_dir)));
        xml.push_str("</D:href><D:propstat><D:prop><D:displayname>");
        xml.push_str(&xml_escape(last_segment(path)));
        xml.push_str("</D:displayname><D:resourcetype>");
        if node.is_dir {
            xml.push_str("<D:collection/>");
        }
        xml.push_str("</D:resourcetype><D:getcontentlength>");
        xml.push_str(&node.data.len().to_string());
        xml.push_str("</D:getcontentlength><D:getlastmodified>");
        xml.push_str(MODIFIED);
        xml.push_str("</D:getlastmodified><D:getetag>");
        xml.push_str(&xml_escape(&node.etag));
        xml.push_str("</D:getetag><D:getcontenttype>");
        xml.push_str(if node.is_dir {
            "httpd/unix-directory"
        } else {
            "application/octet-stream"
        });
        xml.push_str(
            "</D:getcontenttype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        );
    }
    xml.push_str("</D:multistatus>");
    xml
}

fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn encode_href(path: &str, is_dir: bool) -> String {
    let mut href = String::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        href.push('/');
        href.push_str(&url_encode(segment));
    }
    if href.is_empty() {
        href.push('/');
    }
    if is_dir && !href.ends_with('/') {
        href.push('/');
    }
    href
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("/")
}

fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => path[..index].to_string(),
    }
}

fn absolute_path(rel_path: &str) -> String {
    let mut path = BASE_PATH.to_string();
    for segment in rel_path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        path.push('/');
        path.push_str(segment);
    }
    path
}

fn lookup_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

fn request_path(url: &str) -> String {
    let trimmed = url.trim();
    let mut path = trimmed;
    if let Some(index) = trimmed.find("://") {
        let after_scheme = &trimmed[index + 3..];
        path = match after_scheme.find('/') {
            Some(slash) => &after_scheme[slash..],
            None => "/",
        };
    }
    let without_query = path.split(['?', '#']).next().unwrap_or("/");
    let decoded = percent_decode(without_query);
    let mut normalized = String::new();
    for segment in decoded.split('/') {
        if segment.is_empty() {
            continue;
        }
        normalized.push('/');
        normalized.push_str(segment);
    }
    if normalized.is_empty() {
        return "/".to_string();
    }
    if decoded.ends_with('/') {
        normalized.push('/');
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jianguoyun::client::JianguoyunClient;
    use crate::jianguoyun::merge::ProgressDecision;
    use crate::jianguoyun::model::{
        JianguoyunBookSync, JianguoyunError, JianguoyunNote, JianguoyunProgress,
    };

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

    fn book() -> JianguoyunBookSync {
        JianguoyunBookSync::new(HASH, "三体")
    }

    #[test]
    fn propfind_depth_zero_returns_only_the_collection() {
        let server = MockServer::new();
        server.add_dir("Books");
        server.add_file("Books/dune.epub", b"x".to_vec());
        let client = JianguoyunClient::new(server.config(), server.clone());
        client.test_connection().unwrap();

        let request = server.requests().into_iter().next().unwrap();
        assert_eq!(request.path, "/dav/");
        assert_eq!(request.depth.as_deref(), Some("0"));
    }

    #[test]
    fn mock_rejects_bad_credentials_and_missing_parents() {
        let server = MockServer::new();
        let mut config = server.config();
        config.password = "wrong".to_string();
        let client = JianguoyunClient::new(config, server.clone());
        assert_eq!(
            client.test_connection().unwrap_err(),
            JianguoyunError::AuthFailed
        );

        let client = JianguoyunClient::new(server.config(), server.clone());
        assert_eq!(
            client.list_dir("missing").unwrap_err(),
            JianguoyunError::NotFound
        );
        assert!(!server.contains("missing"));
    }

    #[test]
    fn mkcol_reports_existing_collections_as_405() {
        let server = MockServer::new();
        server.add_dir("Books");
        let client = JianguoyunClient::new(server.config(), server.clone());
        let url = server.config().join_remote_path("Books").unwrap();
        let response = server
            .send(
                HttpRequest::new("MKCOL", url)
                    .with_header("Authorization", server.config().basic_auth_header()),
            )
            .unwrap();
        assert_eq!(response.status, 405);
        assert!(client.exists("Books").unwrap());
    }

    #[test]
    fn put_into_a_missing_collection_reports_conflict() {
        let server = MockServer::new();
        let config = server.config();
        let url = config.join_remote_path("missing/book.epub").unwrap();
        let response = server
            .send(
                HttpRequest::new("PUT", url)
                    .with_header("Authorization", config.basic_auth_header())
                    .with_body(b"x".to_vec()),
            )
            .unwrap();
        assert_eq!(response.status, 409);
    }

    #[test]
    fn two_devices_converge_progress_and_notes() {
        let server = MockServer::new();
        let config = server.config();
        let device_a = JianguoyunClient::new(config.clone(), server.clone());
        let device_b = JianguoyunClient::new(config, server.clone());

        let mut book_a = book();
        book_a.progress = Some(progress(0.3, 1_000, "KINDLE"));
        let (synced_a, decision_a) = device_a.sync_book(&book_a).unwrap();
        assert_eq!(decision_a, ProgressDecision::PushLocal);

        let mut book_b = book();
        book_b.progress = Some(progress(0.1, 500, "PHONE"));
        let (synced_b, decision_b) = device_b.sync_book(&book_b).unwrap();
        assert_eq!(decision_b, ProgressDecision::PullRemote);
        assert_eq!(synced_b.progress.as_ref().unwrap().fraction, 0.3);

        let mut advanced_b = synced_b.clone();
        advanced_b.progress = Some(progress(0.55, 2_000, "PHONE"));
        advanced_b.notes.push(JianguoyunNote::highlight(
            "n-1",
            "x0",
            "x1",
            "给岁月以文明",
            1_500,
        ));
        let (pushed_b, decision_b2) = device_b.sync_book(&advanced_b).unwrap();
        assert_eq!(decision_b2, ProgressDecision::PushLocal);

        let puts_before = server.request_count("PUT");
        let (pulled_a, decision_a2) = device_a.sync_book(&synced_a).unwrap();
        assert_eq!(decision_a2, ProgressDecision::PullRemote);
        assert_eq!(pulled_a.progress.as_ref().unwrap().fraction, 0.55);
        assert_eq!(pulled_a.notes.len(), 1);
        assert_eq!(pulled_a.notes[0].text.as_deref(), Some("给岁月以文明"));
        assert_eq!(server.request_count("PUT"), puts_before);

        let mut deleted_a = pulled_a.clone();
        deleted_a.notes[0].deleted_at_ms = Some(3_000);
        deleted_a.notes[0].updated_at_ms = 3_000;
        let (tombstoned, _) = device_a.sync_book(&deleted_a).unwrap();
        assert!(tombstoned.notes[0].is_deleted());

        let (seen_b, _) = device_b.sync_book(&pushed_b).unwrap();
        assert!(seen_b
            .notes
            .iter()
            .any(|note| note.id == "n-1" && note.is_deleted()));
    }

    #[test]
    fn same_millisecond_positions_are_ordered_by_fraction() {
        let server = MockServer::new();
        let config = server.config();
        let device_a = JianguoyunClient::new(config.clone(), server.clone());
        let device_b = JianguoyunClient::new(config, server.clone());

        let mut book_a = book();
        book_a.progress = Some(progress(0.4, 5_000, "KINDLE"));
        let (synced_a, _) = device_a.sync_book(&book_a).unwrap();

        let mut book_b = book();
        book_b.progress = Some(progress(0.6, 5_000, "PHONE"));
        let (converged_b, decision_b) = device_b.sync_book(&book_b).unwrap();
        assert_eq!(decision_b, ProgressDecision::PushLocal);
        assert_eq!(converged_b.progress.as_ref().unwrap().fraction, 0.6);

        let (pulled_a, decision_a) = device_a.sync_book(&synced_a).unwrap();
        assert_eq!(decision_a, ProgressDecision::PullRemote);
        assert_eq!(pulled_a.progress.as_ref().unwrap().fraction, 0.6);
    }

    #[test]
    fn identical_devices_do_not_push() {
        let server = MockServer::new();
        let config = server.config();
        let device_a = JianguoyunClient::new(config.clone(), server.clone());
        let device_b = JianguoyunClient::new(config, server.clone());

        let mut book_a = book();
        book_a.progress = Some(progress(0.5, 1_000, "KINDLE"));
        let (synced_a, _) = device_a.sync_book(&book_a).unwrap();

        let (synced_b, decision) = device_b.sync_book(&synced_a).unwrap();
        assert_eq!(decision, ProgressDecision::InSync);
        assert_eq!(synced_b, synced_a);
        let puts_after_first = server.request_count("PUT");
        let (_, again) = device_b.sync_book(&synced_b).unwrap();
        assert_eq!(again, ProgressDecision::InSync);
        assert_eq!(server.request_count("PUT"), puts_after_first);
    }

    #[test]
    fn injected_faults_are_consumed_once_per_request() {
        let server = MockServer::new();
        let config = server.config();
        server.inject_status_for("PROPFIND", ".moyue/sync", 503, Some(5), 1);
        let client = JianguoyunClient::new(config.clone(), server.clone());
        assert_eq!(
            client.list_dir(".moyue/sync").unwrap_err(),
            JianguoyunError::RateLimited {
                retry_after_secs: Some(5)
            }
        );
        server.clear_faults();
        assert!(client.list_dir("").is_ok());
    }

    #[test]
    fn mock_request_debug_redacts_authorization() {
        let req = MockRequest {
            method: "PROPFIND".to_string(),
            path: "/dav".to_string(),
            depth: Some("0".to_string()),
            authorization: Some("Basic dXNlcjpzZWNyZXQ=".to_string()),
            body: Vec::new(),
        };
        let debug = format!("{req:?}");
        assert!(!debug.contains("dXNlcjpzZWNyZXQ="));
        assert!(!debug.contains("Basic "));
        assert!(debug.contains("[REDACTED]"));
    }
}
