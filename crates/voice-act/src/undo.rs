//! Undo journal. COMMANDS-SPEC.md §3.4 (meta: "undo that" is itself a T0
//! action) and the dispatch note: reversible actions record an inverse
//! (`Invertibility::Full`), snapshot-restorable ones record a snapshot
//! (`Invertibility::Snapshot`), irreversible ones record `None`.
//!
//! In-memory core (two LIFO stacks: undo / redo) plus a pluggable
//! `UndoPersistence` trait for a future durable backend -- explicitly NOT
//! rusqlite in this run, just the seam. `NeverStore` suppresses recording
//! entirely (both the in-memory stacks and the persistence hook), matching
//! SPEC.md §6 "History: ... NeverStore disables it."

use crate::errors::ActError;
use crate::schema::Invertibility;

/// A boxed side-effecting callback used for an entry's undo/redo action.
/// `FnMut` (not `FnOnce`) so a redo can re-run the same closure after a
/// prior undo, and vice versa. Deliberately not `Send`: the real OS
/// boundaries this journal will eventually wrap (AXUIElement, window
/// management) are main-thread-only on macOS, so the journal is expected
/// to live on the command-mode UI thread, not be shipped across threads.
pub type UndoAction = Box<dyn FnMut() -> Result<(), ActError>>;

/// One journaled action. `undo`/`redo` are `None` exactly when
/// `invertible == Invertibility::None` (irreversible) -- callers should not
/// construct a `Full`/`Snapshot` entry without both closures, but the
/// journal itself only trusts the closures' presence, not the tag, when
/// deciding whether an entry can actually be undone.
pub struct UndoEntry {
    pub schema_id: &'static str,
    pub invertible: Invertibility,
    pub undo: Option<UndoAction>,
    pub redo: Option<UndoAction>,
}

impl std::fmt::Debug for UndoEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Closures aren't Debug; this also documents that an UndoEntry has
        // nothing text-bearing to leak if it ever gets logged.
        f.debug_struct("UndoEntry")
            .field("schema_id", &self.schema_id)
            .field("invertible", &self.invertible)
            .field("undoable", &self.undo.is_some())
            .field("redoable", &self.redo.is_some())
            .finish()
    }
}

impl UndoEntry {
    pub fn irreversible(schema_id: &'static str) -> Self {
        Self { schema_id, invertible: Invertibility::None, undo: None, redo: None }
    }

    pub fn full(schema_id: &'static str, undo: UndoAction, redo: UndoAction) -> Self {
        Self { schema_id, invertible: Invertibility::Full, undo: Some(undo), redo: Some(redo) }
    }

    pub fn snapshot(schema_id: &'static str, restore: UndoAction, reapply: UndoAction) -> Self {
        Self { schema_id, invertible: Invertibility::Snapshot, undo: Some(restore), redo: Some(reapply) }
    }
}

/// Pluggable persistence hook for the undo journal. This run's only
/// implementations are in-memory (`NoopPersistence`, `RecordingPersistence`
/// in tests below) -- no rusqlite, no disk I/O. A future durable backend
/// implements this trait against `(schema_id, invertible)` markers; the
/// live closures are process-local by construction and are not persisted.
pub trait UndoPersistence {
    fn record(&mut self, schema_id: &'static str, invertible: Invertibility);
    fn clear(&mut self);
}

/// Default persistence: does nothing. Used when the journal doesn't need a
/// durability seam wired up (e.g. most tests, and until a real backend
/// exists).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopPersistence;

impl UndoPersistence for NoopPersistence {
    fn record(&mut self, _schema_id: &'static str, _invertible: Invertibility) {}
    fn clear(&mut self) {}
}

/// The undo/redo journal. Generic over the persistence backend so tests can
/// swap in a recording stub without the journal depending on any concrete
/// storage technology.
pub struct UndoJournal<P: UndoPersistence = NoopPersistence> {
    never_store: bool,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    persistence: P,
}

impl<P: UndoPersistence> UndoJournal<P> {
    pub fn new(persistence: P) -> Self {
        Self { never_store: false, undo_stack: Vec::new(), redo_stack: Vec::new(), persistence }
    }

    pub fn with_never_store(persistence: P, never_store: bool) -> Self {
        Self { never_store, undo_stack: Vec::new(), redo_stack: Vec::new(), persistence }
    }

    pub fn never_store(&self) -> bool {
        self.never_store
    }

    /// Flip `NeverStore`. Turning it **on** mid-session purges anything
    /// already resident -- both the in-memory undo/redo stacks (which hold
    /// live closures capturing target names) and the persistence backend
    /// via [`UndoPersistence::clear`] -- not just future pushes. SPEC.md
    /// §6: "History: ... NeverStore disables it," which means nothing is
    /// left to disable *around*, not merely that new entries stop landing.
    /// Turning it off, or toggling a flag that's already at the requested
    /// value, purges nothing.
    pub fn set_never_store(&mut self, never_store: bool) {
        let turning_on = never_store && !self.never_store;
        self.never_store = never_store;
        if turning_on {
            self.undo_stack.clear();
            self.redo_stack.clear();
            self.persistence.clear();
        }
    }

    /// Record a newly-executed action's undo entry. No-ops entirely under
    /// `NeverStore` -- the action still happened, but there is nothing to
    /// undo afterward. Pushing a new entry clears the redo stack (a fresh
    /// action invalidates any pending redo, standard editor semantics).
    pub fn push(&mut self, entry: UndoEntry) {
        if self.never_store {
            return;
        }
        self.persistence.record(entry.schema_id, entry.invertible);
        self.redo_stack.clear();
        self.undo_stack.push(entry);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Undo the most recently pushed entry. On success the entry moves to
    /// the redo stack. On failure (empty stack, non-invertible entry, or
    /// the underlying closure erroring) the stack is left exactly as it
    /// was -- an entry is never lost to a failed undo attempt.
    pub fn undo(&mut self) -> Result<(), ActError> {
        match self.undo_stack.pop() {
            None => Err(ActError::NothingToUndo),
            Some(mut entry) => match entry.undo.as_mut() {
                None => {
                    self.undo_stack.push(entry);
                    Err(ActError::NotInvertible)
                }
                Some(undo_fn) => match undo_fn() {
                    Ok(()) => {
                        self.redo_stack.push(entry);
                        Ok(())
                    }
                    Err(e) => {
                        self.undo_stack.push(entry);
                        Err(e)
                    }
                },
            },
        }
    }

    /// Redo the most recently undone entry. Mirrors `undo()`.
    pub fn redo(&mut self) -> Result<(), ActError> {
        match self.redo_stack.pop() {
            None => Err(ActError::NothingToRedo),
            Some(mut entry) => match entry.redo.as_mut() {
                None => {
                    self.redo_stack.push(entry);
                    Err(ActError::NotInvertible)
                }
                Some(redo_fn) => match redo_fn() {
                    Ok(()) => {
                        self.undo_stack.push(entry);
                        Ok(())
                    }
                    Err(e) => {
                        self.redo_stack.push(entry);
                        Err(e)
                    }
                },
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct RecordingPersistence {
        log: Vec<(&'static str, Invertibility)>,
    }
    impl UndoPersistence for RecordingPersistence {
        fn record(&mut self, schema_id: &'static str, invertible: Invertibility) {
            self.log.push((schema_id, invertible));
        }
        fn clear(&mut self) {
            self.log.clear();
        }
    }

    /// Build a Full undo entry over a shared counter: undo decrements,
    /// redo increments. Lets tests observe ordering via the counter value
    /// and a call-order log.
    fn counting_entry(id: &'static str, counter: Rc<RefCell<i32>>, log: Rc<RefCell<Vec<&'static str>>>) -> UndoEntry {
        let (c1, l1) = (counter.clone(), log.clone());
        let undo: UndoAction = Box::new(move || {
            *c1.borrow_mut() -= 1;
            l1.borrow_mut().push(id);
            Ok(())
        });
        let (c2, l2) = (counter, log);
        let redo: UndoAction = Box::new(move || {
            *c2.borrow_mut() += 1;
            l2.borrow_mut().push(id);
            Ok(())
        });
        UndoEntry::full(id, undo, redo)
    }

    #[test]
    fn undo_pops_lifo_order() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(NoopPersistence);

        journal.push(counting_entry("a", counter.clone(), log.clone()));
        journal.push(counting_entry("b", counter.clone(), log.clone()));
        journal.push(counting_entry("c", counter.clone(), log.clone()));

        journal.undo().expect("undo c");
        journal.undo().expect("undo b");
        journal.undo().expect("undo a");

        assert_eq!(*log.borrow(), vec!["c", "b", "a"], "undo must be LIFO: most recent action first");
        assert!(journal.undo().is_err(), "stack exhausted");
    }

    #[test]
    fn redo_restores_in_reverse_of_undo_order() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(NoopPersistence);

        journal.push(counting_entry("a", counter.clone(), log.clone()));
        journal.push(counting_entry("b", counter.clone(), log.clone()));

        journal.undo().expect("undo b"); // counter -1
        journal.undo().expect("undo a"); // counter -2
        assert_eq!(*counter.borrow(), -2);

        log.borrow_mut().clear();
        journal.redo().expect("redo a"); // counter -1
        journal.redo().expect("redo b"); // counter 0

        assert_eq!(*log.borrow(), vec!["a", "b"], "redo must replay in the order entries were undone (LIFO of the redo stack)");
        assert_eq!(*counter.borrow(), 0);
        assert!(journal.redo().is_err(), "redo stack exhausted");
    }

    #[test]
    fn new_push_clears_redo_stack() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(NoopPersistence);

        journal.push(counting_entry("a", counter.clone(), log.clone()));
        journal.undo().expect("undo a");
        assert!(journal.can_redo());

        journal.push(counting_entry("b", counter.clone(), log.clone()));
        assert!(!journal.can_redo(), "a fresh push must invalidate the redo branch");
    }

    #[test]
    fn irreversible_entry_cannot_be_undone_and_is_preserved_on_the_stack() {
        let mut journal = UndoJournal::new(NoopPersistence);
        journal.push(UndoEntry::irreversible("sys.screenshot"));

        let err = journal.undo().expect_err("irreversible entry must not undo");
        assert_eq!(err, ActError::NotInvertible);
        // The entry must not be silently dropped/lost on a failed undo.
        assert_eq!(journal.undo_depth(), 1);
    }

    #[test]
    fn never_store_suppresses_recording_entirely() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::with_never_store(NoopPersistence, true);

        journal.push(counting_entry("a", counter, log));
        assert_eq!(journal.undo_depth(), 0, "NeverStore must suppress recording");
        assert!(matches!(journal.undo(), Err(ActError::NothingToUndo)));
    }

    #[test]
    fn never_store_suppresses_the_persistence_hook_too() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::with_never_store(RecordingPersistence::default(), true);

        journal.push(counting_entry("a", counter, log));
        assert!(journal.persistence.log.is_empty(), "persistence hook must not fire under NeverStore");
    }

    #[test]
    fn set_never_store_purges_existing_entries_mid_session() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(RecordingPersistence::default());

        journal.push(counting_entry("a", counter.clone(), log.clone()));
        journal.push(counting_entry("b", counter.clone(), log.clone()));
        journal.undo().expect("undo b"); // populate the redo stack too
        assert_eq!(journal.undo_depth(), 1);
        assert_eq!(journal.redo_depth(), 1);
        assert!(!journal.persistence.log.is_empty(), "sanity: persistence recorded the pushes");

        // Toggling NeverStore ON mid-session, not at construction, must
        // purge everything already resident -- this is the exact scenario
        // the constructor-only path (`with_never_store`) doesn't cover.
        journal.set_never_store(true);

        assert_eq!(journal.undo_depth(), 0, "mid-session NeverStore toggle must purge the undo stack");
        assert_eq!(journal.redo_depth(), 0, "mid-session NeverStore toggle must purge the redo stack");
        assert!(journal.persistence.log.is_empty(), "mid-session NeverStore toggle must clear persistence too");

        // Toggling back off must not resurrect purged entries.
        journal.set_never_store(false);
        assert_eq!(journal.undo_depth(), 0);
        assert_eq!(journal.redo_depth(), 0);
    }

    #[test]
    fn set_never_store_is_a_noop_when_value_does_not_change() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(NoopPersistence);

        journal.push(counting_entry("a", counter.clone(), log.clone()));
        assert_eq!(journal.undo_depth(), 1);

        // Already false -> false: no transition, nothing purged.
        journal.set_never_store(false);
        assert_eq!(journal.undo_depth(), 1, "false -> false must not purge");

        journal.set_never_store(true);
        assert_eq!(journal.undo_depth(), 0);

        // Already true -> true: no transition (and nothing left to purge
        // anyway), must not panic or misbehave.
        journal.set_never_store(true);
        assert_eq!(journal.undo_depth(), 0);
    }

    #[test]
    fn persistence_hook_fires_when_not_never_store() {
        let counter = Rc::new(RefCell::new(0));
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut journal = UndoJournal::new(RecordingPersistence::default());

        journal.push(counting_entry("win.maximize", counter, log));
        assert_eq!(journal.persistence.log, vec![("win.maximize", Invertibility::Full)]);
    }

    #[test]
    fn failed_undo_leaves_stack_intact() {
        let mut journal = UndoJournal::new(NoopPersistence);
        let failing: UndoAction = Box::new(|| Err(ActError::ExecutionFailed("boom".into())));
        let noop_redo: UndoAction = Box::new(|| Ok(()));
        journal.push(UndoEntry::full("app.quit", failing, noop_redo));

        let err = journal.undo().expect_err("underlying undo failed");
        assert!(matches!(err, ActError::ExecutionFailed(_)));
        assert_eq!(journal.undo_depth(), 1, "entry must stay on the undo stack, not be lost");
        assert_eq!(journal.redo_depth(), 0);
    }
}
