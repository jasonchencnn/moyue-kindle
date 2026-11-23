// SPDX-License-Identifier: AGPL-3.0-or-later
//! Page geometry and typesetting defaults for the Kindle Paperwhite 4.

/// The header and footer font, the UI's Literata, and its directory in the
/// data directory (`<dir>/<family>-{Regular,Italic}.ttf`).
pub const CHROME_FAMILY: &str = "Literata";
pub const CHROME_DIR: &str = "serif";

/// The gray of the guide drawn under every line of body text (0 black,
/// 255 white).
///
/// 0x20 is nearly black: on a 16-level e-ink panel a 0x60 guide washes out to
/// a faint smudge, while 0x20 keeps the same crisp edge as the text. Night
/// mode inverts the whole page, so the guide becomes 0xDF there: light
/// enough not to glare, dark enough to stay visible.
pub const UNDERLINE_GRAY: u8 = 0x20;

/// The font weight presets the reading panel offers, as CSS `font-weight`
/// numbers.
///
/// The engine's base weight shifts every element's weight by
/// `weight - 400`, so a book's own bold headings stay bold on top of it.
pub const FONT_WEIGHT_LIGHT: u16 = 300;
pub const FONT_WEIGHT_NORMAL: u16 = 400;
pub const FONT_WEIGHT_BOLD: u16 = 600;

/// The guide drawn under every line of body text.
///
/// It is a reading guide (like the ruled lines of a schoolbook), not the
/// underline of an annotation: every line of body text gets one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Underline {
    /// No guide.
    #[default]
    Off,
    /// One solid horizontal line.
    Solid,
    /// A dotted horizontal line.
    Dotted,
}

impl Underline {
    /// Every style, in the order the picker shows them.
    pub const ALL: [Underline; 3] = [Underline::Off, Underline::Solid, Underline::Dotted];

    /// Turn a picker index into a style. Anything out of range means "off".
    pub fn from_index(index: usize) -> Underline {
        match index {
            1 => Underline::Solid,
            2 => Underline::Dotted,
            _ => Underline::Off,
        }
    }

    /// The picker's index for this style.
    pub fn index(self) -> usize {
        match self {
            Underline::Off => 0,
            Underline::Solid => 1,
            Underline::Dotted => 2,
        }
    }
}

/// How wide the guide under each line of body text is.
///
/// It is a render option, not a typographic one: changing it needs no new
/// layout, only a new render of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnderlineScope {
    /// Under the text only: the line follows each line's own width.
    #[default]
    TextOnly,
    /// Across the whole text block (`margin_left..width-margin_right`).
    ///
    /// The gaps after a short paragraph and the empty line slots below it are
    /// filled on the line-height grid, so the page reads as ruled paper.
    FullLine,
}

impl UnderlineScope {
    /// Every scope, in the order the picker shows them.
    pub const ALL: [UnderlineScope; 2] = [UnderlineScope::TextOnly, UnderlineScope::FullLine];

    /// Turn a picker index into a scope. Anything out of range means
    /// "text only".
    pub fn from_index(index: usize) -> UnderlineScope {
        match index {
            1 => UnderlineScope::FullLine,
            _ => UnderlineScope::TextOnly,
        }
    }

    /// The picker's index for this scope.
    pub fn index(self) -> usize {
        match self {
            UnderlineScope::TextOnly => 0,
            UnderlineScope::FullLine => 1,
        }
    }
}

/// The direction the text runs in.
///
/// [`WritingMode::Horizontal`] is the layout of every engine Aristotle had
/// before VERT-101: lines run left to right, the page turns forwards to the
/// right. [`WritingMode::VerticalRtl`] is the classical Chinese and Japanese
/// layout: characters run top to bottom, columns run right to left, and the
/// book turns forwards to the left (see `crate::vertical`).
///
/// The mode decides which engine lays the book out: crengine-ng has no
/// `writing-mode: vertical-rl`, so the vertical mode goes to the pure-Rust
/// engine in [`crate::vertical`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WritingMode {
    /// Left to right, top to bottom (the default, and every Latin book).
    #[default]
    Horizontal,
    /// Top to bottom, right to left (classical Chinese, Japanese, Korean).
    VerticalRtl,
}

impl WritingMode {
    /// Every mode, in the order the picker shows them.
    pub const ALL: [WritingMode; 2] = [WritingMode::Horizontal, WritingMode::VerticalRtl];

    /// Turn a picker index into a mode. Anything out of range means
    /// horizontal, so an old configuration keeps reading as it did.
    pub fn from_index(index: usize) -> WritingMode {
        match index {
            1 => WritingMode::VerticalRtl,
            _ => WritingMode::Horizontal,
        }
    }

    /// The picker's index for this mode.
    pub fn index(self) -> usize {
        match self {
            WritingMode::Horizontal => 0,
            WritingMode::VerticalRtl => 1,
        }
    }

    /// Whether the mode lays text out top to bottom, right to left.
    pub fn is_vertical(self) -> bool {
        matches!(self, WritingMode::VerticalRtl)
    }
}

/// Everything that decides how a page looks. [`Style::pw4`] holds the
/// defaults; the CLI can override the typographic ones.
///
/// Two styles that compare equal lay the book out the same way: the reader
/// uses that to tell a new typography from the one on the panel (see
/// [`crate::restyle`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Visible screen width in pixels (1072 on the PW4).
    pub width: u32,
    /// Visible screen height in pixels (1448 on the PW4).
    pub height: u32,
    /// Bytes per row of the page buffer. 1088 on the PW4, matching the
    /// framebuffer's line length, so a page can be copied with one memcpy
    /// per row and dumped files read like framebuffer dumps.
    pub stride: usize,
    /// Panel resolution; Plato converts points and millimetres with it.
    pub dpi: u16,
    /// Font family for body text, looked up by name in [`Style::font_dir`].
    pub font_family: String,
    /// Directory (relative to the data directory, or absolute) holding the
    /// body font. Kept apart from Plato's `fonts/` because Plato opens every
    /// file in the search path to compare family names, which costs time
    /// per book. The header and footer stay in [`CHROME_FAMILY`] whatever
    /// the body font.
    pub font_dir: String,
    /// CJK (Chinese) font family for body text, passed to crengine as fallback face.
    pub cjk_font_family: String,
    /// Body font size in points.
    pub font_size: f32,
    /// Line height in ems.
    pub line_height: f32,
    /// Margin around the text block in millimetres. Plato only takes whole
    /// millimetres, uniform on all sides of the area it lays out.
    pub margin_mm: i32,
    /// Distance in pixels from the top of the screen to the top of the
    /// text block; the header lives above it.
    pub text_top: u32,
    /// Distance in pixels from the bottom of the screen to the bottom of
    /// the text block; the footer lives below it.
    pub text_bottom: u32,
    /// Header and footer font size in points.
    pub chrome_size: f32,
    /// Gray level for header and footer text. Dark enough (below the
    /// threshold of [`crate::page::BW_THRESHOLD`]) to stay visible in the
    /// black and white A2 frame of a page turn.
    pub chrome_gray: u8,
    /// Hyphenation language for EPUB text, overriding the book's
    /// declaration (see [`crate::lang`]). `None`: decide per book.
    pub language: Option<String>,
    /// CJK first-line indent in characters / ems: 2 (default) or 0.
    pub cjk_indent: usize,
    /// CJK paragraph gap in ems: 0.0 (无), 0.3 (小), or 0.6 (大, default).
    pub cjk_gap: f32,
    /// Whether crengine's modern typesetting is on: W3C box model, the
    /// current DOM version, HarfBuzz shaping/kerning, hanging punctuation,
    /// embedded fonts and CSS absolute units scaled for the panel DPI.
    ///
    /// Off is the compatibility escape hatch for a book the modern layout
    /// breaks; it also needs a new layout, as most of it is decided while
    /// parsing.
    pub fine_typesetting: bool,
    /// Body text weight as a CSS `font-weight` number (100..=900).
    ///
    /// The engine treats it as a base weight: every element's own weight is
    /// shifted by `font_weight - 400`, so heading boldness survives.
    pub font_weight: u16,
    /// The direction the text runs in: horizontal (left to right) or the
    /// classical vertical right-to-left layout.
    ///
    /// It selects the engine as well as the layout: the vertical mode is laid
    /// out by the pure-Rust engine in [`crate::vertical`], not by crengine-ng.
    /// A style that differs only in this field needs a new layout.
    pub writing_mode: WritingMode,
}

impl Style {
    pub fn with_screen(mut self, screen: aristotle_screen::Screen, stride: usize) -> Self {
        assert!(
            stride >= screen.width as usize,
            "stride must cover the screen"
        );
        let ratio = screen.density.dpi() as f32 / self.dpi as f32;
        self.text_top = (self.text_top as f32 * ratio).round() as u32;
        self.text_bottom = (self.text_bottom as f32 * ratio).round() as u32;
        self.width = screen.width;
        self.height = screen.height;
        self.stride = stride;
        self.dpi = screen.density.dpi();
        self
    }

    /// Defaults for the PW4, chosen for Polish prose:
    ///
    /// - Literata: designed for e-reading, sturdy serifs and a large x-height
    ///   that survive e-ink's 16 gray levels, full Latin Extended-A
    ///   (ą ć ę ł ń ó ś ź ż) and Polish quotation marks, OFL licensed.
    /// - 10 pt at 300 DPI: Literata's large x-height reads like 11 pt of a
    ///   classical face such as Plato's default Libertinus.
    /// - 1.3 em leading and 5 mm side margins: about 60 characters per line
    ///   on the 91 mm wide panel, a comfortable measure for justified text
    ///   once Polish hyphenation removes the wide gaps.
    pub fn pw4() -> Style {
        Style {
            width: 1072,
            height: 1448,
            stride: 1088,
            dpi: 300,
            font_family: "Literata".to_string(),
            font_dir: "serif".to_string(),
            cjk_font_family: "Noto Sans SC".to_string(),
            font_size: 10.0,
            line_height: 1.3,
            margin_mm: 5,
            text_top: 104,
            text_bottom: 100,
            chrome_size: 7.0,
            chrome_gray: 0x40,
            language: None,
            cjk_indent: 2,
            cjk_gap: 0.6,
            fine_typesetting: true,
            font_weight: FONT_WEIGHT_NORMAL,
            writing_mode: WritingMode::Horizontal,
        }
    }

    /// The margin in pixels, rounded the way Plato rounds it.
    pub fn margin_px(&self) -> i32 {
        (self.margin_mm as f32 * self.dpi as f32 / 25.4).round() as i32
    }

    /// The area handed to Plato for layout, as (top, height) in pixels.
    ///
    /// Plato puts the same margin on all four sides, but the page wants a
    /// header and a footer in the top and bottom margins. So the layout
    /// area spans the full width and is shifted vertically so that the
    /// text block lands exactly at `text_top` and `text_bottom`; the header
    /// and footer are drawn afterwards over Plato's (white) top and bottom
    /// margins.
    pub fn body(&self) -> (i32, u32) {
        let m = self.margin_px();
        let top = self.text_top as i32 - m;
        let bottom = self.height as i32 - self.text_bottom as i32 + m;
        (top, (bottom - top) as u32)
    }

    /// Point size in FreeType's 26.6 fixed point, as `Font::set_size` wants.
    pub fn chrome_size_26_6(&self) -> u32 {
        (self.chrome_size * 64.0).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pw4_defaults_enable_fine_typesetting_and_normal_weight() {
        let style = Style::pw4();
        assert!(style.fine_typesetting);
        assert_eq!(style.font_weight, FONT_WEIGHT_NORMAL);
        assert_eq!(style.font_weight, 400);
    }

    #[test]
    fn font_weight_presets_are_light_normal_and_bold() {
        assert_eq!(FONT_WEIGHT_LIGHT, 300);
        assert_eq!(FONT_WEIGHT_NORMAL, 400);
        assert_eq!(FONT_WEIGHT_BOLD, 600);
    }

    #[test]
    fn underline_gray_is_darker_than_the_old_value() {
        assert_eq!(UNDERLINE_GRAY, 0x20);
    }

    #[test]
    fn writing_mode_defaults_to_horizontal_and_round_trips() {
        assert_eq!(Style::pw4().writing_mode, WritingMode::Horizontal);
        assert_eq!(WritingMode::default(), WritingMode::Horizontal);
        assert_eq!(
            WritingMode::ALL,
            [WritingMode::Horizontal, WritingMode::VerticalRtl]
        );
        for (index, mode) in [(0, WritingMode::Horizontal), (1, WritingMode::VerticalRtl)] {
            assert_eq!(WritingMode::from_index(index), mode);
            assert_eq!(mode.index(), index);
        }
        // An unknown index (an older or newer configuration) reads
        // horizontally rather than failing.
        assert_eq!(WritingMode::from_index(7), WritingMode::Horizontal);
        assert!(!WritingMode::Horizontal.is_vertical());
        assert!(WritingMode::VerticalRtl.is_vertical());
    }

    #[test]
    fn underline_scope_round_trips_through_picker_indices() {
        for (index, scope) in [(0, UnderlineScope::TextOnly), (1, UnderlineScope::FullLine)] {
            assert_eq!(UnderlineScope::from_index(index), scope);
            assert_eq!(scope.index(), index);
        }
        assert_eq!(
            UnderlineScope::ALL,
            [UnderlineScope::TextOnly, UnderlineScope::FullLine]
        );
        // Out of range falls back to the compatible default.
        assert_eq!(UnderlineScope::from_index(7), UnderlineScope::TextOnly);
        assert_eq!(UnderlineScope::default(), UnderlineScope::TextOnly);
    }
}
