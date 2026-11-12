//! Input events and input backends.

/// Phase of a touch contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// The finger touched down.
    Press,
    /// The finger moved while it was down.
    Move,
    /// The finger lifted.
    Release,
}

/// A key of the device.
///
/// Backends map their device-specific codes onto these variants; a code the
/// platform layer does not name is reported as [`KeyCode::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Left / page-back key.
    Left,
    /// Right / page-forward key.
    Right,
    /// Up key.
    Up,
    /// Down key.
    Down,
    /// Select / enter key.
    Enter,
    /// Back key.
    Back,
    /// Menu key.
    Menu,
    /// Page-up key.
    PageUp,
    /// Page-down key.
    PageDown,
    /// Home key.
    Home,
    /// A device-specific key, carrying its raw code.
    Unknown(u16),
}

/// An event produced by an input backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    /// A touch contact, in display coordinates.
    Touch {
        /// Horizontal position, in pixels from the left edge.
        x: i32,
        /// Vertical position, in pixels from the top edge.
        y: i32,
        /// Whether the contact began, moved or ended.
        phase: TouchPhase,
    },
    /// A physical key.
    Key {
        /// The key that changed state.
        code: KeyCode,
        /// `true` when the key went down, `false` when it was released.
        pressed: bool,
    },
}

/// An input backend, e.g. evdev on the device or synthetic events in the
/// simulator.
pub trait Input {
    /// Poll for the next event.
    ///
    /// Returns `None` when no event is pending. The call must not block.
    fn poll(&mut self) -> Option<InputEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Queue(VecDeque<InputEvent>);

    impl Input for Queue {
        fn poll(&mut self) -> Option<InputEvent> {
            self.0.pop_front()
        }
    }

    #[test]
    fn events_carry_touch_and_key_payloads() {
        let touch = InputEvent::Touch {
            x: 10,
            y: 20,
            phase: TouchPhase::Press,
        };
        assert_eq!(
            touch,
            InputEvent::Touch {
                x: 10,
                y: 20,
                phase: TouchPhase::Press
            }
        );

        let key = InputEvent::Key {
            code: KeyCode::Left,
            pressed: true,
        };
        assert_eq!(
            key,
            InputEvent::Key {
                code: KeyCode::Left,
                pressed: true
            }
        );
    }

    #[test]
    fn polling_returns_events_in_order_then_none() {
        let mut queue = Queue(VecDeque::from([
            InputEvent::Key {
                code: KeyCode::PageDown,
                pressed: true,
            },
            InputEvent::Touch {
                x: 1,
                y: 2,
                phase: TouchPhase::Release,
            },
        ]));
        assert!(queue.poll().is_some());
        assert!(queue.poll().is_some());
        assert!(queue.poll().is_none());
    }

    #[test]
    fn unknown_keys_keep_their_raw_code() {
        assert_eq!(KeyCode::Unknown(42), KeyCode::Unknown(42));
        assert_ne!(KeyCode::Unknown(42), KeyCode::Unknown(43));
    }
}
