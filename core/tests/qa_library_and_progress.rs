//! QA-002: core crate 测试（书库扫描边界与阅读进度崩溃安全性）。
//!
//! 覆盖项：
//! - 符号链接处理（文件软链接、目录软链接）
//! - 非 UTF-8 文件名（CORE-005 修复后转为回归测试）
//! - 超大目录扫描与排序
//! - FAT32 / vfat 上断电数据损坏与备份恢复（CORE-005 修复后转为回归测试）

use std::fs::{self, File};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use inkread_core::library::{scan, sort, BookFormat, SortKey};
use inkread_core::progress::{Progress, ProgressStore};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "inkread-qa-core-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn write(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, contents).expect("write test file");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// -----------------------------------------------------------------------------
// 1. 符号链接核查
// -----------------------------------------------------------------------------

#[test]
fn scan_skips_symlink_files_and_symlink_directories() {
    let dir = TempDir::new("symlinks");
    let real_book = dir.write("real_book.epub", b"epub content");
    let _real_sub_book = dir.write("sub/nested.epub", b"nested content");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        // 1. 文件符号链接：link_book.epub -> real_book.epub
        let sym_file = dir.path.join("link_book.epub");
        symlink(&real_book, &sym_file).expect("create file symlink");

        // 2. 目录符号链接：sym_dir -> sub
        let sym_dir = dir.path.join("sym_dir");
        symlink(dir.path.join("sub"), &sym_dir).expect("create dir symlink");

        let books = scan(&dir.path).expect("scan should succeed");

        // 验证：符号链接均被跳过，只有两个真实实体文件被扫出
        let titles: Vec<_> = books.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles.len(), 2, "symlinks must not be included");
        assert!(titles.contains(&"real_book"));
        assert!(titles.contains(&"nested"));
        assert!(!titles.contains(&"link_book"));

        // 验证：sym_dir/nested.epub 没有因为目录符号链接被重复扫描
        assert_eq!(
            books.iter().filter(|b| b.title == "nested").count(),
            1,
            "directory symlinks must not cause duplicate scans or loops"
        );
    }
}

// -----------------------------------------------------------------------------
// 2. 非 UTF-8 文件名
// -----------------------------------------------------------------------------

#[test]
fn defect_scan_drops_non_utf8_filename_books() {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // GBK 编码的 "书.epub" -> [0xca, 0xe9, b'.', b'e', b'p', b'u', b'b']
        // 包含非法 UTF-8 字节 0xca 0xe9
        let gbk_filename = OsStr::from_bytes(&[0xca, 0xe9, b'.', b'e', b'p', b'u', b'b']);
        let path = PathBuf::from(gbk_filename);

        // CORE-005: from_path 改用 to_string_lossy()，扩展名仍是 ASCII，因此可识别。
        assert_eq!(
            BookFormat::from_path(&path),
            Some(BookFormat::Epub),
            "non-UTF8 .epub should be detected by BookFormat::from_path"
        );
    }
}

#[test]
fn defect_title_of_returns_empty_string_for_non_utf8_path() {
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let gbk_filename = OsStr::from_bytes(&[0xca, 0xe9, b'.', b'e', b'p', b'u', b'b']);
        let dir = TempDir::new("non-utf8-title");
        let path = dir.path.join(gbk_filename);

        // APFS/HFS+ 拒绝落盘非法 UTF-8 文件名（EILSEQ），只有能创建的平台上才做
        // 端到端断言；title_of 的纯函数行为由 core/src/library.rs 单测直接覆盖。
        if fs::write(&path, b"epub content").is_err() {
            return;
        }

        // CORE-005: title_of 改用 to_string_lossy()，书名不会退化为空串。
        let books = scan(&dir.path).expect("scan should succeed");
        assert_eq!(books.len(), 1, "non-UTF-8 named book must not be dropped");
        assert_eq!(books[0].format, BookFormat::Epub);
        assert!(
            !books[0].title.is_empty(),
            "title_of() must not return an empty string for non-UTF-8 names"
        );
        assert_eq!(books[0].title, gbk_filename.to_string_lossy());
    }
}

// -----------------------------------------------------------------------------
// 3. 超大目录扫描与性能
// -----------------------------------------------------------------------------

#[test]
fn scan_and_sort_large_directory_hierarchy() {
    let dir = TempDir::new("large-dir");
    const DIR_COUNT: usize = 10;
    const BOOKS_PER_DIR: usize = 10;

    for d in 0..DIR_COUNT {
        for b in 0..BOOKS_PER_DIR {
            let relative = format!("category_{d:02}/book_{d:02}_{b:02}.epub");
            dir.write(&relative, format!("dummy content {d} {b}").as_bytes());
        }
    }

    let start = std::time::Instant::now();
    let mut books = scan(&dir.path).expect("scan large directory");
    let scan_duration = start.elapsed();

    assert_eq!(books.len(), DIR_COUNT * BOOKS_PER_DIR);
    // 100 本书扫描时间应当处于合理量级（如 < 1 秒）
    assert!(
        scan_duration.as_secs() < 2,
        "scan took too long: {scan_duration:?}"
    );

    // 验证排序：按标题升序
    let store = ProgressStore::load(dir.path.join("empty_progress.json")).unwrap();
    sort(&mut books, SortKey::Title, &store);
    assert_eq!(books[0].title, "book_00_00");
    assert_eq!(
        books[books.len() - 1].title,
        format!("book_{:02}_{:02}", DIR_COUNT - 1, BOOKS_PER_DIR - 1)
    );
}

// -----------------------------------------------------------------------------
// 4. ProgressStore 在 FAT32/vfat 上的崩溃与恢复行为
// -----------------------------------------------------------------------------

#[test]
fn progress_store_fails_with_invalid_data_on_corrupt_file() {
    let dir = TempDir::new("progress-corrupt");
    let path = dir.path.join("progress.json");

    // 模拟断电后 FAT32 目录项更新但数据块未 sync 导致的 0 字节损坏文件
    File::create(&path).expect("create 0-byte file");

    let err = ProgressStore::load(&path).expect_err("loading 0-byte file must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

    // 模拟写入一半的半截 JSON
    fs::write(&path, b"{\"hash-a\": {\"page\": 1, ").expect("write partial json");
    let err2 = ProgressStore::load(&path).expect_err("loading partial json must fail");
    assert_eq!(err2.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn defect_progress_store_lacks_bak_fallback_recovery() {
    let dir = TempDir::new("progress-bak-fallback");
    let path = dir.path.join("progress.json");
    let bak_path = dir.path.join("progress.json.bak");

    // 准备一个有效的 .bak 备份
    let mut store = ProgressStore::load(&bak_path).unwrap();
    store.set(
        "book-hash-1",
        Progress {
            xpointer: Some("/body/p[1]".to_string()),
            page: 15,
            total: 100,
            updated_at: 12345678,
        },
    );
    store.save().unwrap();
    assert!(bak_path.exists(), "backup fixture must be on disk");

    // 破坏主文件 progress.json（模拟 FAT32 写入断电损坏）
    fs::write(&path, b"").expect("write 0-byte corrupt main file");

    // CORE-005: load 在正式文件解析失败时回退到 .bak，进度不再全损。
    let loaded = ProgressStore::load(&path).expect("should fall back to .bak when main is corrupt");
    assert!(
        loaded.get("book-hash-1").is_some(),
        "progress from .bak should be preserved"
    );
}
