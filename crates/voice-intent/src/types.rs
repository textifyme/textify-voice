//! Core intent-pipeline contracts. COMMANDS-SPEC §3.3, §3.4.
//!
//! `voice-intent` defines its own minimal `ActionInstance` / `SlotValue`
//! types rather than depending on `crates/voice-act`'s `ActionSchema`
//! registry — the two crates stay decoupled per the unit that produced
//! this crate. `voice-act` remains the source of truth for tiers,
//! invertibility, and the live schema registry; this crate only needs to
//! *name* a schema id and carry typed slot values extracted from the
//! utterance.

use std::fmt;

/// Outcome of running an utterance through the intent pipeline.
/// COMMANDS-SPEC §3.3.
#[derive(Debug, Clone, PartialEq)]
pub enum IntentResult {
    /// The utterance was bound to a registered action.
    Matched {
        action: ActionInstance,
        /// Grammar (stage 1) | LocalLlm (stage 2).
        stage: MatchStage,
        confidence: f32,
    },
    /// The utterance did not produce an action. The HUD shows a single
    /// "didn't catch a command" message regardless of which reason fired
    /// (COMMANDS-SPEC §3.3) — the reason exists for telemetry/debugging,
    /// never surfaced verbatim to the user.
    Reject { reason: RejectReason },
}

/// Which stage produced a `Matched` result. COMMANDS-SPEC §3.3, §3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchStage {
    /// Stage 1: deterministic grammar table match, <20 ms class work.
    Grammar,
    /// Stage 2: constrained local LLM parse over the closed schema set.
    LocalLlm,
}

/// Why the pipeline rejected an utterance. COMMANDS-SPEC §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Nothing in the utterance anchors to a registered command shape —
    /// this is also the bucket for dictation-lookalike prose that merely
    /// *contains* command words without being one (COMMANDS-SPEC C0.0,
    /// §7 "dictation-lookalike adversarials must reject at ≥99%"). The
    /// default, most common reject reason from stage 1.
    NotACommand,
    /// The utterance names multiple plausible candidates with no clean
    /// preference between them. Grammar (stage 1) never produces this on
    /// its own — pattern literals are disjoint by construction, so a
    /// single utterance matches at most one grammar pattern — it is
    /// surfaced by slot *resolution* against live UI state
    /// (`voice-act::Resolution::NeedsDisambiguation`, COMMANDS-SPEC §3.3
    /// "Disambiguation is a dialogue, not a guess"). Reserved here so
    /// stage 2 and the resolve step share one closed reason set.
    Ambiguous,
    /// The utterance matches a recognized command *shape* but a required
    /// slot was missing or could not be parsed into its typed value —
    /// e.g. "open" with no app named, "set volume to bananas percent",
    /// "close all windows" with no app before "windows". Distinct from
    /// `NotACommand`: the grammar *skeleton* was recognized, only the
    /// slot content was invalid.
    Unsupported,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            RejectReason::NotACommand => "not-a-command",
            RejectReason::Ambiguous => "ambiguous",
            RejectReason::Unsupported => "unsupported",
        };
        f.write_str(s)
    }
}

/// A bound action ready for `voice-act` to resolve against live UI state.
/// Minimal local stand-in for `voice-act::ActionSchema` + bound slots —
/// see module doc for why this crate does not depend on `voice-act`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionInstance {
    /// One of the closed schema ids, e.g. `"app.switch"`, `"win.tile"`,
    /// `"ui.click"`, `"sys.volume"`, `"shortcut.run"`. Owned by
    /// `voice-act`'s registry; this crate only names it.
    pub schema_id: &'static str,
    /// Slot values extracted from the utterance, in schema-defined order.
    pub slots: Vec<SlotValue>,
}

/// A typed slot value. COMMANDS-SPEC §3.3: `ActionSchema.slots` is
/// "typed: AppRef, ElementRef, Direction, Ordinal, Percentage,
/// ShortcutName" — one variant per named type.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotValue {
    /// A named application, e.g. "Slack", "Visual Studio Code".
    AppRef(String),
    /// A named on-screen element/label, e.g. "Send", "Cancel".
    ElementRef(String),
    /// A directional/relational value shared across scroll, window
    /// tiling, tab navigation, and volume/brightness commands.
    Direction(Direction),
    /// A 1-based ordinal/count, e.g. `3` in "click number three".
    Ordinal(u32),
    /// A percentage in `0..=100`, e.g. `50` in "set volume to 50 percent".
    Percentage(u8),
    /// A user Shortcut name, e.g. "Morning Routine".
    ShortcutName(String),
}

/// Direction/relational slot values. COMMANDS-SPEC §3.3 typed-slots list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    Top,
    Bottom,
    Next,
    Previous,
    Back,
}

/// The closed resolution sets a caller supplies to stage 1 for slot
/// resolution — the "on-screen nouns" half of COMMANDS-SPEC §3.1's "a
/// near-closed vocabulary over verbs + on-screen nouns". A `Rest`/
/// `RestUntil` slot capture (`AppRef`, `ElementRef`, `ShortcutName`) is
/// only accepted as part of a match if it resolves — tolerantly (case,
/// spacing, common suffixes), since these names arrive transcribed by
/// ASR — against the relevant set here. See [`crate::grammar`] module doc
/// for the mechanism this replaces and why.
///
/// An EMPTY (or entirely default) `CommandContext` is not a special case
/// requiring its own branch — it fails closed *by construction*: nothing
/// can resolve against an empty list, so any pattern with a free-text
/// slot falls through to [`RejectReason::NotACommand`] exactly as if a
/// non-empty but non-matching context had been supplied. This is the
/// deliberate behavior when the caller has no known-app/element/shortcut
/// data yet (e.g. AX read failed, app-list enumeration errored, or
/// `voice-context` hasn't populated this call): stage 1 never falls back
/// to accepting free text just because context is missing, because that
/// would reinstate the exact bug this type exists to close.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandContext {
    /// Installed/running application names (COMMANDS-SPEC §3.1
    /// "installed-app names") that an `AppRef` slot may resolve against.
    pub known_apps: Vec<String>,
    /// Focused-window on-screen element labels (§3.1 "focused-window AX
    /// labels") that an `ElementRef` slot may resolve against.
    pub known_elements: Vec<String>,
    /// User Shortcut names known locally that a `ShortcutName` slot may
    /// resolve against.
    pub known_shortcuts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_reason_display_matches_spec_kebab_names() {
        assert_eq!(RejectReason::NotACommand.to_string(), "not-a-command");
        assert_eq!(RejectReason::Ambiguous.to_string(), "ambiguous");
        assert_eq!(RejectReason::Unsupported.to_string(), "unsupported");
    }

    #[test]
    fn action_instance_carries_typed_slots() {
        let a = ActionInstance {
            schema_id: "app.switch",
            slots: vec![SlotValue::AppRef("Slack".to_string())],
        };
        assert_eq!(a.schema_id, "app.switch");
        assert_eq!(a.slots, vec![SlotValue::AppRef("Slack".to_string())]);
    }

    #[test]
    fn command_context_default_is_empty_on_every_set() {
        // The fail-closed guarantee starts here: `Default` must not
        // secretly populate any set, or "no context supplied" would stop
        // meaning "nothing resolves".
        let ctx = CommandContext::default();
        assert!(ctx.known_apps.is_empty());
        assert!(ctx.known_elements.is_empty());
        assert!(ctx.known_shortcuts.is_empty());
    }
}
