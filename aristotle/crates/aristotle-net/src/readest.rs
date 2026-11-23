// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readest transport implementing `inkread_core::sync::HttpTransport` over `aristotle_net::http::Client`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use inkread_core::sync::{HttpRequest, HttpResponse, HttpTransport, TransportError};

use crate::http::{Client, Method, Request, Timeouts};
use crate::policy::Policy;
use crate::url::Url;

/// Body limit for Readest API responses (20 MiB).
pub const MAX_READEST_BODY_BYTES: u64 = 20 * 1024 * 1024;

/// Default timeout for Readest API calls (15 seconds).
pub const READEST_TIMEOUT_SECS: u64 = 15;

/// Connect/TLS handshake timeout: fail fast on stalled edge IPs so a retry can
/// re-dial promptly instead of blocking for the whole response budget.
const READEST_CONNECT_TIMEOUT_SECS: u64 = 8;

/// Response header timeout for default Readest API calls.
const READEST_RESPONSE_TIMEOUT_SECS: u64 = 25;

/// Response body timeout for default Readest API calls.
const READEST_BODY_TIMEOUT_SECS: u64 = 30;

/// Maximum retries after an initial transient network failure (3 attempts total).
const MAX_RETRIES: usize = 2;

/// Delay between retry attempts.
const RETRY_DELAY: Duration = Duration::from_millis(50);

/// An HTTP transport implementation wrapping a shared `http::Client` with
/// device policy, Keep-Alive pooling, and safe retries.
pub struct ReadestTransport {
    client: Arc<Client>,
    max_body_bytes: u64,
}

impl ReadestTransport {
    pub fn new(policy: Policy) -> Self {
        let timeouts = Timeouts {
            connect: Duration::from_secs(READEST_CONNECT_TIMEOUT_SECS),
            response: Duration::from_secs(READEST_RESPONSE_TIMEOUT_SECS),
            body: Duration::from_secs(READEST_BODY_TIMEOUT_SECS),
        };
        Self {
            client: Client::shared(policy, timeouts),
            max_body_bytes: MAX_READEST_BODY_BYTES,
        }
    }

    /// A transport with an explicit timeout, for short pushes on sleep/exit or
    /// generous full-sync rounds.
    pub fn with_timeout(policy: Policy, secs: u64) -> Self {
        Self::with_timeout_and_limit(policy, secs, MAX_READEST_BODY_BYTES)
    }

    /// A transport with an explicit timeout and response body size limit.
    pub fn with_timeout_and_limit(policy: Policy, secs: u64, max_body_bytes: u64) -> Self {
        let timeouts = Timeouts {
            connect: Duration::from_secs(secs.min(READEST_CONNECT_TIMEOUT_SECS)),
            response: Duration::from_secs(secs),
            body: Duration::from_secs(secs),
        };
        Self {
            client: Client::shared(policy, timeouts),
            max_body_bytes,
        }
    }

    pub fn with_client(client: Client) -> Self {
        Self {
            client: Arc::new(client),
            max_body_bytes: MAX_READEST_BODY_BYTES,
        }
    }
}

impl fmt::Debug for ReadestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadestTransport").finish_non_exhaustive()
    }
}

fn map_http_error(e: crate::http::Error) -> TransportError {
    match e {
        crate::http::Error::Timeout => TransportError::Timeout,
        crate::http::Error::Connect(d) => TransportError::Network(d),
        // The device itself refused the address: the network is
        // not at fault, and the UI must not say it is.
        crate::http::Error::NotAllowed(h) => {
            TransportError::Blocked(format!("{h} is not allowed by policy"))
        }
        crate::http::Error::NoTls => {
            TransportError::Blocked("HTTPS is not supported in this build".to_string())
        }
        crate::http::Error::Tls { detail, .. } => {
            TransportError::Network(format!("TLS error: {detail}"))
        }
        crate::http::Error::Status(s, b) => TransportError::HttpStatus(s, b),
        crate::http::Error::TooLarge(n) => TransportError::TooLarge(n),
        crate::http::Error::Io(io) => TransportError::Network(io.to_string()),
        other => TransportError::Other(other.to_string()),
    }
}

impl HttpTransport for ReadestTransport {
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let url = Url::parse(&req.url)
            .map_err(|e| TransportError::Network(format!("invalid URL: {e}")))?;

        let method = match req.method.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PROPFIND" => Method::Propfind,
            "MKCOL" => Method::Mkcol,
            "HEAD" => Method::Head,
            "DELETE" => Method::Delete,
            other => {
                return Err(TransportError::Other(format!(
                    "unsupported HTTP method {other}"
                )))
            }
        };

        let http_req = Request {
            method,
            url,
            headers: req.headers,
            body: if req.body.is_empty() {
                None
            } else {
                Some(req.body)
            },
            credentials: None,
        };

        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            // Always send through `self.client`: ureq drops any broken socket
            // on error rather than returning it to the pool, so retrying via
            // the same shared agent dials a fresh socket (via Happy Eyeballs
            // and the updated fast-IP table) AND keeps the winning Keep-Alive
            // connection warm for subsequent requests.
            let res = self.client.send(&http_req, self.max_body_bytes);

            match res {
                Ok(response) => {
                    return Ok(HttpResponse {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                    });
                }
                Err(e) => {
                    let retryable = if method.is_idempotent_read() {
                        e.is_idempotent_retryable()
                    } else {
                        e.is_pre_send_retryable()
                    };
                    if retryable && attempt < MAX_RETRIES {
                        std::thread::sleep(RETRY_DELAY);
                        continue;
                    }
                    last_err = Some(e);
                    break;
                }
            }
        }

        Err(map_http_error(
            last_err.expect("retry loop exits with Ok or last_err"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    use inkread_core::model::{Annotation, NoteType};
    use inkread_core::sync::mock::MockServer;
    use inkread_core::sync::{merge_notes, SyncClient, SyncConfig};

    fn run_test_mock_server(
        listener: TcpListener,
        server: Arc<MockServer>,
        running: Arc<AtomicBool>,
    ) {
        let addr = listener.local_addr().unwrap();
        let port = addr.port();

        while running.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(conn) => conn,
                Err(_) => break,
            };

            let s = Arc::clone(&server);
            thread::spawn(move || {
                let mut buffer = Vec::new();
                let mut temp = [0u8; 4096];

                let (header_end, body_start) = loop {
                    match stream.read(&mut temp) {
                        Ok(0) => return,
                        Ok(n) => {
                            buffer.extend_from_slice(&temp[..n]);
                            if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                                break (pos, pos + 4);
                            }
                        }
                        Err(_) => return,
                    }
                };

                let header_str = String::from_utf8_lossy(&buffer[..header_end]);
                let mut lines = header_str.lines();
                let request_line = match lines.next() {
                    Some(l) => l,
                    None => return,
                };
                let parts: Vec<&str> = request_line.split_whitespace().collect();
                if parts.len() < 2 {
                    return;
                }
                let method = parts[0];
                let path = parts[1];

                let mut content_length = 0usize;
                let mut headers = Vec::new();

                for line in lines {
                    if let Some((k, v)) = line.split_once(':') {
                        let key = k.trim().to_string();
                        let val = v.trim().to_string();
                        if key.eq_ignore_ascii_case("content-length") {
                            if let Ok(len) = val.parse::<usize>() {
                                content_length = len;
                            }
                        }
                        headers.push((key, val));
                    }
                }

                let mut body = buffer[body_start..].to_vec();
                while body.len() < content_length {
                    match stream.read(&mut temp) {
                        Ok(0) => break,
                        Ok(n) => {
                            body.extend_from_slice(&temp[..n]);
                        }
                        Err(_) => break,
                    }
                }
                if body.len() > content_length {
                    body.truncate(content_length);
                }

                let url = format!("http://127.0.0.1:{port}{path}");
                let mut http_req = HttpRequest::new(method, url).with_body(body);
                for (k, v) in headers {
                    http_req = http_req.with_header(k, v);
                }

                let (status_code, status_text, resp_headers, resp_body) =
                    match s.handle_request(http_req) {
                        Ok(resp) => {
                            let text = match resp.status {
                                200 => "OK",
                                201 => "Created",
                                400 => "Bad Request",
                                401 => "Unauthorized",
                                403 => "Forbidden",
                                404 => "Not Found",
                                500 => "Internal Server Error",
                                _ => "Response",
                            };
                            (resp.status, text, resp.headers, resp.body)
                        }
                        Err(e) => (
                            500,
                            "Internal Server Error",
                            Vec::new(),
                            e.to_string().into_bytes(),
                        ),
                    };

                let mut response = format!(
                    "HTTP/1.1 {status_code} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp_body.len()
                );
                let has_content_type = resp_headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
                if !has_content_type {
                    response.push_str("Content-Type: application/json\r\n");
                }
                for (k, v) in resp_headers {
                    response.push_str(&format!("{k}: {v}\r\n"));
                }
                response.push_str("\r\n");

                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&resp_body);
                let _ = stream.flush();
            });
        }
    }

    #[test]
    fn a_blocked_auth_host_is_not_reported_as_a_network_failure() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let mock_server = Arc::new(MockServer::new());
        let server_clone = Arc::clone(&mock_server);
        let running_clone = Arc::clone(&running);
        let server_thread = thread::spawn(move || {
            run_test_mock_server(listener, server_clone, running_clone);
        });

        // The device allows the API host but not the authentication host: a
        // scoped `net-allow` that was written before the auth default moved
        // looks exactly like this. Both names reach the same mock server.
        let mut policy = Policy::open();
        policy.scoped = true;
        policy.allowed = crate::policy::parse_allow(&format!("127.0.0.1:{port}"));

        let config = SyncConfig {
            api_base: format!("http://127.0.0.1:{port}/api"),
            supabase_url: format!("http://localhost:{port}"),
            supabase_anon_key: SyncConfig::default().supabase_anon_key,
        };
        let transport = ReadestTransport::new(policy);
        let client = SyncClient::new(transport, config);

        // 1. A live access token: the pull never touches the auth host, so it
        //    works, as the cloud shelf does.
        client.set_session(Some(inkread_core::sync::Session::new(
            "test-access-token",
            "test-refresh-token",
            i64::MAX,
        )));
        let pulled = client
            .pull(Some("books"), 0)
            .expect("pull with a live token");
        assert!(pulled.configs.is_empty() && pulled.notes.is_empty());

        // 2. An expired token: the client must refresh first, and the refresh
        //    is refused by the device's policy. That is not a network failure.
        client.set_session(Some(inkread_core::sync::Session::new(
            "test-access-token",
            "test-refresh-token",
            0,
        )));
        let err = client.pull(Some("books"), 0).expect_err("refresh blocked");
        assert_eq!(
            err.kind(),
            inkread_core::sync::SyncErrorKind::Blocked,
            "{err:?}"
        );

        running.store(false, Ordering::Relaxed);
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        let _ = server_thread.join();
    }

    #[test]
    fn login_uses_the_endpoints_from_the_settings() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let mock_server = Arc::new(MockServer::new());
        let server_clone = Arc::clone(&mock_server);
        let running_clone = Arc::clone(&running);
        let server_thread = thread::spawn(move || {
            run_test_mock_server(listener, server_clone, running_clone);
        });

        let accounts = crate::store::Accounts {
            readest_endpoints: crate::store::ReadestEndpoints {
                api_base: Some(format!("http://127.0.0.1:{port}/api")),
                supabase_url: Some(format!("http://127.0.0.1:{port}")),
                supabase_anon_key: None,
            },
            ..crate::store::Accounts::default()
        };

        // The login request reads the same configuration as sync and cloud.
        let config = accounts.readest_config();
        assert_eq!(config.api_base, format!("http://127.0.0.1:{port}/api"));
        let client = SyncClient::new(ReadestTransport::new(Policy::open()), config);
        let session = client.login("test@example.com", "password123").unwrap();
        assert!(session.access_token.starts_with("mock-acc-"));

        running.store(false, Ordering::Relaxed);
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        let _ = server_thread.join();
    }

    #[test]
    fn readest_transport_end_to_end_mock_integration() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));

        let mock_server = Arc::new(MockServer::new());
        let server_clone = Arc::clone(&mock_server);
        let running_clone = Arc::clone(&running);

        let server_thread = thread::spawn(move || {
            run_test_mock_server(listener, server_clone, running_clone);
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let transport1 = ReadestTransport::new(policy.clone());
        let transport2 = ReadestTransport::new(policy);

        let config = SyncConfig {
            api_base: format!("http://127.0.0.1:{port}/api"),
            supabase_url: format!("http://127.0.0.1:{port}"),
            supabase_anon_key: SyncConfig::default().supabase_anon_key,
        };

        // 1. Device 1 logs in
        let client1 = SyncClient::new(transport1, config.clone());
        let session1 = client1.login("test@example.com", "password123").unwrap();
        assert!(session1.access_token.starts_with("mock-acc-"));

        // 2. Device 1 creates an annotation and pushes
        let ann1 = Annotation {
            id: Some("note-d1".to_string()),
            kind: NoteType::Annotation,
            position: Some((
                "/body/DocFragment[1]/p[2].0".to_string(),
                "/body/DocFragment[1]/p[2].10".to_string(),
            )),
            color: Some("yellow".to_string()),
            style: Some("highlight".to_string()),
            text: Some("Device 1 highlight".to_string()),
            note: Some("Read on kindle".to_string()),
            created_at: Some(1762340000000),
            updated_at: Some(1762340000000),
            deleted_at: None,
        };
        let db_note1 = ann1.to_db_book_note("book-hash-99", "note-d1");

        let push_req = inkread_core::sync::SyncPushRequest {
            books: vec![],
            configs: vec![],
            notes: vec![db_note1.clone()],
            stat_books: vec![],
            stat_pages: vec![],
        };
        let push_resp = client1.push(push_req, None).unwrap();
        assert_eq!(push_resp.notes.len(), 1);

        // 3. Device 2 logs in and pulls
        let client2 = SyncClient::new(transport2, config);
        let _session2 = client2.login("test@example.com", "password123").unwrap();

        let pull_resp = client2.pull(Some("notes"), 0).unwrap();
        assert_eq!(pull_resp.notes.len(), 1);
        let pulled_db_note = &pull_resp.notes[0];
        assert_eq!(pulled_db_note.id, "note-d1");
        assert_eq!(pulled_db_note.text.as_deref(), Some("Device 1 highlight"));

        // 4. Device 2 converts to local Annotation and verifies
        let ann2 = Annotation::from_db_book_note(pulled_db_note);
        assert_eq!(ann2.text.as_deref(), Some("Device 1 highlight"));
        assert_eq!(ann2.color.as_deref(), Some("yellow"));

        // 5. Merge notes locally
        let local_notes = vec![db_note1];
        let merged = merge_notes(&local_notes, &pull_resp.notes);
        assert_eq!(merged.len(), 1);

        // Cleanup
        running.store(false, Ordering::Relaxed);
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        let _ = server_thread.join();
    }

    #[test]
    fn transport_forwards_custom_response_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let resp = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/zip\r\n",
                "encryptkey: secret-envelope-123\r\n",
                "Set-Cookie: wr_skey=cookie-a; Path=/\r\n",
                "Set-Cookie: wr_rt=cookie-b; Path=/\r\n",
                "Content-Length: 2\r\n",
                "Connection: close\r\n",
                "\r\n",
                "PK",
            );
            stream.write_all(resp.as_bytes()).unwrap();
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let transport = ReadestTransport::new(policy);
        let req = HttpRequest::new(
            "GET",
            format!("http://127.0.0.1:{port}/book/chapterdownload"),
        );
        let resp = transport.send(req).unwrap();
        server.join().unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"PK");
        assert!(resp
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("encryptkey") && v == "secret-envelope-123"));
        let cookies: Vec<&str> = resp
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            cookies,
            vec!["wr_skey=cookie-a; Path=/", "wr_rt=cookie-b; Path=/"]
        );
    }

    #[test]
    fn transport_enforces_custom_body_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = vec![b'x'; 64];
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let small = ReadestTransport::with_timeout_and_limit(policy.clone(), 5, 32);
        let err = small
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/big"),
            ))
            .expect_err("body exceeds custom limit");
        assert!(matches!(err, TransportError::TooLarge(_)), "{err:?}");

        let large = ReadestTransport::with_timeout_and_limit(policy, 5, 128);
        let resp = large
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/big"),
            ))
            .expect("body fits custom limit");
        assert_eq!(resp.body.len(), 64);

        server.join().unwrap();
    }

    #[test]
    fn shared_transport_reuses_keepalive_connection() {
        use std::sync::atomic::AtomicUsize;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let accepted_srv = Arc::clone(&accepted);
        let running_srv = Arc::clone(&running);
        let server = thread::spawn(move || {
            let mut handlers = Vec::new();
            while running_srv.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if !running_srv.load(Ordering::SeqCst) {
                    break;
                }
                accepted_srv.fetch_add(1, Ordering::SeqCst);
                handlers.push(thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 1024];
                    for req_idx in 0..2 {
                        let header_end = loop {
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break pos + 4;
                            }
                            match stream.read(&mut tmp) {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            }
                        };
                        buf.drain(..header_end);
                        let (conn, body) = if req_idx == 0 {
                            ("keep-alive", b"ok-1")
                        } else {
                            ("close", b"ok-2")
                        };
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: {conn}\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(body);
                        let _ = stream.flush();
                    }
                }));
            }
            for h in handlers {
                let _ = h.join();
            }
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let transport1 = ReadestTransport::new(policy.clone());
        let resp1 = transport1
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/first"),
            ))
            .unwrap();
        assert_eq!(resp1.status, 200);
        assert_eq!(resp1.body, b"ok-1");

        // A second ReadestTransport constructed independently with the same
        // policy must reuse the pooled Keep-Alive TCP socket.
        let transport2 = ReadestTransport::new(policy);
        let resp2 = transport2
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/second"),
            ))
            .unwrap();
        assert_eq!(resp2.status, 200);
        assert_eq!(resp2.body, b"ok-2");

        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "two separate ReadestTransport::new calls should reuse 1 TCP connection"
        );

        running.store(false, Ordering::SeqCst);
        let _ = TcpStream::connect(addr);
        server.join().unwrap();
    }

    #[test]
    fn transport_retries_transient_connect_drop_and_succeeds() {
        use std::sync::atomic::AtomicUsize;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_srv = Arc::clone(&accepted);

        let server = thread::spawn(move || {
            // First connection: accept and immediately drop without replying.
            let (first, _) = listener.accept().unwrap();
            accepted_srv.fetch_add(1, Ordering::SeqCst);
            drop(first);

            // Second connection: read request and return 200 OK.
            let (mut second, _) = listener.accept().unwrap();
            accepted_srv.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = second.read(&mut buf);
            let resp = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 11\r\n",
                "Connection: close\r\n",
                "\r\n",
                "{\"ok\":true}",
            );
            second.write_all(resp.as_bytes()).unwrap();
            second.flush().unwrap();
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let transport = ReadestTransport::new(policy);
        let resp = transport
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/flaky"),
            ))
            .expect("retry should recover from dropped connection");
        server.join().unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, br#"{"ok":true}"#);
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn transport_decompresses_gzip_response() {
        // gzip.compress(b'{"ok":true,"sync":"gzip-works"}', mtime=0)
        const GZIP_BODY: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xab, 0x56, 0xca, 0xcf,
            0x56, 0xb2, 0x2a, 0x29, 0x2a, 0x4d, 0xd5, 0x51, 0x2a, 0xae, 0xcc, 0x4b, 0x56, 0xb2,
            0x52, 0x4a, 0xaf, 0xca, 0x2c, 0xd0, 0x2d, 0xcf, 0x2f, 0xca, 0x2e, 0x56, 0xaa, 0x05,
            0x00, 0xb4, 0x98, 0x3d, 0x31, 0x1f, 0x00, 0x00, 0x00,
        ];

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = stream.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let req_text = String::from_utf8_lossy(&buf).to_ascii_lowercase();
            assert!(
                req_text.contains("accept-encoding:") && req_text.contains("gzip"),
                "client should advertise Accept-Encoding: gzip; got:\n{req_text}"
            );
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                GZIP_BODY.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(GZIP_BODY).unwrap();
            stream.flush().unwrap();
        });

        let mut policy = Policy::open();
        policy.scoped = true;
        policy.ensure_allowed(&format!("http://127.0.0.1:{port}"));

        let transport = ReadestTransport::new(policy);
        let resp = transport
            .send(HttpRequest::new(
                "GET",
                format!("http://127.0.0.1:{port}/gzip"),
            ))
            .expect("gzip response should decompress cleanly");
        server.join().unwrap();

        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, br#"{"ok":true,"sync":"gzip-works"}"#);
    }
}
