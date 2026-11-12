//! crengine 文档缓存的行为测试。
//!
//! 缓存是进程级的设置：一个进程只有一个缓存实例，初始化一次之后目录就定死了。
//! 所以这个文件里只放一个测试——cargo 每个集成测试文件是一个独立的进程，
//! 别的文件（`crengine_cache_unusable.rs`）才能再试另一种目录。

#![cfg(feature = "crengine")]

use std::path::{Path, PathBuf};

use inkread_engine::crengine::{cache_enabled, configure_cache, CrengineEngine};
use inkread_engine::{Engine, LayoutSettings};
use inkread_platform::GrayBuffer;

/// 目标屏尺寸（Kindle Oasis 2，7 寸 300ppi）。
const PAGE_WIDTH: u32 = 1264;
const PAGE_HEIGHT: u32 = 1680;

/// 缓存目录的容量上限。
const CACHE_MAX_BYTES: u64 = 64 * 1024 * 1024;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 够大的测试书：源文件要过 64 KB，crengine 才会走缓存那条路。
fn fixture_path() -> PathBuf {
    manifest_dir().join("fixtures/cache.epub")
}

/// 一本书打开之后能看到的东西：页数、每页的 XPointer、每页像素的指纹。
#[derive(Debug, PartialEq, Eq)]
struct Book {
    pages: u32,
    xpointers: Vec<String>,
    /// 每页像素的指纹（FNV-1a）。逐字节比全部页面太费内存，
    /// 指纹一样就说明像素一样。
    fingerprints: Vec<u64>,
    /// 是不是从缓存里读出来的。
    from_cache: bool,
    /// 本次打开是否真正执行了重排。
    reformatted: bool,
}

/// 打开一本书，把每一页都渲染一遍，记下能比的东西。
fn open_and_probe(engine: &CrengineEngine, path: &Path) -> Book {
    let layout = LayoutSettings::default();
    let mut doc = engine
        .open(path, (PAGE_WIDTH, PAGE_HEIGHT), &layout)
        .expect("打开测试书失败");
    let pages = doc.page_count();
    assert!(pages > 0, "测试书应该有页");
    let mut xpointers = Vec::with_capacity(pages as usize);
    let mut fingerprints = Vec::with_capacity(pages as usize);
    let mut buffer = GrayBuffer::new(PAGE_WIDTH, PAGE_HEIGHT);
    for page in 0..pages {
        xpointers.push(doc.page_xpointer(page).expect("取 XPointer 失败"));
        doc.render_page(page, &mut buffer).expect("渲染失败");
        fingerprints.push(fingerprint(buffer.pixels()));
    }
    Book {
        pages,
        xpointers,
        fingerprints,
        from_cache: doc.opened_from_cache(),
        reformatted: doc.reformatted(),
    }
}

/// 一页像素的指纹。
fn fingerprint(pixels: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 缓存目录里的缓存文件（crengine 写的 `.cr3`）。
fn cache_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "cr3").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// 这个测试临时用的目录。
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("inkread-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录失败");
    dir
}

#[test]
fn the_second_open_reads_the_cache_and_gives_the_same_book() {
    let root = temp_dir("cache");
    let cache_dir = root.join("cache");
    configure_cache(&cache_dir, CACHE_MAX_BYTES).expect("初始化文档缓存失败");
    assert!(cache_enabled(), "缓存应该开着");

    let engine = CrengineEngine::with_default_fonts().expect("初始化引擎失败");
    let book = fixture_path();

    // 第一次打开：照常排版，排完写进缓存。
    let first = open_and_probe(&engine, &book);
    assert!(!first.from_cache, "第一次打开不该读缓存");
    assert!(first.reformatted, "第一次打开应该执行排版");
    let files = cache_files(&cache_dir);
    assert!(!files.is_empty(), "第一次打开后应该有缓存文件");

    // 第二次打开：同一本书、同一套排版参数，读缓存。
    let second = open_and_probe(&engine, &book);
    assert!(second.from_cache, "第二次打开应该读缓存");
    assert!(!second.reformatted, "第二次打开命中排版缓存应该免重排");
    assert_eq!(first.pages, second.pages, "两次打开的页数应该一样");
    assert_eq!(
        first.xpointers, second.xpointers,
        "两次打开的每页 XPointer 应该一样"
    );
    assert_eq!(
        first.fingerprints, second.fingerprints,
        "两次打开渲染出来的像素应该一样"
    );

    // 排版参数不同（比如字号变了）：DOM 仍然从缓存载入，但重新排版，页数随之改变。
    let large_layout = LayoutSettings {
        font_size_px: 56,
        ..Default::default()
    };
    let large_doc = engine
        .open(&book, (PAGE_WIDTH, PAGE_HEIGHT), &large_layout)
        .expect("改字号打开测试书失败");
    assert!(large_doc.opened_from_cache(), "改字号后 DOM 仍从缓存载入");
    assert!(large_doc.reformatted(), "改字号后应该触发重排");
    assert_ne!(
        large_doc.page_count(),
        first.pages,
        "字号变大后总页数应该改变"
    );

    // 缓存文件坏了：退回正常排版，书照样能打开，结果还是一样。
    for file in &files {
        std::fs::write(file, b"this is not a cache file").expect("写坏缓存文件失败");
    }
    let third = open_and_probe(&engine, &book);
    assert_eq!(first.pages, third.pages, "缓存坏掉后页数应该一样");
    assert_eq!(
        first.xpointers, third.xpointers,
        "缓存坏掉后每页 XPointer 应该一样"
    );
    assert_eq!(
        first.fingerprints, third.fingerprints,
        "缓存坏掉后渲染出来的像素应该一样"
    );

    let _ = std::fs::remove_dir_all(&root);
}
