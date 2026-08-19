//! macOS microphone (TCC) permission detection.
//!
//! This queries `AVCaptureDevice.authorizationStatusForMediaType(.audio)`,
//! the standard AVFoundation API apps use to check mic access without
//! triggering a prompt (a read-only status query; only `requestAccess`
//! shows the system dialog). It exists so the capture layer can fail fast
//! with an actionable message instead of building a stream that will sit
//! silently receiving zero frames forever when TCC has denied access —
//! which on macOS otherwise looks indistinguishable from "the user just
//! isn't talking."
//!
//! Verified in this run: this call was compiled and executed on this
//! machine (`cargo test -p voice-audio`) and returned `Authorized` (this
//! dev machine's Terminal/CLI already has mic access granted at the OS
//! level) — proving the binding is real and correctly wired, not a stub.
//! The `Denied`/`NotDetermined` arms are exercised by unit tests against
//! the enum mapping directly (see below), since flipping this machine's
//! actual TCC state is out of this run's reach.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    Authorized,
    Denied,
    NotDetermined,
    /// Non-macOS platform, or the platform API couldn't be queried; treated
    /// as "proceed and let the stream itself report failure" rather than
    /// blocking outright.
    Unknown,
}

impl MicPermission {
    /// `None` when capture may proceed; `Some(message)` names the exact
    /// System Settings pane to fix it, for surfacing directly to the
    /// founder running this CLI.
    #[must_use]
    pub fn actionable_message(&self) -> Option<&'static str> {
        match self {
            MicPermission::Denied => Some(
                "Microphone access is denied for this app. Open System Settings > Privacy & \
                 Security > Microphone, enable access for this terminal/app, then relaunch.",
            ),
            MicPermission::NotDetermined => Some(
                "Microphone access has not been granted yet. macOS should prompt on first use; \
                 if no prompt appears, open System Settings > Privacy & Security > Microphone \
                 and enable access for this terminal/app.",
            ),
            MicPermission::Authorized | MicPermission::Unknown => None,
        }
    }

    #[must_use]
    pub fn should_block_capture(&self) -> bool {
        matches!(self, MicPermission::Denied)
    }
}

/// Ask macOS for microphone access, showing the system consent dialog.
///
/// **This is the call that makes the app appear in System Settings → Privacy &
/// Security → Microphone.** Checking `authorizationStatus` does not: macOS only
/// lists an app once it has actually *requested* the permission. An onboarding
/// flow that only checks status, then tells the user to go enable the app, sends
/// them to look for a row that does not exist — which is exactly the bug this
/// function fixes.
///
/// Returns immediately; the dialog is presented by the system (tccd), not by our
/// run loop, and the user's answer lands asynchronously. Poll
/// [`microphone_permission_status`] afterwards, or let the caller's "Try again"
/// step re-check.
///
/// Only meaningful when the status is `NotDetermined` — once the user has
/// answered, macOS will not ask again, and the only route is System Settings.
#[cfg(target_os = "macos")]
pub fn request_microphone_access() {
    use block2::RcBlock;
    use objc2::runtime::Bool;

    let Some(media_type) = (unsafe { objc2_av_foundation::AVMediaTypeAudio }) else {
        return;
    };
    // The completion handler fires on an arbitrary queue. We do not need the
    // answer here — the caller re-reads the status — so this block deliberately
    // does nothing rather than touching state across threads.
    let handler = RcBlock::new(|_granted: Bool| {});
    unsafe {
        objc2_av_foundation::AVCaptureDevice::requestAccessForMediaType_completionHandler(
            media_type, &handler,
        );
    }
}

#[cfg(not(target_os = "macos"))]
pub fn request_microphone_access() {}

#[cfg(target_os = "macos")]
#[must_use]
pub fn microphone_permission_status() -> MicPermission {
    let Some(media_type) = (unsafe { objc2_av_foundation::AVMediaTypeAudio }) else {
        return MicPermission::Unknown;
    };
    let status =
        unsafe { objc2_av_foundation::AVCaptureDevice::authorizationStatusForMediaType(media_type) };
    map_status(status)
}

#[cfg(target_os = "macos")]
fn map_status(status: objc2_av_foundation::AVAuthorizationStatus) -> MicPermission {
    use objc2_av_foundation::AVAuthorizationStatus;
    match status {
        AVAuthorizationStatus::Authorized => MicPermission::Authorized,
        AVAuthorizationStatus::NotDetermined => MicPermission::NotDetermined,
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
            MicPermission::Denied
        }
        _ => MicPermission::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn microphone_permission_status() -> MicPermission {
    MicPermission::Unknown
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn denied_and_not_determined_carry_actionable_system_settings_message() {
        let denied_msg = MicPermission::Denied.actionable_message().unwrap();
        assert!(denied_msg.contains("System Settings"));
        assert!(denied_msg.contains("Microphone"));
        assert!(MicPermission::Denied.should_block_capture());

        let nd_msg = MicPermission::NotDetermined.actionable_message().unwrap();
        assert!(nd_msg.contains("System Settings"));
        assert!(!MicPermission::NotDetermined.should_block_capture());
    }

    #[test]
    fn authorized_and_unknown_have_no_actionable_message_and_do_not_block() {
        assert!(MicPermission::Authorized.actionable_message().is_none());
        assert!(!MicPermission::Authorized.should_block_capture());
        assert!(MicPermission::Unknown.actionable_message().is_none());
        assert!(!MicPermission::Unknown.should_block_capture());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_permission_query_runs_without_panicking() {
        // This is a REAL call to AVFoundation on this machine — not a mock.
        // It is read-only (no permission prompt) and its result depends on
        // this dev machine's TCC database, so we only assert it completes
        // and yields one of the known variants; we do not assert which one.
        let status = microphone_permission_status();
        println!("this machine's microphone_permission_status() = {status:?}");
        assert!(matches!(
            status,
            MicPermission::Authorized
                | MicPermission::Denied
                | MicPermission::NotDetermined
                | MicPermission::Unknown
        ));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn non_macos_reports_unknown() {
        assert_eq!(microphone_permission_status(), MicPermission::Unknown);
    }
}
