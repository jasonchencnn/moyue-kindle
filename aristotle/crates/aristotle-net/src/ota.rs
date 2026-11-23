// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OTA update protocol of 墨阅: the `MOYUE-OTA-1` manifest, the version
//! rules and the verified download of a release package.
//!
//! The rules are the ones of the device-side updater
//! (`device/extension/bin/ota-update.sh`) and of the manifest builder
//! (`tools/ota-manifest.py`): the same manifest format, the same refusal to
//! downgrade, the same size limit and the same SHA-256 check. The app does
//! what the script did, but inside the reader, so the reader can show how far
//! the download has come.
//!
//! The app never unpacks a package and never replaces its own files: it
//! downloads and verifies the package and leaves the swap to the updater that
//! runs after this process has stopped.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use crate::http::{Client, Error as HttpError, Request};
use crate::url::Url;

/// The first line of a manifest.
pub const HEADER: &str = "MOYUE-OTA-1";

/// The product a manifest must name.
pub const PRODUCT: &str = "moyue-preview";

/// The largest manifest the app reads, in bytes.
pub const MAX_MANIFEST_BYTES: u64 = 4096;

/// The largest package the app downloads, in bytes.
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;

/// The fields of a manifest, each of them exactly once.
const FIELDS: [&str; 11] = [
    "product",
    "channel",
    "version",
    "device",
    "firmware",
    "package",
    "sha256",
    "size",
    "source",
    "source-sha256",
    "source-size",
];

/// The bytes a field's value may hold, as the device-side parser accepts
/// them: letters, digits and `. _ : / @ % + , = & ? -`.
fn is_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b':' | b'/' | b'@' | b'%' | b'+' | b',' | b'=' | b'&' | b'?' | b'-'
        )
}

/// A release version: three numbers, each of them `0` or `1` to `999` without
/// a leading zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    /// The version of a `X.Y.Z` string, or `None` when it is not one.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.split('.');
        let major = parse_part(parts.next()?)?;
        let minor = parse_part(parts.next()?)?;
        let patch = parse_part(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One number of a version: `0`, or a number of one to three digits that does
/// not start with a zero.
fn parse_part(part: &str) -> Option<u16> {
    if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    part.parse().ok()
}

/// What the check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// A newer version is on the server.
    Available,
    /// The server has the version that is installed.
    UpToDate,
    /// The server has an older version: no downgrade.
    Downgrade,
}

/// A release manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub product: String,
    pub channel: String,
    pub version: String,
    pub device: String,
    pub firmware: String,
    /// Where the package is downloaded from.
    pub package: Url,
    /// The package's SHA-256, in lowercase hex.
    pub sha256: String,
    /// The package's size in bytes.
    pub size: u64,
    /// Where the matching source is published.
    pub source: Url,
    /// The source's SHA-256, in lowercase hex.
    pub source_sha256: String,
    /// The source's size in bytes.
    pub source_size: u64,
}

impl Manifest {
    /// Whether the manifest is meant for this device: its code, its channel
    /// and its firmware.
    ///
    /// The firmware of the device may be newer than the one the manifest
    /// names, as long as it starts with it ("5.13" covers "5.13.6"), the way
    /// the device-side updater accepts it.
    pub fn matches(&self, device: &str, channel: &str, firmware: &str) -> bool {
        self.device == device
            && self.channel == channel
            && (firmware == self.firmware
                || firmware
                    .strip_prefix(&self.firmware)
                    .is_some_and(|rest| rest.starts_with('.')))
    }

    /// What the manifest means for the version that is installed.
    pub fn compare(&self, installed: &str) -> Result<Check, ManifestError> {
        let Some(installed) = Version::parse(installed) else {
            return Err(ManifestError::BadValue {
                field: "version",
                detail: format!("the installed version {installed:?} is not X.Y.Z"),
            });
        };
        let Some(remote) = Version::parse(&self.version) else {
            return Err(ManifestError::BadValue {
                field: "version",
                detail: format!("the manifest version {:?} is not X.Y.Z", self.version),
            });
        };
        Ok(if remote > installed {
            Check::Available
        } else if remote == installed {
            Check::UpToDate
        } else {
            Check::Downgrade
        })
    }
}

/// Why a manifest was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The manifest is empty.
    Empty,
    /// The first line is not the header.
    BadHeader,
    /// The manifest is larger than [`MAX_MANIFEST_BYTES`].
    TooLarge(usize),
    /// A line is not a `key=value` pair.
    BadLine(String),
    /// A field is not one of the manifest's.
    UnknownField(String),
    /// A field appears twice.
    DuplicateField(String),
    /// A field is missing.
    MissingField(&'static str),
    /// A field's value does not fit its rule.
    BadValue { field: &'static str, detail: String },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Empty => f.write_str("the manifest is empty"),
            ManifestError::BadHeader => f.write_str("the manifest is not a MOYUE OTA manifest"),
            ManifestError::TooLarge(n) => write!(f, "the manifest is too large ({n} bytes)"),
            ManifestError::BadLine(why) => write!(f, "the manifest has a bad line: {why}"),
            ManifestError::UnknownField(k) => write!(f, "the manifest has an unknown field: {k}"),
            ManifestError::DuplicateField(k) => write!(f, "the manifest repeats the field {k}"),
            ManifestError::MissingField(k) => write!(f, "the manifest is missing the field {k}"),
            ManifestError::BadValue { field, detail } => {
                write!(f, "the manifest's {field} is not valid: {detail}")
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// What the URLs of a manifest may look like.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    /// The prefix both URLs must start with, e.g.
    /// "https://me.chen-cn.top/moyue/ota/oasis2-5.16/". `None` accepts any
    /// host.
    pub prefix: Option<String>,
    /// Whether plain `http://` is accepted. A device only ever speaks
    /// https; the tests' server does not.
    pub allow_http: bool,
}

impl Rules {
    /// The rules of a device: one prefix, https only.
    pub fn device(prefix: impl Into<String>) -> Rules {
        Rules {
            prefix: Some(prefix.into()),
            allow_http: false,
        }
    }
}

/// Parse a manifest.
///
/// The manifest is a line-oriented text file, not JSON: the first line is the
/// header and every other line is one field, each of them exactly once, with
/// no unknown field and no blank line. Every rule is the one of the
/// device-side updater.
pub fn parse_manifest(text: &str, rules: &Rules) -> Result<Manifest, ManifestError> {
    if text.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge(text.len()));
    }
    if text.contains('\r') {
        return Err(ManifestError::BadLine("it holds a carriage return".into()));
    }
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Err(ManifestError::Empty);
    };
    if header != HEADER {
        return Err(ManifestError::BadHeader);
    }

    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            return Err(ManifestError::BadLine("it holds a blank line".into()));
        }
        if line.starts_with(' ') {
            return Err(ManifestError::BadLine("a line starts with a space".into()));
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(ManifestError::BadLine(format!("{line:?} is not a field")));
        };
        if key.is_empty() || value.is_empty() {
            return Err(ManifestError::BadLine(format!(
                "{line:?} has an empty part"
            )));
        }
        if !key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ManifestError::BadLine(format!(
                "{key:?} is not a field name"
            )));
        }
        if !value.bytes().all(is_value_byte) {
            return Err(ManifestError::BadLine(format!(
                "the value of {key:?} holds a character that is not allowed"
            )));
        }
        if !FIELDS.contains(&key) {
            return Err(ManifestError::UnknownField(key.to_string()));
        }
        if seen.insert(key, value).is_some() {
            return Err(ManifestError::DuplicateField(key.to_string()));
        }
    }
    for field in FIELDS {
        if !seen.contains_key(field) {
            return Err(ManifestError::MissingField(field));
        }
    }
    let field = |name: &str| seen[name];

    if field("product") != PRODUCT {
        return Err(ManifestError::BadValue {
            field: "product",
            detail: format!("expected {PRODUCT}, found {:?}", field("product")),
        });
    }
    let channel = field("channel");
    if channel.is_empty()
        || !channel
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err(ManifestError::BadValue {
            field: "channel",
            detail: format!("{channel:?} is not a channel name"),
        });
    }
    if Version::parse(field("version")).is_none() {
        return Err(ManifestError::BadValue {
            field: "version",
            detail: format!("{:?} is not X.Y.Z", field("version")),
        });
    }
    let sha256 = field("sha256");
    if !is_digest(sha256) {
        return Err(ManifestError::BadValue {
            field: "sha256",
            detail: format!("{sha256:?} is not a SHA-256 digest"),
        });
    }
    let source_sha256 = field("source-sha256");
    if !is_digest(source_sha256) {
        return Err(ManifestError::BadValue {
            field: "source-sha256",
            detail: format!("{source_sha256:?} is not a SHA-256 digest"),
        });
    }
    let size = parse_size(field("size")).ok_or_else(|| ManifestError::BadValue {
        field: "size",
        detail: format!("{:?} is not a size", field("size")),
    })?;
    if size == 0 || size > MAX_PACKAGE_BYTES {
        return Err(ManifestError::BadValue {
            field: "size",
            detail: format!("{size} is out of range"),
        });
    }
    let source_size = parse_size(field("source-size")).ok_or_else(|| ManifestError::BadValue {
        field: "source-size",
        detail: format!("{:?} is not a size", field("source-size")),
    })?;
    if source_size == 0 {
        return Err(ManifestError::BadValue {
            field: "source-size",
            detail: "the source must not be empty".into(),
        });
    }
    let package = check_url(field("package"), "package", rules)?;
    let source = check_url(field("source"), "source", rules)?;

    Ok(Manifest {
        product: field("product").to_string(),
        channel: channel.to_string(),
        version: field("version").to_string(),
        device: field("device").to_string(),
        firmware: field("firmware").to_string(),
        package,
        sha256: sha256.to_string(),
        size,
        source,
        source_sha256: source_sha256.to_string(),
        source_size,
    })
}

/// Whether `text` is a SHA-256 digest: 64 lowercase hex digits.
fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A size in bytes, written in digits only.
fn parse_size(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The URL of the package or of the source, checked against the rules.
fn check_url(raw: &str, field: &'static str, rules: &Rules) -> Result<Url, ManifestError> {
    if let Some(prefix) = &rules.prefix {
        if !raw.starts_with(prefix.as_str()) {
            return Err(ManifestError::BadValue {
                field,
                detail: format!("{raw:?} is not under the allowed path"),
            });
        }
    }
    let url = Url::parse(raw).map_err(|e| ManifestError::BadValue {
        field,
        detail: format!("{raw:?} is not an address: {e}"),
    })?;
    if !url.https() && !rules.allow_http {
        return Err(ManifestError::BadValue {
            field,
            detail: format!("{raw:?} is not an https address"),
        });
    }
    // A path that walks up leaves the directory the manifest may publish in.
    if url.path().split('/').any(|part| part == "..") {
        return Err(ManifestError::BadValue {
            field,
            detail: format!("{raw:?} walks out of its directory"),
        });
    }
    Ok(url)
}

/// Where a package is written while it is fetched, and where the verified
/// package ends up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageTarget {
    /// The file being written.
    pub part: PathBuf,
    /// The file the verified package is renamed to.
    pub verified: PathBuf,
}

impl PackageTarget {
    /// The target of a package called `name` in `dir`.
    pub fn in_dir(dir: &Path, name: &str) -> PackageTarget {
        PackageTarget {
            part: dir.join(format!("{name}.part")),
            verified: dir.join(name),
        }
    }
}

/// Where the update stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaStage {
    /// Nothing has been asked for yet.
    Idle,
    /// The manifest is being read.
    Checking,
    /// A newer version is on the server.
    Available,
    /// The installed version is the newest one.
    UpToDate,
    /// The server has an older version: no downgrade.
    Downgrade,
    /// The package is on its way.
    Downloading,
    /// The package's size and digest are being checked.
    Verifying,
    /// The verified package is being put in place.
    Staging,
    /// The package is ready; the reader can hand it over and stop.
    ReadyToApply,
    /// Something went wrong.
    Failed,
}

/// What the update is doing, for the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtaProgress {
    pub stage: OtaStage,
    /// How far the download has come, from 0 to 100.
    pub percent: u8,
    /// The bytes that have arrived.
    pub downloaded_bytes: u64,
    /// The size of the package, when it is known.
    pub total_bytes: Option<u64>,
    /// The version the update is for.
    pub target_version: Option<String>,
    /// What went wrong, when something did.
    pub error: Option<String>,
}

impl OtaProgress {
    /// A report of a stage, with nothing else.
    pub fn stage(stage: OtaStage) -> OtaProgress {
        OtaProgress {
            stage,
            percent: 0,
            downloaded_bytes: 0,
            total_bytes: None,
            target_version: None,
            error: None,
        }
    }

    /// The share of the package that has arrived, in percent.
    pub fn percent_of(bytes: u64, total: Option<u64>) -> u8 {
        let Some(total) = total.filter(|t| *t > 0) else {
            return 0;
        };
        let percent = (bytes as u128 * 100) / total as u128;
        percent.min(100) as u8
    }
}

/// Why the update failed.
#[derive(Debug)]
pub enum OtaError {
    /// The manifest was refused.
    Manifest(ManifestError),
    /// The request failed.
    Http(HttpError),
    /// The server answered with a status the reader did not expect.
    Status(u16),
    /// The manifest is not for this device.
    WrongDevice {
        device: String,
        channel: String,
    },
    /// The package's size is not the one the manifest names.
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    /// The package's digest is not the one the manifest names.
    DigestMismatch {
        expected: String,
        actual: String,
    },
    /// The manifest is not text.
    NotText,
    Io(std::io::Error),
}

impl fmt::Display for OtaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OtaError::Manifest(e) => e.fmt(f),
            OtaError::Http(e) => e.fmt(f),
            OtaError::Status(s) => write!(f, "the server answered HTTP {s}"),
            OtaError::WrongDevice { device, channel } => {
                write!(f, "the update is not for the device {device} ({channel})")
            }
            OtaError::SizeMismatch { expected, actual } => {
                write!(f, "the package is {actual} bytes, expected {expected}")
            }
            OtaError::DigestMismatch { expected, actual } => {
                write!(f, "the package's SHA-256 is {actual}, expected {expected}")
            }
            OtaError::NotText => f.write_str("the manifest is not text"),
            OtaError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for OtaError {}

impl From<ManifestError> for OtaError {
    fn from(e: ManifestError) -> Self {
        OtaError::Manifest(e)
    }
}

impl From<HttpError> for OtaError {
    fn from(e: HttpError) -> Self {
        OtaError::Http(e)
    }
}

impl From<std::io::Error> for OtaError {
    fn from(e: std::io::Error) -> Self {
        OtaError::Io(e)
    }
}

/// Read the manifest of a release.
pub fn fetch_manifest(client: &Client, url: &Url, rules: &Rules) -> Result<Manifest, OtaError> {
    let response = client.send(&Request::get(url.clone()), MAX_MANIFEST_BYTES)?;
    if response.status != 200 {
        return Err(OtaError::Status(response.status));
    }
    let text = String::from_utf8(response.body).map_err(|_| OtaError::NotText)?;
    Ok(parse_manifest(&text, rules)?)
}

/// Reads the manifest of a release and checks that it is for this device.
///
/// A manifest that names another model, another channel or another firmware
/// is refused: its package would not run here.
pub fn fetch_for_device(
    client: &Client,
    url: &Url,
    rules: &Rules,
    device: &str,
    channel: &str,
    firmware: &str,
) -> Result<Manifest, OtaError> {
    let manifest = fetch_manifest(client, url, rules)?;
    if !manifest.matches(device, channel, firmware) {
        return Err(OtaError::WrongDevice {
            device: device.to_string(),
            channel: channel.to_string(),
        });
    }
    Ok(manifest)
}

/// Download the package of `manifest`, check its size and its digest, and
/// leave it under its final name.
///
/// The bytes go into `target.part` and the digest is computed while they
/// arrive, so the file is never read a second time. Only a package whose size
/// and SHA-256 are exactly the manifest's is renamed to `target.verified`: a
/// package that fails is deleted, so a half-written file never looks like a
/// release.
pub fn download_package(
    client: &Client,
    manifest: &Manifest,
    target: &PackageTarget,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(OtaProgress),
) -> Result<PathBuf, OtaError> {
    if let Some(parent) = target.part.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut sink = DigestWriter::new(&target.part)?;
    let version = Some(manifest.version.clone());
    // The bar starts at zero: the reader shows an empty bar while the first
    // bytes are on their way.
    progress(OtaProgress {
        stage: OtaStage::Downloading,
        percent: 0,
        downloaded_bytes: 0,
        total_bytes: Some(manifest.size),
        target_version: version.clone(),
        error: None,
    });
    let outcome = client.download(
        &Request::get(manifest.package.clone()),
        &mut sink,
        MAX_PACKAGE_BYTES,
        cancel,
        &mut |bytes, total| {
            let total = total.or(Some(manifest.size));
            progress(OtaProgress {
                stage: OtaStage::Downloading,
                percent: OtaProgress::percent_of(bytes, total),
                downloaded_bytes: bytes,
                total_bytes: total,
                target_version: version.clone(),
                error: None,
            });
        },
    );
    let bytes = sink.finish()?;
    if let Err(e) = outcome {
        let _ = std::fs::remove_file(&target.part);
        return Err(OtaError::Http(e));
    }

    progress(OtaProgress {
        stage: OtaStage::Verifying,
        percent: 100,
        downloaded_bytes: bytes,
        total_bytes: Some(manifest.size),
        target_version: version.clone(),
        error: None,
    });
    if bytes != manifest.size {
        let _ = std::fs::remove_file(&target.part);
        return Err(OtaError::SizeMismatch {
            expected: manifest.size,
            actual: bytes,
        });
    }
    let digest = sink.digest();
    if digest != manifest.sha256 {
        let _ = std::fs::remove_file(&target.part);
        return Err(OtaError::DigestMismatch {
            expected: manifest.sha256.clone(),
            actual: digest,
        });
    }

    progress(OtaProgress {
        stage: OtaStage::Staging,
        percent: 100,
        downloaded_bytes: bytes,
        total_bytes: Some(manifest.size),
        target_version: version.clone(),
        error: None,
    });
    // The verified package takes its final name in one step.
    std::fs::rename(&target.part, &target.verified)?;
    progress(OtaProgress {
        stage: OtaStage::ReadyToApply,
        percent: 100,
        downloaded_bytes: bytes,
        total_bytes: Some(manifest.size),
        target_version: version,
        error: None,
    });
    Ok(target.verified.clone())
}

/// A file that hashes what it writes.
struct DigestWriter {
    file: File,
    hasher: Sha256,
    bytes: u64,
}

impl DigestWriter {
    fn new(path: &Path) -> std::io::Result<DigestWriter> {
        Ok(DigestWriter {
            file: File::create(path)?,
            hasher: Sha256::new(),
            bytes: 0,
        })
    }

    /// The digest of what was written, in lowercase hex.
    fn digest(&self) -> String {
        hex_lower(&self.hasher.clone().finalize())
    }

    /// The file is closed and its bytes are on the disk.
    fn finish(&mut self) -> std::io::Result<u64> {
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(self.bytes)
    }
}

impl Write for DigestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Bytes as the lowercase hex a manifest writes.
fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Timeouts;
    use crate::testserver::{Reply, Server};
    use std::time::Duration;

    /// The rules of a device that reads the releases of one channel.
    fn rules(prefix: &str) -> Rules {
        Rules::device(prefix)
    }

    /// A manifest of a release under `base`, with the given version.
    fn manifest_text(base: &str, version: &str, digest: &str, size: u64) -> String {
        format!(
            "{HEADER}\n\
             product={PRODUCT}\n\
             channel=ko2-5.16\n\
             version={version}\n\
             device=oasis2\n\
             firmware=5.16\n\
             package={base}package.tar.gz\n\
             sha256={digest}\n\
             size={size}\n\
             source={base}source.tar.gz\n\
             source-sha256={digest}\n\
             source-size={size}\n"
        )
    }

    const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// A manifest that is valid in every other respect.
    fn good(base: &str) -> String {
        manifest_text(base, "1.2.3", DIGEST, 1024)
    }

    // ------------------------------------------------------------ the manifest

    #[test]
    fn a_valid_manifest_is_read() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let m = parse_manifest(&good(base), &rules(base)).unwrap();
        assert_eq!(m.product, PRODUCT);
        assert_eq!(m.channel, "ko2-5.16");
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.device, "oasis2");
        assert_eq!(m.firmware, "5.16");
        assert_eq!(m.size, 1024);
        assert_eq!(m.sha256, DIGEST);
        assert_eq!(m.package.to_string(), format!("{base}package.tar.gz"));
        assert_eq!(m.source_size, 1024);
    }

    #[test]
    fn the_manifest_needs_its_header() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let text = good(base).replace(HEADER, "MOYUE-OTA-2");
        assert_eq!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadHeader)
        );
        assert_eq!(parse_manifest("", &rules(base)), Err(ManifestError::Empty));
    }

    #[test]
    fn every_field_is_needed_exactly_once() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        // A field missing.
        let text = good(base).replace("\nsize=1024\n", "\n");
        assert_eq!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::MissingField("size"))
        );
        // A field twice.
        let text = format!("{}{}\n", good(base), "size=1024\n");
        assert_eq!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::DuplicateField("size".into()))
        );
        // A field the format does not know.
        let text = format!("{}{}\n", good(base), "signature=abc\n");
        assert_eq!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::UnknownField("signature".into()))
        );
    }

    #[test]
    fn a_manifest_may_not_have_blank_lines_or_strange_bytes() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let text = good(base).replace("size=1024\n", "size=1024\n\n");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadLine(_))
        ));
        let text = good(base).replace('\n', "\r\n");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadLine(_))
        ));
        let text = good(base).replace("version=1.2.3", "version = 1.2.3");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadLine(_))
        ));
        let text = good(base).replace("version=1.2.3", "version=1.2.3 ");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadLine(_))
        ));
    }

    #[test]
    fn a_manifest_too_large_is_refused() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let text = format!("{}{}", good(base), "x".repeat(MAX_MANIFEST_BYTES as usize));
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::TooLarge(_))
        ));
    }

    #[test]
    fn the_values_are_checked() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        // Another product.
        let text = good(base).replace("product=moyue-preview", "product=moyue");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "product",
                ..
            })
        ));
        // A version that is not X.Y.Z.
        for bad in ["1.2", "1.2.3.4", "01.2.3", "1.2.3a", "1.2.1000"] {
            let text = good(base).replace("version=1.2.3", &format!("version={bad}"));
            assert!(
                matches!(
                    parse_manifest(&text, &rules(base)),
                    Err(ManifestError::BadValue {
                        field: "version",
                        ..
                    })
                ),
                "{bad} should be refused"
            );
        }
        // An empty value.
        let text = good(base).replace("version=1.2.3", "version=");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadLine(_))
        ));
        // A digest that is not one.
        let text = good(base).replace(DIGEST, &"z".repeat(64));
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "sha256",
                ..
            })
        ));
        // A size that is not one.
        let text = good(base).replace("size=1024", "size=lots");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue { field: "size", .. })
        ));
        // A size over the limit.
        let text = good(base).replace("size=1024", &format!("size={}", MAX_PACKAGE_BYTES + 1));
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue { field: "size", .. })
        ));
        // An empty size.
        let text = good(base).replace("size=1024", "size=0");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue { field: "size", .. })
        ));
    }

    #[test]
    fn the_addresses_must_be_https_and_under_the_allowed_path() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        // Another host.
        let text = good(base).replace(base, "https://elsewhere.example.org/");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "package",
                ..
            })
        ));
        // Plain http on a device.
        let text = good(base).replace("https://", "http://");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "package",
                ..
            })
        ));
        // A path that walks out of the directory.
        let text = good(base).replace("package.tar.gz", "../../etc/passwd");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "package",
                ..
            })
        ));
        // An address that is not one.
        let text = good(base).replace(&format!("{base}package.tar.gz"), "not-an-address");
        assert!(matches!(
            parse_manifest(&text, &rules(base)),
            Err(ManifestError::BadValue {
                field: "package",
                ..
            })
        ));
    }

    // ------------------------------------------------------------- the version

    #[test]
    fn versions_are_compared_as_numbers() {
        assert_eq!(
            Version::parse("1.2.3").unwrap(),
            Version::parse("1.2.3").unwrap()
        );
        assert!(Version::parse("1.10.0").unwrap() > Version::parse("1.9.9").unwrap());
        assert!(Version::parse("2.0.0").unwrap() > Version::parse("1.999.999").unwrap());
        assert!(Version::parse("0.0.1").unwrap() > Version::parse("0.0.0").unwrap());
        assert_eq!(Version::parse("1.2.3").unwrap().to_string(), "1.2.3");
    }

    #[test]
    fn a_manifest_says_whether_there_is_an_update() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let m = parse_manifest(&good(base), &rules(base)).unwrap();
        // The server has a newer version.
        assert_eq!(m.compare("1.2.2").unwrap(), Check::Available);
        assert_eq!(m.compare("0.9.9").unwrap(), Check::Available);
        // The same version.
        assert_eq!(m.compare("1.2.3").unwrap(), Check::UpToDate);
        // The server has an older version: no downgrade.
        assert_eq!(m.compare("1.2.4").unwrap(), Check::Downgrade);
        assert_eq!(m.compare("2.0.0").unwrap(), Check::Downgrade);
        // An installed version that is not one.
        assert!(matches!(
            m.compare("1.2"),
            Err(ManifestError::BadValue {
                field: "version",
                ..
            })
        ));
    }

    #[test]
    fn a_manifest_is_for_one_device_and_one_firmware() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let m = parse_manifest(&good(base), &rules(base)).unwrap();
        assert!(m.matches("oasis2", "ko2-5.16", "5.16"));
        // A newer firmware of the same line is covered.
        assert!(m.matches("oasis2", "ko2-5.16", "5.16.2"));
        // Another device, another channel, another firmware.
        assert!(!m.matches("pw4", "ko2-5.16", "5.16"));
        assert!(!m.matches("oasis2", "ko2-5.13", "5.16"));
        assert!(!m.matches("oasis2", "ko2-5.16", "5.13"));
        assert!(!m.matches("oasis2", "ko2-5.16", "5.17"));
    }

    // ------------------------------------------------------------ the download

    /// A client that may talk to anything.
    fn client() -> Client {
        Client::new(Policy::open(), Timeouts::quick(Duration::from_secs(10)))
    }

    use crate::policy::Policy;

    /// The digest of `bytes`.
    fn digest_of(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex_lower(&hasher.finalize())
    }

    /// A manifest of a package the server holds.
    fn manifest_of(server: &Server, bytes: &[u8]) -> (Manifest, Rules) {
        let base = format!("http://127.0.0.1:{}/ota/ko2-5.16/", server.url("/").port());
        let text = manifest_text(&base, "1.2.3", &digest_of(bytes), bytes.len() as u64);
        let rules = Rules {
            prefix: Some(base),
            allow_http: true,
        };
        let m = parse_manifest(&text, &rules).unwrap();
        (m, rules)
    }

    #[test]
    fn the_manifest_is_read_over_http() {
        let server = Server::start(|_| Reply::text(200, "MOYUE-OTA-1\n"));
        let rules = Rules {
            prefix: Some(format!("http://127.0.0.1:{}/", server.url("/").port())),
            allow_http: true,
        };
        let url = server.url("/manifest.txt");
        // The server answers something that is not a manifest.
        let e = fetch_manifest(&client(), &url, &rules).unwrap_err();
        assert!(matches!(e, OtaError::Manifest(_)));
    }

    #[test]
    fn the_manifest_is_read_over_http_when_it_is_one() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let text = good(base);
        let server = Server::start(move |_| Reply::text(200, &text));
        let rules = Rules {
            prefix: Some(base.to_string()),
            allow_http: true,
        };
        let url = server.url("/manifest.txt");
        let m = fetch_manifest(&client(), &url, &rules).unwrap();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.compare("1.2.2").unwrap(), Check::Available);
    }

    #[test]
    fn the_package_is_downloaded_and_verified() {
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let served = payload.clone();
        let server = Server::start(move |_| Reply::typed(200, "application/gzip", &served));
        let (manifest, _) = manifest_of(&server, &payload);
        let dir = std::env::temp_dir().join("aristotle-ota-verified");
        let _ = std::fs::remove_dir_all(&dir);
        let target = PackageTarget::in_dir(&dir, "package.tar.gz");
        let cancel = AtomicBool::new(false);
        let mut seen: Vec<OtaProgress> = Vec::new();
        let path = download_package(&client(), &manifest, &target, &cancel, &mut |p| {
            seen.push(p)
        })
        .unwrap();

        // The verified package is under its final name.
        assert_eq!(path, target.verified);
        assert!(target.verified.exists());
        assert!(!target.part.exists());
        assert_eq!(std::fs::read(&path).unwrap(), payload);

        // The progress went from the start to the end, and never back.
        let percents: Vec<u8> = seen.iter().map(|p| p.percent).collect();
        assert!(percents.windows(2).all(|w| w[0] <= w[1]), "{percents:?}");
        assert_eq!(percents.first().copied(), Some(0));
        assert_eq!(percents.last().copied(), Some(100));
        // The stages are the ones the screen shows.
        let stages: Vec<OtaStage> = seen.iter().map(|p| p.stage).collect();
        assert!(stages.contains(&OtaStage::Downloading));
        assert!(stages.contains(&OtaStage::Verifying));
        assert!(stages.contains(&OtaStage::Staging));
        assert_eq!(stages.last(), Some(&OtaStage::ReadyToApply));
        // The bytes are counted.
        assert_eq!(seen.last().unwrap().downloaded_bytes, payload.len() as u64);
        assert_eq!(seen.last().unwrap().total_bytes, Some(payload.len() as u64));
    }

    #[test]
    fn a_package_with_the_wrong_digest_is_deleted() {
        let payload = b"the package that is not the one".to_vec();
        let served = payload.clone();
        let server = Server::start(move |_| Reply::typed(200, "application/gzip", &served));
        let (mut manifest, _) = manifest_of(&server, &payload);
        // The manifest names another digest.
        manifest.sha256 = digest_of(b"another package");
        let dir = std::env::temp_dir().join("aristotle-ota-baddigest");
        let _ = std::fs::remove_dir_all(&dir);
        let target = PackageTarget::in_dir(&dir, "package.tar.gz");
        let cancel = AtomicBool::new(false);
        let e = download_package(&client(), &manifest, &target, &cancel, &mut |_| {}).unwrap_err();
        assert!(matches!(e, OtaError::DigestMismatch { .. }));
        // Nothing is left behind.
        assert!(!target.part.exists());
        assert!(!target.verified.exists());
    }

    #[test]
    fn a_package_with_the_wrong_size_is_deleted() {
        let payload = b"the package that is too short".to_vec();
        let served = payload.clone();
        let server = Server::start(move |_| Reply::typed(200, "application/gzip", &served));
        let (mut manifest, _) = manifest_of(&server, &payload);
        manifest.size = payload.len() as u64 + 10;
        let dir = std::env::temp_dir().join("aristotle-ota-badsize");
        let _ = std::fs::remove_dir_all(&dir);
        let target = PackageTarget::in_dir(&dir, "package.tar.gz");
        let cancel = AtomicBool::new(false);
        let e = download_package(&client(), &manifest, &target, &cancel, &mut |_| {}).unwrap_err();
        assert!(matches!(e, OtaError::SizeMismatch { .. }));
        assert!(!target.part.exists());
        assert!(!target.verified.exists());
    }

    #[test]
    fn a_manifest_of_another_device_is_refused() {
        let base = "https://me.chen-cn.top/moyue/ota/ko2-5.16/";
        let text = good(base);
        let server = Server::start(move |_| Reply::text(200, &text));
        let rules = Rules {
            prefix: Some(base.to_string()),
            allow_http: true,
        };
        let url = server.url("/manifest.txt");
        // The device is the one the manifest names.
        let m = fetch_for_device(&client(), &url, &rules, "oasis2", "ko2-5.16", "5.16").unwrap();
        assert_eq!(m.device, "oasis2");
        // Another model: refused.
        let e = fetch_for_device(&client(), &url, &rules, "pw4", "ko2-5.16", "5.16").unwrap_err();
        assert!(matches!(e, OtaError::WrongDevice { .. }), "{e}");
        // Another channel: refused.
        let e =
            fetch_for_device(&client(), &url, &rules, "oasis2", "ko2-5.13", "5.16").unwrap_err();
        assert!(matches!(e, OtaError::WrongDevice { .. }), "{e}");
        // Another firmware: refused.
        let e =
            fetch_for_device(&client(), &url, &rules, "oasis2", "ko2-5.16", "5.13").unwrap_err();
        assert!(matches!(e, OtaError::WrongDevice { .. }), "{e}");
    }

    #[test]
    fn the_progress_grows_with_the_bytes() {
        assert_eq!(OtaProgress::percent_of(0, Some(100)), 0);
        assert_eq!(OtaProgress::percent_of(50, Some(100)), 50);
        assert_eq!(OtaProgress::percent_of(100, Some(100)), 100);
        // More bytes than the size are still a full bar.
        assert_eq!(OtaProgress::percent_of(200, Some(100)), 100);
        // Without a size there is no share to show.
        assert_eq!(OtaProgress::percent_of(200, None), 0);
    }
}
