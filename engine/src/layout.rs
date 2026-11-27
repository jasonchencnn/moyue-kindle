//! 排版参数：字号、行高、页边距、字体。
//!
//! 默认值来自 `docs/design/README.md`（设计系统的字号 Token 与阅读页布局数值），
//! 范围限制也按同一份文档里「用户可调」的范围来。

use crate::error::{Error, Result};

/// 正文默认字号（设备像素）。
///
/// 来源：`docs/design/README.md` §3 字号 Token 的 `font-reading` = 46px
/// （300 ppi 下约 11.0 pt）。
pub const DEFAULT_FONT_SIZE_PX: u32 = 46;

/// 正文默认行高（百分比，100 表示单倍行距）。
///
/// 来源：`docs/design/README.md` §8.2「正文行高：默认 1.7，可调 1.4~2.0」。
pub const DEFAULT_LINE_SPACING_PERCENT: u32 = 170;

/// 默认左右页边距（像素）。
///
/// 来源：ENG-002 任务包。设计文档 §8.2 给的阅读页左右边距是 48px
/// （可调 24~64px），这里放宽到 72px：正文 46px 时 72px ≈ 1.56 em，
/// 在 1264px 宽的页面上左右各留 1.56 em 才不会让长行读起来串行；
/// 代价是每行少放约 1 个汉字（1120px / 46px ≈ 24 字）。
pub const DEFAULT_MARGIN_HORIZONTAL_PX: u32 = 72;

/// 默认上下页边距（像素）。
///
/// 来源：ENG-002 任务包。比左右边距略小（64px ≈ 1.39 em），因为墨阅的
/// 底部要留给状态栏（设计文档 §8.2：状态栏 56px），上下留白对称更好看。
pub const DEFAULT_MARGIN_VERTICAL_PX: u32 = 64;

/// 正文最小字号（像素）。
///
/// 300 DPI 下最小档 7.5pt ≈ 31px，212 DPI（PW2）下 7.5pt ≈ 22px，
/// 167 DPI（Basic 3 / KT3）下 7.5pt ≈ 17px。下限放宽到 16px，避免低 DPI
/// 机型的前几档字号全部被钳到同一像素值导致调字号无效。
pub const MIN_FONT_SIZE_PX: u32 = 16;

/// 正文最大字号（像素）。
///
/// 来源：`docs/design/README.md` §3「用户可调，范围 32–80 px」。
pub const MAX_FONT_SIZE_PX: u32 = 80;

/// 最小行高（百分比）。
///
/// 放开下限至 0%（0.0 倍行距），将行距数值完全交由用户自由调节；
/// crengine 原生允许 `line_h == 0` 且安全不崩溃。
pub const MIN_LINE_SPACING_PERCENT: u32 = 0;

/// 最大行高（百分比）。
pub const MAX_LINE_SPACING_PERCENT: u32 = 300;

/// 单边页边距上限（像素）。
///
/// crengine 自己也会把页边距钳到屏幕尺寸的 1/5（`LVDocView::updatePageMargins()`），
/// 1264x1680 的目标屏上就是 252px。这里取 256 作为同样的上限，
/// 免得把明显没意义的参数递给引擎。
pub const MAX_MARGIN_PX: u32 = 256;

/// 页面宽度或高度的上限（像素）。
///
/// Kindle 是 32 位 ARM，`size_t` 只有 32 位：页面尺寸的乘法会回绕，
/// 而底层的 `LVGrayDrawBuf` 直接按这个尺寸分配内存，超大尺寸就是一次
/// 越界写。8192 远超任何真实墨水屏（Kindle Oasis 2 是 1264x1680），
/// 超过它一定是调用方传错了参数。
pub const MAX_PAGE_DIMENSION_PX: u32 = 8192;

/// 默认面板 DPI（Kindle Oasis 2 / PW4/5/6 为 300 ppi）。
pub const DEFAULT_DPI: u32 = 300;

/// 允许的最小面板 DPI。
pub const MIN_DPI: u32 = 72;

/// 允许的最大面板 DPI。
pub const MAX_DPI: u32 = 600;

/// 正文字重的默认值：CSS 的 `font-weight: normal`。
pub const DEFAULT_FONT_WEIGHT: u16 = 400;

/// 允许的最小正文字重（CSS `font-weight: 100`）。
pub const MIN_FONT_WEIGHT: u16 = 100;

/// 允许的最大正文字重（CSS `font-weight: 900`）。
pub const MAX_FONT_WEIGHT: u16 = 900;

/// 「细」字重预设：300（CSS `light`）。
pub const FONT_WEIGHT_LIGHT: u16 = 300;

/// 「正常」字重预设：400（CSS `normal`）。
pub const FONT_WEIGHT_NORMAL: u16 = 400;

/// 「粗」字重预设：600（介于 CSS `semibold` 与 `bold` 之间，墨水屏上
/// 比 700 更耐看，不会把笔画糊成一团）。
pub const FONT_WEIGHT_BOLD: u16 = 600;

/// 页面宽度或高度的下限（像素）。
///
/// crengine 的 `LVDocView::Resize()` 会把尺寸钳到 80..=32767：比 80 小的
/// 页面会被静默放大，而引擎那边的页面尺寸和调用方以为的尺寸就对不上了
/// （渲染按 80 宽写，缓冲按调用方的宽度解读，整页错行）。所以下限在这里
/// 挡住，别让那种尺寸走到引擎里。
pub const MIN_PAGE_DIMENSION_PX: u32 = 80;

/// 校验页面尺寸（像素宽高）。
///
/// 宽高都不能是 0：0 宽的页面排不出内容。也不能超过
/// [`MAX_PAGE_DIMENSION_PX`]，理由见那里的说明。
///
/// 尺寸不合法时返回 [`Error::InvalidLayout`]，调用方在改动文档之前就该
/// 挡住，不能让引擎拿着半个改好的状态继续跑。
pub fn validate_page_size(size: (u32, u32)) -> Result<()> {
    let (width, height) = size;
    if width == 0 || height == 0 {
        return Err(Error::InvalidLayout(format!(
            "页面尺寸 {width}x{height} 非法：宽高都不能是 0"
        )));
    }
    if width < MIN_PAGE_DIMENSION_PX || height < MIN_PAGE_DIMENSION_PX {
        return Err(Error::InvalidLayout(format!(
            "页面尺寸 {width}x{height} 小于下限 {MIN_PAGE_DIMENSION_PX}px"
        )));
    }
    if width > MAX_PAGE_DIMENSION_PX || height > MAX_PAGE_DIMENSION_PX {
        return Err(Error::InvalidLayout(format!(
            "页面尺寸 {width}x{height} 超出上限 {MAX_PAGE_DIMENSION_PX}px"
        )));
    }
    Ok(())
}

/// 页面四边的页边距，单位是设备像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Margins {
    /// 左边距。
    pub left: u32,
    /// 右边距。
    pub right: u32,
    /// 上边距。
    pub top: u32,
    /// 下边距。
    pub bottom: u32,
}

impl Default for Margins {
    /// 左右 [`DEFAULT_MARGIN_HORIZONTAL_PX`]，上下 [`DEFAULT_MARGIN_VERTICAL_PX`]。
    fn default() -> Self {
        Self {
            left: DEFAULT_MARGIN_HORIZONTAL_PX,
            right: DEFAULT_MARGIN_HORIZONTAL_PX,
            top: DEFAULT_MARGIN_VERTICAL_PX,
            bottom: DEFAULT_MARGIN_VERTICAL_PX,
        }
    }
}

impl Margins {
    /// 左右边距是否相等。
    pub fn is_symmetric_horizontal(&self) -> bool {
        self.left == self.right
    }
}

/// 排版参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSettings {
    /// 正文字号，单位是设备像素。
    pub font_size_px: u32,
    /// 行高，百分比；100 表示单倍行距。
    pub line_spacing_percent: u32,
    /// 页边距。
    pub margins: Margins,
    /// 字体名（face 名，例如 `Songti SC`）。空串表示用引擎默认字体。
    pub font_face: String,
    /// 回退字体名（face 名）列表：主字体里没有的字形交给它们。
    ///
    /// 正文用 Literata、汉字交给中文字体就是这么配的：主字体只管它有的字形，
    /// 剩下的按这个列表往下找。空列表（默认）表示不用回退字体。
    ///
    /// 引擎里是进程级的设置（字体管理器是全局对象），所以换书会改掉全局那
    /// 一份；渲染前每本书会按自己的排版参数再设回来。
    pub fallback_font_faces: Vec<String>,
    /// 是否保留书内自带的样式（`<style>` 块、外链 CSS、行内 `style` 属性）。
    ///
    /// `true`（默认）= 尊重书的排版设计：书内样式在墨阅的基础样式之上层叠，
    /// 两者都生效。`false` = 只用墨阅的基础样式，忽略书里的 CSS。
    ///
    /// 对应 crengine 的 `PROP_EMBEDDED_STYLES`（ADR-010）。
    pub embedded_styles: bool,
    /// 精排模式下是否允许书内 `@font-face` 自带字体生效。
    ///
    /// `true`（默认）= 允许精排书籍使用内嵌字体；`false` = 保留精排样式
    /// （缩进、居中、图注等），但忽略书内嵌字体、统一使用阅读器选定的字体。
    ///
    /// 对应 crengine 的 `PROP_EMBEDDED_FONTS`。仅在 `fine_typesetting` 与
    /// `embedded_styles` 均开启时生效。
    pub embedded_fonts: bool,
    /// 是否启用 crengine 的现代精排特性。
    ///
    /// `true`（默认）：Web 盒模型与完整的 block rendering 标志、HarfBuzz
    /// shaping/kerning、悬挂标点、书内嵌字体、按面板 DPI 缩放 CSS 绝对单位。
    /// `false`：退回到 crengine 的传统行为（不启用增强 block rendering、不做
    /// 字距调整、不悬挂标点、不加载书内嵌字体、CSS 按 96 DPI 解释），适合
    /// 排查「精排把某本书排坏了」的兼容性问题。
    ///
    /// DOM 版本不随开关切换：两种状态都用 crengine 当前的
    /// `gDOMVersionCurrent`。切到旧 DOM 版本会改变 `DocFragment` 的节点结构，
    /// 用户来回切换精排时，已保存的 XPointer 阅读进度和高亮就会错位。
    ///
    /// 其余属性大多在 `LoadDocument` 之前生效：block rendering 标志、内嵌
    /// 字体在解析时就决定文档结构，事后再改只会触发重排，改不了已经解析出来
    /// 的 DOM（见 ENG-009）。
    pub fine_typesetting: bool,
    /// 正文字重，CSS 的 `font-weight` 数值，范围
    /// [`MIN_FONT_WEIGHT`]..=[`MAX_FONT_WEIGHT`]。
    ///
    /// 对应 crengine 的 `PROP_FONT_BASE_WEIGHT`：它在书里每个元素自己的
    /// 字重上叠加一个偏移（例如正文 400 的书把基准调到 600 会整体加粗），
    /// 而不是覆盖书里的粗体规则。字体没有对应字重时 crengine 会做合成
    /// 加粗/变细，墨水屏上仍能看出笔画粗细的差别。
    pub font_weight: u16,
    /// 目标屏幕 DPI（如 167 / 212 / 300 ppi），用于精排模式下按物理尺寸换算
    /// CSS 绝对单位（`pt` / `mm` / `in`）。范围 [`MIN_DPI`]..=[`MAX_DPI`]。
    pub dpi: u32,
    /// 用户自定义样式或额外 CSS 规则（注入在默认样式之后）。
    pub user_css: Option<String>,
}

impl Default for LayoutSettings {
    /// 设计文档 §3、§8.2 的阅读页默认值。
    fn default() -> Self {
        Self {
            font_size_px: DEFAULT_FONT_SIZE_PX,
            line_spacing_percent: DEFAULT_LINE_SPACING_PERCENT,
            margins: Margins::default(),
            font_face: String::new(),
            fallback_font_faces: Vec::new(),
            embedded_styles: true,
            embedded_fonts: true,
            fine_typesetting: true,
            font_weight: DEFAULT_FONT_WEIGHT,
            dpi: DEFAULT_DPI,
            user_css: None,
        }
    }
}

impl LayoutSettings {
    /// 校验参数是否落在允许范围内。
    ///
    /// 越界时返回 [`Error::InvalidLayout`]，并说明是哪个参数越界。
    pub fn validate(&self) -> Result<()> {
        if !(MIN_FONT_SIZE_PX..=MAX_FONT_SIZE_PX).contains(&self.font_size_px) {
            return Err(Error::InvalidLayout(format!(
                "字号 {}px 超出允许范围 {MIN_FONT_SIZE_PX}~{MAX_FONT_SIZE_PX}px",
                self.font_size_px
            )));
        }
        if !(MIN_LINE_SPACING_PERCENT..=MAX_LINE_SPACING_PERCENT)
            .contains(&self.line_spacing_percent)
        {
            return Err(Error::InvalidLayout(format!(
                "行高 {}% 超出允许范围 {MIN_LINE_SPACING_PERCENT}~{MAX_LINE_SPACING_PERCENT}%",
                self.line_spacing_percent
            )));
        }
        let sides = [
            ("左", self.margins.left),
            ("右", self.margins.right),
            ("上", self.margins.top),
            ("下", self.margins.bottom),
        ];
        for (name, value) in sides {
            if value > MAX_MARGIN_PX {
                return Err(Error::InvalidLayout(format!(
                    "{name}边距 {value}px 超出上限 {MAX_MARGIN_PX}px"
                )));
            }
        }
        if !(MIN_FONT_WEIGHT..=MAX_FONT_WEIGHT).contains(&self.font_weight) {
            return Err(Error::InvalidLayout(format!(
                "字重 {} 超出允许范围 {MIN_FONT_WEIGHT}~{MAX_FONT_WEIGHT}",
                self.font_weight
            )));
        }
        if !(MIN_DPI..=MAX_DPI).contains(&self.dpi) {
            return Err(Error::InvalidLayout(format!(
                "DPI {} 超出允许范围 {MIN_DPI}~{MAX_DPI}",
                self.dpi
            )));
        }
        // 回退字体名要拼成分号隔开的名单传给引擎，名字里不能有分号或 NUL。
        for face in &self.fallback_font_faces {
            if face.is_empty() || face.contains(';') || face.contains('\0') {
                return Err(Error::InvalidLayout(format!(
                    "回退字体名 {face:?} 不是合法的 face 名"
                )));
            }
        }
        if let Some(css) = &self.user_css {
            if css.contains('\0') {
                return Err(Error::InvalidLayout("用户样式包含 NUL 字节".to_string()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_the_design_document() {
        let layout = LayoutSettings::default();
        assert_eq!(layout.font_size_px, 46);
        assert_eq!(layout.line_spacing_percent, 170);
        assert_eq!(layout.margins.left, 72);
        assert_eq!(layout.margins.right, 72);
        assert_eq!(layout.margins.top, 64);
        assert_eq!(layout.margins.bottom, 64);
        // 空字体名 = 引擎默认字体。
        assert!(layout.font_face.is_empty());
        // 默认尊重书内样式（ADR-010）。
        assert!(layout.embedded_styles);
        // 默认开启现代精排与正常字重。
        assert!(layout.fine_typesetting);
        assert_eq!(layout.font_weight, 400);
    }

    #[test]
    fn font_weight_presets_match_the_documented_values() {
        assert_eq!(FONT_WEIGHT_LIGHT, 300);
        assert_eq!(FONT_WEIGHT_NORMAL, 400);
        assert_eq!(FONT_WEIGHT_BOLD, 600);
        assert_eq!(DEFAULT_FONT_WEIGHT, FONT_WEIGHT_NORMAL);
    }

    #[test]
    fn font_weight_bounds_are_validated() {
        // 99 与 901 越界，100/400/900 合法。
        for (weight, valid) in [
            (99, false),
            (100, true),
            (400, true),
            (900, true),
            (901, false),
        ] {
            let layout = LayoutSettings {
                font_weight: weight,
                ..LayoutSettings::default()
            };
            match layout.validate() {
                Ok(()) => assert!(valid, "字重 {weight} 应当被拒绝"),
                Err(Error::InvalidLayout(message)) => {
                    assert!(!valid, "字重 {weight} 应当合法");
                    assert!(message.contains("字重"), "错误信息应当说明字重：{message}");
                }
                Err(other) => panic!("字重 {weight} 返回了意外的错误：{other:?}"),
            }
        }
    }

    #[test]
    fn fine_typesetting_can_be_switched_off() {
        let layout = LayoutSettings {
            fine_typesetting: false,
            ..LayoutSettings::default()
        };
        // 关掉精排不影响参数合法性。
        assert!(layout.validate().is_ok());
    }

    #[test]
    fn default_margins_are_symmetric_horizontally() {
        assert!(Margins::default().is_symmetric_horizontal());
    }

    #[test]
    fn default_layout_is_valid() {
        assert!(LayoutSettings::default().validate().is_ok());
    }

    #[test]
    fn font_size_below_minimum_is_rejected() {
        let layout = LayoutSettings {
            font_size_px: MIN_FONT_SIZE_PX - 1,
            ..LayoutSettings::default()
        };
        let err = layout.validate().unwrap_err();
        assert!(matches!(err, Error::InvalidLayout(_)));
        assert!(err.to_string().contains("字号"));
    }

    #[test]
    fn font_size_above_maximum_is_rejected() {
        let layout = LayoutSettings {
            font_size_px: MAX_FONT_SIZE_PX + 1,
            ..LayoutSettings::default()
        };
        assert!(layout.validate().is_err());
    }

    #[test]
    fn font_size_bounds_are_accepted() {
        for size in [MIN_FONT_SIZE_PX, MAX_FONT_SIZE_PX] {
            let layout = LayoutSettings {
                font_size_px: size,
                ..LayoutSettings::default()
            };
            assert!(layout.validate().is_ok(), "{size}px 应当合法");
        }
    }

    #[test]
    fn line_spacing_out_of_range_is_rejected() {
        for spacing in [MIN_LINE_SPACING_PERCENT, 100, MAX_LINE_SPACING_PERCENT] {
            let layout = LayoutSettings {
                line_spacing_percent: spacing,
                ..LayoutSettings::default()
            };
            assert!(layout.validate().is_ok(), "{spacing}% 应当合法");
        }
        let layout = LayoutSettings {
            line_spacing_percent: MAX_LINE_SPACING_PERCENT + 1,
            ..LayoutSettings::default()
        };
        let err = layout.validate().unwrap_err();
        assert!(matches!(err, Error::InvalidLayout(_)));
        assert!(err.to_string().contains("行高"));
    }

    #[test]
    fn oversized_margin_is_rejected() {
        let layout = LayoutSettings {
            margins: Margins {
                left: MAX_MARGIN_PX + 1,
                ..Margins::default()
            },
            ..LayoutSettings::default()
        };
        let err = layout.validate().unwrap_err();
        assert!(err.to_string().contains("左边距"));
    }

    #[test]
    fn margin_at_the_limit_is_accepted() {
        let layout = LayoutSettings {
            margins: Margins {
                left: MAX_MARGIN_PX,
                right: MAX_MARGIN_PX,
                top: MAX_MARGIN_PX,
                bottom: MAX_MARGIN_PX,
            },
            ..LayoutSettings::default()
        };
        assert!(layout.validate().is_ok());
    }

    #[test]
    fn embedded_styles_can_be_switched_off() {
        let layout = LayoutSettings {
            embedded_styles: false,
            ..LayoutSettings::default()
        };
        assert!(!layout.embedded_styles);
        // 关掉书内样式不影响参数合法性。
        assert!(layout.validate().is_ok());
    }

    #[test]
    fn user_css_with_nul_is_rejected_and_valid_is_accepted() {
        let valid = LayoutSettings {
            user_css: Some("p { text-indent: 0em; }".to_string()),
            ..LayoutSettings::default()
        };
        assert!(valid.validate().is_ok());

        let invalid = LayoutSettings {
            user_css: Some("p { text-indent:\0 0em; }".to_string()),
            ..LayoutSettings::default()
        };
        let err = invalid.validate().unwrap_err();
        assert!(err.to_string().contains("NUL"));
    }
}
