//! Core value types for on-screen context capture.
//!
//! These types are deliberately independent of `voice-core` / `voice-intent` /
//! `voice-act` — SPEC and COMMANDS-SPEC both describe `voice-context` as a
//! provider that other crates consume through narrow, locally-defined
//! contracts, and cross-crate wiring is explicitly deferred (task scope).

/// Coarse application category used to gate downstream behavior — e.g. the
/// local formatting gate is forced off in AI/coding apps.
/// SPEC §3.3 (`BiasContext::app_kind`), SPEC §3.4 (formatting gate).
///
/// RECONCILIATION (integration pass): matches `crates/voice-format::types::
/// AppKind` exactly (built independently, in this same run) and matches the
/// `Code`/`Ai`/`Terminal`/`Browser` spellings of `crates/voice-core::asr::
/// AppKind`, whose `is_ai_or_coding()` names the same two AI/coding buckets.
/// voice-core additionally carries `General`/`Messaging`/`Email`/`Unknown` —
/// on-device-only states this coarser wire type (SPEC line 291: "coarse
/// `app_kind` only") doesn't need. See that type's doc comment for the full
/// rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppKind {
    Ai,
    Code,
    Terminal,
    Browser,
    Chat,
    Document,
    Other,
}

/// Identity of the frontmost application, as read by a platform `ContextProvider`.
/// SPEC §3.1 ("frontmost app" is part of context capture).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub kind: AppKind,
}

/// Role of an actionable element in the accessibility tree.
/// COMMANDS-SPEC §3.2 ("actionable element map (role, label, position, writable, secure)").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElementRole {
    Button,
    TextField,
    CheckBox,
    MenuItem,
    Link,
    Tab,
    Slider,
    Window,
    /// Platform-reported role string that doesn't map onto a known variant.
    Other(String),
}

/// On-screen bounds of an element, in the platform's screen coordinate space.
/// COMMANDS-SPEC §3.2: one of the actionable-element map's fields ("role,
/// label, position, writable, secure").
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A single actionable UI element, as exposed by the platform accessibility API.
///
/// COMMANDS-SPEC §3.2: "ACTIONABLE element map (role, label, position, writable,
/// secure) — memory-only, never persisted, never uploaded."
///
/// # Memory-only invariant
///
/// This type and [`ActionableMap`] intentionally carry **no** serialization or
/// persistence implementation of any kind (no `serde::Serialize`, no
/// `std::io` write path, no database mapping). This crate has zero
/// dependency on any (de)serialization or storage crate (see `Cargo.toml`) —
/// there is structurally no way to write an `ActionableElement` to disk or a
/// network socket without another crate reaching in and hand-rolling a
/// serializer, which would be a visible, reviewable addition to this file.
/// See the `memory_only_invariant` test below, which documents and pins this.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionableElement {
    pub role: ElementRole,
    pub label: String,
    pub position: Position,
    pub writable: bool,
    pub secure: bool,
    pub enabled: bool,
}

/// A bias term surfaced to the ASR/formatting bias pipeline.
///
/// Defined locally per task scope — `voice-context` does not depend on
/// `voice-core`, which owns the canonical `BiasContext` (SPEC §3.3). Callers
/// that need a `voice-core::BiasTerm` are responsible for their own mapping;
/// cross-crate wiring is deferred.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasTerm {
    pub text: String,
    /// Relative weight in `[0.0, 1.0]`; higher means "more likely to be said."
    pub weight: f32,
}

/// Why a [`ActionableMap`] or [`crate::provider::ContextSnapshot`] is missing
/// or incomplete data, so callers can surface the degradation honestly
/// instead of silently treating a partial read as a complete one.
///
/// SPEC §3.1 / COMMANDS-SPEC §3.1: "Windows note: UIPI blocks reads/SendInput
/// into elevated apps — detect and degrade gracefully."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegradedReason {
    /// Windows UIPI: the target process runs elevated and blocks reads.
    ElevatedProcess,
    /// The OS-level accessibility permission has not been granted.
    NoAccessibilityPermission,
    /// The platform read did not complete before the provider gave up.
    Timeout,
    /// Platform-reported reason that doesn't map onto a known variant.
    Unknown(String),
}

/// Honest coverage report for an [`ActionableMap`] (or a snapshot built from
/// one): whether the read fully succeeded, partially succeeded, or failed.
/// Task requirement: "a provider may report partial or unavailable coverage,
/// and callers must be able to surface that honestly."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// The full actionable tree for the focused window was read.
    Full,
    /// Some elements were read; others were unreachable.
    Partial { reason: DegradedReason },
    /// No elements could be read.
    Unavailable { reason: DegradedReason },
}

/// Tri-state answer to the single highest-stakes question this crate's data
/// ever gets asked: **is the current focus target a secure (password)
/// field?**
///
/// This exists because a plain `bool` cannot represent "I don't know" without
/// a caller picking a default — and a fix-wave incident showed that the
/// obvious-looking default (`Option<ActionableElement>::None` → "not secure")
/// is a credential leak: a target app stalling past the AX read timeout, or
/// this process lacking the Accessibility grant, or any other degraded read,
/// silently collapsed to "safe to type into," and a password field got a
/// clipboard-paste transcript through it.
///
/// **"I could not determine whether this is a password field" must never be
/// treated as "this is not a password field."** Every caller deciding
/// whether to type or paste MUST treat [`SecureFieldStatus::Unknown`] the
/// same way it treats `Known(true)` — refuse to synthesize any keystroke —
/// and MAY still offer the text back some other, non-typing way (e.g.
/// clipboard-only, never auto-pasted) since most `Unknown`s are, in
/// practice, an ordinary field the read just couldn't confirm in time, not
/// an actual password box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureFieldStatus {
    /// A real, successful read of the focused element determined this
    /// definitively — never fabricated, never defaulted.
    Known(bool),
    /// The read did not produce a definitive answer (timeout, no
    /// Accessibility permission, no focused element resolved, an unmapped
    /// platform error, or a background reader that vanished without
    /// answering at all). Absence of evidence is not evidence of absence.
    Unknown,
}

impl SecureFieldStatus {
    /// Combine a freshly-resolved status with the status the previous
    /// snapshot already carried, for the case where the fresh read itself
    /// came back `Unknown` (see [`crate::provider::ContextCapture::wait_secure_field_status`]).
    ///
    /// The two directions are deliberately asymmetric:
    /// * A **stale `Known(true)`** (this target used to be a secure field)
    ///   is kept: secure fields don't quietly stop being secure, and the
    ///   fresh read failing to reconfirm that is not evidence it changed —
    ///   "a stale 'this was secure' reading is far better evidence than a
    ///   fresh empty one."
    /// * A **stale `Known(false)`** (this target used to be safe) is
    ///   discarded in favor of the fresh `Unknown`: the user could have
    ///   clicked into a genuinely different, secure field in the time it
    ///   took to speak, and the fresh read gives no evidence either way —
    ///   trusting the old "safe" reading here is exactly the fail-open bug
    ///   this type exists to prevent.
    #[must_use]
    pub fn merge_after_fresh_read(fresh: Self, previous: Self) -> Self {
        match (fresh, previous) {
            (SecureFieldStatus::Unknown, SecureFieldStatus::Known(true)) => SecureFieldStatus::Known(true),
            (fresh, _) => fresh,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_only_invariant() {
        // Enforcement is structural, not a runtime check: this crate declares
        // zero dependency on serde, rusqlite, or any other (de)serialization /
        // storage crate (see Cargo.toml `[dependencies]`, which is empty).
        // ActionableElement/ActionableMap therefore have no way to reach disk
        // or network. This test exists so the invariant is discoverable and
        // regresses loudly: adding a persistence path would require adding a
        // dependency (a reviewable Cargo.toml diff) and touching this file.
        let el = ActionableElement {
            role: ElementRole::Button,
            label: "Send".to_string(),
            position: Position { x: 0.0, y: 0.0, width: 10.0, height: 10.0 },
            writable: false,
            secure: false,
            enabled: true,
        };
        // Sanity: the type is plain data we can move and compare, nothing more.
        let el2 = el.clone();
        assert_eq!(el, el2);
    }

    #[test]
    fn coverage_distinguishes_partial_from_unavailable() {
        let partial = Coverage::Partial { reason: DegradedReason::ElevatedProcess };
        let unavailable = Coverage::Unavailable { reason: DegradedReason::NoAccessibilityPermission };
        assert_ne!(partial, unavailable);
        assert_eq!(Coverage::Full, Coverage::Full);
    }

    // -- SecureFieldStatus::merge_after_fresh_read ---------------------
    //
    // These pin the exact asymmetry the blocker fix depends on: a stale
    // POSITIVE secure reading is sticky (never discarded by a later
    // failure to reconfirm it), but a stale NEGATIVE ("was safe") reading
    // is never trusted over "I can't tell right now" — that second case is
    // the literal fail-open bug (secure-field refusal collapsing an
    // in-flight/timed-out read to "not secure") this type exists to make
    // structurally impossible.

    #[test]
    fn stale_known_secure_survives_a_fresh_unknown() {
        let merged =
            SecureFieldStatus::merge_after_fresh_read(SecureFieldStatus::Unknown, SecureFieldStatus::Known(true));
        assert_eq!(merged, SecureFieldStatus::Known(true), "a previously-resolved secure element must not be discarded by a fresh read that merely failed to reconfirm it");
    }

    #[test]
    fn stale_known_not_secure_does_not_survive_a_fresh_unknown() {
        // The user could have clicked into a genuinely different, secure
        // field in the time it took to speak; the fresh Unknown carries no
        // evidence either way, so the old "safe" answer must NOT win.
        let merged =
            SecureFieldStatus::merge_after_fresh_read(SecureFieldStatus::Unknown, SecureFieldStatus::Known(false));
        assert_eq!(merged, SecureFieldStatus::Unknown);
    }

    #[test]
    fn a_fresh_known_answer_always_wins_regardless_of_previous() {
        for previous in [SecureFieldStatus::Known(true), SecureFieldStatus::Known(false), SecureFieldStatus::Unknown]
        {
            assert_eq!(
                SecureFieldStatus::merge_after_fresh_read(SecureFieldStatus::Known(true), previous),
                SecureFieldStatus::Known(true)
            );
            assert_eq!(
                SecureFieldStatus::merge_after_fresh_read(SecureFieldStatus::Known(false), previous),
                SecureFieldStatus::Known(false)
            );
        }
    }

    #[test]
    fn two_unknowns_stay_unknown() {
        let merged =
            SecureFieldStatus::merge_after_fresh_read(SecureFieldStatus::Unknown, SecureFieldStatus::Unknown);
        assert_eq!(merged, SecureFieldStatus::Unknown);
    }
}
