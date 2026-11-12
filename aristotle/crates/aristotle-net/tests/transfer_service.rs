// SPDX-License-Identifier: AGPL-3.0-or-later
//! The transfer service, over a real socket.
//!
//! The unit tests in the module work on bytes in memory. This one starts
//! the service on a port of the loopback, talks to it with a real TCP
//! connection, and checks that a book dropped on the page ends up in the
//! library.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aristotle_net::transfer::{Config, Service, MAX_IMAGE};

/// A folder of its own for the test, so nothing touches a real library.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-transfer-it-{}-{name}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// A body with one file part.
fn upload_body(boundary: &str, filename: &str, content: &[u8]) -> Vec<u8> {
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

/// Reads the whole reply of a connection.
fn read_reply(mut stream: TcpStream) -> String {
    let mut out = Vec::new();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let _ = stream.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// A service on a port the operating system picks, so the test never
/// fights with a real one.
fn start(config: Config) -> (Service, SocketAddr) {
    let mut config = config;
    config.port = 0;
    let service = Service::start(config).expect("the service starts");
    let port = service.port;
    (service, SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Drops one file on the page and returns the reply.
///
/// `size` is the size the browser says the file has in the query string;
/// the page always sends it. `None` leaves it out, which is what a client
/// that says nothing about the file looks like to the device.
fn drop_file(addr: SocketAddr, filename: &str, content: &[u8], size: Option<u64>) -> String {
    let boundary = "----BoundaryXYZ";
    let body = upload_body(boundary, filename, content);
    let target = match size {
        Some(size) => format!("/upload?size={size}"),
        None => "/upload".to_string(),
    };
    let head = format!(
        "POST {target} HTTP/1.1\r\n\
         Host: kindle\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(&body).expect("write body");
    read_reply(stream)
}

#[test]
fn the_page_is_served_over_tcp() {
    let dir = scratch("page");
    let (service, addr) = start(Config::new(dir.join("documents"), dir.join("fonts")));

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: kindle\r\nConnection: close\r\n\r\n")
        .expect("write");
    let reply = read_reply(stream);

    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(reply.contains("Content-Type: text/html"), "{reply}");
    // The page itself is in the reply.
    assert!(reply.contains("墨阅传书"), "the page is not in the reply");
    assert!(reply.contains("拖书到这里"), "the page has no drop area");

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_book_dropped_on_the_page_lands_in_the_library() {
    let dir = scratch("book");
    let books = dir.join("documents");
    let (service, addr) = start(Config::new(books.clone(), dir.join("fonts")));

    let boundary = "----BoundaryABC";
    let body = upload_body(boundary, "journey.epub", b"the book itself");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "POST /upload HTTP/1.1\r\n\
         Host: kindle\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(&body).expect("write body");
    let reply = read_reply(stream);

    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    // The book is in the library, with the bytes it was sent.
    let landed = books.join("journey.epub");
    assert!(landed.exists(), "the book did not land in {books:?}");
    assert_eq!(fs::read(&landed).unwrap(), b"the book itself");
    // And the service knows it arrived.
    let state = service.state();
    assert_eq!(state.count(), 1);
    assert_eq!(state.total_bytes, b"the book itself".len() as u64);

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_font_dropped_on_the_page_lands_in_the_fonts() {
    let dir = scratch("font");
    let fonts = dir.join("fonts");
    let (service, addr) = start(Config::new(dir.join("documents"), fonts.clone()));

    let boundary = "B";
    let body = upload_body(boundary, "face.ttf", b"font bytes");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "POST /upload HTTP/1.1\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(&body).expect("write body");
    let reply = read_reply(stream);

    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(fonts.join("face.ttf").exists(), "the font did not land");
    assert_eq!(fs::read(fonts.join("face.ttf")).unwrap(), b"font bytes");

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_dropped_on_the_page_lands_in_the_screensavers() {
    let dir = scratch("picture");
    let books = dir.join("documents");
    let screensavers = dir.join("screensavers");
    let (service, addr) = start(Config::new(books, dir.join("fonts")));

    // The browser keeps the name the file has on the machine it came from,
    // and the extension may be in any case.
    let reply = drop_file(addr, "Sunset.JPG", b"jpeg bytes", Some(10));

    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    // The picture is beside the library, under a lower-case name, in a
    // folder the service made.
    let landed = screensavers.join("Sunset.jpg");
    assert!(
        landed.exists(),
        "the picture did not land in {screensavers:?}"
    );
    assert_eq!(fs::read(&landed).unwrap(), b"jpeg bytes");
    // And the service knows it arrived, like a book or a font.
    let state = service.state();
    assert_eq!(state.count(), 1);
    assert_eq!(state.total_bytes, 10);

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_with_the_same_name_gets_a_number() {
    let dir = scratch("picture-dup");
    let books = dir.join("documents");
    let screensavers = dir.join("screensavers");
    let (service, addr) = start(Config::new(books, dir.join("fonts")));

    let first = drop_file(addr, "cover.png", b"first picture", Some(13));
    assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
    let second = drop_file(addr, "cover.png", b"second picture", Some(14));
    assert!(second.starts_with("HTTP/1.1 200 OK"), "{second}");

    assert_eq!(
        fs::read(screensavers.join("cover.png")).unwrap(),
        b"first picture"
    );
    assert_eq!(
        fs::read(screensavers.join("cover (2).png")).unwrap(),
        b"second picture"
    );
    assert_eq!(service.state().count(), 2);

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_over_five_megabytes_is_refused() {
    let dir = scratch("picture-big");
    let books = dir.join("documents");
    let screensavers = dir.join("screensavers");
    let (service, addr) = start(Config::new(books, dir.join("fonts")));

    // The page says how big the file is before it sends it: a picture over
    // the limit is refused before the device reads it.
    let reply = drop_file(addr, "huge.jpg", b"not read", Some(MAX_IMAGE + 1));

    assert!(reply.starts_with("HTTP/1.1 413"), "{reply}");
    assert!(reply.contains("file too large"), "{reply}");
    // Nothing was written, and no folder was made for it.
    assert!(!screensavers.exists(), "the folder was made anyway");
    assert_eq!(service.state().count(), 0);

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_is_judged_by_its_extension_like_a_font_or_a_book() {
    let dir = scratch("picture-fake");
    let books = dir.join("documents");
    let screensavers = dir.join("screensavers");
    let (service, addr) = start(Config::new(books, dir.join("fonts")));

    // A picture name is not looked into, the same as a book's or a font's:
    // the extension decides where the bytes go.
    let reply = drop_file(addr, "not really a picture.png", b"plain text", Some(10));
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert_eq!(
        fs::read(screensavers.join("not really a picture.png")).unwrap(),
        b"plain text"
    );

    // What does not end in a picture extension is not a picture, however it
    // is named.
    let reply = drop_file(addr, "cover.png.exe", b"plain text", Some(10));
    assert!(reply.starts_with("HTTP/1.1 415"), "{reply}");
    assert_eq!(fs::read_dir(&screensavers).unwrap().count(), 1);

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_the_reader_cannot_open_is_refused() {
    let dir = scratch("refused");
    let books = dir.join("documents");
    let (service, addr) = start(Config::new(books.clone(), dir.join("fonts")));

    let boundary = "B";
    let body = upload_body(boundary, "virus.exe", b"not a book");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "POST /upload HTTP/1.1\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(&body).expect("write body");
    let reply = read_reply(stream);

    assert!(reply.starts_with("HTTP/1.1 415"), "{reply}");
    // Nothing was written.
    let left: Vec<_> = fs::read_dir(&books)
        .map(|d| d.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "something was written: {left:?}");

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_request_that_stops_in_the_middle_writes_nothing() {
    let dir = scratch("cut");
    let books = dir.join("documents");
    let (service, addr) = start(Config::new(books.clone(), dir.join("fonts")));

    let boundary = "B";
    let mut body = upload_body(boundary, "cut.epub", b"half a book");
    body.truncate(body.len() - 12);
    let mut stream = TcpStream::connect(addr).expect("connect");
    let head = format!(
        "POST /upload HTTP/1.1\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(&body).expect("write body");
    let reply = read_reply(stream);

    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
    assert!(!books.join("cut.epub").exists());
    // No half-written file is left in the library either.
    let left: Vec<String> = fs::read_dir(&books)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "left behind: {left:?}");

    drop(service);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_service_stops_when_it_is_dropped() {
    let dir = scratch("stop");
    let (service, addr) = start(Config::new(dir.join("documents"), dir.join("fonts")));
    // The page is served while the service runs.
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(b"GET / HTTP/1.1\r\n\r\n").expect("write");
    assert!(read_reply(stream).starts_with("HTTP/1.1 200 OK"));

    drop(service);
    // And after it is dropped, nothing answers on that port.
    std::thread::sleep(Duration::from_millis(200));
    match TcpStream::connect(addr) {
        Err(_) => {}
        Ok(mut stream) => {
            let mut out = Vec::new();
            let _ = stream.read_to_end(&mut out);
            assert!(out.is_empty(), "something still answers: {out:?}");
        }
    }

    let _ = fs::remove_dir_all(&dir);
}
