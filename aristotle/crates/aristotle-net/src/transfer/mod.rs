// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service that takes books over the air.
//!
//! It listens on the device's network, gives the browser a page, and
//! writes what the browser sends into the library. It runs on a thread of
//! its own, so the reader's own loop is never held up by a transfer, and
//! it stops when the page is closed.
//!
//! Nothing here is authenticated: the page is only reachable from the
//! network the device is on, and only while the service runs. That is a
//! deliberate limit of the device, not an oversight; see the task notes.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub mod firewall;
pub mod http;
pub mod local;
pub mod multipart;
pub mod names;
pub mod state;

use crate::transfer::http::Request;
use crate::transfer::multipart::{Event, Multipart};
use crate::transfer::names::Kind;
use crate::transfer::state::{InFlight, Stage, State};

/// The port the service asks for. The device has few of them, and the
/// browser has to type the address, so it is a fixed one.
pub const PORT: u16 = 8080;

/// The largest a file may be. A book is a few megabytes; a video is not a
/// book, and the device has no room for one.
pub const MAX_FILE: u64 = 512 * 1024 * 1024;

/// The largest a sleep-screen picture may be. The picture is held whole
/// when it is decoded for the screen, so it has a limit of its own: a
/// picture over it is refused rather than left to fill the device.
pub const MAX_IMAGE: u64 = 5 * 1024 * 1024;

/// The folder of sleep-screen pictures, beside the library.
const SCREENSAVERS: &str = "screensavers";

/// Where the files go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    /// Where the books are.
    pub books: PathBuf,
    /// Where the fonts are.
    pub fonts: PathBuf,
    /// Where the sleep-screen pictures are.
    pub screensavers: PathBuf,
}

impl Places {
    /// The folder a file of this kind belongs in.
    pub fn folder(&self, kind: Kind) -> &Path {
        match kind {
            Kind::Book => &self.books,
            Kind::Font => &self.fonts,
            Kind::Image => &self.screensavers,
        }
    }
}

/// What the service needs to know.
#[derive(Clone, Debug)]
pub struct Config {
    /// Where the files go.
    pub places: Places,
    /// The port to ask for.
    pub port: u16,
}

impl Config {
    /// A configuration for the given folders.
    ///
    /// The sleep-screen pictures have no folder of their own in the call:
    /// they go to `screensavers/` beside the library, which is
    /// `/mnt/us/screensavers` on the device — the same folder a picture
    /// copied over USB goes to, so both routes end up together.
    pub fn new(books: PathBuf, fonts: PathBuf) -> Self {
        let screensavers = screensavers_beside(&books);
        Self {
            places: Places {
                books,
                fonts,
                screensavers,
            },
            port: PORT,
        }
    }
}

/// The `screensavers` folder that sits beside the library.
fn screensavers_beside(books: &Path) -> PathBuf {
    books.parent().map_or_else(
        || books.join(SCREENSAVERS),
        |parent| parent.join(SCREENSAVERS),
    )
}

/// A running service. Dropping it stops the service.
pub struct Service {
    /// The port the service is on.
    pub port: u16,
    /// Told to stop.
    stop: Arc<AtomicBool>,
    /// The thread the service runs on.
    handle: Option<JoinHandle<()>>,
    /// The state the screen shows.
    state: Arc<Mutex<State>>,
}

impl Service {
    /// Starts the service on its own thread.
    ///
    /// The state is shared with the caller, so the screen can show what is
    /// happening while the service runs.
    pub fn start(config: Config) -> io::Result<Self> {
        let (listener, port) = http::bind(config.port)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State::new()));
        let handle = {
            let stop = Arc::clone(&stop);
            let state = Arc::clone(&state);
            std::thread::spawn(move || serve(listener, config, stop, state))
        };
        Ok(Self {
            port,
            stop,
            handle: Some(handle),
            state,
        })
    }

    /// The state of the transfer, as the screen shows it.
    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Stops the service and waits for its thread.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the thread up so it notices the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The service's loop.
fn serve(listener: TcpListener, config: Config, stop: Arc<AtomicBool>, state: Arc<Mutex<State>>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // A non-blocking listener hands its flag to the accepted
                // socket on BSD and macOS (Linux does not), and the request
                // reader has to wait for the bytes.
                let _ = stream.set_nonblocking(false);
                // One connection at a time: a browser sends one file after
                // another, and the device has little memory to spare.
                let _ = handle(&mut stream, &config, &state);
                let _ = stream.shutdown(Shutdown::Both);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// Handles one connection.
fn handle(stream: &mut TcpStream, config: &Config, state: &Arc<Mutex<State>>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let request = http::read_request(stream)?;
    match request {
        Request::Get => http::write_page(stream),
        Request::Post {
            content_type,
            length,
            size,
        } => {
            let Some(boundary) = multipart::boundary_of(&content_type) else {
                return http::write_reply(stream, 400, "Bad Request", "no boundary");
            };
            receive(stream, config, state, &boundary, length, size)
        }
        Request::Other => http::write_reply(stream, 404, "Not Found", "not found"),
    }
}

/// Takes the files out of a request body and writes them to disk.
fn receive(
    stream: &mut TcpStream,
    config: &Config,
    state: &Arc<Mutex<State>>,
    boundary: &str,
    length: u64,
    size: u64,
) -> io::Result<()> {
    // The body is exactly as long as the browser said. Reading past it
    // would eat the next request on the same connection.
    let body = stream.take(length);
    let mut parser = Multipart::new(body, boundary);
    let mut result = Ok(());
    loop {
        let event = match parser.next_event() {
            Ok(event) => event,
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        match event {
            Event::Start(part) => {
                let Some(filename) = part.filename.clone() else {
                    continue;
                };
                match names::sanitize(&filename) {
                    Ok(safe) => {
                        let folder = config.places.folder(safe.kind);
                        let limit = max_size(safe.kind);
                        // The browser says how big the file is before it
                        // sends it. A file over the limit of its kind is
                        // refused before the device reads a byte of it.
                        if size > limit {
                            http::write_reply(stream, 413, "Payload Too Large", "file too large")?;
                            return Ok(());
                        }
                        match write_file(&mut parser, folder, &safe.name, size, limit, state) {
                            Ok((size, _)) => {
                                if let Ok(mut s) = state.lock() {
                                    s.add_received(safe.name.clone(), size);
                                    s.current = None;
                                    s.stage = Stage::Done;
                                }
                            }
                            Err(e) => {
                                if let Ok(mut s) = state.lock() {
                                    s.abandon_current();
                                }
                                result = Err(e);
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        // A file the reader cannot open is not stored; the
                        // browser is told so it does not wait forever.
                        http::write_reply(
                            stream,
                            415,
                            "Unsupported Media Type",
                            "unsupported file",
                        )?;
                        return Ok(());
                    }
                }
            }
            Event::Data(_) => {}
            Event::End => {}
            Event::Done => break,
        }
    }
    match result {
        Ok(()) => http::write_reply(stream, 200, "OK", "received"),
        Err(_) => http::write_reply(stream, 400, "Bad Request", "bad request"),
    }
}

/// Writes one part's bytes to a file in `folder`.
///
/// The file is written under a temporary name and moved into place once
/// it is whole, so a file that stops in the middle never shows up in the
/// library as if it were complete. `limit` is the most the file of its
/// kind may have; `size` is what the browser said it would send.
fn write_file(
    parser: &mut Multipart<impl Read>,
    folder: &Path,
    name: &str,
    size: u64,
    limit: u64,
    state: &Arc<Mutex<State>>,
) -> io::Result<(u64, PathBuf)> {
    fs::create_dir_all(folder)?;
    // The name is free, so a book that came in before is not overwritten.
    let mut taken = |candidate: &str| folder.join(candidate).exists();
    let final_name = names::unique_name(name, &mut taken);
    let temp = folder.join(format!(".{final_name}.part"));
    // A file that stops in the middle leaves nothing behind: the temporary
    // file goes away with the error.
    let result = write_part(parser, &temp, &final_name, size, limit, state);
    match result {
        Ok(size) => {
            let path = folder.join(&final_name);
            fs::rename(&temp, &path)?;
            Ok((size, path))
        }
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// The largest a file of this kind may be.
///
/// A book may be long and a font may carry many faces; a sleep-screen
/// picture is held whole when it is decoded, so it has a limit of its own.
fn max_size(kind: Kind) -> u64 {
    match kind {
        Kind::Book | Kind::Font => MAX_FILE,
        Kind::Image => MAX_IMAGE,
    }
}

/// Writes one part's bytes to `temp`.
fn write_part(
    parser: &mut Multipart<impl Read>,
    temp: &Path,
    final_name: &str,
    size: u64,
    limit: u64,
    state: &Arc<Mutex<State>>,
) -> io::Result<u64> {
    let mut file = fs::File::create(temp)?;
    let started = Instant::now();
    let mut written: u64 = 0;
    loop {
        let event = parser.next_event()?;
        match event {
            Event::Data(data) => {
                file.write_all(&data)?;
                written += data.len() as u64;
                if written > limit {
                    // Too big for the device to keep.
                    return Err(io::Error::other("file too large"));
                }
                if let Ok(mut s) = state.lock() {
                    s.set_current(InFlight {
                        name: final_name.to_string(),
                        received: written,
                        total: size,
                        elapsed: started.elapsed(),
                    });
                }
            }
            Event::End => break,
            Event::Start(_) | Event::Done => continue,
        }
    }
    file.flush()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::multipart::boundary_of;

    /// A body with one file part.
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

    #[test]
    fn a_book_lands_in_the_library_and_a_font_in_the_fonts() {
        let dir = std::env::temp_dir().join(format!("aristotle-transfer-{}", std::process::id()));
        let books = dir.join("documents");
        let fonts = dir.join("fonts");
        let _ = fs::remove_dir_all(&dir);
        let config = Config::new(books.clone(), fonts.clone());
        let state = Arc::new(Mutex::new(State::new()));

        let boundary = "X";
        let b = body(boundary, "book.epub", b"epub bytes");
        write_file(
            &mut Multipart::new(&b[..], boundary),
            &config.places.books,
            "book.epub",
            10,
            MAX_FILE,
            &state,
        )
        .unwrap();
        assert_eq!(fs::read(books.join("book.epub")).unwrap(), b"epub bytes");

        let b = body(boundary, "face.ttf", b"font bytes");
        write_file(
            &mut Multipart::new(&b[..], boundary),
            &config.places.fonts,
            "face.ttf",
            10,
            MAX_FILE,
            &state,
        )
        .unwrap();
        assert_eq!(fs::read(fonts.join("face.ttf")).unwrap(), b"font bytes");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_book_with_the_same_name_does_not_overwrite_the_first() {
        let dir =
            std::env::temp_dir().join(format!("aristotle-transfer-dup-{}", std::process::id()));
        let books = dir.join("documents");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&books).unwrap();
        let state = Arc::new(Mutex::new(State::new()));
        let config = Config::new(books.clone(), dir.join("fonts"));

        for content in [b"first".as_slice(), b"second".as_slice()] {
            let b = body("X", "book.epub", content);
            write_file(
                &mut Multipart::new(&b[..], "X"),
                &config.places.books,
                "book.epub",
                5,
                MAX_FILE,
                &state,
            )
            .unwrap();
        }
        assert_eq!(fs::read(books.join("book.epub")).unwrap(), b"first");
        assert_eq!(fs::read(books.join("book (2).epub")).unwrap(), b"second");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_stops_in_the_middle_leaves_nothing_behind() {
        let dir =
            std::env::temp_dir().join(format!("aristotle-transfer-cut-{}", std::process::id()));
        let books = dir.join("documents");
        let _ = fs::remove_dir_all(&dir);
        let state = Arc::new(Mutex::new(State::new()));
        let mut b = body("X", "book.epub", b"half a book");
        b.truncate(b.len() - 12);
        let err = write_file(
            &mut Multipart::new(&b[..], "X"),
            &books,
            "book.epub",
            10,
            MAX_FILE,
            &state,
        )
        .unwrap_err();
        assert!(err.to_string().contains("stops in the middle"));
        // The book is not there, and no temporary file is left either.
        assert!(!books.join("book.epub").exists());
        let leftovers: Vec<_> = fs::read_dir(&books)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_lands_in_the_screensavers_and_gets_a_number() {
        let dir =
            std::env::temp_dir().join(format!("aristotle-transfer-img-{}", std::process::id()));
        let books = dir.join("documents");
        let _ = fs::remove_dir_all(&dir);
        let config = Config::new(books.clone(), dir.join("fonts"));
        let state = Arc::new(Mutex::new(State::new()));
        // The pictures go beside the library, as `/mnt/us/screensavers` is
        // beside `/mnt/us/documents` on the device.
        assert_eq!(config.places.screensavers, dir.join("screensavers"));
        assert!(!config.places.screensavers.exists());

        for content in [b"first picture".as_slice(), b"second picture".as_slice()] {
            let b = body("X", "sunset.jpg", content);
            write_file(
                &mut Multipart::new(&b[..], "X"),
                config.places.folder(Kind::Image),
                "sunset.jpg",
                content.len() as u64,
                max_size(Kind::Image),
                &state,
            )
            .unwrap();
        }
        // The folder is made for the first picture, and the second one
        // with the same name does not overwrite it.
        let first = fs::read(config.places.screensavers.join("sunset.jpg")).unwrap();
        let second = fs::read(config.places.screensavers.join("sunset (2).jpg")).unwrap();
        assert_eq!(first, b"first picture");
        assert_eq!(second, b"second picture");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_over_five_megabytes_is_refused_and_leaves_nothing() {
        let dir =
            std::env::temp_dir().join(format!("aristotle-transfer-big-{}", std::process::id()));
        let books = dir.join("documents");
        let _ = fs::remove_dir_all(&dir);
        let config = Config::new(books, dir.join("fonts"));
        let state = Arc::new(Mutex::new(State::new()));

        let content = vec![0u8; MAX_IMAGE as usize + 1];
        let b = body("X", "huge.png", &content);
        let err = write_file(
            &mut Multipart::new(&b[..], "X"),
            config.places.folder(Kind::Image),
            "huge.png",
            content.len() as u64,
            max_size(Kind::Image),
            &state,
        )
        .unwrap_err();
        assert!(err.to_string().contains("file too large"));
        // Neither the picture nor its temporary file is left behind.
        let leftovers: Vec<String> = fs::read_dir(&config.places.screensavers)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_book_of_the_same_size_is_still_kept() {
        // The limit belongs to the kind: five megabytes is too much for a
        // picture and nothing at all for a book.
        let dir =
            std::env::temp_dir().join(format!("aristotle-transfer-lim-{}", std::process::id()));
        let books = dir.join("documents");
        let _ = fs::remove_dir_all(&dir);
        let config = Config::new(books.clone(), dir.join("fonts"));
        let state = Arc::new(Mutex::new(State::new()));

        let content = vec![0u8; 1024];
        let b = body("X", "book.epub", &content);
        write_file(
            &mut Multipart::new(&b[..], "X"),
            config.places.folder(Kind::Book),
            "book.epub",
            content.len() as u64,
            max_size(Kind::Book),
            &state,
        )
        .unwrap();
        assert_eq!(fs::read(books.join("book.epub")).unwrap(), content);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_is_html_and_stands_alone() {
        assert!(http::PAGE.starts_with("<!DOCTYPE html>"));
        assert!(http::PAGE.contains("墨阅传书"));
        assert!(http::PAGE.contains("拖书到这里"));
        // Pictures are taken on the same page, and the page says where they
        // end up.
        assert!(http::PAGE.contains("图片会作为休眠屏图片保存"));
        assert!(http::PAGE.contains(".jpg"));
        // Nothing outside the device is loaded.
        assert!(!http::PAGE.contains("src=\"http"));
        assert!(!http::PAGE.contains("href=\"http"));
    }

    #[test]
    fn the_boundary_of_the_upload_is_found() {
        assert_eq!(
            boundary_of("multipart/form-data; boundary=ABC"),
            Some("ABC".to_string())
        );
    }
}
