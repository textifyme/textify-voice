//! Typed errors. No path in this crate reachable from library input panics;
//! everything terminates in one of these.

use crate::resolution::RefusalReason;

#[derive(Debug, Clone, PartialEq)]
pub enum ActError {
    /// `execute()` was handed a `Resolution` that resolve-time safety
    /// checks should have blocked (secure context, unregistered schema,
    /// T3-never). Defense in depth: this is the belt to resolve()'s
    /// braces, see `crate::executor::guarded_execute`.
    Refused(RefusalReason),
    /// `execute()` was handed a `NeedsDisambiguation` or otherwise-unbound
    /// `Resolution`; there was nothing concrete to execute.
    NotResolved,
    /// `UndoJournal::undo()`/`redo()` called with an empty stack.
    NothingToUndo,
    NothingToRedo,
    /// `UndoJournal::undo()`/`redo()` called on an entry with
    /// `Invertibility::None`.
    NotInvertible,
    /// The mock (or, eventually, real) executor's underlying operation
    /// failed. The string is an internal diagnostic, never utterance text
    /// or a target label -- callers must not surface it to telemetry.
    ExecutionFailed(String),
}

impl std::fmt::Display for ActError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActError::Refused(reason) => write!(f, "refused: {reason:?}"),
            ActError::NotResolved => write!(f, "action not resolved"),
            ActError::NothingToUndo => write!(f, "nothing to undo"),
            ActError::NothingToRedo => write!(f, "nothing to redo"),
            ActError::NotInvertible => write!(f, "entry is not invertible"),
            ActError::ExecutionFailed(msg) => write!(f, "execution failed: {msg}"),
        }
    }
}

impl std::error::Error for ActError {}
