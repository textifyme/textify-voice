//! Local formatting gate (SPEC.md §3.4 step 2): "a heuristic gate decides
//! if an LLM pass would change the output — **forced off in AI/coding apps**
//! (paste raw, matching Wispr's confirmed behavior)."
//!
//! This module decides only *whether* the (native, out-of-crate) local
//! formatter may run — never what it does. The app-kind override is
//! absolute and evaluated first, ahead of any text heuristic or user
//! setting, because it's a privacy/product behavior, not an optimization.

use crate::AppKind;

/// SPEC §3.4 step 2's gate. `user_enabled` models the user-facing formatter
/// toggle (settings UI, out of scope for this crate); `raw_text` is the
/// normalizer's output text.
#[must_use]
pub fn format_gate_open(app_kind: AppKind, raw_text: &str, user_enabled: bool) -> bool {
    if app_kind.is_ai_or_coding() {
        return false; // SPEC: forced off, no exceptions — not even max_accuracy.
    }
    if !user_enabled {
        return false;
    }
    needs_formatting(raw_text)
}

/// Would an LLM formatting pass plausibly change `text`? Text that's
/// already well-formed (capitalized start, terminal punctuation, no long
/// unpunctuated run) gets no benefit from a formatting pass, so the gate
/// stays closed to save the latency (§3.4: "≤500ms p50 ... when the gate
/// opens") and cost.
fn needs_formatting(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    let starts_lowercase = trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() && c.is_lowercase());
    if starts_lowercase {
        return true;
    }

    let ends_with_terminal_punct = matches!(trimmed.chars().last(), Some('.' | '!' | '?'));
    if !ends_with_terminal_punct {
        return true;
    }

    // By this point the text is known to end in ./!/? (checked above), so
    // any comma/semicolon/colon found here is necessarily internal, not
    // trailing — no separate position check needed.
    let word_count = trimmed.split_whitespace().count();
    let has_internal_punct = trimmed.chars().any(|c| matches!(c, ',' | ';' | ':'));
    if word_count > 12 && !has_internal_punct {
        return true; // long unbroken run — likely a run-on that needs sentence breaks
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_off_for_code_editor_even_when_text_obviously_needs_formatting() {
        assert!(!format_gate_open(
            AppKind::Code,
            "this needs formatting badly and has no punctuation at all really",
            true
        ));
    }

    #[test]
    fn forced_off_for_terminal_and_ai_chat() {
        assert!(!format_gate_open(
            AppKind::Terminal,
            "lowercase no punctuation",
            true
        ));
        assert!(!format_gate_open(
            AppKind::Ai,
            "lowercase no punctuation",
            true
        ));
    }

    #[test]
    fn forced_off_even_when_user_enabled_and_needs_formatting() {
        // The AI/coding override must win regardless of the user toggle.
        assert!(!format_gate_open(
            AppKind::Code,
            "lowercase text",
            true
        ));
    }

    #[test]
    fn closed_when_user_disabled_even_in_general_apps() {
        assert!(!format_gate_open(AppKind::General, "lowercase text", false));
    }

    #[test]
    fn open_for_general_app_when_text_needs_formatting_and_user_enabled() {
        assert!(format_gate_open(
            AppKind::General,
            "hello there how are you",
            true
        ));
    }

    #[test]
    fn closed_for_already_well_formed_text() {
        assert!(!format_gate_open(
            AppKind::General,
            "Hello there, how are you today?",
            true
        ));
    }

    #[test]
    fn empty_text_never_opens_the_gate() {
        assert!(!format_gate_open(AppKind::General, "", true));
        assert!(!format_gate_open(AppKind::General, "   ", true));
    }
}
