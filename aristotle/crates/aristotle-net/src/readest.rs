// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readest transport implementing `inkread_core::sync::HttpTransport` over `aristotle_net::http::Client`.

use std::fmt;
use std::time::Duration;

use inkread_core::sync::{HttpRequest, HttpResponse, HttpTransport, TransportError};

use crate::http::{Client, Method, Request, Timeouts};
use crate::policy::Policy;
use crate::url::Url;

/// Body limit for Readest API responses (20 MiB).
pub const MAX_READEST_BODY_BYTES: u64 = 20 * 1024 * 1024;

/// Default timeout for Readest API calls (15 seconds).
pub const READEST_TIMEOUT_SECS: u64 = 15;

/// An HTTP transport implementation wrapping `http::Client` with device policy and timeouts.
pub struct ReadestTransport {
    client: Client,
}

impl ReadestTransport {
    pub fn new(policy: Policy) -> Self {
        let timeouts = Timeouts::quick(Duration::from_secs(READEST_TIMEOUT_SECS));
        Self {
            client: Client::new(policy, timeouts),
        }
    }

    /// A transport that gives up sooner, for pushes on the way to sleep.
    pub fn with_timeout(policy: Policy, secs: u64) -> Self {
        let timeouts = Timeouts::quick(Duration::from_secs(secs));
        Self {
            client: Client::new(policy, timeouts),
        }
    }

    pub fn with_client(client: Client) -> Self {
        Self { client }
    }
}

impl fmt::Debug for ReadestTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadestTransport").finish_non_exhaustive()
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

        let response = match self.client.send(&http_req, MAX_READEST_BODY_BYTES) {
            Ok(resp) => resp,
            Err(e) => {
                return Err(match e {
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
                    other => TransportError::Other(other.to_string()),
                });
            }
        };

        let mut resp_headers = Vec::new();
        if let Some(ct) = response.content_type {
            resp_headers.push(("content-type".to_string(), ct));
        }

        Ok(HttpResponse {
            status: response.status,
            headers: resp_headers,
            body: response.body,
        })
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
}
