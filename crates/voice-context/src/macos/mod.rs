//! macOS `ContextProvider` backend: `NSWorkspace` frontmost app +
//! `AXUIElement` focused element, behind the crate's existing platform
//! boundary (`crate::provider::ContextProvider`) — additive next to
//! [`crate::provider::FixtureContextProvider`], not a replacement.
//!
//! Module layout mirrors the platform-boundary rule from `docs/voice/PORTING.md`:
//! * [`ax`] is the ONLY place that touches `objc2*` types — raw reads in,
//!   plain owned Rust values out.
//! * [`classify`] is pure data + a pure function (`bundle_id -> AppKind`),
//!   independently testable with no platform calls at all.
//! * This module is the glue: it owns the non-blocking / timeout state
//!   machine and translates `ax`'s raw types into the crate's public
//!   `ContextSnapshot` / `ActionableElement` vocabulary.
//!
//! ## Non-blocking contract (SPEC 3.1)
//!
//! `capture()` returns the previous snapshot immediately — see
//! `ContextProvider::capture`'s doc comment for the contract this
//! implements. The real read runs on a background thread; that thread in
//! turn gives the actual `NSWorkspace`/`AXUIElement` work its own timeout
//! (see [`read_snapshot_with`]) so a hung target app degrades the result
//! (`Coverage::Unavailable { reason: DegradedReason::Timeout }`) instead of
//! blocking forever. Rust cannot preempt a thread parked in a blocking FFI
//! call, so the abandoned reader thread is not killed — it finishes on its
//! own schedule and its (by-then-irrelevant) result is simply discarded.
//! This is a documented, deliberate trade-off, not an oversight: the
//! alternative (no timeout at all) would let one hung app silently stall
//! bias/raw-paste context forever, which is strictly worse.
//!
//! ## Honest degradation (task point 4)
//!
//! An `ActionableElement` is only ever constructed from a REAL, successful
//! AX read (see `real_reader`'s `Ok(raw)` branch). When the read fails —
//! for any reason, including no Accessibility permission — no element is
//! fabricated: the snapshot's `focused_element` is `None` and
//! `actionable_map` carries `Coverage::Unavailable` with a `DegradedReason`
//! that distinguishes "no permission" (`NoAccessibilityPermission`) from
//! "the app has nothing focused right now" (`Unknown` with a descriptive
//! message) from "the read timed out" (`Timeout`) — three different
//! situations that need three different pieces of user-facing advice, per
//! the task's explicit requirement. In particular, `secure` and `writable`
//! on a real element are always derived from an actual attribute read
//! (`AXSubrole == AXSecureTextField`, `AXValue`'s settable bit) — never a
//! hardcoded default the way `voice-cli`'s current `CliInsertionBackend`
//! stub does; see the recon note this task shipped with.

mod ax;
mod classify;

pub use classify::{classify_bundle_id, BundleIdRule, BUNDLE_ID_RULES};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::map::ActionableMap;
use crate::provider::{ContextCapture, ContextProvider, ContextSnapshot, PendingContext};
use crate::types::{ActionableElement, AppInfo, AppKind, Coverage, DegradedReason, ElementRole, Position};

/// Budget for a single `NSWorkspace` + `AXUIElement` read before we give up
/// and degrade rather than stall (SPEC 3.1: "An AX call CAN hang on an
/// unresponsive app"). Chosen generously relative to what manual testing
/// observed — every read against a real terminal (iTerm2) and a real
/// browser (Chrome) resolved in low single-digit milliseconds — while still
/// being short enough that a genuinely hung app can't meaningfully delay
/// the staleness-policy read for the NEXT utterance.
pub const DEFAULT_AX_TIMEOUT: Duration = Duration::from_millis(300);

/// macOS [`ContextProvider`]: frontmost app via `NSWorkspace`, focused
/// element via `AXUIElement`. See the module doc comment for the
/// non-blocking and honest-degradation contracts this implements.
pub struct MacosContextProvider {
    /// The most recently RESOLVED snapshot — what the next `capture()` call
    /// hands back as "previous" per SPEC 3.3's staleness policy. `Arc` (not
    /// borrowed from `&self`) specifically so the background thread spawned
    /// by `capture()` can update it after `capture()` itself has already
    /// returned, without needing `self: Arc<Self>` at the call site.
    previous: Arc<Mutex<ContextSnapshot>>,
    seq: Arc<AtomicU64>,
    timeout: Duration,
}

impl Default for MacosContextProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosContextProvider {
    /// A provider using [`DEFAULT_AX_TIMEOUT`].
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_AX_TIMEOUT)
    }

    /// A provider with an explicit per-read timeout — the knob the
    /// non-blocking tests below turn all the way down to prove the
    /// degrade-on-timeout path deterministically.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { previous: Arc::new(Mutex::new(initial_snapshot())), seq: Arc::new(AtomicU64::new(0)), timeout }
    }
}

/// The honest zero-state before the first read has ever resolved: no app,
/// no element, and a map that says so via `Coverage::Unavailable` rather
/// than an empty `Coverage::Full`.
fn initial_snapshot() -> ContextSnapshot {
    ContextSnapshot {
        frontmost_app: None,
        focused_element: None,
        actionable_map: ActionableMap::unavailable(DegradedReason::Unknown("not yet captured".to_string())),
        seq: 0,
    }
}

impl ContextProvider for MacosContextProvider {
    fn capture(&self) -> ContextCapture {
        let previous_snapshot = lock_clone(&self.previous);
        let shared = Arc::clone(&self.previous);
        let seq_counter = Arc::clone(&self.seq);
        let timeout = self.timeout;

        // Spawn-and-return: nothing on the calling thread waits for this.
        // `capture()` returns as soon as this thread is spawned, regardless
        // of how long the real read (inside `read_snapshot_with`, on ITS
        // OWN child thread) takes.
        let (result_tx, result_rx) = mpsc::channel::<ContextSnapshot>();
        thread::spawn(move || {
            let seq = seq_counter.fetch_add(1, Ordering::SeqCst) + 1;
            let snapshot = read_snapshot_with(timeout, seq, real_reader);
            if let Ok(mut guard) = shared.lock() {
                *guard = snapshot.clone();
            }
            let _ = result_tx.send(snapshot);
        });

        ContextCapture { snapshot: previous_snapshot, pending: Some(PendingContext::new(result_rx)) }
    }
}

fn lock_clone(m: &Mutex<ContextSnapshot>) -> ContextSnapshot {
    match m.lock() {
        Ok(guard) => guard.clone(),
        // A panic while holding the lock elsewhere shouldn't turn into a
        // permanent hang for every future capture(); recover the (possibly
        // stale but still valid) data rather than propagate the poison.
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Run `reader` (the real `NSWorkspace`/`AXUIElement` work) on its own
/// thread and wait for it for at most `timeout`. On timeout, return a
/// `Coverage::Unavailable { reason: DegradedReason::Timeout }` snapshot
/// instead of blocking indefinitely.
///
/// Generic over `reader` specifically so this timeout/degrade machinery —
/// the part of the non-blocking contract most worth pinning with a test —
/// is unit-testable with a synthetic slow closure, with no dependency on
/// real AX permission or a real hung app (see the `tests` module below).
fn read_snapshot_with<F>(timeout: Duration, seq: u64, reader: F) -> ContextSnapshot
where
    F: FnOnce() -> ContextSnapshot + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<ContextSnapshot>();
    thread::spawn(move || {
        let _ = tx.send(reader());
    });
    match rx.recv_timeout(timeout) {
        Ok(mut snapshot) => {
            snapshot.seq = seq;
            snapshot
        }
        Err(_) => timeout_snapshot(seq),
    }
}

fn timeout_snapshot(seq: u64) -> ContextSnapshot {
    ContextSnapshot { frontmost_app: None, focused_element: None, actionable_map: ActionableMap::unavailable(DegradedReason::Timeout), seq }
}

/// The real reader: `NSWorkspace` frontmost app + `AXUIElement` focused
/// element, translated into this crate's vocabulary. Never called directly
/// by `capture()` — always through `read_snapshot_with`, which is what
/// enforces the timeout.
fn real_reader() -> ContextSnapshot {
    let frontmost = ax::read_frontmost();

    let frontmost_app = frontmost.as_ref().map(|f| AppInfo {
        name: f.name.clone().or_else(|| f.bundle_id.clone()).unwrap_or_else(|| "Unknown".to_string()),
        kind: f.bundle_id.as_deref().map(classify::classify_bundle_id).unwrap_or(AppKind::Other),
    });

    let (focused_element, actionable_map) = match frontmost.as_ref().map(|f| f.pid) {
        None => (
            None,
            ActionableMap::unavailable(DegradedReason::Unknown("NSWorkspace reported no frontmost application".to_string())),
        ),
        Some(pid) => match ax::read_focused_element(pid) {
            Ok(raw) => {
                let element = to_actionable_element(&raw);
                // Only the focused element is read here, not a full
                // AXChildren tree walk of the window — Partial, not Full,
                // and says exactly what's missing rather than implying a
                // complete actionable-element map.
                let map = ActionableMap::new(
                    vec![element.clone()],
                    Coverage::Partial {
                        reason: DegradedReason::Unknown(
                            "only the focused element is read; a full AX tree walk is out of scope for this backend".to_string(),
                        ),
                    },
                );
                (Some(element), map)
            }
            Err(err) => (None, ActionableMap::unavailable(degrade_reason_for(err))),
        },
    };

    // seq is a placeholder here — read_snapshot_with overwrites it with the
    // caller-assigned monotonic value once this closure returns.
    ContextSnapshot { frontmost_app, focused_element, actionable_map, seq: 0 }
}

/// Map an AX read failure onto the reason a caller needs to give the user
/// different advice for (task point 4: "no permission" vs "app exposes
/// nothing" are different situations).
fn degrade_reason_for(err: ax::AxReadError) -> DegradedReason {
    match err {
        ax::AxReadError::NoPermission => DegradedReason::NoAccessibilityPermission,
        ax::AxReadError::NoFocusedElement => {
            DegradedReason::Unknown("app reports no focused UI element right now (no window focused, or the app does not participate in AX)".to_string())
        }
        // kAXErrorCannotComplete is Apple's own documented signal for "the
        // target application did not respond" — the same failure mode our
        // own timeout exists to catch, so it's classified the same way.
        ax::AxReadError::CannotComplete => DegradedReason::Timeout,
        ax::AxReadError::Other(code) => DegradedReason::Unknown(format!("AXError({code})")),
    }
}

fn to_actionable_element(raw: &ax::RawFocusedElement) -> ActionableElement {
    // Measured, not fabricated: this branch only runs when the AX read of
    // `raw` actually succeeded, so `secure`/`writable` below are real
    // attribute reads, never a hardcoded stand-in.
    let secure = raw.subrole.as_deref() == Some(ax::AX_SECURE_TEXT_FIELD_SUBROLE);

    let label = raw
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| raw.description.as_deref().filter(|s| !s.trim().is_empty()))
        .unwrap_or("")
        .to_string();

    let (x, y) = raw.position.unwrap_or((0.0, 0.0));
    let (width, height) = raw.size.unwrap_or((0.0, 0.0));

    ActionableElement {
        role: ax_role_to_element_role(raw.role.as_deref(), raw.subrole.as_deref()),
        label,
        position: Position { x, y, width, height },
        // Fail-closed: if AXValue's settable bit couldn't be read at all,
        // `raw.value_settable` is already `false` (see ax.rs) — "assume not
        // writable" is the safe direction to default an unknown in, unlike
        // defaulting `secure` false would be.
        writable: raw.value_settable,
        secure,
        // AXEnabled is frequently just absent for perfectly ordinary,
        // working elements (observed live on iTerm2's own focused
        // AXTextArea). Treating "attribute not exposed" as enabled matches
        // the conventional AX consumer behavior: enabled is the default
        // state, and explicit disablement is what apps are expected to
        // signal when it applies.
        enabled: raw.enabled.unwrap_or(true),
    }
}

fn ax_role_to_element_role(role: Option<&str>, subrole: Option<&str>) -> ElementRole {
    let Some(role) = role else {
        return ElementRole::Other("Unknown".to_string());
    };
    match (role, subrole) {
        (_, Some("AXTabButton")) => ElementRole::Tab,
        ("AXButton", _) => ElementRole::Button,
        ("AXTextField", _) | ("AXTextArea", _) | ("AXComboBox", _) => ElementRole::TextField,
        ("AXCheckBox", _) => ElementRole::CheckBox,
        ("AXMenuItem", _) | ("AXMenuBarItem", _) => ElementRole::MenuItem,
        ("AXLink", _) => ElementRole::Link,
        ("AXSlider", _) => ElementRole::Slider,
        ("AXWindow", _) => ElementRole::Window,
        (other, _) => ElementRole::Other(other.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn fake_ready_snapshot(seq: u64) -> ContextSnapshot {
        ContextSnapshot {
            frontmost_app: Some(AppInfo { name: "Fake".to_string(), kind: AppKind::Other }),
            focused_element: None,
            actionable_map: ActionableMap::unavailable(DegradedReason::Unknown("fake".to_string())),
            seq,
        }
    }

    #[test]
    fn read_snapshot_with_returns_reader_result_within_timeout() {
        let snapshot = read_snapshot_with(Duration::from_secs(2), 5, || fake_ready_snapshot(999));
        // seq is overwritten by the caller-assigned value, not the reader's.
        assert_eq!(snapshot.seq, 5);
        assert_eq!(snapshot.frontmost_app.as_ref().unwrap().name, "Fake");
    }

    #[test]
    fn read_snapshot_with_degrades_on_timeout_instead_of_hanging() {
        // The core "give every AX read a timeout and degrade rather than
        // stall" contract (task point 3), proven deterministically: the
        // reader closure below never returns within any reasonable test
        // budget, simulating a genuinely hung AX call into an unresponsive
        // app. read_snapshot_with must still return quickly.
        let start = Instant::now();
        let snapshot = read_snapshot_with(Duration::from_millis(50), 7, || {
            thread::sleep(Duration::from_secs(3600));
            fake_ready_snapshot(999) // never reached
        });
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(1000), "read_snapshot_with took {elapsed:?}, expected to give up at ~50ms");
        assert_eq!(snapshot.seq, 7, "a degraded snapshot must still carry the caller-assigned seq");
        assert_eq!(snapshot.frontmost_app, None, "a timed-out read must not fabricate an app");
        assert_eq!(snapshot.focused_element, None, "a timed-out read must not fabricate an element");
        assert_eq!(snapshot.actionable_map.coverage(), &Coverage::Unavailable { reason: DegradedReason::Timeout });
    }

    #[test]
    fn capture_returns_before_the_background_read_can_have_completed() {
        // Whole-provider version of the same contract: even with a
        // deliberately huge timeout budget (so a slow REAL AX read would
        // never trigger read_snapshot_with's own timeout path), capture()
        // itself must still return immediately — it only spawns the
        // background thread, it never joins it.
        let provider = MacosContextProvider::with_timeout(Duration::from_secs(30));
        let start = Instant::now();
        let capture = provider.capture();
        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_millis(200), "capture() took {elapsed:?}, must return near-instantly");
        assert_eq!(capture.snapshot.seq, 0, "first call must hand back the 'not yet captured' placeholder");
        assert!(capture.pending.is_some(), "a real provider always has a read in flight");
    }

    #[test]
    fn second_capture_sees_first_captures_resolved_read_as_previous() {
        // SPEC 3.3 staleness policy end-to-end: previous -> resolve -> next
        // capture's "previous" is what just resolved. Exercises the real
        // NSWorkspace/AX path (this crate's cfg gate keeps the whole module
        // macOS-only); on a machine without the Accessibility grant this
        // still passes because the degrade path is itself a valid resolved
        // snapshot, just with Coverage::Unavailable.
        let provider = MacosContextProvider::with_timeout(Duration::from_secs(5));

        let first = provider.capture();
        assert_eq!(first.snapshot.seq, 0);
        let resolved = first.pending.expect("pending on first call").wait().expect("background read should complete");
        assert_eq!(resolved.seq, 1);

        let second = provider.capture();
        assert_eq!(second.snapshot.seq, 1, "second capture's previous must be the first capture's resolved snapshot");
    }

    #[test]
    fn ax_role_mapping_covers_the_common_roles() {
        assert_eq!(ax_role_to_element_role(Some("AXButton"), None), ElementRole::Button);
        assert_eq!(ax_role_to_element_role(Some("AXTextArea"), None), ElementRole::TextField);
        assert_eq!(ax_role_to_element_role(Some("AXTextField"), None), ElementRole::TextField);
        assert_eq!(ax_role_to_element_role(Some("AXCheckBox"), None), ElementRole::CheckBox);
        assert_eq!(ax_role_to_element_role(Some("AXMenuItem"), None), ElementRole::MenuItem);
        assert_eq!(ax_role_to_element_role(Some("AXLink"), None), ElementRole::Link);
        assert_eq!(ax_role_to_element_role(Some("AXSlider"), None), ElementRole::Slider);
        assert_eq!(ax_role_to_element_role(Some("AXWindow"), None), ElementRole::Window);
        assert_eq!(ax_role_to_element_role(Some("AXRadioButton"), Some("AXTabButton")), ElementRole::Tab);
        assert_eq!(ax_role_to_element_role(Some("AXWebArea"), None), ElementRole::Other("AXWebArea".to_string()));
        assert_eq!(ax_role_to_element_role(None, None), ElementRole::Other("Unknown".to_string()));
    }

    #[test]
    fn to_actionable_element_marks_secure_only_from_a_real_subrole_match() {
        let secure_raw = ax::RawFocusedElement {
            role: Some("AXTextField".to_string()),
            subrole: Some(ax::AX_SECURE_TEXT_FIELD_SUBROLE.to_string()),
            title: None,
            description: None,
            value_settable: true,
            enabled: Some(true),
            position: Some((10.0, 20.0)),
            size: Some((100.0, 20.0)),
        };
        let el = to_actionable_element(&secure_raw);
        assert!(el.secure, "AXSecureTextField subrole must mark the element secure");
        assert_eq!(el.position, Position { x: 10.0, y: 20.0, width: 100.0, height: 20.0 });

        let non_secure_raw = ax::RawFocusedElement { subrole: None, ..secure_raw };
        let el2 = to_actionable_element(&non_secure_raw);
        assert!(!el2.secure);
    }

    #[test]
    fn to_actionable_element_prefers_title_falls_back_to_description() {
        let with_title = ax::RawFocusedElement {
            role: Some("AXButton".to_string()),
            subrole: None,
            title: Some("Send".to_string()),
            description: Some("ignored".to_string()),
            value_settable: false,
            enabled: None,
            position: None,
            size: None,
        };
        assert_eq!(to_actionable_element(&with_title).label, "Send");

        let title_blank = ax::RawFocusedElement { title: Some("   ".to_string()), ..with_title.clone() };
        assert_eq!(to_actionable_element(&title_blank).label, "ignored");

        let neither = ax::RawFocusedElement { title: None, description: None, ..with_title };
        assert_eq!(to_actionable_element(&neither).label, "");
        // Missing position/size must not be silently claimed as "at the
        // origin with real geometry" by the caller — see doc comment.
        assert_eq!(to_actionable_element(&neither).position, Position { x: 0.0, y: 0.0, width: 0.0, height: 0.0 });
        // AXEnabled absent (None) defaults to enabled — see doc comment on
        // to_actionable_element for the cited convention.
        assert!(to_actionable_element(&neither).enabled);
    }

    #[test]
    fn degrade_reason_distinguishes_permission_from_empty_focus_from_timeout() {
        assert_eq!(degrade_reason_for(ax::AxReadError::NoPermission), DegradedReason::NoAccessibilityPermission);
        assert_eq!(degrade_reason_for(ax::AxReadError::CannotComplete), DegradedReason::Timeout);
        match degrade_reason_for(ax::AxReadError::NoFocusedElement) {
            DegradedReason::Unknown(msg) => assert!(msg.contains("no focused")),
            other => panic!("expected Unknown, got {other:?}"),
        }
        // Permission and "nothing focused" must be genuinely distinct
        // reasons — task point 4's explicit requirement — not the same
        // value under two names.
        assert_ne!(degrade_reason_for(ax::AxReadError::NoPermission), degrade_reason_for(ax::AxReadError::NoFocusedElement));
    }
}
