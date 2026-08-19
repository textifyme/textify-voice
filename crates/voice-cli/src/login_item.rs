//! Launch-at-login, via `SMAppService` (ServiceManagement.framework,
//! macOS 13+) rather than a `~/Library/LaunchAgents` plist.
//!
//! **Why `SMAppService` and not a LaunchAgent plist**, argued explicitly per
//! this unit's brief rather than defaulted to: Apple's own header doc on
//! `SMAppService` states the register/unregister APIs are "a replacement for
//! installing plists in `~/Library/LaunchAgents`" -- so a plist is the
//! *legacy* mechanism, not a neutral alternative. Concretely, a plist would
//! cost us two things `SMAppService` gives for free: (1) the
//! `RequiresApproval` status -- a plist-based agent that the user has not
//! approved in System Settings just silently fails to launch, with nothing
//! to poll to explain why, whereas `SMAppService::status()` reports that
//! state directly; and (2) the grant follows the app's own code identity
//! automatically. No `LaunchAgent` fallback is implemented.
//!
//! **The OS floor moved underneath this module.** `docs/voice/DECISIONS.md`
//! recorded a macOS 13+ floor, which is why there was no older-OS reason to
//! prefer the legacy path; the shipped floor is now 11.0 (see
//! `packaging/Info.plist.template` and `crate::compat`'s Decision 2), which is
//! BELOW `SMAppService`'s macOS 13 requirement. Nothing calls the
//! `SMAppService` paths today — only the pure-Rust bundle detection is used —
//! but wiring the launch-at-login toggle to the UI needs an availability guard
//! first (`crate::compat::meets_floor`, or `objc2`'s `available!`), or it is a
//! missing-selector crash on macOS 11/12 rather than a compile error.
//!
//! The Objective-C binding is `objc2-service-management` (part of the same
//! `objc2` project already used throughout `voice-cli` for AppKit/Core
//! Graphics/Core Foundation) -- a real, working crate, not hand-rolled
//! externs; see this crate's `Cargo.toml` for the added dependency.
//!
//! **Side-effect discipline (hard requirement of this unit):** `enable()`
//! and `disable()` are the *only* two functions here that mutate anything,
//! and neither is ever called except in direct response to an explicit user
//! action (a settings toggle). Nothing in this module runs at import time,
//! at construction time, or from a test. A CLI/menu-bar app that registers
//! itself as a login item without being asked is hostile, and on a real
//! user's machine that is not a hypothetical.
//!
//! **Bundle requirement:** `SMAppService::mainAppService()` resolves "the
//! main app" from the *running process's own code identity* -- which only
//! exists when the process is the executable inside a real `Foo.app`
//! bundle (`Foo.app/Contents/MacOS/Foo`), signed, with a `CFBundleIdentifier`.
//! Invoked as a loose binary (`cargo run`, a bare `target/release/textify-voice`),
//! there is no bundle for it to resolve, so `enable()` checks this itself and
//! fails with an actionable message rather than letting the ObjC call fail
//! opaquely (confirmed empirically: a bare dev binary reports `NotFound`,
//! not an error, from `status()` -- see this module's tests and this unit's
//! verification notes for the real, executed query).
//!
//! `#![allow(dead_code)]`: this module is not yet wired into `main.rs` --
//! that wiring is a settings-toggle UI, a separate unit -- so nothing calls
//! its public API today. Same pattern already used in this crate for
//! `platform::caps::PlatformCaps::NONE`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Current login-item registration status.
///
/// Deliberately not a `bool`: `SMAppService` has a state between "off" and
/// "on" -- registered, but pending the user's explicit approval in
/// System Settings > General > Login Items -- and users hit it constantly
/// (macOS shows this on essentially every first registration). Collapsing
/// that into `is_enabled() == false` makes a working registration look like
/// a broken feature; callers that need the nuance should match on this
/// enum, and `is_enabled()`/`needs_user_approval()` below are the
/// bool-shaped conveniences for callers that don't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    /// No registration exists (never registered, or was unregistered).
    NotRegistered,
    /// Registered and currently eligible to launch at login.
    Enabled,
    /// Registered, but the user has not (yet, or no longer) approved it in
    /// System Settings > General > Login Items. `enable()` having returned
    /// `Ok(())` and `status()` then reporting this is the *expected*
    /// first-run outcome, not a failure -- macOS always requires this
    /// confirmation step for a newly-registered login item.
    RequiresApproval,
    /// No such service could be found. Apple's doc for `SMAppService`
    /// describes this as "an error occurred and no such service could be
    /// found" -- in practice, this is what an unbundled dev binary reports
    /// (see `BundleContext`), because there is no stable identity for
    /// `mainAppService()` to resolve.
    NotFound,
}

impl LoginItemStatus {
    /// True only for `Enabled`. Callers that need to tell "off" apart from
    /// "pending approval" (to render the latter as, e.g., "Almost there --
    /// open System Settings" rather than an unchecked toggle) should match
    /// on the full status instead.
    #[must_use]
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// True when the user must act in System Settings before the item will
    /// actually launch at login. `SMAppService::openSystemSettingsLoginItems()`
    /// is the Apple-documented way to send them there directly; not called
    /// by this module (no side effects outside `enable`/`disable`), but a
    /// UI reacting to this state is exactly what it exists for.
    #[must_use]
    pub fn needs_user_approval(self) -> bool {
        matches!(self, Self::RequiresApproval)
    }
}

/// Whether the running process is the executable inside a real `.app`
/// bundle, and the path that answered the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleContext {
    /// `exe` sits at `Foo.app/Contents/MacOS/Foo` -- `SMAppService` has a
    /// stable identity to register against.
    Bundled(PathBuf),
    /// `exe` is a loose binary. `SMAppService` registration is meaningless
    /// here: there is no bundle, so no `CFBundleIdentifier`, so no stable
    /// identity for `mainAppService()` to resolve.
    NotBundled(PathBuf),
}

/// Walks up from an executable path and returns the `.../Foo.app` directory
/// if `exe` sits at the exact `Foo.app/Contents/MacOS/Foo` layout a real app
/// bundle requires, `None` otherwise.
///
/// Pure path logic, no filesystem access -- safe to call (and to test) on a
/// path that does not exist. `Contents` and `MacOS` are fixed, case-exact
/// directory names in Apple's bundle layout (never optional, never
/// differently cased), so this checks them literally rather than
/// case-insensitively.
fn bundle_root(exe: &Path) -> Option<&Path> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()? != "Contents" {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    if app_dir.extension()? != "app" {
        return None;
    }
    Some(app_dir)
}

fn classify_bundle_path(exe: &Path) -> BundleContext {
    if bundle_root(exe).is_some() {
        BundleContext::Bundled(exe.to_path_buf())
    } else {
        BundleContext::NotBundled(exe.to_path_buf())
    }
}

/// Classify the *currently running* process. Real, not simulated: asks the
/// OS for this process's own executable path (`std::env::current_exe`,
/// ordinary `std`, no ObjC) and applies the same bundle-layout check
/// `enable()` uses to decide whether to even attempt registration.
pub fn current_exe_bundle_context() -> Result<BundleContext> {
    let exe = std::env::current_exe()
        .context("could not determine the running executable's own path")?;
    Ok(classify_bundle_path(&exe))
}

/// Convenience boolean over [`current_exe_bundle_context`], for callers (a
/// settings UI, say) that just need to know whether to grey out the
/// launch-at-login toggle and explain why, before the user ever taps it.
pub fn is_running_from_bundle() -> Result<bool> {
    Ok(matches!(current_exe_bundle_context()?, BundleContext::Bundled(_)))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{BundleContext, LoginItemStatus};
    use anyhow::Result;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    /// `SMAppServiceStatus` is a raw `NSInteger` newtype from the
    /// Objective-C side (`pub struct SMAppServiceStatus(pub NSInteger)`),
    /// not an exhaustive Rust enum the compiler can check membership
    /// against -- so this maps the three named non-default cases
    /// explicitly and treats every other raw value (including
    /// `NotRegistered` itself, and any value a future OS version might add)
    /// as "no active registration," which is the safe, non-panicking
    /// default rather than the misleading one.
    fn map_status(status: SMAppServiceStatus) -> LoginItemStatus {
        if status == SMAppServiceStatus::Enabled {
            LoginItemStatus::Enabled
        } else if status == SMAppServiceStatus::RequiresApproval {
            LoginItemStatus::RequiresApproval
        } else if status == SMAppServiceStatus::NotFound {
            LoginItemStatus::NotFound
        } else {
            LoginItemStatus::NotRegistered
        }
    }

    /// Read-only. Never mutates anything -- safe to call at any time,
    /// including at startup, to render current state.
    pub fn status() -> Result<LoginItemStatus> {
        let service = unsafe { SMAppService::mainAppService() };
        let raw = unsafe { service.status() };
        Ok(map_status(raw))
    }

    /// Registers this app as a login item. **Must only be called in direct
    /// response to an explicit user action** (see this module's top-level
    /// docs) -- this function itself does not enforce that; the caller
    /// must.
    ///
    /// Fails fast with an actionable message, rather than letting
    /// `SMAppService` fail opaquely, when the running process is not
    /// inside a real app bundle: there is no stable identity to register.
    pub fn enable() -> Result<()> {
        if let BundleContext::NotBundled(path) = super::current_exe_bundle_context()? {
            anyhow::bail!(
                "launch-at-login needs Textify Voice's signed .app bundle, not a loose \
                 binary -- SMAppService::mainAppService() resolves \"the main app\" from \
                 the running process's own bundle identity, and {} has none. Build/run \
                 the .app bundle (see docs/voice/PORTING.md) and enable launch-at-login \
                 from there, not from a bare `cargo run` / CLI invocation.",
                path.display()
            );
        }

        let service = unsafe { SMAppService::mainAppService() };
        unsafe { service.registerAndReturnError() }.map_err(|err| {
            let message = err.localizedDescription().to_string();
            anyhow::anyhow!("SMAppService could not register the login item: {message}")
        })
    }

    /// Unregisters this app as a login item. Same explicit-user-action-only
    /// contract as `enable`.
    pub fn disable() -> Result<()> {
        let service = unsafe { SMAppService::mainAppService() };
        unsafe { service.unregisterAndReturnError() }.map_err(|err| {
            let message = err.localizedDescription().to_string();
            anyhow::anyhow!("SMAppService could not unregister the login item: {message}")
        })
    }
}

/// Non-macOS: `SMAppService` is a macOS 13+-only framework, and this crate
/// has no other platform's launch-at-login mechanism implemented (see
/// `docs/voice/PORTING.md`). Reporting an error here rather than a fake
/// `NotRegistered`/`false` -- unlike `permissions.rs`'s accessibility check,
/// which reports "not applicable" as `true` because *nothing downstream
/// cares* on that platform, a settings UI showing a launch-at-login toggle
/// very much cares whether flipping it can do anything at all.
#[cfg(not(target_os = "macos"))]
mod imp {
    use super::LoginItemStatus;
    use anyhow::Result;

    pub fn status() -> Result<LoginItemStatus> {
        anyhow::bail!("launch-at-login is only implemented for macOS (SMAppService) in this build")
    }

    pub fn enable() -> Result<()> {
        anyhow::bail!("launch-at-login is only implemented for macOS (SMAppService) in this build")
    }

    pub fn disable() -> Result<()> {
        anyhow::bail!("launch-at-login is only implemented for macOS (SMAppService) in this build")
    }
}

// `#[allow(unused_imports)]`: same "not wired into main.rs yet" reason as
// the file-level `#![allow(dead_code)]` above -- a `pub use` re-export
// with no local caller in a binary crate is flagged the same way a dead
// `pub fn` is.
#[allow(unused_imports)]
pub use imp::{disable, enable, status};

/// `status()?.is_enabled()` -- the bool-shaped convenience for callers that
/// don't need to distinguish "off" from "pending approval." Prefer
/// [`status`] directly wherever that distinction matters to the UI (it
/// almost always does -- see this module's top-level docs).
pub fn is_enabled() -> Result<bool> {
    Ok(status()?.is_enabled())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    // -- bundle detection: pure path logic, no filesystem, no ObjC --

    #[test]
    fn a_real_bundle_layout_is_recognized() {
        let exe = Path::new("/Applications/TextifyVoice.app/Contents/MacOS/textify-voice");
        assert_eq!(
            classify_bundle_path(exe),
            BundleContext::Bundled(exe.to_path_buf())
        );
    }

    #[test]
    fn a_nested_bundle_layout_is_still_recognized() {
        // Bundles can live anywhere on disk (~/Applications, a DMG mount,
        // a build output dir) -- only the last three path segments matter.
        let exe = Path::new("/Users/dev/onetelos/textify/target/release/bundle/TextifyVoice.app/Contents/MacOS/textify-voice");
        assert!(matches!(classify_bundle_path(exe), BundleContext::Bundled(_)));
    }

    #[test]
    fn a_loose_dev_binary_is_not_bundled() {
        let exe = Path::new("/Users/dev/onetelos/textify/target/release/textify-voice");
        assert_eq!(
            classify_bundle_path(exe),
            BundleContext::NotBundled(exe.to_path_buf())
        );
    }

    #[test]
    fn a_path_that_merely_contains_dot_app_is_not_enough() {
        // Must be the *exact* Contents/MacOS layout, not just any path
        // with ".app" somewhere in it.
        let exe = Path::new("/Applications/TextifyVoice.app/textify-voice");
        assert!(matches!(classify_bundle_path(exe), BundleContext::NotBundled(_)));
    }

    #[test]
    fn wrong_case_directory_names_are_not_bundled() {
        // Contents/MacOS are fixed, case-exact names in Apple's bundle
        // spec -- a differently-cased lookalike is not a real bundle.
        let exe = Path::new("/Applications/TextifyVoice.app/contents/macos/textify-voice");
        assert!(matches!(classify_bundle_path(exe), BundleContext::NotBundled(_)));
    }

    #[test]
    fn a_relative_or_root_level_path_does_not_panic() {
        // Regression guard for the `?`-chain in `bundle_root`: paths too
        // short to have the required ancestors must return `None`, not
        // panic on a missing parent.
        assert!(matches!(classify_bundle_path(Path::new("textify-voice")), BundleContext::NotBundled(_)));
        assert!(matches!(classify_bundle_path(Path::new("/")), BundleContext::NotBundled(_)));
    }

    #[test]
    fn current_exe_bundle_context_runs_for_real() {
        // Real, not mocked: asks the OS for this test binary's own path.
        // `cargo test` produces a loose binary under target/.../deps, so
        // this must report NotBundled -- if it ever reported Bundled here,
        // the bundle-layout check itself would be broken.
        let ctx = current_exe_bundle_context().expect("current_exe() must succeed in a test binary");
        assert!(
            matches!(ctx, BundleContext::NotBundled(_)),
            "a `cargo test` binary is never inside an app bundle: got {ctx:?}"
        );
        assert!(!is_running_from_bundle().expect("must succeed"));
    }

    // -- status mapping --

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RawStatusStub {
        NotRegistered,
        Enabled,
        RequiresApproval,
        NotFound,
        FutureUnknownValue,
    }

    /// Mirrors `imp::map_status`'s decision table without touching the
    /// (macOS-only, cfg'd-out-in-non-mac-test-runs) real ObjC type, so the
    /// mapping logic itself is exercised on every platform this crate's
    /// tests run on.
    fn map_status_stub(raw: RawStatusStub) -> LoginItemStatus {
        match raw {
            RawStatusStub::Enabled => LoginItemStatus::Enabled,
            RawStatusStub::RequiresApproval => LoginItemStatus::RequiresApproval,
            RawStatusStub::NotFound => LoginItemStatus::NotFound,
            RawStatusStub::NotRegistered | RawStatusStub::FutureUnknownValue => {
                LoginItemStatus::NotRegistered
            }
        }
    }

    #[test]
    fn status_mapping_covers_every_named_case() {
        assert_eq!(map_status_stub(RawStatusStub::NotRegistered), LoginItemStatus::NotRegistered);
        assert_eq!(map_status_stub(RawStatusStub::Enabled), LoginItemStatus::Enabled);
        assert_eq!(map_status_stub(RawStatusStub::RequiresApproval), LoginItemStatus::RequiresApproval);
        assert_eq!(map_status_stub(RawStatusStub::NotFound), LoginItemStatus::NotFound);
    }

    #[test]
    fn an_unrecognized_raw_status_falls_back_to_not_registered_not_a_panic() {
        assert_eq!(map_status_stub(RawStatusStub::FutureUnknownValue), LoginItemStatus::NotRegistered);
    }

    #[test]
    fn is_enabled_is_true_only_for_enabled() {
        assert!(LoginItemStatus::Enabled.is_enabled());
        assert!(!LoginItemStatus::NotRegistered.is_enabled());
        assert!(!LoginItemStatus::RequiresApproval.is_enabled());
        assert!(!LoginItemStatus::NotFound.is_enabled());
    }

    #[test]
    fn needs_user_approval_is_true_only_for_requires_approval() {
        assert!(LoginItemStatus::RequiresApproval.needs_user_approval());
        assert!(!LoginItemStatus::Enabled.needs_user_approval());
        assert!(!LoginItemStatus::NotRegistered.needs_user_approval());
        assert!(!LoginItemStatus::NotFound.needs_user_approval());
    }
}
