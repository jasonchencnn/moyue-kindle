// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `multipart/form-data` bodies a browser sends when a file is dropped
//! on the page.
//!
//! The parser reads the body in pieces and hands back the parts one at a
//! time, so a file larger than the memory of the device never has to be
//! held whole. A part's bytes are given to a callback as they arrive; the
//! caller writes them to the file it is making.
//!
//! A request that stops in the middle is refused: a part that never ends
//! is an error, not a file.

use std::io::{self, Read};

/// One part of a multipart body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The `name` of the form field.
    pub name: String,
    /// The file name, if the part carries a file.
    pub filename: Option<String>,
}

/// What the parser found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A part begins; its headers are here.
    Start(Part),
    /// Some more of the part's bytes.
    Data(Vec<u8>),
    /// The part is over.
    End,
    /// The whole body is over.
    Done,
}

/// A body that is not a multipart body, or one that stops early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultipartError {
    /// The `Content-Type` did not name a boundary.
    NoBoundary,
    /// The body ended in the middle of a part.
    Truncated,
    /// A part's headers could not be read.
    BadHeaders,
}

impl std::fmt::Display for MultipartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MultipartError::NoBoundary => write!(f, "the request names no boundary"),
            MultipartError::Truncated => write!(f, "the request stops in the middle"),
            MultipartError::BadHeaders => write!(f, "a part's headers are broken"),
        }
    }
}

impl std::error::Error for MultipartError {}

/// Reads a multipart body part by part.
pub struct Multipart<R: Read> {
    reader: R,
    boundary: Vec<u8>,
    /// The bytes of the part being read, left over from the last read.
    buffer: Vec<u8>,
    /// What the parser is doing now.
    state: State,
    /// The part being read.
    current: Part,
    /// True once the closing boundary has been seen.
    done: bool,
}

enum State {
    /// Looking for the first boundary.
    First,
    /// Reading a part's headers.
    Headers,
    /// Reading a part's bytes.
    Body,
    /// Between a part and the next.
    AfterPart,
}

/// The size of one read from the socket. Big enough that the syscall does
/// not cost more than the copying, small enough that the buffer stays in
/// the device's cache.
const CHUNK: usize = 16 * 1024;

impl<R: Read> Multipart<R> {
    /// A parser for a body whose boundary is `boundary`.
    ///
    /// The boundary is the one the `Content-Type` header names, without
    /// the `--` the body puts in front of it: a header that says
    /// `boundary=----X` has `--` + `----X` in the body.
    pub fn new(reader: R, boundary: &str) -> Self {
        let boundary = boundary.as_bytes().to_vec();
        Self {
            reader,
            boundary,
            buffer: Vec::new(),
            state: State::First,
            current: Part {
                name: String::new(),
                filename: None,
            },
            done: false,
        }
    }

    /// Reads the next thing that happens in the body.
    pub fn next_event(&mut self) -> io::Result<Event> {
        loop {
            match self.state {
                State::First => {
                    // The body may begin with a preamble; the first boundary
                    // is what matters.
                    if self.find_first_boundary()? {
                        self.state = State::Headers;
                    } else {
                        return Ok(Event::Done);
                    }
                }
                State::Headers => {
                    let headers = self.read_headers()?;
                    self.current = headers;
                    self.state = State::Body;
                    return Ok(Event::Start(self.current.clone()));
                }
                State::Body => {
                    // Take what is in the buffer up to the delimiter.
                    let (data, hit) = self.take_until_delimiter()?;
                    if !data.is_empty() {
                        return Ok(Event::Data(data));
                    }
                    if hit {
                        self.state = State::AfterPart;
                        return Ok(Event::End);
                    }
                    // The buffer holds less than a delimiter: read more.
                    if !self.fill()? {
                        return Err(io::Error::other(MultipartError::Truncated));
                    }
                }
                State::AfterPart => {
                    // After a part comes either the next boundary or the
                    // end. Both start with `\r\n--`.
                    self.skip_to_delimiter()?;
                    let closing = self.take_closing()?;
                    if closing {
                        self.done = true;
                        self.state = State::Headers;
                        return Ok(Event::Done);
                    }
                    self.state = State::Headers;
                }
            }
        }
    }

    /// True once the whole body has been read.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Reads more bytes from the body. Returns false at the end.
    fn fill(&mut self) -> io::Result<bool> {
        let mut chunk = vec![0u8; CHUNK];
        let n = self.reader.read(&mut chunk)?;
        if n == 0 {
            return Ok(false);
        }
        chunk.truncate(n);
        self.buffer.extend_from_slice(&chunk);
        Ok(true)
    }

    /// The delimiter that ends a part's bytes: `\r\n--boundary`.
    fn delimiter(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(self.boundary.len() + 4);
        d.extend_from_slice(b"\r\n--");
        d.extend_from_slice(&self.boundary);
        d
    }

    /// Looks for the first boundary at the head of the body.
    fn find_first_boundary(&mut self) -> io::Result<bool> {
        let mut marker = Vec::with_capacity(self.boundary.len() + 2);
        marker.extend_from_slice(b"--");
        marker.extend_from_slice(&self.boundary);
        loop {
            if let Some(at) = find(&self.buffer, &marker) {
                self.buffer.drain(..at + marker.len());
                // The boundary line ends with `\r\n` unless it is the last
                // one, which ends with `--`.
                if self.buffer.starts_with(b"--") {
                    self.buffer.drain(..2);
                    self.done = true;
                    return Ok(false);
                }
                if self.buffer.starts_with(b"\r\n") {
                    self.buffer.drain(..2);
                }
                return Ok(true);
            }
            // Keep the tail that could still be the head of the marker.
            if self.buffer.len() > marker.len() {
                let keep = self.buffer.len() - marker.len();
                self.buffer.drain(..keep);
            }
            if !self.fill()? {
                return Ok(false);
            }
        }
    }

    /// Reads one part's headers.
    fn read_headers(&mut self) -> io::Result<Part> {
        loop {
            if let Some(at) = find(&self.buffer, b"\r\n\r\n") {
                let block = self.buffer[..at].to_vec();
                self.buffer.drain(..at + 4);
                return parse_headers(&block);
            }
            if !self.fill()? {
                return Err(io::Error::other(MultipartError::Truncated));
            }
        }
    }

    /// Takes the part's bytes up to the delimiter that ends the part.
    ///
    /// Returns the bytes and whether the delimiter was found. When it was
    /// not, the bytes are still returned: they are the part's own.
    fn take_until_delimiter(&mut self) -> io::Result<(Vec<u8>, bool)> {
        let delimiter = self.delimiter();
        match find(&self.buffer, &delimiter) {
            Some(at) => {
                let data = self.buffer[..at].to_vec();
                self.buffer.drain(..at);
                Ok((data, true))
            }
            None => {
                // The delimiter may be split across two reads, so the tail
                // of the buffer has to stay until more bytes arrive.
                let keep = delimiter.len() - 1;
                if self.buffer.len() <= keep {
                    return Ok((Vec::new(), false));
                }
                let split = self.buffer.len() - keep;
                let data = self.buffer[..split].to_vec();
                self.buffer.drain(..split);
                Ok((data, false))
            }
        }
    }

    /// Skips the bytes between a part and the next delimiter.
    fn skip_to_delimiter(&mut self) -> io::Result<()> {
        let delimiter = self.delimiter();
        loop {
            if let Some(at) = find(&self.buffer, &delimiter) {
                self.buffer.drain(..at + delimiter.len());
                return Ok(());
            }
            if !self.fill()? {
                return Err(io::Error::other(MultipartError::Truncated));
            }
        }
    }

    /// Reads the two bytes after a delimiter: `--` ends the body, `\r\n`
    /// starts the next part.
    fn take_closing(&mut self) -> io::Result<bool> {
        while self.buffer.len() < 2 {
            if !self.fill()? {
                return Err(io::Error::other(MultipartError::Truncated));
            }
        }
        if self.buffer.starts_with(b"--") {
            self.buffer.drain(..2);
            return Ok(true);
        }
        if self.buffer.starts_with(b"\r\n") {
            self.buffer.drain(..2);
        }
        Ok(false)
    }
}

/// Finds `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Reads the `name` and `filename` out of a part's headers.
fn parse_headers(block: &[u8]) -> io::Result<Part> {
    let text = String::from_utf8_lossy(block);
    let mut name = String::new();
    let mut filename = None;
    for line in text.split("\r\n") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for param in value.split(';').skip(1) {
            let Some((k, v)) = param.split_once('=') else {
                continue;
            };
            let k = k.trim();
            let v = v.trim().trim_matches('"');
            match k {
                "name" => name = v.to_string(),
                "filename" => filename = Some(v.to_string()),
                _ => {}
            }
        }
    }
    if name.is_empty() {
        return Err(io::Error::other(MultipartError::BadHeaders));
    }
    Ok(Part { name, filename })
}

/// The boundary in a `Content-Type` header, if it names one.
pub fn boundary_of(content_type: &str) -> Option<String> {
    let mut parts = content_type.split(';');
    let kind = parts.next()?.trim();
    if !kind.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in parts {
        let Some((k, v)) = param.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = v.trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a body with one file part.
    fn body(boundary: &str, filename: &str, content: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        b.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
                .as_bytes(),
        );
        b.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        b.extend_from_slice(content);
        b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        b
    }

    /// Builds a body with several file parts and one closing boundary.
    fn body_of(boundary: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = Vec::new();
        for (filename, content) in files {
            b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            b.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n"
                )
                .as_bytes(),
            );
            b.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
            b.extend_from_slice(content);
            b.extend_from_slice(b"\r\n");
        }
        b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        b
    }

    /// Reads a whole body and returns its parts.
    fn read_all<R: Read>(reader: R) -> io::Result<(Vec<Part>, Vec<Vec<u8>>)> {
        let mut m = Multipart::new(reader, "X");
        let mut parts = Vec::new();
        let mut data = Vec::new();
        loop {
            match m.next_event()? {
                Event::Start(p) => {
                    parts.push(p);
                    data.push(Vec::new());
                }
                Event::Data(d) => {
                    if let Some(last) = data.last_mut() {
                        last.extend_from_slice(&d);
                    }
                }
                Event::End => {}
                Event::Done => break,
            }
        }
        Ok((parts, data))
    }

    #[test]
    fn one_file_comes_back_whole() {
        let b = body("X", "book.epub", b"hello");
        let (parts, data) = read_all(&b[..]).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].filename.as_deref(), Some("book.epub"));
        assert_eq!(data[0], b"hello");
    }

    #[test]
    fn two_files_come_back_in_order() {
        let b = body_of("X", &[("one.epub", b"first"), ("two.txt", b"second")]);
        let (parts, data) = read_all(&b[..]).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].filename.as_deref(), Some("one.epub"));
        assert_eq!(parts[1].filename.as_deref(), Some("two.txt"));
        assert_eq!(data[0], b"first");
        assert_eq!(data[1], b"second");
    }

    #[test]
    fn a_file_larger_than_one_read_arrives_in_pieces() {
        let content = vec![7u8; 100_000];
        let b = body("X", "big.epub", &content);
        let mut m = Multipart::new(&b[..], "X");
        let mut got = Vec::new();
        let mut name = None;
        loop {
            match m.next_event().unwrap() {
                Event::Start(p) => name = p.filename,
                Event::Data(d) => got.extend_from_slice(&d),
                Event::End => {}
                Event::Done => break,
            }
        }
        assert_eq!(name.as_deref(), Some("big.epub"));
        assert_eq!(got, content);
    }

    #[test]
    fn a_body_that_stops_in_the_middle_is_refused() {
        let mut b = body("X", "book.epub", b"hello");
        // Cut the closing boundary off.
        b.truncate(b.len() - 12);
        let err = read_all(&b[..]).unwrap_err();
        assert_eq!(err.to_string(), MultipartError::Truncated.to_string());
    }

    #[test]
    fn a_body_that_stops_inside_the_headers_is_refused() {
        let b = b"--X\r\nContent-Disposition: form-data; name=\"file\"".to_vec();
        let err = read_all(&b[..]).unwrap_err();
        assert_eq!(err.to_string(), MultipartError::Truncated.to_string());
    }

    #[test]
    fn the_boundary_is_read_out_of_the_content_type() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=----WebKitFormBoundaryABC"),
            Some("----WebKitFormBoundaryABC".to_string())
        );
        assert_eq!(
            boundary_of("multipart/form-data; boundary=\"quoted\""),
            Some("quoted".to_string())
        );
        assert_eq!(boundary_of("application/json"), None);
        assert_eq!(boundary_of("multipart/form-data"), None);
    }

    #[test]
    fn a_part_without_a_name_is_refused() {
        let b = b"--X\r\nContent-Type: text/plain\r\n\r\nbody\r\n--X--\r\n".to_vec();
        let err = read_all(&b[..]).unwrap_err();
        assert_eq!(err.to_string(), MultipartError::BadHeaders.to_string());
    }
}
