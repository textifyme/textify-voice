//! `resolve()` output types. COMMANDS-SPEC.md §3.3.

use crate::schema::{ActionInstance, Tier};
use crate::target::ElementRole;

/// Why `resolve()` refused to bind an [`ActionInstance`] at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// COMMANDS-SPEC.md §3.5 #3: secure keyboard entry / password field.
    /// Checked before tier, before anything else -- a T0 action against a
    /// secure target is refused exactly like a T2 one.
    SecureContext,
    /// No candidate target found in the `ActionableMap`.
    NotFound,
    /// `schema_id` does not name a registered schema (closed action set,
    /// §3.5 #1) or a slot value's type didn't match the schema's `SlotSpec`.
    NotRegistered,
    /// Tier `T3` -- never allowed, independent of confirmation.
    NeverAllowed,
}

/// A candidate target surfaced during disambiguation, safe to show on the
/// HUD (label only, no secure/internal state).
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub element_id: String,
    pub label: String,
    pub role: ElementRole,
}

/// The live target an [`ActionInstance`] was bound to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundTarget {
    pub element_id: Option<String>,
    pub label: Option<String>,
    /// Whether the bound element is a secure-context target (password
    /// field, secure keyboard entry). Every `ActionExecutor::resolve`
    /// implementation in this crate refuses *before* ever constructing a
    /// `Bound` for a secure element (COMMANDS-SPEC.md §3.5 #3), so this is
    /// always `false` on any `Bound` this crate produces -- it exists so
    /// [`crate::authorize::authorize`] can derive a secure/not-secure fact
    /// straight from the `Resolution` it's holding, as belt-and-suspenders
    /// defense in depth, rather than trusting a caller-supplied flag.
    pub secure: bool,
}

/// Result of `ActionExecutor::resolve`. COMMANDS-SPEC.md §3.3: "Resolution
/// never picks silently among near-ties."
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Bound to exactly one live target, ready to execute at
    /// `effective_tier` (which may be escalated above `schema.tier`, see
    /// [`crate::escalation`]).
    Bound { instance: ActionInstance, target: BoundTarget, effective_tier: Tier },
    /// Two or more candidates are tied/near-tied; the HUD must number
    /// exactly these and wait for the user to pick one. Never resolved to
    /// `Bound` automatically.
    NeedsDisambiguation { instance: ActionInstance, candidates: Vec<Candidate> },
    /// Nothing was bound; execution must not proceed.
    Refused { instance: ActionInstance, reason: RefusalReason },
}

impl Resolution {
    pub fn instance(&self) -> &ActionInstance {
        match self {
            Resolution::Bound { instance, .. } => instance,
            Resolution::NeedsDisambiguation { instance, .. } => instance,
            Resolution::Refused { instance, .. } => instance,
        }
    }

    pub fn effective_tier(&self) -> Option<Tier> {
        match self {
            Resolution::Bound { effective_tier, .. } => Some(*effective_tier),
            _ => None,
        }
    }
}
