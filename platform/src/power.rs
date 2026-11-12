//! Power management backends.

use crate::error::Result;

/// A power management backend, e.g. lipc on the device.
pub trait Power {
    /// Current battery charge, in percent (`0..=100`).
    ///
    /// Backends may refresh their cached reading, so this takes `&mut self`.
    fn battery_percent(&mut self) -> Result<u8>;

    /// Keep the device awake, or let it sleep again.
    fn prevent_sleep(&mut self, prevent: bool) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    struct Fake {
        percent: u8,
        awake: bool,
    }

    impl Power for Fake {
        fn battery_percent(&mut self) -> Result<u8> {
            Ok(self.percent)
        }
        fn prevent_sleep(&mut self, prevent: bool) -> Result<()> {
            self.awake = prevent;
            Ok(())
        }
    }

    #[test]
    fn battery_percent_is_reported_in_percent() {
        let mut fake = Fake {
            percent: 87,
            awake: false,
        };
        assert_eq!(fake.battery_percent().expect("battery reading"), 87);
    }

    #[test]
    fn prevent_sleep_toggles_the_awake_flag() {
        let mut fake = Fake {
            percent: 50,
            awake: false,
        };
        fake.prevent_sleep(true).expect("prevent sleep");
        assert!(fake.awake);
        fake.prevent_sleep(false).expect("allow sleep");
        assert!(!fake.awake);
    }

    #[test]
    fn backends_may_report_their_own_failures() {
        struct Broken;
        impl Power for Broken {
            fn battery_percent(&mut self) -> Result<u8> {
                Err(Error::Backend("no battery".into()))
            }
            fn prevent_sleep(&mut self, _prevent: bool) -> Result<()> {
                Ok(())
            }
        }
        let mut broken = Broken;
        assert_eq!(
            broken.battery_percent().expect_err("backend failure"),
            Error::Backend("no battery".into())
        );
    }
}
