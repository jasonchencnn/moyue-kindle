// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Kindle models Aristotle runs on: how to recognise one from its serial
//! number, and what its panel, touchscreen and page-turn keys are.
//!
//! The model comes from the device code in the serial number (`/proc/usid` on
//! the device): the three characters in the middle of it. Serials that start
//! with `B` or `9` use the older scheme, with the code in characters 3-4; the
//! others carry it in characters 4-6. The code tables are KOReader's
//! (`frontend/device/kindle/device.lua`, `pw4_set` and `koa2_set`).

/// A Kindle model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Model {
    /// The Paperwhite 4 (2018): the "Rex" platform.
    Paperwhite4,
    /// The Oasis 2 (2017): the "Zelda" platform.
    Oasis2,
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

/// The panel geometry of a model, in portrait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// The visible width in pixels.
    pub width: u32,
    /// The visible height in pixels.
    pub height: u32,
    /// The bytes per row of the framebuffer.
    pub stride: u32,
    /// The driver's rotation, `FBIOGET_VSCREENINFO`'s `rotate`.
    pub rotate: u32,
    /// The width of the virtual framebuffer.
    pub virtual_width: u32,
    /// The height of the virtual framebuffer.
    pub virtual_height: u32,
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

impl Model {
    /// The model with this device code.
    pub fn from_device_code(code: &str) -> Option<Model> {
        if PW4_CODES.contains(&code) {
            Some(Model::Paperwhite4)
        } else if OASIS2_CODES.contains(&code) {
            Some(Model::Oasis2)
        } else {
            None
        }
    }

    /// The model of a serial number, read from its device code.
    pub fn from_serial(serial: &str) -> Option<Model> {
        Model::from_device_code(device_code(serial)?)
    }

    /// The device codes of the model.
    pub fn device_codes(self) -> &'static [&'static str] {
        match self {
            Model::Paperwhite4 => PW4_CODES,
            Model::Oasis2 => OASIS2_CODES,
        }
    }

    /// The model as the About screen names it.
    pub const fn name(self) -> &'static str {
        match self {
            Model::Paperwhite4 => "Kindle Paperwhite 4",
            Model::Oasis2 => "Kindle Oasis 2",
        }
    }

    /// The panel geometry of the model, in portrait.
    pub const fn geometry(self) -> Geometry {
        match self {
            Model::Paperwhite4 => Geometry {
                width: 1072,
                height: 1448,
                stride: 1088,
                rotate: 3,
                virtual_width: 1088,
                virtual_height: 6144,
            },
            Model::Oasis2 => Geometry {
                width: 1264,
                height: 1680,
                stride: 1280,
                rotate: 2,
                virtual_width: 1280,
                virtual_height: 3584,
            },
        }
    }

    /// The transform of the model's touchscreen.
    ///
    /// Both digitizers report the framebuffer's own portrait orientation, so
    /// this is the identity for them. [`touch_transform`] works out the swap
    /// from the axis ranges a device reports. The framebuffer's rotation
    /// comes on top of this: see [`Model::touch_transform_at`].
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
            Model::Paperwhite4 => None,
            // The Oasis 2's top button turns the page forward and its bottom
            // one turns it back: the reverse of the other Kindles. The codes
            // are KOReader's (`KindleOasis2:init`).
            Model::Oasis2 => Some(PageTurnKeys {
                forward: 104,
                back: 109,
            }),
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
}

/// The transform for a touchscreen that reports these axis ranges.
///
/// A digitizer whose X axis is the longer one describes the panel in its own
/// orientation, which is not the framebuffer's: the axes are then swapped.
/// The Paperwhite 4's Goodix and the Oasis 2's i2c controller both report the
/// framebuffer's portrait orientation, so this is the identity for them; it
/// only differs from the model's own transform on a device that reports
/// otherwise.
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
        // The Paperwhite 5 and anything else is none of ours.
        assert_eq!(Model::from_device_code("1Q0"), None);
        assert_eq!(Model::from_device_code(""), None);
    }

    #[test]
    fn the_model_comes_from_the_serial_number() {
        assert_eq!(
            Model::from_serial("XYZ0PP0123456789"),
            Some(Model::Paperwhite4)
        );
        assert_eq!(Model::from_serial("XYZ0LN0123456789"), Some(Model::Oasis2));
        assert_eq!(Model::from_serial("XYZ1Q00123456789"), None);
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
}
