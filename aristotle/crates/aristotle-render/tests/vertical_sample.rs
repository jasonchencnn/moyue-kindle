// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Renders a real vertical page to `docs/tasks/VERT-101/shots/vertical-sample.png`
//! so the layout can be looked at, not only asserted.
//!
//! The sample is the opening of 紅樓夢 (Cao Xueqin, 1791, public domain) with
//! a chapter heading, the terminal punctuation of a vertical setting and a
//! date in Latin digits, drawn with the device's own CJK font. Run it with the
//! data directory of a device build:
//!
//! ```text
//! ARISTOTLE_RENDER_DATA=$ARISTOTLE_CACHE/render/device/data \
//!     cargo test -p aristotle-render --no-default-features --features crengine \
//!     --test vertical_sample
//! ```
//!
//! Without a CJK font in the data directory the test skips itself: it has
//! nothing to draw with and must not fail a host without the fonts.

use std::path::PathBuf;

use aristotle_render::style::{Style, Underline, UnderlineScope};
use aristotle_render::vertical::{self, Chapter, Document, VerticalStyle};

/// 紅樓夢, chapter one: the author's preface (public domain).
const PARAGRAPHS: [&str; 3] = [
    "此開卷第一回也。作者自云：因曾歷過一番夢幻之後，故將真事隱去，而借通靈之說，撰此《石頭記》一書也。故曰甄士隱云云。但書中所記何事？自己又云：今風塵碌碌，一事無成，忽念及當日所有之女子，一一細考較去，覺其行止見識皆出我之上。",
    "我堂堂鬚眉，誠不若彼裙釵哉！實愧則有餘，悔又無益之大無可如何之日也。當此日，欲將已往所賴天恩祖德、錦衣紈袴之時，飫甘饜肥之日，背父兄教育之恩，負師友規談之德，以至今日一技無成、半生潦倒之罪，編述一集，以告天下人。",
    "我之罪固不免，然閨閣中本自歷歷有人，萬不可因我之不肖，自護己短，一併使其泯滅也。乾隆五十六年辛亥（1791）程甲本。",
];

fn sample_document() -> Document {
    Document::from_chapters(vec![
        Chapter {
            title: Some("第一回　甄士隱夢幻識通靈　賈雨村風塵懷閨秀".to_string()),
            paragraphs: PARAGRAPHS.iter().map(|p| p.to_string()).collect(),
        },
        Chapter {
            title: Some("第二回　賈夫人仙逝揚州城　冷子興演說榮國府".to_string()),
            paragraphs: vec![
                "詩云：一局輸贏料不真，香銷茶盡尚逡巡。欲知目下興衰兆，須問旁觀冷眼人。"
                    .to_string(),
            ],
        },
    ])
}

/// The output path, from the crate directory to the task's shots folder.
fn sample_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/tasks/VERT-101/shots/vertical-sample.png")
}

#[test]
fn render_the_vertical_sample_page() {
    let style = Style::pw4();
    if !vertical::has_font(&style) {
        eprintln!(
            "no CJK font in the data directory (ARISTOTLE_RENDER_DATA); \
             skipping the vertical sample"
        );
        return;
    }
    let document = sample_document();
    let vertical_style = VerticalStyle::from_style(&style);
    let layout = document.layout_page(0, &vertical_style);
    assert!(!layout.is_empty(), "the first page holds text");
    assert_eq!(layout.start, 0);
    assert!(layout.next > 0);
    assert_eq!(
        layout.chapter.as_deref(),
        Some("第一回　甄士隱夢幻識通靈　賈雨村風塵懷閨秀")
    );

    let page = vertical::render_page(&style, &layout, Underline::Solid, UnderlineScope::FullLine)
        .expect("the vertical page renders");

    // The columns run right to left and the text is drawn: the page is not
    // blank.
    let dark = page.data.iter().filter(|&&pixel| pixel < 0x80).count();
    assert!(dark > 2000, "the sample page has text ({dark} dark pixels)");

    let path = sample_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("the shots directory");
    }
    // The page keeps the framebuffer's stride, so drop the padding at the
    // end of every row before writing an image as wide as the screen.
    let width = page.width as usize;
    let mut pixels = Vec::with_capacity(width * page.height as usize);
    for row in 0..page.height as usize {
        let start = row * page.stride;
        pixels.extend_from_slice(&page.data[start..start + width]);
    }
    let file = std::fs::File::create(&path).expect("the sample file");
    let mut encoder = png::Encoder::new(file, page.width, page.height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("the png header");
    writer.write_image_data(&pixels).expect("the png data");
    drop(writer);
    eprintln!("wrote {}", path.display());
}
