// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which Kindle this is, and which EPDC update ABI it speaks.
//!
//! The model comes from the serial number the kernel keeps in `/proc/usid`:
//! the device code in it says which Kindle this is. Nothing is probed for.
//! The EPDC driver answers an update request in the wrong layout with
//! EINVAL, so a guess would leave the panel blank rather than say so.

use std::fs;
use std::io;

use aristotle_screen::model::{self, Model};

/// Where the kernel keeps the device's serial number.
const USID: &str = "/proc/usid";

/// The EPDC update ABI of the i.MX EPDC driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// The Paperwhite 4: `mxcfb_update_data_rex`, 80 bytes.
    Rex,
    /// The Oasis 2: `mxcfb_update_data_zelda`, 88 bytes.
    Zelda,
}

impl Platform {
    /// The ABI of a known model.
    pub fn of(model: Model) -> Platform {
        match model {
            Model::Paperwhite4 => Platform::Rex,
            Model::Oasis2 => Platform::Zelda,
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
