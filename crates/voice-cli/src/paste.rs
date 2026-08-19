//! Synthetic ⌘V keystroke, for `dictate --paste`.
//!
//! Real `CGEvent`-based key synthesis (`objc2-core-graphics`), not a stub —
//! but genuinely NOT VERIFIABLE in this environment: posting a synthetic
//! keyboard event requires the same Accessibility TCC grant this CLI checks
//! for at startup (see `crate::permissions`), which this sandboxed run does
//! not have. `dictate` will not even reach this function without that grant
//! (see `dictate::run`'s permission gate), so this code path has not been
//! exercised end-to-end on real hardware in this session.

#[cfg(target_os = "macos")]
pub fn synthesize_cmd_v() -> anyhow::Result<()> {
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation};

    // kVK_ANSI_V, per Carbon's HIToolbox/Events.h -- the standard macOS
    // virtual keycode table (keycodes are physical-key, layout-independent).
    const KEYCODE_V: u16 = 9;

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
    let source_ref = source.as_deref();

    let key_down = CGEvent::new_keyboard_event(source_ref, KEYCODE_V, true).ok_or_else(|| {
        anyhow::anyhow!(
            "failed to create the key-down CGEvent (this is the exact failure mode expected \
             without the Accessibility permission grant -- see `textify-voice dictate`'s \
             startup permission check)"
        )
    })?;
    CGEvent::set_flags(Some(&*key_down), CGEventFlags::MaskCommand);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&*key_down));

    let key_up = CGEvent::new_keyboard_event(source_ref, KEYCODE_V, false)
        .ok_or_else(|| anyhow::anyhow!("failed to create the key-up CGEvent"))?;
    CGEvent::set_flags(Some(&*key_up), CGEventFlags::MaskCommand);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&*key_up));

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn synthesize_cmd_v() -> anyhow::Result<()> {
    anyhow::bail!("synthetic paste (--paste) is only implemented for macOS in this MVP build")
}
