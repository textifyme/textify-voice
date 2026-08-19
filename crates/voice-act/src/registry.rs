//! The closed action registry. COMMANDS-SPEC.md §3.4: "The action surface
//! at launch (C1-C3, all local, all Free)." COMMANDS-SPEC.md §3.5 #1:
//! "Closed action set... the worst possible parse error is the *wrong
//! registered* action, never an *arbitrary* one." This module is that
//! closed set, statically declared.

use crate::schema::{ActionSchema, Invertibility, SlotKind, SlotSpec, Tier};

macro_rules! slots {
    ($($name:literal : $kind:ident),* $(,)?) => {
        &[$(SlotSpec { name: $name, kind: SlotKind::$kind }),*]
    };
}

const APP_REF: &[SlotSpec] = slots!("app": AppRef);
const ELEMENT_REF: &[SlotSpec] = slots!("target": ElementRef);
const DIRECTION: &[SlotSpec] = slots!("direction": Direction);
const NO_SLOTS: &[SlotSpec] = &[];
const SHORTCUT: &[SlotSpec] = slots!("name": ShortcutName);
const PERCENTAGE: &[SlotSpec] = slots!("amount": Percentage);
const ORDINAL: &[SlotSpec] = slots!("index": Ordinal);

/// The complete §3.4 launch action surface. Every family in the spec table
/// is represented with its specced base tier (escalations from that base
/// live in [`crate::escalation`], not here -- tier here is the schema's
/// declared property per §3.3's contract comment).
pub static LAUNCH_SCHEMAS: &[ActionSchema] = &[
    // --- App lifecycle: T1 (quit-with-unsaved -> T2 via escalation) ---
    ActionSchema { id: "app.open", tier: Tier::T1, slots: APP_REF, invertible: Invertibility::Full },
    ActionSchema { id: "app.switch", tier: Tier::T1, slots: APP_REF, invertible: Invertibility::Full },
    ActionSchema { id: "app.quit", tier: Tier::T1, slots: APP_REF, invertible: Invertibility::Full },
    ActionSchema { id: "app.hide", tier: Tier::T1, slots: APP_REF, invertible: Invertibility::Full },
    ActionSchema { id: "app.close_all_windows", tier: Tier::T1, slots: APP_REF, invertible: Invertibility::Snapshot },
    // --- Window management: T0 (geometry snapshot = full undo) ---
    ActionSchema { id: "win.tile_left", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    ActionSchema { id: "win.tile_right", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    ActionSchema { id: "win.maximize", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    ActionSchema { id: "win.next_display", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    ActionSchema { id: "win.minimize", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    // --- UI interaction: T1 (destructive-labeled controls -> T2) ---
    ActionSchema { id: "ui.click", tier: Tier::T1, slots: ELEMENT_REF, invertible: Invertibility::None },
    ActionSchema { id: "ui.show_numbers", tier: Tier::T1, slots: NO_SLOTS, invertible: Invertibility::None },
    ActionSchema { id: "ui.focus_field", tier: Tier::T1, slots: ELEMENT_REF, invertible: Invertibility::Full },
    ActionSchema { id: "ui.toggle_checkbox", tier: Tier::T1, slots: ELEMENT_REF, invertible: Invertibility::Full },
    // --- Scroll/navigate: T0 ---
    ActionSchema { id: "nav.scroll", tier: Tier::T0, slots: DIRECTION, invertible: Invertibility::Full },
    ActionSchema { id: "nav.scroll_to_top", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "nav.next_tab", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "nav.back", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    // --- System: T0 ---
    ActionSchema { id: "sys.volume", tier: Tier::T0, slots: PERCENTAGE, invertible: Invertibility::Full },
    ActionSchema { id: "sys.brightness", tier: Tier::T0, slots: PERCENTAGE, invertible: Invertibility::Full },
    ActionSchema { id: "sys.mute", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "sys.media_play_pause", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "sys.dnd", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "sys.screenshot", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::None },
    // --- Text edit (on selection/focus): T1, clipboard-snapshotted ---
    ActionSchema { id: "text.select_sentence", tier: Tier::T1, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "text.select_paragraph", tier: Tier::T1, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "text.delete_selection", tier: Tier::T1, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    ActionSchema { id: "text.rewrite_style", tier: Tier::T1, slots: NO_SLOTS, invertible: Invertibility::Snapshot },
    // --- Shortcuts bridge: T2 by default, promotable to T1 per-shortcut ---
    ActionSchema { id: "shortcut.run", tier: Tier::T2, slots: SHORTCUT, invertible: Invertibility::None },
    // --- Meta: T0 ---
    ActionSchema { id: "meta.undo", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::Full },
    ActionSchema { id: "meta.help", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::None },
    ActionSchema { id: "meta.stop", tier: Tier::T0, slots: NO_SLOTS, invertible: Invertibility::None },
];

// ui.show_numbers references ORDINAL indirectly (the numbered overlay
// itself uses Ordinal slots on the *follow-up* click, not this action);
// keep the constant referenced so it isn't a dead-code warning if unused
// elsewhere, and documented as reserved for that follow-up schema.
#[allow(dead_code)]
const _ORDINAL_RESERVED: &[SlotSpec] = ORDINAL;

/// Read-only view over the closed action set.
pub struct ActionRegistry;

impl ActionRegistry {
    pub fn all() -> &'static [ActionSchema] {
        LAUNCH_SCHEMAS
    }

    pub fn by_id(id: &str) -> Option<&'static ActionSchema> {
        LAUNCH_SCHEMAS.iter().find(|s| s.id == id)
    }

    pub fn contains(id: &str) -> bool {
        Self::by_id(id).is_some()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// COMMANDS-SPEC.md §3.4 table, restated as data so a change to either
    /// the registry or this list fails the build. Tuples are
    /// (id, expected *declared* tier -- escalated *effective* tiers are
    /// covered in `crate::escalation`'s tests, not here).
    const EXPECTED_TIERS: &[(&str, Tier)] = &[
        ("app.open", Tier::T1),
        ("app.switch", Tier::T1),
        ("app.quit", Tier::T1),
        ("app.hide", Tier::T1),
        ("app.close_all_windows", Tier::T1),
        ("win.tile_left", Tier::T0),
        ("win.tile_right", Tier::T0),
        ("win.maximize", Tier::T0),
        ("win.next_display", Tier::T0),
        ("win.minimize", Tier::T0),
        ("ui.click", Tier::T1),
        ("ui.show_numbers", Tier::T1),
        ("ui.focus_field", Tier::T1),
        ("ui.toggle_checkbox", Tier::T1),
        ("nav.scroll", Tier::T0),
        ("nav.scroll_to_top", Tier::T0),
        ("nav.next_tab", Tier::T0),
        ("nav.back", Tier::T0),
        ("sys.volume", Tier::T0),
        ("sys.brightness", Tier::T0),
        ("sys.mute", Tier::T0),
        ("sys.media_play_pause", Tier::T0),
        ("sys.dnd", Tier::T0),
        ("sys.screenshot", Tier::T0),
        ("text.select_sentence", Tier::T1),
        ("text.select_paragraph", Tier::T1),
        ("text.delete_selection", Tier::T1),
        ("text.rewrite_style", Tier::T1),
        ("shortcut.run", Tier::T2),
        ("meta.undo", Tier::T0),
        ("meta.help", Tier::T0),
        ("meta.stop", Tier::T0),
    ];

    #[test]
    fn every_expected_schema_is_registered_at_the_specced_tier() {
        for (id, expected_tier) in EXPECTED_TIERS {
            let schema = ActionRegistry::by_id(id).unwrap_or_else(|| panic!("missing schema: {id}"));
            assert_eq!(schema.tier, *expected_tier, "{id} has wrong declared tier");
        }
    }

    #[test]
    fn registry_has_no_undeclared_extra_schemas() {
        assert_eq!(
            LAUNCH_SCHEMAS.len(),
            EXPECTED_TIERS.len(),
            "registry size drifted from the §3.4 table -- update both together"
        );
    }

    #[test]
    fn schema_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for schema in LAUNCH_SCHEMAS {
            assert!(seen.insert(schema.id), "duplicate schema id: {}", schema.id);
        }
    }

    #[test]
    fn no_t3_schema_is_ever_registered() {
        // T3 means "never" (§3.5 #2): excluded-list actions and secure
        // contexts. There must be no registered schema declared T3 at all
        // -- T3 is reachable only via the secure-context refusal path, not
        // as a schema someone could otherwise execute.
        for schema in LAUNCH_SCHEMAS {
            assert_ne!(schema.tier, Tier::T3, "{} must not be a registered T3 schema", schema.id);
        }
    }

    /// COMMANDS-SPEC.md §3.4: "Explicitly not in the registry: file
    /// deletion, sending email/messages composed by the system, payments,
    /// credential entry, shell/terminal execution, browser autonomous
    /// navigation." Encoded as banned id *prefixes* (not raw substrings --
    /// `text.delete_selection` is a legitimate, specced T1 action and must
    /// not false-positive against a bare "delete" scan).
    const BANNED_ID_PREFIXES: &[&str] = &[
        "file.delete",
        "file.remove",
        "email.send",
        "message.send",
        "mail.send",
        "payment.",
        "pay.",
        "credential.",
        "password.",
        "shell.",
        "terminal.",
        "exec.",
        "browser.navigate",
    ];

    #[test]
    fn excluded_action_families_have_no_registered_schema() {
        for schema in LAUNCH_SCHEMAS {
            for banned in BANNED_ID_PREFIXES {
                assert!(
                    !schema.id.starts_with(banned),
                    "{} matches excluded-from-registry family prefix {banned:?} (§3.4)",
                    schema.id
                );
            }
        }
    }

    #[test]
    fn specific_excluded_ids_are_absent() {
        for banned_id in [
            "file.delete",
            "email.send",
            "message.send",
            "payment.charge",
            "credential.enter",
            "shell.exec",
            "terminal.run",
            "browser.navigate_autonomous",
        ] {
            assert!(!ActionRegistry::contains(banned_id), "excluded action {banned_id} must never be registered");
        }
    }

    #[test]
    fn shortcut_run_is_the_only_t2_default_schema() {
        // Everything else in the table is T0/T1 at declaration time;
        // consequential T2 behavior for other families is reached only via
        // escalation (crate::escalation), never declared as a base tier.
        let t2_ids: Vec<&str> = LAUNCH_SCHEMAS.iter().filter(|s| s.tier == Tier::T2).map(|s| s.id).collect();
        assert_eq!(t2_ids, vec!["shortcut.run"]);
    }
}
