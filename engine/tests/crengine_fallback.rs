//! 字体回退的行为测试。
//!
//! 需要 `--features crengine`，并且要有能编译 C++ 引擎的工具链。
//! 跑法见 `scripts/check-engine.sh`。
//!
//! 回退字体在 crengine 里是进程级的设置（字体管理器是全局对象），所以这些
//! 用例共用一把锁：同一时刻只允许一版书在排版。

#![cfg(feature = "crengine")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use inkread_engine::crengine::CrengineEngine;
use inkread_engine::{Engine, LayoutSettings, Word};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 正文用的拉丁字体：设备包里的 Literata，只有拉丁字形。
const LATIN_FACE: &str = "Literata";

/// 回退用的中文字体：设备包里的 DejaVuSans-Symbols.ttf，内含 Noto Sans SC。
const CJK_FACE: &str = "Noto Sans SC";

/// 回退字体是全局设置：一版书换设置，别的一版书就受影响。
/// 用例之间串起来跑。
static FALLBACK_LOCK: Mutex<()> = Mutex::new(());

fn lock_fallback() -> MutexGuard<'static, ()> {
    FALLBACK_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

/// 设备包里的字体文件（只读）。
///
/// 字体在设备上，不在仓库里：机器上没有就跳过字体相关的用例。
fn device_fonts() -> Option<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME")?;
    let data = PathBuf::from(home).join(".cache/aristotle/render/device/data");
    let latin = data.join("serif/Literata-Regular.ttf");
    let cjk = data.join("fonts/DejaVuSans-Symbols.ttf");
    (latin.exists() && cjk.exists()).then_some((latin, cjk))
}

/// 用设备包的字体建引擎。字体不在就返回 None，调用方跳过。
fn device_engine() -> Option<CrengineEngine> {
    let (latin, cjk) = device_fonts()?;
    CrengineEngine::new(vec![latin, cjk]).ok()
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/mixed.epub")
}

/// 排出来的一页：词矩形 + 画面。
struct Rendered {
    words: Vec<Word>,
    buffer: GrayBuffer,
}

impl Rendered {
    /// 词在页面上的横向位置：x 与宽度。
    ///
    /// 同一行的词 y 相同，宽度才是字形来源的证据：同一个字，用哪个字体排，
    /// 宽度不一样。
    fn span(&self, text: &str) -> (i32, i32) {
        let word = self
            .words
            .iter()
            .find(|word| word.text == text)
            .unwrap_or_else(|| panic!("页面里没有 {text:?} 这个词"));
        let rect = word.rects.first().expect("词应当有矩形");
        (rect.x, rect.width)
    }

    /// 词所占矩形里的深色像素数：字形画出来没有的直接证据。
    fn ink(&self, text: &str) -> usize {
        let word = self
            .words
            .iter()
            .find(|word| word.text == text)
            .unwrap_or_else(|| panic!("页面里没有 {text:?} 这个词"));
        let rect = word.rects.first().expect("词应当有矩形");
        let mut ink = 0;
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                if self.buffer.get_pixel(x, y).unwrap_or(255) < 128 {
                    ink += 1;
                }
            }
        }
        ink
    }
}

/// 按给定的字体和回退字体排第 0 页。
fn render(engine: &CrengineEngine, face: &str, fallback: &[&str]) -> Rendered {
    let layout = LayoutSettings {
        font_face: face.to_string(),
        fallback_font_faces: fallback.iter().map(|f| f.to_string()).collect(),
        ..LayoutSettings::default()
    };
    let mut doc = engine
        .open(&fixture_path(), (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("打开测试书失败");
    let words = doc.page_words(0).expect("取第 0 页的词失败");
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    doc.render_page(0, &mut buffer).expect("渲染第 0 页失败");
    Rendered { words, buffer }
}

#[test]
fn the_fallback_font_supplies_the_glyphs_the_main_font_lacks() {
    let _guard = lock_fallback();
    let Some(engine) = device_engine() else {
        eprintln!("跳过：设备包里没有 Literata 与中文字体");
        return;
    };

    let without = render(&engine, LATIN_FACE, &[]);
    let with = render(&engine, LATIN_FACE, &[CJK_FACE]);
    let cjk_only = render(&engine, CJK_FACE, &[]);

    // 没有回退时主字体排不出汉字：字形是空的方框，宽度也不是全角。
    assert_ne!(
        without.span("墨"),
        with.span("墨"),
        "主字体没有这些汉字，回退之后宽度应当变"
    );
    assert!(
        without.ink("墨") < with.ink("墨"),
        "回退之后汉字应当真的画出来：{} -> {}",
        without.ink("墨"),
        with.ink("墨")
    );

    // 有了回退，汉字就是中文字体的字形：和直接拿中文字体排一模一样。
    assert_eq!(
        with.span("墨"),
        cjk_only.span("墨"),
        "回退排出来的汉字应当和中文字体排出来的一样"
    );
    assert_eq!(
        with.ink("墨"),
        cjk_only.ink("墨"),
        "回退排出来的汉字应当和中文字体排出来的一样"
    );
}

#[test]
fn the_fallback_font_does_not_touch_the_glyphs_the_main_font_has() {
    let _guard = lock_fallback();
    let Some(engine) = device_engine() else {
        eprintln!("跳过：设备包里没有 Literata 与中文字体");
        return;
    };

    let without = render(&engine, LATIN_FACE, &[]);
    let with = render(&engine, LATIN_FACE, &[CJK_FACE]);
    let cjk_only = render(&engine, CJK_FACE, &[]);

    // 拉丁文还是主字体的字形：有没有回退都一样。
    assert_eq!(
        without.span("Alpha"),
        with.span("Alpha"),
        "回退字体不该动主字体本来就有的字形"
    );
    assert_eq!(without.ink("Alpha"), with.ink("Alpha"));

    // 而且确实不是中文字体排的：中文字体的字形宽度不一样。
    assert_ne!(
        with.span("Alpha"),
        cjk_only.span("Alpha"),
        "拉丁文应当走主字体，不是回退字体"
    );
}

#[test]
fn the_engine_reports_the_fallback_faces_that_are_in_effect() {
    let _guard = lock_fallback();
    let Some(engine) = device_engine() else {
        eprintln!("跳过：设备包里没有 Literata 与中文字体");
        return;
    };

    // 引擎只认自己找得到的 face 名：认不出来的会被丢掉。
    let _ = render(&engine, LATIN_FACE, &["No Such Face"]);
    assert!(
        engine.fallback_font_faces().is_empty(),
        "认不出来的 face 名不该生效，实际生效：{:?}",
        engine.fallback_font_faces()
    );

    let _ = render(&engine, LATIN_FACE, &[CJK_FACE]);
    assert_eq!(
        engine.fallback_font_faces(),
        vec![CJK_FACE.to_string()],
        "生效的回退字体应当是排版参数里指定的那个"
    );

    // 换回不用回退：全局设置要跟着清掉。
    let _ = render(&engine, LATIN_FACE, &[]);
    assert!(
        engine.fallback_font_faces().is_empty(),
        "不用回退字体时不该留着上一本的名单"
    );
}
