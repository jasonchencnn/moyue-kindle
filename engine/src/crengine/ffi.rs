//! crengine-ng 的 FFI 绑定。
//!
//! 本模块是整个 `inkread-engine` crate 里唯一允许出现 `unsafe` 的地方，
//! 每一处都带 `// SAFETY:` 说明前提。上一层（`crate::crengine`）只暴露安全接口。

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use inkread_platform::{GrayBuffer, Rect};

use crate::{LayoutSettings, Link, LinkTarget, TocEntry, Word};

/// crengine-ng 的进程级互斥锁。
///
/// crengine-ng 的字体管理器（`fontMan`）是进程级全局对象，引擎内部不加锁：
/// `LVDocView` 的互斥锁是每个实例自己的，两个文档可以同时渲染，同时去碰
/// 同一个 `fontMan` 就会踩坏它的内部状态（实测会 SIGSEGV / SIGBUS）。
///
/// 每一个进入 C shim 的调用都在这把锁上排队：字体初始化、开书、排版、
/// 渲染、XPointer、目录、词、链接，销毁文档也算。锁的粒度是「一次 FFI
/// 调用」——调用方不需要（也不该）自己加锁。
static ENGINE_LOCK: Mutex<()> = Mutex::new(());

/// 同时打开的文档上限。
///
/// crengine 的文档实例表只有 16 个槽位（`MAX_DOCUMENT_INSTANCE_COUNT`），
/// 每次 `cr_doc_open` 会临时多注册一个解析用文档，实测 14 本同时打开是安全的，
/// 第 15 本起 `registerDocument()` 返回 -1，随后 `setNodeStyle()` 拿到空
/// 文档指针并 SIGSEGV（QA-101 §5）。所以在打开入口先拦截，超限返回错误。
pub const MAX_OPEN_DOCUMENTS: usize = 14;

/// 当前打开的文档数。只保护计数本身，不覆盖引擎调用；`ENGINE_LOCK` 仍然
/// 负责把 crengine 的调用串起来。
static OPEN_DOCUMENTS: Mutex<usize> = Mutex::new(0);

/// 预占一个文档槽位；失败时文档数不变。
fn reserve_document_slot() -> Result<(), String> {
    let mut open = OPEN_DOCUMENTS.lock().unwrap_or_else(|err| err.into_inner());
    if *open >= MAX_OPEN_DOCUMENTS {
        return Err(format!(
            "同时打开的文档已达上限 {MAX_OPEN_DOCUMENTS} 本，请先关闭不再阅读的书"
        ));
    }
    *open += 1;
    Ok(())
}

/// 归还 [`reserve_document_slot`] 预占的槽位。
fn release_document_slot() {
    let mut open = OPEN_DOCUMENTS.lock().unwrap_or_else(|err| err.into_inner());
    *open = open.saturating_sub(1);
}

/// 在引擎锁的保护下调用 shim。
///
/// 上一个持锁的线程 panic 时锁会中毒，这时照样放行：把中毒锁里的值取出来
/// （`PoisonError::into_inner`）继续用，而不是让第二次 panic 把整个引擎卡
/// 死。crengine 的全局状态可能已经不干净，但继续跑总比整个引擎再也用不了
/// 好——而且真正会 panic 的地方在 shim 里已经被 C++ 异常挡掉了。
fn with_engine_lock<T>(call: impl FnOnce() -> T) -> T {
    let _guard = ENGINE_LOCK.lock().unwrap_or_else(|err| err.into_inner());
    call()
}

/// shim.h 里的不透明文档句柄。
#[repr(C)]
pub struct CrDoc {
    _private: [u8; 0],
}

/// shim.h 里的排版参数结构，字段顺序必须与 C 侧一致。
#[repr(C)]
#[derive(Clone, Copy)]
struct CrLayout {
    margin_left: c_int,
    margin_top: c_int,
    margin_right: c_int,
    margin_bottom: c_int,
    font_face: *const c_char,
    font_size: c_int,
    line_spacing_percent: c_int,
    css: *const c_char,
    embedded_styles: c_int,
    fallback_font_faces: *const c_char,
}

/// 排版参数在 FFI 边界上的表示。
///
/// `CrLayout` 里的字符串字段都是裸指针，指向的内存必须活到 shim 调用结束。
/// 这个结构把那些 CString 一起带着，调用方只要让它活到调用结束就行。
struct CrLayoutArgs {
    settings: LayoutSettings,
    face: Option<CString>,
    fallback_faces: Option<CString>,
    css: CString,
}

impl CrLayoutArgs {
    /// 把 Rust 侧的排版参数搬进 C 侧的结构。
    fn new(settings: &LayoutSettings, css: &str) -> Result<Self, String> {
        let face = if settings.font_face.is_empty() {
            None
        } else {
            Some(
                CString::new(settings.font_face.as_str())
                    .map_err(|_| "字体名包含 NUL 字节".to_string())?,
            )
        };
        // 回退字体是 face 名列表：分号隔开，和 crengine 自己的写法一样。
        let fallback_faces = if settings.fallback_font_faces.is_empty() {
            None
        } else {
            Some(
                CString::new(settings.fallback_font_faces.join("; "))
                    .map_err(|_| "回退字体名包含 NUL 字节".to_string())?,
            )
        };
        let css_c = CString::new(css).map_err(|_| "样式表包含 NUL 字节".to_string())?;
        Ok(Self {
            settings: settings.clone(),
            face,
            fallback_faces,
            css: css_c,
        })
    }

    /// C 侧看到的排版参数。
    ///
    /// 里面的字符串指针都指向本结构自己带的 CString：`self` 得活到 shim
    /// 调用结束。
    fn layout(&self) -> CrLayout {
        CrLayout {
            margin_left: self.settings.margins.left as c_int,
            margin_top: self.settings.margins.top as c_int,
            margin_right: self.settings.margins.right as c_int,
            margin_bottom: self.settings.margins.bottom as c_int,
            font_face: self.face.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            font_size: self.settings.font_size_px as c_int,
            line_spacing_percent: self.settings.line_spacing_percent as c_int,
            css: self.css.as_ptr(),
            embedded_styles: c_int::from(self.settings.embedded_styles),
            fallback_font_faces: self
                .fallback_faces
                .as_ref()
                .map_or(std::ptr::null(), |c| c.as_ptr()),
        }
    }
}

/// shim.h 里的像素矩形。
#[repr(C)]
#[derive(Clone, Copy)]
struct CrRect {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
}

/// shim.h 里的目录项。
#[repr(C)]
struct CrTocItem {
    title: *mut c_char,
    level: c_int,
    xpointer: *mut c_char,
    page: c_int,
}

/// shim.h 里的目录列表。
#[repr(C)]
struct CrTocList {
    count: c_int,
    items: *mut CrTocItem,
}

/// shim.h 里的词。
#[repr(C)]
struct CrWord {
    text: *mut c_char,
    start_xpointer: *mut c_char,
    end_xpointer: *mut c_char,
    rect_count: c_int,
    rects: *mut CrRect,
}

/// shim.h 里的词列表。
#[repr(C)]
struct CrWordList {
    count: c_int,
    words: *mut CrWord,
}

/// shim.h 里的链接。
#[repr(C)]
struct CrLink {
    href: *mut c_char,
    is_internal: c_int,
    target_xpointer: *mut c_char,
    target_page: c_int,
    rect_count: c_int,
    rects: *mut CrRect,
}

/// shim.h 里的链接列表。
#[repr(C)]
struct CrLinkList {
    count: c_int,
    links: *mut CrLink,
}

extern "C" {
    fn cr_fonts_init(
        font_paths: *const *const c_char,
        font_count: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_fonts_face_count() -> c_int;
    fn cr_fonts_face_name(index: c_int) -> *const c_char;
    fn cr_fonts_fallback_faces(out: *mut c_char, out_len: usize) -> c_int;
    fn cr_cache_init(
        dir: *const c_char,
        max_size: c_ulong,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_cache_enabled() -> c_int;
    fn cr_cache_clear(err: *mut c_char, err_len: usize) -> c_int;
    fn cr_doc_from_cache(doc: *const CrDoc) -> c_int;
    fn cr_doc_reformatted(doc: *const CrDoc) -> c_int;
    fn cr_doc_can_partial_rerender(doc: *const CrDoc) -> c_int;
    fn cr_doc_enable_partial_rerender(
        doc: *mut CrDoc,
        enable: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_is_partial_rerender_enabled(doc: *const CrDoc) -> c_int;
    fn cr_doc_is_rerender_delayed(doc: *const CrDoc) -> c_int;
    fn cr_doc_catchup_render(
        doc: *mut CrDoc,
        progress_cb: Option<CrProgressCb>,
        user_data: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_storage_size_factor_set(factor: f32);
    fn cr_storage_size_factor_get() -> f32;
    fn cr_doc_open(
        path: *const c_char,
        width: c_int,
        height: c_int,
        layout: *const CrLayout,
        progress_cb: Option<CrProgressCb>,
        user_data: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> *mut CrDoc;
    fn cr_doc_close(doc: *mut CrDoc);
    fn cr_doc_page_count(doc: *const CrDoc) -> c_int;
    fn cr_doc_render_page(
        doc: *mut CrDoc,
        page: c_int,
        dst: *mut u8,
        dst_len: usize,
        dst_width: c_int,
        dst_height: c_int,
        invert_images: c_int,
        underline: c_int,
        underline_gray: c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_set_layout(
        doc: *mut CrDoc,
        layout: *const CrLayout,
        progress_cb: Option<CrProgressCb>,
        user_data: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_set_viewport(
        doc: *mut CrDoc,
        width: c_int,
        height: c_int,
        layout: *const CrLayout,
        progress_cb: Option<CrProgressCb>,
        user_data: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_page_xpointer(
        doc: *const CrDoc,
        page: c_int,
        out: *mut c_char,
        out_len: usize,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_page_of_xpointer(
        doc: *const CrDoc,
        xpointer: *const c_char,
        out_page: *mut c_int,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_doc_title(doc: *const CrDoc, out: *mut c_char, out_len: usize) -> c_int;
    fn cr_doc_author(doc: *const CrDoc, out: *mut c_char, out_len: usize) -> c_int;
    fn cr_doc_toc(
        doc: *const CrDoc,
        out: *mut CrTocList,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_toc_free(list: *mut CrTocList);
    fn cr_doc_page_words(
        doc: *const CrDoc,
        page: c_int,
        out: *mut CrWordList,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_words_free(list: *mut CrWordList);
    fn cr_doc_page_links(
        doc: *const CrDoc,
        page: c_int,
        out: *mut CrLinkList,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
    fn cr_links_free(list: *mut CrLinkList);
}

/// 错误缓冲区长度，和 shim.h 约定一致。
const ERR_LEN: usize = 512;

/// 字符串输出缓冲区长度。书名、作者、XPointer 都放得下。
const OUT_LEN: usize = 1024;

/// 已注册的字体文件路径集合。
///
/// crengine 的字体管理器是进程级全局对象。通过记录已注册的文件路径，
/// 可以在多次调用时避免重复注册相同文件，同时允许动态追加新字体（如用户自装字体）。
static REGISTERED_FONTS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// 把路径转成 C 字符串。路径允许含非 UTF-8 字节。
fn path_to_cstring(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("路径包含 NUL 字节：{}", path.display()))
}

/// 读取 shim 写进错误缓冲区的文本。
fn take_error(buf: &[c_char]) -> String {
    // SAFETY: shim 总是以 NUL 结尾写错误信息；buf 长度固定 ERR_LEN，
    // 且 buf 本身是合法的可读内存。
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, buf.len()) };
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// 把 shim 分配的 C 字符串拷进 Rust 的 String。
///
/// shim 分配的字符串在对应的 `cr_*_free` 调用之前一直有效。
fn take_c_string(ptr: *mut c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: ptr 非空，shim 保证它指向以 NUL 结尾的字符串，且在
    // `cr_*_free` 调用之前一直有效。
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// 把 shim 分配的矩形数组拷进 Rust。
fn take_rects(rects: *mut CrRect, count: c_int) -> Vec<Rect> {
    let count = count.max(0) as usize;
    let mut out = Vec::with_capacity(count);
    if rects.is_null() {
        return out;
    }
    // SAFETY: rects 指向 shim 分配的 count 个 CrRect 的数组，count 与数组
    // 长度一致；数组在 `cr_*_free` 调用之前一直有效。
    let slice = unsafe { std::slice::from_raw_parts(rects, count) };
    for rect in slice {
        out.push(Rect::new(rect.x, rect.y, rect.width, rect.height));
    }
    out
}

/// 初始化字体管理器或注册新的字体文件。
///
/// 首次调用时初始化 fontMan 并注册给定字体。
/// 后续调用时只向 fontMan 追加未注册过的新字体文件。若所有文件均已注册，则直接返回 Ok(())。
pub fn init_fonts(font_paths: &[&Path]) -> Result<(), String> {
    let mut reg = REGISTERED_FONTS.lock().unwrap_or_else(|e| e.into_inner());
    match &mut *reg {
        None => {
            let res = register_fonts(font_paths);
            if res.is_ok() {
                let mut set = HashSet::new();
                for p in font_paths {
                    set.insert(p.to_path_buf());
                }
                *reg = Some(set);
            }
            res
        }
        Some(set) => {
            let new_paths: Vec<&Path> = font_paths
                .iter()
                .copied()
                .filter(|p| !set.contains(*p))
                .collect();
            if new_paths.is_empty() {
                return Ok(());
            }
            let res = register_fonts(&new_paths);
            if res.is_ok() {
                for p in new_paths {
                    set.insert(p.to_path_buf());
                }
            }
            res
        }
    }
}

/// 真正注册字体。`fontMan` 是 crengine 的全局对象，只初始化一次。
fn register_fonts(font_paths: &[&Path]) -> Result<(), String> {
    let mut owned = Vec::with_capacity(font_paths.len());
    let mut ptrs: Vec<*const c_char> = Vec::with_capacity(font_paths.len());
    for path in font_paths {
        let c = path_to_cstring(path)?;
        ptrs.push(c.as_ptr());
        owned.push(c);
    }
    let mut err = [0 as c_char; ERR_LEN];

    // SAFETY: ptrs 里每个指针都指向 owned 中一个 NUL 结尾的 CString，在本次调用期间有效；
    // err 是合法的长度为 ERR_LEN 的可写缓冲区；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    let code = with_engine_lock(|| unsafe {
        cr_fonts_init(
            ptrs.as_ptr(),
            ptrs.len() as c_int,
            err.as_mut_ptr(),
            ERR_LEN,
        )
    });
    if code != 0 {
        let msg = take_error(&err);
        return Err(if msg.is_empty() {
            format!("字体初始化失败（代码 {code}）")
        } else {
            msg
        });
    }
    Ok(())
}

/// 已注册的字体（face）名称列表。
pub fn face_names() -> Vec<String> {
    with_engine_lock(|| {
        // SAFETY: 无参函数，返回值由 shim 管理；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let count = unsafe { cr_fonts_face_count() }.max(0);
        let mut names = Vec::with_capacity(count as usize);
        for index in 0..count {
            // SAFETY: index 在 [0, count) 内，shim 返回指向静态存储的 NUL 结尾字符串。
            let ptr = unsafe { cr_fonts_face_name(index) };
            if ptr.is_null() {
                continue;
            }
            // SAFETY: ptr 非空且由 shim 保证 NUL 结尾。
            let name = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            names.push(name);
        }
        names
    })
}

/// 生效中的回退字体（face 名）列表。
///
/// 引擎里设的回退字体名可能被 crengine 丢掉一部分：它只认自己找得到的
/// face 名。这里返回的是真正生效的那一份，没设过就是空列表。
pub fn fallback_font_faces() -> Vec<String> {
    with_engine_lock(|| {
        let mut out = [0 as c_char; OUT_LEN];
        // SAFETY: out 是合法的长度为 OUT_LEN 的可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = unsafe { cr_fonts_fallback_faces(out.as_mut_ptr(), OUT_LEN) };
        if code != 0 {
            return Vec::new();
        }
        // SAFETY: out 是合法的长度为 OUT_LEN 的缓冲区，shim 保证以 NUL 结尾。
        let text = unsafe { CStr::from_ptr(out.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        text.split(';')
            .map(str::trim)
            .filter(|face| !face.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// 打开 crengine 的文档缓存。
///
/// 缓存是进程级的：crengine 只有一个缓存实例，目录建不出来或索引写不进去时
/// 返回错误，缓存等于没开，引擎照常排版。
pub fn init_cache(dir: &Path, max_bytes: u64) -> Result<(), String> {
    let dir_c = path_to_cstring(dir)?;
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: dir_c 是 NUL 结尾的 CString，在调用期间有效；
    // err 是合法的长度为 ERR_LEN 的可写缓冲区；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    let code = with_engine_lock(|| unsafe {
        cr_cache_init(
            dir_c.as_ptr(),
            max_bytes as c_ulong,
            err.as_mut_ptr(),
            ERR_LEN,
        )
    });
    if code != 0 {
        let msg = take_error(&err);
        return Err(if msg.is_empty() {
            format!("初始化文档缓存失败（代码 {code}）")
        } else {
            msg
        });
    }
    Ok(())
}

/// 缓存是不是已经开着。
pub fn cache_enabled() -> bool {
    // SAFETY: 无参函数，返回值由 shim 管理；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    with_engine_lock(|| unsafe { cr_cache_enabled() != 0 })
}

/// 清空缓存目录里的缓存文件。
pub fn clear_cache() -> Result<(), String> {
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: err 是合法的长度为 ERR_LEN 的可写缓冲区；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    let code = with_engine_lock(|| unsafe { cr_cache_clear(err.as_mut_ptr(), ERR_LEN) });
    if code != 0 {
        let msg = take_error(&err);
        return Err(if msg.is_empty() {
            format!("清空文档缓存失败（代码 {code}）")
        } else {
            msg
        });
    }
    Ok(())
}

/// 设置全局文档存储大小因子（cre_storage_size_factor）。
pub fn set_storage_size_factor(factor: f32) {
    // SAFETY: 无指针传递，调用底层 C 函数；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    with_engine_lock(|| unsafe { cr_storage_size_factor_set(factor) });
}

/// 获取当前全局文档存储大小因子。
pub fn storage_size_factor() -> f32 {
    // SAFETY: 无参函数，返回浮点值；
    // 引擎锁保证同一时刻没有别的线程在 crengine 里。
    with_engine_lock(|| unsafe { cr_storage_size_factor_get() })
}

/// 已打开的文档。渲染时需要可变借用。
pub struct Document {
    doc: *mut CrDoc,
    width: u32,
    height: u32,
}

// SAFETY: 把文档交给另一个线程是安全的，理由有三条：
//
// 1. 每一个碰到 `self.doc` 的操作都是一次 shim 调用，都持有进程级的
//    `ENGINE_LOCK`。同一时刻只有一个线程在 crengine 里，两个线程不会同时
//    去碰全局的字体管理器（那会踩坏它的内部状态，实测 SIGSEGV / SIGBUS）。
// 2. crengine-ng 不用线程局部存储：源码里没有 `thread_local`、`__thread`、
//    `pthread_getspecific`、`pthread_key_create`（`engine/vendor/crengine-ng`
//    的 `crengine/src` 整个目录），所以 C++ 对象不与创建它的线程绑定，
//    换线程用不会踩到「这个对象属于那个线程」的假设。
// 3. 句柄只归这个结构所有：它不是 `Clone`，`Drop` 里关闭文档且只关一次，
//    关闭之后不会再有人用这个句柄。
//
// 只实现 `Send`，不实现 `Sync`：方法大多收 `&self`，共享引用跨线程会让两个
// 线程同时进 shim，`Sync` 的前提不成立。`*mut CrDoc` 本身是 `!Sync`，所以
// 不写 `unsafe impl Sync` 的话它自动就是 `!Sync`。
unsafe impl Send for Document {}

type CrProgressCb = unsafe extern "C" fn(progress: c_int, user_data: *mut c_void);

unsafe extern "C" fn progress_trampoline(progress: c_int, user_data: *mut c_void) {
    if !user_data.is_null() {
        // SAFETY: user_data 指向调用方栈上的 `&mut dyn FnMut(u32)`，
        // 在 cr_doc_open / cr_doc_set_layout 同步调用期间其生命周期严格有效。
        let cb = unsafe { &mut *(user_data as *mut &mut dyn FnMut(u32)) };
        cb(progress.clamp(0, 100) as u32);
    }
}

impl Document {
    /// 打开文档并按 `width x height` 排版，可选传入排版进度回调（0..=100）。
    ///
    /// `css` 是要注入的样式表；`layout` 是排版参数。
    pub fn open(
        path: &Path,
        width: u32,
        height: u32,
        layout: &LayoutSettings,
        css: &str,
        mut progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<Self, String> {
        let path_c = path_to_cstring(path)?;
        let args = CrLayoutArgs::new(layout, css)?;
        let cr_layout = args.layout();
        let mut err = [0 as c_char; ERR_LEN];

        let (cb, user_data) = match &mut progress {
            Some(f) => {
                let ptr: *mut &mut dyn FnMut(u32) = f;
                (
                    Some(progress_trampoline as CrProgressCb),
                    ptr as *mut c_void,
                )
            }
            None => (None, std::ptr::null_mut()),
        };

        // SAFETY: path_c 是 NUL 结尾的 CString，在调用期间有效；
        // args 带着 face、fallback_faces、css 三个 CString，cr_layout 里的
        // 指针都指向它们，在调用期间有效；
        // cr_layout 是对齐的 repr(C) 结构；
        // progress_trampoline 在调用期间同步触发，user_data 指向本栈帧上的闭包引用；
        // err 是合法的长度为 ERR_LEN 的可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        reserve_document_slot()?;
        let doc = with_engine_lock(|| unsafe {
            cr_doc_open(
                path_c.as_ptr(),
                width as c_int,
                height as c_int,
                &cr_layout,
                cb,
                user_data,
                err.as_mut_ptr(),
                ERR_LEN,
            )
        });
        if doc.is_null() {
            release_document_slot();
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("打开文档失败：{}", path.display())
            } else {
                msg
            });
        }
        Ok(Self { doc, width, height })
    }

    /// 文档的页面宽度（像素）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 文档的页面高度（像素）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 文档总页数。
    pub fn page_count(&self) -> u32 {
        let doc = self.doc;
        // SAFETY: self.doc 由 cr_doc_open 返回且尚未关闭，指针有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_page_count(doc) }).max(0) as u32
    }

    /// 这一份文档是不是从磁盘缓存里读出来的。
    pub fn opened_from_cache(&self) -> bool {
        let doc = self.doc;
        // SAFETY: self.doc 有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_from_cache(doc) != 0 })
    }

    /// 这一份文档在本次打开或修改排版时是否真正执行了排版/重排。
    pub fn reformatted(&self) -> bool {
        let doc = self.doc;
        // SAFETY: self.doc 有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_reformatted(doc) != 0 })
    }

    /// 查询当前文档是否支持局部重排。
    pub fn can_partial_rerender(&self) -> bool {
        let doc = self.doc;
        // SAFETY: self.doc 有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_can_partial_rerender(doc) != 0 })
    }

    /// 开启或关闭局部重排模式。
    pub fn enable_partial_rerender(&mut self, enable: bool) -> Result<(), String> {
        let mut err = [0 as c_char; ERR_LEN];
        let doc = self.doc;
        // SAFETY: self.doc 有效；err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = with_engine_lock(|| unsafe {
            cr_doc_enable_partial_rerender(
                doc,
                if enable { 1 } else { 0 },
                err.as_mut_ptr(),
                ERR_LEN,
            )
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("设置局部重排失败（代码 {code}）")
            } else {
                msg
            });
        }
        Ok(())
    }

    /// 查询当前文档是否开启了局部重排。
    pub fn is_partial_rerender_enabled(&self) -> bool {
        let doc = self.doc;
        // SAFETY: self.doc 有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_is_partial_rerender_enabled(doc) != 0 })
    }

    /// 查询文档在修改排版参数后是否处于延迟重排状态。
    pub fn is_rerender_delayed(&self) -> bool {
        let doc = self.doc;
        // SAFETY: self.doc 有效；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_is_rerender_delayed(doc) != 0 })
    }

    /// 局部重排后补完全书排版，可选传入排版进度回调（0..=100）。
    pub fn catchup_render(
        &mut self,
        mut progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<(), String> {
        let mut err = [0 as c_char; ERR_LEN];
        let (cb, user_data) = match &mut progress {
            Some(f) => {
                let ptr: *mut &mut dyn FnMut(u32) = f;
                (
                    Some(progress_trampoline as CrProgressCb),
                    ptr as *mut c_void,
                )
            }
            None => (None, std::ptr::null_mut()),
        };
        let doc = self.doc;
        // SAFETY: self.doc 有效；progress_trampoline 在调用期间同步触发；
        // err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = with_engine_lock(|| unsafe {
            cr_doc_catchup_render(doc, cb, user_data, err.as_mut_ptr(), ERR_LEN)
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("补完全书排版失败（代码 {code}）")
            } else {
                msg
            });
        }
        Ok(())
    }

    /// 渲染第 `page` 页（0 起）到 8 位灰度缓冲。
    pub fn render_page(&mut self, page: u32, buffer: &mut GrayBuffer) -> Result<(), String> {
        self.render_page_with_options(page, buffer, false, 0, 0)
    }

    /// 渲染第 `page` 页（0 起）到 8 位灰度缓冲，支持设置图片反色与正文引导线。
    ///
    /// `underline` 取值：0 不画、1 实线、2 点状虚线；`underline_gray` 是
    /// 引导线的灰度（0 黑、255 白），0 表示用引擎默认灰度。
    pub fn render_page_with_options(
        &mut self,
        page: u32,
        buffer: &mut GrayBuffer,
        invert_images: bool,
        underline: u8,
        underline_gray: u8,
    ) -> Result<(), String> {
        let mut err = [0 as c_char; ERR_LEN];
        let len = buffer.pixels().len();
        // SAFETY: self.doc 有效；buffer 的像素区长度是 width*height，
        // 与 shim 需要的字节数一致（shim 自己也会再校验一次）；
        // err 是合法可写缓冲区。
        let dst = buffer.pixels_mut().as_mut_ptr();
        let doc = self.doc;
        let width = buffer.width() as c_int;
        let height = buffer.height() as c_int;
        let invert = if invert_images { 1 } else { 0 };
        // SAFETY: 同上，dst 指向 len 字节的有效可写内存；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = with_engine_lock(|| unsafe {
            cr_doc_render_page(
                doc,
                page as c_int,
                dst,
                len,
                width,
                height,
                invert,
                underline as c_int,
                underline_gray as c_int,
                err.as_mut_ptr(),
                ERR_LEN,
            )
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("渲染第 {page} 页失败（代码 {code}）")
            } else {
                msg
            });
        }
        Ok(())
    }

    /// 换一套排版参数并重新排版，可选传入排版进度回调（0..=100）。
    pub fn set_layout(
        &mut self,
        layout: &LayoutSettings,
        css: &str,
        mut progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<(), String> {
        let args = CrLayoutArgs::new(layout, css)?;
        let cr_layout = args.layout();
        let mut err = [0 as c_char; ERR_LEN];

        let (cb, user_data) = match &mut progress {
            Some(f) => {
                let ptr: *mut &mut dyn FnMut(u32) = f;
                (
                    Some(progress_trampoline as CrProgressCb),
                    ptr as *mut c_void,
                )
            }
            None => (None, std::ptr::null_mut()),
        };

        // SAFETY: self.doc 有效；cr_layout 是对齐的 repr(C) 结构，
        // 其内部指针都指向 args 里的 CString，在调用期间有效；
        // progress_trampoline 在调用期间同步触发，user_data 指向本栈帧上的闭包引用；
        // err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let doc = self.doc;
        let code = with_engine_lock(|| unsafe {
            cr_doc_set_layout(doc, &cr_layout, cb, user_data, err.as_mut_ptr(), ERR_LEN)
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("重新排版失败（代码 {code}）")
            } else {
                msg
            });
        }
        Ok(())
    }

    /// 换一套排版参数，并按新的页面尺寸重新排版，可选传入排版进度回调（0..=100）。
    ///
    /// 尺寸变了要整本重排：排好版的分片是按旧宽度排的。shim 那边会把视口、
    /// 输出缓冲和尺寸元数据一起换掉，重排不出来时换回原尺寸。
    ///
    /// 只有成功之后才更新本 struct 里的宽高：失败时 Rust 侧记的还是旧尺寸，
    /// 和 shim 里回退后的尺寸对得上。
    pub fn set_viewport(
        &mut self,
        width: u32,
        height: u32,
        layout: &LayoutSettings,
        css: &str,
        mut progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<(), String> {
        let args = CrLayoutArgs::new(layout, css)?;
        let cr_layout = args.layout();
        let mut err = [0 as c_char; ERR_LEN];

        let (cb, user_data) = match &mut progress {
            Some(f) => {
                let ptr: *mut &mut dyn FnMut(u32) = f;
                (
                    Some(progress_trampoline as CrProgressCb),
                    ptr as *mut c_void,
                )
            }
            None => (None, std::ptr::null_mut()),
        };

        // SAFETY: self.doc 有效；cr_layout 是对齐的 repr(C) 结构，
        // 其内部指针都指向 args 里的 CString，在调用期间有效；
        // progress_trampoline 在调用期间同步触发，user_data 指向本栈帧上的闭包引用；
        // err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let doc = self.doc;
        let code = with_engine_lock(|| unsafe {
            cr_doc_set_viewport(
                doc,
                width as c_int,
                height as c_int,
                &cr_layout,
                cb,
                user_data,
                err.as_mut_ptr(),
                ERR_LEN,
            )
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("换页面尺寸失败（代码 {code}）")
            } else {
                msg
            });
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// 第 `page` 页（0 起）起始位置的 XPointer。
    pub fn page_xpointer(&self, page: u32) -> Result<String, String> {
        let mut out = [0 as c_char; OUT_LEN];
        let mut err = [0 as c_char; ERR_LEN];
        let doc = self.doc;
        // SAFETY: self.doc 有效；out 是合法的长度为 OUT_LEN 的可写缓冲区；
        // err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = with_engine_lock(|| unsafe {
            cr_doc_page_xpointer(
                doc,
                page as c_int,
                out.as_mut_ptr(),
                OUT_LEN,
                err.as_mut_ptr(),
                ERR_LEN,
            )
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("取第 {page} 页 XPointer 失败（代码 {code}）")
            } else {
                msg
            });
        }
        // SAFETY: out 是合法的长度为 OUT_LEN 的缓冲区，shim 保证以 NUL 结尾。
        let text = unsafe { CStr::from_ptr(out.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(text)
    }

    /// 按 XPointer 反查页码（0 起）。
    ///
    /// 定位不到时返回错误，不截成第 0 页：格式对但指不到位置的 XPointer
    /// （书换了版本、目录指向被删掉的章节）必须让调用方知道定位失败了。
    pub fn page_of_xpointer(&self, xpointer: &str) -> Result<u32, String> {
        let xp_c = CString::new(xpointer).map_err(|_| "XPointer 包含 NUL 字节".to_string())?;
        let mut page: c_int = -1;
        let mut err = [0 as c_char; ERR_LEN];
        let doc = self.doc;
        // SAFETY: self.doc 有效；xp_c 是 NUL 结尾的 CString，在调用期间有效；
        // page 是合法的可写 c_int；err 是合法可写缓冲区；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        let code = with_engine_lock(|| unsafe {
            cr_doc_page_of_xpointer(doc, xp_c.as_ptr(), &mut page, err.as_mut_ptr(), ERR_LEN)
        });
        if code != 0 {
            let msg = take_error(&err);
            return Err(if msg.is_empty() {
                format!("按 XPointer 定位页码失败（代码 {code}）")
            } else {
                msg
            });
        }
        // shim 只会写非负页码，负数说明约定被破坏了，不能当成第 0 页。
        if page < 0 {
            return Err("按 XPointer 定位页码失败：页码为负".to_string());
        }
        Ok(page as u32)
    }

    /// 书名，取不到时是空串。
    pub fn title(&self) -> String {
        with_engine_lock(|| {
            self.read_string(|out, out_len| {
                // SAFETY: self.doc 有效；out 是合法的可写缓冲区。
                unsafe { cr_doc_title(self.doc, out, out_len) }
            })
        })
    }

    /// 作者，取不到时是空串。
    pub fn author(&self) -> String {
        with_engine_lock(|| {
            self.read_string(|out, out_len| {
                // SAFETY: self.doc 有效；out 是合法的可写缓冲区。
                unsafe { cr_doc_author(self.doc, out, out_len) }
            })
        })
    }

    /// 目录（TOC）。没有目录时是空列表。
    pub fn toc(&self) -> Result<Vec<TocEntry>, String> {
        with_engine_lock(|| {
            let mut list = CrTocList {
                count: 0,
                items: std::ptr::null_mut(),
            };
            let mut err = [0 as c_char; ERR_LEN];
            // SAFETY: self.doc 有效；list 是合法的可写结构；err 是合法可写缓冲区；
            // 引擎锁保证同一时刻没有别的线程在 crengine 里。
            let code = unsafe { cr_doc_toc(self.doc, &mut list, err.as_mut_ptr(), ERR_LEN) };
            if code != 0 {
                let msg = take_error(&err);
                return Err(if msg.is_empty() {
                    format!("取目录失败（代码 {code}）")
                } else {
                    msg
                });
            }
            let count = list.count.max(0) as usize;
            let mut entries = Vec::with_capacity(count);
            if !list.items.is_null() {
                // SAFETY: shim 保证 items 指向 count 个 CrTocItem 的数组，count
                // 与数组长度一致；数组在 `cr_toc_free` 调用之前一直有效。
                let items = unsafe { std::slice::from_raw_parts(list.items, count) };
                for item in items {
                    entries.push(TocEntry {
                        title: take_c_string(item.title),
                        level: item.level.max(0) as u32,
                        xpointer: take_c_string(item.xpointer),
                        page: item.page.max(0) as u32,
                    });
                }
            }
            // SAFETY: list 是上面 cr_doc_toc 填的，shim 分配的内存在这里释放。
            unsafe { cr_toc_free(&mut list) };
            Ok(entries)
        })
    }

    /// 第 `page` 页（0 起）里的词。
    pub fn page_words(&self, page: u32) -> Result<Vec<Word>, String> {
        with_engine_lock(|| {
            let mut list = CrWordList {
                count: 0,
                words: std::ptr::null_mut(),
            };
            let mut err = [0 as c_char; ERR_LEN];
            // SAFETY: self.doc 有效；list 是合法的可写结构；err 是合法可写缓冲区；
            // 引擎锁保证同一时刻没有别的线程在 crengine 里。
            let code = unsafe {
                cr_doc_page_words(
                    self.doc,
                    page as c_int,
                    &mut list,
                    err.as_mut_ptr(),
                    ERR_LEN,
                )
            };
            if code != 0 {
                let msg = take_error(&err);
                return Err(if msg.is_empty() {
                    format!("取第 {page} 页的词失败（代码 {code}）")
                } else {
                    msg
                });
            }
            let count = list.count.max(0) as usize;
            let mut words = Vec::with_capacity(count);
            if !list.words.is_null() {
                // SAFETY: shim 保证 words 指向 count 个 CrWord 的数组，count
                // 与数组长度一致；数组在 `cr_words_free` 调用之前一直有效。
                let items = unsafe { std::slice::from_raw_parts(list.words, count) };
                for word in items {
                    words.push(Word {
                        text: take_c_string(word.text),
                        rects: take_rects(word.rects, word.rect_count),
                        start_xpointer: take_c_string(word.start_xpointer),
                        end_xpointer: take_c_string(word.end_xpointer),
                    });
                }
            }
            // SAFETY: list 是上面 cr_doc_page_words 填的，shim 分配的内存在这里释放。
            unsafe { cr_words_free(&mut list) };
            Ok(words)
        })
    }

    /// 第 `page` 页（0 起）里的链接。
    pub fn page_links(&self, page: u32) -> Result<Vec<Link>, String> {
        with_engine_lock(|| {
            let mut list = CrLinkList {
                count: 0,
                links: std::ptr::null_mut(),
            };
            let mut err = [0 as c_char; ERR_LEN];
            // SAFETY: self.doc 有效；list 是合法的可写结构；err 是合法可写缓冲区；
            // 引擎锁保证同一时刻没有别的线程在 crengine 里。
            let code = unsafe {
                cr_doc_page_links(
                    self.doc,
                    page as c_int,
                    &mut list,
                    err.as_mut_ptr(),
                    ERR_LEN,
                )
            };
            if code != 0 {
                let msg = take_error(&err);
                return Err(if msg.is_empty() {
                    format!("取第 {page} 页的链接失败（代码 {code}）")
                } else {
                    msg
                });
            }
            let count = list.count.max(0) as usize;
            let mut links = Vec::with_capacity(count);
            if !list.links.is_null() {
                // SAFETY: shim 保证 links 指向 count 个 CrLink 的数组，count
                // 与数组长度一致；数组在 `cr_links_free` 调用之前一直有效。
                let items = unsafe { std::slice::from_raw_parts(list.links, count) };
                for link in items {
                    let href = take_c_string(link.href);
                    let target = if link.is_internal != 0 {
                        LinkTarget::Internal {
                            xpointer: take_c_string(link.target_xpointer),
                            page: (link.target_page >= 0).then_some(link.target_page as u32),
                        }
                    } else {
                        LinkTarget::External { url: href.clone() }
                    };
                    links.push(Link {
                        href,
                        target,
                        rects: take_rects(link.rects, link.rect_count),
                    });
                }
            }
            // SAFETY: list 是上面 cr_doc_page_links 填的，shim 分配的内存在这里释放。
            unsafe { cr_links_free(&mut list) };
            Ok(links)
        })
    }

    /// 读一段 NUL 结尾的 UTF-8 字符串。
    fn read_string<F>(&self, call: F) -> String
    where
        F: FnOnce(*mut c_char, usize) -> c_int,
    {
        let mut out = [0 as c_char; OUT_LEN];
        // SAFETY: out 是合法的长度为 OUT_LEN 的可写缓冲区，call 只往里写。
        let code = call(out.as_mut_ptr(), OUT_LEN);
        if code != 0 {
            return String::new();
        }
        // SAFETY: out 是合法的长度为 OUT_LEN 的缓冲区，shim 保证以 NUL 结尾。
        unsafe { CStr::from_ptr(out.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        // 销毁也要排队：LVDocView 的析构会碰全局字体管理器。
        let doc = self.doc;
        // SAFETY: self.doc 由 cr_doc_open 返回，只在 Drop 里关闭一次；
        // 引擎锁保证同一时刻没有别的线程在 crengine 里。
        with_engine_lock(|| unsafe { cr_doc_close(doc) });
        release_document_slot();
    }
}
