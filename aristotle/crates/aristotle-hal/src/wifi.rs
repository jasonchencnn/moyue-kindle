// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wi-Fi radio and network management for Kindle (`lipc` + `wpa_cli`) and Kobo
//! (`wpa_cli`).
//!
//! All external commands are invoked with standard stdin/stderr detached so
//! they never block the UI loop when called from a worker thread, and fall back
//! cleanly when run off-device (e.g. in host tests or desktop previews).

use std::collections::HashMap;
use std::io;
use std::process::{Command, Stdio};

use aristotle_screen::WifiBadge;

/// A scanned or saved wireless access point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub bssid: String,
    /// Signal level in dBm (e.g. `-55`).
    pub signal_dbm: i32,
    /// Normalized 0..=4 bar strength.
    pub signal_bars: u8,
    /// Whether the network requires a WPA/WEP passphrase.
    pub secured: bool,
    /// Currently associated network.
    pub connected: bool,
    /// Present in `wpa_supplicant` saved networks.
    pub saved: bool,
}

/// Snapshot of the device's wireless radio and association state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiStatus {
    /// True when airplane mode is on (Kindle `wirelessEnable == 0`).
    pub airplane_mode: bool,
    /// True when the wireless radio is enabled.
    pub enabled: bool,
    /// Associated SSID, if connected.
    pub connected_ssid: Option<String>,
    /// Assigned IPv4 address, if connected.
    pub ip: Option<String>,
    /// Normalized signal bars `0..=4`.
    pub signal_bars: u8,
}

impl WifiStatus {
    /// Convert into the compact [`WifiBadge`] shown in status bars and reader chrome.
    pub fn badge(&self) -> WifiBadge {
        if self.airplane_mode {
            WifiBadge::Airplane
        } else if !self.enabled {
            WifiBadge::Off
        } else if self.connected_ssid.is_some() {
            WifiBadge::Connected {
                signal: self.signal_bars.min(4),
            }
        } else {
            WifiBadge::Disconnected
        }
    }
}

/// Query current Wi-Fi and airplane-mode status.
pub fn status() -> WifiStatus {
    let wireless_enable = crate::powerd::get_prop("com.lab126.cmd", "wirelessEnable", true)
        .ok()
        .and_then(|v| v.trim().parse::<i32>().ok());

    let (airplane_mode, enabled) = match wireless_enable {
        Some(0) => (true, false),
        Some(_) => (false, true),
        None => (false, true),
    };

    if airplane_mode {
        return WifiStatus {
            airplane_mode: true,
            enabled: false,
            connected_ssid: None,
            ip: None,
            signal_bars: 0,
        };
    }

    let wpa_out = run_wpa(&["status"]).unwrap_or_default();
    let mut st = parse_wpa_status(&wpa_out);
    st.airplane_mode = false;
    st.enabled = enabled;
    if st.connected_ssid.is_some() && st.signal_bars == 0 {
        if let Ok(sig) = run_wpa(&["signal_poll"]) {
            if let Some(dbm) = parse_signal_poll_rssi(&sig) {
                st.signal_bars = dbm_to_bars(dbm);
            } else {
                st.signal_bars = 3;
            }
        } else {
            st.signal_bars = 3;
        }
    }
    st
}

/// Enable or disable the wireless radio (`on = false` enables airplane mode on Kindle).
pub fn set_wireless_enabled(on: bool) -> io::Result<()> {
    let val = if on { "1" } else { "0" };
    if crate::powerd::set_prop("com.lab126.cmd", "wirelessEnable", val, true).is_ok() {
        return Ok(());
    }
    // Fallback for Kobo or non-lipc Linux environments.
    let status = Command::new("ifconfig")
        .args(["wlan0", if on { "up" } else { "down" }])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("ifconfig wlan0: {status}")))
    }
}

/// Trigger a fresh Wi-Fi scan and return deduplicated, sorted networks.
pub fn scan() -> io::Result<Vec<WifiNetwork>> {
    let _ = run_wpa(&["scan"]);
    // Give the radio a brief moment to populate scan results without blocking long.
    std::thread::sleep(std::time::Duration::from_millis(600));
    let raw = run_wpa(&["scan_results"])?;
    let cur = status();
    let saved_out = run_wpa(&["list_networks"]).unwrap_or_default();
    let saved = parse_saved_ssids(&saved_out);
    Ok(parse_scan_results(
        &raw,
        cur.connected_ssid.as_deref(),
        &saved,
    ))
}

/// Connect to an SSID (with optional passphrase for secured networks).
pub fn connect(ssid: &str, password: Option<&str>) -> io::Result<()> {
    if ssid.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty ssid"));
    }
    // Check if already saved; if so reuse its network id.
    let saved_out = run_wpa(&["list_networks"]).unwrap_or_default();
    let net_id = find_saved_network_id(&saved_out, ssid)
        .map(Ok)
        .unwrap_or_else(|| {
            let out = run_wpa(&["add_network"])?;
            out.lines()
                .last()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                .map(String::from)
                .ok_or_else(|| io::Error::other(format!("wpa_cli add_network failed: {out}")))
        })?;

    let quoted_ssid = format!("\"{}\"", escape_wpa_str(ssid));
    run_wpa(&["set_network", &net_id, "ssid", &quoted_ssid])?;
    match password {
        Some(pw) if !pw.is_empty() => {
            let quoted_pw = format!("\"{}\"", escape_wpa_str(pw));
            run_wpa(&["set_network", &net_id, "psk", &quoted_pw])?;
        }
        _ => {
            run_wpa(&["set_network", &net_id, "key_mgmt", "NONE"])?;
        }
    }
    run_wpa(&["enable_network", &net_id])?;
    run_wpa(&["select_network", &net_id])?;
    let _ = run_wpa(&["save_config"]);
    Ok(())
}

/// Disconnect from the currently associated access point.
pub fn disconnect() -> io::Result<()> {
    let _ = run_wpa(&["disconnect"])?;
    Ok(())
}

/// Remove a saved network profile by SSID.
pub fn forget(ssid: &str) -> io::Result<()> {
    let saved_out = run_wpa(&["list_networks"])?;
    if let Some(id) = find_saved_network_id(&saved_out, ssid) {
        let _ = run_wpa(&["remove_network", &id])?;
        let _ = run_wpa(&["save_config"]);
    }
    Ok(())
}

fn run_wpa(args: &[&str]) -> io::Result<String> {
    let out = Command::new("wpa_cli")
        .arg("-i")
        .arg("wlan0")
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(io::Error::other(format!(
            "wpa_cli {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn escape_wpa_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Map RSSI dBm to `0..=4` signal bars.
pub fn dbm_to_bars(dbm: i32) -> u8 {
    match dbm {
        d if d >= -55 => 4,
        d if d >= -67 => 3,
        d if d >= -78 => 2,
        d if d >= -88 => 1,
        _ => 0,
    }
}

/// Parse `wpa_cli status` output into a [`WifiStatus`].
pub fn parse_wpa_status(raw: &str) -> WifiStatus {
    let mut state_completed = false;
    let mut ssid = None;
    let mut ip = None;
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "wpa_state" => state_completed = v.trim() == "COMPLETED",
            "ssid" if !v.trim().is_empty() => ssid = Some(v.trim().to_string()),
            "ip_address" if !v.trim().is_empty() => ip = Some(v.trim().to_string()),
            _ => {}
        }
    }
    WifiStatus {
        airplane_mode: false,
        enabled: true,
        connected_ssid: if state_completed { ssid } else { None },
        ip: if state_completed { ip } else { None },
        signal_bars: 0,
    }
}

fn parse_signal_poll_rssi(raw: &str) -> Option<i32> {
    for line in raw.lines() {
        if let Some(v) = line.strip_prefix("RSSI=") {
            if let Ok(dbm) = v.trim().parse::<i32>() {
                return Some(dbm);
            }
        }
    }
    None
}

fn parse_saved_ssids(raw: &str) -> Vec<String> {
    raw.lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let _id = cols.next()?;
            let ssid = cols.next()?.trim();
            (!ssid.is_empty()).then(|| ssid.to_string())
        })
        .collect()
}

fn find_saved_network_id(raw: &str, target_ssid: &str) -> Option<String> {
    raw.lines().skip(1).find_map(|line| {
        let mut cols = line.split('\t');
        let id = cols.next()?.trim();
        let ssid = cols.next()?.trim();
        (ssid == target_ssid).then(|| id.to_string())
    })
}

/// Parse `wpa_cli scan_results` output, deduplicating by SSID (strongest signal
/// wins) and sorting connected first, then descending by signal strength.
pub fn parse_scan_results(
    raw: &str,
    connected_ssid: Option<&str>,
    saved_ssids: &[String],
) -> Vec<WifiNetwork> {
    let mut by_ssid: HashMap<String, WifiNetwork> = HashMap::new();
    for line in raw.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        let bssid = cols[0].trim();
        let signal_dbm = cols[2].trim().parse::<i32>().unwrap_or(-90);
        let flags = cols[3].trim();
        let ssid = cols[4].trim();
        if ssid.is_empty() || ssid.starts_with("\\x00") {
            continue;
        }
        let secured = flags.contains("WPA") || flags.contains("WEP") || flags.contains("RSN");
        let connected = connected_ssid == Some(ssid);
        let saved = connected || saved_ssids.iter().any(|s| s == ssid);
        let entry = WifiNetwork {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            signal_dbm,
            signal_bars: dbm_to_bars(signal_dbm),
            secured,
            connected,
            saved,
        };
        match by_ssid.get(ssid) {
            Some(prev) if prev.signal_dbm >= entry.signal_dbm => {}
            _ => {
                by_ssid.insert(ssid.to_string(), entry);
            }
        }
    }
    let mut list: Vec<WifiNetwork> = by_ssid.into_values().collect();
    list.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.signal_dbm.cmp(&a.signal_dbm))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpa_status_and_badge() {
        let raw = "bssid=aa:bb:cc:dd:ee:ff\nssid=HomeWifi\nwpa_state=COMPLETED\nip_address=192.168.1.42\n";
        let mut st = parse_wpa_status(raw);
        st.signal_bars = dbm_to_bars(-52);
        assert_eq!(st.connected_ssid.as_deref(), Some("HomeWifi"));
        assert_eq!(st.ip.as_deref(), Some("192.168.1.42"));
        assert_eq!(st.badge(), WifiBadge::Connected { signal: 4 });

        let dis = parse_wpa_status("wpa_state=DISCONNECTED\n");
        assert_eq!(dis.connected_ssid, None);
        assert_eq!(dis.badge(), WifiBadge::Disconnected);
    }

    #[test]
    fn parses_and_deduplicates_scan_results() {
        let raw = "\
bssid / frequency / signal level / flags / ssid
11:22:33:44:55:66\t2412\t-70\t[WPA2-PSK-CCMP][ESS]\tOffice
aa:bb:cc:dd:ee:ff\t5180\t-48\t[WPA2-PSK-CCMP][ESS]\tOffice
22:33:44:55:66:77\t2437\t-60\t[ESS]\tCafeOpen
33:44:55:66:77:88\t2462\t-80\t[WPA2-PSK-CCMP][ESS]\tHomeWifi
";
        let nets = parse_scan_results(raw, Some("HomeWifi"), &["Office".into()]);
        assert_eq!(nets.len(), 3);
        assert_eq!(nets[0].ssid, "HomeWifi");
        assert!(nets[0].connected);
        assert_eq!(nets[1].ssid, "Office");
        assert_eq!(nets[1].signal_dbm, -48);
        assert!(nets[1].saved);
        assert!(nets[1].secured);
        assert_eq!(nets[2].ssid, "CafeOpen");
        assert!(!nets[2].secured);
    }
}
