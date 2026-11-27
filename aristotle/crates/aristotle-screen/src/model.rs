// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Kindle models Aristotle runs on: how to recognise one from its serial
//! number, and what its panel, touchscreen and page-turn keys are.
//!
//! The model comes from the device code in the serial number (`/proc/usid` on
//! the device): the three characters in the middle of it. Serials that start
//! with `B` or `9` use the older scheme, with the code in characters 3-4; the
//! others carry it in characters 4-6. The code tables are KOReader's
//! (`frontend/device/kindle/device.lua`), kept in the same model groups as
//! `scripts/kindle-models.json`.
//!
//! Each entry is the full set of variants KOReader ships for that model, not
//! only the single code one test device reports. A package built for a model
//! therefore matches every variant of that model; the model's firmware and
//! driver still decide the update ABI and the runtime geometry.

/// A Kindle model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Model {
    /// The Paperwhite 1 (2012): the legacy mxcfb ("K51") ABI.
    Paperwhite1,
    /// The Paperwhite 2 (2013): the legacy mxcfb ("K51") ABI.
    Paperwhite2,
    /// The Paperwhite 3 (2015): the legacy mxcfb ("K51") ABI.
    Paperwhite3,
    /// The Paperwhite 4 (2018): the "Rex" platform.
    Paperwhite4,
    /// The Paperwhite 5 (2021): the MediaTek ("MTK") platform.
    Paperwhite5,
    /// The Paperwhite 5 Signature Edition (2021): the MTK platform.
    Paperwhite5Se,
    /// The Paperwhite 6 (2024): the MTK platform.
    Paperwhite6,
    /// The Voyage (2014): the legacy mxcfb ("K51") ABI.
    Voyage,
    /// The Oasis 1 (2016): the legacy mxcfb ("K51") ABI.
    Oasis1,
    /// The Oasis 2 (2017): the "Zelda" platform.
    Oasis2,
    /// The Oasis 3 (2019): the same "Zelda" platform as the Oasis 2.
    Oasis3,
    /// The Basic 2 (2016), also known as the Kindle Touch 3 (KT3).
    KindleBasic2,
    /// The Basic 3 (2019), also known as the Kindle 10; "Rex".
    KindleBasic3,
    /// The Basic 4 (2022): the MTK platform.
    KindleBasic4,
    /// The Basic 5 (2024): the MTK platform.
    KindleBasic5,
    /// The Kindle Scribe (2022): the MTK platform.
    KindleScribe,
    /// The Kindle Scribe 2 (2024): the same MTK backend as the Scribe.
    KindleScribe2,
    /// The Kindle Scribe 3 (2025): the same MTK backend as the Scribe.
    KindleScribe3,
    /// The Kindle Colorsoft (2024): MTK with a colour panel.
    ColorSoft,
    /// The Kindle Scribe Colorsoft (2025): MTK with a colour panel.
    ScribeColorSoft,
}

impl Model {
    /// Every model this build recognises.
    pub const ALL: &'static [Model] = &[
        Model::Paperwhite1,
        Model::Paperwhite2,
        Model::Paperwhite3,
        Model::Paperwhite4,
        Model::Paperwhite5,
        Model::Paperwhite5Se,
        Model::Paperwhite6,
        Model::Voyage,
        Model::Oasis1,
        Model::Oasis2,
        Model::Oasis3,
        Model::KindleBasic2,
        Model::KindleBasic3,
        Model::KindleBasic4,
        Model::KindleBasic5,
        Model::KindleScribe,
        Model::KindleScribe2,
        Model::KindleScribe3,
        Model::ColorSoft,
        Model::ScribeColorSoft,
    ];

    /// The model with this device code.
    pub fn from_device_code(code: &str) -> Option<Model> {
        MODEL_CODES
            .iter()
            .find(|(_, codes)| codes.contains(&code))
            .map(|(model, _)| *model)
    }

    /// The model of a serial number, read from its device code.
    pub fn from_serial(serial: &str) -> Option<Model> {
        Model::from_device_code(device_code(serial)?)
    }

    /// The device codes of the model, all variants included.
    pub fn device_codes(self) -> &'static [&'static str] {
        MODEL_CODES
            .iter()
            .find(|(model, _)| *model == self)
            .map(|(_, codes)| *codes)
            .unwrap_or(&[])
    }

    /// The model as the About screen names it.
    pub const fn name(self) -> &'static str {
        match self {
            Model::Paperwhite1 => "Kindle Paperwhite 1",
            Model::Paperwhite2 => "Kindle Paperwhite 2",
            Model::Paperwhite3 => "Kindle Paperwhite 3",
            Model::Paperwhite4 => "Kindle Paperwhite 4",
            Model::Paperwhite5 => "Kindle Paperwhite 5",
            Model::Paperwhite5Se => "Kindle Paperwhite 5 Signature Edition",
            Model::Paperwhite6 => "Kindle Paperwhite 6",
            Model::Voyage => "Kindle Voyage",
            Model::Oasis1 => "Kindle Oasis",
            Model::Oasis2 => "Kindle Oasis 2",
            Model::Oasis3 => "Kindle Oasis 3",
            Model::KindleBasic2 => "Kindle Basic 2",
            Model::KindleBasic3 => "Kindle Basic 3",
            Model::KindleBasic4 => "Kindle Basic 4",
            Model::KindleBasic5 => "Kindle Basic 5",
            Model::KindleScribe => "Kindle Scribe",
            Model::KindleScribe2 => "Kindle Scribe 2",
            Model::KindleScribe3 => "Kindle Scribe 3",
            Model::ColorSoft => "Kindle Colorsoft",
            Model::ScribeColorSoft => "Kindle Scribe Colorsoft",
        }
    }

    /// The density to use for the native Kindle UI and reader.
    ///
    /// The Kindle 10 (2019) is a 6-inch 600 x 800 panel: 167 dpi, where the
    /// Paperwhite 2 is the 212 dpi 758 x 1024 one and the rest are 300.
    pub const fn dpi(self) -> u16 {
        match self {
            Self::Paperwhite2 => 212,
            Self::KindleBasic3 => 167,
            _ => 300,
        }
    }

    /// The panel geometry of the model, in portrait.
    ///
    /// The runtime always reads the geometry from the driver; these values
    /// are the reference used to size the touchscreen by tools that run
    /// before, or without, opening the framebuffer. A field of 0 in a table
    /// means this source does not carry that value: use the driver's
    /// `FBIOGET_FSCREENINFO` / `FBIOGET_VSCREENINFO` result.
    pub const fn geometry(self) -> Geometry {
        match self {
            Model::Paperwhite1 => Geometry {
                width: 758,
                height: 1024,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
            // Measured on Paperwhite 2 (`17`, `D4`, `5A`), firmware 5.12.2.2,
            // Linux 3.0.35-lab126, `mxc_epdc_fb` 8 bpp reporting
            // `virtual_size` 768 x 4096, stride 768 and `rotate=3`.
            Model::Paperwhite2 => Geometry {
                width: 758,
                height: 1024,
                stride: 768,
                rotate: 3,
                virtual_width: 768,
                virtual_height: 4096,
            },
            Model::Paperwhite3 | Model::Paperwhite4 => Geometry {
                width: 1072,
                height: 1448,
                stride: 1088,
                rotate: 3,
                virtual_width: 1088,
                virtual_height: 6144,
            },
            Model::Paperwhite5 | Model::Paperwhite5Se => Geometry {
                width: 1236,
                height: 1648,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
            // Measured on Paperwhite 6 (`33W`, `33X`, `3H3`), firmware
            // 5.17.1.0.4 / 5.20.1.0.2 / 5.20.2.0.1, Linux 5.15.41-lab126,
            // `hwtcon_v2` 8 bpp reporting 1272 x 1696, `virtual_size`
            // 1272 x 3392, stride 1272 and `rotate=0`.
            Model::Paperwhite6 => Geometry {
                width: 1272,
                height: 1696,
                stride: 1272,
                rotate: 0,
                virtual_width: 1272,
                virtual_height: 3392,
            },
            Model::Voyage => Geometry {
                width: 1072,
                height: 1448,
                stride: 1088,
                rotate: 3,
                virtual_width: 1088,
                virtual_height: 3072,
            },
            Model::Oasis1 => Geometry {
                width: 1072,
                height: 1448,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
            Model::Oasis2 => Geometry {
                width: 1264,
                height: 1680,
                stride: 1280,
                rotate: 2,
                virtual_width: 1280,
                virtual_height: 3584,
            },
            Model::Oasis3 => Geometry {
                width: 1264,
                height: 1680,
                stride: 1280,
                rotate: 0,
                virtual_width: 1280,
                virtual_height: 3584,
            },
            // Measured on the Kindle 10 (2019): device code `0VB`, firmware
            // 5.16.2.1.1, `mxc_epdc_fb` at 8 bpp reporting `virtual_size`
            // 608 x 10752, stride 608 and `rotate=3`. The Basic 2's panel
            // reports the same numbers, so both models share this table.
            Model::KindleBasic2 | Model::KindleBasic3 => Geometry {
                width: 600,
                height: 800,
                stride: 608,
                rotate: 3,
                virtual_width: 608,
                virtual_height: 10752,
            },
            Model::KindleBasic4 | Model::KindleBasic5 => Geometry {
                width: 1072,
                height: 1448,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
            Model::KindleScribe
            | Model::KindleScribe2
            | Model::KindleScribe3
            | Model::ScribeColorSoft => Geometry {
                width: 1860,
                height: 2480,
                stride: 1872,
                rotate: 3,
                virtual_width: 1872,
                virtual_height: 4960,
            },
            Model::ColorSoft => Geometry {
                width: 1264,
                height: 1680,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
        }
    }

    /// The transform of the model's touchscreen.
    ///
    /// All supported digitizers report the framebuffer's own portrait
    /// orientation, so this is the identity for them. [`touch_transform`]
    /// works out the swap from the axis ranges a device reports. The
    /// framebuffer's rotation comes on top of this: see
    /// [`Model::touch_transform_at`].
    pub const fn touch_transform(self) -> TouchTransform {
        let _ = self;
        TouchTransform {
            swap_xy: false,
            mirror_x: false,
            mirror_y: false,
        }
    }

    /// The transform of the model's touchscreen for a framebuffer rotation.
    ///
    /// `rotate` is the rotation the driver reports, read once at startup.
    /// The touchscreen always reports the panel's own coordinates, so the
    /// transform follows the framebuffer: at 0 the two agree, and at
    /// [`ROTATE_UPSIDE_DOWN`] the framebuffer is turned around and both
    /// axes have to be mirrored. The rotations 1 and 3 (landscape) are not
    /// supported: the caller logs them and reads them as 0.
    pub const fn touch_transform_at(self, rotate: u32) -> TouchTransform {
        let base = self.touch_transform();
        let upside_down = rotate == ROTATE_UPSIDE_DOWN;
        TouchTransform {
            swap_xy: base.swap_xy,
            mirror_x: base.mirror_x || upside_down,
            mirror_y: base.mirror_y || upside_down,
        }
    }

    /// The page-turn buttons of the model, if it has any. The Paperwhite 4
    /// has none; the Oasis 2's two buttons are on its left edge.
    pub const fn page_turn_keys(self) -> Option<PageTurnKeys> {
        match self {
            // The Voyage's top button is the forward one and its bottom one
            // the back one, the other way round from the Oasis (KOReader's
            // `KindleVoyage:init`: 104 LPgBack, 109 LPgFwd).
            Model::Voyage => Some(PageTurnKeys {
                forward: 109,
                back: 104,
            }),
            // The Oasis family keeps the same two codes; its top button turns
            // the page forward and its bottom one turns it back (KOReader's
            // `KindleOasis:init`, `KindleOasis2:init`, `KindleOasis3:init`).
            Model::Oasis1 | Model::Oasis2 | Model::Oasis3 => Some(PageTurnKeys {
                forward: 104,
                back: 109,
            }),
            Model::Paperwhite1
            | Model::Paperwhite2
            | Model::Paperwhite3
            | Model::Paperwhite4
            | Model::Paperwhite5
            | Model::Paperwhite5Se
            | Model::Paperwhite6
            | Model::KindleBasic2
            | Model::KindleBasic3
            | Model::KindleBasic4
            | Model::KindleBasic5
            | Model::KindleScribe
            | Model::KindleScribe2
            | Model::KindleScribe3
            | Model::ColorSoft
            | Model::ScribeColorSoft => None,
        }
    }

    /// The page-turn buttons of the model for a framebuffer rotation.
    ///
    /// The buttons are in the panel's own orientation, so holding the device
    /// upside down swaps their places: the button that was on top is at the
    /// bottom now. Their roles swap with them, so the button the reader
    /// sees on top is the same one all the same, as on the stock interface.
    pub const fn page_turn_keys_at(self, rotate: u32) -> Option<PageTurnKeys> {
        match self.page_turn_keys() {
            None => None,
            Some(keys) => {
                if rotate == ROTATE_UPSIDE_DOWN {
                    Some(PageTurnKeys {
                        forward: keys.back,
                        back: keys.forward,
                    })
                } else {
                    Some(keys)
                }
            }
        }
    }

    /// Whether this model has dual-tone warm frontlight LEDs.
    pub const fn has_warm_light(self) -> bool {
        matches!(
            self,
            Model::Oasis3
                | Model::Paperwhite5
                | Model::Paperwhite5Se
                | Model::Paperwhite6
                | Model::KindleScribe
                | Model::KindleScribe2
                | Model::KindleScribe3
                | Model::ColorSoft
                | Model::ScribeColorSoft
        )
    }

    /// Whether this model has an ambient light sensor (auto-brightness).
    pub const fn has_als(self) -> bool {
        matches!(
            self,
            Model::Voyage | Model::Oasis2 | Model::Oasis3 | Model::Paperwhite5Se
        )
    }

    /// Whether this model has an accelerometer for automatic screen rotation.
    pub const fn has_accelerometer(self) -> bool {
        matches!(
            self,
            Model::Oasis1
                | Model::Oasis2
                | Model::Oasis3
                | Model::KindleScribe
                | Model::KindleScribe2
                | Model::KindleScribe3
                | Model::ScribeColorSoft
        )
    }
}

/// The framebuffer rotation of a device held upside down.
///
/// The Oasis 2's framebuffer follows the accelerometer: the driver turns
/// the content around when the device is held the other way up, so what
/// is drawn is the panel's own orientation turned around.
pub const ROTATE_UPSIDE_DOWN: u32 = 2;

/// The device codes of the Paperwhite 4, from KOReader's `pw4_set`.
pub const PW4_CODES: &[&str] = &[
    "0PP", "0T1", "0T2", "0T3", "0T4", "0T5", "0T6", "0T7", "0TJ", "0TK", "0TL", "0TM", "0TN",
    "102", "103", "16Q", "16R", "16S", "16T", "16U", "16V",
];

/// The device codes of the Oasis 2, from KOReader's `koa2_set`.
pub const OASIS2_CODES: &[&str] = &[
    "0LM", "0LN", "0LP", "0LQ", "0P1", "0P2", "0P6", "0P7", "0P8", "0S1", "0S2", "0S3", "0S4",
    "0S7", "0SA",
];

/// Every model with the full set of device codes KOReader lists for it.
///
/// Keep this table in step with `scripts/kindle-models.json`: the two are the
/// public and the in-binary view of the same registry.
pub const MODEL_CODES: &[(Model, &[&str])] = &[
    (Model::Paperwhite1, &["24", "1B", "1D", "1F", "1C", "20"]),
    (
        Model::Paperwhite2,
        &[
            "D4", "5A", "D5", "D6", "D7", "D8", "F2", "17", "60", "F4", "F9", "62", "61", "5F",
        ],
    ),
    (
        Model::Paperwhite3,
        &[
            "0G1", "0G2", "0G4", "0G5", "0G6", "0G7", "0KB", "0KC", "0KD", "0KE", "0KF", "0KG",
            "0LK", "0LL",
        ],
    ),
    (Model::Paperwhite4, PW4_CODES),
    (
        Model::Paperwhite5,
        &["1Q0", "1PX", "1VD", "21A", "2BJ", "2DK"],
    ),
    (Model::Paperwhite5Se, &["1LG", "219", "2BH"]),
    (
        Model::Paperwhite6,
        &[
            "33W", "33X", "346", "349", "3H3", "3H5", "3H8", "3HA", "3J5", "3JS",
        ],
    ),
    (Model::Voyage, &["13", "54", "2A", "4F", "52", "53"]),
    (Model::Oasis1, &["0GC", "0GD", "0GR", "0GS", "0GT", "0GU"]),
    (Model::Oasis2, OASIS2_CODES),
    (Model::Oasis3, &["11L", "0WQ", "0WP", "0WN", "0WM", "0WL"]),
    (Model::KindleBasic2, &["0DU", "0K9", "0KA"]),
    (
        Model::KindleBasic3,
        &["10L", "0WF", "0WG", "0WH", "0WJ", "0VB"],
    ),
    (
        Model::KindleBasic4,
        &["22D", "25T", "23A", "2AQ", "2AP", "1XH", "22C"],
    ),
    (
        Model::KindleBasic5,
        &["A89", "3L2", "3L3", "3L4", "3L5", "3L6", "3KM"],
    ),
    (
        Model::KindleScribe,
        &["27J", "2BL", "263", "227", "2BM", "23L", "23M", "270"],
    ),
    (
        Model::KindleScribe2,
        &["3V0", "3V1", "3X5", "3UV", "3X4", "3X3", "41E", "410"],
    ),
    (
        Model::KindleScribe3,
        &["4PG", "4PE", "4PL", "4F8", "4FA", "454"],
    ),
    (
        Model::ColorSoft,
        &[
            "3H2", "3H4", "3H6", "3H7", "3H9", "3JT", "3J6", "455", "456", "4EP", "34X", "3HB",
            "5R6", "5R7", "5R8", "5R9", "5RA", "5RB", "65R", "65S", "65T",
        ],
    ),
    (
        Model::ScribeColorSoft,
        &["4VX", "4PF", "4PH", "4F9", "4FB", "46P"],
    ),
];

/// The device code in a serial number, or `None` when the serial is too short
/// to hold one.
pub fn device_code(serial: &str) -> Option<&str> {
    let serial = serial.trim();
    if serial.starts_with('B') || serial.starts_with('9') {
        // The older scheme: the code is in characters 3-4.
        serial.get(2..4)
    } else {
        serial.get(3..6)
    }
}

/// A Kobo model the Kobo first release supports.
///
/// The product names and model numbers come from Plato's `device.rs` and its
/// `contrib/plato.sh` (the model-number table it takes from FBInk and
/// calibre). Both first-batch models use the i.MX EPDC ("kobo1") backend;
/// the sunxi backend of the mark-8 devices is deliberately not modelled here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KoboModel {
    /// The Clara HD (product `nova`, model number 376, mark 7).
    ClaraHd,
    /// The Libra 2 (product `io`, model number 388, mark 9).
    Libra2,
}

impl KoboModel {
    /// Every Kobo model this build recognises.
    pub const ALL: &'static [KoboModel] = &[KoboModel::ClaraHd, KoboModel::Libra2];

    /// The model with this Kobo product name (`PRODUCT` in the environment).
    pub fn from_product(product: &str) -> Result<KoboModel, KoboModelError> {
        match product.trim() {
            "nova" => Ok(KoboModel::ClaraHd),
            "io" => Ok(KoboModel::Libra2),
            other => Err(KoboModelError::UnknownProduct(other.to_string())),
        }
    }

    /// The model with this Kobo model number, with or without leading zeros.
    ///
    /// Known models outside the first release are rejected as unsupported,
    /// with the backend they would need, instead of being treated as a
    /// Kindle or guessed at.
    pub fn from_model_number(number: &str) -> Result<KoboModel, KoboModelError> {
        let number = number.trim().trim_start_matches('0');
        match number {
            "376" => Ok(KoboModel::ClaraHd),
            "388" => Ok(KoboModel::Libra2),
            // The mark-8 devices use the sunxi "kobo2" backend.
            "383" | "387" | "389" => Err(KoboModelError::UnsupportedModel {
                model_number: number.to_string(),
                backend: KoboBackend::Sunxi,
            }),
            // Other i.MX EPDC Kobos would need their own waveform table and
            // panel checks; they are not enabled in this release.
            "371" | "372" | "374" | "375" | "377" | "380" | "382" | "384" | "386" | "391"
            | "393" | "395" => Err(KoboModelError::UnsupportedModel {
                model_number: number.to_string(),
                backend: KoboBackend::ImxEpdc,
            }),
            other => Err(KoboModelError::UnknownModelNumber(other.to_string())),
        }
    }

    /// The model from whichever sources are available.
    ///
    /// The product name is what Nickel exports and Plato reads; the model
    /// number is the sixth field of `/mnt/onboard/.kobo/version`. When both
    /// are present they must agree: a mismatch is an error, not a guess.
    /// `Ok(None)` means neither source identified a Kobo.
    pub fn from_sources(
        product: Option<&str>,
        model_number: Option<&str>,
    ) -> Result<Option<KoboModel>, KoboModelError> {
        let product = product.map(str::trim).filter(|p| !p.is_empty());
        let model_number = model_number.map(str::trim).filter(|n| !n.is_empty());
        let by_product = match product {
            Some(p) => match KoboModel::from_product(p) {
                Ok(m) => Some(m),
                Err(KoboModelError::UnknownProduct(_)) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let by_number = match model_number {
            Some(n) => Some(KoboModel::from_model_number(n)?),
            None => None,
        };
        match (by_product, by_number) {
            (Some(a), Some(b)) if a != b => Err(KoboModelError::Mismatch {
                product: product.unwrap_or_default().to_string(),
                model_number: model_number.unwrap_or_default().to_string(),
            }),
            (Some(m), _) | (_, Some(m)) => Ok(Some(m)),
            (None, None) => Ok(None),
        }
    }

    /// The model as the About screen names it.
    pub const fn name(self) -> &'static str {
        match self {
            KoboModel::ClaraHd => "Kobo Clara HD",
            KoboModel::Libra2 => "Kobo Libra 2",
        }
    }

    /// The Kobo product name (`nova`, `io`).
    pub const fn product(self) -> &'static str {
        match self {
            KoboModel::ClaraHd => "nova",
            KoboModel::Libra2 => "io",
        }
    }

    /// The four-character Kobo model code (`N249`, `N418`).
    pub const fn model_code(self) -> &'static str {
        match self {
            KoboModel::ClaraHd => "N249",
            KoboModel::Libra2 => "N418",
        }
    }

    /// The model number from the firmware tag (376, 388).
    pub const fn model_number(self) -> &'static str {
        match self {
            KoboModel::ClaraHd => "376",
            KoboModel::Libra2 => "388",
        }
    }

    /// The package model id used by the download centre and the installer.
    pub const fn model_id(self) -> &'static str {
        match self {
            KoboModel::ClaraHd => "kobo-clara-hd",
            KoboModel::Libra2 => "kobo-libra-2",
        }
    }

    /// The panel geometry, in portrait. Zero stride means "read the driver's".
    pub const fn geometry(self) -> Geometry {
        match self {
            KoboModel::ClaraHd => Geometry {
                width: 1072,
                height: 1448,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
            KoboModel::Libra2 => Geometry {
                width: 1264,
                height: 1680,
                stride: 0,
                rotate: 0,
                virtual_width: 0,
                virtual_height: 0,
            },
        }
    }

    /// The panel density in dots per inch.
    pub const fn dpi(self) -> u16 {
        300
    }

    /// The EPDC update backend. Both first-batch models use i.MX EPDC.
    pub const fn backend(self) -> KoboBackend {
        KoboBackend::ImxEpdc
    }

    /// The driver's model mark: 7 on the Clara HD, 9 on the Libra 2.
    ///
    /// It selects the update layout (mark 7 and up use the 72-byte v2
    /// request) and the waveform table (mark 9 adds GCK16/GLKW16).
    pub const fn mark(self) -> u8 {
        match self {
            KoboModel::ClaraHd => 7,
            KoboModel::Libra2 => 9,
        }
    }

    /// The digitizer protocol, from Plato's `Device::new`.
    pub const fn touch_protocol(self) -> KoboProtocol {
        match self {
            KoboModel::ClaraHd => KoboProtocol::MultiB,
            KoboModel::Libra2 => KoboProtocol::MultiC,
        }
    }

    /// The transform of the model's touchscreen.
    ///
    /// Plato's Kobo parser applies the rotation and mirroring schemes to the
    /// raw digitizer coordinates. This release keeps the identity mapping
    /// (the framebuffer rotation is passed separately) and only mirrors both
    /// axes for the upside-down rotation; the exact Kobo mirroring schemes
    /// are not verified on hardware yet.
    pub const fn touch_transform_at(self, rotate: u32) -> TouchTransform {
        let upside_down = rotate == ROTATE_UPSIDE_DOWN;
        TouchTransform {
            swap_xy: false,
            mirror_x: upside_down,
            mirror_y: upside_down,
        }
    }

    /// The page-turn buttons, if the model has any. The Libra 2 has two on
    /// its right edge; the Clara HD has none (Plato's
    /// `has_page_turn_buttons`). Codes are Linux `KEY_BACKWARD`/`KEY_FORWARD`.
    pub const fn page_turn_keys(self) -> Option<PageTurnKeys> {
        match self {
            KoboModel::ClaraHd => None,
            KoboModel::Libra2 => Some(PageTurnKeys {
                forward: 194,
                back: 193,
            }),
        }
    }

    /// Both Clara HD and Libra 2 carry ComfortLight PRO warm frontlight LEDs.
    pub const fn has_warm_light(self) -> bool {
        matches!(self, KoboModel::ClaraHd | KoboModel::Libra2)
    }

    /// Neither Clara HD nor Libra 2 has an ambient light sensor.
    pub const fn has_als(self) -> bool {
        false
    }

    /// Libra 2 carries an accelerometer; Clara HD does not.
    pub const fn has_accelerometer(self) -> bool {
        matches!(self, KoboModel::Libra2)
    }
}

/// The EPDC backend a Kobo model needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KoboBackend {
    /// The i.MX EPDC backend of Plato's `KoboFramebuffer1` (mark 7 to 10).
    ImxEpdc,
    /// The sunxi backend of Plato's `KoboFramebuffer2` (mark 8).
    Sunxi,
}

/// A Kobo digitizer protocol, as differentiated by Plato's input parser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KoboProtocol {
    /// Releases a contact that is absent from the report; pressure does not
    /// signal the release (Clara HD).
    MultiB,
    /// Releases a contact when its pressure reaches zero (Libra 2).
    MultiC,
}

/// Why a Kobo model number or product name was rejected.
///
/// The variants carry model codes only. A serial number or the raw version
/// tag never reaches these values, so they are safe to log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KoboModelError {
    /// The `PRODUCT` value is not a Kobo product this build knows.
    UnknownProduct(String),
    /// The model number is not in any Kobo table.
    UnknownModelNumber(String),
    /// A known Kobo model that this release does not support, and the
    /// backend it would need.
    UnsupportedModel {
        model_number: String,
        backend: KoboBackend,
    },
    /// `PRODUCT` and the firmware model number disagree.
    Mismatch {
        product: String,
        model_number: String,
    },
}

impl std::fmt::Display for KoboModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KoboModelError::UnknownProduct(product) => {
                write!(f, "unknown Kobo product {product:?}")
            }
            KoboModelError::UnknownModelNumber(number) => {
                write!(f, "unknown Kobo model number {number:?}")
            }
            KoboModelError::UnsupportedModel {
                model_number,
                backend,
            } => write!(
                f,
                "Kobo model {model_number} is not supported by this release (it needs the {backend:?} backend)"
            ),
            KoboModelError::Mismatch {
                product,
                model_number,
            } => write!(
                f,
                "Kobo product {product:?} and model number {model_number:?} disagree"
            ),
        }
    }
}

impl std::error::Error for KoboModelError {}

/// The panel geometry of a model, in portrait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The visible width in pixels.
    pub width: u32,
    /// The visible height in pixels.
    pub height: u32,
    /// The bytes per row of the framebuffer. 0 when the reference table does
    /// not carry it; the driver's value is authoritative.
    pub stride: u32,
    /// The driver's rotation, `FBIOGET_VSCREENINFO`'s `rotate`.
    pub rotate: u32,
    /// The width of the virtual framebuffer.
    pub virtual_width: u32,
    /// The height of the virtual framebuffer.
    pub virtual_height: u32,
}

impl Geometry {
    /// Turns the driver's `xres`, `yres` and `line_length` into the logical
    /// portrait screen the app draws.
    ///
    /// Some drivers report `xres`/`yres` in their native landscape order
    /// while the content is rotated: the Scribe is the case that matters
    /// here. Using `xres` as the row width then makes every row wider than
    /// `line_length`, so a full-frame blit writes nothing and a full-screen
    /// update falls outside the panel. The short side is the portrait width;
    /// the long side is the height.
    pub fn normalize(xres: u32, yres: u32, line_length: u32) -> Result<(u32, u32), &'static str> {
        let (width, height) = (xres.min(yres), xres.max(yres));
        if width == 0 || height == 0 {
            return Err("framebuffer reports an empty screen");
        }
        if line_length < width {
            return Err("framebuffer stride is narrower than the visible width");
        }
        Ok((width, height))
    }
}

/// How a touchscreen's axes map onto the framebuffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchTransform {
    /// The touchscreen's X axis is the framebuffer's Y axis.
    pub swap_xy: bool,
    /// The touchscreen's X axis runs the other way.
    pub mirror_x: bool,
    /// The touchscreen's Y axis runs the other way.
    pub mirror_y: bool,
}

/// The page-turn buttons of a model, as evdev key codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageTurnKeys {
    /// The key that turns the page forward.
    pub forward: u16,
    /// The key that turns the page back.
    pub back: u16,
}

/// The transform for a touchscreen that reports these axis ranges.
///
/// A digitizer whose X axis is the longer one describes the panel in its own
/// orientation, which is not the framebuffer's: the axes are then swapped.
/// The Paperwhite 4's Goodix, the Oasis 2's i2c controller and the Scribe's
/// touch device all report the framebuffer's portrait orientation, so this
/// is the identity for them; it only differs from the model's own transform
/// on a device that reports otherwise.
pub const fn touch_transform(
    screen_width: u32,
    screen_height: u32,
    x_max: i32,
    y_max: i32,
) -> TouchTransform {
    TouchTransform {
        swap_xy: screen_height > screen_width && x_max > y_max,
        mirror_x: false,
        mirror_y: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paperwhite_trial_models_use_their_native_density() {
        assert_eq!(Model::from_device_code("17").unwrap().dpi(), 212);
        assert_eq!(Model::from_device_code("0G1").unwrap().dpi(), 300);
        assert_eq!(Model::from_device_code("33X").unwrap().dpi(), 300);
    }

    #[test]
    fn the_kindle_10_is_the_six_inch_167_dpi_panel() {
        let model = Model::from_device_code("0VB").unwrap();
        assert_eq!(model, Model::KindleBasic3);
        assert_eq!(model.dpi(), 167);
        // The numbers the device reported for firmware 5.16.2.1.1.
        let g = model.geometry();
        assert_eq!((g.width, g.height), (600, 800));
        assert_eq!((g.stride, g.rotate), (608, 3));
        assert_eq!((g.virtual_width, g.virtual_height), (608, 10752));
    }

    #[test]
    fn device_code_is_in_the_middle_of_the_serial() {
        assert_eq!(device_code("XYZ0PP0123456789"), Some("0PP"));
        assert_eq!(device_code("XYZ0LN0123456789"), Some("0LN"));
        // The older scheme puts the code in characters 3-4.
        assert_eq!(device_code("B00ABCDEFGH"), Some("0A"));
        assert_eq!(device_code("900ABCDEFGH"), Some("0A"));
        // A serial too short to hold a code has none.
        assert_eq!(device_code("XY"), None);
        assert_eq!(device_code(""), None);
    }

    #[test]
    fn the_pw4_is_told_apart_from_the_oasis_2() {
        assert_eq!(Model::from_device_code("0PP"), Some(Model::Paperwhite4));
        assert_eq!(Model::from_device_code("16V"), Some(Model::Paperwhite4));
        assert_eq!(Model::from_device_code("0LN"), Some(Model::Oasis2));
        assert_eq!(Model::from_device_code("0SA"), Some(Model::Oasis2));
        // The Paperwhite 5 is a model of its own now.
        assert_eq!(Model::from_device_code("1Q0"), Some(Model::Paperwhite5));
        // Anything else is none of ours.
        assert_eq!(Model::from_device_code("9ZZ"), None);
        assert_eq!(Model::from_device_code(""), None);
    }

    #[test]
    fn the_model_comes_from_the_serial_number() {
        assert_eq!(
            Model::from_serial("XYZ0PP0123456789"),
            Some(Model::Paperwhite4)
        );
        assert_eq!(Model::from_serial("XYZ0LN0123456789"), Some(Model::Oasis2));
        assert_eq!(
            Model::from_serial("XYZ1Q00123456789"),
            Some(Model::Paperwhite5)
        );
        assert_eq!(Model::from_serial("XYZ9ZZ0123456789"), None);
        // The older scheme's two-character code is in no table.
        assert_eq!(Model::from_serial("B00ABCDEFGH"), None);
    }

    #[test]
    fn the_models_name_themselves() {
        assert_eq!(Model::Paperwhite4.name(), "Kindle Paperwhite 4");
        assert_eq!(Model::Oasis2.name(), "Kindle Oasis 2");
    }

    #[test]
    fn the_codes_do_not_overlap() {
        for code in PW4_CODES {
            assert!(!OASIS2_CODES.contains(code), "{code} is in both tables");
        }
        assert_eq!(PW4_CODES.len() + OASIS2_CODES.len(), 36);
    }

    #[test]
    fn the_oasis_2_is_bigger_than_the_pw4() {
        let pw4 = Model::Paperwhite4.geometry();
        let oasis = Model::Oasis2.geometry();
        assert_eq!((pw4.width, pw4.height, pw4.stride), (1072, 1448, 1088));
        assert_eq!(
            (oasis.width, oasis.height, oasis.stride),
            (1264, 1680, 1280)
        );
        assert_eq!(oasis.rotate, 2);
        assert_eq!((oasis.virtual_width, oasis.virtual_height), (1280, 3584));
        assert!(oasis.width > pw4.width && oasis.height > pw4.height);
    }

    #[test]
    fn the_stride_covers_the_visible_width() {
        for model in [Model::Paperwhite4, Model::Oasis2] {
            let g = model.geometry();
            assert!(g.stride >= g.width, "{model:?}");
            assert!(g.virtual_width >= g.stride, "{model:?}");
            assert!(g.virtual_height >= g.height, "{model:?}");
        }
    }

    #[test]
    fn the_oasis_2_has_page_turn_keys_and_the_pw4_has_none() {
        let keys = Model::Oasis2
            .page_turn_keys()
            .expect("the Oasis 2 has buttons");
        assert_eq!(keys.forward, 104);
        assert_eq!(keys.back, 109);
        assert_eq!(Model::Paperwhite4.page_turn_keys(), None);
    }

    #[test]
    fn a_portrait_touchscreen_needs_no_swap() {
        // The Paperwhite 4's Goodix reports the framebuffer's orientation.
        let t = touch_transform(1072, 1448, 1071, 1447);
        assert_eq!(t, Model::Paperwhite4.touch_transform());
        // So does the Oasis 2's i2c controller.
        let t = touch_transform(1264, 1680, 1263, 1679);
        assert_eq!(t, Model::Oasis2.touch_transform());
    }

    #[test]
    fn a_landscape_digitizer_is_swapped() {
        let t = touch_transform(1264, 1680, 1679, 1263);
        assert!(t.swap_xy);
        assert!(!t.mirror_x && !t.mirror_y);
        // A screen that is not portrait keeps its axes.
        let t = touch_transform(1680, 1264, 1679, 1263);
        assert!(!t.swap_xy);
    }

    #[test]
    fn the_touch_follows_the_framebuffer_rotation() {
        // The Oasis 2 held the right way up: the framebuffer and the
        // digitizer agree, so nothing is mirrored.
        let t = Model::Oasis2.touch_transform_at(0);
        assert_eq!(
            t,
            TouchTransform {
                swap_xy: false,
                mirror_x: false,
                mirror_y: false
            }
        );
        // Held upside down: the framebuffer is turned around, so both axes
        // are mirrored.
        let t = Model::Oasis2.touch_transform_at(ROTATE_UPSIDE_DOWN);
        assert_eq!(
            t,
            TouchTransform {
                swap_xy: false,
                mirror_x: true,
                mirror_y: true
            }
        );
    }

    #[test]
    fn a_landscape_rotation_is_read_as_no_rotation() {
        // Rotations 1 and 3 are not supported: the caller logs them and
        // treats them as 0.
        for rotate in [1, 3] {
            assert_eq!(
                Model::Oasis2.touch_transform_at(rotate),
                Model::Oasis2.touch_transform_at(0)
            );
        }
    }

    #[test]
    fn the_paperwhite_keeps_the_transform_it_had() {
        // The Paperwhite 4's framebuffer is rotated 3, which is landscape
        // and unsupported: its transform stays the one HAL-001 had.
        assert_eq!(
            Model::Paperwhite4.touch_transform_at(3),
            Model::Paperwhite4.touch_transform()
        );
        assert_eq!(
            Model::Paperwhite4.touch_transform_at(0),
            Model::Paperwhite4.touch_transform()
        );
    }

    #[test]
    fn the_page_turn_keys_swap_when_the_device_is_upside_down() {
        let upright = Model::Oasis2
            .page_turn_keys_at(0)
            .expect("the Oasis 2 has buttons");
        assert_eq!((upright.forward, upright.back), (104, 109));
        // Upside down, the top button is the other one: their roles swap.
        let upside = Model::Oasis2
            .page_turn_keys_at(ROTATE_UPSIDE_DOWN)
            .expect("the Oasis 2 has buttons");
        assert_eq!((upside.forward, upside.back), (109, 104));
        // A rotation that is not a turn around changes nothing.
        let flat = Model::Oasis2.page_turn_keys_at(1).expect("buttons");
        assert_eq!((flat.forward, flat.back), (104, 109));
    }

    #[test]
    fn the_paperwhite_has_no_buttons_in_any_rotation() {
        for rotate in [0, 1, 2, 3] {
            assert_eq!(Model::Paperwhite4.page_turn_keys_at(rotate), None);
        }
    }

    #[test]
    fn every_model_claims_all_of_its_device_codes() {
        for model in Model::ALL {
            let codes = model.device_codes();
            assert!(!codes.is_empty(), "{model:?}");
            for code in codes {
                assert_eq!(
                    Model::from_device_code(code),
                    Some(*model),
                    "{code} is in the table of {model:?} but resolves elsewhere"
                );
            }
        }
    }

    #[test]
    fn the_supported_code_sets_do_not_overlap() {
        let mut seen: Vec<&str> = Vec::new();
        for model in Model::ALL {
            for code in model.device_codes() {
                assert!(!seen.contains(code), "{code} is in two model tables");
                seen.push(code);
            }
        }
        assert_eq!(seen.len(), 179);
    }

    #[test]
    fn the_code_sets_cover_every_registry_entry() {
        assert_eq!(Model::ALL.len(), 20);
        assert_eq!(Model::Paperwhite3.device_codes().len(), 14);
        assert_eq!(Model::Paperwhite4.device_codes().len(), 21);
        assert_eq!(Model::Paperwhite5.device_codes().len(), 6);
        assert_eq!(Model::Paperwhite5Se.device_codes().len(), 3);
        assert_eq!(Model::Paperwhite2.device_codes().len(), 14);
        assert_eq!(Model::Paperwhite6.device_codes().len(), 10);
        assert_eq!(Model::KindleBasic3.device_codes().len(), 6);
        assert_eq!(Model::Voyage.device_codes().len(), 6);
        assert_eq!(Model::Oasis1.device_codes().len(), 6);
        assert_eq!(Model::Oasis2.device_codes().len(), 15);
        assert_eq!(Model::Oasis3.device_codes().len(), 6);
        assert_eq!(Model::KindleBasic2.device_codes().len(), 3);
        assert_eq!(Model::KindleScribe.device_codes().len(), 8);
        assert_eq!(Model::KindleScribe2.device_codes().len(), 8);
        assert_eq!(Model::KindleScribe3.device_codes().len(), 6);
        assert_eq!(Model::ColorSoft.device_codes().len(), 21);
        assert_eq!(Model::ScribeColorSoft.device_codes().len(), 6);
    }

    #[test]
    fn the_scribe_is_recognised_and_has_no_page_turn_keys() {
        assert_eq!(Model::from_device_code("227"), Some(Model::KindleScribe));
        assert_eq!(Model::from_device_code("3V0"), Some(Model::KindleScribe2));
        assert_eq!(
            Model::from_serial("XYZ2270123456789"),
            Some(Model::KindleScribe)
        );
        assert_eq!(Model::KindleScribe.name(), "Kindle Scribe");
        assert_eq!(Model::KindleScribe.page_turn_keys(), None);
        let g = Model::KindleScribe.geometry();
        assert_eq!((g.width, g.height, g.stride), (1860, 2480, 1872));
        assert!(g.stride >= g.width);
        assert!(g.virtual_height >= g.height);
    }

    #[test]
    fn the_voyage_buttons_are_the_other_way_round() {
        let keys = Model::Voyage
            .page_turn_keys()
            .expect("the Voyage has buttons");
        assert_eq!((keys.forward, keys.back), (109, 104));
        let upside = Model::Voyage
            .page_turn_keys_at(ROTATE_UPSIDE_DOWN)
            .expect("the Voyage has buttons");
        assert_eq!((upside.forward, upside.back), (104, 109));
    }

    #[test]
    fn the_pw2_and_pw6_match_measured_probe_geometry() {
        for code in ["17", "D4", "5A"] {
            let model = Model::from_device_code(code).expect("pw2 variant");
            assert_eq!(model, Model::Paperwhite2);
            assert_eq!(model.dpi(), 212);
            let g = model.geometry();
            assert_eq!(
                (
                    g.width,
                    g.height,
                    g.stride,
                    g.rotate,
                    g.virtual_width,
                    g.virtual_height
                ),
                (758, 1024, 768, 3, 768, 4096)
            );
        }
        for code in ["33W", "33X", "3H3"] {
            let model = Model::from_device_code(code).expect("pw6 variant");
            assert_eq!(model, Model::Paperwhite6);
            assert_eq!(model.dpi(), 300);
            let g = model.geometry();
            assert_eq!(
                (
                    g.width,
                    g.height,
                    g.stride,
                    g.rotate,
                    g.virtual_width,
                    g.virtual_height
                ),
                (1272, 1696, 1272, 0, 1272, 3392)
            );
        }
    }

    #[test]
    fn the_new_models_have_reference_dimensions() {
        for model in Model::ALL {
            let g = model.geometry();
            assert!(g.width > 0 && g.height > 0, "{model:?}");
            assert!(g.width <= g.height, "{model:?}");
            if g.stride != 0 {
                assert!(g.stride >= g.width, "{model:?}");
            }
        }
    }

    #[test]
    fn the_kobo_models_are_told_apart_by_product_and_model_number() {
        assert_eq!(KoboModel::from_product("nova"), Ok(KoboModel::ClaraHd));
        assert_eq!(KoboModel::from_product("io"), Ok(KoboModel::Libra2));
        assert_eq!(KoboModel::from_model_number("376"), Ok(KoboModel::ClaraHd));
        assert_eq!(KoboModel::from_model_number("0388"), Ok(KoboModel::Libra2));
        assert_eq!(
            KoboModel::from_sources(Some("nova"), Some("376")),
            Ok(Some(KoboModel::ClaraHd))
        );
        assert_eq!(
            KoboModel::from_sources(None, Some("388")),
            Ok(Some(KoboModel::Libra2))
        );
        assert_eq!(KoboModel::from_sources(None, None), Ok(None));
        // A Kindle code is not a Kobo model number.
        assert!(matches!(
            KoboModel::from_model_number("0PP"),
            Err(KoboModelError::UnknownModelNumber(_))
        ));
    }

    #[test]
    fn the_kobo_first_batch_geometry_and_protocol_match_plato() {
        let clara = KoboModel::ClaraHd;
        let g = clara.geometry();
        assert_eq!((g.width, g.height), (1072, 1448));
        assert_eq!(clara.dpi(), 300);
        assert_eq!(clara.mark(), 7);
        assert_eq!(clara.touch_protocol(), KoboProtocol::MultiB);
        assert_eq!(clara.model_code(), "N249");
        assert_eq!(clara.model_id(), "kobo-clara-hd");
        assert_eq!(clara.page_turn_keys(), None);

        let libra = KoboModel::Libra2;
        let g = libra.geometry();
        assert_eq!((g.width, g.height), (1264, 1680));
        assert_eq!(libra.dpi(), 300);
        assert_eq!(libra.mark(), 9);
        assert_eq!(libra.touch_protocol(), KoboProtocol::MultiC);
        assert_eq!(libra.model_code(), "N418");
        assert_eq!(libra.model_id(), "kobo-libra-2");
        let keys = libra.page_turn_keys().expect("the Libra 2 has buttons");
        assert_eq!((keys.forward, keys.back), (194, 193));
        for model in KoboModel::ALL {
            assert_eq!(model.backend(), KoboBackend::ImxEpdc);
        }
    }

    #[test]
    fn a_supported_kobo_model_rejects_a_wrong_product() {
        assert_eq!(
            KoboModel::from_sources(Some("nova"), Some("388")),
            Err(KoboModelError::Mismatch {
                product: "nova".to_string(),
                model_number: "388".to_string(),
            })
        );
        assert!(matches!(
            KoboModel::from_sources(Some("io"), Some("376")),
            Err(KoboModelError::Mismatch { .. })
        ));
    }

    #[test]
    fn kobo_models_outside_the_first_batch_are_rejected_not_guessed() {
        assert_eq!(
            KoboModel::from_model_number("383"),
            Err(KoboModelError::UnsupportedModel {
                model_number: "383".to_string(),
                backend: KoboBackend::Sunxi,
            })
        );
        assert_eq!(
            KoboModel::from_model_number("386"),
            Err(KoboModelError::UnsupportedModel {
                model_number: "386".to_string(),
                backend: KoboBackend::ImxEpdc,
            })
        );
        assert!(matches!(
            KoboModel::from_model_number("999"),
            Err(KoboModelError::UnknownModelNumber(_))
        ));
        let text = KoboModel::from_model_number("383").unwrap_err().to_string();
        assert!(text.contains("not supported") && !text.contains("serial"));
    }

    #[test]
    fn the_normalized_geometry_keeps_portrait_and_rejects_bad_stride() {
        assert_eq!(Geometry::normalize(1072, 1448, 1088), Ok((1072, 1448)));
        assert_eq!(Geometry::normalize(1448, 1072, 1088), Ok((1072, 1448)));
        assert_eq!(Geometry::normalize(2480, 1860, 1872), Ok((1860, 2480)));
        assert_eq!(Geometry::normalize(1860, 2480, 1872), Ok((1860, 2480)));
        assert_eq!(
            Geometry::normalize(2480, 1860, 1024),
            Err("framebuffer stride is narrower than the visible width")
        );
        assert_eq!(
            Geometry::normalize(0, 0, 0),
            Err("framebuffer reports an empty screen")
        );
    }
}
