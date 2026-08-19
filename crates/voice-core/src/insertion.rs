//! Text insertion policy (SPEC.md §3.1 "Text insertion" row; V1.4
//! acceptance: "secure-field refusal verified").
//!
//! > AX insertion where the focused element is writable; clipboard paste as
//! > fallback (snapshot → write → synthesized ⌘V/Ctrl-V → restore-after-
//! > paste-confirmed) ... secure input fields (macOS secure keyboard entry
//! > blocks synthetic events — **detect and refuse to type into password
//! > fields**).
//!
//! [`InsertionBackend`] is the trait a real macOS AX / Windows UIA / clipboard
//! backend implements; this crate ships only the pure policy function and a
//! deterministic [`MockInsertionBackend`] for tests.

/// Why insertion was refused outright rather than attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// SPEC §3.1: secure keyboard entry / password field — "no clicking, no
    /// typing, no reading" (this crate only owns typing).
    SecureField,
}

/// Which mechanism the policy chose, or that it refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionMethod {
    AxInsert,
    ClipboardPaste,
    Refused(RefusalReason),
}

/// Everything the policy needs to know about the current focus target.
/// Queried from the backend at decision time (not cached), since focus can
/// change between utterances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertionTarget {
    pub is_secure_field: bool,
    pub is_ax_writable: bool,
}

/// Typed error for a backend operation that failed after the policy
/// decided to attempt it (e.g. the paste's ⌘V synthesis failed, or the
/// clipboard restore raced). Per this run's no-panic rule, backends must
/// return this rather than panicking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertionError {
    AxWriteFailed(String),
    ClipboardFailed(String),
    /// A backend must never be asked to insert into a secure field; if a
    /// caller bypasses [`insert_text`] and calls a backend method directly
    /// anyway, the backend itself is expected to refuse rather than type.
    Refused(RefusalReason),
}

/// A text-insertion backend: real implementations are macOS AXUIElement /
/// Windows UIAutomation + clipboard, entirely native/IO and out of scope
/// for this crate. This trait is the contract they implement.
pub trait InsertionBackend {
    /// Read the current focus target's properties. Must reflect
    /// SPEC-mandated secure-field detection faithfully — this is the
    /// single point every other guarantee in this module depends on.
    fn current_target(&self) -> InsertionTarget;
    fn ax_insert(&mut self, text: &str) -> Result<(), InsertionError>;
    /// Snapshot → write → synthesized paste → restore-after-confirmed, per
    /// SPEC §3.1. A single call encapsulates the whole sequence; a mock
    /// only needs to record that it happened.
    fn clipboard_paste(&mut self, text: &str) -> Result<(), InsertionError>;
}

/// The insertion policy itself (pure aside from the two backend queries/
/// calls it makes): refuse outright on a secure field; otherwise prefer AX
/// insertion when the target is writable, falling back to clipboard paste.
pub fn insert_text<B: InsertionBackend>(
    backend: &mut B,
    text: &str,
) -> Result<InsertionMethod, InsertionError> {
    let target = backend.current_target();
    if target.is_secure_field {
        // No clicking, no typing, no reading — refuse before touching the
        // backend at all.
        return Ok(InsertionMethod::Refused(RefusalReason::SecureField));
    }
    if target.is_ax_writable {
        backend.ax_insert(text)?;
        Ok(InsertionMethod::AxInsert)
    } else {
        backend.clipboard_paste(text)?;
        Ok(InsertionMethod::ClipboardPaste)
    }
}

/// Deterministic in-memory backend for tests: records every call instead of
/// touching real accessibility APIs or the system clipboard.
#[derive(Debug, Clone, Default)]
pub struct MockInsertionBackend {
    pub target: InsertionTargetOverride,
    pub ax_insert_calls: Vec<String>,
    pub clipboard_paste_calls: Vec<String>,
    pub fail_next: Option<InsertionError>,
}

/// Wrapper so `MockInsertionBackend` can `#[derive(Default)]` while still
/// defaulting to a realistic, writable, non-secure target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertionTargetOverride(pub InsertionTarget);

impl Default for InsertionTargetOverride {
    fn default() -> Self {
        Self(InsertionTarget {
            is_secure_field: false,
            is_ax_writable: true,
        })
    }
}

impl MockInsertionBackend {
    #[must_use]
    pub fn writable() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn clipboard_only() -> Self {
        Self {
            target: InsertionTargetOverride(InsertionTarget {
                is_secure_field: false,
                is_ax_writable: false,
            }),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn secure_field() -> Self {
        Self {
            target: InsertionTargetOverride(InsertionTarget {
                is_secure_field: true,
                is_ax_writable: true,
            }),
            ..Default::default()
        }
    }
}

impl InsertionBackend for MockInsertionBackend {
    fn current_target(&self) -> InsertionTarget {
        self.target.0
    }

    fn ax_insert(&mut self, text: &str) -> Result<(), InsertionError> {
        if let Some(err) = self.fail_next.take() {
            return Err(err);
        }
        self.ax_insert_calls.push(text.to_string());
        Ok(())
    }

    fn clipboard_paste(&mut self, text: &str) -> Result<(), InsertionError> {
        if let Some(err) = self.fail_next.take() {
            return Err(err);
        }
        self.clipboard_paste_calls.push(text.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_field_is_refused_outright_no_backend_call_made() {
        let mut backend = MockInsertionBackend::secure_field();
        let Ok(result) = insert_text(&mut backend, "my password is hunter2") else {
            panic!("refusal is Ok(Refused), not Err");
        };
        assert_eq!(result, InsertionMethod::Refused(RefusalReason::SecureField));
        assert!(
            backend.ax_insert_calls.is_empty(),
            "must not type into a secure field"
        );
        assert!(
            backend.clipboard_paste_calls.is_empty(),
            "must not paste into a secure field either"
        );
    }

    #[test]
    fn writable_target_prefers_ax_insert() {
        let mut backend = MockInsertionBackend::writable();
        let Ok(result) = insert_text(&mut backend, "hello") else {
            panic!("insertion should succeed against a writable target");
        };
        assert_eq!(result, InsertionMethod::AxInsert);
        assert_eq!(backend.ax_insert_calls, vec!["hello".to_string()]);
        assert!(backend.clipboard_paste_calls.is_empty());
    }

    #[test]
    fn non_writable_target_falls_back_to_clipboard_paste() {
        let mut backend = MockInsertionBackend::clipboard_only();
        let Ok(result) = insert_text(&mut backend, "hello") else {
            panic!("insertion should succeed by falling back to clipboard paste");
        };
        assert_eq!(result, InsertionMethod::ClipboardPaste);
        assert_eq!(backend.clipboard_paste_calls, vec!["hello".to_string()]);
        assert!(backend.ax_insert_calls.is_empty());
    }

    #[test]
    fn backend_failure_surfaces_as_typed_error_not_panic() {
        let mut backend = MockInsertionBackend::writable();
        backend.fail_next = Some(InsertionError::AxWriteFailed("simulated".to_string()));
        let result = insert_text(&mut backend, "hello");
        assert_eq!(
            result,
            Err(InsertionError::AxWriteFailed("simulated".to_string()))
        );
    }

    #[test]
    fn secure_field_refusal_wins_even_when_also_ax_writable() {
        // Realistic case: a password field is AX-writable in the "you could
        // technically send it a value" sense, but must still be refused.
        let mut backend = MockInsertionBackend::secure_field();
        assert!(backend.current_target().is_ax_writable);
        let Ok(result) = insert_text(&mut backend, "secret") else {
            panic!("refusal is Ok(Refused), not Err");
        };
        assert_eq!(result, InsertionMethod::Refused(RefusalReason::SecureField));
    }
}
