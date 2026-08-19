//! `voice-act` — the closed action registry and safety model for Textify's
//! Command Mode. COMMANDS-SPEC.md §3.3-§3.5.
//!
//! This crate is deliberately pure logic: no cpal, no AX/UIAutomation
//! bindings, no SQLite, no network. Every OS boundary is behind the
//! [`executor::ActionExecutor`] trait; the only implementation shipped here
//! is [`mock::MockDesktopExecutor`], an in-memory stand-in that exercises
//! the whole registry without touching a real desktop.
//!
//! Module map:
//! - [`schema`]: `ActionSchema`, `Tier`, `Invertibility`, `SlotSpec`,
//!   `ActionInstance` — the closed action-set contracts (§3.3).
//! - [`target`]: a minimal local stand-in for the actionable-element map
//!   `resolve()` reads against (the real one belongs to `voice-context`,
//!   out of scope here — see that module's doc comment).
//! - [`resolution`]: `Resolution` (`Bound` / `NeedsDisambiguation` /
//!   `Refused`), never picking silently among near-ties (§3.3).
//! - [`disambiguate`]: label similarity scoring (own Levenshtein
//!   implementation) and near-tie detection.
//! - [`escalation`]: the destructive-label lexicon and the pure functions
//!   that compute an action's *effective* tier from its declared base tier.
//! - [`gate`]: the tier gate itself — T0 auto-execute, T1 execute+announce,
//!   T2 confirm-with-default-deny-on-timeout, T3 never (§3.5 #2).
//! - [`authorize`]: the sole path from a `Resolution` to something
//!   `ActionExecutor::execute` accepts -- the tier gate enforced
//!   structurally, not just advisorily (§3.5 #2).
//! - [`undo`]: the in-memory undo/redo journal with a pluggable persistence
//!   seam and `NeverStore` support.
//! - [`telemetry`]: a telemetry event shape that cannot structurally carry
//!   utterance text or target labels (§3.5 #6).
//! - [`registry`]: the closed, statically-declared launch action surface
//!   (§3.4), plus regression tests pinning tiers and the excluded list.
//! - [`executor`]: the `ActionExecutor` trait.
//! - [`mock`]: in-memory executors wiring the whole registry together.

pub mod authorize;
pub mod disambiguate;
pub mod errors;
pub mod escalation;
pub mod executor;
pub mod gate;
pub mod mock;
pub mod registry;
pub mod resolution;
pub mod schema;
pub mod target;
pub mod telemetry;
pub mod undo;

pub use authorize::{authorize, Authorized, AuthorizeError};
pub use errors::ActError;
pub use executor::ActionExecutor;
pub use registry::ActionRegistry;
pub use resolution::{BoundTarget, Candidate, RefusalReason, Resolution};
pub use schema::{
    ActionInstance, ActionSchema, Direction, Invertibility, SlotKind, SlotSpec, SlotValue, Tier,
};
pub use target::{ActionableElement, ActionableMap, ElementRole};
pub use undo::{UndoAction, UndoEntry, UndoJournal, UndoPersistence};
