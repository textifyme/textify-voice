//! Action schema contracts. COMMANDS-SPEC.md §3.3.

/// Blast-radius tier for an [`ActionSchema`]. COMMANDS-SPEC.md §3.5 #2.
///
/// - `T0` reversible -> execute + show undo chip.
/// - `T1` disruptive-but-recoverable -> execute + announce prominently.
/// - `T2` consequential -> HUD confirm ("say yes / no"), default-deny on timeout.
/// - `T3` never -> excluded-list actions and secure contexts; always refused.
///
/// `Ord` is derived in declaration order (T0 < T1 < T2 < T3) so callers can
/// reason about "at least as consequential as" without a manual mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    T0,
    T1,
    T2,
    T3,
}

/// How an executed action can be undone. Drives what the undo journal
/// records for a given [`crate::undo::UndoEntry`]. COMMANDS-SPEC.md §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Invertibility {
    /// A precise inverse action exists (e.g. re-open the app that was quit).
    Full,
    /// State was snapshotted before the action; undo restores the snapshot
    /// (e.g. window geometry before a tile/maximize).
    Snapshot,
    /// No inverse exists; the action is irreversible.
    None,
}

/// The typed slot kinds an [`ActionSchema`] can declare. COMMANDS-SPEC.md §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    AppRef,
    ElementRef,
    Direction,
    Ordinal,
    Percentage,
    ShortcutName,
}

/// A named, typed slot on an [`ActionSchema`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotSpec {
    pub name: &'static str,
    pub kind: SlotKind,
}

/// A closed, statically-declared action the system can execute.
/// COMMANDS-SPEC.md §3.3: "every executable action is an instance of a
/// closed schema set." Stage 2 (constrained decoding, `voice-intent`) can
/// only ever emit an [`ActionInstance`] whose `schema_id` names one of
/// these -- the worst possible parse error is the *wrong registered*
/// action, never an arbitrary one (§3.5 #1).
#[derive(Debug, Clone, Copy)]
pub struct ActionSchema {
    pub id: &'static str,
    /// Default/base tier for this schema. Some families escalate the
    /// *effective* tier at resolve time (see [`crate::escalation`]) without
    /// mutating this declared value -- e.g. `app.quit` is declared `T1` but
    /// resolves to an effective `T2` when the target has unsaved changes.
    pub tier: Tier,
    pub slots: &'static [SlotSpec],
    pub invertible: Invertibility,
}

/// Compass/relative direction used by window-management and scroll/navigate
/// slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    Next,
    Previous,
}

/// A bound value for one [`SlotSpec`] on an [`ActionInstance`]. The variant
/// in use must match the corresponding `SlotSpec::kind`; `voice-intent`'s
/// constrained decoder is responsible for that invariant upstream, this
/// crate treats a mismatch as a resolve-time `NotRegistered`/refusal rather
/// than panicking.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotValue {
    AppRef(String),
    ElementRef(String),
    Direction(Direction),
    Ordinal(u32),
    Percentage(u8),
    ShortcutName(String),
}

/// A single parsed command: which schema, with which slots bound to
/// user-utterance-derived values, not yet resolved against live UI state.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionInstance {
    pub schema_id: &'static str,
    pub slots: Vec<SlotValue>,
}

impl ActionInstance {
    pub fn new(schema_id: &'static str, slots: Vec<SlotValue>) -> Self {
        Self { schema_id, slots }
    }

    /// Convenience accessor for the (first) `ElementRef` slot value, used
    /// by UI-interaction resolution to find a label to match against the
    /// [`crate::target::ActionableMap`].
    pub fn element_ref(&self) -> Option<&str> {
        self.slots.iter().find_map(|s| match s {
            SlotValue::ElementRef(label) => Some(label.as_str()),
            _ => None,
        })
    }

    /// Convenience accessor for the (first) `AppRef` slot value.
    pub fn app_ref(&self) -> Option<&str> {
        self.slots.iter().find_map(|s| match s {
            SlotValue::AppRef(name) => Some(name.as_str()),
            _ => None,
        })
    }

    /// Convenience accessor for the (first) `ShortcutName` slot value.
    pub fn shortcut_name(&self) -> Option<&str> {
        self.slots.iter().find_map(|s| match s {
            SlotValue::ShortcutName(name) => Some(name.as_str()),
            _ => None,
        })
    }
}
