// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader header and footer ("bars") configuration shared between the renderer,
//! the UI configuration sheet, and the app's persisted preferences.

/// High-level style presets for the reader's top and bottom bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReaderBarPreset {
    /// Both top bar and bottom bar with progress bar (matches the classic layout).
    #[default]
    Full,
    /// No top bar; compact single-row bottom bar with chapter, percentage and clock.
    Minimal,
    /// Neither top bar nor bottom bar; body text expands into the freed margins.
    Immersive,
    /// User-configured header/footer visibility and per-slot items.
    Custom,
}

impl ReaderBarPreset {
    /// All presets in UI display order.
    pub const ALL: [ReaderBarPreset; 4] = [
        ReaderBarPreset::Full,
        ReaderBarPreset::Minimal,
        ReaderBarPreset::Immersive,
        ReaderBarPreset::Custom,
    ];

    /// Turn a picker index into a preset, defaulting to [`ReaderBarPreset::Full`].
    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Minimal,
            2 => Self::Immersive,
            3 => Self::Custom,
            _ => Self::Full,
        }
    }

    /// The picker index for this preset.
    pub const fn index(self) -> usize {
        match self {
            Self::Full => 0,
            Self::Minimal => 1,
            Self::Immersive => 2,
            Self::Custom => 3,
        }
    }

    /// Stable string key for JSON persistence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Minimal => "minimal",
            Self::Immersive => "immersive",
            Self::Custom => "custom",
        }
    }

    /// Parse from a persisted string key, falling back to [`ReaderBarPreset::Full`].
    pub fn parse(s: &str) -> Self {
        match s {
            "minimal" => Self::Minimal,
            "immersive" => Self::Immersive,
            "custom" => Self::Custom,
            _ => Self::Full,
        }
    }
}

/// Information item that can be placed in any left, center, or right slot of
/// the reader's top bar or bottom bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BarItem {
    /// Empty slot.
    #[default]
    None,
    /// Book author and title (running head).
    BookTitle,
    /// Current chapter title.
    ChapterTitle,
    /// Wall-clock time (respects the 12/24-hour preference).
    Clock,
    /// Battery icon, percentage and charging state.
    Battery,
    /// Wi-Fi connection / signal / airplane mode badge.
    Wifi,
    /// Frontlight status and brightness step (plus warmth when supported).
    Frontlight,
    /// Whole-book reading progress percentage, e.g. `43%`.
    ProgressPercent,
    /// Whole-book page position, e.g. `128 / 1046`.
    BookPages,
    /// Current chapter page position, e.g. `5 / 18`.
    ChapterPages,
    /// Estimated reading time remaining in the current chapter.
    ChapterTimeLeft,
    /// Estimated reading time remaining in the whole book.
    BookTimeLeft,
    /// Compact device status cluster: Wi-Fi + frontlight + battery + clock.
    DeviceStatusGroup,
}

impl BarItem {
    /// All selectable bar items in picker order.
    pub const ALL: [BarItem; 13] = [
        BarItem::None,
        BarItem::BookTitle,
        BarItem::ChapterTitle,
        BarItem::Clock,
        BarItem::Battery,
        BarItem::Wifi,
        BarItem::Frontlight,
        BarItem::ProgressPercent,
        BarItem::BookPages,
        BarItem::ChapterPages,
        BarItem::ChapterTimeLeft,
        BarItem::BookTimeLeft,
        BarItem::DeviceStatusGroup,
    ];

    /// Turn a picker index into a bar item, defaulting to [`BarItem::None`].
    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::BookTitle,
            2 => Self::ChapterTitle,
            3 => Self::Clock,
            4 => Self::Battery,
            5 => Self::Wifi,
            6 => Self::Frontlight,
            7 => Self::ProgressPercent,
            8 => Self::BookPages,
            9 => Self::ChapterPages,
            10 => Self::ChapterTimeLeft,
            11 => Self::BookTimeLeft,
            12 => Self::DeviceStatusGroup,
            _ => Self::None,
        }
    }

    /// The picker index for this bar item.
    pub const fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::BookTitle => 1,
            Self::ChapterTitle => 2,
            Self::Clock => 3,
            Self::Battery => 4,
            Self::Wifi => 5,
            Self::Frontlight => 6,
            Self::ProgressPercent => 7,
            Self::BookPages => 8,
            Self::ChapterPages => 9,
            Self::ChapterTimeLeft => 10,
            Self::BookTimeLeft => 11,
            Self::DeviceStatusGroup => 12,
        }
    }

    /// Advance to the next item in [`BarItem::ALL`], wrapping around.
    pub const fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }

    /// Stable string key for JSON persistence.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BookTitle => "book_title",
            Self::ChapterTitle => "chapter_title",
            Self::Clock => "clock",
            Self::Battery => "battery",
            Self::Wifi => "wifi",
            Self::Frontlight => "frontlight",
            Self::ProgressPercent => "progress_percent",
            Self::BookPages => "book_pages",
            Self::ChapterPages => "chapter_pages",
            Self::ChapterTimeLeft => "chapter_time_left",
            Self::BookTimeLeft => "book_time_left",
            Self::DeviceStatusGroup => "device_status_group",
        }
    }

    /// Parse from a persisted string key, falling back to [`BarItem::None`].
    pub fn parse(s: &str) -> Self {
        match s {
            "book_title" => Self::BookTitle,
            "chapter_title" => Self::ChapterTitle,
            "clock" => Self::Clock,
            "battery" => Self::Battery,
            "wifi" => Self::Wifi,
            "frontlight" => Self::Frontlight,
            "progress_percent" => Self::ProgressPercent,
            "book_pages" => Self::BookPages,
            "chapter_pages" => Self::ChapterPages,
            "chapter_time_left" => Self::ChapterTimeLeft,
            "book_time_left" => Self::BookTimeLeft,
            "device_status_group" => Self::DeviceStatusGroup,
            _ => Self::None,
        }
    }
}

/// Live Wi-Fi status rendered in the reader bars and global status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WifiBadge {
    /// Wi-Fi status unknown or radio powered off.
    #[default]
    Off,
    /// Radio enabled but not associated with an access point.
    Disconnected,
    /// Connected to a network; `signal` is 0..=4 bars.
    Connected { signal: u8 },
    /// Airplane mode enabled.
    Airplane,
}

/// Complete configuration for the reader's top and bottom bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ReaderBarConfig {
    pub preset: ReaderBarPreset,
    pub show_header: bool,
    pub show_footer: bool,
    /// Top bar slots: `[left, center, right]`.
    pub header_slots: [BarItem; 3],
    /// Bottom bar slots: `[left, center, right]`.
    pub footer_slots: [BarItem; 3],
    /// Whether to draw the fine progress bar in the bottom band.
    pub show_progress_bar: bool,
    /// Whether to draw thin horizontal rules separating the bars from body text.
    pub show_dividers: bool,
    /// Whether to draw the top-right bookmark ribbon outline when unbookmarked.
    pub show_bookmark_ribbon: bool,
    /// Compact bar height (reduces top/bottom band height to give more room to text).
    pub compact: bool,
}

/// Reference (300 DPI) top margin when the header is shown in standard mode.
pub const HEADER_TOP_STANDARD_REF: u32 = 104;
/// Reference (300 DPI) top margin when the header is shown in compact mode.
pub const HEADER_TOP_COMPACT_REF: u32 = 72;
/// Reference (300 DPI) top margin when the header is hidden (immersive).
pub const HEADER_TOP_HIDDEN_REF: u32 = 32;

/// Reference (300 DPI) bottom margin when the footer + progress bar are shown.
pub const FOOTER_BOTTOM_STANDARD_REF: u32 = 100;
/// Reference (300 DPI) bottom margin when the footer is compact / single-row.
pub const FOOTER_BOTTOM_COMPACT_REF: u32 = 68;
/// Reference (300 DPI) bottom margin when the footer is hidden (immersive).
pub const FOOTER_BOTTOM_HIDDEN_REF: u32 = 32;

impl Default for ReaderBarConfig {
    fn default() -> Self {
        Self::for_preset(ReaderBarPreset::Full)
    }
}

impl ReaderBarConfig {
    /// Canonical configuration for a given preset.
    pub const fn for_preset(preset: ReaderBarPreset) -> Self {
        match preset {
            ReaderBarPreset::Full => Self {
                preset: ReaderBarPreset::Full,
                show_header: true,
                show_footer: true,
                header_slots: [
                    BarItem::BookTitle,
                    BarItem::None,
                    BarItem::DeviceStatusGroup,
                ],
                footer_slots: [
                    BarItem::ChapterTitle,
                    BarItem::None,
                    BarItem::ChapterTimeLeft,
                ],
                show_progress_bar: true,
                show_dividers: false,
                show_bookmark_ribbon: true,
                compact: false,
            },
            ReaderBarPreset::Minimal => Self {
                preset: ReaderBarPreset::Minimal,
                show_header: false,
                show_footer: true,
                header_slots: [BarItem::None, BarItem::None, BarItem::None],
                footer_slots: [
                    BarItem::ChapterTitle,
                    BarItem::ProgressPercent,
                    BarItem::Clock,
                ],
                show_progress_bar: false,
                show_dividers: false,
                show_bookmark_ribbon: true,
                compact: true,
            },
            ReaderBarPreset::Immersive => Self {
                preset: ReaderBarPreset::Immersive,
                show_header: false,
                show_footer: false,
                header_slots: [BarItem::None, BarItem::None, BarItem::None],
                footer_slots: [BarItem::None, BarItem::None, BarItem::None],
                show_progress_bar: false,
                show_dividers: false,
                show_bookmark_ribbon: false,
                compact: true,
            },
            ReaderBarPreset::Custom => Self {
                preset: ReaderBarPreset::Custom,
                show_header: true,
                show_footer: true,
                header_slots: [
                    BarItem::BookTitle,
                    BarItem::None,
                    BarItem::DeviceStatusGroup,
                ],
                footer_slots: [
                    BarItem::ChapterTitle,
                    BarItem::BookTimeLeft,
                    BarItem::ProgressPercent,
                ],
                show_progress_bar: true,
                show_dividers: false,
                show_bookmark_ribbon: true,
                compact: false,
            },
        }
    }

    /// Apply a preset, updating visibility and slots unless `Custom` is selected
    /// (which keeps the current detailed settings and marks preset as `Custom`).
    pub fn apply_preset(&mut self, preset: ReaderBarPreset) {
        if preset == ReaderBarPreset::Custom {
            self.preset = ReaderBarPreset::Custom;
        } else {
            *self = Self::for_preset(preset);
        }
    }

    /// Effective top band height in 300 DPI reference pixels.
    pub const fn text_top_ref_px(self) -> u32 {
        if !self.show_header {
            HEADER_TOP_HIDDEN_REF
        } else if self.compact {
            HEADER_TOP_COMPACT_REF
        } else {
            HEADER_TOP_STANDARD_REF
        }
    }

    /// Effective bottom band height in 300 DPI reference pixels.
    pub const fn text_bottom_ref_px(self) -> u32 {
        if !self.show_footer {
            FOOTER_BOTTOM_HIDDEN_REF
        } else if self.compact && !self.show_progress_bar {
            FOOTER_BOTTOM_COMPACT_REF
        } else {
            FOOTER_BOTTOM_STANDARD_REF
        }
    }

    /// Whether changing from `self` to `other` alters the body text rectangle
    /// (and therefore requires re-paginating the book rather than just redrawing bars).
    pub const fn changes_body_geometry(self, other: Self) -> bool {
        self.text_top_ref_px() != other.text_top_ref_px()
            || self.text_bottom_ref_px() != other.text_bottom_ref_px()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_round_trips_index_and_str() {
        for (i, p) in ReaderBarPreset::ALL.iter().copied().enumerate() {
            assert_eq!(p.index(), i);
            assert_eq!(ReaderBarPreset::from_index(i), p);
            assert_eq!(ReaderBarPreset::parse(p.as_str()), p);
        }
        assert_eq!(ReaderBarPreset::from_index(99), ReaderBarPreset::Full);
        assert_eq!(ReaderBarPreset::parse("unknown"), ReaderBarPreset::Full);
    }

    #[test]
    fn bar_item_round_trips_index_str_and_next() {
        for (i, item) in BarItem::ALL.iter().copied().enumerate() {
            assert_eq!(item.index(), i);
            assert_eq!(BarItem::from_index(i), item);
            assert_eq!(BarItem::parse(item.as_str()), item);
            let next = item.next();
            assert_eq!(next, BarItem::ALL[(i + 1) % BarItem::ALL.len()]);
        }
        assert_eq!(BarItem::from_index(99), BarItem::None);
        assert_eq!(BarItem::parse("unknown"), BarItem::None);
    }

    #[test]
    fn geometry_change_detection_matches_band_heights() {
        let full = ReaderBarConfig::for_preset(ReaderBarPreset::Full);
        let mut tweaked = full;
        tweaked.header_slots[2] = BarItem::Battery;
        tweaked.show_dividers = true;
        assert!(
            !full.changes_body_geometry(tweaked),
            "changing slot items or dividers must not require relayout"
        );

        let minimal = ReaderBarConfig::for_preset(ReaderBarPreset::Minimal);
        let immersive = ReaderBarConfig::for_preset(ReaderBarPreset::Immersive);
        assert!(full.changes_body_geometry(minimal));
        assert!(full.changes_body_geometry(immersive));
        assert!(minimal.changes_body_geometry(immersive));
        assert_eq!(full.text_top_ref_px(), HEADER_TOP_STANDARD_REF);
        assert_eq!(full.text_bottom_ref_px(), FOOTER_BOTTOM_STANDARD_REF);
        assert_eq!(immersive.text_top_ref_px(), HEADER_TOP_HIDDEN_REF);
        assert_eq!(immersive.text_bottom_ref_px(), FOOTER_BOTTOM_HIDDEN_REF);
    }
}
