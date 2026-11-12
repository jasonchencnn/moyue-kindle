// SPDX-License-Identifier: AGPL-3.0-or-later
//! Letting a browser reach the device, and taking it away again.
//!
//! The Kindle's firewall drops what comes in, so the port the transfer
//! listens on has to be let through while the page is open, and closed
//! again when it is not. Only the running rules are touched: nothing is
//! written to the firmware or to the device's own directory, so a restart
//! leaves the firewall as it was.
//!
//! Two places hold the rules, because the device's network policy may put
//! its own chain in front of ours:
//!
//! - `INPUT`: the rule that lets the port in.
//! - the policy's own chain (`ARISTOTLE_IN`), when it is there: the rule
//!   goes in front of that chain's final drop.
//!
//! The policy rebuilds its chains every so often, so the rules are checked
//! and put back while the page is open.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The chain the device's network policy puts in front of `INPUT`.
pub const POLICY_CHAIN: &str = "ARISTOTLE_IN";

/// The command that changes the rules.
const IPTABLES: &str = "iptables";

/// The directories the device keeps `iptables` in, in the order it is
/// looked for. The reader may run with a `PATH` (`/usr/bin:/bin`) that does
/// not include them, so the absolute paths come first.
const IPTABLES_DIRS: [&str; 2] = ["/usr/sbin", "/sbin"];

/// The rule that lets the port in, on the `INPUT` chain.
///
/// The rule is the same whatever is done with it; only the operation in
/// front of it changes. That is what lets the rule be looked for again
/// when it has to be taken off.
pub fn input_rule(port: u16, iface: &str) -> Vec<String> {
    vec![
        "INPUT".into(),
        "-i".into(),
        iface.into(),
        "-p".into(),
        "tcp".into(),
        "--dport".into(),
        port.to_string(),
        "-j".into(),
        "ACCEPT".into(),
    ]
}

/// The rule that lets the port into the policy's own chain.
pub fn chain_rule(port: u16, chain: &str) -> Vec<String> {
    vec![
        chain.into(),
        "-p".into(),
        "tcp".into(),
        "--dport".into(),
        port.to_string(),
        "-j".into(),
        "ACCEPT".into(),
    ]
}

/// The arguments that put the rule in front of everything else.
pub fn allow(rule: &[String]) -> Vec<String> {
    let mut args = vec!["-I".to_string()];
    args.extend_from_slice(rule);
    args.insert(2, "1".to_string());
    args
}

/// The arguments that take the rule away again.
pub fn deny(rule: &[String]) -> Vec<String> {
    let mut args = vec!["-D".to_string()];
    args.extend_from_slice(rule);
    args
}

/// The arguments that ask whether the rule is in place.
pub fn check(rule: &[String]) -> Vec<String> {
    let mut args = vec!["-C".to_string()];
    args.extend_from_slice(rule);
    args
}

/// The port the firewall lets through.
pub struct Firewall {
    port: u16,
    iface: String,
    /// The binary that changes the rules.
    iptables: PathBuf,
    /// Whether the rules are in place.
    open: bool,
}

impl Firewall {
    /// A firewall for the given port and interface.
    pub fn new(port: u16, iface: &str) -> Self {
        Self::with_iptables(port, iface, locate_iptables())
    }

    fn with_iptables(port: u16, iface: &str, iptables: PathBuf) -> Self {
        Self {
            port,
            iface: iface.to_string(),
            iptables,
            open: false,
        }
    }

    /// The port this firewall lets through.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether the rules are in place.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Lets the port in.
    ///
    /// A device whose firewall cannot be changed is not a reason to refuse
    /// the transfer: the rules may not be needed at all. The error says so,
    /// and the caller carries on.
    pub fn open(&mut self) -> io::Result<()> {
        self.keep()?;
        self.open = true;
        Ok(())
    }

    /// Puts the rules back if the device's policy took them away.
    pub fn keep(&mut self) -> io::Result<()> {
        let rule = input_rule(self.port, &self.iface);
        if !self.has(&rule)? {
            self.run(&allow(&rule))?;
        }
        // The policy's chain drops everything that is not established, so
        // the rule has to be in front of that drop.
        let chain = chain_rule(self.port, POLICY_CHAIN);
        if self.has_chain(POLICY_CHAIN)? && !self.has(&chain)? {
            self.run(&allow(&chain))?;
        }
        Ok(())
    }

    /// Closes the port again: the rules go away, the firewall is as it was.
    pub fn close(&mut self) {
        // A rule that was put in twice is taken out twice.
        for rule in [
            input_rule(self.port, &self.iface),
            chain_rule(self.port, POLICY_CHAIN),
        ] {
            while self.has(&rule).unwrap_or(false) {
                if self.run(&deny(&rule)).is_err() {
                    break;
                }
            }
        }
        self.open = false;
    }

    /// Whether the rule is in place already.
    fn has(&self, rule: &[String]) -> io::Result<bool> {
        Ok(self.run(&check(rule)).is_ok())
    }

    /// Whether the chain is there.
    fn has_chain(&self, chain: &str) -> io::Result<bool> {
        Ok(self
            .run(&["-L".to_string(), chain.into(), "-n".into()])
            .is_ok())
    }

    /// Runs the firewall command with the given arguments, saying nothing.
    fn run(&self, args: &[String]) -> io::Result<()> {
        let status = Command::new(&self.iptables)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{} {args:?}: {status}",
                self.iptables.display()
            )))
        }
    }
}

impl Drop for Firewall {
    fn drop(&mut self) {
        self.close();
    }
}

/// The `iptables` to run: the first of the device's own directories that
/// holds it, then whatever `PATH` finds.
///
/// The device runs the reader with a `PATH` that may not include `/usr/sbin`,
/// where its `iptables` lives, so the bare name alone is not enough.
fn locate_iptables() -> PathBuf {
    locate_iptables_with(|p| p.is_file())
}

fn locate_iptables_with(exists: impl Fn(&Path) -> bool) -> PathBuf {
    for dir in IPTABLES_DIRS {
        let path = Path::new(dir).join(IPTABLES);
        if exists(&path) {
            return path;
        }
    }
    PathBuf::from(IPTABLES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_lets_the_port_in_on_the_wireless_interface() {
        let rule = input_rule(8080, "wlan0");
        assert_eq!(
            rule,
            vec!["INPUT", "-i", "wlan0", "-p", "tcp", "--dport", "8080", "-j", "ACCEPT"]
        );
    }

    #[test]
    fn the_rule_goes_in_front_of_everything_else() {
        let rule = input_rule(8080, "wlan0");
        let args = allow(&rule);
        assert_eq!(args[0], "-I");
        assert_eq!(args[1], "INPUT");
        assert_eq!(args[2], "1");
        assert_eq!(&args[3..], &rule[1..]);
    }

    #[test]
    fn the_rule_is_taken_off_the_same_way() {
        let rule = input_rule(8080, "wlan0");
        // The same rule, one operation apart: that is what makes the
        // removal find the rule again.
        assert_eq!(&deny(&rule)[1..], &rule[..]);
        assert_eq!(deny(&rule)[0], "-D");
        assert_eq!(&check(&rule)[1..], &rule[..]);
        assert_eq!(check(&rule)[0], "-C");
    }

    #[test]
    fn the_policy_chain_rule_has_no_interface() {
        let rule = chain_rule(8080, POLICY_CHAIN);
        assert_eq!(rule[0], POLICY_CHAIN);
        assert!(!rule.contains(&"-i".to_string()));
        assert!(rule.contains(&"--dport".to_string()));
        assert_eq!(&deny(&rule)[1..], &rule[..]);
    }

    #[test]
    fn a_firewall_that_is_never_opened_closes_without_a_complaint() {
        // Closing a firewall whose rules were never put in place must not
        // fail: the device may not have `iptables` at all.
        let mut fw = Firewall::new(8080, "wlan0");
        fw.close();
        assert!(!fw.is_open());
    }

    #[test]
    fn iptables_comes_from_the_sbin_directories_before_path() {
        // `/usr/sbin` wins when both are there; `/sbin` is next; the bare
        // name is the last resort, for a device whose `PATH` has it.
        let prefer_usr_sbin = locate_iptables_with(|p| {
            p == Path::new("/usr/sbin/iptables") || p == Path::new("/sbin/iptables")
        });
        assert_eq!(prefer_usr_sbin, PathBuf::from("/usr/sbin/iptables"));

        let only_sbin = locate_iptables_with(|p| p == Path::new("/sbin/iptables"));
        assert_eq!(only_sbin, PathBuf::from("/sbin/iptables"));

        assert_eq!(locate_iptables_with(|_| false), PathBuf::from("iptables"));
    }

    #[test]
    fn a_missing_iptables_leaves_the_firewall_closed() {
        // The reader's `PATH` may not hold `/usr/sbin`: opening then fails
        // with `NotFound`, and the page must not pretend the port is open.
        let mut fw = Firewall::with_iptables(
            8080,
            "wlan0",
            PathBuf::from("/nonexistent/aristotle/iptables"),
        );
        let error = fw.open().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!fw.is_open());
        fw.close();
        assert!(!fw.is_open());
    }
}
