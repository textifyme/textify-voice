//! What this operating system can actually do, declared rather than assumed.
//!
//! The rule this type exists to enforce: **a capability we lack must be
//! visible, never silent.** Concretely, each field corresponds to a way a
//! dictation tool can appear to work while quietly failing:
//!
//! * `can_inject_keystroke` — Windows UIPI blocks `SendInput` into elevated
//!   processes, and Wayland blocks synthetic input outright unless a portal
//!   session is established. Without it, text still reaches the clipboard;
//!   the user just pastes it themselves. That is a fine product. Pretending
//!   we pasted when we did not is not.
//! * `can_overlay` — GNOME/Wayland has no `layer-shell`, so an always-on-top
//!   indicator may be impossible.
//! * `can_detect_secure_field` — if false, we cannot honour COMMANDS-SPEC
//!   3.5 #3's refusal to type into a password field, and must say so rather
//!   than implying protection we do not have. (macOS is `true`: a live
//!   `voice_context::MacosContextProvider` AX read now feeds
//!   `CliInsertionBackend::current_target()`'s secure-field refusal.)
//! * `can_read_focused_app` — gates bias layer 2 and the app-kind raw-paste
//!   rule. Without it both silently no-op, which looks like a quality
//!   problem rather than a missing capability.
//! * `can_show_status_ui` — whether this platform has any menu-bar/system-tray
//!   surface at all to host `platform::StatusUi`. macOS always does
//!   (`NSStatusBar`); a Linux port may not (no StatusNotifierItem host on
//!   some minimal window managers, the same category of gap `can_overlay`
//!   already covers for GNOME/Wayland). Without it, `platform::NullStatusUi`
//!   is used and the agent loop still runs — the status item is chrome, not
//!   a delivery mechanism, so its absence is a visible gap, not a refusal.

/// Capability report for the running platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformCaps {
    /// Push-to-talk on a bare modifier (distinct key-down and key-up).
    pub can_hold_bare_modifier: bool,
    /// Synthesize the paste keystroke ourselves.
    pub can_inject_keystroke: bool,
    /// Commit text directly as an input method — no injection permission
    /// required. The preferred Linux path (IBus / `input-method-v2`).
    pub can_commit_via_ime: bool,
    /// Place an always-on-top, non-activating overlay.
    pub can_overlay: bool,
    /// Identify the frontmost application (drives bias + app-kind rules).
    pub can_read_focused_app: bool,
    /// Detect that the focused field is a secure/password input.
    pub can_detect_secure_field: bool,
    /// Host a menu-bar / system-tray status item (`platform::StatusUi`).
    pub can_show_status_ui: bool,
}

impl PlatformCaps {
    /// Nothing supported — the honest default for an unported platform.
    ///
    /// Unused on macOS (the `unsupported` backend is `cfg`'d out there), which
    /// is exactly why it must not be deleted: it is the value every future
    /// port starts from.
    #[allow(dead_code)]
    pub const NONE: Self = Self {
        can_hold_bare_modifier: false,
        can_inject_keystroke: false,
        can_commit_via_ime: false,
        can_overlay: false,
        can_read_focused_app: false,
        can_detect_secure_field: false,
        can_show_status_ui: false,
    };

    /// True when text can reach the focused field without the user pasting
    /// manually, by whichever mechanism this platform offers.
    pub fn can_deliver_text(&self) -> bool {
        self.can_inject_keystroke || self.can_commit_via_ime
    }

    /// Lines describing every capability we lack, for printing at startup.
    ///
    /// Deliberately returns gaps rather than a full report: the user does not
    /// need to be told what works, only what will surprise them.
    pub fn gaps(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.can_hold_bare_modifier {
            out.push("hold-to-talk is unavailable on this platform -- using tap-to-toggle instead");
        }
        if !self.can_deliver_text() {
            out.push("text cannot be inserted automatically -- it will be copied to the clipboard for you to paste");
        }
        if !self.can_overlay {
            out.push("no listening indicator on this platform -- watch the terminal instead");
        }
        if !self.can_read_focused_app {
            out.push("the frontmost app cannot be identified -- screen-derived bias terms and the raw-paste rule for code/terminal apps are inactive");
        }
        if !self.can_detect_secure_field {
            out.push("secure fields cannot be detected -- do NOT dictate into a password field");
        }
        if !self.can_show_status_ui {
            out.push("no menu-bar status item on this platform -- watch the terminal instead");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn an_unported_platform_admits_to_everything() {
        let caps = PlatformCaps::NONE;
        assert!(!caps.can_deliver_text());
        // Every single capability must produce a warning line. A gap that
        // does not surface is exactly the silent failure this type exists
        // to prevent.
        assert_eq!(caps.gaps().len(), 6);
    }

    #[test]
    fn ime_alone_counts_as_delivering_text() {
        // The Linux path: committing as an input method needs no injection
        // permission at all. If this ever regressed to requiring
        // can_inject_keystroke, Wayland would report a false gap and users
        // would be told to paste manually when they did not have to.
        let caps = PlatformCaps { can_commit_via_ime: true, ..PlatformCaps::NONE };
        assert!(caps.can_deliver_text());
        assert!(!caps.gaps().iter().any(|g| g.contains("clipboard")));
    }

    #[test]
    fn a_fully_capable_platform_reports_no_gaps() {
        let caps = PlatformCaps {
            can_hold_bare_modifier: true,
            can_inject_keystroke: true,
            can_commit_via_ime: false,
            can_overlay: true,
            can_read_focused_app: true,
            can_detect_secure_field: true,
            can_show_status_ui: true,
        };
        assert!(caps.gaps().is_empty());
    }

    #[test]
    fn todays_macos_reports_zero_gaps_now_that_ax_context_is_wired() {
        // Pins the current honest state, updated from the two-gaps era this
        // test's old name and body described: a live AX reader
        // (`voice_context::MacosContextProvider`) is now wired into
        // `dictate.rs` on both fronts the two flags below gate --
        // frontmost-app detection feeds `BiasContext`/`app_kind`, and the
        // focused element's secure subrole feeds `CliInsertionBackend`'s
        // secure-field refusal -- so macOS has no remaining capability gap.
        // If a future change breaks either wiring without updating this
        // pin, this test fails loudly instead of silently regressing to a
        // platform that claims a capability it no longer has.
        let caps = super::super::current_caps();
        if cfg!(target_os = "macos") {
            assert!(caps.can_hold_bare_modifier);
            assert!(caps.can_inject_keystroke);
            assert!(caps.can_overlay);
            assert!(caps.can_read_focused_app, "the live AX context provider is wired -- this must be true");
            assert!(
                caps.can_detect_secure_field,
                "the live AX context provider is wired -- this must be true"
            );
            assert!(caps.can_show_status_ui, "NSStatusBar is always available on macOS");
            assert!(caps.gaps().is_empty(), "macOS should report zero capability gaps now");
        } else {
            assert_eq!(caps, PlatformCaps::NONE);
        }
    }
}
