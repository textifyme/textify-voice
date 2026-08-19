//! `CommandBias` — static action lexicon + installed-app names +
//! focused-window element labels, emitted as bias terms for the ASR
//! decode-time hotword layer. COMMANDS-SPEC §3.1 ("Command biasing" row):
//! "commands are a near-closed vocabulary over verbs + on-screen nouns —
//! the best possible case for the hotword pipeline".
//!
//! Pure: callers supply app names and on-screen labels as plain data;
//! this module never enumerates the real system (no IO, no AX/UIA calls).
//! It mirrors the *shape* of `voice-core`'s `BiasContext.terms` (Voice
//! SPEC §3.3) without depending on `voice-core` — this crate stays
//! decoupled from its siblings per the unit that produced it.

use crate::grammar::command_lexicon;
use crate::types::CommandContext;

/// The bias terms to hand to the ASR layer's decode-time hotword pass for
/// the current command-mode utterance.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandBias {
    /// Deduplicated (case-insensitively) bias terms, in priority order:
    /// static action lexicon first, then installed-app names, then
    /// focused-window element labels.
    pub terms: Vec<String>,
}

/// Builds a [`CommandBias`] from local inputs only. COMMANDS-SPEC §3.1:
/// "CommandBias = static action lexicon + installed-app names +
/// focused-window AX labels, fed through bias layers 1–2".
#[derive(Debug, Clone, Default)]
pub struct CommandBiasBuilder {
    app_names: Vec<String>,
    element_labels: Vec<String>,
    shortcut_names: Vec<String>,
}

impl CommandBiasBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Installed/running application names known locally (e.g. read by
    /// the caller from `NSWorkspace`/shell enumeration elsewhere in the
    /// app) — this builder does not enumerate anything itself.
    #[must_use]
    pub fn with_app_names<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.app_names = names.into_iter().map(Into::into).collect();
        self
    }

    /// Labels read from the focused window's actionable-element map
    /// (`voice-context`, extended for command mode — memory-only, never
    /// persisted, never uploaded per COMMANDS-SPEC §3.1/§6).
    #[must_use]
    pub fn with_element_labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.element_labels = labels.into_iter().map(Into::into).collect();
        self
    }

    /// User Shortcut names known locally (`voice-context`/Shortcuts
    /// enumeration elsewhere in the app) — this builder does not
    /// enumerate anything itself. Not part of COMMANDS-SPEC §3.1's
    /// three-part `CommandBias` definition, but the same builder is the
    /// natural place to collect it since [`Self::context`] needs it
    /// alongside app names and element labels for stage-1 slot
    /// resolution (grammar module doc).
    #[must_use]
    pub fn with_shortcut_names<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.shortcut_names = names.into_iter().map(Into::into).collect();
        self
    }

    /// Merge the static action lexicon (derived from the stage-1 grammar
    /// table via [`command_lexicon`]) with app names, element labels, and
    /// shortcut names, deduplicated case-insensitively, in priority
    /// order. Pure — no IO.
    pub fn build(&self) -> CommandBias {
        let mut terms: Vec<String> = Vec::new();
        let mut seen_lower: Vec<String> = Vec::new();

        let push = |terms: &mut Vec<String>, seen: &mut Vec<String>, s: &str| {
            let key = s.to_lowercase();
            if !seen.contains(&key) {
                seen.push(key);
                terms.push(s.to_string());
            }
        };

        for w in command_lexicon() {
            push(&mut terms, &mut seen_lower, w);
        }
        for name in &self.app_names {
            push(&mut terms, &mut seen_lower, name);
        }
        for label in &self.element_labels {
            push(&mut terms, &mut seen_lower, label);
        }
        for name in &self.shortcut_names {
            push(&mut terms, &mut seen_lower, name);
        }

        CommandBias { terms }
    }

    /// Produce the typed [`CommandContext`] stage 1 needs for closed-set
    /// slot resolution (grammar module doc) from the same app/element/
    /// shortcut inputs [`Self::build`] flattens for ASR biasing. Kept
    /// distinct from [`CommandBias`] because resolution needs to know
    /// *which* set a candidate slot capture must match against — a
    /// flattened, deduplicated `terms: Vec<String>` throws that
    /// distinction away. Pure — no IO, just cloning this builder's
    /// inputs.
    pub fn context(&self) -> CommandContext {
        CommandContext {
            known_apps: self.app_names.clone(),
            known_elements: self.element_labels.clone(),
            known_shortcuts: self.shortcut_names.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_includes_static_lexicon_terms() {
        let bias = CommandBiasBuilder::new().build();
        assert!(bias.terms.contains(&"open".to_string()));
        assert!(bias.terms.contains(&"scroll".to_string()));
        assert!(bias.terms.contains(&"undo".to_string()));
    }

    #[test]
    fn build_includes_app_names_and_element_labels_in_priority_order() {
        let bias = CommandBiasBuilder::new()
            .with_app_names(["Slack", "Notion"])
            .with_element_labels(["Send", "Cancel"])
            .build();

        assert!(bias.terms.contains(&"Slack".to_string()));
        assert!(bias.terms.contains(&"Notion".to_string()));
        assert!(bias.terms.contains(&"Send".to_string()));
        assert!(bias.terms.contains(&"Cancel".to_string()));

        let lexicon_pos = bias.terms.iter().position(|t| t == "open");
        let app_pos = bias.terms.iter().position(|t| t == "Slack");
        let label_pos = bias.terms.iter().position(|t| t == "Send");
        match (lexicon_pos, app_pos, label_pos) {
            (Some(lexicon_pos), Some(app_pos), Some(label_pos)) => {
                assert!(lexicon_pos < app_pos, "static lexicon must come before app names");
                assert!(app_pos < label_pos, "app names must come before element labels");
            }
            other => panic!("expected all three terms present, got positions {other:?}"),
        }
    }

    #[test]
    fn build_deduplicates_case_insensitively_keeping_first_occurrence() {
        // "Open" (an app literally named that) collides with the static
        // lexicon's "open" verb — the lexicon entry wins since it is
        // merged first, and no duplicate is added.
        let bias = CommandBiasBuilder::new().with_app_names(["Open", "Slack"]).build();
        let occurrences = bias.terms.iter().filter(|t| t.eq_ignore_ascii_case("open")).count();
        assert_eq!(occurrences, 1, "case-insensitive duplicate must be collapsed");
        assert_eq!(bias.terms.iter().filter(|t| **t == "open").count(), 1);
        assert!(!bias.terms.contains(&"Open".to_string()), "the lexicon's lowercase form wins");
    }

    #[test]
    fn build_is_pure_same_inputs_same_output() {
        let a = CommandBiasBuilder::new().with_app_names(["Slack"]).with_element_labels(["Send"]).build();
        let b = CommandBiasBuilder::new().with_app_names(["Slack"]).with_element_labels(["Send"]).build();
        assert_eq!(a, b);
    }

    #[test]
    fn empty_builder_still_carries_the_static_lexicon() {
        let bias = CommandBiasBuilder::new().build();
        assert!(!bias.terms.is_empty());
    }

    #[test]
    fn build_includes_shortcut_names_after_element_labels() {
        let bias = CommandBiasBuilder::new()
            .with_app_names(["Slack"])
            .with_element_labels(["Send"])
            .with_shortcut_names(["Morning Routine"])
            .build();

        assert!(bias.terms.contains(&"Morning Routine".to_string()));
        let label_pos = bias.terms.iter().position(|t| t == "Send");
        let shortcut_pos = bias.terms.iter().position(|t| t == "Morning Routine");
        match (label_pos, shortcut_pos) {
            (Some(label_pos), Some(shortcut_pos)) => {
                assert!(label_pos < shortcut_pos, "element labels must come before shortcut names");
            }
            other => panic!("expected both terms present, got positions {other:?}"),
        }
    }

    #[test]
    fn context_carries_the_typed_resolution_sets_separately() {
        let ctx = CommandBiasBuilder::new()
            .with_app_names(["Slack", "Chrome"])
            .with_element_labels(["Send", "Cancel"])
            .with_shortcut_names(["Morning Routine"])
            .context();

        assert_eq!(ctx.known_apps, vec!["Slack".to_string(), "Chrome".to_string()]);
        assert_eq!(ctx.known_elements, vec!["Send".to_string(), "Cancel".to_string()]);
        assert_eq!(ctx.known_shortcuts, vec!["Morning Routine".to_string()]);
    }

    #[test]
    fn empty_builder_context_has_every_set_empty_fail_closed() {
        let ctx = CommandBiasBuilder::new().context();
        assert!(ctx.known_apps.is_empty());
        assert!(ctx.known_elements.is_empty());
        assert!(ctx.known_shortcuts.is_empty());
    }
}
