// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which Kindle this is, and which EPDC update ABI it speaks.
//!
//! The model comes from the serial number the kernel keeps in `/proc/usid`:
//! the device code in it says which Kindle this is. Nothing is probed for.
//! The EPDC driver answers an update request in the wrong layout with
//! EINVAL, so a guess would leave the panel blank rather than say so.

use std::fs;
use std::io;

use aristotle_screen::model::{self, KoboModel, Model};

/// Where the kernel keeps the device's serial number.
const USID: &str = "/proc/usid";

/// The firmware tag Nickel keeps on the user partition. Its fields are
/// separated by commas: 1 serial number, 3 firmware, 6 model number. Only the
/// last two are read; see [`KoboVersionTag`].
const KOBO_VERSION_TAG: &str = "/mnt/onboard/.kobo/version";

/// The EPDC update ABI of the i.MX/MTK EPDC driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// The older Kindle driver: `mxcfb_update_data`, 72 bytes.
    K51,
    /// The Paperwhite 4: `mxcfb_update_data_rex`, 80 bytes.
    Rex,
    /// The Oasis 2: `mxcfb_update_data_zelda`, 88 bytes.
    Zelda,
    /// The Paperwhite 5 and Kindle Scribe: `mxcfb_update_data_mtk`, 96 bytes.
    Mtk,
    /// A Kobo i.MX EPDC panel. Label only: Kobo requests are built by
    /// [`crate::kobo`] and must not be sent with any Kindle layout.
    KoboImx,
}

impl Platform {
    /// The ABI of a known model.
    pub fn of(model: Model) -> Platform {
        match model {
            Model::Paperwhite1
            | Model::Paperwhite2
            | Model::Paperwhite3
            | Model::Voyage
            | Model::Oasis1
            | Model::KindleBasic2 => Platform::K51,
            Model::Paperwhite4 | Model::KindleBasic3 => Platform::Rex,
            Model::Oasis2 | Model::Oasis3 => Platform::Zelda,
            Model::Paperwhite5
            | Model::Paperwhite5Se
            | Model::Paperwhite6
            | Model::KindleBasic4
            | Model::KindleBasic5
            | Model::KindleScribe
            | Model::KindleScribe2
            | Model::KindleScribe3
            | Model::ColorSoft
            | Model::ScribeColorSoft => Platform::Mtk,
        }
    }

    /// The model of this device, from its serial number.
    pub fn model() -> io::Result<Model> {
        let serial = serial_number()?;
        Model::from_serial(&serial).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unknown Kindle: device code {:?}",
                    model::device_code(&serial)
                ),
            )
        })
    }

    /// The ABI of this device, from its serial number.
    pub fn detect() -> io::Result<Platform> {
        Ok(Platform::of(Platform::model()?))
    }
}

/// The serial number of this device: the first line of `/proc/usid`.
pub fn serial_number() -> io::Result<String> {
    let text = fs::read_to_string(USID)?;
    Ok(text.lines().next().unwrap_or_default().trim().to_string())
}

/// A detected Kobo: its model and firmware. Deliberately carries no serial
/// number, so it is safe to log and to put in a report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KoboIdentity {
    /// The Kobo model.
    pub model: KoboModel,
    /// The firmware version from the tag's third field, when it is present.
    pub firmware: Option<String>,
}

/// The two fields of `/mnt/onboard/.kobo/version` this build uses.
///
/// The first field is the device's serial number. [`Self::parse`] never
/// retains it: the parser splits the line but keeps only fields 3 and 6.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KoboVersionTag {
    /// The firmware version, third field.
    pub firmware: Option<String>,
    /// The model number, sixth field, without leading zeros.
    pub model_number: Option<String>,
}

impl KoboVersionTag {
    /// Parse a version tag. A tag too short to hold a model number is an
    /// error; the error reports the field count, never the contents.
    pub fn parse(text: &str) -> io::Result<Self> {
        let fields: Vec<&str> = text.trim_end().split(',').collect();
        if fields.len() < 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Kobo version tag has {} fields, needs 6", fields.len()),
            ));
        }
        let field = |i: usize| {
            let value = fields[i].trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        Ok(Self {
            firmware: field(2),
            model_number: field(5).map(|n| n.trim_start_matches(['0', '-']).to_string()),
        })
    }
}

/// Detect a Kobo from the sources the stock system exposes.
///
/// The product name comes from `PRODUCT`, the variable Plato reads
/// (`device.rs`); the model number and firmware come from the version tag.
/// `Ok(None)` means this is not a Kobo, so the caller keeps the Kindle path.
/// The serial-number field is never read out of the tag or logged.
pub fn detect_kobo() -> io::Result<Option<KoboIdentity>> {
    let product = std::env::var("PRODUCT")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let tag = match fs::read_to_string(KOBO_VERSION_TAG) {
        Ok(text) => Some(KoboVersionTag::parse(&text)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let model = KoboModel::from_sources(
        product.as_deref(),
        tag.as_ref().and_then(|t| t.model_number.as_deref()),
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    Ok(model.map(|model| KoboIdentity {
        model,
        firmware: tag.and_then(|t| t.firmware),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_families_use_their_own_update_layout() {
        assert_eq!(Platform::of(Model::KindleBasic2), Platform::K51);
        assert_eq!(Platform::of(Model::Voyage), Platform::K51);
        assert_eq!(Platform::of(Model::Paperwhite4), Platform::Rex);
        assert_eq!(Platform::of(Model::KindleBasic3), Platform::Rex);
        assert_eq!(Platform::of(Model::Oasis2), Platform::Zelda);
        assert_eq!(Platform::of(Model::Oasis3), Platform::Zelda);
        assert_eq!(Platform::of(Model::Paperwhite5), Platform::Mtk);
        assert_eq!(Platform::of(Model::KindleScribe), Platform::Mtk);
    }

    #[test]
    fn every_known_model_has_a_platform() {
        for model in Model::ALL {
            let platform = Platform::of(*model);
            assert!(
                matches!(
                    platform,
                    Platform::K51
                        | Platform::Rex
                        | Platform::Zelda
                        | Platform::Mtk
                        | Platform::KoboImx
                ),
                "{model:?}"
            );
        }
    }

    #[test]
    fn the_kobo_version_tag_reads_only_firmware_and_model_number() {
        // Field 1 is a fake serial: it must not survive parsing anywhere.
        let text = "SN000000000001,4.38.21908,4.38.21908,4.38.21908,0,00000000-0000-0000-0000-000000000376\n";
        let tag = KoboVersionTag::parse(text).unwrap();
        assert_eq!(tag.firmware.as_deref(), Some("4.38.21908"));
        assert_eq!(tag.model_number.as_deref(), Some("376"));
        let debug = format!("{tag:?}");
        assert!(!debug.contains("SN000000000001"));
        assert!(!debug.contains("000000000001"));

        let short = KoboVersionTag::parse("SN123,4.38").unwrap_err();
        assert_eq!(short.kind(), io::ErrorKind::InvalidData);
        assert!(!short.to_string().contains("SN123"));
    }

    #[test]
    fn kobo_detection_uses_product_and_model_number_and_rejects_mismatch() {
        assert_eq!(
            KoboModel::from_sources(Some("nova"), Some("376")),
            Ok(Some(KoboModel::ClaraHd))
        );
        assert_eq!(
            KoboModel::from_sources(Some("io"), None),
            Ok(Some(KoboModel::Libra2))
        );
        assert!(KoboModel::from_sources(Some("nova"), Some("388")).is_err());
        assert_eq!(KoboModel::from_sources(Some("kindle"), None), Ok(None));
        assert_eq!(KoboModel::from_sources(None, None), Ok(None));
    }

    #[test]
    fn the_kobo_identity_has_no_serial_field() {
        let identity = KoboIdentity {
            model: KoboModel::ClaraHd,
            firmware: Some("4.38.21908".to_string()),
        };
        assert_eq!(identity.model.model_code(), "N249");
        let text = format!("{identity:?}");
        assert!(!text.to_lowercase().contains("serial"));
    }
}
