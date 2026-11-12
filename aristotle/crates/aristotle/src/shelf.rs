// SPDX-License-Identifier: AGPL-3.0-or-later
//! The library screens fed from the real library index.
//!
//! [`Nav`] is where the reader is: a view of the home screen (books with a
//! shelf filter, folders, series), or inside a folder or a series.
//! [`ShelfModel`] turns the index into the rows of that place and remembers
//! what each row opens.
//!
//! Folders mirror `documents/` exactly: a book's folder is the directory of
//! its path in the index, so "Mickiewicz, Adam/Dziady.epub" is in the folder
//! "Mickiewicz, Adam", and nested folders nest. Inside a folder its
//! subfolders come first, then its own books.
//!
//! The book to continue is the most recently opened one (by `last_opened`,
//! which stock-imported progress does not set, so before the first book is
//! opened here, the most recently updated progress counts).

use std::collections::{BTreeMap, HashMap, VecDeque};

use aristotle_i18n::{tf, tr, Msg};
use aristotle_library::cover::{read_pgm, ThumbSpec};
use aristotle_library::text::fold;
use aristotle_library::{BookRecord, Library, Query, SortKey, Status};
use aristotle_ui::screens::home as ui_home;
use aristotle_ui::screens::library::{
    self as ui_lib, Book, CoverMode, Filter, Item, LibraryLayout, Place, Shelf, StatusLine, View,
};
use aristotle_ui::widgets::GrayImage;

/// Sort orders offered by the sort button, in the order it cycles.
pub const SORTS: [(SortKey, Msg); 4] = [
    (SortKey::Recent, Msg::LibrarySortRecent),
    (SortKey::Title, Msg::LibrarySortTitle),
    (SortKey::Author, Msg::LibrarySortAuthor),
    (SortKey::Pinyin, Msg::LibrarySortPinyin),
];

/// Where the reader is in the library.
#[derive(Clone, Debug, PartialEq)]
pub struct Nav {
    pub view: View,
    pub filter: Filter,
    /// Index into [`SORTS`].
    pub sort: usize,
    /// The open folder, as path components below `documents/`.
    pub folder: Vec<String>,
    /// The open series.
    pub series: Option<String>,
}

impl Default for Nav {
    fn default() -> Self {
        Nav {
            view: View::Books,
            filter: Filter::All,
            sort: 0,
            folder: Vec::new(),
            series: None,
        }
    }
}

impl Nav {
    pub fn at_home(&self) -> bool {
        self.folder.is_empty() && self.series.is_none()
    }

    /// One level up: out of the series, or to the parent folder.
    pub fn up(&mut self) {
        if self.series.take().is_none() {
            self.folder.pop();
        }
    }
}

/// What a row opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Book(String),
    Folder(String),
    Series(String),
}

/// The library as one screen shows it.
pub struct ShelfModel {
    pub hero: Option<(String, Book)>,
    pub items: Vec<Item>,
    /// What `items[i]` opens.
    pub targets: Vec<Target>,
    pub nav: Nav,
    /// The folder or series name shown in the header of a shelf.
    pub name: String,
    pub layout: LibraryLayout,
    pub total_local_books: usize,
}

/// Thumbnails already read from the cover cache, by cover key.
///
/// Only the ones that were read: a book whose key is in the index but whose
/// thumbnail is not on disk yet (the background fill has not reached it, or
/// the file was cleaned) is looked up again next time, so the cover shows
/// without a restart once the fill writes it.
#[derive(Default)]
pub struct Covers {
    loaded: HashMap<String, GrayImage>,
    order: VecDeque<String>,
    bytes: usize,
}

const COVER_CACHE_BYTES: usize = 8 * 1024 * 1024;
const COVER_CACHE_ENTRIES: usize = 128;

impl Covers {
    /// The shelf thumbnail of `rec` (the size every list uses).
    pub fn get(&mut self, lib: &Library, rec: &BookRecord) -> Option<GrayImage> {
        self.load(lib, rec, None)
    }

    /// The screensaver's large thumbnail of `rec`, made from the book the
    /// first time it is asked for.
    pub fn get_large(&mut self, lib: &Library, rec: &BookRecord) -> Option<GrayImage> {
        self.load(lib, rec, Some(ThumbSpec::large()))
    }

    fn load(
        &mut self,
        lib: &Library,
        rec: &BookRecord,
        spec: Option<ThumbSpec>,
    ) -> Option<GrayImage> {
        let key = rec.cover.clone()?;
        let cache_key = match spec {
            Some(s) => format!("{key}-{}x{}", s.width, s.max_height),
            None => key.clone(),
        };
        if let Some(cached) = self.loaded.get(&cache_key) {
            return Some(cached.clone());
        }
        let path = match spec {
            Some(s) => lib.ensure_cover_path(rec, s)?,
            None => lib.cover_path(rec)?,
        };
        let g = read_pgm(&path).ok()?;
        let image = GrayImage::new(g.width, g.height, g.pixels);
        self.remember(cache_key, image.clone());
        Some(image)
    }

    fn remember(&mut self, key: String, image: GrayImage) {
        debug_assert!(!self.loaded.contains_key(&key));
        let bytes = image.pixels.capacity();
        if bytes > COVER_CACHE_BYTES {
            return;
        }
        while self.bytes + bytes > COVER_CACHE_BYTES || self.loaded.len() >= COVER_CACHE_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.loaded.remove(&oldest) {
                self.bytes -= old.pixels.capacity();
            }
        }
        self.bytes += bytes;
        self.order.push_back(key.clone());
        self.loaded.insert(key, image);
    }
}

/// The book to continue: the last one opened here, else the one whose
/// progress (perhaps imported from the stock reader) changed last.
pub fn current_book(lib: &Library) -> Option<&BookRecord> {
    lib.books()
        .iter()
        .filter(|b| b.last_opened.is_some())
        .max_by_key(|b| b.last_opened)
        .or_else(|| {
            lib.books()
                .iter()
                .filter(|b| b.status == Status::Reading)
                .filter_map(|b| b.progress.as_ref().map(|p| (p.updated, b)))
                .max_by_key(|(t, _)| *t)
                .map(|(_, b)| b)
        })
}

/// The folder of a book path relative to `documents/`, as components.
pub fn folder_of(path: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    parts.pop();
    parts
}

/// One level of the folder tree: the subfolders of `prefix` with their book
/// counts (all books below them, and how many are being read), and the
/// indices of the books lying directly in `prefix`.
pub fn folder_level(
    books: &[(&str, Status)],
    prefix: &[String],
) -> (Vec<(String, usize, usize)>, Vec<usize>) {
    let mut sub: BTreeMap<String, (String, usize, usize)> = BTreeMap::new();
    let mut own = Vec::new();
    for (i, (path, status)) in books.iter().enumerate() {
        let dir = folder_of(path);
        if dir.len() < prefix.len() || dir.iter().zip(prefix).any(|(a, b)| *a != b.as_str()) {
            continue;
        }
        match dir.get(prefix.len()) {
            None => own.push(i),
            Some(name) => {
                let e = sub
                    .entry(fold(name))
                    .or_insert_with(|| (name.to_string(), 0, 0));
                e.1 += 1;
                e.2 += (*status == Status::Reading) as usize;
            }
        }
    }
    (sub.into_values().collect(), own)
}

/// A library record as the screens show it, with the book's own cover
/// whenever the library has a thumbnail of it. Every screen that draws a
/// cover uses this: the generated card is only for books without one.
pub fn screen_book(lib: &Library, rec: &BookRecord, covers: &mut Covers) -> Book {
    let cover = covers.get(lib, rec);
    screen_book_with(rec, cover)
}

/// A record as the screens show it, without reading its cover. The library
/// with covers off ("none" or "cards") takes this road: a page of a
/// thousand books must not read a thousand thumbnails to draw cards.
pub fn screen_book_plain(rec: &BookRecord) -> Book {
    screen_book_with(rec, None)
}

fn screen_book_with(rec: &BookRecord, cover: Option<GrayImage>) -> Book {
    let author = rec
        .meta
        .authors
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut b = Book::new(&rec.meta.title, &author, 0.0);
    b.format = rec.format.name().to_uppercase();
    b.progress = rec.progress.as_ref().map_or(0.0, |p| p.fraction);
    b.status = match rec.status {
        Status::Unread => ui_lib::Status::Unread,
        Status::Reading => ui_lib::Status::Reading,
        Status::Finished => ui_lib::Status::Finished,
    };
    if let Some(page) = rec.progress.as_ref().and_then(|p| p.page.as_ref()) {
        b.chapter = tf!(Msg::LibraryPage, page = page);
    }
    let dir = folder_of(&rec.file.path);
    if !dir.is_empty() {
        b.folder = Some(dir.join("/"));
    }
    if let Some(s) = &rec.meta.series {
        b.series = Some((s.name.clone(), s.index.unwrap_or(0.0) as f32));
    }
    b.cover = cover;
    b
}

impl ShelfModel {
    /// Loads photos after laying out this model's library tree and page.
    pub fn load_covers(
        &self,
        lib: &Library,
        covers: &mut Covers,
        view: &ui_lib::Library,
        tree: &mut aristotle_ui::tree::Tree,
    ) {
        view.update_covers(tree, |index| {
            let Target::Book(id) = self.target(index)? else {
                return None;
            };
            covers.get(lib, lib.book(id)?)
        });
    }

    pub fn build(lib: &Library, nav: &Nav, layout: LibraryLayout) -> ShelfModel {
        let sort = SORTS[nav.sort % SORTS.len()].0;
        let sorted = lib.query(&Query {
            sort,
            ..Query::default()
        });
        let mut items = Vec::new();
        let mut targets = Vec::new();
        // Books come after any folders; collected here, added at the end.
        let mut book_rows: Vec<&BookRecord> = Vec::new();
        let mut name = String::new();
        if let Some(series) = &nav.series {
            name = series.clone();
            let mut books: Vec<&BookRecord> = sorted
                .iter()
                .copied()
                .filter(|r| r.meta.series.as_ref().is_some_and(|s| &s.name == series))
                .collect();
            books.sort_by(|a, b| {
                let i = |r: &BookRecord| r.meta.series.as_ref().and_then(|s| s.index);
                i(a).partial_cmp(&i(b)).unwrap_or(std::cmp::Ordering::Equal)
            });
            book_rows = books;
        } else if !nav.folder.is_empty() || nav.view == View::Folders {
            name = nav.folder.last().cloned().unwrap_or_default();
            let paths: Vec<(&str, Status)> = sorted
                .iter()
                .map(|r| (r.file.path.as_str(), r.status))
                .collect();
            let (subs, own) = folder_level(&paths, &nav.folder);
            for (folder, books, reading) in subs {
                targets.push(Target::Folder(folder.clone()));
                items.push(Item::Folder {
                    name: folder,
                    books,
                    reading,
                });
            }
            book_rows.extend(own.into_iter().map(|i| sorted[i]));
        } else if nav.view == View::Series {
            let mut series: BTreeMap<String, (String, usize, usize)> = BTreeMap::new();
            for r in &sorted {
                if let Some(s) = &r.meta.series {
                    let e = series
                        .entry(fold(&s.name))
                        .or_insert_with(|| (s.name.clone(), 0, 0));
                    e.1 += 1;
                    e.2 += (r.status == Status::Finished) as usize;
                }
            }
            for (name, books, finished) in series.into_values() {
                targets.push(Target::Series(name.clone()));
                items.push(Item::Series {
                    name,
                    books,
                    finished,
                });
            }
        } else {
            for r in &sorted {
                book_rows.push(r);
            }
        }
        for r in book_rows {
            items.push(Item::Book(screen_book_plain(r)));
            targets.push(Target::Book(r.id.clone()));
        }
        let total_local_books = lib.books().len();
        ShelfModel {
            hero: None,
            items,
            targets,
            nav: nav.clone(),
            name,
            layout,
            total_local_books,
        }
    }

    pub fn shelf<'a>(&'a self, status: StatusLine<'a>, covers: CoverMode) -> Shelf<'a> {
        let place = if self.nav.series.is_some() {
            Place::Series { name: &self.name }
        } else if !self.nav.folder.is_empty() {
            Place::Folder { name: &self.name }
        } else {
            Place::Home {
                view: self.nav.view,
                filter: self.nav.filter,
            }
        };
        let sort = if self.nav.series.is_some() {
            ""
        } else {
            tr(SORTS[self.nav.sort % SORTS.len()].1)
        };
        Shelf {
            place,
            hero: None,
            items: &self.items,
            sort,
            status,
            covers,
            layout: self.layout,
            local_books_count: self.total_local_books,
        }
    }

    /// What `action::ITEM + i` opens.
    pub fn target(&self, i: usize) -> Option<&Target> {
        self.targets.get(i)
    }

    pub fn hero_id(&self) -> Option<&str> {
        self.hero.as_ref().map(|(id, _)| id.as_str())
    }
}

/// The home screen's data: the book being read and the books opened
/// lately.
///
/// Both come from the index that is already in memory, so building the
/// home screen costs no disk read and no scan.
pub struct HomeModel {
    /// The book being read, with the key it is opened by.
    pub reading: Option<(String, Book)>,
    /// The books opened most recently, at most three of them, with the
    /// keys they are opened by. The book being read is not among them: it
    /// has the card of its own.
    pub recent: Vec<(String, Book)>,
}

impl HomeModel {
    /// The home screen's data for `lib`.
    pub fn build(lib: &Library, covers: &mut Covers) -> HomeModel {
        let sorted = lib.query(&Query {
            sort: SortKey::Recent,
            ..Query::default()
        });
        let reading = current_book(lib)
            .and_then(|r| lib.book(&r.id))
            .map(|r| (r.id.clone(), screen_book(lib, r, covers)));
        let recent = sorted
            .iter()
            .copied()
            .filter(|r| reading.as_ref().is_none_or(|(id, _)| id != &r.id))
            .take(ui_home::RECENT_BOOKS)
            .map(|r| (r.id.clone(), screen_book(lib, r, covers)))
            .collect();
        HomeModel { reading, recent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn folders_mirror_documents() {
        let books = [
            ("Mickiewicz, Adam/Pan Tadeusz.epub", Status::Reading),
            ("Mickiewicz, Adam/Dziady.epub", Status::Unread),
            ("Platon/Fedon.epub", Status::Finished),
            ("Filozofia/Stoicy/Medytacje.epub", Status::Reading),
            ("Filozofia/Etyka.pdf", Status::Unread),
            ("Moby Dick.mobi", Status::Unread),
        ];
        let (subs, own) = folder_level(&books, &[]);
        let names: Vec<&str> = subs.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(names, ["Filozofia", "Mickiewicz, Adam", "Platon"]);
        assert_eq!(subs[0], ("Filozofia".into(), 2, 1), "nested books count");
        assert_eq!(subs[1], ("Mickiewicz, Adam".into(), 2, 1));
        assert_eq!(own, [5], "loose books at the top");
        let (subs, own) = folder_level(&books, &s(&["Filozofia"]));
        assert_eq!(subs, [("Stoicy".to_string(), 1, 1)]);
        assert_eq!(own, [4]);
        let (subs, own) = folder_level(&books, &s(&["Filozofia", "Stoicy"]));
        assert!(subs.is_empty());
        assert_eq!(own, [3]);
        // A sibling whose name starts the same is not inside.
        let (_, own) = folder_level(&[("Platonizm/x.epub", Status::Unread)], &s(&["Platon"]));
        assert!(own.is_empty());
    }

    #[test]
    fn nav_goes_up_one_level() {
        let mut n = Nav {
            folder: s(&["Filozofia", "Stoicy"]),
            ..Nav::default()
        };
        n.up();
        assert_eq!(n.folder, s(&["Filozofia"]));
        n.series = Some("Trylogia".into());
        n.up();
        assert_eq!(n.folder, s(&["Filozofia"]), "leaving a series first");
        n.up();
        assert!(n.at_home());
    }

    #[test]
    fn folder_of_paths() {
        assert_eq!(folder_of("a/b/c.epub"), ["a", "b"]);
        assert!(folder_of("c.epub").is_empty());
    }

    /// A book can have its key in the index and no thumbnail on disk: the
    /// data directory was cleaned, or the background fill has not reached it
    /// yet. That miss must not be remembered, or the cover the fill writes
    /// later would only show after a restart.
    #[test]
    fn a_cover_written_after_a_miss_shows_without_a_restart() {
        use aristotle_library::cover::{write_pgm, Gray};
        use aristotle_library::index::{FileStamp, Index};
        use aristotle_library::library::COVERS_DIR;

        let dir = crate::testdir("covers-late");
        let root = dir.join("documents");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Lalka.epub"), b"not opened here").unwrap();
        let key = "0123456789abcdef0123456789abcdef";
        let index = Index {
            root: Some(root.to_string_lossy().into_owned()),
            books: vec![BookRecord {
                id: "a".into(),
                file: FileStamp {
                    path: "Lalka.epub".into(),
                    size: 1,
                    mtime: 0,
                    mtime_ns: 0,
                },
                format: aristotle_library::Format::Epub,
                meta: Default::default(),
                cover: Some(key.into()),
                added: 0,
                last_opened: None,
                status: Default::default(),
                progress: None,
                settings: Default::default(),
                annotations: Vec::new(),
                annotation_tombstones: Vec::new(),
            }],
            ..Default::default()
        };
        index.save(&dir.join("library.json")).unwrap();
        let lib = Library::open(&dir).unwrap();
        let rec = &lib.books()[0];
        let mut covers = Covers::default();
        assert!(
            covers.get(&lib, rec).is_none(),
            "the thumbnail is not on disk yet"
        );
        // The background fill (covers.rs) writes it under the same key and
        // the app repaints; the next look must read the file.
        std::fs::create_dir_all(dir.join(COVERS_DIR)).unwrap();
        write_pgm(
            &lib.data_dir()
                .join(COVERS_DIR)
                .join(format!("{key}-200x320.pgm")),
            &Gray {
                width: 200,
                height: 320,
                pixels: vec![0x22; 200 * 320],
            },
        )
        .unwrap();
        let model = ShelfModel::build(&lib, &Nav::default(), LibraryLayout::List);
        assert!(model
            .items
            .iter()
            .all(|item| { matches!(item, Item::Book(book) if book.cover.is_none()) }));
        let image = covers
            .get(&lib, rec)
            .expect("a thumbnail written after a miss is read on the next look");
        assert_eq!((image.width, image.height), (200, 320));
        for i in 0..COVER_CACHE_ENTRIES {
            covers.remember(format!("other-{i}"), GrayImage::new(1, 1, vec![0]));
        }
        assert!(!covers.loaded.contains_key(key));
        assert_eq!(covers.get(&lib, rec), Some(image));
    }

    #[test]
    fn cover_cache_bounds_memory_and_preserves_visible_images() {
        let mut covers = Covers::default();
        let visible = GrayImage::new(200, 320, vec![42; 200 * 320]);
        let weak = std::rc::Rc::downgrade(&visible.pixels);
        covers.remember("first".into(), visible.clone());
        for i in 0..1000 {
            covers.remember(i.to_string(), GrayImage::new(200, 320, vec![0; 200 * 320]));
            assert!(covers.bytes <= COVER_CACHE_BYTES);
            assert!(covers.loaded.len() <= COVER_CACHE_ENTRIES);
            assert_eq!(covers.loaded.len(), covers.order.len());
        }
        assert!(!covers.loaded.contains_key("first"));
        assert_eq!(visible.pixels[0], 42);
        drop(visible);
        assert!(weak.upgrade().is_none());
        let oversized = GrayImage::new(
            COVER_CACHE_BYTES as u32 + 1,
            1,
            vec![0; COVER_CACHE_BYTES + 1],
        );
        covers.remember("oversized".into(), oversized);
        assert!(!covers.loaded.contains_key("oversized"));
        for i in 0..1000 {
            covers.remember(format!("small-{i}"), GrayImage::new(1, 1, vec![0]));
        }
        assert_eq!(covers.loaded.len(), COVER_CACHE_ENTRIES);
        assert_eq!(covers.bytes, COVER_CACHE_ENTRIES);
    }
}
