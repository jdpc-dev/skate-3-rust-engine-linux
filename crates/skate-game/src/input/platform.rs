//! Windows device transport. Raw signed axes/trigger bytes reach the TU3
//! converter without Bevy/gilrs deadzones or normalized-axis reconstruction.
use skate_core::input::xbox::XboxState;

pub(crate) struct DevicePacket {
    pub number: u32,
    pub state: XboxState,
    pub subtype: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceError {
    Disconnected,
    State(u32),
    Capabilities(u32),
    #[cfg(not(windows))]
    UnsupportedPlatform,
}

/// Device identity is metadata; raw input is still sampled every host frame.
/// Refresh periodically as well as after errors, so hot swaps cannot leave a
/// subtype cached indefinitely even if Windows never exposes a disconnect.
#[derive(Default)]
pub(crate) struct CapabilityCache {
    value: Option<(u8, std::time::Instant)>,
}
impl CapabilityCache {
    pub(crate) fn invalidate(&mut self) {
        self.value = None;
    }
    fn get(
        &mut self,
        now: std::time::Instant,
        read: impl FnOnce() -> Result<u8, DeviceError>,
    ) -> Result<u8, DeviceError> {
        if let Some((subtype, expires)) = self.value {
            if now < expires {
                return Ok(subtype);
            }
        }
        self.value = None;
        let subtype = read()?;
        self.value = Some((subtype, now + std::time::Duration::from_secs(1)));
        Ok(subtype)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::mem::MaybeUninit;

    // ABI from the installed Windows SDK Xinput.h. No OS-owned pointers are
    // retained and only successful calls permit reading output storage.
    #[repr(C)]
    struct Gamepad {
        buttons: u16,
        left_trigger: u8,
        right_trigger: u8,
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    }
    #[repr(C)]
    struct State {
        number: u32,
        gamepad: Gamepad,
    }
    #[repr(C)]
    struct Vibration {
        left: u16,
        right: u16,
    }
    #[repr(C)]
    struct Capabilities {
        device_type: u8,
        subtype: u8,
        flags: u16,
        gamepad: Gamepad,
        vibration: Vibration,
    }
    const _: () = assert!(size_of::<Gamepad>() == 12);
    const _: () = assert!(size_of::<State>() == 16);
    const _: () = assert!(size_of::<Capabilities>() == 20);

    #[link(name = "xinput")]
    unsafe extern "system" {
        fn XInputGetState(index: u32, state: *mut State) -> u32;
        fn XInputGetCapabilities(index: u32, flags: u32, capabilities: *mut Capabilities) -> u32;
    }

    pub(super) fn poll(
        index: u32,
        cache: &mut CapabilityCache,
    ) -> Result<DevicePacket, DeviceError> {
        let mut state = MaybeUninit::<State>::uninit();
        // SAFETY: properly aligned writable storage with the SDK's exact C ABI.
        let result = unsafe { XInputGetState(index, state.as_mut_ptr()) };
        if result != 0 {
            cache.invalidate();
        }
        if result == 1167 {
            return Err(DeviceError::Disconnected);
        }
        if result != 0 {
            return Err(DeviceError::State(result));
        }
        let subtype = cache.get(std::time::Instant::now(), || {
            let mut capabilities = MaybeUninit::<Capabilities>::uninit();
            // SAFETY: writable storage with the SDK ABI; read only on success.
            let result = unsafe { XInputGetCapabilities(index, 1, capabilities.as_mut_ptr()) };
            if result != 0 {
                return Err(DeviceError::Capabilities(result));
            }
            Ok(unsafe { capabilities.assume_init() }.subtype)
        })?;
        // SAFETY: successful XInputGetState initialized the complete structure.
        let state = unsafe { state.assume_init() };
        Ok(DevicePacket {
            number: state.number,
            state: XboxState {
                buttons: state.gamepad.buttons,
                triggers: [state.gamepad.left_trigger, state.gamepad.right_trigger],
                left: [state.gamepad.left_x, state.gamepad.left_y],
                right: [state.gamepad.right_x, state.gamepad.right_y],
            },
            subtype,
        })
    }
}

#[cfg(not(windows))]
mod linux {
    use super::*;
    use gilrs::{Axis, Button, Gamepad, GamepadId, Gilrs, GilrsBuilder};
    use std::cell::RefCell;

    const MAX_PADS: usize = 4;
    /// XInput reports a standard pad as subtype 1, which is what makes the TU3
    /// converter emit a zeroed device byte 13.
    const STANDARD_GAMEPAD: u8 = 1;
    /// Axis-only d-pad fallback: without the default filters a pad that reports
    /// a hat and no d-pad buttons is folded in here instead.
    const DPAD_THRESHOLD: f32 = 0.5;

    /// The bundled SDL database maps an Xbox pad's `leftshoulder`/`rightshoulder`
    /// onto these, and its `lefttrigger`/`righttrigger` onto `Axis::LeftZ`/
    /// `Axis::RightZ`, so the digital and analog halves land in the same two
    /// XInput fields the Windows path fills from `Gamepad`.
    const TRACKED_BUTTONS: [Button; 15] = [
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
        Button::Start,
        Button::Select,
        Button::LeftThumb,
        Button::RightThumb,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Mode,
        Button::South,
        Button::East,
        Button::West,
        Button::North,
    ];

    const fn button_bit(button: Button) -> u16 {
        match button {
            Button::DPadUp => 1 << 0,
            Button::DPadDown => 1 << 1,
            Button::DPadLeft => 1 << 2,
            Button::DPadRight => 1 << 3,
            Button::Start => 1 << 4,
            Button::Select => 1 << 5,
            Button::LeftThumb => 1 << 6,
            Button::RightThumb => 1 << 7,
            Button::LeftTrigger => 1 << 8,
            Button::RightTrigger => 1 << 9,
            // XInput bit 11 is the guide button; the converter drops it, but
            // callers reading `XboxState::buttons` expect XInput's layout.
            Button::Mode => 1 << 11,
            Button::South => 1 << 12,
            Button::East => 1 << 13,
            Button::West => 1 << 14,
            Button::North => 1 << 15,
            _ => 0,
        }
    }

    /// gilrs normalizes every axis onto `-1..=1` and Linux reverses Y so that
    /// pushing the stick up reads negative. Xinput hands the converter an `i16`
    /// with up positive, so flip Y and span the full asymmetric range.
    fn axis_i16(value: f32) -> i16 {
        let value = value.clamp(-1.0, 1.0);
        let scaled = if value < 0.0 {
            value * f32::from(i16::MIN.unsigned_abs())
        } else {
            value * f32::from(i16::MAX)
        };
        scaled as i16
    }

    /// A trigger axis arriving from evdev as `0..=1023` is normalized by gilrs
    /// onto `-1..=1` like any other axis, so re-center before scaling into the
    /// XInput `0..=255` byte; released must land on 0, not on -1.
    fn trigger_u8(value: f32) -> u8 {
        let unit = (value.clamp(-1.0, 1.0) + 1.0) * 0.5;
        (unit * f32::from(u8::MAX)).round() as u8
    }

    /// A trigger reported as a button-with-value is not centered, so it is
    /// scaled directly. Axis form is preferred because it is what the bundled
    /// database uses for pads that expose a real analog trigger.
    fn trigger_byte(gamepad: &Gamepad<'_>, axis: Axis, button: Button) -> u8 {
        match gamepad.axis_data(axis) {
            Some(data) => trigger_u8(data.value()),
            None => match gamepad.button_data(button) {
                Some(data) => (data.value().clamp(0.0, 1.0) * f32::from(u8::MAX)).round() as u8,
                None => 0,
            },
        }
    }

    /// Missing elements read as rest rather than panicking, so an unmapped
    /// stick is neutral instead of a hard failure.
    fn axis_value(gamepad: &Gamepad<'_>, axis: Axis) -> f32 {
        gamepad.axis_data(axis).map_or(0.0, |data| data.value())
    }

    struct Pads {
        gilrs: Gilrs,
        /// gilrs keeps a pad's id stable for the life of the context, so pairing
        /// ids with slots here keeps a pad on one controller slot and leaves
        /// the native slot history coherent across frames and hot-plugs.
        slots: Vec<(GamepadId, usize)>,
        packets: [u32; MAX_PADS],
    }

    impl Pads {
        fn refresh_slots(&mut self) {
            let connected: Vec<GamepadId> = self.gilrs.gamepads().map(|(id, _)| id).collect();
            // Free the slots of pads that went away before handing out new ones.
            self.slots.retain(|(id, _)| connected.contains(id));
            for id in connected {
                if self.slots.iter().any(|(known, _)| *known == id) {
                    continue;
                }
                if let Some(free) =
                    (0..MAX_PADS).find(|slot| !self.slots.iter().any(|(_, taken)| taken == slot))
                {
                    self.slots.push((id, free));
                }
            }
        }

        fn poll(&mut self, index: usize) -> Result<DevicePacket, DeviceError> {
            // gilrs only refreshes cached gamepad state while events are drained.
            while self.gilrs.next_event().is_some() {}
            self.refresh_slots();
            let Some((id, _)) = self.slots.iter().find(|(_, slot)| *slot == index).copied() else {
                return Err(DeviceError::Disconnected);
            };
            let gamepad = self.gilrs.gamepad(id);

            let mut buttons = TRACKED_BUTTONS
                .iter()
                .filter(|button| gamepad.is_pressed(**button))
                .fold(0u16, |bits, button| bits | button_bit(*button));
            // A hat that the mapping turned into axes is all a pad without d-pad
            // buttons reports; these are the same bits, so folding them in is
            // idempotent.
            let horizontal = axis_value(&gamepad, Axis::DPadX);
            if horizontal <= -DPAD_THRESHOLD {
                buttons |= 1 << 2;
            } else if horizontal >= DPAD_THRESHOLD {
                buttons |= 1 << 3;
            }
            let vertical = axis_value(&gamepad, Axis::DPadY);
            if vertical <= -DPAD_THRESHOLD {
                buttons |= 1 << 0;
            } else if vertical >= DPAD_THRESHOLD {
                buttons |= 1 << 1;
            }

            self.packets[index] = self.packets[index].wrapping_add(1);
            Ok(DevicePacket {
                number: self.packets[index],
                state: XboxState {
                    buttons,
                    triggers: [
                        trigger_byte(&gamepad, Axis::LeftZ, Button::LeftTrigger2),
                        trigger_byte(&gamepad, Axis::RightZ, Button::RightTrigger2),
                    ],
                    left: [
                        axis_i16(axis_value(&gamepad, Axis::LeftStickX)),
                        axis_i16(-axis_value(&gamepad, Axis::LeftStickY)),
                    ],
                    right: [
                        axis_i16(axis_value(&gamepad, Axis::RightStickX)),
                        axis_i16(-axis_value(&gamepad, Axis::RightStickY)),
                    ],
                },
                subtype: STANDARD_GAMEPAD,
            })
        }
    }

    thread_local! {
        /// One context per thread, built on first poll. A build failure is
        /// remembered so a host without usable input does not pay for a retry
        /// every frame.
        static PADS: RefCell<Option<Result<Pads, ()>>> = const { RefCell::new(None) };
    }

    pub(super) fn poll(
        index: usize,
        cache: &mut CapabilityCache,
    ) -> Result<DevicePacket, DeviceError> {
        PADS.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.is_none() {
                *cell = Some(build());
            }
            match cell.as_mut().expect("just initialized") {
                Ok(pads) => {
                    let result = pads.poll(index);
                    if result.is_err() {
                        cache.invalidate();
                    }
                    result
                }
                // gilrs could not reach a usable input source on this host, so
                // there is no pad to report.
                Err(()) => Err(DeviceError::UnsupportedPlatform),
            }
        })
    }

    fn build() -> Result<Pads, ()> {
        GilrsBuilder::new()
            .with_default_filters(false)
            .with_force_feedback(false)
            .build()
            .map(|gilrs| Pads {
                gilrs,
                slots: Vec::new(),
                packets: [0; MAX_PADS],
            })
            .map_err(|_| ())
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        /// gilrs normalizes onto `-1..=1`, so the endpoints have to survive the trip
        /// into Xinput's asymmetric `i16` without saturating or wrapping.
        #[test]
        fn axis_i16_spans_the_xinput_range() {
            assert_eq!(axis_i16(0.0), 0);
            assert_eq!(axis_i16(-1.0), i16::MIN);
            assert_eq!(axis_i16(1.0), i16::MAX);
            assert_eq!(axis_i16(f32::INFINITY), i16::MAX);
            assert_eq!(axis_i16(f32::NEG_INFINITY), i16::MIN);
            assert_eq!(axis_i16(f32::NAN), 0);
            assert_eq!(axis_i16(0.5), 16383);
        }

        /// A released evdev trigger reaches gilrs as -1 and must convert to 0; the
        /// old bug of scaling the raw axis left triggers near 0 until fully pressed.
        #[test]
        fn trigger_u8_recenters_the_normalized_axis() {
            assert_eq!(trigger_u8(-1.0), 0);
            assert_eq!(trigger_u8(1.0), 255);
            assert_eq!(trigger_u8(0.0), 128);
            assert_eq!(trigger_u8(-1.0001), 0);
            assert_eq!(trigger_u8(1.0001), 255);
        }

        /// The bundled database puts the Xbox bumpers on the trigger-named buttons
        /// and leaves bits 10/11 to the guide button.
        #[test]
        fn button_bits_match_the_xinput_layout() {
            assert_eq!(button_bit(Button::DPadUp), 0x0001);
            assert_eq!(button_bit(Button::DPadDown), 0x0002);
            assert_eq!(button_bit(Button::DPadLeft), 0x0004);
            assert_eq!(button_bit(Button::DPadRight), 0x0008);
            assert_eq!(button_bit(Button::Start), 0x0010);
            assert_eq!(button_bit(Button::Select), 0x0020);
            assert_eq!(button_bit(Button::LeftThumb), 0x0040);
            assert_eq!(button_bit(Button::RightThumb), 0x0080);
            assert_eq!(button_bit(Button::LeftTrigger), 0x0100);
            assert_eq!(button_bit(Button::RightTrigger), 0x0200);
            assert_eq!(button_bit(Button::Mode), 0x0800);
            assert_eq!(button_bit(Button::South), 0x1000);
            assert_eq!(button_bit(Button::East), 0x2000);
            assert_eq!(button_bit(Button::West), 0x4000);
            assert_eq!(button_bit(Button::North), 0x8000);
            // C/Z and the untracked variants must not invent bits.
            assert_eq!(button_bit(Button::C), 0);
            assert_eq!(button_bit(Button::Z), 0);
            assert_eq!(button_bit(Button::LeftTrigger2), 0);
            assert_eq!(button_bit(Button::RightTrigger2), 0);
        }

        /// Bumpers are read as buttons while the analog triggers come from the centered
        /// axes, so a pad that also reports the trigger-named axes must not have them
        /// folded into the digital bits.
        #[test]
        fn tracked_buttons_cover_the_xbox_layout_exactly_once() {
            let tracked = |button| TRACKED_BUTTONS.contains(&button);
            assert!(tracked(Button::LeftTrigger) && tracked(Button::RightTrigger));
            assert!(
                !tracked(Button::LeftTrigger2) && !tracked(Button::RightTrigger2),
                "the analog trigger buttons must not set a digital bit"
            );
            for button in TRACKED_BUTTONS {
                let bit = button_bit(button);
                assert_ne!(bit, 0, "{button:?} is tracked but maps to no bit");
                assert_eq!(
                    TRACKED_BUTTONS
                        .iter()
                        .filter(|other| button_bit(**other) == bit)
                        .count(),
                    1,
                    "{button:?} collides with another tracked button"
                );
            }
        }

        /// The converter drops bits 10 and 11, so a reported subtype must not be 7
        /// or byte 13 would be set for an ordinary pad.
        #[test]
        fn subtype_keeps_device_byte_13_clear() {
            assert_ne!(STANDARD_GAMEPAD, 7);
        }
    }
}

pub(crate) fn poll_cached(
    index: usize,
    cache: &mut CapabilityCache,
) -> Result<DevicePacket, DeviceError> {
    assert!(index < 4);
    #[cfg(windows)]
    return windows::poll(index as u32, cache);
    #[cfg(not(windows))]
    return linux::poll(index, cache);
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn capability_cache_refreshes_and_never_caches_errors() {
        let start = std::time::Instant::now();
        let mut cache = CapabilityCache::default();
        assert_eq!(cache.get(start, || Ok(1)), Ok(1));
        assert_eq!(
            cache.get(start + std::time::Duration::from_millis(999), || panic!(
                "redundant capability query"
            )),
            Ok(1)
        );
        assert_eq!(
            cache.get(start + std::time::Duration::from_secs(1), || Ok(2)),
            Ok(2)
        );
        cache.invalidate();
        assert_eq!(
            cache.get(start, || Err(DeviceError::Capabilities(5))),
            Err(DeviceError::Capabilities(5))
        );
        assert_eq!(cache.get(start, || Ok(3)), Ok(3));
        cache.invalidate();
        assert_eq!(cache.get(start, || Ok(4)), Ok(4));
    }
}

// Preserve the uncached API for menu-only polling.
pub(crate) fn poll(index: usize) -> Result<DevicePacket, DeviceError> {
    poll_cached(index, &mut CapabilityCache::default())
}
