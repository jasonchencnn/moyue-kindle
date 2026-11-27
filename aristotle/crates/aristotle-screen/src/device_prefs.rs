// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared device-level setting enums and timezone table used across HAL, UI,
//! and the main reader app.

/// Action triggered by a long press on a hardware page-turn button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeyLongPressAction {
    /// Repeatedly turn pages while held (or turn multiple pages).
    #[default]
    RepeatTurn,
    /// Jump to the previous or next chapter.
    ChapterJump,
    /// Toggle the frontlight on/off.
    ToggleLight,
    /// Trigger a full e-ink screen refresh (GC16).
    FullRefresh,
    /// Ignore long presses (short press only).
    Disabled,
}

impl KeyLongPressAction {
    pub const ALL: [Self; 5] = [
        Self::RepeatTurn,
        Self::ChapterJump,
        Self::ToggleLight,
        Self::FullRefresh,
        Self::Disabled,
    ];

    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::ChapterJump,
            2 => Self::ToggleLight,
            3 => Self::FullRefresh,
            4 => Self::Disabled,
            _ => Self::RepeatTurn,
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::RepeatTurn => 0,
            Self::ChapterJump => 1,
            Self::ToggleLight => 2,
            Self::FullRefresh => 3,
            Self::Disabled => 4,
        }
    }

    pub const fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }
}

/// Touch tap-zone layout in the reader (middle band between top/bottom menu bands).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TouchZoneLayout {
    /// Left 30% previous page, right 70% next page (classic thumb layout).
    #[default]
    Default30_70,
    /// Left 50% previous page, right 50% next page.
    Split50_50,
    /// Entire middle band advances to the next page (one-handed mode).
    NextOnly,
}

impl TouchZoneLayout {
    pub const ALL: [Self; 3] = [Self::Default30_70, Self::Split50_50, Self::NextOnly];

    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Split50_50,
            2 => Self::NextOnly,
            _ => Self::Default30_70,
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Default30_70 => 0,
            Self::Split50_50 => 1,
            Self::NextOnly => 2,
        }
    }

    pub const fn next(self) -> Self {
        Self::from_index((self.index() + 1) % Self::ALL.len())
    }
}

/// Screen orientation preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Orientation {
    /// Standard upright portrait.
    #[default]
    Portrait,
    /// Upside-down portrait (180°).
    PortraitInverted,
    /// Landscape rotated 90° counter-clockwise.
    LandscapeLeft,
    /// Landscape rotated 90° clockwise.
    LandscapeRight,
    /// Follow the device accelerometer (only on hardware with gravity sensor).
    Auto,
}

impl Orientation {
    pub const ALL: [Self; 5] = [
        Self::Portrait,
        Self::PortraitInverted,
        Self::LandscapeLeft,
        Self::LandscapeRight,
        Self::Auto,
    ];

    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::PortraitInverted,
            2 => Self::LandscapeLeft,
            3 => Self::LandscapeRight,
            4 => Self::Auto,
            _ => Self::Portrait,
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Portrait => 0,
            Self::PortraitInverted => 1,
            Self::LandscapeLeft => 2,
            Self::LandscapeRight => 3,
            Self::Auto => 4,
        }
    }

    /// Cycle through available orientations; skips [`Orientation::Auto`] when
    /// the hardware lacks an accelerometer.
    pub const fn next(self, has_auto: bool) -> Self {
        let len = if has_auto { 5 } else { 4 };
        Self::from_index((self.index() + 1) % len)
    }

    /// Target Linux framebuffer `rotate` value (`0..=3`) when not `Auto`.
    pub const fn fb_rotate(self) -> Option<u32> {
        match self {
            Self::Portrait => Some(0),
            Self::LandscapeRight => Some(1),
            Self::PortraitInverted => Some(2),
            Self::LandscapeLeft => Some(3),
            Self::Auto => None,
        }
    }
}

/// One selectable timezone entry with both its IANA name and POSIX `TZ` string
/// (musl reads POSIX `TZ` reliably without requiring `/usr/share/zoneinfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimezoneOption {
    pub id: &'static str,
    pub posix_tz: &'static str,
    pub label_zh: &'static str,
    pub label_en: &'static str,
}

/// Common world timezones, with East Asia first for Chinese Kindle users.
pub const TIMEZONES: &[TimezoneOption] = &[
    TimezoneOption {
        id: "Asia/Shanghai",
        posix_tz: "CST-8",
        label_zh: "北京 / 上海 (UTC+8)",
        label_en: "Beijing / Shanghai (UTC+8)",
    },
    TimezoneOption {
        id: "Asia/Hong_Kong",
        posix_tz: "HKT-8",
        label_zh: "香港 (UTC+8)",
        label_en: "Hong Kong (UTC+8)",
    },
    TimezoneOption {
        id: "Asia/Taipei",
        posix_tz: "CST-8",
        label_zh: "台北 (UTC+8)",
        label_en: "Taipei (UTC+8)",
    },
    TimezoneOption {
        id: "Asia/Singapore",
        posix_tz: "<+08>-8",
        label_zh: "新加坡 (UTC+8)",
        label_en: "Singapore (UTC+8)",
    },
    TimezoneOption {
        id: "Asia/Tokyo",
        posix_tz: "JST-9",
        label_zh: "东京 (UTC+9)",
        label_en: "Tokyo (UTC+9)",
    },
    TimezoneOption {
        id: "Asia/Seoul",
        posix_tz: "KST-9",
        label_zh: "首尔 (UTC+9)",
        label_en: "Seoul (UTC+9)",
    },
    TimezoneOption {
        id: "Asia/Bangkok",
        posix_tz: "<+07>-7",
        label_zh: "曼谷 (UTC+7)",
        label_en: "Bangkok (UTC+7)",
    },
    TimezoneOption {
        id: "Asia/Kolkata",
        posix_tz: "IST-5:30",
        label_zh: "新德里 (UTC+5:30)",
        label_en: "New Delhi (UTC+5:30)",
    },
    TimezoneOption {
        id: "Asia/Dubai",
        posix_tz: "<+04>-4",
        label_zh: "迪拜 (UTC+4)",
        label_en: "Dubai (UTC+4)",
    },
    TimezoneOption {
        id: "UTC",
        posix_tz: "UTC0",
        label_zh: "协调世界时 (UTC)",
        label_en: "UTC (UTC+0)",
    },
    TimezoneOption {
        id: "Europe/London",
        posix_tz: "GMT0BST,M3.5.0/1,M10.5.0",
        label_zh: "伦敦 (UTC+0/+1)",
        label_en: "London (UTC+0/+1)",
    },
    TimezoneOption {
        id: "Europe/Paris",
        posix_tz: "CET-1CEST,M3.5.0,M10.5.0/3",
        label_zh: "巴黎 / 柏林 (UTC+1/+2)",
        label_en: "Paris / Berlin (UTC+1/+2)",
    },
    TimezoneOption {
        id: "Europe/Warsaw",
        posix_tz: "CET-1CEST,M3.5.0,M10.5.0/3",
        label_zh: "华沙 (UTC+1/+2)",
        label_en: "Warsaw (UTC+1/+2)",
    },
    TimezoneOption {
        id: "Europe/Moscow",
        posix_tz: "MSK-3",
        label_zh: "莫斯科 (UTC+3)",
        label_en: "Moscow (UTC+3)",
    },
    TimezoneOption {
        id: "America/New_York",
        posix_tz: "EST5EDT,M3.2.0,M11.1.0",
        label_zh: "纽约 (UTC-5/-4)",
        label_en: "New York (UTC-5/-4)",
    },
    TimezoneOption {
        id: "America/Chicago",
        posix_tz: "CST6CDT,M3.2.0,M11.1.0",
        label_zh: "芝加哥 (UTC-6/-5)",
        label_en: "Chicago (UTC-6/-5)",
    },
    TimezoneOption {
        id: "America/Los_Angeles",
        posix_tz: "PST8PDT,M3.2.0,M11.1.0",
        label_zh: "洛杉矶 (UTC-8/-7)",
        label_en: "Los Angeles (UTC-8/-7)",
    },
    TimezoneOption {
        id: "Australia/Sydney",
        posix_tz: "AEST-10AEDT,M10.1.0,M4.1.0/3",
        label_zh: "悉尼 (UTC+10/+11)",
        label_en: "Sydney (UTC+10/+11)",
    },
    TimezoneOption {
        id: "Pacific/Auckland",
        posix_tz: "NZST-12NZDT,M9.5.0,M4.1.0/3",
        label_zh: "奥克兰 (UTC+12/+13)",
        label_en: "Auckland (UTC+12/+13)",
    },
];

/// Look up a timezone entry by its IANA `id`.
pub fn find_timezone(id: &str) -> Option<&'static TimezoneOption> {
    TIMEZONES.iter().find(|tz| tz.id == id)
}

/// Format hour and minute in either 24-hour (`21:05`) or 12-hour (`9:05 PM`) form.
pub fn format_clock_hm(hour: u32, minute: u32, clock_24h: bool) -> String {
    let h = hour % 24;
    let m = minute % 60;
    if clock_24h {
        format!("{h:02}:{m:02}")
    } else {
        let (suffix, h12) = match h {
            0 => ("AM", 12),
            1..=11 => ("AM", h),
            12 => ("PM", 12),
            _ => ("PM", h - 12),
        };
        format!("{h12}:{m:02} {suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formatting_supports_12h_and_24h() {
        assert_eq!(format_clock_hm(0, 5, true), "00:05");
        assert_eq!(format_clock_hm(0, 5, false), "12:05 AM");
        assert_eq!(format_clock_hm(9, 30, false), "9:30 AM");
        assert_eq!(format_clock_hm(12, 0, false), "12:00 PM");
        assert_eq!(format_clock_hm(21, 38, true), "21:38");
        assert_eq!(format_clock_hm(21, 38, false), "9:38 PM");
    }

    #[test]
    fn orientation_next_skips_auto_without_accelerometer() {
        assert_eq!(
            Orientation::LandscapeRight.next(false),
            Orientation::Portrait
        );
        assert_eq!(Orientation::LandscapeRight.next(true), Orientation::Auto);
        assert_eq!(Orientation::Auto.next(true), Orientation::Portrait);
    }

    #[test]
    fn timezone_lookup_finds_shanghai_and_utc() {
        assert_eq!(find_timezone("Asia/Shanghai").unwrap().posix_tz, "CST-8");
        assert_eq!(find_timezone("UTC").unwrap().posix_tz, "UTC0");
        assert!(find_timezone("Mars/Olympus").is_none());
    }
}
