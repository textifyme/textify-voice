//! `ContextProvider`: the async, best-effort context capture contract.
//!
//! SPEC §3.1: "Context capture ... Native AX (macOS `AXUIElement`, Windows
//! UIAutomation) in `crates/voice-context`; **async, best-effort, never
//! blocks first audio frame**." SPEC §3.3: "Assembled async by
//! `voice-context`; NEVER blocks the first audio frame. Staleness policy: an
//! utterance starts with the PREVIOUS context snapshot ... fresh context
//! applies via `update_bias()` where the engine supports it."
//!
//! This module encodes that contract structurally rather than by convention:
//! [`ContextProvider::capture`] takes no async runtime, does no I/O, and
//! cannot block — it returns the previous snapshot plus a [`PendingContext`]
//! handle immediately. A fresher read (real AX/UIA walk in production; a
//! background thread in this fixture) resolves independently and is polled
//! or blocking-waited on by the caller, on the caller's own schedule.

use std::sync::mpsc::{self, Receiver, RecvError, TryRecvError};
use std::thread;

use crate::map::ActionableMap;
use crate::types::{ActionableElement, AppInfo, SecureFieldStatus};

/// A point-in-time read of on-screen context.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextSnapshot {
    pub frontmost_app: Option<AppInfo>,
    pub focused_element: Option<ActionableElement>,
    pub actionable_map: ActionableMap,
    /// Monotonic sequence number assigned by the provider; used to order
    /// snapshots without depending on wall-clock time (kept deterministic
    /// for tests, and immune to clock skew in production).
    pub seq: u64,
}

impl ContextSnapshot {
    /// This snapshot's own answer to "is the focus target a secure field?" —
    /// see [`SecureFieldStatus`] for why this is tri-state rather than
    /// `bool`.
    ///
    /// `Known` only ever comes from an actual, successfully-read
    /// [`ActionableElement`] — never fabricated. Every other case —
    /// `focused_element: None`, for ANY reason (AX read timeout, missing
    /// Accessibility permission, the app reporting no focused element right
    /// now, or an unmapped platform error) — is `Unknown`. Those reasons are
    /// real and distinguishable via `actionable_map.coverage()` for
    /// diagnostics/UX copy, but for the secure-field decision they are
    /// deliberately NOT distinguished from each other: collapsing any of
    /// them to "known safe" is exactly the fail-open bug this type exists
    /// to make unrepresentable. A caller with a specific, positive case to
    /// carve out as known-safe (e.g. "this platform's AX API reliably
    /// reports no-focused-element as a true, non-degraded state") should do
    /// so explicitly and separately, not by weakening this method.
    #[must_use]
    pub fn secure_field_status(&self) -> SecureFieldStatus {
        match &self.focused_element {
            Some(el) => SecureFieldStatus::Known(el.secure),
            None => SecureFieldStatus::Unknown,
        }
    }
}

/// Outcome of a non-blocking poll on a [`PendingContext`].
#[derive(Debug, Clone, PartialEq)]
pub enum PollResult {
    /// The fresher read has not completed yet — try again later, or proceed
    /// with the snapshot already in hand.
    NotReady,
    /// The fresher read completed.
    Ready(ContextSnapshot),
    /// The background read failed or was abandoned; there will be no
    /// fresher snapshot for this handle.
    Disconnected,
}

/// A handle to a context read in flight. Never produced by blocking work on
/// the caller's thread — see [`ContextProvider::capture`].
pub struct PendingContext {
    rx: Receiver<ContextSnapshot>,
}

impl PendingContext {
    /// `pub(crate)`, not private: real platform backends (e.g.
    /// `crate::macos::MacosContextProvider`) live in sibling modules and
    /// need to hand back a `PendingContext` of their own construction, the
    /// same way `FixtureContextProvider` does from within this module.
    pub(crate) fn new(rx: Receiver<ContextSnapshot>) -> Self {
        Self { rx }
    }

    /// Non-blocking check: is the fresher snapshot ready yet? This is the
    /// method the hot path is expected to use — it returns immediately
    /// regardless of how far along the background read is.
    pub fn try_poll(&self) -> PollResult {
        match self.rx.try_recv() {
            Ok(snapshot) => PollResult::Ready(snapshot),
            Err(TryRecvError::Empty) => PollResult::NotReady,
            Err(TryRecvError::Disconnected) => PollResult::Disconnected,
        }
    }

    /// Block until the fresher snapshot arrives (or the sender is dropped).
    /// Intentionally distinct from `try_poll`: a caller that has decided
    /// waiting is acceptable (e.g. `update_bias` before a later utterance,
    /// not the first-audio-frame path) can opt into it explicitly. The hot
    /// path documented in SPEC §3.1 must not call this.
    pub fn wait(self) -> Result<ContextSnapshot, RecvError> {
        self.rx.recv()
    }
}

/// Result of [`ContextProvider::capture`]: what we have right now, plus a
/// handle to what may be coming.
pub struct ContextCapture {
    /// The best snapshot available without waiting — typically the previous
    /// snapshot per SPEC §3.3's staleness policy.
    pub snapshot: ContextSnapshot,
    /// A fresher read in flight, if the provider has one running.
    pub pending: Option<PendingContext>,
}

impl ContextCapture {
    /// THE safety-critical secure-field decision, end to end: wait (bounded
    /// by the provider's own per-read timeout — see
    /// `voice_context::DEFAULT_AX_TIMEOUT` on macOS) for the freshest
    /// possible read, then combine it with what the previous snapshot
    /// already knew via [`SecureFieldStatus::merge_after_fresh_read`].
    ///
    /// This is deliberately the ONE place that gets to call [`PendingContext::wait`]
    /// and interpret its result for this purpose — every step that can go
    /// wrong (`wait()` returning `Err` because the background reader
    /// vanished without answering; `wait()` returning `Ok` with a
    /// `focused_element: None` snapshot because the read degraded, not
    /// because it definitively found nothing) resolves to
    /// [`SecureFieldStatus::Unknown`], not to a fabricated "not secure".
    ///
    /// Intentionally consumes `self`/`self.pending` (blocking) rather than
    /// polling: SPEC's "never blocks the first audio frame" contract is
    /// about `ContextProvider::capture()` itself, not about this decision —
    /// callers use this immediately before the one moment where a wrong
    /// answer leaks a credential, not on the hot path that must never
    /// stall. See `voice-cli`'s `CliInsertionBackend::current_target` for
    /// the real caller and why blocking there is the deliberate right call.
    #[must_use]
    pub fn wait_secure_field_status(self) -> SecureFieldStatus {
        let previous_status = self.snapshot.secure_field_status();
        match self.pending {
            None => previous_status,
            Some(pending) => {
                let fresh_status = match pending.wait() {
                    Ok(fresh) => fresh.secure_field_status(),
                    // The background reader thread vanished (channel
                    // disconnected) without ever answering -- that is not
                    // evidence of anything, least of all "safe".
                    Err(_) => SecureFieldStatus::Unknown,
                };
                SecureFieldStatus::merge_after_fresh_read(fresh_status, previous_status)
            }
        }
    }
}

/// Frontmost app / focused element / actionable map, captured asynchronously
/// and best-effort. SPEC §3.1, COMMANDS-SPEC §3.2.
///
/// Implementations for real platforms (macOS `AXUIElement`, Windows
/// UIAutomation) are out of scope for this run — see [`FixtureContextProvider`]
/// for the deterministic in-memory implementation used by tests and by other
/// crates until native backends land.
pub trait ContextProvider {
    /// Returns immediately with the previous snapshot and, if a fresher read
    /// is in flight, a handle to it. Must never block on the platform read —
    /// SPEC §3.1's hard requirement that context capture never delays the
    /// first captured audio frame.
    fn capture(&self) -> ContextCapture;
}

/// Deterministic in-memory [`ContextProvider`] fixture. No AX/UIA calls.
///
/// Reads happen on a background thread gated by an explicit release signal
/// (a synchronous rendezvous channel), so tests can prove `capture()` itself
/// never blocks without relying on timing/sleep — the background thread is
/// provably still waiting on the gate at the moment `capture()` has already
/// returned.
pub struct FixtureContextProvider {
    previous: ContextSnapshot,
    /// Optional queued fresh snapshot the next `capture()` call will read,
    /// released only once the test/caller signals via the gate this
    /// provider hands back.
    next: Option<ContextSnapshot>,
}

impl FixtureContextProvider {
    /// A provider with no fresh read in flight — `capture()` just returns
    /// `previous` with `pending: None`. Useful for the common case (no AX
    /// change since the last utterance).
    pub fn stable(previous: ContextSnapshot) -> Self {
        Self { previous, next: None }
    }

    /// A provider whose `capture()` will asynchronously resolve to `next`,
    /// but only after the returned gate is released — modeling an AX/UIA
    /// read that takes real wall-clock time.
    pub fn with_pending_read(previous: ContextSnapshot, next: ContextSnapshot) -> Self {
        Self { previous, next: Some(next) }
    }
}

impl ContextProvider for FixtureContextProvider {
    fn capture(&self) -> ContextCapture {
        let Some(next) = self.next.clone() else {
            return ContextCapture { snapshot: self.previous.clone(), pending: None };
        };

        // Spawn-and-return: the background thread does the "read" (in a
        // real provider, the AX/UIA walk) on its own schedule. Nothing on
        // the calling thread waits for it — `capture()` returns as soon as
        // the thread is spawned, regardless of how long that thread takes.
        let (result_tx, result_rx) = mpsc::channel::<ContextSnapshot>();
        thread::spawn(move || {
            let _ = result_tx.send(next);
        });

        ContextCapture { snapshot: self.previous.clone(), pending: Some(PendingContext::new(result_rx)) }
    }
}

impl FixtureContextProvider {
    /// Like [`ContextProvider::capture`], but also returns a release gate so
    /// a test can control exactly when the simulated background read
    /// completes, without relying on timing/sleep. Not part of the
    /// `ContextProvider` contract (a real provider wires its own platform
    /// I/O); this exists purely to make the "capture cannot block" property
    /// deterministically testable.
    pub fn capture_with_gate(&self) -> (ContextCapture, Option<mpsc::SyncSender<()>>) {
        let Some(next) = self.next.clone() else {
            return (ContextCapture { snapshot: self.previous.clone(), pending: None }, None);
        };

        // Rendezvous (capacity 0): the background thread blocks in
        // `gate_rx.recv()` until the test calls `gate_tx.send(())`. Because
        // this thread spawn happens *after* `capture()` has already
        // returned in the sibling test, and this thread does nothing before
        // parking on `gate_rx`, a poll taken immediately after `capture()`
        // returns is provably still `NotReady`.
        let (gate_tx, gate_rx) = mpsc::sync_channel::<()>(0);
        let (result_tx, result_rx) = mpsc::channel::<ContextSnapshot>();

        thread::spawn(move || {
            if gate_rx.recv().is_ok() {
                let _ = result_tx.send(next);
            }
        });

        (
            ContextCapture { snapshot: self.previous.clone(), pending: Some(PendingContext::new(result_rx)) },
            Some(gate_tx),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::types::{AppKind, DegradedReason};

    fn snapshot(seq: u64, app_name: &str) -> ContextSnapshot {
        ContextSnapshot {
            frontmost_app: Some(AppInfo { name: app_name.to_string(), kind: AppKind::Other }),
            focused_element: None,
            actionable_map: ActionableMap::unavailable(DegradedReason::Timeout),
            seq,
        }
    }

    #[test]
    fn stable_provider_returns_previous_with_no_pending() {
        let provider = FixtureContextProvider::stable(snapshot(1, "Slack"));
        let capture = provider.capture();
        assert_eq!(capture.snapshot.seq, 1);
        assert!(capture.pending.is_none());
    }

    #[test]
    fn capture_returns_before_background_read_completes() {
        // The core "never blocks the first audio frame" contract: at the
        // moment capture_with_gate() has returned, the background thread is
        // provably still parked on the rendezvous (we haven't released it
        // yet), so a poll right now MUST be NotReady — capture() cannot have
        // done the read itself.
        let provider = FixtureContextProvider::with_pending_read(snapshot(1, "Slack"), snapshot(2, "Slack"));
        let (capture, gate) = provider.capture_with_gate();

        assert_eq!(capture.snapshot.seq, 1, "must hand back the PREVIOUS snapshot per SPEC §3.3 staleness policy");
        let pending = capture.pending.expect("with_pending_read must produce a pending handle");
        assert_eq!(pending.try_poll(), PollResult::NotReady);

        // Now release the gate and wait — proves the fresh read does
        // eventually land, just never on the capture() call itself.
        let gate = gate.expect("with_pending_read must produce a release gate in this fixture");
        gate.send(()).expect("background thread should still be waiting on the gate");
        let fresh = pending.wait().expect("background thread should deliver the fresh snapshot");
        assert_eq!(fresh.seq, 2);
    }

    #[test]
    fn disconnected_pending_reports_disconnected_not_a_hang() {
        let (tx, rx) = mpsc::channel::<ContextSnapshot>();
        drop(tx); // no sender alive at all — `_tx` would keep it alive by binding, so drop explicitly
        let pending = PendingContext::new(rx);
        // No sender alive at all: try_poll must resolve immediately, not
        // hang, and must say so honestly rather than pretending NotReady.
        assert_eq!(pending.try_poll(), PollResult::Disconnected);
    }

    // -- wait_secure_field_status: the blocker-fix regression suite ----
    //
    // Reproduces, deterministically and without any live AX/permission
    // dependency, the exact fail-open bug found live against the real
    // MacosContextProvider: a target app stalls past the AX timeout, a
    // password field is focused, and the transcript nearly got typed into
    // it because `focused_element: None` collapsed to "not secure". Every
    // scenario below must refuse (produce `Unknown` or a sticky
    // `Known(true)`), never `Known(false)`.

    use crate::types::{ElementRole, Position, SecureFieldStatus};

    fn secure_element_snapshot(seq: u64) -> ContextSnapshot {
        ContextSnapshot {
            frontmost_app: Some(AppInfo { name: "Passwords".to_string(), kind: AppKind::Other }),
            focused_element: Some(ActionableElement {
                role: ElementRole::TextField,
                label: "Password".to_string(),
                position: Position { x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
                writable: true,
                secure: true,
                enabled: true,
            }),
            actionable_map: ActionableMap::new(vec![], crate::types::Coverage::Full),
            seq,
        }
    }

    fn safe_element_snapshot(seq: u64) -> ContextSnapshot {
        ContextSnapshot {
            frontmost_app: Some(AppInfo { name: "iTerm2".to_string(), kind: AppKind::Terminal }),
            focused_element: Some(ActionableElement {
                role: ElementRole::TextField,
                label: "".to_string(),
                position: Position { x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
                writable: true,
                secure: false,
                enabled: true,
            }),
            actionable_map: ActionableMap::new(vec![], crate::types::Coverage::Full),
            seq,
        }
    }

    fn degraded_snapshot(seq: u64, reason: DegradedReason) -> ContextSnapshot {
        ContextSnapshot {
            frontmost_app: None,
            focused_element: None,
            actionable_map: ActionableMap::unavailable(reason),
            seq,
        }
    }

    #[test]
    fn timeout_must_refuse_to_type() {
        // The exact finding: an AX read that timed out (`DegradedReason::
        // Timeout`) must never be treated as "not secure".
        let provider = FixtureContextProvider::with_pending_read(
            safe_element_snapshot(1),
            degraded_snapshot(2, DegradedReason::Timeout),
        );
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Unknown, "a timed-out read must refuse, not default to safe");
    }

    #[test]
    fn no_accessibility_permission_must_refuse_to_type() {
        let provider = FixtureContextProvider::with_pending_read(
            safe_element_snapshot(1),
            degraded_snapshot(2, DegradedReason::NoAccessibilityPermission),
        );
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Unknown, "no Accessibility grant means we cannot possibly know -- must refuse, not default to safe");
    }

    #[test]
    fn no_focused_element_must_refuse_to_type() {
        // `focused_element: None` for the "app reports nothing focused right
        // now" reason -- still not a positive, structured confirmation that
        // there is no secure field, so it stays Unknown rather than being
        // upgraded to Known(false).
        let provider = FixtureContextProvider::with_pending_read(
            safe_element_snapshot(1),
            degraded_snapshot(2, DegradedReason::Unknown("app reports no focused UI element right now".to_string())),
        );
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Unknown, "no focused element resolved must refuse, not default to safe");
    }

    #[test]
    fn a_previously_resolved_secure_element_is_not_discarded_by_a_timed_out_refresh() {
        // The second half of the blocker: `current_target()` used to call
        // `pending.wait().unwrap_or(capture.snapshot)` -- since a timed-out
        // read still resolves `Ok`, that `unwrap_or` never fired, and the
        // stale, correctly-secure previous snapshot was silently replaced by
        // the fresh, empty, "not secure" one. Prove the fix keeps the stale
        // positive.
        let provider =
            FixtureContextProvider::with_pending_read(secure_element_snapshot(1), degraded_snapshot(2, DegradedReason::Timeout));
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Known(true), "a previously-resolved secure element must survive a fresh read that only degraded, not disprove it");
    }

    #[test]
    fn a_stale_safe_reading_does_not_survive_a_timed_out_refresh() {
        // The mirror case, equally important: staleness cuts only one way.
        // A previously-resolved SAFE reading must NOT be trusted once a
        // fresher read fails to reconfirm it -- the user could have tabbed
        // into a real password field in between.
        let provider =
            FixtureContextProvider::with_pending_read(safe_element_snapshot(1), degraded_snapshot(2, DegradedReason::Timeout));
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Unknown, "a stale 'safe' reading must not be trusted once the fresh read can no longer confirm it");
    }

    #[test]
    fn a_resolved_non_secure_read_still_types_normally() {
        // Non-regression: the fix must not make ordinary, successfully
        // resolved, non-secure targets refuse too.
        let provider = FixtureContextProvider::with_pending_read(safe_element_snapshot(1), safe_element_snapshot(2));
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Known(false));
    }

    #[test]
    fn no_pending_read_uses_the_previous_snapshots_own_status() {
        // `FixtureContextProvider::stable` never produces a `pending` read at
        // all (mirrors a provider with nothing fresher in flight) -- the
        // decision must fall back to what the snapshot in hand already says,
        // not silently default to safe.
        let provider = FixtureContextProvider::stable(secure_element_snapshot(1));
        let status = provider.capture().wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Known(true));
    }

    #[test]
    fn a_disconnected_background_reader_refuses_rather_than_defaulting_to_safe() {
        // `PendingContext::wait()` returning `Err` (the sender was dropped
        // without ever answering) must not be treated as "not secure" either.
        let (_tx, rx) = mpsc::channel::<ContextSnapshot>();
        drop(_tx);
        let capture = ContextCapture { snapshot: safe_element_snapshot(1), pending: Some(PendingContext::new(rx)) };
        let status = capture.wait_secure_field_status();
        assert_eq!(status, SecureFieldStatus::Unknown, "a vanished background reader is not evidence of safety");
    }
}
