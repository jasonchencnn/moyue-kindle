//! Readest synchronization client managing session lifecycle, pull/push batching,
//! pagination, retry on token expiration, and authoritative record updates.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::sync::auth::{self, AuthError, Session, SyncConfig};
use crate::sync::policy::filter_push_payload;
use crate::sync::records::{
    DbBook, DbBookConfig, DbBookNote, SyncPullResponse, SyncPushRequest, SyncPushResponse,
};
use crate::sync::session::{SessionStore, TokenError};
use crate::sync::time::parse_iso8601;
use crate::sync::transport::{HttpRequest, HttpResponse, HttpTransport, TransportError};

/// Batch size for pushing records to `/api/sync` (contract §2.1).
pub const BATCH_SIZE: usize = 100;

/// Default expiration margin in milliseconds (60 seconds).
pub const DEFAULT_EXPIRY_MARGIN_MS: i64 = 60_000;

/// High-level synchronization error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("not authenticated: please login")]
    NotAuthenticated,
    #[error("authentication expired: {0}")]
    AuthExpired(String),
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("unexpected http status {0}: {1}")]
    HttpStatus(u16, String),
    #[error("failed to deserialize response: {0}")]
    Deserialization(String),
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    #[error("endpoint not reachable from this device: {0}")]
    Blocked(String),
    #[error("response exceeded the {0}-byte limit")]
    TooLarge(u64),
    #[error("pagination stalled: cursor did not advance on a full page")]
    PaginationStalled,
}

/// A coarse failure class: what the UI needs to pick a localized message.
///
/// It deliberately drops the server's text: HTTP bodies and transport details
/// must not reach the screen or the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorKind {
    /// Wrong email or password.
    AuthFailed,
    /// The refresh token is gone: sign in again.
    AuthExpired,
    /// Wi-Fi off, host unreachable, timeout.
    Network,
    /// The service answered, but with an error.
    Server,
    /// A request the client itself considers malformed.
    Invalid,
    /// The device refused the address (network allowlist, no HTTPS in the
    /// build): the network itself may be fine.
    Blocked,
    /// The response exceeded this device's byte limit.
    TooLarge,
}

impl SyncError {
    /// Classify the failure for the UI.
    pub fn kind(&self) -> SyncErrorKind {
        match self {
            Self::NotAuthenticated | Self::AuthExpired(_) => SyncErrorKind::AuthExpired,
            Self::AuthFailed(_) => SyncErrorKind::AuthFailed,
            Self::Transport(_) => SyncErrorKind::Network,
            Self::HttpStatus(401 | 403, _) => SyncErrorKind::AuthExpired,
            Self::HttpStatus(_, _) | Self::Deserialization(_) => SyncErrorKind::Server,
            Self::InvalidParameter(_) | Self::PaginationStalled => SyncErrorKind::Invalid,
            Self::Blocked(_) => SyncErrorKind::Blocked,
            Self::TooLarge(_) => SyncErrorKind::TooLarge,
        }
    }
}

impl From<AuthError> for SyncError {
    fn from(err: AuthError) -> Self {
        match err {
            AuthError::AuthExpired => Self::AuthExpired("session expired".to_string()),
            AuthError::LoginFailed(msg) => Self::AuthFailed(msg),
            AuthError::RefreshFailed(msg) => Self::HttpStatus(0, msg),
            AuthError::Transport(msg) => Self::Transport(msg),
            AuthError::Blocked(detail) => Self::Blocked(detail),
            AuthError::TooLarge(limit) => Self::TooLarge(limit),
            AuthError::InvalidResponse(msg) => Self::Deserialization(msg),
        }
    }
}

impl From<TransportError> for SyncError {
    fn from(err: TransportError) -> Self {
        match err {
            // A status the transport did not expect is an answer from the
            // service, not a network failure: keep it classifiable.
            TransportError::HttpStatus(status, body) => Self::HttpStatus(status, body),
            TransportError::Blocked(detail) => Self::Blocked(detail),
            TransportError::TooLarge(limit) => Self::TooLarge(limit),
            TransportError::Network(detail) => Self::Transport(detail),
            TransportError::Timeout => Self::Transport(TransportError::Timeout.to_string()),
            TransportError::Other(detail) => Self::Transport(detail),
        }
    }
}

fn default_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The client's view of a failure to get an access token.
fn token_error(err: TokenError) -> SyncError {
    match err {
        TokenError::NotAuthenticated => SyncError::NotAuthenticated,
        TokenError::Auth(AuthError::AuthExpired) => {
            SyncError::AuthExpired("session expired".to_string())
        }
        TokenError::Auth(e) => e.into(),
    }
}

/// Readest synchronization client.
///
/// The session belongs to a [`SessionStore`], not to the client: the requests
/// of one account share it, so they refresh a rotating token once and a
/// request that finishes late reads the newest session.
pub struct SyncClient<T: HttpTransport> {
    transport: T,
    config: SyncConfig,
    session: SessionStore,
    time_fn: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl<T: HttpTransport> SyncClient<T> {
    /// Create a new `SyncClient` with default system time provider.
    pub fn new(transport: T, config: SyncConfig) -> Self {
        Self::with_session_store(transport, config, SessionStore::new())
    }

    /// Create a client that shares the account's session with the other
    /// requests of the same account.
    ///
    /// Sharing the session is what keeps two requests that run at the same
    /// time from spending the same rotating refresh token twice.
    pub fn with_session_store(transport: T, config: SyncConfig, session: SessionStore) -> Self {
        Self {
            transport,
            config,
            session,
            time_fn: Box::new(default_now_ms),
        }
    }

    /// Set a custom timestamp provider (useful for deterministic tests).
    pub fn with_time_provider(
        mut self,
        provider: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        self.time_fn = Box::new(provider);
        self
    }

    /// Return current time in milliseconds using configured time provider.
    pub fn now_ms(&self) -> i64 {
        (self.time_fn)()
    }

    /// Return a clone of current session if authenticated.
    ///
    /// With a shared store this is the account's newest session, not the one
    /// this client started with.
    pub fn session(&self) -> Option<Session> {
        self.session.current()
    }

    /// Set or clear the current active session.
    pub fn set_session(&self, session: Option<Session>) {
        self.session.set_session(session);
    }

    /// The store the session of this client lives in.
    pub fn session_store(&self) -> &SessionStore {
        &self.session
    }

    /// Perform password authentication and store the resulting session.
    pub fn login(&self, email: &str, password: &str) -> Result<Session, SyncError> {
        let now = self.now_ms();
        let session = auth::login(&self.transport, &self.config, email, password, now)?;
        self.set_session(Some(session.clone()));
        Ok(session)
    }

    /// Ensure the current session has a valid, non-expired access token.
    ///
    /// Automatically performs token refresh if near or past expiration.
    pub fn ensure_token(&self) -> Result<String, SyncError> {
        let now = self.now_ms();
        let mut refresh =
            |token: &str| auth::refresh(&self.transport, &self.config, token, self.now_ms());
        self.session
            .access_token(now, DEFAULT_EXPIRY_MARGIN_MS, &mut refresh)
            .map_err(token_error)
    }

    /// An access token and the generation it belongs to.
    ///
    /// Both come from one lock hold in the store: a request that is refused
    /// with a 401 must know whether the token it used is still the current
    /// one. A refresh that happened in between must not look like its own,
    /// or the request would spend the rotating token a second time.
    fn ensure_token_with_generation(&self) -> Result<(String, u64), SyncError> {
        let now = self.now_ms();
        let mut refresh =
            |token: &str| auth::refresh(&self.transport, &self.config, token, self.now_ms());
        self.session
            .access_token_with_generation(now, DEFAULT_EXPIRY_MARGIN_MS, &mut refresh)
            .map_err(token_error)
    }

    /// Force a refresh of the current session token.
    ///
    /// `since` is the generation the request started with: when somebody else
    /// refreshed in the meantime, their token is newer and the rotating
    /// refresh token must not be spent a second time.
    fn refresh_token_force(&self, since: u64) -> Result<String, SyncError> {
        let mut refresh =
            |token: &str| auth::refresh(&self.transport, &self.config, token, self.now_ms());
        self.session
            .refreshed_token(since, &mut refresh)
            .map_err(token_error)
    }

    /// Send an authenticated request with automatic retry on 401/403.
    fn execute_with_auth<F>(&self, build_req: F) -> Result<HttpResponse, SyncError>
    where
        F: Fn(&str) -> HttpRequest,
    {
        let (token, generation) = self.ensure_token_with_generation()?;
        let req = build_req(&token);
        let resp = self.transport.send(req)?;

        if resp.status == 401 || resp.status == 403 {
            // Attempt refresh once and retry
            let fresh_token = self.refresh_token_force(generation)?;
            let retry_req = build_req(&fresh_token);
            let retry_resp = self.transport.send(retry_req)?;
            if retry_resp.status == 401 || retry_resp.status == 403 {
                self.set_session(None);
                return Err(SyncError::AuthExpired(format!(
                    "HTTP {}",
                    retry_resp.status
                )));
            }
            Ok(retry_resp)
        } else {
            Ok(resp)
        }
    }

    /// Pull incremental sync changes in a single request from `GET /api/sync`.
    ///
    /// Note: this performs a single bounded pull request and does not automatically follow
    /// cursor pagination for large book deltas. For complete paginated book synchronization,
    /// use [`pull_all_books`](Self::pull_all_books).
    pub fn pull(
        &self,
        sync_type: Option<&str>,
        since_ms: i64,
    ) -> Result<SyncPullResponse, SyncError> {
        self.pull_internal(sync_type, since_ms, None, None)
    }

    /// Pull incremental sync changes with specific query parameters.
    pub fn pull_internal(
        &self,
        sync_type: Option<&str>,
        since_ms: i64,
        limit: Option<usize>,
        book_filter: Option<&str>,
    ) -> Result<SyncPullResponse, SyncError> {
        if let Some(0) = limit {
            return Err(SyncError::InvalidParameter("limit must be > 0".to_string()));
        }

        let mut query = format!("since={since_ms}");
        if let Some(t) = sync_type {
            query.push_str(&format!("&type={t}"));
        }
        if let Some(lim) = limit {
            query.push_str(&format!("&limit={lim}"));
        }
        if let Some(bf) = book_filter {
            query.push_str(&format!("&book={bf}"));
        }

        let url = format!("{}/sync?{query}", self.config.api_base);

        let resp = self.execute_with_auth(|token| {
            HttpRequest::new("GET", &url)
                .with_header("Authorization", format!("Bearer {token}"))
                .with_header("Accept", "application/json")
        })?;

        if resp.status != 200 {
            return Err(SyncError::HttpStatus(
                resp.status,
                String::from_utf8_lossy(&resp.body).into_owned(),
            ));
        }

        let mut pull_resp: SyncPullResponse = serde_json::from_slice(&resp.body)
            .map_err(|e| SyncError::Deserialization(e.to_string()))?;

        // Filter out dummy book if present (contract §1.4)
        pull_resp
            .books
            .retain(|b| b.book_hash != "00000000000000000000000000000000");

        // Filter configs for TXT books in this response (contract §8)
        let txt_hashes: HashSet<String> = pull_resp
            .books
            .iter()
            .filter(|b| crate::sync::policy::is_txt_format(&b.format))
            .map(|b| b.book_hash.clone())
            .collect();

        if !txt_hashes.is_empty() {
            for config in &mut pull_resp.configs {
                if txt_hashes.contains(&config.book_hash) {
                    *config = crate::sync::policy::filter_pulled_config_for_txt(config.clone());
                }
            }
            // TXT notes stay local (contract §8): drop any the server still has.
            pull_resp
                .notes
                .retain(|note| !txt_hashes.contains(&note.book_hash));
        }

        Ok(pull_resp)
    }

    /// Pull all books incrementally using cursor pagination until exhausted.
    pub fn pull_all_books(
        &self,
        since_ms: i64,
        limit_per_page: usize,
    ) -> Result<Vec<DbBook>, SyncError> {
        self.pull_all_books_with_cursor(since_ms, limit_per_page)
            .map(|(books, _)| books)
    }

    /// Pull all books incrementally and report the newest server timestamp
    /// among the rows that came back.
    ///
    /// The cursor is what the server stamped (`synced_at`, contract §1.2), not
    /// the device's clock: a Kindle whose clock runs ahead would otherwise
    /// skip every change another device makes until its clock caught up.
    /// `None` means nothing was pulled, so the caller must keep its old cursor
    /// instead of jumping to "now".
    pub fn pull_all_books_with_cursor(
        &self,
        since_ms: i64,
        limit_per_page: usize,
    ) -> Result<(Vec<DbBook>, Option<i64>), SyncError> {
        if limit_per_page == 0 {
            return Err(SyncError::InvalidParameter(
                "limit_per_page must be > 0".to_string(),
            ));
        }

        let mut all_books = Vec::new();
        let mut newest: Option<i64> = None;
        let mut cursor_ms = since_ms;

        loop {
            let page = self.pull_internal(Some("books"), cursor_ms, Some(limit_per_page), None)?;
            if page.books.is_empty() {
                break;
            }

            let page_len = page.books.len();
            let prev_cursor = cursor_ms;
            newest = newest.max(page.books_cursor());
            for b in page.books {
                let ts_ms = b
                    .synced_at
                    .as_deref()
                    .or(b.updated_at.as_deref())
                    .and_then(|ts| parse_iso8601(ts).ok());
                if let Some(ms) = ts_ms {
                    if ms > cursor_ms {
                        cursor_ms = ms;
                    }
                }
                all_books.push(b);
            }

            if page_len < limit_per_page {
                break;
            }

            // Loop guard: if full page was returned but cursor did not advance, abort
            if cursor_ms <= prev_cursor {
                return Err(SyncError::PaginationStalled);
            }
        }

        Ok((all_books, newest))
    }

    /// Push local changes to `POST /api/sync` in batches of `BATCH_SIZE`.
    ///
    /// Automatically derives TXT book hashes from `req.books` (where format is TXT),
    /// merges with `txt_book_hashes` if provided, and filters configs/notes per contract §8.
    pub fn push(
        &self,
        mut req: SyncPushRequest,
        txt_book_hashes: Option<&HashSet<String>>,
    ) -> Result<SyncPushResponse, SyncError> {
        let mut all_txt_hashes = HashSet::new();
        if let Some(explicit) = txt_book_hashes {
            all_txt_hashes.extend(explicit.iter().cloned());
        }
        for b in &req.books {
            if crate::sync::policy::is_txt_format(&b.format) {
                all_txt_hashes.insert(b.book_hash.clone());
            }
        }

        if !all_txt_hashes.is_empty() {
            let (filtered_configs, filtered_notes) =
                filter_push_payload(&all_txt_hashes, req.configs, req.notes);
            req.configs = filtered_configs;
            req.notes = filtered_notes;
        }

        let mut combined_response = SyncPushResponse::default();
        let url = format!("{}/sync", self.config.api_base);

        // Chunk configs into batches
        let config_batches: Vec<Vec<DbBookConfig>> =
            req.configs.chunks(BATCH_SIZE).map(|c| c.to_vec()).collect();
        let note_batches: Vec<Vec<DbBookNote>> =
            req.notes.chunks(BATCH_SIZE).map(|n| n.to_vec()).collect();
        let book_batches: Vec<Vec<DbBook>> =
            req.books.chunks(BATCH_SIZE).map(|b| b.to_vec()).collect();

        let num_batches = config_batches
            .len()
            .max(note_batches.len())
            .max(book_batches.len())
            .max(1);

        for i in 0..num_batches {
            let chunk_req = SyncPushRequest {
                books: book_batches.get(i).cloned().unwrap_or_default(),
                configs: config_batches.get(i).cloned().unwrap_or_default(),
                notes: note_batches.get(i).cloned().unwrap_or_default(),
                stat_books: if i == 0 {
                    req.stat_books.clone()
                } else {
                    Vec::new()
                },
                stat_pages: if i == 0 {
                    req.stat_pages.clone()
                } else {
                    Vec::new()
                },
            };

            let body_bytes = serde_json::to_vec(&chunk_req)
                .map_err(|e| SyncError::Deserialization(e.to_string()))?;

            let resp = self.execute_with_auth(|token| {
                HttpRequest::new("POST", &url)
                    .with_header("Authorization", format!("Bearer {token}"))
                    .with_header("Content-Type", "application/json")
                    .with_body(body_bytes.clone())
            })?;

            if resp.status != 200 {
                return Err(SyncError::HttpStatus(
                    resp.status,
                    String::from_utf8_lossy(&resp.body).into_owned(),
                ));
            }

            let push_resp: SyncPushResponse = serde_json::from_slice(&resp.body)
                .map_err(|e| SyncError::Deserialization(e.to_string()))?;

            combined_response.books.extend(push_resp.books);
            combined_response.configs.extend(push_resp.configs);
            combined_response.notes.extend(push_resp.notes);
        }

        Ok(combined_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mock::MockServer;

    #[test]
    fn two_devices_sync_end_to_end_via_mock() {
        let server = MockServer::new();
        server.set_clock_ms(parse_iso8601("2026-11-05T09:59:00.000Z").unwrap());
        server.add_user("alice@example.com", "pass123", "usr-alice");

        let config = SyncConfig::default();
        let client_a = SyncClient::new(server.clone(), config.clone());
        let client_b = SyncClient::new(server.clone(), config);

        // 1. Both devices log in
        client_a
            .login("alice@example.com", "pass123")
            .expect("alice login a");
        client_b
            .login("alice@example.com", "pass123")
            .expect("alice login b");

        // 2. Device A pushes a new book and a note
        let book_hash = "book-hash-999";
        let book_a = DbBook {
            book_hash: book_hash.to_string(),
            title: "三体".to_string(),
            author: "刘慈欣".to_string(),
            format: "EPUB".to_string(),
            updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
            ..Default::default()
        };
        let note_a = DbBookNote {
            book_hash: book_hash.to_string(),
            id: "note-01".to_string(),
            kind: "annotation".to_string(),
            text: Some("消灭人类暴政，世界属于三体".to_string()),
            created_at: Some("2026-11-05T10:00:00.000Z".to_string()),
            updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
            ..Default::default()
        };

        let push_req = SyncPushRequest {
            books: vec![book_a],
            notes: vec![note_a],
            ..Default::default()
        };
        client_a.push(push_req, None).expect("push a");

        // 3. Device B pulls from since=0 and receives book and note
        let pull_b = client_b.pull(None, 0).expect("pull b");
        assert_eq!(pull_b.books.len(), 1);
        assert_eq!(pull_b.books[0].title, "三体");
        assert_eq!(pull_b.notes.len(), 1);
        assert_eq!(pull_b.notes[0].id, "note-01");

        // 4. Device B updates progress and creates note 2, plus marks note 1 deleted
        server.set_clock_ms(parse_iso8601("2026-11-05T11:00:00.000Z").unwrap());
        let config_b = DbBookConfig {
            book_hash: book_hash.to_string(),
            xpointer: Some("/body/DocFragment[5]/p[3].0".to_string()),
            progress: Some("[50, 200]".to_string()),
            updated_at: Some("2026-11-05T11:00:00.000Z".to_string()),
            ..Default::default()
        };
        let note_2 = DbBookNote {
            book_hash: book_hash.to_string(),
            id: "note-02".to_string(),
            kind: "bookmark".to_string(),
            xpointer0: Some("/body/DocFragment[5]/p[3].0".to_string()),
            created_at: Some("2026-11-05T11:00:00.000Z".to_string()),
            updated_at: Some("2026-11-05T11:00:00.000Z".to_string()),
            ..Default::default()
        };
        let note_1_tombstone = DbBookNote {
            book_hash: book_hash.to_string(),
            id: "note-01".to_string(),
            kind: "annotation".to_string(),
            updated_at: Some("2026-11-05T11:00:00.000Z".to_string()),
            deleted_at: Some("2026-11-05T11:00:00.000Z".to_string()),
            ..Default::default()
        };

        let push_req_b = SyncPushRequest {
            configs: vec![config_b],
            notes: vec![note_2, note_1_tombstone],
            ..Default::default()
        };
        client_b.push(push_req_b, None).expect("push b");

        // 5. Device A pulls changes since 10:00:00
        let since_10am = parse_iso8601("2026-11-05T10:00:00.000Z").unwrap();
        let pull_a = client_a.pull(None, since_10am).expect("pull a");

        // Config updated with progress
        assert_eq!(pull_a.configs.len(), 1);
        assert_eq!(
            pull_a.configs[0].xpointer.as_deref(),
            Some("/body/DocFragment[5]/p[3].0")
        );

        // Notes: note-01 is marked deleted, note-02 is present
        assert_eq!(pull_a.notes.len(), 2);
        let note1 = pull_a.notes.iter().find(|n| n.id == "note-01").unwrap();
        assert!(note1.deleted_at.is_some());
        let note2 = pull_a.notes.iter().find(|n| n.id == "note-02").unwrap();
        assert_eq!(note2.kind, "bookmark");
    }

    #[test]
    fn token_expired_triggers_auto_refresh_and_retry() {
        let server = MockServer::new();
        server.add_user("bob@example.com", "pass456", "usr-bob");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        client.login("bob@example.com", "pass456").unwrap();

        // Expire the token by setting expires_at to 0
        let mut session = client.session().unwrap();
        session.expires_at = 0;
        client.set_session(Some(session));

        // Pull should detect expired session, refresh token automatically, and succeed
        let pull_res = client.pull(None, 0);
        assert!(pull_res.is_ok(), "auto refresh succeeded on pull");
        assert!(client.session().unwrap().expires_at > 0);
    }

    #[test]
    fn refresh_failure_yields_auth_expired() {
        let server = MockServer::new();
        server.add_user("carol@example.com", "pass789", "usr-carol");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        client.login("carol@example.com", "pass789").unwrap();

        // Expire token and inject auth error on server
        let mut session = client.session().unwrap();
        session.expires_at = 0;
        client.set_session(Some(session));
        server.set_inject_auth_expired(true);

        let pull_res = client.pull(None, 0);
        assert!(matches!(pull_res, Err(SyncError::AuthExpired(_))));
    }

    #[test]
    fn network_transport_error_does_not_corrupt_client_session() {
        let server = MockServer::new();
        server.add_user("dave@example.com", "pass123", "usr-dave");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        let session = client.login("dave@example.com", "pass123").unwrap();

        server.set_inject_network_error(true);
        let pull_res = client.pull(None, 0);
        assert!(matches!(pull_res, Err(SyncError::Transport(_))));

        // Session is still preserved
        assert_eq!(client.session().unwrap().access_token, session.access_token);

        // Once network recovers, pull succeeds
        server.set_inject_network_error(false);
        assert!(client.pull(None, 0).is_ok());
    }

    #[test]
    fn sync_auth_failure_triggers_refresh_and_retry_success() {
        let server = MockServer::new();
        server.add_user("frank@example.com", "pass123", "usr-frank");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        client.login("frank@example.com", "pass123").unwrap();

        // Inject 1 auth failure on /api/sync
        server.inject_sync_auth_failures(1);

        // Pull should trigger 403 -> refresh_token_force() -> retry -> succeeds
        let pull_res = client.pull(None, 0);
        assert!(
            pull_res.is_ok(),
            "single 403 failure refreshed and retried successfully"
        );
    }

    #[test]
    fn refresh_endpoint_401_yields_auth_expired_and_clears_session() {
        let server = MockServer::new();
        server.add_user("eve@example.com", "pass123", "usr-eve");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        client.login("eve@example.com", "pass123").unwrap();

        // Expire token and inject 401 on refresh endpoint
        let mut session = client.session().unwrap();
        session.expires_at = 0;
        client.set_session(Some(session));
        server.set_inject_refresh_status(Some(401));

        let pull_res = client.pull(None, 0);
        assert!(matches!(pull_res, Err(SyncError::AuthExpired(_))));
        assert!(
            client.session().is_none(),
            "session cleared on 401 refresh failure"
        );
    }

    #[test]
    fn refresh_endpoint_500_preserves_session() {
        let server = MockServer::new();
        server.add_user("grace@example.com", "pass123", "usr-grace");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        let initial_session = client.login("grace@example.com", "pass123").unwrap();

        // Expire token and inject 500 on refresh endpoint
        let mut session = initial_session.clone();
        session.expires_at = 0;
        client.set_session(Some(session));
        server.set_inject_refresh_status(Some(500));

        let pull_res = client.pull(None, 0);
        assert!(pull_res.is_err());
        assert!(!matches!(pull_res, Err(SyncError::AuthExpired(_))));
        assert!(
            client.session().is_some(),
            "session preserved on 500 refresh failure"
        );
        assert_eq!(
            client.session().unwrap().refresh_token,
            initial_session.refresh_token
        );
    }

    #[test]
    fn txt_book_push_auto_derivation_and_filtering() {
        let server = MockServer::new();
        server.add_user("txtuser@example.com", "pass123", "usr-txt");

        let config = SyncConfig::default();
        let client = SyncClient::new(server.clone(), config);

        client.login("txtuser@example.com", "pass123").unwrap();

        let txt_hash = "txt-hash-01";
        let book_txt = DbBook {
            book_hash: txt_hash.to_string(),
            title: "Plain Text Book".to_string(),
            format: "TXT".to_string(),
            ..Default::default()
        };

        let conf_txt = DbBookConfig {
            book_hash: txt_hash.to_string(),
            progress: Some("[100, 500]".to_string()),
            xpointer: Some("/body/DocFragment[1]/p[1].0".to_string()),
            view_settings: Some("{\"fontSize\": 18}".to_string()),
            ..Default::default()
        };

        let note_txt = DbBookNote {
            book_hash: txt_hash.to_string(),
            id: "txt-note-1".to_string(),
            kind: "bookmark".to_string(),
            ..Default::default()
        };

        // Push without providing txt_book_hashes (auto-derivation from book.format)
        let push_req = SyncPushRequest {
            books: vec![book_txt],
            configs: vec![conf_txt],
            notes: vec![note_txt],
            ..Default::default()
        };
        let push_res = client.push(push_req, None);
        assert!(push_res.is_ok(), "push succeeded");

        // Pull to verify:
        // 1. Config should have font_size retained, but progress and xpointer cleared
        // 2. Note should be dropped completely
        let pull_res = client.pull(None, 0).unwrap();
        assert_eq!(pull_res.books.len(), 1);
        assert_eq!(pull_res.books[0].format, "TXT");
        assert_eq!(pull_res.books[0].progress, None); // Mock piggyback didn't update progress

        assert_eq!(pull_res.configs.len(), 1);
        assert_eq!(
            pull_res.configs[0].view_settings.as_deref(),
            Some("{\"fontSize\": 18}")
        );
        assert_eq!(pull_res.configs[0].progress, None);
        assert_eq!(pull_res.configs[0].xpointer, None);

        assert_eq!(pull_res.notes.len(), 0);
    }

    #[test]
    fn error_kinds_are_what_the_ui_localizes() {
        assert_eq!(
            SyncError::AuthFailed("bad password".into()).kind(),
            SyncErrorKind::AuthFailed
        );
        assert_eq!(
            SyncError::AuthExpired("gone".into()).kind(),
            SyncErrorKind::AuthExpired
        );
        assert_eq!(
            SyncError::NotAuthenticated.kind(),
            SyncErrorKind::AuthExpired
        );
        assert_eq!(
            SyncError::Transport("offline".into()).kind(),
            SyncErrorKind::Network
        );
        assert_eq!(
            SyncError::HttpStatus(503, "body".into()).kind(),
            SyncErrorKind::Server
        );
        assert_eq!(
            SyncError::HttpStatus(401, "body".into()).kind(),
            SyncErrorKind::AuthExpired
        );
        assert_eq!(
            SyncError::Deserialization("junk".into()).kind(),
            SyncErrorKind::Server
        );
        assert_eq!(
            SyncError::InvalidParameter("x".into()).kind(),
            SyncErrorKind::Invalid
        );
        assert_eq!(
            SyncError::Blocked("host:443".into()).kind(),
            SyncErrorKind::Blocked
        );
    }

    #[test]
    fn a_transport_status_is_not_a_network_failure() {
        assert_eq!(
            SyncError::from(TransportError::TooLarge(20 * 1024 * 1024)).kind(),
            SyncErrorKind::TooLarge
        );
        assert_eq!(
            SyncError::from(AuthError::TooLarge(20 * 1024 * 1024)).kind(),
            SyncErrorKind::TooLarge
        );
        let err = SyncError::from(TransportError::HttpStatus(
            503,
            "service unavailable".to_string(),
        ));
        assert_eq!(
            err,
            SyncError::HttpStatus(503, "service unavailable".into())
        );
        assert_eq!(err.kind(), SyncErrorKind::Server);

        let blocked = SyncError::from(TransportError::Blocked("host:443".into()));
        assert_eq!(blocked.kind(), SyncErrorKind::Blocked);
        // The detail never reaches the screen: only the kind does.
        assert!(!format!("{blocked:?}").is_empty());

        let timeout = SyncError::from(TransportError::Timeout);
        assert_eq!(timeout.kind(), SyncErrorKind::Network);
        let offline = SyncError::from(TransportError::Network("connection reset".into()));
        assert_eq!(offline.kind(), SyncErrorKind::Network);
    }

    #[test]
    fn an_auth_blocked_endpoint_stays_blocked() {
        let err = SyncError::from(AuthError::Blocked("auth.example:443".into()));
        assert_eq!(err.kind(), SyncErrorKind::Blocked);
        let refresh = SyncError::from(TransportError::Blocked("auth.example:443".into()));
        assert_eq!(refresh.kind(), SyncErrorKind::Blocked);
    }

    #[test]
    fn pull_all_books_reports_the_server_cursor() {
        let server = MockServer::new();
        server.add_user("cursor@example.com", "pass123", "usr-cursor");
        server.insert_book(
            "usr-cursor",
            DbBook {
                book_hash: "hash-1".to_string(),
                title: "Book 1".to_string(),
                synced_at: Some("2026-11-05T10:00:00.000Z".to_string()),
                ..Default::default()
            },
        );
        server.insert_book(
            "usr-cursor",
            DbBook {
                book_hash: "hash-2".to_string(),
                title: "Book 2".to_string(),
                synced_at: Some("2026-11-05T11:30:00.000Z".to_string()),
                ..Default::default()
            },
        );

        let client = SyncClient::new(server.clone(), SyncConfig::default());
        client.login("cursor@example.com", "pass123").unwrap();

        let (books, cursor) = client.pull_all_books_with_cursor(0, 100).unwrap();
        assert_eq!(books.len(), 2);
        assert_eq!(
            cursor,
            Some(parse_iso8601("2026-11-05T11:30:00.000Z").unwrap()),
            "the cursor is the server's newest synced_at"
        );

        // Nothing new: no cursor, so the caller must keep the old one rather
        // than jump to the device's clock.
        let (none, no_cursor) = client
            .pull_all_books_with_cursor(cursor.unwrap(), 100)
            .unwrap();
        assert!(none.is_empty());
        assert_eq!(no_cursor, None);
    }

    #[test]
    fn a_pull_response_reports_one_cursor_per_record_type() {
        let resp = SyncPullResponse {
            books: vec![DbBook {
                book_hash: "b".into(),
                synced_at: Some("2026-11-05T10:00:00.000Z".into()),
                ..Default::default()
            }],
            configs: vec![DbBookConfig {
                book_hash: "b".into(),
                updated_at: Some("2026-11-05T09:00:00.000Z".into()),
                deleted_at: Some("2026-11-05T12:00:00.000Z".into()),
                ..Default::default()
            }],
            notes: vec![DbBookNote {
                book_hash: "b".into(),
                id: "n".into(),
                kind: "bookmark".into(),
                updated_at: Some("2026-11-05T08:00:00.000Z".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resp.books_cursor(),
            Some(parse_iso8601("2026-11-05T10:00:00.000Z").unwrap())
        );
        assert_eq!(
            resp.configs_cursor(),
            Some(parse_iso8601("2026-11-05T12:00:00.000Z").unwrap()),
            "a tombstone advances the configs cursor"
        );
        assert_eq!(
            resp.notes_cursor(),
            Some(parse_iso8601("2026-11-05T08:00:00.000Z").unwrap())
        );
        assert_eq!(SyncPullResponse::default().books_cursor(), None);
    }

    #[test]
    fn pull_drops_notes_of_txt_books() {
        let server = MockServer::new();
        server.add_user("txtpull@example.com", "pass123", "usr-txtpull");
        server.insert_book(
            "usr-txtpull",
            DbBook {
                book_hash: "txt-book".to_string(),
                format: "TXT".to_string(),
                updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
                ..Default::default()
            },
        );
        server.insert_note(
            "usr-txtpull",
            DbBookNote {
                book_hash: "txt-book".to_string(),
                id: "txt-note".to_string(),
                kind: "annotation".to_string(),
                updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
                ..Default::default()
            },
        );

        let config = SyncConfig::default();
        let client = SyncClient::new(server, config);
        client.login("txtpull@example.com", "pass123").unwrap();
        let pulled = client.pull(None, 0).unwrap();
        assert_eq!(pulled.books.len(), 1);
        assert!(pulled.notes.is_empty(), "TXT notes must not be pulled");
    }

    #[test]
    fn pull_all_books_limit_zero_returns_invalid_parameter() {
        let server = MockServer::new();
        server.add_user("hannah@example.com", "pass123", "usr-hannah");

        let config = SyncConfig::default();
        let client = SyncClient::new(server, config);
        client.login("hannah@example.com", "pass123").unwrap();

        let res = client.pull_all_books(0, 0);
        assert!(matches!(res, Err(SyncError::InvalidParameter(_))));
    }

    #[test]
    fn pull_all_books_falls_back_to_updated_at() {
        let server = MockServer::new();
        server.add_user("ian@example.com", "pass123", "usr-ian");

        // Insert two books with NO synced_at, but updated_at set
        server.insert_book(
            "usr-ian",
            DbBook {
                book_hash: "hash-upd-1".to_string(),
                title: "Book 1".to_string(),
                synced_at: None,
                updated_at: Some("2026-11-05T10:00:00.000Z".to_string()),
                ..Default::default()
            },
        );
        server.insert_book(
            "usr-ian",
            DbBook {
                book_hash: "hash-upd-2".to_string(),
                title: "Book 2".to_string(),
                synced_at: None,
                updated_at: Some("2026-11-05T10:30:00.000Z".to_string()),
                ..Default::default()
            },
        );

        let config = SyncConfig::default();
        let client = SyncClient::new(server, config);
        client.login("ian@example.com", "pass123").unwrap();

        // Paginate with limit 1: should fetch 2 pages and combine both books
        let books = client.pull_all_books(0, 1).unwrap();
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].book_hash, "hash-upd-1");
        assert_eq!(books[1].book_hash, "hash-upd-2");
    }

    #[test]
    fn pull_all_books_detects_stalled_cursor() {
        let server = MockServer::new();
        server.add_user("jack@example.com", "pass123", "usr-jack");

        // Insert a book with no timestamps
        server.insert_book(
            "usr-jack",
            DbBook {
                book_hash: "hash-stalled".to_string(),
                title: "Stalled Book".to_string(),
                synced_at: None,
                updated_at: None,
                ..Default::default()
            },
        );

        let config = SyncConfig::default();
        let client = SyncClient::new(server, config);
        client.login("jack@example.com", "pass123").unwrap();

        // since = -1 matches the book with ts=0. Full page of limit 1 is returned,
        // but timestamp is None so cursor cannot advance past -1 -> PaginationStalled
        let stalled_res = client.pull_all_books(-1, 1);
        assert_eq!(stalled_res, Err(SyncError::PaginationStalled));
    }

    /// Two requests of one account that start with the same expired session
    /// must not spend the rotating refresh token twice.
    ///
    /// The mock rotates the refresh token like GoTrue does: the token that
    /// was used is refused afterwards. Without a shared session the second
    /// request refreshes with a token that is already spent, and the account
    /// looks signed out.
    #[test]
    fn two_requests_of_one_account_spend_the_rotating_token_once() {
        let server = MockServer::new();
        server.add_user("alice@example.com", "pass123", "usr-alice");
        let config = SyncConfig::default();

        let login = SyncClient::new(server.clone(), config.clone());
        let session = login.login("alice@example.com", "pass123").expect("login");

        // Both requests start with the same session, expired on purpose.
        let store = SessionStore::with_session(Some(Session::new(
            String::new(),
            session.refresh_token.clone(),
            0,
        )));
        let client_a =
            SyncClient::with_session_store(server.clone(), config.clone(), store.clone());
        let client_b = SyncClient::with_session_store(server.clone(), config, store.clone());

        let (first, second) = std::thread::scope(|scope| {
            let a = scope.spawn(|| client_a.pull(Some("books"), 0));
            let b = scope.spawn(|| client_b.pull(Some("books"), 0));
            (
                a.join().expect("first thread"),
                b.join().expect("second thread"),
            )
        });

        assert!(first.is_ok(), "the first request must succeed: {first:?}");
        assert!(
            second.is_ok(),
            "the second request must succeed: {second:?}"
        );
        assert_eq!(
            server.refresh_count(),
            1,
            "the rotating refresh token is spent once, not once per request"
        );
    }

    /// A request that finishes late reports the newest session, so a stale
    /// worker cannot write an old refresh token over a newer one.
    #[test]
    fn a_finished_request_reports_the_newest_session() {
        let server = MockServer::new();
        server.add_user("alice@example.com", "pass123", "usr-alice");
        let config = SyncConfig::default();

        let login = SyncClient::new(server.clone(), config.clone());
        let session = login.login("alice@example.com", "pass123").expect("login");

        let store = SessionStore::with_session(Some(Session::new(
            String::new(),
            session.refresh_token.clone(),
            0,
        )));
        let slow = SyncClient::with_session_store(server.clone(), config.clone(), store.clone());
        let other = SyncClient::with_session_store(server.clone(), config, store.clone());

        // The other request refreshes the token first.
        other.pull(Some("books"), 0).expect("the other request");
        let newest = store.current().expect("a session");
        assert_ne!(newest.refresh_token, session.refresh_token);

        // The request that started earlier reports the newest session.
        assert_eq!(slow.session().expect("a session"), newest);
    }

    /// The same two requests without a shared session: each refreshes on its
    /// own, the second spends a token the first already rotated, and one of
    /// the requests is refused although the account is fine.
    ///
    /// This is what the shared session prevents.
    #[test]
    fn two_requests_without_a_shared_session_spend_the_token_twice() {
        let server = MockServer::new();
        server.add_user("alice@example.com", "pass123", "usr-alice");
        let config = SyncConfig::default();

        let login = SyncClient::new(server.clone(), config.clone());
        let session = login.login("alice@example.com", "pass123").expect("login");
        let expired = || {
            Some(Session::new(
                String::new(),
                session.refresh_token.clone(),
                0,
            ))
        };

        let client_a = SyncClient::with_session_store(
            server.clone(),
            config.clone(),
            SessionStore::with_session(expired()),
        );
        let client_b = SyncClient::with_session_store(
            server.clone(),
            config,
            SessionStore::with_session(expired()),
        );

        let (first, second) = std::thread::scope(|scope| {
            let a = scope.spawn(|| client_a.pull(Some("books"), 0));
            let b = scope.spawn(|| client_b.pull(Some("books"), 0));
            (
                a.join().expect("first thread"),
                b.join().expect("second thread"),
            )
        });

        let failures = usize::from(first.is_err()) + usize::from(second.is_err());
        assert_eq!(failures, 1, "one request is refused: {first:?} {second:?}");
        assert_eq!(server.refresh_count(), 2, "the token was spent twice");
    }
}
