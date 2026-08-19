//! In-memory mock executors. Every schema in [`crate::registry::LAUNCH_SCHEMAS`]
//! is dispatchable through [`MockDesktopExecutor`] without touching any real
//! OS API -- no AXUIElement, no UIAutomation, no window server. A handful of
//! families (app lifecycle, window geometry, volume, "undo that") get real
//! in-memory state modeling so undo/redo and tier-escalation behavior can be
//! exercised end to end; the remaining families are mocked with a
//! state-independent no-op that still honors their schema's `Invertibility`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::disambiguate::{match_candidates, MatchOutcome, BINDING_FLOOR};
use crate::errors::ActError;
use crate::escalation::{effective_tier, EscalationContext};
use crate::registry::ActionRegistry;
use crate::resolution::{BoundTarget, Candidate, RefusalReason, Resolution};
use crate::schema::{ActionInstance, ActionSchema, Invertibility};
use crate::target::{ActionableElement, ActionableMap, ElementRole};
use crate::undo::{UndoAction, UndoEntry, UndoJournal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

const DEFAULT_GEOMETRY: WindowGeometry = WindowGeometry { x: 0, y: 0, w: 1200, h: 800 };

impl Default for WindowGeometry {
    fn default() -> Self {
        DEFAULT_GEOMETRY
    }
}

#[derive(Debug, Default)]
pub struct MockDesktopState {
    pub open_apps: HashSet<String>,
    pub focused_window: WindowGeometry,
    pub volume: u8,
    pub muted: bool,
    pub promoted_shortcuts: HashSet<String>,
    pub click_log: Vec<String>,
}

/// Mock executor covering the entire launch registry. Holds shared,
/// interior-mutable state (`Rc<RefCell<..>>`) so that undo/redo closures
/// handed out to an external [`UndoJournal`] can mutate the same state the
/// executor reads, independent of the executor's own borrow lifetime.
pub struct MockDesktopExecutor {
    state: Rc<RefCell<MockDesktopState>>,
    journal: Rc<RefCell<UndoJournal>>,
}

impl MockDesktopExecutor {
    pub fn new(journal: Rc<RefCell<UndoJournal>>) -> Self {
        Self { state: Rc::new(RefCell::new(MockDesktopState::default())), journal }
    }

    pub fn state(&self) -> Rc<RefCell<MockDesktopState>> {
        self.state.clone()
    }

    pub fn open_app(&self, name: impl Into<String>) {
        self.state.borrow_mut().open_apps.insert(name.into());
    }

    pub fn promote_shortcut(&self, name: impl Into<String>) {
        self.state.borrow_mut().promoted_shortcuts.insert(name.into());
    }

    fn label_resolve(
        &self,
        instance: &ActionInstance,
        schema: &ActionSchema,
        role: ElementRole,
        query: Option<&str>,
        ctx: &ActionableMap,
    ) -> Resolution {
        let Some(query) = query else {
            return Resolution::Refused { instance: instance.clone(), reason: RefusalReason::NotRegistered };
        };
        let elements: Vec<_> = ctx.by_role(role).collect();
        match match_candidates(query, &elements, |e| e.label.as_str(), BINDING_FLOOR) {
            MatchOutcome::None => Resolution::Refused { instance: instance.clone(), reason: RefusalReason::NotFound },
            MatchOutcome::Unique(c) => self.bind_secure_checked(instance, schema, c.item),
            MatchOutcome::Tied(tied) => {
                // COMMANDS-SPEC.md §3.5 #3: secure elements must never
                // appear on the HUD, not even as a disambiguation
                // candidate (id *or* label). Re-run the same near-tie
                // classification over the secure-filtered subset rather
                // than just stripping entries out of the already-decided
                // `tied` list, so a "tie" that only existed *because* of a
                // secure element correctly collapses to a clean Unique (or
                // None) once that element is excluded from contention.
                let non_secure: Vec<_> = tied.iter().filter(|c| !c.item.secure).map(|c| *c.item).collect();
                if non_secure.is_empty() {
                    // Every near-tied candidate was secure -- there is
                    // nothing safe to disambiguate among. Report it as a
                    // secure refusal (not a generic not-found) so it stays
                    // as informative as the single-secure-candidate case.
                    return Resolution::Refused { instance: instance.clone(), reason: RefusalReason::SecureContext };
                }
                match match_candidates(query, &non_secure, |e| e.label.as_str(), BINDING_FLOOR) {
                    MatchOutcome::None => {
                        Resolution::Refused { instance: instance.clone(), reason: RefusalReason::NotFound }
                    }
                    MatchOutcome::Unique(c) => self.bind_secure_checked(instance, schema, c.item),
                    MatchOutcome::Tied(tied2) => Resolution::NeedsDisambiguation {
                        instance: instance.clone(),
                        candidates: tied2
                            .into_iter()
                            .map(|c| Candidate { element_id: c.item.id.clone(), label: c.item.label.clone(), role: c.item.role })
                            .collect(),
                    },
                }
            }
        }
    }

    /// Build a `Bound` resolution for `element`, re-verifying the secure
    /// check independent of whatever filtering already happened upstream
    /// (defense in depth -- COMMANDS-SPEC.md §3.5 #3 is "checked before
    /// tier, before anything else"). This is the *only* place a `Bound` is
    /// constructed from a live `ActionableElement` in this executor, so
    /// every resolve path -- the ordinary unique-match path, the
    /// tie-collapses-to-one path, and [`Self::pick_candidate`] -- shares
    /// one secure gate rather than three copies of the same check.
    fn bind_secure_checked(&self, instance: &ActionInstance, schema: &ActionSchema, element: &ActionableElement) -> Resolution {
        if element.secure {
            return Resolution::Refused { instance: instance.clone(), reason: RefusalReason::SecureContext };
        }
        let promoted = element_name_promoted(&self.state, &element.label);
        let tier = effective_tier(
            schema,
            EscalationContext {
                target_label: Some(element.label.as_str()),
                target_has_unsaved_changes: element.has_unsaved_changes,
                shortcut_promoted_to_t1: promoted,
            },
        );
        Resolution::Bound {
            instance: instance.clone(),
            target: BoundTarget { element_id: Some(element.id.clone()), label: Some(element.label.clone()), secure: false },
            effective_tier: tier,
        }
    }

    /// Re-resolve a specific candidate the user picked (by element id) from
    /// a prior `Resolution::NeedsDisambiguation` HUD list. Goes through
    /// [`Self::bind_secure_checked`] exactly like every other resolve path,
    /// so a secure element can never reach `Bound` by being picked, even if
    /// it had somehow slipped into a candidate list upstream.
    pub fn pick_candidate(&self, instance: &ActionInstance, element_id: &str, ctx: &ActionableMap) -> Resolution {
        let Some(schema) = ActionRegistry::by_id(instance.schema_id) else {
            return Resolution::Refused { instance: instance.clone(), reason: RefusalReason::NotRegistered };
        };
        let Some(element) = ctx.find(element_id) else {
            return Resolution::Refused { instance: instance.clone(), reason: RefusalReason::NotFound };
        };
        self.bind_secure_checked(instance, schema, element)
    }

    fn trivial_bound(&self, instance: &ActionInstance, schema: &ActionSchema) -> Resolution {
        let tier = effective_tier(schema, EscalationContext::default());
        Resolution::Bound {
            instance: instance.clone(),
            target: BoundTarget::default(),
            effective_tier: tier,
        }
    }
}

fn element_name_promoted(state: &Rc<RefCell<MockDesktopState>>, label: &str) -> bool {
    state.borrow().promoted_shortcuts.contains(label)
}

fn noop_undo_entry(schema: &ActionSchema) -> UndoEntry {
    match schema.invertible {
        Invertibility::None => UndoEntry::irreversible(schema.id),
        Invertibility::Full => {
            let undo: UndoAction = Box::new(|| Ok(()));
            let redo: UndoAction = Box::new(|| Ok(()));
            UndoEntry::full(schema.id, undo, redo)
        }
        Invertibility::Snapshot => {
            let undo: UndoAction = Box::new(|| Ok(()));
            let redo: UndoAction = Box::new(|| Ok(()));
            UndoEntry::snapshot(schema.id, undo, redo)
        }
    }
}

impl crate::executor::ActionExecutor for MockDesktopExecutor {
    fn schemas(&self) -> &[ActionSchema] {
        ActionRegistry::all()
    }

    fn resolve(&self, a: &ActionInstance, ctx: &ActionableMap) -> Resolution {
        let Some(schema) = ActionRegistry::by_id(a.schema_id) else {
            return Resolution::Refused { instance: a.clone(), reason: RefusalReason::NotRegistered };
        };

        match schema.id {
            "app.open" | "app.switch" | "app.quit" | "app.hide" => {
                self.label_resolve(a, schema, ElementRole::App, a.app_ref(), ctx)
            }
            "ui.click" | "ui.focus_field" | "ui.toggle_checkbox" => {
                self.label_resolve(a, schema, ElementRole::Button, a.element_ref(), ctx)
            }
            "shortcut.run" => self.label_resolve(a, schema, ElementRole::Shortcut, a.shortcut_name(), ctx),
            _ => self.trivial_bound(a, schema),
        }
    }

    fn execute(&self, authorized: &crate::authorize::Authorized<'_>) -> Result<UndoEntry, ActError> {
        // `authorized` can only have been minted by `authorize::authorize`,
        // which already required `resolution()` to be `Bound` (and
        // non-secure, and gate-cleared for its tier) -- the non-Bound arm
        // here is unreachable in practice, kept only so this match stays
        // exhaustive and defensive rather than assuming the invariant.
        let (instance, target) = match authorized.resolution() {
            Resolution::Bound { instance, target, .. } => (instance, target),
            Resolution::NeedsDisambiguation { .. } | Resolution::Refused { .. } => {
                return Err(ActError::NotResolved)
            }
        };

        let Some(schema) = ActionRegistry::by_id(instance.schema_id) else {
            return Err(ActError::Refused(RefusalReason::NotRegistered));
        };

        match schema.id {
            "app.quit" => self.execute_app_quit(target),
            "app.open" => self.execute_app_open(target),
            "win.maximize" | "win.tile_left" | "win.tile_right" | "win.minimize" => {
                self.execute_window_op(schema.id, geometry_for(schema.id))
            }
            "sys.volume" => self.execute_volume(instance),
            "sys.mute" => self.execute_mute(),
            "ui.click" => self.execute_click(target),
            "meta.undo" => self.execute_meta_undo(),
            _ => {
                self.state.borrow_mut().click_log.push(schema.id.to_string());
                Ok(noop_undo_entry(schema))
            }
        }
    }
}

fn geometry_for(schema_id: &str) -> WindowGeometry {
    match schema_id {
        "win.maximize" => WindowGeometry { x: 0, y: 0, w: 1920, h: 1080 },
        "win.tile_left" => WindowGeometry { x: 0, y: 0, w: 960, h: 1080 },
        "win.tile_right" => WindowGeometry { x: 960, y: 0, w: 960, h: 1080 },
        "win.minimize" => WindowGeometry { x: 0, y: 0, w: 0, h: 0 },
        _ => DEFAULT_GEOMETRY,
    }
}

impl MockDesktopExecutor {
    fn execute_app_quit(&self, target: &BoundTarget) -> Result<UndoEntry, ActError> {
        let Some(app) = target.label.clone() else { return Err(ActError::NotResolved) };
        self.state.borrow_mut().open_apps.remove(&app);

        let (s1, a1) = (self.state.clone(), app.clone());
        let undo: UndoAction = Box::new(move || {
            s1.borrow_mut().open_apps.insert(a1.clone());
            Ok(())
        });
        let (s2, a2) = (self.state.clone(), app);
        let redo: UndoAction = Box::new(move || {
            s2.borrow_mut().open_apps.remove(&a2);
            Ok(())
        });
        Ok(UndoEntry::full("app.quit", undo, redo))
    }

    fn execute_app_open(&self, target: &BoundTarget) -> Result<UndoEntry, ActError> {
        let Some(app) = target.label.clone() else { return Err(ActError::NotResolved) };
        self.state.borrow_mut().open_apps.insert(app.clone());

        let (s1, a1) = (self.state.clone(), app.clone());
        let undo: UndoAction = Box::new(move || {
            s1.borrow_mut().open_apps.remove(&a1);
            Ok(())
        });
        let (s2, a2) = (self.state.clone(), app);
        let redo: UndoAction = Box::new(move || {
            s2.borrow_mut().open_apps.insert(a2.clone());
            Ok(())
        });
        Ok(UndoEntry::full("app.open", undo, redo))
    }

    fn execute_window_op(&self, schema_id: &'static str, new_geometry: WindowGeometry) -> Result<UndoEntry, ActError> {
        let previous = self.state.borrow().focused_window;
        self.state.borrow_mut().focused_window = new_geometry;

        let s1 = self.state.clone();
        let restore: UndoAction = Box::new(move || {
            s1.borrow_mut().focused_window = previous;
            Ok(())
        });
        let s2 = self.state.clone();
        let reapply: UndoAction = Box::new(move || {
            s2.borrow_mut().focused_window = new_geometry;
            Ok(())
        });
        Ok(UndoEntry::snapshot(schema_id, restore, reapply))
    }

    fn execute_volume(&self, instance: &ActionInstance) -> Result<UndoEntry, ActError> {
        let target_pct = instance.slots.iter().find_map(|s| match s {
            crate::schema::SlotValue::Percentage(p) => Some(*p),
            _ => None,
        });
        let Some(target_pct) = target_pct else { return Err(ActError::NotResolved) };

        let previous = self.state.borrow().volume;
        self.state.borrow_mut().volume = target_pct;

        let s1 = self.state.clone();
        let undo: UndoAction = Box::new(move || {
            s1.borrow_mut().volume = previous;
            Ok(())
        });
        let s2 = self.state.clone();
        let redo: UndoAction = Box::new(move || {
            s2.borrow_mut().volume = target_pct;
            Ok(())
        });
        Ok(UndoEntry::full("sys.volume", undo, redo))
    }

    fn execute_mute(&self) -> Result<UndoEntry, ActError> {
        {
            let mut s = self.state.borrow_mut();
            s.muted = !s.muted;
        }
        let s1 = self.state.clone();
        let undo: UndoAction = Box::new(move || {
            let mut s = s1.borrow_mut();
            s.muted = !s.muted;
            Ok(())
        });
        let s2 = self.state.clone();
        let redo: UndoAction = Box::new(move || {
            let mut s = s2.borrow_mut();
            s.muted = !s.muted;
            Ok(())
        });
        Ok(UndoEntry::full("sys.mute", undo, redo))
    }

    fn execute_click(&self, target: &BoundTarget) -> Result<UndoEntry, ActError> {
        let label = target.label.clone().unwrap_or_default();
        self.state.borrow_mut().click_log.push(label);
        // ui.click is declared Invertibility::None -- a click is not
        // generically undoable (the app-specific effect is unknown).
        Ok(UndoEntry::irreversible("ui.click"))
    }

    /// "undo that" is itself T0 (COMMANDS-SPEC.md §3.4 Meta) and Full
    /// invertible: undoing the undo is exactly a redo of the original
    /// journal, so its own `undo` closure calls back into `journal.redo()`.
    fn execute_meta_undo(&self) -> Result<UndoEntry, ActError> {
        self.journal.borrow_mut().undo()?;

        let j1 = self.journal.clone();
        let undo: UndoAction = Box::new(move || j1.borrow_mut().redo());
        let j2 = self.journal.clone();
        let redo: UndoAction = Box::new(move || j2.borrow_mut().undo());
        Ok(UndoEntry::full("meta.undo", undo, redo))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::authorize::{authorize, AuthorizeError};
    use crate::executor::ActionExecutor;
    use crate::gate::{decide, GateDecision, T2Confirmation, UserResponse};
    use crate::schema::{Direction, SlotValue, Tier};
    use crate::target::ActionableElement;
    use std::time::Duration;

    fn journal() -> Rc<RefCell<UndoJournal>> {
        Rc::new(RefCell::new(UndoJournal::new(crate::undo::NoopPersistence)))
    }

    fn push_result(j: &Rc<RefCell<UndoJournal>>, r: Result<UndoEntry, ActError>) {
        if let Ok(entry) = r {
            j.borrow_mut().push(entry);
        }
    }

    #[test]
    fn every_registered_schema_resolves_and_executes_without_panicking() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j.clone());
        exec.open_app("Finder");
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("app-finder", ElementRole::App, "Finder"),
            ActionableElement::new("btn-ok", ElementRole::Button, "OK"),
            ActionableElement::new("sc-1", ElementRole::Shortcut, "Archive Inbox"),
        ]);

        for schema in ActionRegistry::all() {
            let slots = match schema.id {
                "app.open" | "app.switch" | "app.quit" | "app.hide" => vec![SlotValue::AppRef("Finder".into())],
                "ui.click" | "ui.focus_field" | "ui.toggle_checkbox" => vec![SlotValue::ElementRef("OK".into())],
                "shortcut.run" => vec![SlotValue::ShortcutName("Archive Inbox".into())],
                "sys.volume" | "sys.brightness" => vec![SlotValue::Percentage(50)],
                "nav.scroll" => vec![SlotValue::Direction(Direction::Down)],
                _ => vec![],
            };
            let instance = ActionInstance::new(schema.id, slots);
            let resolution = exec.resolve(&instance, &ctx);
            // Every schema must resolve to *something* concrete here (the
            // fixtures were chosen to match); a Bound resolution must then
            // authorize and execute without error. An affirmative,
            // well-within-timeout confirmation is supplied unconditionally
            // -- it's a no-op for T0/T1 and is exactly what's needed for
            // `shortcut.run`'s default T2, so this test still exercises
            // every schema's dispatch/undo-shape logic without also being
            // a (redundant) gate test.
            if let Resolution::Bound { .. } = &resolution {
                let confirmation = T2Confirmation::default();
                let authorized = authorize(&resolution, &confirmation, Duration::ZERO, Some(UserResponse::Yes))
                    .unwrap_or_else(|e| panic!("{} failed to authorize: {e:?}", schema.id));
                let result = exec.execute(&authorized);
                if schema.id == "meta.undo" {
                    // meta.undo's success is a function of whatever the
                    // journal's top entry happens to be at this point in
                    // iteration order (possibly an irreversible entry from
                    // a schema executed just before it) -- that's a
                    // legitimate, non-panicking outcome, not a bug. The
                    // dedicated `app_quit_then_undo_reopens_the_app` test
                    // covers the success path explicitly.
                    assert!(
                        matches!(&result, Ok(_) | Err(ActError::NotInvertible) | Err(ActError::NothingToUndo)),
                        "meta.undo returned an unexpected error: {result:?}"
                    );
                } else {
                    assert!(result.is_ok(), "{} failed to execute: {result:?}", schema.id);
                }
                // Push into the journal as a real caller would, so later
                // schemas (including meta.undo itself) see realistic state.
                push_result(&j, result);
            } else {
                panic!("{} did not resolve to Bound with the fixture context: {resolution:?}", schema.id);
            }
        }
    }

    #[test]
    fn app_quit_then_undo_reopens_the_app() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j.clone());
        exec.open_app("Mail");
        let ctx = ActionableMap::new(vec![ActionableElement::new("app-mail", ElementRole::App, "Mail")]);

        let instance = ActionInstance::new("app.quit", vec![SlotValue::AppRef("Mail".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        let confirmation = T2Confirmation::default();
        let authorized = authorize(&resolution, &confirmation, Duration::ZERO, None).expect("app.quit at base T1 must authorize with no confirmation needed");
        let result = exec.execute(&authorized);
        push_result(&j, result);

        assert!(!exec.state().borrow().open_apps.contains("Mail"));
        j.borrow_mut().undo().unwrap();
        assert!(exec.state().borrow().open_apps.contains("Mail"), "undo must reopen the quit app");
    }

    #[test]
    fn app_quit_with_unsaved_changes_escalates_to_t2_end_to_end() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("app-notes", ElementRole::App, "Notes").unsaved(true),
        ]);
        let instance = ActionInstance::new("app.quit", vec![SlotValue::AppRef("Notes".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        match resolution {
            Resolution::Bound { effective_tier, .. } => assert_eq!(effective_tier, Tier::T2),
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn window_maximize_then_undo_restores_prior_geometry() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j.clone());
        let ctx = ActionableMap::empty();
        let before = exec.state().borrow().focused_window;

        let instance = ActionInstance::new("win.maximize", vec![]);
        let resolution = exec.resolve(&instance, &ctx);
        let confirmation = T2Confirmation::default();
        let authorized = authorize(&resolution, &confirmation, Duration::ZERO, None).expect("win.maximize at T0 must authorize with no confirmation needed");
        let result = exec.execute(&authorized);
        push_result(&j, result);

        assert_ne!(exec.state().borrow().focused_window, before);
        j.borrow_mut().undo().unwrap();
        assert_eq!(exec.state().borrow().focused_window, before, "undo must restore the pre-maximize geometry snapshot");
    }

    #[test]
    fn secure_target_is_refused_regardless_of_family_tier() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        // ui.click is T1 (not even T2) -- secure refusal must still apply.
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("pw-field", ElementRole::Button, "Continue").secure(true),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Continue".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        assert_eq!(
            resolution,
            Resolution::Refused { instance: instance.clone(), reason: RefusalReason::SecureContext }
        );

        // Defense in depth: even if something hands `authorize` a
        // hand-crafted Bound resolution for that same secure target at the
        // most permissive possible tier (T0) with an affirmative
        // confirmation, it refuses it a second time -- derived from the
        // `Resolution`'s own `target.secure`, not a caller-supplied flag.
        let forged_bound = Resolution::Bound {
            instance,
            target: BoundTarget { element_id: Some("pw-field".into()), label: Some("Continue".into()), secure: true },
            effective_tier: Tier::T0,
        };
        let confirmation = T2Confirmation::default();
        let guarded = authorize(&forged_bound, &confirmation, Duration::ZERO, Some(UserResponse::Yes));
        assert_eq!(
            guarded.err(),
            Some(AuthorizeError::SecureContext),
            "a secure target must refuse even at T0 with an affirmative confirmation"
        );
    }

    #[test]
    fn secure_element_never_appears_in_disambiguation_candidates() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("send-secure", ElementRole::Button, "Send").secure(true),
            ActionableElement::new("send-1", ElementRole::Button, "Send"),
            ActionableElement::new("send-2", ElementRole::Button, "Send"),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Send".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        match resolution {
            Resolution::NeedsDisambiguation { candidates, .. } => {
                assert_eq!(candidates.len(), 2, "the secure Send must be excluded from the near-tie, leaving the two non-secure Sends");
                assert!(
                    !candidates.iter().any(|c| c.element_id == "send-secure"),
                    "a secure element's id must never appear in a HUD candidate list: {candidates:?}"
                );
                assert!(
                    candidates.iter().all(|c| c.element_id == "send-1" || c.element_id == "send-2"),
                    "only the non-secure Sends may surface: {candidates:?}"
                );
            }
            other => panic!("expected NeedsDisambiguation, got {other:?}"),
        }
    }

    #[test]
    fn tie_that_only_existed_because_of_a_secure_candidate_collapses_to_unique() {
        // Two "Send" buttons tie -- one secure, one not. Once the secure one
        // is excluded from contention, exactly one candidate remains and
        // must bind cleanly rather than surface a one-item "disambiguation".
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("send-secure", ElementRole::Button, "Send").secure(true),
            ActionableElement::new("send-1", ElementRole::Button, "Send"),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Send".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        match resolution {
            Resolution::Bound { target, .. } => {
                assert_eq!(target.element_id.as_deref(), Some("send-1"));
                assert!(!target.secure);
            }
            other => panic!("expected Bound to the sole non-secure candidate, got {other:?}"),
        }
    }

    #[test]
    fn near_tie_entirely_among_secure_candidates_is_a_secure_refusal() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("pw-1", ElementRole::Button, "Continue").secure(true),
            ActionableElement::new("pw-2", ElementRole::Button, "Continue").secure(true),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Continue".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        assert_eq!(
            resolution,
            Resolution::Refused { instance, reason: RefusalReason::SecureContext },
            "a near-tie entirely among secure candidates must stay a secure refusal, not leak a disambiguation list"
        );
    }

    #[test]
    fn picking_a_secure_candidate_id_is_refused_not_bound() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("pw-field", ElementRole::Button, "Continue").secure(true),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Continue".into())]);

        // Simulates a client that (incorrectly, or via a stale HUD list)
        // tries to pick a secure element's id -- the pick path must
        // re-verify independent of whatever produced that id.
        let resolution = exec.pick_candidate(&instance, "pw-field", &ctx);
        assert_eq!(resolution, Resolution::Refused { instance, reason: RefusalReason::SecureContext });
    }

    #[test]
    fn picking_a_non_secure_candidate_id_binds_normally() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("send-1", ElementRole::Button, "Send"),
            ActionableElement::new("send-2", ElementRole::Button, "Send"),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Send".into())]);
        let resolution = exec.pick_candidate(&instance, "send-2", &ctx);
        match resolution {
            Resolution::Bound { target, .. } => assert_eq!(target.element_id.as_deref(), Some("send-2")),
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_send_buttons_require_disambiguation_not_a_guess() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![
            ActionableElement::new("send-1", ElementRole::Button, "Send"),
            ActionableElement::new("send-2", ElementRole::Button, "Send"),
        ]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Send".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        match resolution {
            Resolution::NeedsDisambiguation { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("expected NeedsDisambiguation, got {other:?}"),
        }
    }

    #[test]
    fn destructive_labeled_click_escalates_and_gate_requires_confirmation() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        let ctx = ActionableMap::new(vec![ActionableElement::new("del-1", ElementRole::Button, "Delete")]);
        let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("Delete".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        let tier = resolution.effective_tier().expect("must resolve to Bound");
        assert_eq!(tier, Tier::T2, "clicking a Delete-labeled control must escalate to T2");

        let confirmation = T2Confirmation::default();
        let decision = decide(tier, &confirmation, Duration::from_secs(6), None);
        assert_eq!(decision, GateDecision::Denied, "unconfirmed T2 must default-deny on timeout even for an ordinary-looking click");
    }

    #[test]
    fn confirmed_bypass_labels_bind_at_t1_query_and_still_escalate_to_t2_end_to_end() {
        // The three inputs this unit's dispatch confirmed live against this
        // exact executor: U+200E LEFT-TO-RIGHT MARK, U+200F RIGHT-TO-LEFT
        // MARK, and multi-substitution leetspeak "d3l3t3" -- each binds to
        // the user's spoken "delete" at the real 0.5 similarity floor, and
        // each must now resolve to T2 (not silently execute at the
        // schema's base T1) end to end through the real
        // `MockDesktopExecutor`, not just the pure `is_destructive_label`
        // unit check.
        for label in ["De\u{200E}lete", "De\u{200F}lete", "d3l3t3"] {
            let j = journal();
            let exec = MockDesktopExecutor::new(j);
            let ctx = ActionableMap::new(vec![ActionableElement::new("del-1", ElementRole::Button, label)]);
            let instance = ActionInstance::new("ui.click", vec![SlotValue::ElementRef("delete".into())]);
            let resolution = exec.resolve(&instance, &ctx);
            match &resolution {
                Resolution::Bound { effective_tier, .. } => {
                    assert_eq!(*effective_tier, Tier::T2, "label {label:?} bound but did not escalate to T2");
                }
                other => panic!("expected the spoken query \"delete\" to bind label {label:?}, got {other:?}"),
            }

            let confirmation = T2Confirmation::default();
            let decision = decide(Tier::T2, &confirmation, Duration::from_secs(6), None);
            assert_eq!(
                decision,
                GateDecision::Denied,
                "label {label:?} must default-deny an unconfirmed destructive click, not silently execute"
            );
        }
    }

    #[test]
    fn shortcut_promotion_flows_through_resolve_to_t1() {
        let j = journal();
        let exec = MockDesktopExecutor::new(j);
        exec.promote_shortcut("Archive Inbox");
        let ctx = ActionableMap::new(vec![ActionableElement::new("sc-1", ElementRole::Shortcut, "Archive Inbox")]);
        let instance = ActionInstance::new("shortcut.run", vec![SlotValue::ShortcutName("Archive Inbox".into())]);
        let resolution = exec.resolve(&instance, &ctx);
        assert_eq!(resolution.effective_tier(), Some(Tier::T1));
    }
}
