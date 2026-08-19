//! Live-UI grounding types that `ActionExecutor::resolve` reads against.
//!
//! COMMANDS-SPEC.md §3.2 assigns the real "actionable-element map (roles,
//! labels, secure flags)" to `crates/voice-context`. That crate is out of
//! scope for this unit (and, in this run, does not exist yet as a buildable
//! crate). `voice-act` only needs the *shape* resolve() reads: a flat list
//! of candidate targets with a label, a role, and a secure flag. We define
//! that minimal shape locally so this crate builds and tests standalone;
//! `voice-context`'s eventual map is expected to be adapted into this shape
//! (or this type re-exported from there) rather than this crate depending
//! on a sibling that isn't part of this run.

/// Coarse role of an actionable element, enough to route resolution without
/// needing the full AX role taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ElementRole {
    App,
    Window,
    Button,
    Field,
    Checkbox,
    Tab,
    Shortcut,
    MenuItem,
}

/// One candidate target: an app, window, button, field, etc. that a
/// resolved [`crate::schema::ActionInstance`] can bind to.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionableElement {
    pub id: String,
    pub role: ElementRole,
    pub label: String,
    /// COMMANDS-SPEC.md §3.5 #3: secure keyboard entry / password fields.
    /// No clicking, no typing, no reading -- checked before anything else,
    /// independent of tier.
    pub secure: bool,
    /// Used by the `app.quit` escalation rule (quit-with-unsaved -> T2).
    pub has_unsaved_changes: bool,
}

impl ActionableElement {
    pub fn new(id: impl Into<String>, role: ElementRole, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role,
            label: label.into(),
            secure: false,
            has_unsaved_changes: false,
        }
    }

    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    pub fn unsaved(mut self, unsaved: bool) -> Self {
        self.has_unsaved_changes = unsaved;
        self
    }
}

/// Snapshot of currently-groundable targets, passed into `resolve()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionableMap {
    pub elements: Vec<ActionableElement>,
}

impl ActionableMap {
    pub fn new(elements: Vec<ActionableElement>) -> Self {
        Self { elements }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn find(&self, id: &str) -> Option<&ActionableElement> {
        self.elements.iter().find(|e| e.id == id)
    }

    pub fn by_role(&self, role: ElementRole) -> impl Iterator<Item = &ActionableElement> {
        self.elements.iter().filter(move |e| e.role == role)
    }
}
