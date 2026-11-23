//! 墨阅排版引擎接口。
//!
//! 这一层只描述「怎么排版、怎么渲染」，不关心底下用哪个排版引擎。
//! 第一个实现是 crengine-ng，放在 `crengine` cargo feature 后面（默认关闭），
//! 这样没有引擎源码的机器也能跑 `scripts/check.sh`。
//!
//! ```no_run
//! use inkread_engine::{Engine, LayoutSettings};
//! # use std::path::Path;
//! # fn demo(engine: &dyn Engine) -> inkread_engine::Result<()> {
//! let layout = LayoutSettings::default();
//! let mut doc = engine.open(Path::new("book.epub"), (1264, 1680), &layout)?;
//! let mut buffer = inkread_platform::GrayBuffer::new(1264, 1680);
//! doc.render_page(0, &mut buffer)?;
//! # Ok(())
//! # }
//! ```

mod error;
mod interaction;
mod layout;
mod toc;

#[cfg(feature = "crengine")]
pub mod crengine;

pub use error::{Error, Result};
pub use interaction::{Link, LinkTarget, Word};
pub use layout::{
    validate_page_size, LayoutSettings, Margins, DEFAULT_DPI, DEFAULT_FONT_SIZE_PX,
    DEFAULT_FONT_WEIGHT, DEFAULT_LINE_SPACING_PERCENT, DEFAULT_MARGIN_HORIZONTAL_PX,
    DEFAULT_MARGIN_VERTICAL_PX, FONT_WEIGHT_BOLD, FONT_WEIGHT_LIGHT, FONT_WEIGHT_NORMAL, MAX_DPI,
    MAX_FONT_SIZE_PX, MAX_FONT_WEIGHT, MAX_LINE_SPACING_PERCENT, MAX_MARGIN_PX,
    MAX_PAGE_DIMENSION_PX, MIN_DPI, MIN_FONT_SIZE_PX, MIN_FONT_WEIGHT, MIN_LINE_SPACING_PERCENT,
    MIN_PAGE_DIMENSION_PX,
};
pub use toc::TocEntry;

/// 像素矩形。
///
/// 与 [`GrayBuffer`] 同一坐标系：左上角是原点，右边与下边是开区间。
pub use inkread_platform::Rect;

use std::path::Path;

use inkread_platform::GrayBuffer;

/// 排版引擎。
///
/// 实现负责打开文档、排版，并把页面渲染到灰度缓冲里。
pub trait Engine {
    /// 打开 `path` 处的文档，按 `page_size`（像素宽高）排版。
    ///
    /// `layout` 是本次排版用的参数；之后可以用 [`Document::set_layout`] 改。
    /// 打开失败返回 [`Error::Open`]，参数不合法返回 [`Error::InvalidLayout`]。
    fn open(
        &self,
        path: &Path,
        page_size: (u32, u32),
        layout: &LayoutSettings,
    ) -> Result<Box<dyn Document>> {
        self.open_with_progress(path, page_size, layout, None)
    }

    /// 打开 `path` 处的文档，按 `page_size`（像素宽高）排版，
    /// 可选传入排版进度回调闭包（上报 0..=100 整数进度）。
    ///
    /// 注意：回调在持有引擎全局锁的线程里同步执行，回调内部不能调用任何 crengine 接口。
    fn open_with_progress(
        &self,
        path: &Path,
        page_size: (u32, u32),
        layout: &LayoutSettings,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<Box<dyn Document>>;
}

/// 已打开的文档。
///
/// 文档内部持有排版状态，所以改排版参数与渲染页面都要可变借用。
///
/// 正文每行下方的引导线样式。
///
/// 引导线画在正文每一行的下方（像读本上的横线），不是批注的下划线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Underline {
    /// 不画引导线。
    #[default]
    Off = 0,
    /// 整条实线。
    Solid = 1,
    /// 点状虚线。
    Dotted = 2,
}

impl Underline {
    /// 全部样式，顺序与 UI 里的选择器一致。
    pub const ALL: [Underline; 3] = [Underline::Off, Underline::Solid, Underline::Dotted];

    /// 把 UI 的下标（0/1/2）变成样式，越界的一律按“不画”处理。
    pub fn from_index(index: usize) -> Underline {
        match index {
            1 => Underline::Solid,
            2 => Underline::Dotted,
            _ => Underline::Off,
        }
    }

    /// 这个样式在 UI 选择器里的下标。
    pub fn index(self) -> usize {
        match self {
            Underline::Off => 0,
            Underline::Solid => 1,
            Underline::Dotted => 2,
        }
    }
}

/// 引导线的横向范围。
///
/// 只影响画面，不影响排版：换范围不用重排，重新渲染当前页即可。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum UnderlineScope {
    /// 文字划线：只在有文字的位置画，宽度跟着这一行的文字走（默认）。
    #[default]
    TextOnly = 0,
    /// 整行划线：横跨 `margin_left..width-margin_right`，页面上没有文字的
    /// 行槽也按行高网格补满，像读本上的横格纸。
    FullLine = 1,
}

impl UnderlineScope {
    /// 全部范围，顺序与 UI 里的选择器一致。
    pub const ALL: [UnderlineScope; 2] = [UnderlineScope::TextOnly, UnderlineScope::FullLine];

    /// 把 UI 的下标（0/1）变成范围，越界的一律按「文字划线」处理。
    pub fn from_index(index: usize) -> UnderlineScope {
        match index {
            1 => UnderlineScope::FullLine,
            _ => UnderlineScope::TextOnly,
        }
    }

    /// 这个范围在 UI 选择器里的下标。
    pub fn index(self) -> usize {
        match self {
            UnderlineScope::TextOnly => 0,
            UnderlineScope::FullLine => 1,
        }
    }
}

/// 页面渲染选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOptions {
    /// 是否对页面中的图片执行反色（供夜间模式抵消全屏反色使用）。
    pub invert_images: bool,
    /// 正文每行下方的引导线。
    pub underline: Underline,
    /// 引导线的灰度（0 黑、255 白）。
    pub underline_gray: u8,
    /// 引导线的横向范围：文字宽度或整行宽度。
    pub underline_scope: UnderlineScope,
}

/// 文档可以换线程用（`Send`）：阅读器在工作线程里开书，再把文档交给主线程。
/// 引擎自己保证这么做是安全的——crengine-ng 的进程级全局状态由一把互斥锁
/// 串起来，见 [`crengine`] 模块。
pub trait Document: Send {
    /// 文档总页数。
    fn page_count(&self) -> u32;

    /// 这一份文档是不是从引擎的磁盘缓存里读出来的。
    ///
    /// 缓存只是加速手段：读缓存出来的书和现排的书内容一样。没有磁盘缓存的
    /// 引擎一律返回 false。
    fn opened_from_cache(&self) -> bool {
        false
    }

    /// 这一份文档在本次打开或修改排版时是否真正执行了排版/重排。
    ///
    /// 命中排版缓存时返回 false（直接复用缓存里的分页与排版结果，免重排）；
    /// 首次打开、无缓存、或排版参数改变触发重排时为 true。
    fn reformatted(&self) -> bool {
        true
    }

    /// 把第 `page` 页（0 起）渲染到 `buffer`。
    ///
    /// `buffer` 的尺寸应当与打开文档时给的 `page_size` 一致；
    /// 页码越界返回 [`Error::PageOutOfRange`]。
    fn render_page(&mut self, page: u32, buffer: &mut GrayBuffer) -> Result<()> {
        self.render_page_with_options(page, buffer, &RenderOptions::default())
    }

    /// 按照指定的 [`RenderOptions`] 把第 `page` 页（0 起）渲染到 `buffer`。
    fn render_page_with_options(
        &mut self,
        page: u32,
        buffer: &mut GrayBuffer,
        options: &RenderOptions,
    ) -> Result<()>;

    /// 查询当前文档是否支持局部重排。
    ///
    /// 需满足：开启文档缓存、排版过至少一次、文档包含 2 个及以上章节分片（DocFragment）。
    /// 没有局部重排能力的引擎默认返回 false。
    fn can_partial_rerender(&self) -> bool {
        false
    }

    /// 开启或关闭局部重排模式。
    ///
    /// 开启后，后续调用 [`Document::set_layout`] 会延迟全书排版，
    /// 仅在 [`Document::render_page`] 渲染当前页时局部排版当前章节。
    fn enable_partial_rerender(&mut self, enable: bool) -> Result<()> {
        let _ = enable;
        Err(Error::Engine("当前文档不支持局部重排".to_string()))
    }

    /// 查询当前文档是否开启了局部重排。
    fn is_partial_rerender_enabled(&self) -> bool {
        false
    }

    /// 查询文档在修改排版参数后是否处于延迟重排状态。
    fn is_rerender_delayed(&self) -> bool {
        false
    }

    /// 局部重排后补完全书排版（计算所有剩余章节的分页并校准总页数）。
    fn catchup_render(&mut self) -> Result<()> {
        self.catchup_render_with_progress(None)
    }

    /// 局部重排后补完全书排版，可选传入排版进度回调闭包（上报 0..=100 整数进度）。
    fn catchup_render_with_progress(
        &mut self,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()> {
        let _ = progress;
        Ok(())
    }

    /// 换一套排版参数并重新排版。
    ///
    /// 重新排版后页数、页码都可能变，之前拿到的 XPointer 也可能失效。
    fn set_layout(&mut self, layout: &LayoutSettings) -> Result<()> {
        self.set_layout_with_progress(layout, None)
    }

    /// 换一套排版参数并重新排版，可选传入排版进度回调闭包（上报 0..=100 整数进度）。
    ///
    /// 注意：回调在持有引擎全局锁的线程里同步执行，回调内部不能调用任何 crengine 接口。
    fn set_layout_with_progress(
        &mut self,
        layout: &LayoutSettings,
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()>;

    /// 换一套排版参数，并按新的页面尺寸（像素宽高）重新排版。
    ///
    /// 尺寸变了要整本重排：页数、页码都会变，之前拿到的 XPointer 也可能失效。
    /// 文档不用重开：DOM、目录、书签都还在。
    ///
    /// 引擎不支持改尺寸时返回 [`Error::Engine`]。尺寸不合法时返回
    /// [`Error::InvalidLayout`]，文档保持原样可用。
    fn set_layout_with_size(&mut self, layout: &LayoutSettings, size: (u32, u32)) -> Result<()> {
        self.set_layout_with_size_and_progress(layout, size, None)
    }

    /// 换一套排版参数并按新的页面尺寸重新排版，可选传入排版进度回调闭包
    /// （上报 0..=100 整数进度）。
    ///
    /// 注意：回调在持有引擎全局锁的线程里同步执行，回调内部不能调用任何 crengine 接口。
    fn set_layout_with_size_and_progress(
        &mut self,
        layout: &LayoutSettings,
        size: (u32, u32),
        progress: Option<&mut dyn FnMut(u32)>,
    ) -> Result<()> {
        let _ = (layout, size, progress);
        Err(Error::Engine("当前引擎不支持改页面尺寸".to_string()))
    }

    /// 第 `page` 页（0 起）起始位置的 XPointer。
    ///
    /// XPointer 是一串与文档绑定的定位串，可以存进阅读进度里；
    /// 文档重新排版后旧串可能定位到别的页。
    fn page_xpointer(&self, page: u32) -> Result<String>;

    /// 按 XPointer 反查它落在第几页（0 起）。
    ///
    /// 串无效或定位不到时返回 [`Error::XPointer`]。
    fn page_of_xpointer(&self, xpointer: &str) -> Result<u32>;

    /// 书名。解析不出来时返回空串。
    fn title(&self) -> String;

    /// 作者。解析不出来时返回空串。
    fn author(&self) -> String;

    /// 目录（TOC）。
    ///
    /// 返回摊平的列表，顺序与书里的目录一致，层级从 0 起。没有目录的书
    /// 返回空列表，不算错误。
    fn toc(&self) -> Result<Vec<TocEntry>>;

    /// 第 `page` 页（0 起）属于哪一节。
    ///
    /// 返回目录项在 [`Document::toc`] 列表里的下标：页面上同时落在几层
    /// 目录项的范围里时，取层级最深的那一个。第一个目录项之前的页不属于
    /// 任何目录项，返回 `None`。
    fn chapter_of_page(&self, page: u32) -> Result<Option<usize>> {
        let entries = self.toc()?;
        Ok(toc::chapter_of_page(&entries, page))
    }

    /// 第 `page` 页（0 起）里的词。
    ///
    /// 用来做长按选词。断词规则由引擎决定：西文按空格与标点断词，
    /// 汉字一个字算一个词。
    fn page_words(&self, page: u32) -> Result<Vec<Word>>;

    /// 第 `page` 页（0 起）里的链接。
    ///
    /// 用来做点链接与脚注跳转。
    fn page_links(&self, page: u32) -> Result<Vec<Link>>;
}
