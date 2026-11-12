// SPDX-License-Identifier: AGPL-3.0-or-later
//! Owned KAF compatibility service for powerd after a framework takeover.
//!
//! powerd asks lipcd whether the framework is up before it acts on the power
//! button. The original framework answers that until it stops: a takeover
//! kills it ten to thirteen seconds after the app starts, and then nothing
//! answers any more. powerd takes that for "the splash screen is still on"
//! and ignores the button (FIX-108). The shim takes the answer over as soon
//! as the original is gone, and takes it back if it dies.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SCRIPT: &str = include_str!("framework.lua");
const READY: &[u8] = b"aristotle-framework-ready\n";
const STARTUP: Duration = Duration::from_secs(5);
/// The lipc service powerd asks about the framework through.
const KAF: &str = "com.lab126.kaf";
/// The property that says the framework is up.
const KAF_STARTED: &str = "frameworkStarted";
/// The takeover marker: the app owns the screen, not the stock GUI.
const TAKEOVER: &str = "/var/run/aristotle-takeover";

/// What answers powerd's question about the framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The original framework: it is up.
    Original,
    /// The original framework: it is there, but it is not up.
    NotReady,
    /// Nothing answers.
    Missing,
}

/// What to do about the framework's KAF source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shim {
    /// Leave it alone.
    Keep,
    /// The shim has to answer for the framework.
    Start,
}

/// The decision, as a pure function.
///
/// * `source`: what answers `com.lab126.kaf` right now.
/// * `takeover`: the app took the framework over, so the original is going away.
/// * `shim`: the shim the app started is still running.
fn decide(source: Source, takeover: bool, shim: bool) -> Shim {
    if shim || !takeover {
        return Shim::Keep;
    }
    match source {
        Source::Original | Source::NotReady => Shim::Keep,
        Source::Missing => Shim::Start,
    }
}

/// The framework's KAF source, and the shim that stands in for it.
pub(crate) struct Framework {
    /// The shim, when the app had to start one.
    child: Option<Child>,
}

impl Framework {
    /// Take the framework's KAF source over at startup.
    ///
    /// The original framework answers while it is up, so the shim waits for
    /// it: a takeover stops the original ten to thirteen seconds later, and
    /// [`ensure`](Self::ensure) takes over then.
    pub(crate) fn acquire() -> io::Result<Self> {
        let existing = crate::powerd::get_prop(KAF, KAF_STARTED, true);
        if !needs_shim(existing, takeover())? {
            return Ok(Self { child: None });
        }
        Ok(Self {
            child: Some(start_shim()?),
        })
    }

    /// Keep the framework's KAF source there.
    ///
    /// True when the shim was started now. The shim is watched with its
    /// child handle, so this does not talk to lipcd while it runs.
    pub(crate) fn ensure(&mut self) -> io::Result<bool> {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                // The shim runs: it answers for the framework.
                return Ok(false);
            }
        }
        // The shim died, or there never was one: the original framework may
        // have gone with it.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let takeover = takeover();
        if !takeover {
            // The stock framework is not ours to replace, and asking lipcd
            // about it every couple of seconds would be work for nothing.
            return Ok(false);
        }
        if decide(read_source(), takeover, false) != Shim::Start {
            return Ok(false);
        }
        self.child = Some(start_shim()?);
        Ok(true)
    }
}

/// Whether the shim has to answer for the framework at startup.
///
/// The startup path is strict: a KAF source that is there but not up, or a
/// missing one outside a takeover, stops the app from starting.
fn needs_shim(existing: io::Result<String>, takeover: bool) -> io::Result<bool> {
    let (source, error) = match existing {
        Ok(value) if value.parse::<u32>().is_ok_and(|value| value != 0) => (Source::Original, None),
        Ok(_) => (Source::NotReady, None),
        Err(error) => (Source::Missing, Some(error)),
    };
    match decide(source, takeover, false) {
        Shim::Start => Ok(true),
        Shim::Keep => match source {
            Source::Original => Ok(false),
            Source::NotReady => Err(io::Error::other("existing KAF is not ready")),
            Source::Missing => Err(error.unwrap_or_else(|| io::Error::other("KAF is gone"))),
        },
    }
}

/// What answers the framework's KAF source right now.
fn read_source() -> Source {
    match crate::powerd::get_prop(KAF, KAF_STARTED, true) {
        Ok(value) if value.parse::<u32>().is_ok_and(|value| value != 0) => Source::Original,
        Ok(_) => Source::NotReady,
        Err(_) => Source::Missing,
    }
}

/// Whether the app took the framework over.
fn takeover() -> bool {
    std::path::Path::new(TAKEOVER).is_file()
}

/// Start the shim and wait until it answers for the framework.
fn start_shim() -> io::Result<Child> {
    let mut command = Command::new("/usr/bin/lua");
    command.args(["-e", SCRIPT]);
    let mut child = spawn(&mut command, STARTUP)?;
    let registered = crate::powerd::get_prop(KAF, KAF_STARTED, true)?;
    if registered != "1" || child.try_wait()?.is_some() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other(
            "KAF helper registration did not remain ready",
        ));
    }
    Ok(child)
}

/// Start the helper and wait for its readiness line.
fn spawn(command: &mut Command, timeout: Duration) -> io::Result<Child> {
    let parent = std::process::id();
    // SAFETY: the child only makes async-signal-safe system calls before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent as libc::pid_t {
                return Err(io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Err(error) = ready(&mut child, timeout) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

/// Wait for the helper's readiness line.
fn ready(child: &mut Child, timeout: Duration) -> io::Result<()> {
    let mut stdout = child.stdout.take().unwrap();
    let fd = stdout.as_raw_fd();
    // SAFETY: fcntl only changes the flags of our owned pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: nonblocking reads keep an unresponsive Lua child within the deadline.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    loop {
        let mut buffer = [0; 64];
        match stdout.read(&mut buffer) {
            Ok(size) => output.extend_from_slice(&buffer[..size]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        if child.try_wait()?.is_some() {
            return Err(io::Error::other("KAF helper exited during startup"));
        }
        if output == READY {
            return Ok(());
        }
        if !READY.starts_with(&output) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected KAF helper readiness response",
            ));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "KAF helper did not become ready",
            ));
        }
        thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

impl Drop for Framework {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            if let Err(error) = child.wait() {
                eprintln!("aristotle: reaping KAF helper: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    fn reaped(pid: u32) {
        // SAFETY: WNOHANG inspects only the child PID and cannot block.
        assert_eq!(
            unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn existing_kaf_is_never_replaced() {
        assert!(!needs_shim(Ok("1".into()), true).unwrap());
        assert!(needs_shim(Ok("0".into()), true).is_err());
        assert!(needs_shim(Ok("invalid".into()), true).is_err());
        assert!(needs_shim(Err(io::Error::other("absent")), false).is_err());
        assert!(needs_shim(Err(io::Error::other("absent")), true).unwrap());
    }

    #[test]
    fn the_shim_takes_over_when_the_original_framework_is_gone() {
        // At startup the original framework answers: the app leaves it alone.
        assert_eq!(decide(Source::Original, true, false), Shim::Keep);
        // It stops a few seconds later: the shim answers for it.
        assert_eq!(decide(Source::Missing, true, false), Shim::Start);
        // The shim runs: there is nothing left to start.
        assert_eq!(decide(Source::Missing, true, true), Shim::Keep);
        assert_eq!(decide(Source::Original, true, true), Shim::Keep);
    }

    #[test]
    fn without_a_takeover_the_stock_framework_is_left_alone() {
        assert_eq!(decide(Source::Missing, false, false), Shim::Keep);
        assert_eq!(decide(Source::Original, false, false), Shim::Keep);
        assert_eq!(decide(Source::NotReady, false, false), Shim::Keep);
    }

    #[test]
    fn a_source_that_is_not_up_is_left_to_its_owner() {
        // The original framework is there but says it is not up: the shim
        // could not register the name anyway.
        assert_eq!(decide(Source::NotReady, true, false), Shim::Keep);
    }

    #[test]
    fn embedded_lua_readiness_protocol_when_lua_is_available() {
        let stub = r#"
local registered = false
package.preload.liblipclua = function()
    return {
        init = function(name)
            assert(name == 'com.lab126.kaf')
            return {
                register_int_property = function(_, name, mode)
                    assert(name == 'frameworkStarted' and mode == 'r')
                    return setmetatable({}, {__newindex = function(_, key, value)
                        assert(key == 'value' and value == 1)
                        registered = true
                    end})
                end
            }
        end,
        run_event_loop = function()
            assert(registered)
            os.exit(0)
        end
    }
end
"#;
        let script = format!("{stub}\n{SCRIPT}");
        let output = match crate::lipc_command::run(Command::new("lua").args(["-e", &script])) {
            Ok(output) => output,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                eprintln!("host Lua unavailable; embedded protocol test skipped");
                return;
            }
            Err(error) => panic!("running embedded Lua: {error}"),
        };
        assert!(output.status.success());
        assert_eq!(output.stdout, READY);
        for rejection in [
            "package.preload.liblipclua=function() return {init=function() return nil end} end",
            "package.preload.liblipclua=function() return {init=function() return {register_int_property=function() return nil end} end} end",
        ] {
            let script = format!("{rejection}\n{SCRIPT}");
            let output = crate::lipc_command::run(Command::new("lua").args(["-e", &script])).unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
        }
    }

    #[test]
    fn ready_child_is_owned_until_drop_and_reaped() {
        let child = spawn(
            &mut command("printf 'aristotle-framework-ready\\n'; exec sleep 60"),
            Duration::from_secs(1),
        )
        .unwrap();
        let pid = child.id();
        let guard = Framework { child: Some(child) };
        drop(guard);
        reaped(pid);
    }

    #[test]
    fn timeout_kills_and_reaps_the_unready_child() {
        let mut child = command("exec sleep 60")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let error = ready(&mut child, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let _ = child.kill();
        let _ = child.wait();
        reaped(pid);
    }

    #[test]
    fn an_unready_helper_does_not_survive_the_start() {
        let mut command = command("exec sleep 60");
        let error = spawn(&mut command, Duration::from_millis(20)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn name_collision_or_registration_failure_cannot_become_ready() {
        assert!(spawn(&mut command("exit 1"), Duration::from_secs(1)).is_err());
        assert!(spawn(
            &mut command("printf invalid; exec sleep 60"),
            Duration::from_secs(1)
        )
        .is_err());
    }
}
