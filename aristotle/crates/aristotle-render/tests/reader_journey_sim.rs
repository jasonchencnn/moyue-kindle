// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real pixel-level typography user-journey simulation tests for `aristotle-render`.
//!
//! Verifies:
//! - TC-TP-01: Fine typesetting toggle produces real pixel differences on CSS-styled EPUB
//! - TC-TP-02: Horizontal <-> VerticalRtl engine switch & progress preservation
//! - TC-TP-03: Font weight presets (Light 300 < Normal 400 < Bold 700) dark-ink pixel density
//! - TC-TP-04: Underline TextOnly vs FullLine fills across the right side with clear contrast

use std::io::Write;
use std::path::{Path, PathBuf};

use aristotle_render::book::{Book, Engine, Position};
use aristotle_render::page::Chrome;
use aristotle_render::style::{Style, Underline, UnderlineScope, WritingMode};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn data_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|v| !v.is_empty());
    let dir = var("ARISTOTLE_RENDER_DATA")
        .map(PathBuf::from)
        .or_else(|| {
            var("ARISTOTLE_CACHE")
                .map(PathBuf::from)
                .or_else(|| {
                    var("XDG_CACHE_HOME")
                        .map(PathBuf::from)
                        .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".cache")))
                        .map(|c| c.join("aristotle"))
                })
                .map(|c| c.join("render/host/data"))
        })?;
    dir.join("fonts").is_dir().then_some(dir)
}

fn use_data() -> bool {
    match data_dir() {
        Some(d) => {
            aristotle_render::use_data_dir(&d).unwrap();
            true
        }
        None => {
            eprintln!("skipped: no render data directory (run tools/build-render.sh host)");
            false
        }
    }
}

fn make_epub(name: &str, css: &str, chapters: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aristotle-render-journey-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.epub"));
    let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut put = |file: &str, data: &[u8]| {
        zip.start_file(file, opts).unwrap();
        zip.write_all(data).unwrap();
    };
    put("mimetype", b"application/epub+zip");
    put(
        "META-INF/container.xml",
        b"<?xml version=\"1.0\"?>\n<container version=\"1.0\" \
          xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles>\
          <rootfile full-path=\"content.opf\" media-type=\"application/oebps-package+xml\"/>\
          </rootfiles></container>\n",
    );
    put("style.css", css.as_bytes());
    let mut manifest = String::from(
        "<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n\
         <item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n",
    );
    let mut spine = String::new();
    let mut nav = String::new();
    for (n, body) in chapters.iter().enumerate() {
        let file = format!("c{n}.xhtml");
        put(
            &file,
            format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                 <html xmlns=\"http://www.w3.org/1999/xhtml\" lang=\"zh\" xml:lang=\"zh\">\
                 <head><title>第{n}章</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\
                 <body>{body}</body></html>\n"
            )
            .as_bytes(),
        );
        manifest.push_str(&format!(
            "<item id=\"c{n}\" href=\"{file}\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("<itemref idref=\"c{n}\"/>\n"));
        nav.push_str(&format!("<li><a href=\"{file}\">第{n}章</a></li>\n"));
    }
    put(
        "nav.xhtml",
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\
             <head><title>目录</title></head><body><nav epub:type=\"toc\"><ol>{nav}</ol></nav></body></html>\n"
        )
        .as_bytes(),
    );
    put(
        "content.opf",
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"id\">\
             <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
             <dc:identifier id=\"id\">journey-{name}</dc:identifier>\
             <dc:title>{name}</dc:title><dc:language>zh</dc:language>\
             </metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>\n"
        )
        .as_bytes(),
    );
    zip.finish().unwrap();
    path
}

fn render_first_page(path: &Path, style: &Style) -> Vec<u8> {
    let mut book = Book::open(path, style, None).expect("open epub");
    let mut chrome = Chrome::load(style).expect("chrome");
    book.render_page(0, &mut chrome)
        .expect("render page 0")
        .data
}

fn dark_ink_pixels(frame: &[u8]) -> usize {
    frame.iter().filter(|&&p| p < 200).count()
}

#[test]
fn tc_tp_01_fine_typesetting_changes_rendered_pixels() {
    if !use_data() {
        return;
    }
    let css = "h1 { text-align: center; margin-top: 120px; margin-bottom: 40px; } \
               p { text-indent: 4em; margin-top: 24px; }";
    let body = "<h1>第一回 甄士隐梦幻识通灵</h1>\
                <p>此开卷第一回也。作者自云：因曾历过一番梦幻之后，故将真事隐去，而借通灵之说，撰此《石头记》一书也。</p>\
                <p>故曰“甄士隐”云云。但书中所记何事何人？自又云：“今风尘碌碌，一事无成。”</p>";
    let path = make_epub("fine-css", css, &[body]);

    let mut plain = Style::pw4();
    plain.fine_typesetting = false;
    let frame_plain = render_first_page(&path, &plain);

    let mut fine = Style::pw4();
    fine.fine_typesetting = true;
    let frame_fine = render_first_page(&path, &fine);

    let diff = frame_plain
        .iter()
        .zip(frame_fine.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        diff > 1000,
        "fine typesetting toggle must produce substantial pixel differences (got {diff})"
    );
}

#[test]
fn tc_tp_02_horizontal_to_vertical_switch_and_progress_roundtrip() {
    if !use_data() {
        return;
    }
    let mut para = String::new();
    for _ in 0..35 {
        para.push_str("<p>此开卷第一回也。作者自云：因曾历过一番梦幻之后，故将真事隐去，而借通灵之说，撰此《石头记》一书也。满纸荒唐言，一把辛酸泪。</p>");
    }
    let path = make_epub("vert-roundtrip", "", &[&para]);

    let mut horiz = Style::pw4();
    horiz.writing_mode = WritingMode::Horizontal;
    let mut book_h = Book::open(&path, &horiz, None).expect("open horizontal");
    assert_eq!(book_h.engine(), Engine::Crengine);
    let h_pages = book_h.paginate();
    assert!(h_pages >= 3, "need multi-page book, got {h_pages}");

    let mid_page = h_pages / 2;
    let mid_frac = mid_page as f32 / h_pages as f32;
    let mut chrome_h = Chrome::load(&horiz).expect("chrome h");
    let frame_h = book_h
        .render_page(mid_page, &mut chrome_h)
        .expect("render horiz")
        .data;

    // Switch to vertical RTL engine at mid_frac.
    let mut vert = Style::pw4();
    vert.writing_mode = WritingMode::VerticalRtl;
    let mut book_v = Book::open(&path, &vert, None).expect("open vertical");
    assert_eq!(book_v.engine(), Engine::Vertical);
    let v_pages = book_v.paginate();
    assert!(v_pages >= 2, "vertical pages: {v_pages}");
    let v_loc = book_v
        .location_of(&Position::Fraction(mid_frac))
        .expect("locate mid fraction in vertical");
    let v_pos = book_v.position_at(v_loc);
    assert!(matches!(v_pos, Position::Fraction(_)));
    let mut chrome_v = Chrome::load(&vert).expect("chrome v");
    let frame_v = book_v
        .render_location(v_loc, &mut chrome_v)
        .expect("render vert")
        .data;

    let diff = frame_h
        .iter()
        .zip(frame_v.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        diff > 5000,
        "vertical and horizontal frames must differ substantially (got {diff})"
    );

    // Switch back to horizontal and confirm progress is preserved near mid_page.
    let back_loc = book_h
        .location_of(&v_pos)
        .expect("roundtrip back to horizontal");
    let back_frac = back_loc as f32 / h_pages as f32;
    assert!(
        (back_frac - mid_frac).abs() < 0.15,
        "progress drifted too far across horizontal<->vertical switch: {mid_frac:.2} -> {back_frac:.2}"
    );
}

#[test]
fn tc_tp_03_font_weight_presets_increase_ink_density() {
    if !use_data() {
        return;
    }
    let body = "<p>天地玄黄，宇宙洪荒。日月盈昃，辰宿列张。寒来暑往，秋收冬藏。闰余成岁，律吕调阳。</p>\
                <p>云腾致雨，露结为霜。金生丽水，玉出昆冈。剑号巨阙，珠称夜光。果珍李柰，菜重芥姜。</p>";
    let path = make_epub("weight", "", &[body]);

    let mut light = Style::pw4();
    light.font_weight = 300;
    let ink_light = dark_ink_pixels(&render_first_page(&path, &light));

    let mut normal = Style::pw4();
    normal.font_weight = 400;
    let ink_normal = dark_ink_pixels(&render_first_page(&path, &normal));

    let mut bold = Style::pw4();
    bold.font_weight = 700;
    let ink_bold = dark_ink_pixels(&render_first_page(&path, &bold));

    assert!(
        ink_light < ink_bold,
        "bold weight (700) must have more dark ink than light (300): light={ink_light}, normal={ink_normal}, bold={ink_bold}"
    );
    assert!(
        ink_normal <= ink_bold,
        "bold weight (700) must have at least as much dark ink as normal (400): normal={ink_normal}, bold={ink_bold}"
    );
}

#[test]
fn tc_tp_04_underline_full_line_fills_right_half_clearly() {
    if !use_data() {
        return;
    }
    let body = "<p>短句。</p><p>再一句。</p>";
    let path = make_epub("underline", "", &[body]);

    let style = Style::pw4();
    let mut chrome = Chrome::load(&style).expect("chrome");
    let mut book = Book::open(&path, &style, None).expect("open underline epub");
    book.set_underline(Underline::Solid);
    book.set_underline_scope(UnderlineScope::TextOnly);
    let frame_text = book.render_page(0, &mut chrome).expect("text only").data;

    book.set_underline_scope(UnderlineScope::FullLine);
    let frame_full = book.render_page(0, &mut chrome).expect("full line").data;

    // Count dark pixels in the right half of the page (where short lines have no text).
    let w = style.width as usize;
    let h = style.height as usize;
    let stride = style.stride;
    let count_right_dark = |frame: &[u8]| -> usize {
        let mut n = 0;
        for y in 0..h {
            for x in (w / 2)..w {
                if frame[y * stride + x] < 190 {
                    n += 1;
                }
            }
        }
        n
    };

    let right_text = count_right_dark(&frame_text);
    let right_full = count_right_dark(&frame_full);
    assert!(
        right_full > right_text + 500,
        "FullLine underline must draw clear dark ruled lines across the right half: TextOnly={right_text}, FullLine={right_full}"
    );
}
