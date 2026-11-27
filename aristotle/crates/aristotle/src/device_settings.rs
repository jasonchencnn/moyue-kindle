// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device settings helpers: hardware capability detection, HTTP `Date` clock
//! synchronization, safe cache cleanup, and localized labels.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aristotle_hal::platform::{detect_kobo, Platform};
use aristotle_i18n::{tr, Msg};
use aristotle_screen::Orientation;

/// Minimum interval between automatic background network time syncs once a
/// sync has succeeded (10 minutes). Manual syncs bypass this cooldown.
const AUTO_SYNC_COOLDOWN: Duration = Duration::from_secs(600);

/// Asynchronous network time synchronization controller.
///
/// Runs HTTP `Date` lookups on a background thread so neither automatic
/// post-connect syncs nor manual sync taps ever block the UI event loop.
#[derive(Default)]
pub struct TimeSyncState {
    in_flight: Option<(bool, mpsc::Receiver<Option<i64>>)>,
    last_synced_at: Option<Instant>,
    scheduled_at: Option<Instant>,
}

impl TimeSyncState {
    /// Whether a network time sync request is currently running in the background.
    pub fn is_syncing(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Schedule an automatic background time sync after `delay` (unless a sync
    /// is already running or one succeeded within [`AUTO_SYNC_COOLDOWN`]).
    pub fn schedule_auto(&mut self, delay: Duration) {
        if self.in_flight.is_some() {
            return;
        }
        let now = Instant::now();
        if self
            .last_synced_at
            .is_some_and(|t| now.saturating_duration_since(t) < AUTO_SYNC_COOLDOWN)
        {
            return;
        }
        let target = now + delay;
        self.scheduled_at = Some(match self.scheduled_at {
            Some(prev) => prev.min(target),
            None => target,
        });
    }

    /// Start a user-initiated network time sync immediately (bypasses cooldown).
    pub fn start_manual(&mut self, wake: fn()) {
        if self.in_flight.is_some() {
            return;
        }
        self.scheduled_at = None;
        self.spawn(true, wake);
    }

    /// Next deadline the event loop should wake up for a scheduled auto-sync.
    pub fn deadline(&self) -> Option<Instant> {
        self.scheduled_at
    }

    /// Poll for a completed sync or launch a due scheduled auto-sync.
    /// Returns `Some((is_manual, result))` when a background fetch finishes.
    pub fn poll(&mut self, now: Instant, wake: fn()) -> Option<(bool, Option<i64>)> {
        if let Some((is_manual, rx)) = &self.in_flight {
            match rx.try_recv() {
                Ok(res) => {
                    let manual = *is_manual;
                    self.in_flight = None;
                    if res.is_some() {
                        self.last_synced_at = Some(now);
                    }
                    return Some((manual, res));
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let manual = *is_manual;
                    self.in_flight = None;
                    return Some((manual, None));
                }
            }
        }
        if self.scheduled_at.is_some_and(|at| now >= at) {
            self.scheduled_at = None;
            if self
                .last_synced_at
                .is_none_or(|t| now.saturating_duration_since(t) >= AUTO_SYNC_COOLDOWN)
            {
                self.spawn(false, wake);
            }
        }
        None
    }

    fn spawn(&mut self, is_manual: bool, wake: fn()) {
        let (tx, rx) = mpsc::channel();
        self.in_flight = Some((is_manual, rx));
        let _ = std::thread::Builder::new()
            .name("time-sync".into())
            .spawn(move || {
                let res = if !is_manual {
                    let st = aristotle_hal::wifi::status();
                    if st.airplane_mode || !st.enabled {
                        None
                    } else {
                        fetch_network_unix_time()
                    }
                } else {
                    fetch_network_unix_time()
                };
                let _ = tx.send(res);
                wake();
            });
    }
}

/// Hardware capabilities probed once at startup to show/hide device-specific
/// settings cleanly (keys, warm light, ALS, accelerometer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceCaps {
    pub has_page_keys: bool,
    pub has_warm_light: bool,
    pub has_als: bool,
    pub has_accelerometer: bool,
}

impl DeviceCaps {
    /// Detect hardware capabilities for the current Kindle or Kobo model.
    pub fn detect() -> Self {
        if let Ok(model) = Platform::model() {
            return Self {
                has_page_keys: model.page_turn_keys().is_some(),
                has_warm_light: model.has_warm_light(),
                has_als: model.has_als(),
                has_accelerometer: model.has_accelerometer(),
            };
        }
        if let Ok(Some(kobo)) = detect_kobo() {
            let m = kobo.model;
            return Self {
                has_page_keys: m.page_turn_keys().is_some(),
                has_warm_light: m.has_warm_light(),
                has_als: m.has_als(),
                has_accelerometer: m.has_accelerometer(),
            };
        }
        Self::default()
    }
}

/// Localized label for an [`Orientation`] value.
pub fn orientation_label(o: Orientation) -> &'static str {
    match o {
        Orientation::Portrait => tr(Msg::DeviceSettingsHwOrientPortrait),
        Orientation::PortraitInverted => tr(Msg::DeviceSettingsHwOrientInverted),
        Orientation::LandscapeLeft => tr(Msg::DeviceSettingsHwOrientLandLeft),
        Orientation::LandscapeRight => tr(Msg::DeviceSettingsHwOrientLandRight),
        Orientation::Auto => tr(Msg::DeviceSettingsHwOrientAuto),
    }
}

/// Safely clear regenerable caches (`crengine-cache`, `epub-cache`,
/// `covers-cache`) inside `data_dir`. Books, progress, notes, and preferences
/// are never touched. Returns total bytes reclaimed.
pub fn clear_caches(data_dir: &Path) -> u64 {
    let mut freed = 0u64;
    for folder in ["crengine-cache", "epub-cache", "covers-cache"] {
        let dir = data_dir.join(folder);
        if dir.is_dir() {
            freed = freed.saturating_add(dir_size(&dir));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    freed
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            } else if meta.is_dir() {
                total = total.saturating_add(dir_size(&p));
            }
        }
    }
    total
}

/// Parse an RFC 7231 / HTTP `Date` header (e.g. `"Tue, 24 Nov 2026 02:55:10 GMT"`)
/// into a UNIX timestamp (seconds since 1970-01-01 00:00:00 UTC).
pub fn parse_http_date_unix(header: &str) -> Option<i64> {
    let parts: Vec<&str> = header.split_whitespace().collect();
    // Expected: ["Tue,", "24", "Nov", "2026", "02:55:10", "GMT"]
    if parts.len() < 5 {
        return None;
    }
    let day: i64 = parts[1].parse().ok()?;
    let month: i64 = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[3].parse().ok()?;
    let mut hms = parts[4].split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnant's civil-from-days inverse: days since 1970-01-01.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Minimum plausible UNIX timestamp returned by a network time source
/// (2023-11-14T22:13:20Z), rejecting captive portals or misconfigured proxies.
const MIN_VALID_UNIX_SECS: i64 = 1_700_000_000;

/// Days in a Gregorian calendar month (`1..=12`).
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Shift `unix_secs` by `delta_months` in the current local calendar, clamping
/// the day to the target month's length (e.g. Jan 31 + 1 month -> Feb 28/29)
/// and preserving the local time-of-day.
pub fn shift_local_months(unix_secs: i64, delta_months: i32) -> i64 {
    let Some(lt) = aristotle_hal::system::local_time_at(unix_secs) else {
        return unix_secs.saturating_add(i64::from(delta_months) * 30 * 86_400);
    };
    let cur_days = days_from_civil(i64::from(lt.year), i64::from(lt.month), i64::from(lt.day));
    let total_months = i64::from(lt.year) * 12 + i64::from(lt.month - 1) + i64::from(delta_months);
    let new_year = total_months.div_euclid(12).clamp(1970, 2099);
    let new_month = total_months.rem_euclid(12) + 1;
    let new_day = i64::from(lt.day).min(days_in_month(new_year, new_month));
    let new_days = days_from_civil(new_year, new_month, new_day);
    unix_secs.saturating_add((new_days - cur_days) * 86_400)
}

/// Fetch UTC UNIX time by issuing a lightweight HTTP HEAD request and reading
/// the `Date` response header (avoids UDP 123 firewall blocks).
pub fn fetch_network_unix_time() -> Option<i64> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    const HOSTS: &[&str] = &[
        "www.baidu.com:80",
        "connect.rom.miui.com:80",
        "connectivitycheck.gstatic.com:80",
        "captive.apple.com:80",
    ];

    for host in HOSTS {
        let Ok(mut addrs) = host.to_socket_addrs() else {
            continue;
        };
        let Some(addr) = addrs.next() else {
            continue;
        };
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let host_hdr = host.split(':').next().unwrap_or(host);
        let req = format!("HEAD / HTTP/1.1\r\nHost: {host_hdr}\r\nConnection: close\r\n\r\n");
        if stream.write_all(req.as_bytes()).is_err() {
            continue;
        }
        let mut buf = [0u8; 2048];
        let Ok(n) = stream.read(&mut buf) else {
            continue;
        };
        let text = String::from_utf8_lossy(&buf[..n]);
        for line in text.lines() {
            if let Some(val) = line
                .strip_prefix("Date:")
                .or_else(|| line.strip_prefix("date:"))
            {
                if let Some(ts) = parse_http_date_unix(val.trim()) {
                    if ts >= MIN_VALID_UNIX_SECS {
                        return Some(ts);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc7231_http_date_accurately() {
        assert_eq!(
            parse_http_date_unix("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(0)
        );
        assert_eq!(
            parse_http_date_unix("Tue, 24 Nov 2026 02:55:10 GMT"),
            Some(1_795_402_510)
        );
        assert_eq!(parse_http_date_unix("invalid"), None);
    }

    #[test]
    fn clear_caches_removes_only_cache_directories() {
        let dir = std::env::temp_dir().join(format!("aristotle-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crengine-cache")).unwrap();
        std::fs::write(dir.join("crengine-cache/a.bin"), vec![0u8; 1024]).unwrap();
        std::fs::write(dir.join("reader.json"), b"{}").unwrap();

        let freed = clear_caches(&dir);
        assert!(freed >= 1024);
        assert!(!dir.join("crengine-cache").exists());
        assert!(dir.join("reader.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shift_local_months_clamps_month_end_and_leap_years() {
        // 2024-01-31 12:00:00 UTC -> +1 month -> 2024-02-29 12:00:00 UTC (leap year)
        let jan31_2024 = days_from_civil(2024, 1, 31) * 86_400 + 12 * 3600;
        let feb_2024 = shift_local_months(jan31_2024, 1);
        assert_eq!(feb_2024 - jan31_2024, 29 * 86_400);

        // 2025-01-31 12:00:00 UTC -> +1 month -> 2025-02-28 12:00:00 UTC (non-leap)
        let jan31_2025 = days_from_civil(2025, 1, 31) * 86_400 + 12 * 3600;
        let feb_2025 = shift_local_months(jan31_2025, 1);
        assert_eq!(feb_2025 - jan31_2025, 28 * 86_400);

        // +12 months from 2024-02-29 -> 2025-02-28
        let leap_day = days_from_civil(2024, 2, 29) * 86_400 + 12 * 3600;
        let next_year = shift_local_months(leap_day, 12);
        assert_eq!(next_year - leap_day, 365 * 86_400);
    }
}
