//! ISO-8601 and Unix epoch millisecond conversions without heavy third-party crates.

/// Errors arising during ISO-8601 parsing or conversion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("invalid ISO-8601 format: {0}")]
    InvalidFormat(String),
    #[error("date component out of range: {0}")]
    OutOfRange(String),
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
///
/// Uses Howard Hinnant's algorithm for proleptic Gregorian calendar computation.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Convert (year, month, day) to days since Unix epoch (1970-01-01).
///
/// Uses Howard Hinnant's algorithm for proleptic Gregorian calendar computation.
fn ymd_to_days(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + (doe as i64) - 719468
}

/// Parse an ISO-8601 string into Unix epoch milliseconds.
///
/// Handles canonical formats such as `2026-11-05T12:00:00.000Z`, `2026-11-05T12:00:00Z`,
/// and offsets like `+08:00`, `-05:00`.
pub fn parse_iso8601(s: &str) -> Result<i64, TimeError> {
    let s = s.trim();
    if !s.is_ascii() {
        return Err(TimeError::InvalidFormat(format!(
            "non-ASCII characters in '{s}'"
        )));
    }
    if s.len() < 10 {
        return Err(TimeError::InvalidFormat(format!("string too short: '{s}'")));
    }

    // Expect YYYY-MM-DD
    let year_str = s
        .get(0..4)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid year in '{s}'")))?;
    let year: i32 = year_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid year in '{s}'")))?;
    let s_bytes = s.as_bytes();
    if s_bytes.get(4) != Some(&b'-') || s_bytes.get(7) != Some(&b'-') {
        return Err(TimeError::InvalidFormat(format!(
            "expected date separators '-' in '{s}'"
        )));
    }
    let month_str = s
        .get(5..7)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid month in '{s}'")))?;
    let month: u32 = month_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid month in '{s}'")))?;
    let day_str = s
        .get(8..10)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid day in '{s}'")))?;
    let day: u32 = day_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid day in '{s}'")))?;

    if !(1..=12).contains(&month) {
        return Err(TimeError::OutOfRange(format!(
            "month {month} out of range 1..=12"
        )));
    }
    if !(1..=31).contains(&day) {
        return Err(TimeError::OutOfRange(format!(
            "day {day} out of range 1..=31"
        )));
    }

    let mut remaining = s.get(10..).unwrap_or("");
    if remaining.is_empty() {
        let days = ymd_to_days(year, month, day);
        return Ok(days * 86_400_000);
    }

    if !remaining.starts_with('T') && !remaining.starts_with('t') && !remaining.starts_with(' ') {
        return Err(TimeError::InvalidFormat(format!(
            "expected 'T' separator, got '{remaining}'"
        )));
    }
    remaining = remaining.get(1..).unwrap_or("");

    if remaining.len() < 8 {
        return Err(TimeError::InvalidFormat(format!(
            "time part too short in '{s}'"
        )));
    }

    let hour_str = remaining
        .get(0..2)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid hour in '{s}'")))?;
    let hour: u32 = hour_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid hour in '{s}'")))?;
    let rem_bytes = remaining.as_bytes();
    if rem_bytes.get(2) != Some(&b':') || rem_bytes.get(5) != Some(&b':') {
        return Err(TimeError::InvalidFormat(format!(
            "expected time separators ':' in '{s}'"
        )));
    }
    let minute_str = remaining
        .get(3..5)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid minute in '{s}'")))?;
    let minute: u32 = minute_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid minute in '{s}'")))?;
    let second_str = remaining
        .get(6..8)
        .ok_or_else(|| TimeError::InvalidFormat(format!("invalid second in '{s}'")))?;
    let second: u32 = second_str
        .parse()
        .map_err(|_| TimeError::InvalidFormat(format!("invalid second in '{s}'")))?;

    if hour > 23 || minute > 59 || second > 59 {
        return Err(TimeError::OutOfRange(format!(
            "time {hour:02}:{minute:02}:{second:02} out of range"
        )));
    }

    remaining = remaining.get(8..).unwrap_or("");
    let mut millis: u32 = 0;
    if remaining.starts_with('.') || remaining.starts_with(',') {
        remaining = remaining.get(1..).unwrap_or("");
        let end_idx = remaining
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(remaining.len());
        let frac_str = remaining.get(..end_idx).unwrap_or("");
        remaining = remaining.get(end_idx..).unwrap_or("");

        if frac_str.is_empty() {
            return Err(TimeError::InvalidFormat(format!(
                "empty fractional seconds in '{s}'"
            )));
        }
        // Normalize fraction to 3-digit milliseconds
        let mut padded = frac_str.to_string();
        if padded.len() > 3 {
            padded.truncate(3);
        } else {
            while padded.len() < 3 {
                padded.push('0');
            }
        }
        millis = padded
            .parse()
            .map_err(|_| TimeError::InvalidFormat(format!("invalid fraction in '{s}'")))?;
    }

    // Timezone offset in minutes
    let mut tz_offset_mins: i64 = 0;
    let tz_str = remaining.trim();
    if !tz_str.is_empty() && tz_str != "Z" && tz_str != "z" {
        let sign = if tz_str.starts_with('+') {
            1
        } else if tz_str.starts_with('-') {
            -1
        } else {
            return Err(TimeError::InvalidFormat(format!(
                "unknown timezone format '{tz_str}'"
            )));
        };
        let tz_body = &tz_str[1..];
        if tz_body.contains(':') {
            let parts: Vec<&str> = tz_body.split(':').collect();
            if parts.len() != 2 {
                return Err(TimeError::InvalidFormat(format!(
                    "invalid timezone offset '{tz_str}'"
                )));
            }
            let h: i64 = parts[0]
                .parse()
                .map_err(|_| TimeError::InvalidFormat(format!("invalid tz hour '{tz_str}'")))?;
            let m: i64 = parts[1]
                .parse()
                .map_err(|_| TimeError::InvalidFormat(format!("invalid tz minute '{tz_str}'")))?;
            tz_offset_mins = sign * (h * 60 + m);
        } else if tz_body.len() == 2 {
            let h: i64 = tz_body
                .parse()
                .map_err(|_| TimeError::InvalidFormat(format!("invalid tz hour '{tz_str}'")))?;
            tz_offset_mins = sign * (h * 60);
        } else if tz_body.len() == 4 {
            let h: i64 = tz_body[0..2]
                .parse()
                .map_err(|_| TimeError::InvalidFormat(format!("invalid tz hour '{tz_str}'")))?;
            let m: i64 = tz_body[2..4]
                .parse()
                .map_err(|_| TimeError::InvalidFormat(format!("invalid tz minute '{tz_str}'")))?;
            tz_offset_mins = sign * (h * 60 + m);
        } else {
            return Err(TimeError::InvalidFormat(format!(
                "unrecognized timezone '{tz_str}'"
            )));
        }
    }

    let days = ymd_to_days(year, month, day);
    let day_ms = days * 86_400_000;
    let time_ms = (hour as i64 * 3600 + minute as i64 * 60 + second as i64) * 1000 + millis as i64;
    let utc_ms = day_ms + time_ms - (tz_offset_mins * 60_000);

    Ok(utc_ms)
}

/// Format Unix epoch milliseconds into canonical ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SS.sssZ`).
pub fn format_iso8601(ms: i64) -> String {
    let mut days = ms / 86_400_000;
    let mut rem_ms = ms % 86_400_000;
    if rem_ms < 0 {
        days -= 1;
        rem_ms += 86_400_000;
    }

    let (year, month, day) = days_to_ymd(days);
    let millis = (rem_ms % 1000) as u32;
    let total_seconds = (rem_ms / 1000) as u32;
    let second = total_seconds % 60;
    let minute = (total_seconds / 60) % 60;
    let hour = total_seconds / 3600;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_roundtrip() {
        let original = "2026-11-05T12:00:00.000Z";
        let ms = parse_iso8601(original).expect("valid parse");
        let formatted = format_iso8601(ms);
        assert_eq!(formatted, original);
    }

    #[test]
    fn parse_with_fractional_and_timezone() {
        let with_offset = "2026-11-05T20:00:00.123+08:00";
        let ms = parse_iso8601(with_offset).expect("valid parse");
        assert_eq!(format_iso8601(ms), "2026-11-05T12:00:00.123Z");
    }

    #[test]
    fn epoch_boundary() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00.000Z").unwrap(), 0);
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn leap_year_handling() {
        let leap_day = "2024-02-29T15:30:00.500Z";
        let ms = parse_iso8601(leap_day).unwrap();
        assert_eq!(format_iso8601(ms), leap_day);
    }

    #[test]
    fn non_ascii_or_malformed_input_returns_error_safely() {
        assert!(parse_iso8601("你好世界2026-11-05").is_err());
        assert!(parse_iso8601("🔥2026-11-05T00:00:00Z").is_err());
        assert!(parse_iso8601("short").is_err());
        assert!(parse_iso8601("").is_err());
    }
}
