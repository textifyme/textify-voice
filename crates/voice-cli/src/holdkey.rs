//! Hold-to-talk on a **bare modifier** key (hold Option, speak, release).
//!
//! `global-hotkey` cannot do this: it parses "modifiers + one key", and a lone
//! modifier is not a valid `HotKey`. Detecting a bare modifier means watching
//! `flagsChanged` events directly, which is what this module does with a
//! `CGEventTap`.
//!
//! Three details here are load-bearing and easy to get wrong:
//!
//! 1. **The tap is `ListenOnly`.** A tap that could swallow events would break
//!    Option as a typing modifier system-wide — no more `å`, `ø`, `≈`. We only
//!    ever observe; every event passes through untouched.
//! 2. **A key pressed while the modifier is held CANCELS the utterance.**
//!    Holding Option is also how you type special characters, so `Option+e`
//!    must not silently become a 200 ms recording. Same for a second modifier
//!    joining (`Cmd+Option+…` shortcuts).
//! 3. **The system disables a tap that runs slow** (`TapDisabledByTimeout`) or
//!    on certain user input. That is not fatal but it IS silent — the hotkey
//!    just stops working forever. We surface it and re-arm.
//!
//! NOT VERIFIED: this requires the Accessibility TCC grant, so it has never run
//! in an automated environment. See `crates/voice-cli/README.md`.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};

use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
use crate::platform::{HoldEvent, HoldKey};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType,
};

impl HoldKey {
    /// Carbon virtual keycodes (`HIToolbox/Events.h`); these are physical-key
    /// codes and so are keyboard-layout independent.
    fn keycodes(self) -> &'static [i64] {
        match self {
            HoldKey::LeftOption => &[58],
            HoldKey::RightOption => &[61],
            HoldKey::EitherOption => &[58, 61],
            HoldKey::Fn => &[63],
            HoldKey::RightCommand => &[54],
            HoldKey::LeftControl => &[59],
            HoldKey::RightControl => &[62],
        }
    }

    /// The aggregate flag bit that reflects "this modifier is held". Note it is
    /// shared between left and right variants, which is why `is_down` state is
    /// tracked per-keycode rather than inferred from flags alone.
    fn flag(self) -> CGEventFlags {
        match self {
            HoldKey::LeftOption | HoldKey::RightOption | HoldKey::EitherOption => {
                CGEventFlags::MaskAlternate
            }
            HoldKey::Fn => CGEventFlags::MaskSecondaryFn,
            HoldKey::RightCommand => CGEventFlags::MaskCommand,
            HoldKey::LeftControl | HoldKey::RightControl => CGEventFlags::MaskControl,
        }
    }

}


/// Callback-side state. Lives behind a leaked pointer handed to the C callback;
/// the tap callback runs on the run loop of the thread that installed it, i.e.
/// the same thread that drains the receiver, so no locking is needed.
struct TapState {
    keycodes: &'static [i64],
    flag: CGEventFlags,
    is_down: bool,
    tx: Sender<HoldEvent>,
}

unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // Always hand the event straight back — this tap is ListenOnly and must
    // never alter or drop input.
    let passthrough = event.as_ptr();

    if user_info.is_null() {
        return passthrough;
    }
    // SAFETY: `user_info` is the leaked `TapState` from `HoldKeyTap::install`,
    // which outlives the tap, and this callback is only ever invoked on the
    // installing thread's run loop, so the &mut is not aliased.
    let state = unsafe { &mut *(user_info as *mut TapState) };

    if event_type == CGEventType::TapDisabledByTimeout
        || event_type == CGEventType::TapDisabledByUserInput
    {
        if state.is_down {
            state.is_down = false;
            let _ = state.tx.send(HoldEvent::Cancel("the input tap was disabled mid-utterance"));
        }
        let _ = state.tx.send(HoldEvent::SourceDisabled);
        return passthrough;
    }

    let ev = unsafe { event.as_ref() };

    if event_type == CGEventType::KeyDown {
        // A real key while the modifier is held means the user is typing a
        // special character (Option+e) or firing a shortcut, not dictating.
        if state.is_down {
            state.is_down = false;
            let _ = state
                .tx
                .send(HoldEvent::Cancel("another key was pressed while the hold key was down"));
        }
        return passthrough;
    }

    if event_type != CGEventType::FlagsChanged {
        return passthrough;
    }

    let keycode = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode);
    let flags = CGEvent::flags(Some(ev));

    if !state.keycodes.contains(&keycode) {
        // Some *other* modifier changed. If it joined while we were holding,
        // this is a chord (Cmd+Option+…), not dictation.
        if state.is_down && flags.contains(state.flag) {
            state.is_down = false;
            let _ = state
                .tx
                .send(HoldEvent::Cancel("another modifier joined while the hold key was down"));
        }
        return passthrough;
    }

    let now_held = flags.contains(state.flag);
    if now_held && !state.is_down {
        state.is_down = true;
        let _ = state.tx.send(HoldEvent::Down);
    } else if !now_held && state.is_down {
        state.is_down = false;
        let _ = state.tx.send(HoldEvent::Up);
    }

    passthrough
}

/// An installed bare-modifier tap. Dropping it removes the tap.
pub struct HoldKeyTap {
    port: CFRetained<CFMachPort>,
    rx: Receiver<HoldEvent>,
    _state: Box<TapState>,
}

impl HoldKeyTap {
    /// Install the tap on the **current thread's** run loop. The caller must
    /// keep pumping that run loop or no events will ever arrive.
    pub fn install(key: HoldKey) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel();

        let mut state = Box::new(TapState {
            keycodes: key.keycodes(),
            flag: key.flag(),
            is_down: false,
            tx,
        });
        let state_ptr = (&mut *state) as *mut TapState as *mut c_void;

        let mask: CGEventMask = (1u64 << CGEventType::FlagsChanged.0) | (1u64 << CGEventType::KeyDown.0);

        // SAFETY: `tap_callback` matches CGEventTapCallBack, and `state_ptr`
        // points at a Box we own and keep alive for the life of this struct.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                mask,
                Some(tap_callback),
                state_ptr,
            )
        }
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not create the input event tap. This is the expected failure when \
                 Accessibility has not been granted: System Settings > Privacy & Security > \
                 Accessibility, add the terminal (or textify-voice) and enable it, then re-run."
            )
        })?;

        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0).ok_or_else(|| {
            anyhow::anyhow!("could not create a run loop source for the input event tap")
        })?;
        let run_loop = CFRunLoop::current()
            .ok_or_else(|| anyhow::anyhow!("no run loop on the current thread"))?;
        // SAFETY: kCFRunLoopCommonModes is a static CF constant.
        let mode = unsafe { kCFRunLoopCommonModes };
        run_loop.add_source(Some(&source), mode);
        CGEvent::tap_enable(&port, true);

        Ok(Self { port, rx, _state: state })
    }

    /// Drain everything the tap has observed since the last call. Never blocks.
    pub fn poll(&self) -> Vec<HoldEvent> {
        self.rx.try_iter().collect()
    }

    /// Re-arm after the OS disabled the tap.
    pub fn re_enable(&self) {
        CGEvent::tap_enable(&self.port, true);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn every_hold_key_maps_to_a_keycode_and_a_flag() {
        for key in [
            HoldKey::LeftOption,
            HoldKey::RightOption,
            HoldKey::EitherOption,
            HoldKey::Fn,
            HoldKey::RightCommand,
            HoldKey::LeftControl,
            HoldKey::RightControl,
        ] {
            assert!(!key.keycodes().is_empty(), "{key:?} has no keycode");
            assert!(!key.describe().is_empty());
            // Every flag must be a single bit: an aggregate would match
            // unrelated modifiers and fire dictation on the wrong key.
            assert_eq!(key.flag().bits().count_ones(), 1, "{key:?} flag is not a single bit");
        }
    }

    #[test]
    fn left_and_right_variants_use_distinct_keycodes_but_share_a_flag() {
        assert_ne!(HoldKey::LeftOption.keycodes(), HoldKey::RightOption.keycodes());
        assert_eq!(HoldKey::LeftOption.flag(), HoldKey::RightOption.flag());
        // Either-option must cover both physical keys.
        assert_eq!(HoldKey::EitherOption.keycodes(), &[58, 61]);
    }

    #[test]
    fn option_keycodes_match_the_carbon_virtual_keycode_table() {
        // Guards against a silent regression: if these drift, dictation binds
        // to the wrong physical key and there is no error to notice.
        assert_eq!(HoldKey::LeftOption.keycodes(), &[58]); // kVK_Option
        assert_eq!(HoldKey::RightOption.keycodes(), &[61]); // kVK_RightOption
        assert_eq!(HoldKey::Fn.keycodes(), &[63]); // kVK_Function
        assert_eq!(HoldKey::RightCommand.keycodes(), &[54]); // kVK_RightCommand
    }
}
