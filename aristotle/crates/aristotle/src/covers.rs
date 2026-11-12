// SPDX-License-Identifier: AGPL-3.0-or-later
//! Filling in the covers of a library that was scanned before they were
//! kept: the book files without a thumbnail are read again in the
//! background, one at a time, and their covers are cached.
//!
//! The work is done on a thread of its own, so a library of a thousand
//! books never holds the interface: the index on the screen is left alone
//! and only the thumbnails are written. The thread sends back the content
//! key of each book it made a cover for; the app sets it on its own index
//! and saves it, the way the scan's report is applied.
//!
//! Books with no cover in the file (TXT, a PDF of type, an image that does
//! not decode) keep the generated card and are logged; they are tried again
//! the next time the app starts, which costs a parse, not a scan.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use aristotle_library::cover::{CoverCache, ThumbSpec};
use aristotle_library::library::COVERS_DIR;
use aristotle_library::{Format, Library};

/// A book that may need its thumbnail made: where its file is, how to read
/// it, and the cover key the index has for it, if any.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub path: PathBuf,
    pub format: Format,
    /// The book's folder below the library root, for the author fallback.
    pub author_dir: Option<String>,
    /// The key in the index: when its thumbnail is on disk the book is
    /// skipped without being read. Checked on the fill thread.
    pub key: Option<String>,
}

/// Every book in the index that has a path, with the cover key it already
/// has. Whether its thumbnail is on disk is not asked here: a thousand
/// `stat`s on the interface thread at startup hold the screen for no reason,
/// and the fill thread looks anyway.
pub fn jobs(lib: &Library) -> Vec<Job> {
    lib.books()
        .iter()
        .filter_map(|b| {
            let path = lib.absolute_path(b)?;
            let author_dir = b
                .file
                .path
                .rsplit_once('/')
                .map(|(d, _)| d)
                .and_then(|d| d.rsplit('/').next())
                .map(str::to_string);
            Some(Job {
                id: b.id.clone(),
                path,
                format: b.format,
                author_dir,
                key: b.cover.clone(),
            })
        })
        .collect()
}

/// Whether `job` already has its thumbnail on disk (a record without a key,
/// or with one whose thumbnail is gone — a cleaned data directory, a cache
/// written for another size — is read again).
fn has_thumbnail(cache: &CoverCache, job: &Job) -> bool {
    job.key
        .as_deref()
        .map(aristotle_library::cover::base_key)
        .is_some_and(|key| cache.path(key).exists())
}

enum Msg {
    Filled { id: String, key: String },
    NoCover { id: String },
    Done,
}

/// The background thread, and the covers it has filled in since the last
/// time they were read.
pub struct Fill {
    cancel: Arc<AtomicBool>,
    rx: Receiver<Msg>,
    _handle: Option<thread::JoinHandle<()>>,
    finished: bool,
}

impl Fill {
    /// Starts filling `jobs`. `None` when there is nothing to do or the
    /// thread cannot be started.
    pub fn start(data_dir: &Path, jobs: Vec<Job>) -> Option<Fill> {
        if jobs.is_empty() {
            return None;
        }
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let cache = CoverCache::new(data_dir.join(COVERS_DIR), ThumbSpec::default());
        let handle = thread::Builder::new()
            .name("covers".into())
            .spawn(move || {
                for job in jobs {
                    if flag.load(Ordering::Relaxed) {
                        return;
                    }
                    if has_thumbnail(&cache, &job) {
                        continue;
                    }
                    match cover_of(&job) {
                        Some(bytes) => match cache.store(&bytes) {
                            Ok(key) => {
                                if tx.send(Msg::Filled { id: job.id, key }).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                eprintln!("aristotle: covers: {}: {e}", job.path.display());
                                let _ = tx.send(Msg::NoCover { id: job.id });
                            }
                        },
                        None => {
                            let _ = tx.send(Msg::NoCover { id: job.id });
                        }
                    }
                    // One book at a time, with a breath between them: the
                    // panel, touches and the reader share this one core.
                    thread::sleep(std::time::Duration::from_millis(30));
                }
                let _ = tx.send(Msg::Done);
            })
            .ok()?;
        Some(Fill {
            cancel,
            rx,
            _handle: Some(handle),
            finished: false,
        })
    }

    /// The covers filled in since the last call, and whether the work is
    /// over.
    pub fn poll(&mut self) -> (Vec<(String, String)>, bool) {
        let mut filled = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Filled { id, key }) => filled.push((id, key)),
                Ok(Msg::NoCover { id }) => {
                    eprintln!("aristotle: covers: {id}: no cover in the book");
                }
                Ok(Msg::Done) => {
                    self.finished = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        (filled, self.finished)
    }

    /// Stops the thread at the next book; the thumbnails it made stay.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Fill {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// The cover image bytes of a book, or `None` when the file has none.
fn cover_of(job: &Job) -> Option<Vec<u8>> {
    match aristotle_library::metadata::extract(
        &job.path,
        job.format,
        job.author_dir.as_deref(),
        true,
    ) {
        Ok(ex) => ex.cover,
        Err(e) => {
            eprintln!("aristotle: covers: {}: {e}", job.path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(id: &str, path: &str, cover: Option<&str>) -> aristotle_library::BookRecord {
        aristotle_library::BookRecord {
            id: id.into(),
            file: aristotle_library::index::FileStamp {
                path: path.into(),
                size: 1,
                mtime: 0,
                mtime_ns: 0,
            },
            format: Format::Txt,
            meta: Default::default(),
            cover: cover.map(str::to_string),
            added: 0,
            last_opened: None,
            status: Default::default(),
            progress: None,
            settings: Default::default(),
            annotations: Vec::new(),
            annotation_tombstones: Vec::new(),
        }
    }

    #[test]
    fn only_books_without_a_thumbnail_are_read_again() {
        let dir = crate::testdir("cover-jobs");
        let root = dir.join("documents");
        std::fs::create_dir_all(root.join("Prus")).unwrap();
        std::fs::write(root.join("Prus/Lalka.txt"), b"text").unwrap();
        let mut index = aristotle_library::index::Index::default();
        index.root = Some(root.to_string_lossy().into_owned());
        let key = "0123456789abcdef0123456789abcdef";
        let cache = CoverCache::new(dir.join(COVERS_DIR), ThumbSpec::default());
        std::fs::create_dir_all(dir.join(COVERS_DIR)).unwrap();
        aristotle_library::cover::write_pgm(
            &cache.path(key),
            &aristotle_library::cover::Gray {
                width: 200,
                height: 300,
                pixels: vec![0x22; 200 * 300],
            },
        )
        .unwrap();
        index.books = vec![
            book("a", "Prus/Lalka.txt", None),
            book("b", "Prus/Ogniem.txt", Some(key)),
            book("c", "Prus/Sierota.txt", None),
        ];
        index.save(&dir.join("library.json")).unwrap();
        let lib = Library::open(&dir).unwrap();
        let jobs = jobs(&lib);
        // Collecting the jobs touches no file: every book comes back with
        // the key it has, and the fill thread decides which to read.
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[1].key.as_deref(), Some(key));
        // Ogniem already has its thumbnail on disk; the other two are read
        // again.
        let ids: Vec<&str> = jobs
            .iter()
            .filter(|j| !has_thumbnail(&cache, j))
            .map(|j| j.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "c"]);
        assert_eq!(jobs[0].author_dir.as_deref(), Some("Prus"));
        assert_eq!(jobs[0].path, root.join("Prus/Lalka.txt"));
    }

    /// The list is built on the interface thread when the app starts. A
    /// thousand books must not cost a thousand `stat`s there: the jobs come
    /// back whether or not the thumbnail is on disk.
    #[test]
    fn a_thousand_jobs_are_collected_without_touching_the_disk() {
        let dir = crate::testdir("cover-jobs-many");
        let root = dir.join("documents");
        std::fs::create_dir_all(&root).unwrap();
        let books: Vec<_> = (0..1000)
            .map(|i| {
                let key = format!("{i:032x}");
                book(&format!("b{i}"), &format!("b{i}.epub"), Some(&key))
            })
            .collect();
        let index = aristotle_library::index::Index {
            root: Some(root.to_string_lossy().into_owned()),
            books,
            ..Default::default()
        };
        index.save(&dir.join("library.json")).unwrap();
        let lib = Library::open(&dir).unwrap();
        let jobs = jobs(&lib);
        assert_eq!(jobs.len(), 1000, "no thumbnail is on disk yet");
        let cache = CoverCache::new(dir.join(COVERS_DIR), ThumbSpec::default());
        assert!(jobs.iter().all(|j| !has_thumbnail(&cache, j)));
        let key = jobs[7].key.clone().unwrap();
        std::fs::create_dir_all(dir.join(COVERS_DIR)).unwrap();
        aristotle_library::cover::write_pgm(
            &cache.path(&key),
            &aristotle_library::cover::Gray {
                width: 200,
                height: 300,
                pixels: vec![0x22; 200 * 300],
            },
        )
        .unwrap();
        assert!(has_thumbnail(&cache, &jobs[7]), "the thread skips this one");
    }

    #[test]
    fn a_text_file_has_no_cover_and_the_thread_stops() {
        let dir = crate::testdir("cover-fill");
        let root = dir.join("documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("book.txt"), b"Hello").unwrap();
        let mut index = aristotle_library::index::Index::default();
        index.root = Some(root.to_string_lossy().into_owned());
        index.books = vec![book("a", "book.txt", None)];
        index.save(&dir.join("library.json")).unwrap();
        let lib = Library::open(&dir).unwrap();
        let mut fill = Fill::start(dir.as_path(), jobs(&lib)).expect("started");
        let mut finished = false;
        for _ in 0..400 {
            let (filled, done) = fill.poll();
            assert!(filled.is_empty(), "a TXT has no cover to fill in");
            if done {
                finished = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(finished, "the thread ends after the last book");
    }

    /// The home screen used to read no thumbnails at all (covers were off by
    /// default) and now reads the four it shows. This measures both on the
    /// same 500-book library, so the cost of that change is visible: the
    /// cold start must not grow by more than a fifth.
    ///
    ///     scripts/test-app-oracle.sh -n --test-threads=1 covers::tests::home
    #[test]
    fn home_cold_start_with_covers_stays_within_the_budget() {
        use aristotle_screen::Screen;
        use aristotle_ui::screens::home as home_screen;
        use aristotle_ui::text::{FamilyId, TextEngine};
        use aristotle_ui::theme::Styles;

        let dir = crate::testdir("home-covers-bench");
        let cache = CoverCache::new(dir.join(COVERS_DIR), ThumbSpec::default());
        let mut index = aristotle_library::index::Index::default();
        const BOOKS: usize = 500;
        for i in 0..BOOKS {
            let mut rec = book(&format!("book-{i}"), &format!("book{i:03}.epub"), None);
            rec.meta.title = format!("Book {i}");
            let author = format!("Author {i}");
            rec.meta.authors = vec![aristotle_library::metadata::Author::new(&author, None)];
            rec.last_opened = Some(1_000 - i as i64);
            if i < 4 {
                // Four different covers, one per book on the home screen.
                let key = format!("{i:032x}");
                let pixels: Vec<u8> = (0..200 * 300)
                    .map(|p| ((p % 200) as u8 / 4 * 4).min(255) + (i as u8 * 30))
                    .collect();
                aristotle_library::cover::write_pgm(
                    &cache.path(&key),
                    &aristotle_library::cover::Gray {
                        width: 200,
                        height: 300,
                        pixels,
                    },
                )
                .unwrap();
                rec.cover = Some(key);
            }
            index.books.push(rec);
        }
        index.save(&dir.join("library.json")).unwrap();

        let fonts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aristotle-render/assets/fonts");
        let screen = Screen::new(1264, 1680, 300).unwrap();
        let styles = Styles::new(FamilyId(0));
        let status = aristotle_ui::screens::library::StatusLine {
            clock: "21:38",
            battery: Some(0.8),
            charging: false,
        };
        let clock = home_screen::ClockText::default();

        let mut before = f64::MAX;
        let mut after = f64::MAX;
        let mut build_before = f64::MAX;
        let mut build_after = f64::MAX;
        // The covers read for the screen (the model), timed apart from the
        // widgets built around them.
        let mut files = f64::MAX;
        for _ in 0..5 {
            // Before: the home screen of a library with covers off, which
            // read no thumbnail. The index and the fonts are opened by both
            // runs, as they are at every start.
            let t = std::time::Instant::now();
            let lib = Library::open(&dir).unwrap();
            let mut text = TextEngine::new(crate::ui_fonts(&fonts_dir, &fonts_dir).unwrap());
            let opened = t.elapsed().as_secs_f64() * 1000.0;
            let build = std::time::Instant::now();
            let sorted = lib.query(&aristotle_library::Query {
                sort: aristotle_library::SortKey::Recent,
                ..Default::default()
            });
            let books: Vec<_> = sorted
                .iter()
                .take(4)
                .map(|r| crate::shelf::screen_book_plain(r))
                .collect();
            let home = home_screen::Home {
                reading: books.first(),
                recent: &books[1..],
                status,
                clock: home_screen::ClockStyle::BigLunar,
                clock_text: clock.clone(),
                stats: None,
                quote: None,
                pet: false,
            };
            let (tree, _) = home_screen::HomeUi::build(&mut text, &styles, &home, 1264);
            let mut ui = aristotle_ui::ui::Ui::with_screen(tree, text, screen);
            ui.layout();
            ui.render();
            build_before = build_before.min(build.elapsed().as_secs_f64() * 1000.0);
            before = before.min(opened + build.elapsed().as_secs_f64() * 1000.0);

            // After: the same screen, now with the books' own covers.
            let t = std::time::Instant::now();
            let lib = Library::open(&dir).unwrap();
            let mut text = TextEngine::new(crate::ui_fonts(&fonts_dir, &fonts_dir).unwrap());
            let opened = t.elapsed().as_secs_f64() * 1000.0;
            let build = std::time::Instant::now();
            let mut covers = crate::shelf::Covers::default();
            let t_model = std::time::Instant::now();
            let model = crate::shelf::HomeModel::build(&lib, &mut covers);
            files = files.min(t_model.elapsed().as_secs_f64() * 1000.0);
            let recent: Vec<_> = model.recent.iter().map(|(_, b)| b.clone()).collect();
            let home = home_screen::Home {
                reading: model.reading.as_ref().map(|(_, b)| b),
                recent: &recent,
                status,
                clock: home_screen::ClockStyle::BigLunar,
                clock_text: clock.clone(),
                stats: None,
                quote: None,
                pet: false,
            };
            let (tree, _) = home_screen::HomeUi::build(&mut text, &styles, &home, 1264);
            let mut ui = aristotle_ui::ui::Ui::with_screen(tree, text, screen);
            ui.layout();
            ui.render();
            build_after = build_after.min(build.elapsed().as_secs_f64() * 1000.0);
            after = after.min(opened + build.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "home cold start, {BOOKS} books: {before:.1} -> {after:.1} ms ({:.2} x); screen build {build_before:.1} -> {build_after:.1} ms (of which thumbnails {files:.1} ms)",
            after / before
        );
        assert!(
            after <= before * 1.2,
            "the home screen must not be slower than 1.2 x its old self: {before:.1} -> {after:.1} ms"
        );
    }
}
