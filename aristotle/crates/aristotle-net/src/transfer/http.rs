// SPDX-License-Identifier: AGPL-3.0-or-later
//! The little HTTP server the browser talks to.
//!
//! It speaks just enough HTTP for one page and one upload: a `GET` for
//! the page, a `POST` for the file. There is no framework behind it and no
//! TLS: the server listens on the device's own network for as long as the
//! page is open, and it is the browser on the same network that connects.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};

/// The page the browser gets.
pub const PAGE: &str = include_str!("page.html");

/// The largest request head that is read, so a long line cannot eat the
/// device's memory.
const MAX_HEAD: usize = 16 * 1024;

/// What the browser asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The page itself.
    Get,
    /// A file coming in.
    Post {
        /// The `Content-Type` header, with its boundary.
        content_type: String,
        /// How many bytes the body has.
        length: u64,
        /// How many bytes the file itself has, when the browser said.
        ///
        /// The browser knows the size of the file it is sending and says so
        /// in the query string; without it the device cannot say how much
        /// is still to come.
        size: u64,
    },
    /// Something else.
    Other,
}

/// A request whose head could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The browser stopped before the head was over.
    Truncated,
    /// The head was longer than the server will read.
    TooLong,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Truncated => write!(f, "the request stops early"),
            RequestError::TooLong => write!(f, "the request head is too long"),
        }
    }
}

impl std::error::Error for RequestError {}

/// Reads the head of a request and says what it is.
///
/// The body is left in the stream for the caller to read.
pub fn read_request<R: Read>(stream: &mut R) -> io::Result<Request> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    // The head ends at the empty line.
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > MAX_HEAD {
            return Err(io::Error::other(RequestError::TooLong));
        }
        let n = stream.read(&mut byte)?;
        if n == 0 {
            return Err(io::Error::other(RequestError::Truncated));
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");
    let mut content_type = String::new();
    let mut length = 0u64;
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-type") {
            content_type = value.to_string();
        } else if key.eq_ignore_ascii_case("content-length") {
            length = value.parse().unwrap_or(0);
        }
    }
    let path = raw_path.split('?').next().unwrap_or("");
    let query = path_query(raw_path);
    Ok(match method {
        "GET" if path == "/" || path == "/index.html" => Request::Get,
        "POST" if path == "/upload" => Request::Post {
            content_type,
            length,
            size: query_value(query, "size")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        },
        _ => Request::Other,
    })
}

/// The query string of a request line, without the `?`.
fn path_query(path: &str) -> &str {
    path.split_once('?').map_or("", |(_, q)| q)
}

/// One value of the query string, by name.
fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Writes the page and ends the reply.
pub fn write_page(stream: &mut TcpStream) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        PAGE.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(PAGE.as_bytes())?;
    stream.flush()
}

/// Writes a short reply and ends it.
pub fn write_reply(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// Binds the server to a port on every interface of the device.
///
/// The port asked for is used when it is free; otherwise the next ports
/// are tried, so a stale server from a page that was closed does not stop
/// the new one. The port the server actually got is returned: asking for
/// port 0 lets the operating system pick one.
pub fn bind(port: u16) -> io::Result<(TcpListener, u16)> {
    let mut last = None;
    for try_port in port..port.saturating_add(16) {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, try_port)) {
            Ok(listener) => {
                let port = listener.local_addr().map(|a| a.port()).unwrap_or(try_port);
                return Ok((listener, port));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no free port")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    /// A stream that reads from a string, so the request reader can be
    /// tested without a socket.
    fn reader(text: &str) -> impl Read + '_ {
        BufReader::new(text.as_bytes())
    }

    #[test]
    fn a_get_for_the_page_is_read() {
        let mut text = reader("GET / HTTP/1.1\r\nHost: kindle\r\n\r\n");
        assert_eq!(read_request(&mut text).unwrap(), Request::Get);
    }

    #[test]
    fn a_post_for_an_upload_is_read_with_its_head() {
        let mut text = reader(
            "POST /upload HTTP/1.1\r\n\
             Content-Type: multipart/form-data; boundary=ABC\r\n\
             Content-Length: 1234\r\n\r\n",
        );
        let request = read_request(&mut text).unwrap();
        assert_eq!(
            request,
            Request::Post {
                content_type: "multipart/form-data; boundary=ABC".to_string(),
                length: 1234,
                size: 0,
            }
        );
    }

    #[test]
    fn the_size_the_browser_says_is_read() {
        let mut text = reader("POST /upload?size=4096 HTTP/1.1\r\nContent-Length: 10\r\n\r\n");
        let request = read_request(&mut text).unwrap();
        assert_eq!(
            request,
            Request::Post {
                content_type: String::new(),
                length: 10,
                size: 4096,
            }
        );
    }

    #[test]
    fn a_size_that_is_not_a_number_is_no_size() {
        let mut text = reader("POST /upload?size=abc HTTP/1.1\r\nContent-Length: 10\r\n\r\n");
        let request = read_request(&mut text).unwrap();
        assert_eq!(
            request,
            Request::Post {
                content_type: String::new(),
                length: 10,
                size: 0,
            }
        );
    }

    #[test]
    fn a_query_string_does_not_change_the_page() {
        let mut text = reader("GET /?x=1 HTTP/1.1\r\n\r\n");
        assert_eq!(read_request(&mut text).unwrap(), Request::Get);
    }

    #[test]
    fn another_path_is_not_the_page() {
        let mut text = reader("GET /elsewhere HTTP/1.1\r\n\r\n");
        assert_eq!(read_request(&mut text).unwrap(), Request::Other);
        let mut text = reader("DELETE /upload HTTP/1.1\r\n\r\n");
        assert_eq!(read_request(&mut text).unwrap(), Request::Other);
    }

    #[test]
    fn a_head_that_stops_early_is_refused() {
        let mut text = reader("GET / HTTP/1.1\r\n");
        let err = read_request(&mut text).unwrap_err();
        assert_eq!(err.to_string(), RequestError::Truncated.to_string());
    }

    #[test]
    fn a_very_long_head_is_refused() {
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "y".repeat(MAX_HEAD));
        let mut text = reader(&long);
        let err = read_request(&mut text).unwrap_err();
        assert_eq!(err.to_string(), RequestError::TooLong.to_string());
    }

    #[test]
    fn the_page_carries_the_steps_and_no_nothing_else() {
        // The page must work with no network beyond the device, so it may
        // not point at anything outside itself.
        assert!(!PAGE.contains("http://"));
        assert!(!PAGE.contains("https://"));
        assert!(!PAGE.contains("//cdn"));
    }
}
