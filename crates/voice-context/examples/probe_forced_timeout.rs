//! Reproduces the auditor's finding against the REAL MacosContextProvider,
//! side by side: applies both the OLD (buggy) `current_target()` mapping and
//! the NEW (fixed) `wait_secure_field_status()`-based mapping to the SAME
//! forced-timeout capture, so the before/after contrast is direct evidence
//! against live AX, not a synthetic snapshot.
//!
//! Fix-wave verification probe for unit "fix:secure-fail-closed".

use std::time::Duration;
use voice_context::{ContextProvider, ContextSnapshot, SecureFieldStatus};

/// The OLD logic, verbatim, from `voice-cli/src/dictate.rs` before this fix
/// (`current_target()`'s body prior to this wave):
/// ```ignore
/// let capture = self.context_provider.capture();
/// let snapshot = match capture.pending {
///     Some(pending) => pending.wait().unwrap_or(capture.snapshot),
///     None => capture.snapshot,
/// };
/// match &snapshot.focused_element {
///     Some(el) => el.secure,
///     None => false,   // <-- THE BUG: fail-open
/// }
/// ```
fn old_buggy_is_secure_field(previous: ContextSnapshot, pending: voice_context::PendingContext) -> bool {
    let snapshot = pending.wait().unwrap_or(previous);
    match &snapshot.focused_element {
        Some(el) => el.secure,
        None => false,
    }
}

fn main() {
    // 0ns budget: guaranteed to lose the race against any real AX round
    // trip, deterministically forcing the exact timeout path.
    let provider = voice_context::MacosContextProvider::with_timeout(Duration::from_nanos(0));

    println!("=== forcing a real AX read timeout (timeout budget = 0ns) against the live desktop ===\n");

    // Two independent captures (the old-logic replica needs to consume its
    // own `pending`, since `PendingContext::wait` takes `self`).
    let capture_old = provider.capture();
    let Some(pending_old) = capture_old.pending else {
        println!("no pending read -- unexpected for a real provider, nothing to probe");
        return;
    };
    let old_result = old_buggy_is_secure_field(capture_old.snapshot, pending_old);

    let capture_new = provider.capture();
    let new_status = capture_new.wait_secure_field_status();

    println!("BEFORE (original current_target() logic):");
    println!("  is_secure_field = {old_result}");
    if !old_result {
        println!("  --> InsertionTarget{{ is_secure_field: false }} --> insert_text() calls");
        println!("      backend.clipboard_paste(text) --> with --paste, CMD+V is synthesized");
        println!("      into whatever is focused, sight unseen. THIS IS THE BLOCKER.");
    }

    println!();
    println!("AFTER (wait_secure_field_status()-based logic):");
    println!("  SecureFieldStatus = {new_status:?}");
    match new_status {
        SecureFieldStatus::Unknown | SecureFieldStatus::Known(true) => {
            println!("  --> InsertionTarget{{ is_secure_field: true }} --> insert_text() refuses");
            println!("      outright: neither ax_insert nor clipboard_paste is ever called.");
        }
        SecureFieldStatus::Known(false) => {
            println!("  --> a real element resolved before the 0ns budget elapsed (race); not the case under test");
        }
    }

    assert!(!old_result || matches!(new_status, SecureFieldStatus::Known(true)), "sanity: the old logic must reproduce is_secure_field=false for this to be a real repro");
    assert_ne!(new_status, SecureFieldStatus::Known(false), "FIX VERIFICATION FAILED: new logic still fails open on a forced timeout");
    println!("\nVERIFIED: old logic fails open (is_secure_field=false), new logic fails closed ({new_status:?}).");
}
