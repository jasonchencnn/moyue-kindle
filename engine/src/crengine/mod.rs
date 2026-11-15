//! crengine-ng 排版引擎实现。
//!
//! 底层是 crengine-ng 0.9.15（GPLv2+，与本仓库的 AGPL-3.0-or-later 兼容），
//! 以静态库方式链进来。源码由 `engine/fetch.sh` 拉取，放在 `engine/vendor/`
//! 下，不进版本库。
//!
//! # 线程安全
//!
//! crengine-ng 的字体管理器是进程级全局对象，引擎内部不加锁。
//! 本实现用一把进程级互斥锁把引擎调用串起来，因此可以从多个线程调用，
//! 但同一时刻只有一个引擎操作在跑。渲染一页约 60 ms，串行化对阅读器够用。
//!
//! 那把锁住在 FFI 层（[`ffi`] 模块）：每一次进入 C shim 的调用都在锁上排队，
//! 调用方不需要自己加锁。文档类型因此是 `Send` 的：阅读器可以在工作线程里
//! 开书，再把文档交给主线程。

mod ffi;

pub use ffi::MAX_OPEN_DOCUMENTS;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use inkread_platform::GrayBuffer;

use crate::{
    validate_page_size, Document, Engine, Error, LayoutSettings, Link, Result, TocEntry, Word,
};

/// 墨阅内置样式表。
///
/// 打进二进制的默认排版规则：正文首行缩进 2em、标题加粗放大。
///
/// 这是**基础**样式，不是唯一样式：书内自带的 `<style>`、外链 CSS、行内
/// `style` 属性都照常生效，在基础样式之上层叠（crengine 的
/// `PROP_EMBEDDED_STYLES`，默认开启，见 ADR-010）。要强制统一的规则，
/// 在这里写 `!important`。
pub const DEFAULT_CSS: &str = include_str!("../../assets/default.css");

/// 合并默认样式与用户/排版设置额外样式。
fn effective_css(layout: &LayoutSettings) -> String {
    match &layout.user_css {
        Some(extra) => format!("{DEFAULT_CSS}\n{extra}"),
        None => DEFAULT_CSS.to_string(),
    }
}

/// 文档缓存的当前状态。
///
/// 缓存是进程级的：crengine 只有一个缓存实例，初始化一次之后目录就定死了。
struct CacheState {
    /// 缓存目录。第一次 [`configure_cache`] 传进来的那个。
    dir: PathBuf,
    /// 初始化结果。第一次失败就永远失败：缓存只是加速手段，不该拖住开书。
    result: std::result::Result<(), String>,
}

/// 已经初始化过的缓存状态。
static CACHE: OnceLock<CacheState> = OnceLock::new();

/// 打开 crengine 的文档缓存。
///
/// 缓存把解析好的 DOM 写到磁盘：下一次打开同一本书时不用再解析一遍。
///
/// 排版结果能不能一起复用，要看 crengine 的渲染上下文比对（样式哈希、样式表
/// 哈希、文档标志、页面宽高都对得上才复用）。书自带 CSS 时往往对不上，
/// 那就只省下解析，排版照旧重做。
///
/// 这是**进程级**设置：整个进程只有一个缓存实例，第一次调用生效。之后的
/// 调用必须用同一个目录，否则返回 [`Error::Engine`]——换目录等于换缓存，
/// 而缓存已经建起来了。
///
/// 不调用这个函数时，引擎行为和没有缓存时完全一样。
///
/// `max_bytes` 是缓存目录的容量上限（字节）：超了 crengine 会从最旧的
/// 缓存文件开始删。
pub fn configure_cache(dir: &Path, max_bytes: u64) -> Result<()> {
    let state = CACHE.get_or_init(|| CacheState {
        dir: dir.to_path_buf(),
        result: ffi::init_cache(dir, max_bytes),
    });
    if state.dir != dir {
        return Err(Error::Engine(format!(
            "文档缓存已经初始化过（目录 {}），不能再改成 {}",
            state.dir.display(),
            dir.display()
        )));
    }
    state.result.clone().map_err(Error::Engine)
}

/// 文档缓存是不是开着。
///
/// 没调过 [`configure_cache`]、或者初始化失败，都是 false。
pub fn cache_enabled() -> bool {
    ffi::cache_enabled()
}

/// 清空缓存目录里的缓存文件。
///
/// 缓存没开时返回 [`Error::Engine`]。
pub fn clear_cache() -> Result<()> {
    ffi::clear_cache().map_err(Error::Engine)
}

/// 设置全局文档存储大小因子（默认为 2.0）。
///
/// 增大因子可扩展文本节点、元素、矩形与样式数据的最大允许解压内存空间，
/// 防止超大书籍（如《紅樓夢》）在解析或排版时触发 storage reached max allowed uncompressed size 告警与频繁 compact/swap。
/// 对小书无额外内存开销（按需按 chunk 分配）。
pub fn configure_storage_size_factor(factor: f32) {
    ffi::set_storage_size_factor(factor);
}

/// 获取当前全局文档存储大小因子。
pub fn storage_size_factor() -> f32 {
    ffi::storage_size_factor()
}

/// 注册额外的字体文件（支持增量注册）。
///
/// 进程内已注册的文件会自动去重，新文件会被注册进全局字体管理器。
pub fn register_fonts<P: AsRef<Path>>(fonts: &[P]) -> Result<()> {
    let refs: Vec<&Path> = fonts.iter().map(|p| p.as_ref()).collect();
    ffi::init_fonts(&refs).map_err(Error::Engine)
}

/// 字体文件候选。
///
/// 现在只有 macOS 开发机能跑，所以先列 macOS 自带的中文字体。
/// ENG-001b 交叉编译到 armv7 时，要换成打进安装包里的字体文件
/// （真机系统里没有 CJK 字体）。也可以用环境变量 `INKREAD_ENGINE_FONT`
/// 指定字体文件，多个之间用 `:` 分隔。
pub const FONT_CANDIDATES: &[&str] = &[
    "/System/Library/Fonts/Supplemental/Songti.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
];

/// 优先选用的中文字体名，按顺序匹配已注册的 face 列表。
///
/// 正文用宋体（`Songti SC`）：衬线体在 300 ppi 的墨水屏上比黑体更接近纸感。
const CJK_FACE_PREFERENCES: &[&str] = &[
    "Songti SC",
    "STSong",
    "Heiti SC",
    "PingFang SC",
    "Hiragino Sans GB",
    "Songti TC",
    "Arial Unicode MS",
];

/// 收集要注册的字体文件：环境变量优先，其次是候选列表里存在的。
fn collect_font_files() -> Vec<PathBuf> {
    if let Ok(value) = std::env::var("INKREAD_ENGINE_FONT") {
        let files: Vec<PathBuf> = value
            .split(':')
            .filter(|part| !part.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .collect();
        if !files.is_empty() {
            return files;
        }
    }
    FONT_CANDIDATES
        .iter()
        .filter(|candidate| Path::new(candidate).exists())
        .map(PathBuf::from)
        .collect()
}

/// 从已注册的 face 列表里挑一个能显示中文的字体名。
fn pick_face(faces: &[String]) -> Option<String> {
    for preferred in CJK_FACE_PREFERENCES {
        if let Some(found) = faces.iter().find(|face| face.as_str() == *preferred) {
            return Some(found.clone());
        }
    }
    // 退而求其次：名字里带 CJK 常见后缀的。
    faces
        .iter()
        .find(|face| {
            face.ends_with("SC")
                || face.ends_with("TC")
                || face.ends_with("JP")
                || face.ends_with("GB")
        })
        .cloned()
        .or_else(|| faces.first().cloned())
}

/// crengine-ng 排版引擎。
pub struct CrengineEngine {
    fonts: Vec<PathBuf>,
    default_face: Option<String>,
}

impl CrengineEngine {
    /// 用指定的字体文件创建引擎，并立刻注册它们。
    ///
    /// 字体注册是进程级的，重复创建引擎不会重复注册。
    pub fn new(fonts: Vec<PathBuf>) -> Result<Self> {
        if fonts.is_empty() {
            return Err(Error::Engine("没有可用的字体文件".to_string()));
        }
        let default_face = {
            ffi::init_fonts(&fonts.iter().map(PathBuf::as_path).collect::<Vec<_>>())
                .map_err(Error::Engine)?;
            pick_face(&ffi::face_names())
        };
        Ok(Self {
            fonts,
            default_face,
        })
    }

    /// 用默认候选字体创建引擎。
    pub fn with_default_fonts() -> Result<Self> {
        let fonts = collect_font_files();
        if fonts.is_empty() {
            return Err(Error::Engine(format!(
                "找不到可用的字体文件，已尝试：\n  {}",
                FONT_CANDIDATES.join("\n  ")
            )));
        }
        Self::new(fonts)
    }

    /// 已注册的字体（face）名称列表。
    pub fn font_faces(&self) -> Vec<String> {
        ffi::face_names()
    }

    /// 引擎里生效的回退字体（face 名）列表。
    ///
    /// 这是引擎的实际状态，不是排版参数里写的名字：引擎只认自己找得到的
    /// face 名，认不出来的会被丢掉。刚建好引擎时通常是空的，回退字体要等
    /// 打开文档、排版时才会定下来。
    pub fn fallback_font_faces(&self) -> Vec<String> {
        ffi::fallback_font_faces()
    }

    /// 引擎实际使用的字体文件。
    pub fn font_files(&self) -> &[PathBuf] {
        &self.fonts
    }

    /// 注册额外的字体文件（如用户放进 fonts/ 目录的自装字体）。
    ///
    /// 进程内已注册的文件会自动去重，新文件会被注册进全局字体管理器。
    pub fn register_fonts<P: AsRef<Path>>(&self, fonts: &[P]) -> Result<()> {
        register_fonts(fonts)
    }

    /// 排版参数里没写字体名时，用引擎挑的那个。
    fn resolve_layout(&self, layout: &LayoutSettings) -> LayoutSettings {
        if !layout.font_face.is_empty() {
            return layout.clone();
        }
        match &self.default_face {
            Some(face) => LayoutSettings {
                font_face: face.clone(),
                ..layout.clone()
            },
            None => layout.clone(),
        }
    }
}

impl Engine for CrengineEngine {
    fn open_with_progress(
        &self,
        path: &Path,
        page_size: (u32, u32),
        layout: &LayoutSettings,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<Box<dyn Document>> {
        layout.validate()?;
        // 页面尺寸的校验与改尺寸那条路共用一套规则：0 宽、越界的尺寸
        // 都得在碰引擎之前挡住。
        validate_page_size(page_size)?;
        let layout = self.resolve_layout(layout);
        let css = effective_css(&layout);
        let inner = ffi::Document::open(path, page_size.0, page_size.1, &layout, &css, progress)
            .map_err(Error::Open)?;
        Ok(Box::new(CrengineDocument {
            inner,
            default_face: self.default_face.clone(),
            last_layout: layout,
        }))
    }
}

/// 已经打开的 crengine-ng 文档。
struct CrengineDocument {
    inner: ffi::Document,
    default_face: Option<String>,
    /// 上一次成功用上的排版参数（已解析出字体名）。
    ///
    /// 改尺寸失败时要靠它把文档恢复回去：引擎那边只回滚了尺寸，新排版
    /// 参数还套在 view 上，不换回来的话书虽然能渲染，排版却是坏的。
    last_layout: LayoutSettings,
}

impl CrengineDocument {
    /// 排版参数里没写字体名时，用引擎挑的那个。
    fn resolve_layout(&self, layout: &LayoutSettings) -> LayoutSettings {
        if !layout.font_face.is_empty() {
            return layout.clone();
        }
        match &self.default_face {
            Some(face) => LayoutSettings {
                font_face: face.clone(),
                ..layout.clone()
            },
            None => layout.clone(),
        }
    }

    /// 用上一次成功的排版参数，在原来的尺寸上重新排版。
    ///
    /// 改尺寸失败后调用：引擎那边只回滚了尺寸，新排版参数还套在 view 上，
    /// 尺寸元数据也已经回到旧值（见 ffi 层），所以这里按旧尺寸重排一次，
    /// 文档就回到失败前的样子，调用方可以直接接着读。
    ///
    /// 不带进度回调：恢复是善后，不该再打扰调用方。
    fn restore_last_layout(&mut self) -> std::result::Result<(), String> {
        let layout = self.last_layout.clone();
        let css = effective_css(&layout);
        // inner 记的宽高只在成功时更新，失败时还是旧尺寸。
        let (width, height) = (self.inner.width(), self.inner.height());
        // 恢复是在旧尺寸上重排：局部重排开着的话，同尺寸重排会被延迟，
        // 失败留下的「0 页」状态排不回来，文档就不能直接接着读。
        // 临时关掉，让旧布局整本重排完，再把原开关恢复。
        let partial = self.inner.is_partial_rerender_enabled();
        if partial {
            self.inner.enable_partial_rerender(false)?;
        }
        let result = self.inner.set_viewport(width, height, &layout, &css, None);
        if partial {
            self.inner.enable_partial_rerender(true)?;
        }
        result
    }
}

impl Document for CrengineDocument {
    fn page_count(&self) -> u32 {
        self.inner.page_count()
    }

    fn opened_from_cache(&self) -> bool {
        self.inner.opened_from_cache()
    }

    fn reformatted(&self) -> bool {
        self.inner.reformatted()
    }

    fn can_partial_rerender(&self) -> bool {
        self.inner.can_partial_rerender()
    }

    fn enable_partial_rerender(&mut self, enable: bool) -> Result<()> {
        self.inner
            .enable_partial_rerender(enable)
            .map_err(Error::Engine)
    }

    fn is_partial_rerender_enabled(&self) -> bool {
        self.inner.is_partial_rerender_enabled()
    }

    fn is_rerender_delayed(&self) -> bool {
        self.inner.is_rerender_delayed()
    }

    fn catchup_render_with_progress(
        &mut self,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()> {
        self.inner.catchup_render(progress).map_err(Error::Render)
    }

    fn render_page(&mut self, page: u32, buffer: &mut GrayBuffer) -> Result<()> {
        self.render_page_with_options(page, buffer, &crate::RenderOptions::default())
    }

    fn render_page_with_options(
        &mut self,
        page: u32,
        buffer: &mut GrayBuffer,
        options: &crate::RenderOptions,
    ) -> Result<()> {
        if page >= self.inner.page_count() {
            return Err(Error::PageOutOfRange(format!(
                "请求第 {page} 页，文档只有 {} 页",
                self.inner.page_count()
            )));
        }
        // 缓冲宽高必须与文档页面尺寸完全一致。像素总数相同但宽高对调时，
        // 引擎按文档行宽写入、缓冲按自己的行宽解读，整页都会错行。
        if buffer.width() != self.inner.width() || buffer.height() != self.inner.height() {
            return Err(Error::Render(format!(
                "缓冲区尺寸与页面尺寸不符：期望 {}x{}，实际 {}x{}",
                self.inner.width(),
                self.inner.height(),
                buffer.width(),
                buffer.height()
            )));
        }
        if !options.invert_images && options.underline == crate::Underline::Off {
            // 最常见的一档：既不反色也不画引导线，走原来的快速路径。
            self.inner.render_page(page, buffer).map_err(Error::Render)
        } else {
            self.inner
                .render_page_with_options(
                    page,
                    buffer,
                    options.invert_images,
                    options.underline as u8,
                    options.underline_gray,
                )
                .map_err(Error::Render)
        }
    }

    fn set_layout_with_progress(
        &mut self,
        layout: &LayoutSettings,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()> {
        layout.validate()?;
        let layout = self.resolve_layout(layout);
        let css = effective_css(&layout);
        self.inner
            .set_layout(&layout, &css, progress)
            .map_err(Error::Render)?;
        // 只有成功才记：失败时旧参数仍是当前生效的那套。
        self.last_layout = layout;
        Ok(())
    }

    fn set_layout_with_size_and_progress(
        &mut self,
        layout: &LayoutSettings,
        size: (u32, u32),
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()> {
        layout.validate()?;
        // 尺寸先校验：不合法的尺寸在改动文档之前就该挡住，
        // 文档保持原样可用。
        validate_page_size(size)?;
        let layout = self.resolve_layout(layout);
        let css = effective_css(&layout);
        match self
            .inner
            .set_viewport(size.0, size.1, &layout, &css, progress)
        {
            Ok(()) => {
                self.last_layout = layout;
                Ok(())
            }
            Err(err) => {
                // 引擎只回滚了尺寸，新排版参数还套在 view 上：拿上一套
                // 成功的参数在原尺寸上重排一次，把文档恢复成能直接读的样子。
                match self.restore_last_layout() {
                    Ok(()) => Err(Error::Render(err)),
                    // 恢复也失败：两层的错误都要报出来，不能吞掉任何一层。
                    Err(restore_err) => Err(Error::Render(format!(
                        "{err}；恢复上一套排版参数也失败：{restore_err}"
                    ))),
                }
            }
        }
    }

    fn page_xpointer(&self, page: u32) -> Result<String> {
        if page >= self.inner.page_count() {
            return Err(Error::PageOutOfRange(format!(
                "请求第 {page} 页，文档只有 {} 页",
                self.inner.page_count()
            )));
        }
        self.inner.page_xpointer(page).map_err(Error::XPointer)
    }

    fn page_of_xpointer(&self, xpointer: &str) -> Result<u32> {
        self.inner
            .page_of_xpointer(xpointer)
            .map_err(Error::XPointer)
    }

    fn title(&self) -> String {
        self.inner.title()
    }

    fn author(&self) -> String {
        self.inner.author()
    }

    fn toc(&self) -> Result<Vec<TocEntry>> {
        self.inner.toc().map_err(Error::Engine)
    }

    fn page_words(&self, page: u32) -> Result<Vec<Word>> {
        if page >= self.inner.page_count() {
            return Err(Error::PageOutOfRange(format!(
                "请求第 {page} 页，文档只有 {} 页",
                self.inner.page_count()
            )));
        }
        self.inner.page_words(page).map_err(Error::Engine)
    }

    fn page_links(&self, page: u32) -> Result<Vec<Link>> {
        if page >= self.inner.page_count() {
            return Err(Error::PageOutOfRange(format!(
                "请求第 {page} 页，文档只有 {} 页",
                self.inner.page_count()
            )));
        }
        self.inner.page_links(page).map_err(Error::Engine)
    }
}
