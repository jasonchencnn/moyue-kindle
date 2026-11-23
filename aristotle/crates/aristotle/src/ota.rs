// SPDX-License-Identifier: AGPL-3.0-or-later
//! The app's side of the software update: the state the reader sees, the
//! worker that does the work, and the handover that lets the device's
//! updater take over.
//!
//! The reader never replaces its own files while it runs. It reads the
//! manifest, downloads the package, checks its size and its digest, and
//! then writes a handover file and stops: the device's updater
//! (`device/extension/bin/ota-update.sh`) finds the file, does the swap
//! outside the running program, and starts the new build.
//!
//! The work runs on a thread of its own and reports through the app's
//! channel, so the screen is never blocked. The screen is refreshed when the
//! stage changes or when the bar has moved by five percent or more: an
//! e-ink page does not need to know about every kilobyte.
//!
//! Only one update runs at a time. Every run carries a number, so a report
//! of a run that was given up on is thrown away instead of landing on the
//! screen of the run that replaced it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use aristotle_hal::platform;
use aristotle_hal::system;
use aristotle_net::http::Client;
use aristotle_net::ota::{self, Manifest, OtaProgress, OtaStage, PackageTarget, Rules};
use aristotle_screen::model::device_code;

pub use aristotle_net::ota::Check;

/// Where the releases of the devices live.
pub const BASE_URL: &str = "https://me.chen-cn.top/moyue/ota/chan";

/// The name of the file that hands the package over to the updater.
pub const HANDOVER_FILE: &str = "handover.conf";

/// How much the bar has to move before the screen is drawn again.
pub const STEP_PERCENT: u8 = 5;

/// How long the reader waits for the server.
///
/// A package is tens of megabytes over a slow connection, so the body is
/// given the time it needs; the same values the cloud features use.
pub fn timeouts() -> aristotle_net::http::Timeouts {
    aristotle_net::http::Timeouts {
        connect: std::time::Duration::from_secs(20),
        response: std::time::Duration::from_secs(30),
        body: std::time::Duration::from_secs(300),
    }
}

/// What the update is doing, as the app holds it.
#[derive(Clone, Debug)]
pub struct State {
    /// Where the update stands.
    pub stage: OtaStage,
    /// The version that is installed.
    pub current_version: String,
    /// The version the server has, when it answered.
    pub remote_version: Option<String>,
    /// How far the download has come, from 0 to 100.
    pub percent: u8,
    /// The bytes that have arrived.
    pub downloaded_bytes: u64,
    /// The size of the package, when it is known.
    pub total_bytes: Option<u64>,
    /// What the last step said, when it had something to say.
    pub message: Option<String>,
    /// The manifest of the release, once it has been read.
    pub manifest: Option<Manifest>,
    /// The package that was verified, once it is there.
    pub package: Option<PathBuf>,
}

impl State {
    /// The state of a reader that has not looked for an update yet.
    pub fn new(current_version: String) -> State {
        State {
            stage: OtaStage::Idle,
            current_version,
            remote_version: None,
            percent: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            message: None,
            manifest: None,
            package: None,
        }
    }

    /// Takes a report of the worker and keeps it.
    ///
    /// The report is kept as it is: the screen decides whether it is worth
    /// drawing again.
    pub fn apply(&mut self, progress: OtaProgress) {
        self.stage = progress.stage;
        if let Some(version) = progress.target_version {
            self.remote_version = Some(version);
        }
        self.percent = progress.percent;
        self.downloaded_bytes = progress.downloaded_bytes;
        if progress.total_bytes.is_some() {
            self.total_bytes = progress.total_bytes;
        }
        if progress.error.is_some() {
            self.message = progress.error.clone();
        }
    }

    /// Whether the screen is worth drawing again for this report.
    ///
    /// The stage always is; the bar only when it has moved far enough.
    pub fn worth_drawing(&self, previous: &State) -> bool {
        previous.stage != self.stage
            || self.percent.saturating_sub(previous.percent) >= STEP_PERCENT
            || (previous.downloaded_bytes != self.downloaded_bytes && self.total_bytes.is_none())
    }
}

/// The channels this device may have, most specific first.
///
/// A device with the firmware "5.13.6" may read a release published for
/// "5.13.6", for "5.13" and for "5": the updater on the device tries them
/// in that order.
pub fn channels(code: &str, firmware: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut candidate = firmware.to_string();
    loop {
        out.push(format!("{code}-{candidate}"));
        match candidate.rfind('.') {
            Some(at) => candidate.truncate(at),
            None => break,
        }
    }
    out
}

/// The device this reader runs on: its code and its firmware.
pub fn device() -> Option<(String, String)> {
    let serial = platform::serial_number().ok()?;
    let code = device_code(&serial)?.to_string();
    let firmware = system::firmware_version()?;
    Some((code, firmware))
}

/// The folder the packages and the handover live in.
///
/// It is the folder the device's updater looks at: `$MOYUE_OTA_DIR` when it
/// is set, and the app's own data folder otherwise. It is never the book
/// library.
pub fn ota_dir(data: &Path) -> PathBuf {
    match std::env::var_os("MOYUE_OTA_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => data.join("ota"),
    }
}

/// The rules a manifest of this device must follow.
fn rules(channel: &str) -> Rules {
    Rules::device(format!("{BASE_URL}/{channel}/"))
}

/// Reads the manifest of the newest release this device may have.
///
/// The channels are tried in order; a channel that is not there moves to the
/// next one, and any other failure is a real failure. A manifest that is
/// not for this device is refused: a package built for another model would
/// not run here.
pub fn check(client: &Client, code: &str, firmware: &str) -> Result<Manifest, ota::OtaError> {
    let mut last: Option<ota::OtaError> = None;
    for channel in channels(code, firmware) {
        let url = format!("{BASE_URL}/{channel}/manifest.txt");
        let url = match aristotle_net::url::Url::parse(&url) {
            Ok(url) => url,
            Err(e) => {
                return Err(ota::OtaError::Manifest(
                    aristotle_net::ota::ManifestError::BadValue {
                        field: "channel",
                        detail: format!("{channel} is not a usable address: {e}"),
                    },
                ))
            }
        };
        match ota::fetch_for_device(client, &url, &rules(&channel), code, &channel, firmware) {
            Ok(manifest) => return Ok(manifest),
            Err(ota::OtaError::Status(404)) => last = Some(ota::OtaError::Status(404)),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        ota::OtaError::Manifest(aristotle_net::ota::ManifestError::MissingField("channel"))
    }))
}

/// Downloads and verifies the package of a release.
pub fn download(
    client: &Client,
    manifest: &Manifest,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(OtaProgress),
) -> Result<PathBuf, ota::OtaError> {
    let name = manifest
        .package
        .path()
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("package.tar.gz");
    let target = PackageTarget::in_dir(dir, name);
    ota::download_package(client, manifest, &target, cancel, progress)
}

/// Writes the handover that lets the device's updater take over.
///
/// The file names the package that was verified, the digest it was checked
/// against and the device it is for, so the updater never trusts a file it
/// did not write itself and never installs a package of another model. The
/// reader stops right after; the swap happens outside it.
pub fn write_handover(dir: &Path, manifest: &Manifest, package: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = format!(
        "version={}\n\
         device={}\n\
         channel={}\n\
         firmware={}\n\
         package={}\n\
         sha256={}\n\
         size={}\n",
        manifest.version,
        manifest.device,
        manifest.channel,
        manifest.firmware,
        package.display(),
        manifest.sha256,
        manifest.size
    );
    let path = dir.join(HANDOVER_FILE);
    let tmp = dir.join(format!("{HANDOVER_FILE}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        file.flush()?;
        // The handover is only complete when it is on the disk: the reader
        // stops right after this, and the updater must not read half a file.
        file.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// What the worker reports to the main loop.
pub enum Report {
    /// The update moved on.
    Progress(OtaProgress),
    /// The check is over: the release is there, or there is none.
    Checked(Result<(Check, Manifest), String>),
    /// The package is on the disk and checked.
    Verified(Result<PathBuf, String>),
}

/// Does the step of the update that was asked for, on the worker's thread.
///
/// The step is one of the two: looking for a release, or bringing its
/// package in. The progress of the download travels through `tx` as it
/// happens; the result is the value that is returned.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    tx: &crate::wake::Sender<crate::net::Done>,
    generation: u64,
    client: Client,
    code: String,
    firmware: String,
    dir: PathBuf,
    manifest: Option<Manifest>,
    version: String,
    cancel: Arc<AtomicBool>,
    check_only: bool,
) -> crate::net::Done {
    let send = |report: Report| {
        let _ = tx.send(crate::net::Done::Ota { generation, report });
    };
    if check_only {
        let report = match check(&client, &code, &firmware) {
            Ok(manifest) => match manifest.compare(&version) {
                Ok(check) => Report::Checked(Ok((check, manifest))),
                Err(e) => Report::Checked(Err(e.to_string())),
            },
            Err(e) => Report::Checked(Err(e.to_string())),
        };
        // The result is the value that is returned: the caller sends it.
        return crate::net::Done::Ota { generation, report };
    }
    let Some(manifest) = manifest else {
        return crate::net::Done::Ota {
            generation,
            report: Report::Verified(Err("there is no release to download".into())),
        };
    };
    let mut progress = |p: OtaProgress| {
        send(Report::Progress(p));
    };
    let result = download(&client, &manifest, &dir, &cancel, &mut progress);
    let report = match result {
        Ok(path) => Report::Verified(Ok(path)),
        Err(e) => Report::Verified(Err(e.to_string())),
    };
    crate::net::Done::Ota { generation, report }
}

/// The version of the running program.
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// A handle that asks a run to stop.
///
/// The reader keeps the handle and the worker keeps a copy of it, so the
/// download stops at the next chunk instead of at the end of the package.
pub fn cancel_handle() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channels_are_tried_from_the_whole_firmware_down() {
        assert_eq!(
            channels("oasis2", "5.13.6"),
            ["oasis2-5.13.6", "oasis2-5.13", "oasis2-5"]
        );
        assert_eq!(channels("pw4", "5.16"), ["pw4-5.16", "pw4-5"]);
    }

    #[test]
    fn the_ota_folder_is_not_the_book_library() {
        let dir = ota_dir(Path::new("/mnt/us/.aristotle"));
        assert_eq!(dir, PathBuf::from("/mnt/us/.aristotle/ota"));
        assert!(!dir.starts_with("/mnt/us/documents"));
    }

    #[test]
    fn the_handover_names_the_package_and_the_device() {
        let dir = std::env::temp_dir().join("aristotle-ota-handover");
        let _ = std::fs::remove_dir_all(&dir);
        let manifest = Manifest {
            product: ota::PRODUCT.to_string(),
            channel: "oasis2-5.16".into(),
            version: "1.2.3".into(),
            device: "oasis2".into(),
            firmware: "5.16".into(),
            package: aristotle_net::Url::parse(
                "https://me.chen-cn.top/moyue/ota/chan/oasis2-5.16/p.tar.gz",
            )
            .unwrap(),
            sha256: "a".repeat(64),
            size: 1234,
            source: aristotle_net::Url::parse(
                "https://me.chen-cn.top/moyue/ota/chan/oasis2-5.16/s.tar.gz",
            )
            .unwrap(),
            source_sha256: "b".repeat(64),
            source_size: 5678,
        };
        let package = dir.join("p.tar.gz");
        write_handover(&dir, &manifest, &package).unwrap();
        let text = std::fs::read_to_string(dir.join(HANDOVER_FILE)).unwrap();
        assert!(text.contains("version=1.2.3\n"), "{text}");
        assert!(text.contains("device=oasis2\n"), "{text}");
        assert!(text.contains("channel=oasis2-5.16\n"), "{text}");
        assert!(text.contains("firmware=5.16\n"), "{text}");
        assert!(
            text.contains(&format!("package={}\n", package.display())),
            "{text}"
        );
        assert!(
            text.contains(&format!("sha256={}\n", "a".repeat(64))),
            "{text}"
        );
        assert!(text.contains("size=1234\n"), "{text}");
        // The temporary file is gone: the handover was renamed into place.
        assert!(!dir.join(format!("{HANDOVER_FILE}.tmp")).exists());
    }
}
