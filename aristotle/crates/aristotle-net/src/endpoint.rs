// SPDX-License-Identifier: AGPL-3.0-or-later
//! The addresses a Readest account may talk to.
//!
//! Credentials (access and refresh tokens, the Supabase anon key) travel in
//! headers, so an endpoint has to be trustworthy: HTTPS is always accepted.
//! Plain HTTP is accepted only for loopback hosts and RFC 1918 private IPv4
//! addresses: a mock server or a self-hosted Readest instance on the same
//! LAN. That is deliberately narrow — it is meant for development and
//! private networks, and the credentials are in clear on that network.
//! Everything else over HTTP is refused.

use crate::url::Url;

/// Whether `url` may be used as a Readest endpoint (API base or Supabase
/// URL).
///
/// HTTPS is always allowed. HTTP is allowed only when the host is loopback
/// (`127.0.0.1`, `localhost`, `::1`) or a private IPv4 address (`10/8`,
/// `172.16/12`, `192.168/16`). Credentials over such an HTTP connection are
/// readable on the local network; accepting them is a deliberate relaxation
/// for mock servers and self-hosted services, not for the public internet.
pub fn is_allowed(url: &Url) -> bool {
    if url.https() {
        return true;
    }
    let host = url.host();
    if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
        return true;
    }
    private_ipv4(host)
}

/// Whether `host` is a dotted-quad IPv4 address in one of the RFC 1918
/// ranges.
fn private_ipv4(host: &str) -> bool {
    let mut octets = [0u8; 4];
    let mut parts = host.split('.');
    for octet in &mut octets {
        // Canonical decimal only: "010" is octal to inet_aton-style resolvers.
        match parts
            .next()
            .filter(|p| p.len() == 1 || !p.starts_with('0'))
            .and_then(|p| p.parse::<u8>().ok())
        {
            Some(v) => *octet = v,
            None => return false,
        }
    }
    if parts.next().is_some() {
        return false;
    }
    matches!(octets, [10, ..] | [172, 16..=31, ..] | [192, 168, ..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(s: &str) -> bool {
        is_allowed(&Url::parse(s).unwrap())
    }

    #[test]
    fn leading_zero_octets_are_refused() {
        assert!(!allowed("http://010.0.0.1"));
        assert!(!allowed("http://192.168.01.1"));
        assert!(allowed("http://10.0.0.1"));
    }

    #[test]
    fn endpoints_debug_hides_the_anon_key() {
        let e = crate::store::ReadestEndpoints {
            supabase_anon_key: Some("secret-key".into()),
            ..Default::default()
        };
        assert!(!format!("{e:?}").contains("secret-key"));
    }

    #[test]
    fn https_is_always_allowed() {
        assert!(allowed("https://web.readest.com/api"));
        assert!(allowed("https://vbsyxfusjjqdxkjqlysc.supabase.co"));
    }

    #[test]
    fn http_is_allowed_on_loopback_and_private_networks() {
        for s in [
            "http://192.168.1.5:8765",
            "http://10.0.0.2",
            "http://172.20.1.1",
            "http://172.16.0.1",
            "http://172.31.255.254",
            "http://127.0.0.1:9",
            "http://localhost:8765",
            "http://[::1]:8765",
        ] {
            assert!(allowed(s), "{s}");
        }
    }

    #[test]
    fn http_is_refused_elsewhere() {
        for s in [
            "http://example.com",
            "http://172.32.0.1",
            "http://192.169.1.1",
            "http://8.8.8.8",
            "http://172.15.0.1",
            "http://11.0.0.1",
            "http://127.0.0.2",
        ] {
            assert!(!allowed(s), "{s}");
        }
    }
}
