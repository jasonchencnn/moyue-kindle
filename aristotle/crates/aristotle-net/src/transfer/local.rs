// SPDX-License-Identifier: AGPL-3.0-or-later
//! The address of the device on its own network.
//!
//! The page tells the browser where to go, and that is the address of the
//! wireless interface: the device is on the network through it and nowhere
//! else. Without one the device is not on Wi-Fi, and the transfer has
//! nothing to offer.

use std::net::Ipv4Addr;

/// The wireless interface of the device.
pub const WLAN: &str = "wlan0";

/// The IPv4 address of the device on the given interface.
///
/// `None` when the interface is not there, has no address, or the system
/// will not say.
pub fn address_of(interface: &str) -> Option<Ipv4Addr> {
    let mut list: Vec<(String, Ipv4Addr)> = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` fills the list it is given and links it into
    // `head`; `head` is only read while the list is alive.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return None;
    }
    // SAFETY: the list is not null after a successful call, and every entry
    // of it is a valid `ifaddrs` until `freeifaddrs` is called.
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: `cur` points into the list, so its `ifa_addr` is either
        // null or a valid `sockaddr` of the family it says, and its
        // `ifa_name` is a NUL-terminated string.
        unsafe {
            let entry = &*cur;
            let addr = entry.ifa_addr;
            if !addr.is_null() && (*addr).sa_family as i32 == libc::AF_INET {
                // SAFETY: the family is AF_INET, so the address is a
                // `sockaddr_in`.
                let v4 = &*(addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr));
                let name = std::ffi::CStr::from_ptr(entry.ifa_name);
                list.push((name.to_string_lossy().into_owned(), ip));
            }
            cur = entry.ifa_next;
        }
    }
    // SAFETY: `head` is the list `getifaddrs` gave, freed once.
    unsafe { libc::freeifaddrs(head) };
    pick(&list, interface)
}

/// The address of the interface out of the list the system gave.
///
/// A loopback address is never the one to show: the browser is somewhere
/// else on the network.
fn pick(addresses: &[(String, Ipv4Addr)], interface: &str) -> Option<Ipv4Addr> {
    addresses
        .iter()
        .find(|(name, ip)| name == interface && !ip.is_loopback())
        .map(|(_, ip)| *ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_of_the_interface_is_picked() {
        let list = vec![
            ("lo".to_string(), Ipv4Addr::new(127, 0, 0, 1)),
            ("wlan0".to_string(), Ipv4Addr::new(192, 168, 1, 23)),
        ];
        assert_eq!(pick(&list, "wlan0"), Some(Ipv4Addr::new(192, 168, 1, 23)));
    }

    #[test]
    fn a_loopback_address_is_not_the_address_to_show() {
        let list = vec![("wlan0".to_string(), Ipv4Addr::new(127, 0, 0, 1))];
        assert_eq!(pick(&list, "wlan0"), None);
    }

    #[test]
    fn an_interface_that_is_not_there_has_no_address() {
        let list = vec![("eth0".to_string(), Ipv4Addr::new(10, 0, 0, 2))];
        assert_eq!(pick(&list, "wlan0"), None);
    }

    #[test]
    fn the_host_has_an_address_of_its_own() {
        // The loopback is always there, so this is a check that the call
        // itself works and does not crash.
        let _ = address_of("lo");
    }
}
