// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Chinese lunar calendar and the 24 solar terms.
//!
//! The calendar itself is `tyme4rs`'s: this module only says which of its
//! numbers the clock card shows and how they read. Chinese keeps the names
//! the library gives; the other languages say the same thing in their own
//! words, so the card does not turn into a line of hanzi on an English
//! screen.
//!
//! The names of the heavenly stems and the earthly branches, and the names
//! of the solar terms, are the same in every language, so they are tables
//! here rather than a calendar of our own.

use tyme4rs::tyme::solar::SolarDay;
use tyme4rs::tyme::{Culture, Tyme};

/// The heavenly stems, in the order of the cycle.
const STEMS: [&str; 10] = [
    "Jia", "Yi", "Bing", "Ding", "Wu", "Ji", "Geng", "Xin", "Ren", "Gui",
];
/// The earthly branches, in the order of the cycle.
const BRANCHES: [&str; 12] = [
    "Zi", "Chou", "Yin", "Mao", "Chen", "Si", "Wu", "Wei", "Shen", "You", "Xu", "Hai",
];
/// The 24 solar terms, in the order `tyme4rs` numbers them: 冬至 is the
/// first, the winter solstice that opens the year.
const TERMS: [&str; 24] = [
    "Winter Solstice",
    "Minor Cold",
    "Major Cold",
    "Beginning of Spring",
    "Rain Water",
    "Awakening of Insects",
    "Spring Equinox",
    "Pure Brightness",
    "Grain Rain",
    "Beginning of Summer",
    "Grain Buds",
    "Grain in Ear",
    "Summer Solstice",
    "Minor Heat",
    "Major Heat",
    "Beginning of Autumn",
    "End of Heat",
    "White Dew",
    "Autumn Equinox",
    "Cold Dew",
    "Frost's Descent",
    "Beginning of Winter",
    "Minor Snow",
    "Major Snow",
];

/// The lunar calendar of a day, and the solar term near it.
pub struct Lunar {
    /// The sexagenary year in Chinese: "丙午".
    pub ganzhi: String,
    /// The year in the sexagenary cycle: 0 is 甲子.
    pub ganzhi_index: usize,
    /// The lunar month, 1 to 12.
    pub month: usize,
    /// The name of the lunar month in Chinese: "九月".
    pub month_name: String,
    /// The lunar day, 1 to 30.
    pub day: usize,
    /// The name of the lunar day in Chinese: "廿六".
    pub day_name: String,
    /// The solar term near the day, when one is within a week of it.
    pub term: Option<Term>,
}

/// A solar term and the days between it and the day.
pub struct Term {
    /// The name of the term in Chinese: "立冬".
    pub name: String,
    /// The number of the term in the year, 0 is 冬至.
    pub index: usize,
    /// The days to the term: 0 on the term's own day.
    pub in_days: u32,
}

impl Lunar {
    /// The calendar of the solar day `year-month-day`.
    pub fn of(year: i32, month: u8, day: u8) -> Self {
        let solar = SolarDay::from_ymd(year as isize, month as usize, day as usize);
        let lunar_day = solar.get_lunar_day();
        let lunar_month = lunar_day.get_lunar_month();
        let lunar_year = lunar_month.get_lunar_year();
        let cycle = lunar_year.get_sixty_cycle();
        // The cycle starts at 甲子, the year 4 of the calendar.
        let ganzhi_index = (lunar_year.get_year() - 4).rem_euclid(60) as usize;

        // The term the day falls in, and the one that comes next.
        let current = solar.get_term();
        let target = if solar.subtract(current.get_solar_day()) == 0 {
            current
        } else {
            current.next(1)
        };
        let in_days = target.get_solar_day().subtract(solar).max(0) as u32;
        let term = if in_days <= 7 {
            Some(Term {
                name: target.get_name(),
                index: target.get_index(),
                in_days,
            })
        } else {
            None
        };

        Lunar {
            ganzhi: cycle.get_name(),
            ganzhi_index,
            month: lunar_month.get_month(),
            month_name: lunar_month.get_name(),
            day: lunar_day.get_day(),
            day_name: lunar_day.get_name(),
            term,
        }
    }

    /// The line the clock card shows in Chinese: "丙午年 九月廿六".
    pub fn zh(&self) -> String {
        format!("{}年 {}{}", self.ganzhi, self.month_name, self.day_name)
    }

    /// The same line in English: "9th lunar month, 26th day".
    ///
    /// The sexagenary year is left out: "Bingwu" is a name English
    /// readers cannot use, and the card has no room for it.
    pub fn en(&self) -> String {
        format!(
            "{} lunar month, {} day",
            ordinal(self.month),
            ordinal(self.day)
        )
    }

    /// The month and the day without the year: "九月廿六". The one-line
    /// clock shows it next to the date.
    pub fn short_zh(&self) -> String {
        format!("{}{}", self.month_name, self.day_name)
    }

    /// The same in English: "9th month, 26th day".
    pub fn short_en(&self) -> String {
        format!("{} month, {} day", ordinal(self.month), ordinal(self.day))
    }
    /// The sexagenary year in English: "Bingwu". The two names are read
    /// as one word, as the pinyin of the year has it.
    pub fn ganzhi_en(&self) -> String {
        let stem = STEMS[self.ganzhi_index % 10];
        let branch = BRANCHES[self.ganzhi_index % 12].to_lowercase();
        format!("{stem}{branch}")
    }
}

impl Term {
    /// The chip the clock card shows in Chinese: "立冬前三日", "今日立冬".
    pub fn zh(&self) -> String {
        if self.in_days == 0 {
            format!("今日{}", self.name)
        } else {
            format!("{}前{}日", self.name, DAY_NAMES[self.in_days as usize])
        }
    }

    /// The same chip in English: "3 days to Beginning of Winter".
    pub fn en(&self) -> String {
        let name = TERMS[self.index % TERMS.len()];
        if self.in_days == 0 {
            format!("{name} today")
        } else if self.in_days == 1 {
            format!("1 day to {name}")
        } else {
            format!("{} days to {name}", self.in_days)
        }
    }
}

/// The Chinese names of the days of a month, 1 to 7.
const DAY_NAMES: [&str; 8] = ["", "一", "二", "三", "四", "五", "六", "七"];

/// "1st", "2nd", "3rd", "21st".
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_day() {
        let l = Lunar::of(2026, 11, 4);
        assert_eq!(l.zh(), "丙午年 九月廿六");
        let term = l.term.expect("立冬 is three days away");
        assert_eq!(term.name, "立冬");
        assert_eq!(term.in_days, 3);
        assert_eq!(term.zh(), "立冬前三日");
    }

    #[test]
    fn the_term_day_itself() {
        let l = Lunar::of(2026, 11, 7);
        let term = l.term.as_ref().expect("立冬 is today");
        assert_eq!(term.in_days, 0);
        assert_eq!(term.zh(), "今日立冬");
        assert_eq!(l.zh(), "丙午年 九月廿九");
    }

    #[test]
    fn a_term_out_of_the_week_shows_nothing() {
        assert!(Lunar::of(2026, 11, 10).term.is_none());
    }

    #[test]
    fn the_seven_days_before_a_term() {
        let term = Lunar::of(2026, 11, 1).term.expect("立冬 is six days away");
        assert_eq!(term.in_days, 6);
        assert_eq!(term.zh(), "立冬前六日");
    }

    #[test]
    fn english_says_the_same_thing() {
        let l = Lunar::of(2026, 11, 4);
        assert_eq!(l.ganzhi_en(), "Bingwu");
        assert_eq!(l.en(), "9th lunar month, 26th day");
        assert_eq!(l.short_zh(), "九月廿六");
        assert_eq!(l.short_en(), "9th month, 26th day");
        let term = l.term.expect("立冬 is three days away");
        assert_eq!(term.en(), "3 days to Beginning of Winter");
        assert_eq!(
            Lunar::of(2026, 11, 7).term.expect("today").en(),
            "Beginning of Winter today"
        );
    }

    #[test]
    fn the_day_names_come_from_the_library() {
        let l = Lunar::of(2026, 11, 4);
        assert_eq!(l.month_name, "九月");
        assert_eq!(l.day_name, "廿六");
        let l = Lunar::of(2026, 11, 7);
        assert_eq!(l.day_name, "廿九");
    }
}
