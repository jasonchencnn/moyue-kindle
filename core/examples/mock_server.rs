//! Standalone Readest mock server running over std `TcpListener` (HTTP/1.1).
//!
//! Provides a local HTTP port for device and simulator testing without third-party dependencies.
//!
//! Usage:
//! ```bash
//! cargo run -p inkread-core --example mock_server --features mock -- 8080
//! ```
//!
//! For a device on the same LAN, bind an address it can reach and give the
//! server the address the device will use:
//! ```bash
//! cargo run -p inkread-core --example mock_server --features mock -- \
//!   --bind 0.0.0.0 --port 8090 --public-base http://192.168.1.20:8090 \
//!   --seed-book /path/to/book.epub
//! ```

use std::env;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread;

use inkread_core::hash::partial_md5;
use inkread_core::sync::mock::MockServer;
use inkread_core::sync::records::DbBook;
use inkread_core::sync::time::format_iso8601;
use inkread_core::sync::transport::HttpRequest;

/// Command line for [`main`], parsed by hand to keep the example dependency free.
struct Args {
    bind: IpAddr,
    port: u16,
    public_base: Option<String>,
    seed_books: Vec<String>,
    /// No arguments at all: keep the old "fall back to a random port" behavior.
    legacy: bool,
}

impl Args {
    fn parse(raw: Vec<String>) -> Result<Self, String> {
        let mut args = Args {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 8080,
            public_base: None,
            seed_books: Vec::new(),
            legacy: raw.is_empty(),
        };

        let mut iter = raw.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--bind" => {
                    let value = iter.next().ok_or("--bind needs an IP address")?;
                    args.bind = value
                        .parse()
                        .map_err(|_| format!("invalid --bind address: {value}"))?;
                }
                "--port" => {
                    let value = iter.next().ok_or("--port needs a number")?;
                    args.port = value
                        .parse()
                        .map_err(|_| format!("invalid --port: {value}"))?;
                }
                "--public-base" => {
                    let value = iter.next().ok_or("--public-base needs a URL")?;
                    args.public_base = Some(value.trim_end_matches('/').to_string());
                }
                "--seed-book" => {
                    let value = iter.next().ok_or("--seed-book needs a path")?;
                    args.seed_books.push(value);
                }
                other if !other.starts_with('-') => {
                    args.port = other
                        .parse()
                        .map_err(|_| format!("invalid port: {other}"))?;
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }

        Ok(args)
    }
}

/// Current wall clock in Unix epoch milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Read each path, hash it, and insert it as a cloud book for the default user.
fn seed_books(server: &MockServer, paths: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    for path in paths {
        let path_ref = Path::new(path);
        let data = std::fs::read(path_ref)?;
        let hash = partial_md5(&mut std::io::Cursor::new(&data))?;
        let title = path_ref
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        let format = path_ref
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("epub")
            .to_ascii_lowercase();
        let size = data.len();
        // `insert_book_with_file` only fills `uploaded_at`; the sync endpoint
        // selects on `synced_at`/`updated_at`, so a seeded book needs both.
        let now = format_iso8601(now_ms());
        let file_key = server.insert_book_with_file(
            "usr-01",
            DbBook {
                book_hash: hash.clone(),
                title,
                author: String::new(),
                format,
                created_at: Some(now.clone()),
                updated_at: Some(now.clone()),
                synced_at: Some(now),
                ..Default::default()
            },
            data,
        );
        println!("[mock_server] Seeded {path}: hash={hash} size={size} bytes fileKey={file_key}");
    }
    Ok(())
}

fn handle_client(mut stream: TcpStream, server: Arc<MockServer>, local_port: u16) {
    let mut buffer = Vec::new();
    let mut temp = [0u8; 4096];

    // Read headers until \r\n\r\n
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

    // Read remaining body bytes if needed
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

    let url = format!("http://127.0.0.1:{local_port}{path}");
    let mut http_req = HttpRequest::new(method, url).with_body(body);
    for (k, v) in headers {
        http_req = http_req.with_header(k, v);
    }

    let (status_code, status_text, resp_headers, resp_body) = match server.handle_request(http_req)
    {
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
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw: Vec<String> = env::args().skip(1).collect();
    let args = Args::parse(raw)?;

    let addr = SocketAddr::new(args.bind, args.port);
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) if !args.legacy => {
            return Err(format!("cannot bind {addr}: {e}").into());
        }
        Err(_) => {
            println!(
                "[mock_server] Port {} busy, binding to random available port...",
                args.port
            );
            TcpListener::bind(SocketAddr::new(args.bind, 0))?
        }
    };

    let actual_addr = listener.local_addr()?;
    let actual_port = actual_addr.port();

    let server = Arc::new(MockServer::new());
    if let Some(base) = &args.public_base {
        server.set_download_base(&format!("{base}/download"));
    }

    print_banner(actual_addr, args.public_base.as_deref());
    seed_books(&server, &args.seed_books)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let s = Arc::clone(&server);
                thread::spawn(move || {
                    handle_client(stream, s, actual_port);
                });
            }
            Err(e) => {
                eprintln!("[mock_server] Accept error: {e}");
            }
        }
    }

    Ok(())
}

/// Print the URLs and credentials the device needs for this server.
fn print_banner(actual_addr: SocketAddr, public_base: Option<&str>) {
    println!("[mock_server] Readest Mock Server listening on http://{actual_addr}");
    println!("[mock_server] Default credentials: test@example.com / password123");
    println!("[mock_server] Sync endpoint: http://{actual_addr}/api/sync");
    println!("[mock_server] Auth endpoint: http://{actual_addr}/auth/v1/token");

    if let Some(base) = public_base {
        println!("[mock_server] Device API base: {base}/api");
        println!("[mock_server] Device Supabase URL: {base}");
        println!("[mock_server] Signed download base: {base}/download");
    } else {
        println!(
            "[mock_server] Public base not set; signed download URLs use \
             https://mock.storage.local/download and only the in-process mock can fetch them"
        );
    }
}
