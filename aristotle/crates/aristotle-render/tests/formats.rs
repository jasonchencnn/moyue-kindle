// SPDX-License-Identifier: AGPL-3.0-or-later
//! Default-bundle format checks with generated content.

use std::io::Write;
use std::path::PathBuf;

use aristotle_library::format::{sniff, Format};
use aristotle_render::book::{Book, Engine, Position};
use aristotle_render::style::Style;
use plato_core::png::{BitDepth, ColorType, Encoder};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(gray: u8) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(&mut out, 12, 16);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[gray; 12 * 16])
            .unwrap();
    }
    out
}

fn scratch() -> Option<Scratch> {
    let cache = std::env::var_os("ARISTOTLE_CACHE")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/aristotle")))
        .unwrap();
    let data = std::env::var_os("ARISTOTLE_RENDER_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("render/host/data"));
    if !data.join("fonts").is_dir() {
        assert!(
            std::env::var_os("CI").is_none(),
            "no render data directory in CI"
        );
        eprintln!("skipped: no render data directory (run tools/build-render.sh host)");
        return None;
    }
    aristotle_render::use_data_dir(&data).unwrap();
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "aristotle-formats-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir_all(&scratch.0).unwrap();
    Some(scratch)
}

fn comic(scratch: &Scratch, filename: &str, pages: &[(&str, u8)]) -> PathBuf {
    let path = scratch.0.join(filename);
    let mut archive = ZipWriter::new(std::fs::File::create(&path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for &(name, gray) in pages {
        archive.start_file(name, options).unwrap();
        archive.write_all(&png(gray)).unwrap();
    }
    archive.finish().unwrap();
    path
}

fn center(book: &mut Book, location: usize) -> u8 {
    let pixmap = book.render_body(location).unwrap();
    let center = (pixmap.height / 2 * pixmap.width + pixmap.width / 2) as usize;
    pixmap.data[center]
}

#[test]
fn default_bundle_renders_cbz_pages_in_natural_order() {
    let Some(scratch) = scratch() else {
        return;
    };
    let path = comic(
        &scratch,
        "generated.cbz",
        &[("page20.png", 224), ("page10.png", 128), ("page2.png", 32)],
    );

    for filename in ["generated.cbz", "generated.zip"] {
        let renamed = scratch.0.join(filename);
        if renamed != path {
            std::fs::copy(&path, &renamed).unwrap();
        }
        assert_eq!(sniff(&renamed).unwrap(), Some(Format::Cbz));
        let metadata =
            aristotle_library::metadata::extract(&renamed, Format::Cbz, None, true).unwrap();
        assert_eq!(metadata.meta.pages, Some(3));
        assert_eq!(metadata.cover, Some(png(32)));
        let mut book = Book::open(&renamed, &Style::pw4(), None).unwrap();
        assert_eq!(book.engine(), Engine::Fixed);
        assert_eq!(book.known_page_count(), Some(3));
        for (page, expected) in [32u8, 128, 224].into_iter().enumerate() {
            let location = book.locate(page).unwrap();
            assert_eq!(book.position_at(location), Position::Page(page));
            assert_eq!(book.location_of(&Position::Page(page)), Some(location));
            assert!(
                center(&mut book, location).abs_diff(expected) <= 1,
                "{filename}: page {page}"
            );
        }
        assert_eq!(book.next_page(2), None);
        assert_eq!(book.previous_page(1), Some(0));
    }
}

#[test]
fn cbz_large_numeric_names_keep_the_cover_and_first_page_in_agreement() {
    let Some(scratch) = scratch() else {
        return;
    };
    let path = comic(
        &scratch,
        "large-numbers.cbz",
        &[
            ("PAGE2147483648.png", 128),
            ("page999999999999999999999999.png", 224),
            ("page9.png", 32),
        ],
    );
    let metadata = aristotle_library::metadata::extract(&path, Format::Cbz, None, true).unwrap();
    assert_eq!(metadata.cover, Some(png(32)));
    let mut book = Book::open(&path, &Style::pw4(), None).unwrap();
    assert_eq!(book.known_page_count(), Some(3));
    for (page, expected) in [32u8, 128, 224].into_iter().enumerate() {
        assert!(
            center(&mut book, page).abs_diff(expected) <= 1,
            "page {page}"
        );
    }
}

#[test]
fn cbz_numeric_and_ascii_case_ties_use_raw_filename_order() {
    let Some(scratch) = scratch() else {
        return;
    };
    let path = comic(
        &scratch,
        "ties.cbz",
        &[
            ("page3.png", 128),
            ("page02.png", 96),
            ("PAGE0002.PNG", 64),
            ("page2.png", 32),
        ],
    );
    let mut book = Book::open(&path, &Style::pw4(), None).unwrap();
    assert_eq!(book.known_page_count(), Some(4));
    for (page, expected) in [64u8, 96, 32, 128].into_iter().enumerate() {
        assert!(center(&mut book, page).abs_diff(expected) <= 1);
    }
}

#[test]
fn cbz_shuffled_numeric_ties_keep_cover_and_reader_on_the_same_first_page() {
    let Some(scratch) = scratch() else {
        return;
    };
    let names = [
        "page008.png",
        "page09.png",
        "page08.png",
        "page01.png",
        "page002.png",
        "page009.png",
        "page07.png",
        "page05.png",
        "page4.png",
        "page006.png",
        "page06.png",
        "page1.png",
        "page03.png",
        "page8.png",
        "page3.png",
        "page5.png",
        "page007.png",
        "page005.png",
        "page6.png",
        "page2.png",
        "page003.png",
        "page001.png",
        "page9.png",
        "page7.png",
        "page02.png",
    ];
    let pages: Vec<(&str, u8)> = names
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name, (16 + i * 7) as u8))
        .collect();
    let path = comic(&scratch, "shuffled-ties.cbz", &pages);
    let metadata = aristotle_library::metadata::extract(&path, Format::Cbz, None, true).unwrap();
    assert_eq!(metadata.cover, Some(png(163)));
    let mut book = Book::open(&path, &Style::pw4(), None).unwrap();
    assert_eq!(book.known_page_count(), Some(25));
    for (page, expected) in [163u8, 37, 93].into_iter().enumerate() {
        assert!(center(&mut book, page).abs_diff(expected) <= 1);
    }
}

#[test]
fn cbz_unicode_names_use_unsigned_utf8_bytes_on_every_target() {
    let Some(scratch) = scratch() else {
        return;
    };
    let path = comic(&scratch, "unicode.cbz", &[("é.png", 224), ("a.png", 32)]);
    let metadata = aristotle_library::metadata::extract(&path, Format::Cbz, None, true).unwrap();
    assert_eq!(metadata.cover, Some(png(32)));
    let mut book = Book::open(&path, &Style::pw4(), None).unwrap();
    assert_eq!(book.known_page_count(), Some(2));
    assert!(center(&mut book, 0).abs_diff(32) <= 1);
    assert!(center(&mut book, 1).abs_diff(224) <= 1);
}

/// Build an 8-bit grayscale PNG of arbitrary dimensions from raw pixels.
fn png_rect(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), (width * height) as usize);
    let mut out = Vec::new();
    {
        let mut encoder = Encoder::new(&mut out, width, height);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(pixels)
            .unwrap();
    }
    out
}

#[test]
fn cbz_fixed_layout_fullscreen_autocrop_spread_split_and_night_images() {
    let Some(scratch) = scratch() else {
        return;
    };
    // Page 0: landscape two-page spread (160x80), left half gray=40, right half gray=180.
    let mut spread = vec![0u8; 160 * 80];
    for y in 0..80 {
        for x in 0..160 {
            spread[y * 160 + x] = if x < 80 { 40 } else { 180 };
        }
    }
    // Page 1: portrait page (100x140) with wide white margins around a dark center box (gray=24).
    let mut margined = vec![255u8; 100 * 140];
    for y in 35..105 {
        for x in 25..75 {
            margined[y * 100 + x] = 24;
        }
    }
    let path = scratch.0.join("manga-features.cbz");
    let p0 = png_rect(160, 80, &spread);
    let p1 = png_rect(100, 140, &margined);
    {
        let mut archive = ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        archive.start_file("01.png", options).unwrap();
        archive.write_all(&p0).unwrap();
        archive.start_file("02.png", options).unwrap();
        archive.write_all(&p1).unwrap();
        archive.finish().unwrap();
    }

    // 1. RTL spread split: page 0 splits into right (180) then left (40), page 1 stays whole.
    let mut style = Style::pw4();
    style.fixed_layout.fullscreen = true;
    style.fixed_layout.split_spread = true;
    style.fixed_layout.direction = aristotle_render::style::PageDirection::Rtl;
    style.fixed_layout.keep_images_in_night = true;

    let mut book = Book::open(&path, &style, None).unwrap();
    assert_eq!(book.len(), 3, "spread + portrait = 3 virtual pages");
    assert!(
        center(&mut book, 0).abs_diff(180) <= 2,
        "RTL shows right half first"
    );
    assert!(
        center(&mut book, 1).abs_diff(40) <= 2,
        "RTL shows left half second"
    );
    assert_eq!(book.position_at(0), Position::Page(0));
    assert_eq!(book.position_at(1), Position::Page(0));
    assert_eq!(book.position_at(2), Position::Page(1));

    // Night mode artwork protection records the rendered image bounds.
    let mut chrome = aristotle_render::page::Chrome::load(&style).unwrap();
    let rendered = book.render_page(0, &mut chrome).unwrap();
    assert!(
        !rendered.images.is_empty(),
        "keep_images_in_night populates page.images"
    );

    // 2. Auto-crop trims the wide white margins on page 1 so the dark box fills more of the page.
    let uncropped = book.render_body(2).unwrap();
    let dark_before = uncropped.data.iter().filter(|&&p| p < 128).count();
    style.fixed_layout.auto_crop = true;
    book.set_layout(&style).unwrap();
    let cropped = book.render_body(2).unwrap();
    let dark_after = cropped.data.iter().filter(|&&p| p < 128).count();
    assert!(
        dark_after > dark_before * 2,
        "auto-crop should enlarge the dark content area: before={dark_before}, after={dark_after}"
    );
}
