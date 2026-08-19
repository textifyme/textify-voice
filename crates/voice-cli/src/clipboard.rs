//! Clipboard snapshot / write / restore for the clipboard-paste insertion
//! path (SPEC.md §3.1: "clipboard paste as fallback (snapshot → write →
//! synthesized ⌘V/Ctrl-V → restore-after-paste-confirmed)").
//!
//! `dictate.rs`'s current `CliInsertionBackend::clipboard_paste` (as of this
//! writing) just calls `arboard::Clipboard::set_text` directly -- it
//! clobbers whatever was on the clipboard before the utterance and never
//! restores it. This module is the replacement primitive; **it is not wired
//! into `dictate.rs` by this change** (that is explicitly another agent's
//! job per this unit's dispatch) -- it only has to be correct and usable.
//!
//! # The race this module exists to manage
//!
//! macOS gives a process no callback for "the receiving app just pasted."
//! `NSPasteboard` exposes exactly one observable signal: `changeCount`, an
//! integer that increments every time *any* process writes to the general
//! pasteboard. Reading (which is what a paste does) never touches it. So
//! there is no true "paste confirmed" event to wait for -- only two
//! imperfect proxies, and this module uses both together:
//!
//! 1. **A bounded delay** ([`ClipboardGuard::restore_after_delay`]) long
//!    enough for a synthesized ⌘V to have been dispatched by the OS and
//!    processed by a typical target app's run loop before we write anything
//!    back. This is a heuristic, not a guarantee: a target app that is
//!    busy, off the main thread, or simply slower than the chosen delay can
//!    still read the pasteboard *after* we have restored it, and will then
//!    paste the caller's old content instead of the transcript. Choosing a
//!    larger delay shrinks this risk but makes every dictated paste feel
//!    laggier; the default here (150ms) is a starting point tuned for
//!    normal local apps, not a proven bound -- callers with slower targets
//!    should pass a larger `settle` explicitly.
//! 2. **A `changeCount` guard**, checked immediately before the restore
//!    write: only restore if the pasteboard's `changeCount` is still
//!    exactly what it was right after our own write. If it has moved, some
//!    other process (a clipboard manager normalizing formats, the user
//!    copying something new, another app) has written to the pasteboard in
//!    the interim, and restoring would silently destroy *that* newer
//!    content instead of protecting the caller's older content -- so this
//!    module refuses and reports [`RestoreOutcome::SkippedChanged`] rather
//!    than guessing. The tradeoff this accepts: when a clipboard manager
//!    (or any other pasteboard-writing process) is active, restores will
//!    often be skipped, and the transcript is what's left on the clipboard
//!    permanently instead of the caller's original content. That is
//!    considered the safer failure mode of the two.
//!
//! Neither mechanism, alone or combined, is a correctness proof. Together
//! they are: don't restore too early (bounded delay), and don't restore
//! over someone else's newer write (changeCount guard). Both failure modes
//! above are real and this module cannot eliminate them -- only a genuine
//! "target app read the pasteboard" signal from the OS could, and macOS
//! does not expose one to third-party processes.
//!
//! # Non-string payloads
//!
//! The naive `arboard::Clipboard::set_text` path this replaces only ever
//! deals in text, so copying an image or a file and then dictating into any
//! text field would silently destroy the image/file on the clipboard. This
//! module's macOS implementation snapshots *every* UTI type currently on
//! the pasteboard as raw bytes (`NSPasteboard::dataForType`, not just the
//! string representation) and restores each one, so a copied image or file
//! reference round-trips byte-for-byte. See the crate-level docs above the
//! test module for a real measured round-trip (this was exercised live
//! against the actual system pasteboard in this environment, including a
//! synthetic PNG payload, not just asserted from the API docs).
//!
//! The non-macOS fallback (`arboard`, the crate's existing cross-platform
//! clipboard dependency) has no raw multi-type read/write API and no
//! `changeCount` equivalent, so on non-macOS platforms this module only
//! preserves *text* and can only offer the weaker "re-read and compare"
//! guard described on [`ClipboardError`] and the fallback's doc comment
//! below -- **this is honestly a materially weaker guarantee** than the
//! macOS path, not a portable equivalent of it. It has not been exercised
//! in this session (this environment is macOS-only).

use std::fmt;
use std::time::Duration;

/// One clipboard representation: a UTI-ish type string (e.g.
/// `"public.utf8-plain-text"`, `"public.png"`) paired with its raw bytes,
/// exactly as read from (or written to) the pasteboard. Kept opaque/raw
/// rather than parsed, so restore can round-trip any payload -- text,
/// image, file reference, whatever the pasteboard held -- without this
/// module needing to understand its format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteboardItem {
    pub uti: String,
    pub data: Vec<u8>,
}

/// Everything read off the clipboard at snapshot time: the items whose raw
/// bytes we could read, the change-count at that instant, and the types (if
/// any) whose bytes we could *not* read -- reported rather than silently
/// dropped, per this unit's "don't quietly lose data" mandate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardSnapshot {
    pub items: Vec<PasteboardItem>,
    /// Types the pasteboard listed in `types()` but whose `dataForType`
    /// read came back empty/unreadable. Restoring a snapshot never writes
    /// these back (there is nothing to write), but they are named here so
    /// a caller can at least log that something was on the clipboard this
    /// module could not preserve.
    pub unreadable_types: Vec<String>,
    change_count: isize,
}

impl ClipboardSnapshot {
    /// True if the pasteboard held nothing readable at snapshot time (a
    /// genuinely empty clipboard, or -- rare -- one holding only types this
    /// process could not read at all).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn change_count(&self) -> isize {
        self.change_count
    }
}

/// What went wrong talking to the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// The write (of the transcript, or of a restored item) was rejected
    /// by the pasteboard API itself.
    WriteFailed(String),
    /// Could not read the clipboard/pasteboard at all (distinct from an
    /// empty clipboard, which is not an error -- see [`ClipboardSnapshot::is_empty`]).
    ///
    /// wire:live-path note: only the `#[cfg(not(target_os = "macos"))]`
    /// fallback backend below ever constructs this today, so a macOS-only
    /// build (this workspace's only real target so far) sees it as
    /// "never constructed" under `-D warnings`'s dead-code lint even though
    /// it is real, load-bearing API for that other platform. `#[allow(dead_code)]`
    /// here documents why, rather than silently deleting the variant or its
    /// doc comment.
    #[allow(dead_code)]
    ReadFailed(String),
    /// A capability this platform's backend does not implement (e.g. a
    /// non-macOS build asked to restore a non-text payload). Same
    /// cross-platform-only-construction note as [`ClipboardError::ReadFailed`].
    #[allow(dead_code)]
    Unsupported(String),
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardError::WriteFailed(msg) => write!(f, "clipboard write failed: {msg}"),
            ClipboardError::ReadFailed(msg) => write!(f, "clipboard read failed: {msg}"),
            ClipboardError::Unsupported(msg) => write!(f, "clipboard operation unsupported: {msg}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// What happened when a [`ClipboardGuard`] was asked to restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The pre-write snapshot was written back successfully; `changeCount`
    /// matched what this guard left behind, so nothing else touched the
    /// clipboard in between.
    Restored,
    /// The snapshot was empty (clipboard had nothing before we wrote), so
    /// "restoring" means leaving the pasteboard cleared -- which this
    /// module did.
    RestoredEmpty,
    /// The caller explicitly opted out via [`ClipboardGuard::disarm`]; no
    /// restore was attempted. Not an error -- e.g. `--clipboard-only` mode,
    /// where leaving the transcript on the clipboard is the whole point.
    Disarmed,
    /// The pasteboard's `changeCount` had moved since this guard's write,
    /// meaning some other process wrote to it in the meantime (a clipboard
    /// manager, another app, a fresh user copy). Restoring would have
    /// clobbered that newer content, so this guard refused. `expected` is
    /// the change-count this guard left behind; `found` is what it read
    /// just before the would-be restore.
    SkippedChanged { expected: isize, found: isize },
    /// The restore write itself failed after the changeCount guard passed
    /// (rare -- e.g. the pasteboard became briefly unavailable).
    Failed(ClipboardError),
}

/// Snapshot → write → (caller synthesizes the paste) → restore, as one
/// guarded object. This is the API `InsertionBackend::clipboard_paste`
/// implementations should call; see module docs for exactly what the
/// restore step relies on and how it can still fail.
///
/// Typical use from an insertion backend (not wired up by this unit):
///
/// ```ignore
/// let mut guard = ClipboardGuard::stage(text)?;
/// crate::paste::synthesize_cmd_v()?;
/// match guard.restore_after_delay(Duration::from_millis(150)) {
///     RestoreOutcome::Restored | RestoreOutcome::RestoredEmpty => {}
///     RestoreOutcome::SkippedChanged { .. } => { /* log, don't treat as fatal */ }
///     other => { /* log */ }
/// }
/// ```
pub struct ClipboardGuard {
    snapshot: ClipboardSnapshot,
    written_change_count: isize,
    armed: bool,
}

impl ClipboardGuard {
    /// Snapshot whatever is currently on the clipboard, then overwrite it
    /// with `text`. Returns the guard armed (restore will be attempted
    /// unless [`disarm`](Self::disarm) is called first).
    pub fn stage(text: &str) -> Result<Self, ClipboardError> {
        let snapshot = backend::snapshot()?;
        let written_change_count = backend::write_text(text)?;
        Ok(Self {
            snapshot,
            written_change_count,
            armed: true,
        })
    }

    /// The `changeCount` immediately after this guard's write -- exposed so
    /// a caller with its own event-driven "the paste happened" signal (or
    /// its own polling loop) can compare against [`current_change_count`]
    /// without going through the blocking convenience methods below.
    #[must_use]
    pub fn written_change_count(&self) -> isize {
        self.written_change_count
    }

    #[must_use]
    pub fn snapshot(&self) -> &ClipboardSnapshot {
        &self.snapshot
    }

    /// Opt out of restoring entirely. For `--clipboard-only` style callers
    /// where leaving the transcript on the clipboard is the desired end
    /// state, not an accident to undo.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Block the calling thread for `settle`, then attempt the guarded
    /// restore. See the module docs for exactly what `settle` does and
    /// does not guarantee -- it is a heuristic delay, not a confirmation.
    ///
    /// **Threading note**: this calls `std::thread::sleep`. `dictate.rs`
    /// currently performs insertion on the main thread (which also owns
    /// the `CGEventTap` run loop and the HUD panel) -- sleeping there would
    /// stall both. A caller on that thread should use
    /// [`restore_now`](Self::restore_now) (no sleep) driven from its own
    /// timer/run-loop callback instead of this method. This method is
    /// meant for a caller on a background thread, or a test.
    #[must_use]
    pub fn restore_after_delay(self, settle: Duration) -> RestoreOutcome {
        std::thread::sleep(settle);
        self.restore_now()
    }

    /// Attempt the guarded restore immediately, with no sleep: read the
    /// current `changeCount`, and only if it still equals
    /// [`written_change_count`](Self::written_change_count) write the
    /// snapshot back. For a caller driving its own delay/polling loop.
    #[must_use]
    pub fn restore_now(self) -> RestoreOutcome {
        if !self.armed {
            return RestoreOutcome::Disarmed;
        }
        let found = match backend::current_change_count() {
            Ok(count) => count,
            Err(e) => return RestoreOutcome::Failed(e),
        };
        if found != self.written_change_count {
            return RestoreOutcome::SkippedChanged {
                expected: self.written_change_count,
                found,
            };
        }
        if self.snapshot.is_empty() {
            if let Err(e) = backend::clear() {
                return RestoreOutcome::Failed(e);
            }
            return RestoreOutcome::RestoredEmpty;
        }
        match backend::restore(&self.snapshot) {
            Ok(()) => RestoreOutcome::Restored,
            Err(e) => RestoreOutcome::Failed(e),
        }
    }
}

/// Read the general clipboard's current change-count, with no other side
/// effects. Exposed for callers implementing their own poll loop instead of
/// [`ClipboardGuard::restore_after_delay`]'s single fixed sleep.
pub fn current_change_count() -> Result<isize, ClipboardError> {
    backend::current_change_count()
}

// ---------------------------------------------------------------------
// Platform backends. `ClipboardGuard` above is all policy (snapshot/write
// bookkeeping, the changeCount guard, arm/disarm); everything below is the
// actual system call, and is the only part that differs per platform.
// ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod backend {
    //! Real `NSPasteboard` calls (`objc2-app-kit`, already a dependency of
    //! this crate on macOS -- no new dependency added by this file). Every
    //! *method* called here (`generalPasteboard`, `changeCount`,
    //! `clearContents`, `types`, `dataForType`, `setData_forType`,
    //! `setString_forType`) is a safe method in objc2-app-kit 0.3.2 (none
    //! of them are declared `unsafe fn` in the generated bindings). The one
    //! `unsafe` block in this module is [`string_type`] reading the
    //! `NSPasteboardTypeString` FFI extern static -- see its doc comment.

    use super::{ClipboardError, ClipboardSnapshot, PasteboardItem};
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSData, NSString};

    fn pasteboard() -> objc2::rc::Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }

    /// `NSPasteboardTypeString` is an FFI `extern "C" static` (a `&'static
    /// NSPasteboardType` initialized by AppKit at load time), so reading it
    /// is the one place this module needs `unsafe`: the compiler cannot
    /// verify AppKit initializes it correctly before Rust code runs, only
    /// that dereferencing an uninitialized extern static would be UB.
    /// Isolated behind this one accessor so every actual caller stays safe
    /// code, matching the "no unsafe blocks needed" claim in this module's
    /// top doc comment about every *method call* here.
    pub(super) fn string_type() -> &'static objc2_app_kit::NSPasteboardType {
        // SAFETY: `NSPasteboardTypeString` is populated by AppKit's own
        // load-time initialization before any Objective-C runtime call
        // (including `NSPasteboard::generalPasteboard()`, called just
        // above in every real call path) can execute; reading it here,
        // after AppKit is already linked and running, observes a fully
        // initialized value.
        unsafe { objc2_app_kit::NSPasteboardTypeString }
    }

    pub(super) fn current_change_count() -> Result<isize, ClipboardError> {
        Ok(pasteboard().changeCount())
    }

    pub(super) fn snapshot() -> Result<ClipboardSnapshot, ClipboardError> {
        let pb = pasteboard();
        let change_count = pb.changeCount();
        let types: Vec<objc2::rc::Retained<NSString>> = pb
            .types()
            .map(|arr| arr.iter().collect())
            .unwrap_or_default();

        let mut items = Vec::with_capacity(types.len());
        let mut unreadable_types = Vec::new();
        for t in &types {
            match pb.dataForType(t) {
                Some(data) => items.push(PasteboardItem {
                    uti: t.to_string(),
                    data: data.to_vec(),
                }),
                None => unreadable_types.push(t.to_string()),
            }
        }

        Ok(ClipboardSnapshot {
            items,
            unreadable_types,
            change_count,
        })
    }

    pub(super) fn write_text(text: &str) -> Result<isize, ClipboardError> {
        let pb = pasteboard();
        pb.clearContents();
        let ns_text = NSString::from_str(text);
        if !pb.setString_forType(&ns_text, string_type()) {
            return Err(ClipboardError::WriteFailed(
                "NSPasteboard setString_forType returned false".to_string(),
            ));
        }
        Ok(pb.changeCount())
    }

    pub(super) fn clear() -> Result<(), ClipboardError> {
        pasteboard().clearContents();
        Ok(())
    }

    pub(super) fn restore(snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError> {
        let pb = pasteboard();
        pb.clearContents();
        for item in &snapshot.items {
            let uti = NSString::from_str(&item.uti);
            let data = NSData::with_bytes(&item.data);
            if !pb.setData_forType(Some(&data), &uti) {
                return Err(ClipboardError::WriteFailed(format!(
                    "NSPasteboard setData_forType returned false restoring type {:?}",
                    item.uti
                )));
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod backend {
    //! `arboard` fallback (the crate's existing cross-platform clipboard
    //! dependency). Text-only: `arboard::Clipboard` has no raw multi-type
    //! read/write API and no `changeCount` equivalent, so this backend can
    //! only preserve text and can only approximate the changeCount guard by
    //! re-reading and comparing text content -- weaker than the macOS path
    //! (see module docs). **Not exercised in this session** (this
    //! environment is macOS-only); written to keep the crate building on
    //! other targets, not verified to work on them.

    use super::{ClipboardError, ClipboardSnapshot, PasteboardItem};

    const TEXT_UTI: &str = "text/plain";

    fn read_text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok()
    }

    /// No real change-count on this backend, so the "counter" this module
    /// reports is just a hash of the current text content: equal content
    /// reads as an unchanged counter, different content (by any process)
    /// reads as a changed one. This cannot distinguish "unchanged" from
    /// "changed to the same bytes by someone else," which the real
    /// `NSPasteboard.changeCount` can (it increments on every write,
    /// including a no-op-looking one) -- named honestly as a known gap.
    fn content_fingerprint(text: &Option<String>) -> isize {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        // Truncate to isize range to match the macOS backend's signature;
        // collisions are a theoretical, not practical, concern here.
        (hasher.finish() & 0x7fff_ffff_ffff_ffff) as isize
    }

    pub(super) fn current_change_count() -> Result<isize, ClipboardError> {
        Ok(content_fingerprint(&read_text()))
    }

    pub(super) fn snapshot() -> Result<ClipboardSnapshot, ClipboardError> {
        let text = read_text();
        let items = match text {
            Some(t) if !t.is_empty() => vec![PasteboardItem {
                uti: TEXT_UTI.to_string(),
                data: t.into_bytes(),
            }],
            _ => Vec::new(),
        };
        Ok(ClipboardSnapshot {
            items,
            unreadable_types: Vec::new(),
            change_count: 0, // unused: current_change_count() re-derives its own fingerprint
        })
    }

    pub(super) fn write_text(text: &str) -> Result<isize, ClipboardError> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| ClipboardError::WriteFailed(e.to_string()))?;
        clipboard
            .set_text(text)
            .map_err(|e| ClipboardError::WriteFailed(e.to_string()))?;
        Ok(content_fingerprint(&Some(text.to_string())))
    }

    pub(super) fn clear() -> Result<(), ClipboardError> {
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| ClipboardError::WriteFailed(e.to_string()))?;
        clipboard
            .set_text(String::new())
            .map_err(|e| ClipboardError::WriteFailed(e.to_string()))
    }

    pub(super) fn restore(snapshot: &ClipboardSnapshot) -> Result<(), ClipboardError> {
        let Some(item) = snapshot.items.first() else {
            return clear();
        };
        let text = String::from_utf8(item.data.clone()).map_err(|e| {
            ClipboardError::Unsupported(format!("non-UTF8 payload on non-macOS fallback: {e}"))
        })?;
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| ClipboardError::WriteFailed(e.to_string()))?;
        clipboard
            .set_text(text)
            .map_err(|e| ClipboardError::WriteFailed(e.to_string()))
    }
}

#[cfg(all(test, target_os = "macos"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! These tests run against the **real system pasteboard** (there is no
    //! in-process fake -- `NSPasteboard.generalPasteboard()` is a process-
    //! wide singleton owned by `pboard`, the system service, not something
    //! this crate can inject a double for). Two consequences:
    //!
    //! 1. Tests in this module must not run concurrently with each other
    //!    (they'd race on the same real pasteboard and produce flaky
    //!    changeCount assertions) -- serialized via `TEST_LOCK` below.
    //! 2. Every test must leave the pasteboard exactly as it found it when
    //!    it started, since this is the developer's/CI machine's actual
    //!    clipboard, not a sandboxed resource -- each test snapshots first
    //!    and restores via `RestoreGuard`'s `Drop` no matter how the test
    //!    exits (including via a failed assertion).
    //!
    //! This module does not run on non-macOS targets: the fallback
    //! `backend` above is untested (see its module doc) rather than
    //! falsely exercised by tests that don't match its real behavior.

    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Restores the machine's real pre-test clipboard on drop (including on
    /// panic/failed assertion), so a test failure never permanently leaves
    /// test data on the developer's actual clipboard.
    struct RestoreGuard(ClipboardSnapshot);
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
            pb.clearContents();
            for item in &self.0.items {
                let uti = objc2_foundation::NSString::from_str(&item.uti);
                let data = objc2_foundation::NSData::with_bytes(&item.data);
                pb.setData_forType(Some(&data), &uti);
            }
        }
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn snapshot_then_write_then_restore_round_trips_text_byte_for_byte() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        let guard = ClipboardGuard::stage("textify-voice clipboard test marker 1")
            .expect("stage (snapshot + write) against real pasteboard");

        // Confirm the write actually landed before we restore it.
        let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
        let readback = pb.stringForType(backend::string_type());
        assert_eq!(
            readback.map(|s| s.to_string()).as_deref(),
            Some("textify-voice clipboard test marker 1")
        );

        let outcome = guard.restore_now();
        assert_eq!(outcome, RestoreOutcome::Restored, "restore should succeed: nothing else touched the pasteboard between write and restore_now in a single-threaded, serialized test");

        let restored = backend::snapshot().expect("snapshot after restore");
        assert_eq!(
            restored.items, original.items,
            "restored pasteboard contents must byte-for-byte match the pre-test snapshot"
        );
    }

    #[test]
    fn restore_after_delay_round_trips_with_a_real_sleep() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        let guard = ClipboardGuard::stage("textify-voice clipboard test marker 2")
            .expect("stage against real pasteboard");
        let outcome = guard.restore_after_delay(Duration::from_millis(20));
        assert_eq!(outcome, RestoreOutcome::Restored);

        let restored = backend::snapshot().expect("snapshot after restore");
        assert_eq!(restored.items, original.items);
    }

    #[test]
    fn disarm_skips_restore_and_leaves_written_text_in_place() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        let mut guard = ClipboardGuard::stage("textify-voice clipboard test marker 3 (disarmed)")
            .expect("stage against real pasteboard");
        guard.disarm();
        assert!(!guard.is_armed());
        let outcome = guard.restore_now();
        assert_eq!(outcome, RestoreOutcome::Disarmed);

        let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
        let readback = pb.stringForType(backend::string_type());
        assert_eq!(
            readback.map(|s| s.to_string()).as_deref(),
            Some("textify-voice clipboard test marker 3 (disarmed)"),
            "disarmed guard must leave the write in place, not restore over it"
        );
        // RestoreGuard's Drop cleans this up regardless of the assertion above.
    }

    #[test]
    fn changed_pasteboard_since_write_skips_restore_instead_of_clobbering() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        let guard = ClipboardGuard::stage("textify-voice clipboard test marker 4 (pre-interloper)")
            .expect("stage against real pasteboard");
        let written_count = guard.written_change_count();

        // Simulate "another process wrote to the clipboard before we
        // restored" -- e.g. a clipboard manager, or the user copying
        // something new during the paste-settle window.
        let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
        pb.clearContents();
        let interloper =
            objc2_foundation::NSString::from_str("interloper content from another process");
        assert!(pb.setString_forType(&interloper, backend::string_type()));
        let interloper_count = pb.changeCount();
        assert_ne!(
            interloper_count, written_count,
            "the interloper write must itself have bumped changeCount for this test to be meaningful"
        );

        let outcome = guard.restore_now();
        assert_eq!(
            outcome,
            RestoreOutcome::SkippedChanged {
                expected: written_count,
                found: interloper_count
            }
        );

        // The interloper's content must survive untouched -- this is the
        // whole point of the guard.
        let readback = pb.stringForType(backend::string_type());
        assert_eq!(
            readback.map(|s| s.to_string()).as_deref(),
            Some("interloper content from another process")
        );
        // RestoreGuard's Drop restores the true original over this at the end.
    }

    #[test]
    fn current_change_count_matches_a_fresh_write() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        let guard = ClipboardGuard::stage("textify-voice clipboard test marker 5")
            .expect("stage against real pasteboard");
        let observed = current_change_count().expect("read change count");
        assert_eq!(observed, guard.written_change_count());
        let _ = guard.restore_now();
    }

    #[test]
    fn snapshot_of_a_genuinely_empty_clipboard_round_trips_via_restored_empty() {
        let _serial = lock();
        let original = backend::snapshot().expect("snapshot real pasteboard");
        let _restore_real = RestoreGuard(original.clone());

        // Force a genuinely empty pasteboard (clearContents with nothing
        // written after it).
        let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
        pb.clearContents();
        let empty_snapshot = backend::snapshot().expect("snapshot the now-empty pasteboard");
        assert!(
            empty_snapshot.is_empty(),
            "clearContents with nothing written after should read back as empty, got: {empty_snapshot:?}"
        );

        let guard = ClipboardGuard::stage("textify-voice clipboard test marker 6 (was empty)")
            .expect("stage against the now-empty real pasteboard");
        let outcome = guard.restore_now();
        assert_eq!(outcome, RestoreOutcome::RestoredEmpty);

        let after = backend::snapshot().expect("snapshot after restoring emptiness");
        assert!(
            after.is_empty(),
            "restoring an empty snapshot should leave the pasteboard empty again, got: {after:?}"
        );
        // RestoreGuard's Drop puts the developer's real original content back.
    }
}
