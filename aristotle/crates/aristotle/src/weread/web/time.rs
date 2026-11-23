// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local UTC offset for the heartbeat's `timeZone` field (contract §14.5).

use aristotle_hal::system;

/// The local UTC offset as `+08:00`, without a timezone database.
pub(super) fn local_offset(now: i64) -> String {
    let Some(local) = system::local_time() else {
        return "+00:00".to_string();
    };
    let seconds = days_from_civil(local.year, local.month, local.day) * 86_400
        + i64::from(local.hour) * 3_600
        + i64::from(local.minute) * 60
        + i64::from(local.second);
    format_utc_offset(now, seconds)
}

/// Format `local_seconds - now` as a signed `±HH:MM` UTC offset.
pub(super) fn format_utc_offset(now: i64, local_seconds: i64) -> String {
    let offset = local_seconds - now;
    // Round to the nearest minute so a 1-second skew between `now` and
    // `local_time()` never produces `+07:59` instead of `+08:00`.
    let rounded_mins = if offset >= 0 {
        (offset + 30) / 60
    } else {
        -(((-offset) + 30) / 60)
    };
    let sign = if rounded_mins < 0 { '-' } else { '+' };
    let mins = rounded_mins.abs();
    format!("{sign}{:02}:{:02}", mins / 60, mins % 60)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant).
pub(super) fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_utc_offset_handles_positive_negative_zero_and_half_hour_zones() {
        let base = 1_700_000_000i64;
        assert_eq!(format_utc_offset(base, base + 8 * 3_600), "+08:00");
        assert_eq!(format_utc_offset(base, base - 5 * 3_600), "-05:00");
        assert_eq!(format_utc_offset(base, base), "+00:00");
        assert_eq!(
            format_utc_offset(base, base + 5 * 3_600 + 30 * 60),
            "+05:30"
        );
        assert_eq!(
            format_utc_offset(base, base - 3 * 3_600 - 30 * 60),
            "-03:30"
        );
        // Minor clock skew between reading UTC and local time still rounds cleanly.
        assert_eq!(format_utc_offset(base, base + 8 * 3_600 - 1), "+08:00");
        assert_eq!(format_utc_offset(base, base - 1), "+00:00");
    }
}
