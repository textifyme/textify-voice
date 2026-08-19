//! Startup permission checks for `dictate`.
//!
//! Two macOS TCC-gated permissions the live loop needs, checked up front so
//! `dictate` fails fast with an actionable message instead of half-running
//! (registering a hotkey that will never fire, or opening a mic stream that
//! will silently receive zero frames forever):
//!
//! - **Microphone** — real check, delegated to `voice_audio::microphone_permission_status`
//!   (an actual `AVCaptureDevice.authorizationStatusForMediaType` call, not a stub).
//! - **Accessibility** — real check via `AXIsProcessTrusted()`, a read-only
//!   query (no prompt) into the same TCC database System Settings' "Privacy
//!   & Security > Accessibility" pane controls. `global-hotkey`'s macOS
//!   backend installs a `CGEventTap`, and this CLI's `--paste` mode posts
//!   synthetic keyboard events via `CGEventPost` — both require this grant.
//!   ("Input Monitoring" is the same underlying gate for a plain listen-only
//!   event tap on modern macOS; Accessibility is what's needed for the
//!   *posting* path this CLI actually uses, so that's the pane named below.)
//!
//! Neither check has been exercised against a `Denied` state on this dev
//! machine (see this crate's README): both are real, non-mocked API calls,
//! but their `Denied` arm is not something this environment can force.

use voice_audio::{microphone_permission_status, MicPermission};

pub struct PermissionReport {
    pub mic: MicPermission,
    pub accessibility_trusted: bool,
}

impl PermissionReport {
    #[must_use]
    pub fn all_granted(&self) -> bool {
        self.mic == MicPermission::Authorized && self.accessibility_trusted
    }

    pub fn print(&self) {
        let mic_ok = self.mic == MicPermission::Authorized;
        println!(
            "  [{}] Microphone            : {:?}",
            if mic_ok { "OK" } else { "MISSING" },
            self.mic
        );
        if let Some(msg) = self.mic.actionable_message() {
            println!("        -> {msg}");
        }

        println!(
            "  [{}] Accessibility         : {}",
            if self.accessibility_trusted { "OK" } else { "MISSING" },
            if self.accessibility_trusted { "granted" } else { "not granted" }
        );
        if !self.accessibility_trusted {
            println!(
                "        -> Open System Settings > Privacy & Security > Accessibility, enable \
                 access for this terminal/app (the app you launched textify-voice from), then \
                 relaunch. If the hold key still never fires after that, also check System \
                 Settings > Privacy & Security > Input Monitoring for the same app."
            );
        }
    }
}

#[must_use]
pub fn check() -> PermissionReport {
    PermissionReport {
        mic: microphone_permission_status(),
        accessibility_trusted: accessibility_granted(),
    }
}

/// Ask macOS for Accessibility trust, showing the system prompt.
///
/// **This is what registers the app under System Settings → Privacy & Security
/// → Accessibility.** `AXIsProcessTrusted()` is read-only and never adds a row
/// there, so an onboarding flow built on the read-only check alone tells the
/// user to enable an entry that does not exist yet.
///
/// Prompting is asynchronous and does not change the return value, so the
/// caller must re-check afterwards.
///
/// Reserved for explicitly user-initiated onboarding — a CLI popping a
/// permission dialog on every run would be worse than telling the user which
/// pane to open. `permission_report` still uses the silent check.
#[cfg(target_os = "macos")]
pub fn prompt_for_accessibility() -> bool {
    use objc2_core_foundation::{kCFBooleanTrue, CFBoolean, CFDictionary, CFRetained, CFString};

    // SAFETY: kAXTrustedCheckOptionPrompt is a static CF constant.
    let key: &CFString = unsafe { objc2_application_services::kAXTrustedCheckOptionPrompt };
    // SAFETY: kCFBooleanTrue is a static CF constant. Using it rather than
    // CFBoolean::new avoids constructing an owned value only to reborrow it.
    let Some(value) = (unsafe { kCFBooleanTrue }) else {
        return accessibility_granted();
    };
    let opts: CFRetained<CFDictionary<CFString, CFBoolean>> =
        CFDictionary::from_slices(&[key], &[value]);
    // SAFETY: the dictionary holds the CFString key and CFBoolean value this
    // API documents.
    unsafe {
        objc2_application_services::AXIsProcessTrustedWithOptions(Some(opts.as_opaque()))
    }
}

/// `AXIsProcessTrusted()` — real AppKit/ApplicationServices call, read-only
/// (does not trigger the system permission prompt, and does not register the
/// app in the Accessibility list; see `prompt_for_accessibility`).
#[cfg(target_os = "macos")]
pub fn accessibility_granted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

/// Non-macOS: this CLI's `dictate` live loop is macOS-only in this MVP (see
/// `dictate::run`), so there is no accessibility-equivalent gate to check on
/// other platforms. Reporting `true` here is not a claim of "granted" — it
/// means "not applicable," and `dictate::run` refuses on non-macOS for an
/// unrelated, more fundamental reason before this value is ever consulted.
#[cfg(not(target_os = "macos"))]
pub fn accessibility_granted() -> bool {
    true
}
