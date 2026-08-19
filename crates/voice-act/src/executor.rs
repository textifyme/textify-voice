//! `ActionExecutor` trait. COMMANDS-SPEC.md §3.3.

use crate::authorize::Authorized;
use crate::errors::ActError;
use crate::resolution::Resolution;
use crate::schema::{ActionInstance, ActionSchema};
use crate::target::ActionableMap;
use crate::undo::UndoEntry;

/// Every family of actions (app lifecycle, window mgmt, UI interaction,
/// ...) implements this against its own OS boundary. In this run every
/// implementation is an in-memory mock (see [`crate::mock`]) -- no cpal,
/// no AXUIElement, no UIAutomation.
pub trait ActionExecutor {
    fn schemas(&self) -> &[ActionSchema];

    /// Bind `a`'s slots to live targets in `ctx`. Must never silently pick
    /// among near-tied candidates (COMMANDS-SPEC.md §3.3) and must refuse
    /// secure-context targets independent of tier (§3.5 #3).
    fn resolve(&self, a: &ActionInstance, ctx: &ActionableMap) -> Resolution;

    /// Execute an action the tier gate has already cleared.
    ///
    /// This deliberately does **not** accept a bare `Resolution`. The only
    /// way to obtain an `Authorized<'_>` is [`crate::authorize::authorize`],
    /// which runs the resolution's tier and secure-ness -- derived from the
    /// `Resolution` itself, never from a caller-supplied flag -- through
    /// [`crate::gate::decide`] (COMMANDS-SPEC.md §3.5 #2). That makes the
    /// unsafe call structurally unrepresentable: there is no way to reach
    /// this method for a `T2` action without a confirmation that actually
    /// arrived within the timeout, and no way to reach it at all for `T3`.
    /// See `crate::authorize`'s module docs and tests for the exhaustive
    /// argument.
    fn execute(&self, authorized: &Authorized<'_>) -> Result<UndoEntry, ActError>;
}
