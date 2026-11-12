// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end tests over generated fixtures: tiny EPUBs, a PDF, a TXT and a
//! CBZ laid out like `/mnt/us/documents`, scanned through the public API.
//! No copyrighted books are needed; everything is built here.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use aristotle_library::clippings;
use aristotle_library::cover::read_pgm;
use aristotle_library::index::{AnnotationKind, Locator, Progress, Status};
use aristotle_library::library::{Library, ScanAdmission, ScanOptions};
use aristotle_library::{Format, Query, SortKey, StatusFilter};

fn testdir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir()
        .join("aristotle-library-tests")
        .join(format!("it-{name}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&d).unwrap();
    d
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Stored-only ZIP writer, enough for EPUB and CBZ fixtures.
fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in members {
        let off = out.len() as u32;
        let mut h = Vec::new();
        h.extend_from_slice(&[20, 0, 0, 0x08, 0, 0, 0, 0, 0, 0]);
        h.extend_from_slice(&crc32(data).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&[0, 0]);
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&h);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        cd.extend_from_slice(b"PK\x01\x02\x14\x00");
        cd.extend_from_slice(&h);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&off.to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn png(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        let data: Vec<u8> = (0..w * h)
            .flat_map(|i| [shade, (i % 256) as u8, 255 - shade, 255])
            .collect();
        wr.write_image_data(&data).unwrap();
    }
    out
}

const CONTAINER: &[u8] = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

/// EPUB 3 with a cover-image item and a series collection.
fn epub3(
    title: &str,
    author: &str,
    file_as: &str,
    series: Option<(&str, u32)>,
    cover_shade: u8,
) -> Vec<u8> {
    let series_meta = series
        .map(|(s, i)| {
            format!(
                r##"<meta property="belongs-to-collection" id="c">{s}</meta><meta refines="#c" property="collection-type">series</meta><meta refines="#c" property="group-position">{i}</meta>"##
            )
        })
        .unwrap_or_default();
    let opf = format!(
        r##"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="id">urn:uuid:{title}</dc:identifier>
<dc:title>{title}</dc:title>
<dc:creator id="a">{author}</dc:creator><meta refines="#a" property="file-as">{file_as}</meta>
<dc:language>pl</dc:language>
{series_meta}
</metadata>
<manifest>
<item id="img" href="images/cover.png" media-type="image/png" properties="cover-image"/>
<item id="t" href="text.xhtml" media-type="application/xhtml+xml"/>
</manifest>
<spine><itemref idref="t"/></spine>
</package>"##
    );
    let cover = png(60, 90, cover_shade);
    zip(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/images/cover.png", &cover),
        (
            "OEBPS/text.xhtml",
            b"<html><body><p>Tekst.</p></body></html>",
        ),
    ])
}

/// EPUB 2 in ISO-8859-2 with `<meta name="cover">`, calibre series, and the
/// cover reachable only through an XHTML page with an SVG `<image>`.
fn epub2_latin2() -> Vec<u8> {
    let mut opf: Vec<u8> = br#"<?xml version="1.0" encoding="iso-8859-2"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>"#
        .to_vec();
    opf.extend_from_slice(b"\xa3\xf3d\xbc noc\xb1"); // "Łódź nocą" in ISO-8859-2
    opf.extend_from_slice(
        br#"</dc:title>
<dc:creator opf:role="aut" opf:file-as="Nowak, Anna">Anna Nowak</dc:creator>
<meta name="calibre:series" content="Miasta"/><meta name="calibre:series_index" content="1"/>
<meta name="cover" content="cpage"/>
</metadata>
<manifest><item id="cpage" href="cover.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="cpage"/></spine>
</package>"#,
    );
    let page = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body>&nbsp;<svg xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="img/okladka.png"/></svg></body></html>"#;
    let cover = png(30, 40, 10);
    zip(&[
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", CONTAINER),
        ("OEBPS/content.opf", &opf),
        ("OEBPS/cover.xhtml", page),
        ("OEBPS/img/okladka.png", &cover),
    ])
}

fn pdf(title: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = vec![];
    for body in [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [] /Count 12 >>".into(),
        format!("<< /Title ({title}) /Author (Jan Kowalski) >>"),
    ] {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", offs.len()).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R /Info 3 0 R >>\nstartxref\n{x}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

/// A documents folder like the device's: author folders, sidecars,
/// clutter that must be skipped.
fn make_library(root: &Path) {
    write(
        &root.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    write(
        &root.join("Platon/Fedon.epub"),
        &epub3("Fedon", "Platon", "Platon", Some(("Dialogi", 1)), 40),
    );
    write(&root.join("Anna Nowak/lodz.epub"), &epub2_latin2());
    write(
        &root.join("Jan Kowalski/wyklad.pdf"),
        &pdf("Liczby zespolone"),
    );
    write(
        &root.join("Bolesław Prus/Lalka.txt"),
        "Tom pierwszy\n".as_bytes(),
    );
    write(
        &root.join("Komiksy/Tytus.cbz"),
        &zip(&[("002.png", &png(10, 10, 0)), ("001.png", &png(20, 30, 128))]),
    );
    // Things that must not become books.
    write(&root.join("My Clippings.txt"), b"x");
    write(
        &root.join(".hidden/Ukryta.epub"),
        &epub3("Ukryta", "X", "X", None, 0),
    );
    write(
        &root.join("Platon/Uczta.sdr/Uczta.epub"),
        &epub3("Sidecar", "X", "X", None, 0),
    );
    write(
        &root.join("dictionaries/Słownik.epub"),
        &epub3("Słownik", "X", "X", None, 0),
    );
    write(&root.join("Platon/notes.zip"), &zip(&[("a.txt", b"x")]));
    write(&root.join("Platon/broken.epub"), b"PK\x03\x04garbage");
}

fn by_title<'a>(lib: &'a Library, title: &str) -> &'a aristotle_library::BookRecord {
    lib.books()
        .iter()
        .find(|b| b.meta.title == title)
        .unwrap_or_else(|| panic!("no book {title}"))
}

#[test]
fn legacy_cbz_metadata_refreshes_once_and_keeps_reading_state() {
    let base = testdir("cbz-refresh");
    let root = base.join("documents");
    let data = base.join("data");
    write(
        &root.join("Comic.cbz"),
        &zip(&[
            ("page10.png", &png(12, 16, 224)),
            ("page2.png", &png(12, 16, 32)),
            ("unused.webp", b"x"),
        ]),
    );
    write(&root.join("Bitmap.cbz"), &zip(&[("page1.BMP", b"BM")]));
    write(&root.join("Webp.cbz"), &zip(&[("page1.webp", b"x")]));
    let mut library = Library::open(&data).unwrap();
    let scan = library.scan(&root, &ScanOptions::default()).unwrap();
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    let id = by_title(&library, "Comic").id.clone();
    let progress = Progress {
        fraction: 0.5,
        locator: Locator::Page { index: 1 },
        updated: 1_000,
        source: None,
        page: None,
    };
    library.mark_opened(&id, 1_000);
    library.set_progress(&id, progress.clone());
    library.save().unwrap();

    let path = data.join("library.json");
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("cbz_metadata_version");
    let webp_stamp = legacy["ignored"][0].clone();
    let books = legacy["books"].as_array_mut().unwrap();
    let comic = books.iter_mut().find(|b| b["id"] == id).unwrap();
    comic["meta"]["pages"] = 99.into();
    comic["cover"] = serde_json::Value::Null;
    let mut webp = comic.clone();
    webp["id"] = "legacy-webp".into();
    for key in ["path", "size", "mtime", "mtime_ns"] {
        webp[key] = webp_stamp[key].clone();
    }
    let bitmap = books
        .iter()
        .position(|b| b["path"] == "Bitmap.cbz")
        .unwrap();
    let record: aristotle_library::BookRecord =
        serde_json::from_value(books.remove(bitmap)).unwrap();
    let stamp = serde_json::to_value(record.file).unwrap();
    books.push(webp);
    legacy["ignored"] = serde_json::json!([stamp]);
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let mut library = Library::open(&data).unwrap();
    let scan = library.scan(&root, &ScanOptions::default()).unwrap();
    assert!(scan.errors.is_empty(), "{:?}", scan.errors);
    assert_eq!((scan.updated, scan.added), (1, 1));
    assert_eq!(scan.removed, 1);
    let comic = library.book(&id).unwrap();
    assert_eq!(comic.meta.pages, Some(2));
    assert_eq!(comic.progress, Some(progress.clone()));
    assert_eq!(comic.last_opened, Some(1_000));
    assert_eq!(comic.status, Status::Reading);
    let cache = aristotle_library::cover::CoverCache::new(data.join("covers"), Default::default());
    assert_eq!(
        comic.cover,
        Some(aristotle_library::cover::CoverCache::key(&png(12, 16, 32)))
    );
    let thumbnail = read_pgm(&library.cover_path(comic).unwrap()).unwrap();
    assert_eq!(thumbnail.width, 200);
    assert!(cache.path(comic.cover.as_deref().unwrap()).exists());
    assert_eq!(by_title(&library, "Bitmap").meta.pages, Some(1));
    let removed = library
        .index()
        .removed
        .iter()
        .find(|b| b.id == "legacy-webp")
        .unwrap();
    assert_eq!(removed.progress, Some(progress.clone()));
    library.save().unwrap();

    let mut library = Library::open(&data).unwrap();
    let scan = library.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!((scan.unchanged, scan.updated, scan.added), (2, 0, 0));
    assert!(scan.parse_time.is_zero());

    let original = fs::read(root.join("Comic.cbz")).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("cbz_metadata_version");
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    fs::write(root.join("Comic.cbz"), b"PK\x03\x04broken").unwrap();
    let mut library = Library::open(&data).unwrap();
    let scan = library.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(scan.errors.len(), 1);
    assert_eq!(library.book(&id).unwrap().progress, Some(progress.clone()));
    assert_eq!(library.index().cbz_metadata_version, 0);
    library.save().unwrap();
    fs::write(root.join("Comic.cbz"), original).unwrap();
    let mut library = Library::open(&data).unwrap();
    assert!(library
        .scan(&root, &ScanOptions::default())
        .unwrap()
        .errors
        .is_empty());
    assert_eq!(library.book(&id).unwrap().progress, Some(progress));
    assert_eq!(library.index().cbz_metadata_version, 2);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn cbz_refresh_preserves_duplicate_copy_ids() {
    let base = testdir("cbz-duplicate-refresh");
    let root = base.join("documents");
    let data = base.join("data");
    let comic = zip(&[("page2.png", &png(12, 16, 32))]);
    write(&root.join("First.cbz"), &comic);
    write(&root.join("Second.cbz"), &comic);
    let mut library = Library::open(&data).unwrap();
    library.scan(&root, &ScanOptions::default()).unwrap();
    library.save().unwrap();

    let path = data.join("library.json");
    let mut legacy: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("cbz_metadata_version");
    let first = legacy["books"][0]["id"].clone();
    legacy["books"][0]["id"] = legacy["books"][1]["id"].clone();
    legacy["books"][1]["id"] = first;
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let mut library = Library::open(&data).unwrap();
    let first = by_title(&library, "First").id.clone();
    let second = by_title(&library, "Second").id.clone();
    library.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(by_title(&library, "First").id, first);
    assert_eq!(by_title(&library, "Second").id, second);
    assert_ne!(first, second);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn scan_describes_every_format() {
    let base = testdir("formats");
    let root = base.join("documents");
    make_library(&root);
    let mut lib = Library::open(base.join("data")).unwrap();
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(
        lib.books().len(),
        6,
        "{:#?}",
        lib.books().iter().map(|b| &b.file.path).collect::<Vec<_>>()
    );
    assert_eq!((r.added, r.ignored), (6, 1), "the plain zip is ignored");
    assert_eq!(r.errors.len(), 1, "broken.epub is reported: {:?}", r.errors);
    assert!(r.errors[0].0.ends_with("broken.epub"));

    let uczta = by_title(&lib, "Uczta");
    assert_eq!(uczta.format, Format::Epub);
    assert_eq!(
        uczta
            .meta
            .series
            .as_ref()
            .map(|s| (s.name.as_str(), s.index)),
        Some(("Dialogi", Some(2.0)))
    );
    let thumb = read_pgm(&lib.cover_path(uczta).expect("thumbnail")).unwrap();
    assert_eq!((thumb.width, thumb.height), (200, 300));
    assert!(thumb.pixels.iter().all(|v| v % 17 == 0));

    let lodz = by_title(&lib, "Łódź nocą");
    assert_eq!(lodz.meta.authors[0].sort, "Nowak, Anna");
    assert!(
        lib.cover_path(lodz).is_some(),
        "cover through the XHTML cover page"
    );

    let pdf = by_title(&lib, "Liczby zespolone");
    assert_eq!((pdf.format, pdf.meta.pages), (Format::Pdf, Some(12)));

    let txt = by_title(&lib, "Lalka");
    assert_eq!(txt.format, Format::Txt);
    assert_eq!(
        txt.meta.authors[0].name, "Bolesław Prus",
        "author from the folder"
    );

    let cbz = by_title(&lib, "Tytus");
    assert_eq!((cbz.format, cbz.meta.pages), (Format::Cbz, Some(2)));
    let t = read_pgm(&lib.cover_path(cbz).unwrap()).unwrap();
    assert_eq!(t.height, 300, "first page by name (20 x 30) is the cover");

    // Same cover bytes, same thumbnail file.
    lib.save().unwrap();
    let again = Library::open(base.join("data")).unwrap();
    assert_eq!(again.books().len(), 6);
}

#[test]
fn rescans_are_incremental_and_state_follows_the_book() {
    let base = testdir("incremental");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();
    lib.mark_opened(&id, 1_000);
    lib.set_progress(
        &id,
        Progress {
            fraction: 0.4,
            locator: Locator::Epub {
                spine: 1,
                offset: 10,
            },
            updated: 1_000,
            source: None,
            page: None,
        },
    );
    lib.save().unwrap();

    // Unchanged: nothing is opened again.
    let mut lib = Library::open(&data).unwrap();
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!((r.unchanged, r.added, r.updated, r.ignored), (6, 0, 0, 1));
    assert!(r.parse_time.is_zero());

    // Move to another folder and rename: same ID, state kept.
    fs::create_dir_all(root.join("Inni")).unwrap();
    fs::rename(
        root.join("Platon/Uczta.epub"),
        root.join("Inni/Symposion.epub"),
    )
    .unwrap();
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!((r.moved, r.added, r.removed), (1, 0, 0));
    let b = lib.book(&id).unwrap();
    assert_eq!(b.file.path, "Inni/Symposion.epub");
    assert_eq!((b.status, b.last_opened), (Status::Reading, Some(1_000)));
    assert_eq!(b.progress.as_ref().unwrap().fraction, 0.4);

    // Delete, rescan, copy back: the state is restored from `removed`.
    let bytes = fs::read(root.join("Inni/Symposion.epub")).unwrap();
    fs::remove_file(root.join("Inni/Symposion.epub")).unwrap();
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r.removed, 1);
    assert!(lib.book(&id).is_none());
    write(&root.join("Platon/Uczta.epub"), &bytes);
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r.restored, 1);
    assert_eq!(
        lib.book(&id).unwrap().progress.as_ref().unwrap().fraction,
        0.4
    );

    // Content replaced in place: new ID, state carried over by path.
    write(
        &root.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 90),
    );
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r.updated, 1);
    let b = by_title(&lib, "Uczta");
    assert_ne!(b.id, id);
    assert_eq!(b.last_opened, Some(1_000));
}

#[test]
fn queries_over_a_scanned_library() {
    let base = testdir("query");
    let root = base.join("documents");
    make_library(&root);
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let fedon = by_title(&lib, "Fedon").id.clone();
    lib.set_status(&fedon, Status::Finished);
    lib.mark_opened(&fedon, 50);

    let titles = |q: &Query| {
        lib.query(q)
            .iter()
            .map(|b| b.meta.title.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(&Query {
            sort: SortKey::Series,
            formats: vec![Format::Epub],
            ..Default::default()
        }),
        ["Fedon", "Uczta", "Łódź nocą"]
    );
    assert_eq!(
        titles(&Query {
            search: Some("LODZ".into()),
            ..Default::default()
        }),
        ["Łódź nocą"]
    );
    assert_eq!(
        titles(&Query {
            search: Some("prus".into()),
            ..Default::default()
        }),
        ["Lalka"]
    );
    assert_eq!(
        titles(&Query {
            status: Some(StatusFilter::Finished),
            ..Default::default()
        }),
        ["Fedon"]
    );
    assert_eq!(
        titles(&Query::default())[0],
        "Fedon",
        "recently opened first"
    );
    assert_eq!(
        lib.query(&Query {
            status: Some(StatusFilter::Unread),
            ..Default::default()
        })
        .len(),
        5
    );
}

#[test]
fn clippings_attach_to_books_once() {
    let base = testdir("clippings");
    let root = base.join("documents");
    make_library(&root);
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let file = "\u{FEFF}Uczta (Platon)\r\n- Your Highlight on page 3 | location 40-42 | Added on Friday, 3 September 2021 07:05:09\r\n\r\nEros jest filozofem.\r\n==========\r\n\
Lodz noca (Nowak, Anna)\r\n- Twoja notatka w lokalizacji 12 | Dodano: piątek, 3 września 2021 07:05:09\r\n\r\nNotatka\r\n==========\r\n\
Inna książka (Ktoś)\r\n- Your Bookmark at location 1 | Added on Friday, 3 September 2021 07:05:09\r\n\r\n\r\n==========\r\n";
    let parsed = clippings::parse(file.as_bytes());
    assert_eq!(parsed.clippings.len(), 3);
    let r = lib.import_clippings(&parsed.clippings);
    assert_eq!((r.matched, r.added, r.unmatched.len()), (2, 2, 1));
    let r = lib.import_clippings(&parsed.clippings);
    assert_eq!((r.added, r.duplicates), (0, 2));
    let u = by_title(&lib, "Uczta");
    assert_eq!(u.annotations.len(), 1);
    assert_eq!(u.annotations[0].kind, AnnotationKind::Highlight);
    assert_eq!(
        u.annotations[0].text.as_deref(),
        Some("Eros jest filozofem.")
    );
    assert_eq!(
        by_title(&lib, "Łódź nocą").annotations[0].kind,
        AnnotationKind::Note
    );
}

#[test]
fn koreader_sidecar_is_imported_for_new_books() {
    let base = testdir("koreader");
    let root = base.join("documents");
    make_library(&root);
    write(
        &root.join("Platon/Fedon.sdr/metadata.epub.lua"),
        b"return {\n    [\"last_xpointer\"] = \"/body/DocFragment[3]/body/p[2]/text().0\",\n    [\"percent_finished\"] = 1,\n    [\"summary\"] = {\n        [\"status\"] = \"complete\",\n    },\n}\n",
    );
    let mut lib = Library::open(base.join("data")).unwrap();
    let r = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r.sidecars_imported, 1);
    let f = by_title(&lib, "Fedon");
    assert_eq!(f.status, Status::Finished);
    let p = f.progress.as_ref().unwrap();
    assert_eq!((p.fraction, p.source.as_deref()), (1.0, Some("koreader")));
    assert!(
        matches!(&p.locator, Locator::XPointer { xpointer } if xpointer.starts_with("/body/DocFragment[3]"))
    );
}

#[test]
fn remove_book_moves_record_to_removed_and_restore_recovers_state() {
    let base = testdir("remove-book");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();

    let uczta = by_title(&lib, "Uczta");
    let id = uczta.id.clone();
    lib.set_status(&id, Status::Finished);
    lib.set_progress(
        &id,
        Progress {
            fraction: 0.85,
            locator: Locator::Epub {
                spine: 2,
                offset: 50,
            },
            updated: 2_000,
            source: None,
            page: Some("85".to_string()),
        },
    );
    lib.save().unwrap();

    // Remove book via Library::remove_book
    let removed_rec = lib.remove_book(&id).expect("book should be removed");
    assert_eq!(removed_rec.id, id);
    assert!(lib.book(&id).is_none());
    assert_eq!(lib.removed().len(), 1);
    assert_eq!(lib.removed()[0].id, id);
    assert_eq!(lib.removed()[0].status, Status::Finished);
    lib.save().unwrap();

    // Verify removed state persists across reload
    let mut reloaded = Library::open(&data).unwrap();
    assert!(reloaded.book(&id).is_none());
    assert_eq!(reloaded.removed().len(), 1);
    assert_eq!(reloaded.removed()[0].id, id);

    // Delete the file on disk as user would, then rescan:
    let epub_path = root.join("Platon/Uczta.epub");
    let epub_bytes = fs::read(&epub_path).unwrap();
    fs::remove_file(&epub_path).unwrap();
    let r = reloaded.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r.removed, 0, "already removed, not in active books");

    // Copy book back to disk: rescan restores it and inherits progress/status
    write(&epub_path, &epub_bytes);
    let r2 = reloaded.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(r2.restored, 1);
    let restored = reloaded.book(&id).expect("book restored");
    assert_eq!(restored.status, Status::Finished);
    assert_eq!(restored.progress.as_ref().unwrap().fraction, 0.85);
    assert_eq!(
        restored.progress.as_ref().unwrap().page.as_deref(),
        Some("85")
    );
}

#[test]
fn scan_progress_and_completion_report_with_corrupt_file() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let base = testdir("scan-report-corrupt");
    let root = base.join("documents");
    let data = base.join("data");

    // Valid books
    write(
        &root.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    write(
        &root.join("Bolesław Prus/Lalka.txt"),
        "Tom pierwszy\n".as_bytes(),
    );
    // Corrupt file (zip magic but corrupt archive)
    write(
        &root.join("Damaged/corrupt.cbz"),
        b"PK\x03\x04corrupt zip content",
    );

    let mut lib = Library::open(&data).unwrap();

    let progress_count = Arc::new(AtomicUsize::new(0));
    let last_found = Arc::new(AtomicUsize::new(0));
    let progress_cb_count = progress_count.clone();
    let found_cb = last_found.clone();

    let opts = ScanOptions {
        on_progress: Some(Arc::new(move |p| {
            progress_cb_count.fetch_add(1, Ordering::SeqCst);
            found_cb.store(p.found, Ordering::SeqCst);
        })),
        ..Default::default()
    };

    let report = lib.scan(&root, &opts).unwrap();

    assert!(!report.cancelled);
    assert_eq!(report.added, 2);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].0.contains("corrupt.cbz"));
    assert_eq!(report.added_books.len(), 2);
    assert!(progress_count.load(Ordering::SeqCst) > 0);
    assert_eq!(last_found.load(Ordering::SeqCst), 2);
    assert_eq!(lib.books().len(), 2);
}

#[test]
fn scan_cancellation_mid_scan() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let base = testdir("scan-cancel");
    let root = base.join("documents");
    let data = base.join("data");

    for i in 0..10 {
        write(
            &root.join(format!("Book_{i}.epub")),
            &epub3(&format!("Book {i}"), "Author", "Author", None, 10),
        );
    }

    let mut lib = Library::open(&data).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_in_cb = cancel.clone();

    let opts = ScanOptions {
        cancel: Some(cancel.clone()),
        on_progress: Some(Arc::new(move |p| {
            if p.found >= 1 {
                cancel_in_cb.store(true, Ordering::SeqCst);
            }
        })),
        ..Default::default()
    };

    let report = lib.scan(&root, &opts).unwrap();

    assert!(report.cancelled);
    // Even after cancellation, index remains intact and scan can be rerun cleanly
    let report_rerun = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert!(!report_rerun.cancelled);
    assert_eq!(report_rerun.added + report_rerun.unchanged, 10);
    assert_eq!(lib.books().len(), 10);
}

#[test]
fn annotation_tombstone_recorded_and_cleared_on_readd() {
    let base = testdir("ann-tombstones");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();

    let anno_id = "test-anno-12345".to_string();
    let a = aristotle_library::index::Annotation {
        id: anno_id.clone(),
        kind: aristotle_library::index::AnnotationKind::Highlight,
        text: Some("Test note".into()),
        page: None,
        location: None,
        position: Some(("pos0".into(), "pos1".into())),
        created: Some("2026-11-09 10:00:00".into()),
        modified: None,
        color: None,
        style: None,
        source: "aristotle".into(),
        chapter: None,
        note: None,
    };
    lib.add_annotation(&id, a.clone());
    assert_eq!(lib.book(&id).unwrap().annotations.len(), 1);
    assert!(lib.book(&id).unwrap().annotation_tombstones.is_empty());

    // Remove annotation: tombstone must be created
    let removed = lib.remove_annotations(&id, |ann| ann.id == anno_id);
    assert_eq!(removed, 1);
    assert!(lib.book(&id).unwrap().annotations.is_empty());
    let tombstones = &lib.book(&id).unwrap().annotation_tombstones;
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].id, anno_id);

    // Re-adding the annotation removes the tombstone
    lib.add_annotation(&id, a);
    assert_eq!(lib.book(&id).unwrap().annotations.len(), 1);
    assert!(lib.book(&id).unwrap().annotation_tombstones.is_empty());
}

#[test]
fn annotation_tombstones_are_cleared_after_a_successful_push() {
    let base = testdir("ann-tombstone-clear");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();

    let a = aristotle_library::index::Annotation {
        id: "test-anno-clear".into(),
        kind: aristotle_library::index::AnnotationKind::Highlight,
        text: Some("Note".into()),
        page: None,
        location: None,
        position: Some(("pos0".into(), "pos1".into())),
        created: Some("2026-11-09 10:00:00".into()),
        modified: None,
        color: None,
        style: None,
        source: "aristotle".into(),
        chapter: None,
        note: None,
    };
    lib.add_annotation(&id, a);
    lib.remove_annotations(&id, |ann| ann.id == "test-anno-clear");
    assert_eq!(lib.book(&id).unwrap().annotation_tombstones.len(), 1);

    assert!(lib.clear_annotation_tombstones(&id));
    assert!(lib.book(&id).unwrap().annotation_tombstones.is_empty());
    assert!(!lib.clear_annotation_tombstones(&id));
}

/// A push that was answered while the reader kept working may only forget
/// the deletions it carried.
#[test]
fn only_the_deletions_of_the_submitted_snapshot_are_acknowledged() {
    let base = testdir("ann-tombstone-ack");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();

    let a = highlight("anno-ack");
    lib.add_annotation(&id, a);
    lib.remove_annotations(&id, |ann| ann.id == "anno-ack");
    assert_eq!(lib.book(&id).unwrap().annotation_tombstones.len(), 1);

    // The snapshot the push carries.
    let submitted = lib.book(&id).unwrap().clone();

    // The reader deletes another highlight while the push is in flight.
    let b = highlight("anno-later");
    lib.add_annotation(&id, b);
    lib.remove_annotations(&id, |ann| ann.id == "anno-later");

    let ack = lib.acknowledge_push(&id, &submitted);
    assert!(ack.acknowledged, "the submitted deletion was forgotten");
    assert!(ack.pending, "the later deletion is still unsent");
    let left = &lib.book(&id).unwrap().annotation_tombstones;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, "anno-later");
}

/// The same id with a newer timestamp is a new deletion, not the one that
/// went up.
#[test]
fn a_newer_deletion_of_the_same_id_stays_unsent() {
    let base = testdir("ann-tombstone-ack-same-id");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();

    let a = highlight("anno-same");
    lib.add_annotation(&id, a);
    lib.remove_annotations(&id, |ann| ann.id == "anno-same");
    let submitted = lib.book(&id).unwrap().clone();

    // The same id is deleted again, later: the local record knows the newer
    // timestamp only.
    lib.book_mut(&id).unwrap().annotation_tombstones.push(
        aristotle_library::index::AnnotationTombstone {
            id: "anno-same".into(),
            deleted_at: submitted.annotation_tombstones[0].deleted_at + 1_000,
        },
    );

    let ack = lib.acknowledge_push(&id, &submitted);
    assert!(ack.pending, "the newer deletion is still unsent");
    let left = &lib.book(&id).unwrap().annotation_tombstones;
    assert_eq!(left.len(), 1, "the newer tombstone survived");
    assert_eq!(
        left[0].deleted_at,
        submitted.annotation_tombstones[0].deleted_at + 1_000
    );
}

/// A book that did not change is fully sent; one that changed keeps its
/// place on the pending list.
#[test]
fn an_unchanged_book_is_fully_sent_and_a_changed_one_stays_pending() {
    let base = testdir("ann-tombstone-ack-pending");
    let root = base.join("documents");
    make_library(&root);
    let data = base.join("data");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();

    let a = highlight("anno-pending");
    lib.add_annotation(&id, a);
    lib.remove_annotations(&id, |ann| ann.id == "anno-pending");
    let submitted = lib.book(&id).unwrap().clone();

    // Nothing changed: the push carried everything.
    let ack = lib.acknowledge_push(&id, &submitted);
    assert!(ack.acknowledged);
    assert!(!ack.pending, "the book is fully on the server");
    assert!(lib.book(&id).unwrap().annotation_tombstones.is_empty());

    // A note added after the snapshot: the book has something left.
    let b = highlight("anno-after");
    lib.add_annotation(&id, b);
    let ack = lib.acknowledge_push(&id, &submitted);
    assert!(
        !ack.acknowledged,
        "nothing of the snapshot was left to forget"
    );
    assert!(ack.pending, "the new note is still unsent");
}

fn highlight(id: &str) -> aristotle_library::index::Annotation {
    aristotle_library::index::Annotation {
        id: id.into(),
        kind: AnnotationKind::Highlight,
        text: Some("Zaznaczenie".into()),
        page: None,
        location: None,
        position: Some(("pos0".into(), "pos1".into())),
        created: Some("2026-11-10 10:00:00".into()),
        modified: None,
        color: None,
        style: None,
        source: "aristotle".into(),
        chapter: None,
        note: None,
    }
}

/// FIX-106: a scan works on its own copy of the index while the reader keeps
/// reading. Folding the scan back must keep both sides' writes: the books it
/// found, and the progress, highlight, status and cover key the reader made
/// meanwhile (each of those would have been lost by the old save-then-reload).
#[test]
fn a_scan_does_not_roll_back_what_the_reader_wrote_meanwhile() {
    let base = testdir("absorb");
    let root = base.join("documents");
    let data = base.join("data");
    make_library(&root);
    fs::create_dir_all(&data).unwrap();

    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    lib.save().unwrap();

    // What the background scan opened: the index as it is now, its own copy.
    let mut scanned = Library::open(&data).unwrap();

    // Meanwhile the reader turns a page, makes a highlight, marks a book
    // finished and gets the TXT's cover filled in.
    let read = by_title(&lib, "Uczta").id.clone();
    let progress = Progress {
        fraction: 0.62,
        locator: Locator::Epub {
            spine: 3,
            offset: 40,
        },
        updated: 2_000,
        source: Some("aristotle".into()),
        page: None,
    };
    lib.set_progress(&read, progress.clone());
    lib.add_annotation(&read, highlight("during-scan"));
    let txt = by_title(&lib, "Lalka").id.clone();
    lib.set_cover(&txt, "cover-key-filled-meanwhile");
    let finished = by_title(&lib, "Fedon").id.clone();
    lib.set_status(&finished, Status::Finished);

    // A new book lands while the scan walks the folder.
    write(
        &root.join("Nowa/Traktat.epub"),
        &epub3("Traktat", "Autor", "Autor", None, 77),
    );
    let report = scanned.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(report.added, 1);

    lib.absorb_scan(scanned.index().clone());

    assert_eq!(
        lib.book(&read).unwrap().progress.as_ref().unwrap(),
        &progress,
        "the page turned during the scan survives"
    );
    assert_eq!(lib.book(&read).unwrap().annotations.len(), 1);
    assert_eq!(
        lib.book(&txt).unwrap().cover.as_deref(),
        Some("cover-key-filled-meanwhile")
    );
    assert_eq!(lib.book(&finished).unwrap().status, Status::Finished);
    assert_eq!(by_title(&lib, "Traktat").status, Status::Unread);
}

/// The scan's copy still lists a book the reader deleted after the scan read
/// it. The file is gone, so the deletion wins: the book does not come back.
#[test]
fn a_book_deleted_during_a_scan_stays_deleted() {
    let base = testdir("absorb-delete");
    let root = base.join("documents");
    let data = base.join("data");
    make_library(&root);
    fs::create_dir_all(&data).unwrap();

    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    lib.save().unwrap();

    let mut scanned = Library::open(&data).unwrap();
    scanned.scan(&root, &ScanOptions::default()).unwrap();

    let id = by_title(&lib, "Uczta").id.clone();
    let path = lib.absolute_path(lib.book(&id).unwrap()).unwrap();
    fs::remove_file(&path).unwrap();
    lib.remove_book(&id);

    lib.absorb_scan(scanned.index().clone());

    assert!(lib.book(&id).is_none(), "the deleted book stays deleted");
    let remembered = lib
        .removed()
        .iter()
        .find(|b| b.id == id)
        .expect("its state is still remembered");
    assert_eq!(remembered.file.path, "Platon/Uczta.epub");
}

/// The same conflict from the other side: the reader deleted the book before
/// the scan started, the file was copied back while the scan ran, and the scan
/// restored it. The book belongs on the shelf with its state.
#[test]
fn a_book_the_scan_restored_stays_on_the_shelf() {
    let base = testdir("absorb-restore");
    let root = base.join("documents");
    let data = base.join("data");
    make_library(&root);
    fs::create_dir_all(&data).unwrap();

    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();
    lib.set_progress(
        &id,
        Progress {
            fraction: 0.31,
            locator: Locator::Epub {
                spine: 2,
                offset: 5,
            },
            updated: 3_000,
            source: None,
            page: None,
        },
    );
    lib.save().unwrap();

    let bytes = fs::read(root.join("Platon/Uczta.epub")).unwrap();
    fs::remove_file(root.join("Platon/Uczta.epub")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    lib.save().unwrap();
    assert!(lib.book(&id).is_none());

    // The scan opens the index that has the book among the removed ones...
    let mut scanned = Library::open(&data).unwrap();
    // ...the file comes back while it runs...
    write(&root.join("Platon/Uczta.epub"), &bytes);
    let report = scanned.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(report.restored, 1);

    lib.absorb_scan(scanned.index().clone());

    let rec = lib.book(&id).expect("restored books stay on the shelf");
    assert_eq!(rec.progress.as_ref().unwrap().fraction, 0.31);
    assert!(!lib.removed().iter().any(|b| b.id == id));
}

/// A catalogue download is scanned on the main thread while the background
/// scan runs on its own copy: the scan never saw that book, so folding it in
/// must not drop it from the shelf.
#[test]
fn a_book_scanned_on_the_main_thread_survives_the_background_scan() {
    let base = testdir("absorb-catalog");
    let root = base.join("documents");
    let data = base.join("data");
    make_library(&root);
    fs::create_dir_all(&data).unwrap();

    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    lib.save().unwrap();

    let mut scanned = Library::open(&data).unwrap();
    let report = scanned.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(report.added, 0);

    // The download lands after the background scan walked the folder.
    write(
        &root.join("Katalog/Nowa.epub"),
        &epub3("Nowa", "Autor", "Autor", None, 55),
    );
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Nowa").id.clone();

    lib.absorb_scan(scanned.index().clone());

    assert_eq!(lib.book(&id).unwrap().meta.title, "Nowa");
}

/// LIB-006: the default scan covers the whole Kindle user store. Books in
/// `documents` are found as before, books in `Books`, `Download` and folders
/// beside `documents` are new, and the store's own app and system folders —
/// including the library's data directory — are not books. A user folder
/// named like one of those deeper in the tree still scans.
#[test]
fn a_store_scan_covers_every_user_folder_and_skips_app_folders() {
    let base = testdir("store-scope");
    let store = base.join("store");
    let docs = store.join("documents");
    write(
        &docs.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    write(&store.join("Books/Anna Nowak/lodz.epub"), &epub2_latin2());
    write(&store.join("Download/wyklad.pdf"), &pdf("Liczby zespolone"));
    write(&store.join("Loose.txt"), "Tom pierwszy\n".as_bytes());
    // A user folder named `system` below the store root's `documents`.
    write(
        &docs.join("system/Ukryta.epub"),
        &epub3("Ukryta", "X", "X", None, 0),
    );
    // App and system folders at the store root: never books.
    for (path, title) in [
        (store.join("system/Nota.epub"), "Nota"),
        (store.join("aristotle/licenses/License.txt"), "License"),
        (store.join("koreader/data/dict/Slownik.epub"), "Slownik"),
        (store.join("aristotle-notes/Notatki.epub"), "Notatki"),
        (store.join("screensavers/Obraz.epub"), "Obraz"),
        (store.join("extensions/Pomoc.epub"), "Pomoc"),
    ] {
        write(&path, &epub3(title, "X", "X", None, 1));
    }
    // Skipped at any depth, and sidecars and hidden folders as before.
    write(
        &docs.join("dictionaries/Slownik.epub"),
        &epub3("Slownik", "X", "X", None, 2),
    );
    write(
        &docs.join("Uczta.sdr/Uczta.epub"),
        &epub3("Sidecar", "X", "X", None, 3),
    );
    write(
        &docs.join(".hidden/Ukryta.epub"),
        &epub3("Hidden", "X", "X", None, 4),
    );
    // The library's own data directory can be a custom, visible folder.
    let data = store.join("state");
    write(
        &data.join("planted.epub"),
        &epub3("Planted", "X", "X", None, 5),
    );

    let mut lib = Library::open(&data).unwrap();
    let opts = ScanOptions {
        root_skip_dirs: [
            "system",
            "aristotle",
            "koreader",
            "aristotle-notes",
            "screensavers",
            "extensions",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        ..Default::default()
    };
    let r = lib.scan(&store, &opts).unwrap();
    let titles: Vec<&str> = lib.books().iter().map(|b| b.meta.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Łódź nocą", "Liczby zespolone", "Loose", "Uczta", "Ukryta"],
        "errors: {:?}",
        r.errors
    );
    assert_eq!((r.added, r.errors.len()), (5, 0));
    let paths: Vec<&str> = lib.books().iter().map(|b| b.file.path.as_str()).collect();
    assert!(paths.contains(&"documents/Platon/Uczta.epub"));
    assert!(paths.contains(&"documents/system/Ukryta.epub"));
    assert!(paths.contains(&"Books/Anna Nowak/lodz.epub"));
    assert!(paths.contains(&"Download/wyklad.pdf"));
    assert!(paths.contains(&"Loose.txt"));

    // Every record resolves to the file it was made from.
    for b in lib.books() {
        let abs = lib.absolute_path(b).unwrap();
        assert!(abs.is_file(), "{} is not a file", abs.display());
    }

    // A rescan opens nothing again, and the result survives save/reopen.
    lib.save().unwrap();
    let mut lib = Library::open(&data).unwrap();
    let r = lib.scan(&store, &opts).unwrap();
    assert_eq!((r.unchanged, r.added), (5, 0));
    assert!(r.parse_time.is_zero());
    fs::remove_dir_all(base).unwrap();
}

/// LIB-006: growing the root from `documents` to the user store only moves
/// the relative paths down. The IDs (including duplicate `-2` ones), reading
/// state, annotations, settings and timestamps stay, the path of a removed
/// book is rebased too, and an unchanged book is not parsed again.
#[test]
fn expanding_documents_to_the_store_migrates_state_without_reparsing() {
    use aristotle_library::index::{AnnotationKind, INDEX_FILE};

    let base = testdir("migrate-root");
    let store = base.join("store");
    let docs = store.join("documents");
    let data = base.join("data");
    write(
        &docs.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    write(&docs.join("Prus/Lalka.txt"), "Tom pierwszy\n".as_bytes());
    let duplicate = epub3("Duplicate", "Autor", "Autor", None, 33);
    write(&docs.join("Dup/One.epub"), &duplicate);
    write(&docs.join("Dup/Two.epub"), &duplicate);

    let mut lib = Library::open(&data).unwrap();
    let r = lib.scan(&docs, &ScanOptions::default()).unwrap();
    assert_eq!(r.added, 4);
    let uczta = by_title(&lib, "Uczta").id.clone();
    let lalka = by_title(&lib, "Lalka").id.clone();
    let progress = Progress {
        fraction: 0.42,
        locator: Locator::Epub {
            spine: 2,
            offset: 30,
        },
        updated: 1_000,
        source: Some("aristotle".into()),
        page: None,
    };
    lib.set_progress(&uczta, progress.clone());
    lib.mark_opened(&uczta, 1_234);
    lib.add_annotation(&uczta, highlight("migrated-mark"));
    lib.book_mut(&uczta).unwrap().settings.font_size = Some(13.5);
    lib.set_progress(
        &lalka,
        Progress {
            fraction: 0.9,
            locator: Locator::Text { offset: 10 },
            updated: 2_000,
            source: None,
            page: None,
        },
    );
    lib.set_status(&lalka, Status::Finished);
    let added = lib.book(&uczta).unwrap().added;
    // Duplicate copies of one file share a content ID; the second gets a
    // `-2` suffix. Both must keep their ID through the migration.
    let mut dup_ids: Vec<String> = lib
        .books()
        .iter()
        .filter(|b| b.meta.title == "Duplicate")
        .map(|b| b.id.clone())
        .collect();
    dup_ids.sort();
    assert_eq!(dup_ids.len(), 2);
    assert!(dup_ids[0] != dup_ids[1] && dup_ids[1].ends_with("-2"));

    // The TXT disappears: its record moves to `removed`, whose path is
    // relative to `documents` and has to move down as well.
    fs::remove_file(docs.join("Prus/Lalka.txt")).unwrap();
    let r = lib.scan(&docs, &ScanOptions::default()).unwrap();
    assert_eq!(r.removed, 1);
    lib.save().unwrap();

    let mut lib = Library::open(&data).unwrap();
    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    assert!(r.migrated, "the expanded root is reported to the caller");
    assert_eq!(
        (r.added, r.updated, r.moved, r.removed, r.restored),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(r.unchanged, 3, "nothing is added or loses its state");
    assert!(
        r.parse_time.is_zero(),
        "unchanged books are not parsed again"
    );
    let expected_root = fs::canonicalize(&store).unwrap();
    assert_eq!(lib.root().unwrap(), expected_root, "the store is the root");

    let u = lib.book(&uczta).unwrap();
    assert_eq!(u.file.path, "documents/Platon/Uczta.epub");
    assert_eq!(u.progress, Some(progress));
    assert_eq!(
        (u.status, u.last_opened, u.added),
        (Status::Reading, Some(1_234), added)
    );
    assert_eq!(u.settings.font_size, Some(13.5));
    assert_eq!(u.annotations.len(), 1);
    assert_eq!(u.annotations[0].kind, AnnotationKind::Highlight);
    let abs = lib.absolute_path(u).unwrap();
    assert_eq!(
        abs,
        fs::canonicalize(docs.join("Platon/Uczta.epub")).unwrap()
    );
    assert!(abs.is_file());

    let mut migrated_dup_ids: Vec<String> = lib
        .books()
        .iter()
        .filter(|b| b.meta.title == "Duplicate")
        .map(|b| b.id.clone())
        .collect();
    migrated_dup_ids.sort();
    assert_eq!(migrated_dup_ids, dup_ids);

    let removed = lib
        .removed()
        .iter()
        .find(|b| b.id == lalka)
        .expect("the removed book is still remembered");
    assert_eq!(removed.file.path, "documents/Prus/Lalka.txt");
    assert_eq!(removed.status, Status::Finished);
    assert_eq!(removed.progress.as_ref().unwrap().fraction, 0.9);

    // Save and reopen: the migrated paths are what the next start reads.
    lib.save().unwrap();
    let mut lib = Library::open(&data).unwrap();
    assert_eq!(lib.root().unwrap(), expected_root);
    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    assert!(!r.migrated);
    assert_eq!((r.unchanged, r.added), (3, 0));
    assert_eq!(
        lib.book(&uczta).unwrap().file.path,
        "documents/Platon/Uczta.epub"
    );
    assert_eq!(
        lib.book(&uczta)
            .unwrap()
            .progress
            .as_ref()
            .unwrap()
            .fraction,
        0.42
    );
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join(INDEX_FILE)).unwrap()).unwrap();
    assert_eq!(index["root"], expected_root.to_string_lossy().as_ref());
    fs::remove_dir_all(base).unwrap();
}

/// LIB-006: with the old root switch every book was dumped into `removed`
/// and the list is capped at 500. A store with more books than that must not
/// lose the oldest reading state, so the ancestor migration rebases instead.
#[test]
fn an_ancestor_migration_keeps_more_than_the_removed_cap() {
    use aristotle_library::index::{BookRecord, FileStamp, Index, INDEX_FILE, INDEX_VERSION};
    use aristotle_library::metadata::Metadata;
    use std::time::UNIX_EPOCH;

    let base = testdir("migrate-cap");
    let store = base.join("store");
    let docs = store.join("documents");
    let data = base.join("data");
    fs::create_dir_all(docs.join("Many")).unwrap();
    let mut books = Vec::new();
    for i in 0..600 {
        let name = format!("book{i:03}.txt");
        let path = docs.join("Many").join(&name);
        fs::write(&path, b"x").unwrap();
        let md = fs::metadata(&path).unwrap();
        let mtime = md.modified().unwrap().duration_since(UNIX_EPOCH).unwrap();
        books.push(BookRecord {
            id: format!("id{i:03}"),
            file: FileStamp {
                path: format!("Many/{name}"),
                size: 1,
                mtime: mtime.as_secs() as i64,
                mtime_ns: mtime.subsec_nanos(),
            },
            format: Format::Txt,
            meta: Metadata {
                title: format!("Book {i}"),
                ..Metadata::default()
            },
            cover: None,
            added: 1,
            last_opened: (i % 2 == 0).then_some(10),
            status: Status::Reading,
            progress: Some(Progress {
                fraction: 0.5,
                locator: Locator::Text { offset: 0 },
                updated: 1,
                source: None,
                page: None,
            }),
            settings: Default::default(),
            annotations: Vec::new(),
            annotation_tombstones: Vec::new(),
        });
    }
    fs::create_dir_all(&data).unwrap();
    Index {
        version: INDEX_VERSION,
        root: Some(
            fs::canonicalize(&docs)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        books,
        ..Index::default()
    }
    .save(&data.join(INDEX_FILE))
    .unwrap();

    let mut lib = Library::open(&data).unwrap();
    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    assert!(r.migrated);
    assert_eq!((r.added, r.updated, r.moved, r.removed), (0, 0, 0, 0));
    assert_eq!(r.unchanged, 600);
    assert!(r.parse_time.is_zero());
    assert_eq!(lib.books().len(), 600);
    for (i, b) in lib.books().iter().enumerate() {
        assert_eq!(b.id, format!("id{i:03}"));
        assert_eq!(b.file.path, format!("documents/Many/book{i:03}.txt"));
        assert_eq!(b.progress.as_ref().unwrap().fraction, 0.5);
        assert_eq!(b.last_opened, (i % 2 == 0).then_some(10));
    }
    fs::remove_dir_all(base).unwrap();
}

/// LIB-006: a scan cancelled before it starts changes nothing, not even the
/// root or the paths; the next full scan migrates and keeps the state.
#[test]
fn a_cancelled_scan_does_not_migrate_the_index() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let base = testdir("migrate-cancel");
    let store = base.join("store");
    let docs = store.join("documents");
    let data = base.join("data");
    write(
        &docs.join("Platon/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&docs, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();
    let progress = Progress {
        fraction: 0.3,
        locator: Locator::Epub {
            spine: 1,
            offset: 2,
        },
        updated: 3_000,
        source: None,
        page: None,
    };
    lib.set_progress(&id, progress.clone());
    lib.save().unwrap();

    let mut lib = Library::open(&data).unwrap();
    let cancel = Arc::new(AtomicBool::new(true));
    let r = lib
        .scan(
            &store,
            &ScanOptions {
                cancel: Some(cancel),
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert!(r.cancelled);
    assert!(!r.migrated);
    assert_eq!(lib.root().unwrap(), fs::canonicalize(&docs).unwrap());
    assert_eq!(lib.book(&id).unwrap().file.path, "Platon/Uczta.epub");
    assert_eq!(lib.book(&id).unwrap().progress, Some(progress.clone()));

    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    assert!(!r.cancelled);
    assert!(r.migrated);
    assert_eq!(
        lib.book(&id).unwrap().file.path,
        "documents/Platon/Uczta.epub"
    );
    assert_eq!(lib.book(&id).unwrap().progress, Some(progress));
    fs::remove_dir_all(base).unwrap();
}

/// LIB-006: a folder the walk cannot read keeps the books the index already
/// has there, and the failure is reported. A transient Permission denied or
/// an interrupted mount must not make the shelf lose books (and state).
#[cfg(unix)]
#[test]
fn an_unreadable_folder_keeps_its_books_and_is_reported() {
    use std::os::unix::fs::PermissionsExt;

    let base = testdir("scan-unreadable");
    let store = base.join("store");
    let docs = store.join("documents");
    let data = base.join("data");
    write(
        &docs.join("Kept/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", Some(("Dialogi", 2)), 200),
    );
    write(&docs.join("Open/Lalka.txt"), "Tom pierwszy\n".as_bytes());
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&store, &ScanOptions::default()).unwrap();
    let id = by_title(&lib, "Uczta").id.clone();
    lib.set_progress(
        &id,
        Progress {
            fraction: 0.6,
            locator: Locator::Epub {
                spine: 1,
                offset: 9,
            },
            updated: 4_000,
            source: None,
            page: None,
        },
    );
    lib.save().unwrap();

    let kept = docs.join("Kept");
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o000)).unwrap();
    // Root and some filesystems ignore the mode; the assertion is only
    // meaningful where the host enforces it.
    let enforced = fs::read_dir(&kept).is_err();
    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o755)).unwrap();
    if enforced {
        assert!(!r.errors.is_empty(), "the failure is reported");
        assert!(r.errors.iter().any(|(p, _)| p.contains("Kept")));
        let b = lib
            .book(&id)
            .expect("the book in the unreadable folder stays");
        assert_eq!(b.file.path, "documents/Kept/Uczta.epub");
        assert_eq!(b.progress.as_ref().unwrap().fraction, 0.6);
        assert_eq!(r.unchanged, 2, "both books are still in the index");
    }

    let r = lib.scan(&store, &ScanOptions::default()).unwrap();
    assert!(
        r.errors.is_empty(),
        "the folder reads again: {:?}",
        r.errors
    );
    assert_eq!((r.unchanged, r.added, r.removed), (2, 0, 0));
    assert!(lib.book(&id).is_some());
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn custom_roots_keep_user_folders_and_reserved_files_stay_out() {
    let base = testdir("custom-scope");
    let root = base.join("books");
    for path in [
        "system/book.txt",
        "aristotle/book.txt",
        "Books/quotes.txt",
        "quotes.txt",
        "documents/quotes.txt",
        "inkread-probe.txt",
    ] {
        write(
            &root.join(path),
            format!("Original text for {path}").as_bytes(),
        );
    }
    let mut lib = Library::open(base.join("data")).unwrap();
    let report = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert!(report.errors.is_empty());
    let paths: Vec<_> = lib.books().iter().map(|b| b.file.path.as_str()).collect();
    assert_eq!(
        paths,
        ["Books/quotes.txt", "aristotle/book.txt", "system/book.txt"]
    );
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn cancelling_during_migration_restores_the_entire_index() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let base = testdir("cancel-migration-midway");
    let docs = base.join("store/documents");
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(&docs.join(name), name.as_bytes());
    }
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&docs, &ScanOptions::default()).unwrap();
    let before = serde_json::to_value(lib.index()).unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(
            &docs.join(name),
            format!("Changed original text for {name}").as_bytes(),
        );
    }
    for cancel_on_last in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let options = ScanOptions {
            cancel: Some(cancel),
            on_progress: Some(Arc::new(move |progress| {
                if !cancel_on_last || progress.current == progress.total {
                    signal.store(true, Ordering::Relaxed);
                }
            })),
            ..Default::default()
        };
        let report = lib.scan(&base.join("store"), &options).unwrap();
        assert!(report.cancelled);
        assert!(!report.migrated);
        assert_eq!(serde_json::to_value(lib.index()).unwrap(), before);
    }
    lib.save().unwrap();
    let mut lib = Library::open(base.join("data")).unwrap();
    assert_eq!(serde_json::to_value(lib.index()).unwrap(), before);
    assert!(
        lib.scan(&base.join("store"), &ScanOptions::default())
            .unwrap()
            .migrated
    );
    assert_eq!(lib.books().len(), 3);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn unreadable_or_damaged_book_content_preserves_the_old_record() {
    let base = testdir("damaged-keeps-state");
    let root = base.join("documents");
    let path = root.join("book.epub");
    write(&path, &epub3("Original", "Author", "Author", None, 10));
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = lib.books()[0].id.clone();
    lib.book_mut(&id).unwrap().status = Status::Reading;
    let before = serde_json::to_value(lib.book(&id).unwrap()).unwrap();
    for bytes in [
        b"damaged original file".as_slice(),
        b"PK\x03\x04broken zip".as_slice(),
    ] {
        write(&path, bytes);
        let report = lib.scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.removed, 0);
        assert_eq!(
            serde_json::to_value(lib.book(&id).unwrap()).unwrap(),
            before
        );
    }
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn absorbing_a_migrated_scan_keeps_a_late_download() {
    let base = testdir("absorb-migration-download");
    let store = base.join("store");
    let docs = store.join("documents");
    let data = base.join("data");
    write(&docs.join("first.txt"), b"First original book");
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&docs, &ScanOptions::default()).unwrap();
    lib.save().unwrap();
    let mut worker = Library::open(&data).unwrap();
    worker.scan(&store, &ScanOptions::default()).unwrap();
    write(&docs.join("late.txt"), b"Late original download");
    lib.scan(&docs, &ScanOptions::default()).unwrap();
    let id = lib
        .books()
        .iter()
        .find(|b| b.file.path == "late.txt")
        .unwrap()
        .id
        .clone();
    lib.book_mut(&id).unwrap().status = Status::Reading;
    lib.absorb_scan(worker.index().clone());
    let book = lib.book(&id).unwrap();
    assert_eq!(book.status, Status::Reading);
    assert_eq!(book.file.path, "documents/late.txt");
    assert_eq!(
        lib.absolute_path(book).unwrap(),
        fs::canonicalize(docs.join("late.txt")).unwrap()
    );
    assert_eq!(lib.books().len(), 2);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn a_damaged_original_and_a_new_copy_keep_unique_ids() {
    let base = testdir("damaged-and-copy");
    let root = base.join("documents");
    let content = epub3("Original", "Author", "Author", None, 10);
    write(&root.join("a-new.epub"), b"unrecognized content");
    write(&root.join("z-original.epub"), &content);
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = lib.books()[0].id.clone();
    lib.book_mut(&id).unwrap().status = Status::Reading;
    write(&root.join("a-new.epub"), &content);
    write(&root.join("z-original.epub"), b"damaged original");
    let report = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(report.errors.len(), 1);
    assert_eq!(lib.books().len(), 2);
    assert_ne!(lib.books()[0].id, lib.books()[1].id);
    assert_eq!(lib.book(&id).unwrap().file.path, "z-original.epub");
    assert_eq!(lib.book(&id).unwrap().status, Status::Reading);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn an_equivalent_root_spelling_keeps_duplicate_identity() {
    let base = testdir("equivalent-root");
    let root = base.join("documents");
    let data = base.join("data");
    for path in ["a.txt", "b.txt"] {
        write(&root.join(path), b"Two copies of original test text");
    }
    let mut lib = Library::open(&data).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    lib.save().unwrap();
    let ids: Vec<_> = lib.books().iter().map(|b| b.id.clone()).collect();
    let path = data.join("library.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["root"] = format!("{}/", fs::canonicalize(&root).unwrap().display()).into();
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let mut lib = Library::open(&data).unwrap();
    let report = lib.scan(&root, &ScanOptions::default()).unwrap();
    assert!(report.migrated);
    assert_eq!(report.unchanged, 2);
    assert_eq!(report.restored, 0);
    assert_eq!(
        lib.books().iter().map(|b| b.id.clone()).collect::<Vec<_>>(),
        ids
    );
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn readest_push_ack_keeps_txt_reading_state_local() {
    let base = testdir("txt-push-ack");
    let root = base.join("documents");
    write(&root.join("book.txt"), b"Original TXT regression text");
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = lib.books()[0].id.clone();
    lib.add_annotation(&id, highlight("local-txt"));
    lib.remove_annotations(&id, |_| true);
    let submitted = lib.book(&id).unwrap().clone();
    let ack = lib.acknowledge_push(&id, &submitted);
    assert!(!ack.acknowledged);
    assert!(!ack.pending);
    assert_eq!(
        lib.book(&id).unwrap().annotation_tombstones,
        submitted.annotation_tombstones
    );
    fs::remove_dir_all(base).unwrap();
}

/// The review list is what a scan may show and nothing more: a staged scan
/// adds no book, the candidates are found again on the next scan, and a
/// confirmed scan adds only the checked paths. TXT is a book like any other.
#[test]
fn a_staged_scan_reports_books_and_only_confirmed_paths_are_added() {
    let base = testdir("staged");
    let root = base.join("documents");
    write(&root.join("Novels/Lalka.txt"), b"Tom pierwszy.\n");
    write(
        &root.join("Novels/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", None, 20),
    );
    let mut lib = Library::open(base.join("data")).unwrap();
    let staged = ScanOptions {
        admission: ScanAdmission::Staged,
        covers: false,
        ..ScanOptions::default()
    };
    let report = lib.scan(&root, &staged).unwrap();
    assert_eq!((report.added, lib.books().len()), (0, 0));
    let found: Vec<(&str, &str, bool)> = report
        .candidates
        .iter()
        .map(|c| (c.path.as_str(), c.format.as_str(), c.is_new()))
        .collect();
    assert_eq!(
        found,
        [
            ("Novels/Lalka.txt", "TXT", true),
            ("Novels/Uczta.epub", "EPUB", true),
        ]
    );

    // An unchecked candidate is not remembered as ignored: a later scan
    // still finds it.
    let again = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(HashSet::new()),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(again.candidates.len(), 2);
    assert_eq!(again.ignored, 0);

    let selected: HashSet<String> = ["Novels/Lalka.txt".to_string()].into_iter().collect();
    let confirmed = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(selected),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(confirmed.added, 1);
    let titles: Vec<&str> = lib.books().iter().map(|b| b.meta.title.as_str()).collect();
    assert_eq!(titles, ["Lalka"], "only the checked book is imported");
    assert_eq!(
        confirmed
            .candidates
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["Novels/Uczta.epub"],
        "the unchecked book waits for its own confirmation"
    );

    let selected: HashSet<String> = ["Novels/Uczta.epub".to_string()].into_iter().collect();
    let confirmed = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(selected),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!(confirmed.added, 1);
    assert_eq!(lib.books().len(), 2);
    fs::remove_dir_all(base).unwrap();
}

/// Starting the app or a background scan may refresh what the index already
/// knows, but a new file waits for the reader. Reading state survives, and
/// the file is still listed by a later review.
#[test]
fn an_automatic_scan_never_admits_a_new_file() {
    let base = testdir("auto-no-admit");
    let root = base.join("documents");
    write(&root.join("Lalka.txt"), b"Tom pierwszy.\n");
    write(
        &root.join("Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", None, 20),
    );
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = lib
        .books()
        .iter()
        .find(|b| b.file.path == "Lalka.txt")
        .unwrap()
        .id
        .clone();
    lib.set_progress(
        &id,
        Progress {
            fraction: 0.6,
            locator: Locator::Text { offset: 12 },
            updated: 99,
            source: None,
            page: None,
        },
    );
    // A book appears on the store and an indexed one changes.
    write(&root.join("Nowa.txt"), b"Nowa powiesc.\n");
    write(&root.join("Lalka.txt"), b"Tom pierwszy, poprawiony.\n");

    let report = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(HashSet::new()),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!((report.added, report.updated), (0, 1));
    assert_eq!(lib.books().len(), 2, "the new file is not on the shelf");
    let updated = lib
        .books()
        .iter()
        .find(|b| b.file.path == "Lalka.txt")
        .unwrap();
    assert_eq!(updated.progress.as_ref().unwrap().fraction, 0.6);
    assert_eq!(
        report
            .candidates
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["Nowa.txt"]
    );

    // Confirming it later imports only it; the known book is not added twice.
    let selected: HashSet<String> = ["Nowa.txt".to_string()].into_iter().collect();
    let report = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(selected),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!((report.added, lib.books().len()), (1, 3));
    fs::remove_dir_all(base).unwrap();
}

/// Several sources are walked as one review list, and overlapping picks do
/// not offer the same file twice.
#[test]
fn multiple_sources_make_one_list_without_duplicates() {
    let base = testdir("multi-source");
    let store = base.join("store");
    write(&store.join("documents/novels/Lalka.txt"), b"Lalka.\n");
    write(&store.join("documents/poems/Treny.txt"), b"Treny.\n");
    write(
        &store.join("downloads/Uczta.epub"),
        &epub3("Uczta", "Platon", "Platon", None, 20),
    );
    let mut lib = Library::open(base.join("data")).unwrap();
    let staged = ScanOptions {
        admission: ScanAdmission::Staged,
        covers: false,
        ..ScanOptions::default()
    };
    // `documents` already contains `documents/novels`: deduplicated.
    let sources = vec![
        store.join("documents"),
        store.join("documents/novels"),
        store.join("downloads"),
    ];
    let report = lib.scan_sources(&store, &sources, &staged).unwrap();
    let paths: Vec<&str> = report.candidates.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "documents/novels/Lalka.txt",
            "documents/poems/Treny.txt",
            "downloads/Uczta.epub"
        ]
    );
    assert_eq!(lib.books().len(), 0);
    fs::remove_dir_all(base).unwrap();
}

/// A book the reader deleted is not quietly restored by an automatic scan
/// when its file is still on the store: only an explicit confirmation may
/// bring it back (UX-112 review point 1).
#[test]
fn an_automatic_scan_does_not_restore_a_removed_book() {
    let base = testdir("removed-no-restore");
    let root = base.join("documents");
    write(&root.join("Lalka.txt"), b"Tom pierwszy.\n");
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    let id = lib.books()[0].id.clone();
    lib.remove_book(&id);
    let report = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(HashSet::new()),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!((report.added, report.restored), (0, 0));
    assert!(lib.books().is_empty(), "the deleted book stays deleted");
    assert_eq!(
        report
            .candidates
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["Lalka.txt"],
        "it waits on the review list like any other file"
    );

    let selected: HashSet<String> = ["Lalka.txt".to_string()].into_iter().collect();
    let report = lib
        .scan(
            &root,
            &ScanOptions {
                admission: ScanAdmission::Confirmed(selected),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!((report.added, report.restored), (0, 1));
    assert_eq!(lib.books().len(), 1, "explicit confirmation restores it");
}

/// A folder the walk now skips keeps the records an earlier scan imported
/// from it: an exclusion is not permission to delete the reader's books
/// (UX-112 review point 2).
#[test]
fn an_excluded_app_folder_keeps_its_existing_records() {
    let base = testdir("excluded-keeps-records");
    let root = base.join("store");
    write(&root.join("appdata/notes.txt"), b"configuration\n");
    let mut lib = Library::open(base.join("data")).unwrap();
    lib.scan(&root, &ScanOptions::default()).unwrap();
    assert_eq!(lib.books().len(), 1);
    let report = lib
        .scan(
            &root,
            &ScanOptions {
                root_skip_dirs: vec!["appdata".into()],
                admission: ScanAdmission::Confirmed(HashSet::new()),
                covers: false,
                ..ScanOptions::default()
            },
        )
        .unwrap();
    assert_eq!((report.removed, lib.books().len()), (0, 1));
    assert_eq!(lib.books()[0].file.path, "appdata/notes.txt");
    fs::remove_dir_all(base).unwrap();
}
