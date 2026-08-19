//! Shared CLI value types used by more than one subcommand.

use clap::ValueEnum;
use voice_asr_whisper::ModelId;
use voice_core::AppKind;

/// `--model` choice, shared by `transcribe`, `dictate`, and `models`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ModelArg {
    #[value(name = "tiny.en")]
    TinyEn,
    #[value(name = "base.en")]
    BaseEn,
}

impl ModelArg {
    #[must_use]
    pub fn to_model_id(self) -> ModelId {
        match self {
            ModelArg::TinyEn => ModelId::TinyEn,
            ModelArg::BaseEn => ModelId::BaseEn,
        }
    }
}

/// `--app-kind` choice for `transcribe`. Named to match SPEC.md's app-kind
/// vocabulary rather than `voice_core::AppKind`'s full variant set (which
/// also has `General`/`Browser`/`Messaging`/`Email`/`Unknown` — irrelevant
/// distinctions for a one-shot file transcription where the caller just
/// wants "is this AI/coding (raw paste) or prose (full normalize)").
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AppKindArg {
    Code,
    Ai,
    Terminal,
    Prose,
}

impl AppKindArg {
    #[must_use]
    pub fn to_voice_core(self) -> AppKind {
        match self {
            AppKindArg::Code => AppKind::Code,
            AppKindArg::Ai => AppKind::Ai,
            AppKindArg::Terminal => AppKind::Terminal,
            AppKindArg::Prose => AppKind::General,
        }
    }
}

/// Map `voice-context`'s coarse, wire-shaped `AppKind` (frontmost-app
/// classification from a live AX/NSWorkspace read) onto `voice-core`'s
/// richer, on-device `AppKind` (what `BiasContext` and the normalizer/
/// format-gate actually consume).
///
/// `None` (no frontmost app resolved yet -- e.g. before the first context
/// capture has completed, or `voice-context` degraded to
/// `Coverage::Unavailable`) maps to `AppKind::General`: the same safe,
/// fully-normalized default `dictate.rs` used before this wiring existed,
/// so an unresolved context never surprises a user who was previously
/// getting full normalization.
///
/// `Chat` -> `Messaging` and `Document`/`Other` -> `General` are the two
/// lossy corners: `voice-core::AppKind` has no `Chat`/`Document`/`Other`
/// variants of its own (see that type's doc comment), so these are the
/// closest honest fit rather than an exact round-trip.
#[must_use]
pub fn context_app_kind_to_core(kind: Option<voice_context::AppKind>) -> AppKind {
    match kind {
        None => AppKind::General,
        Some(voice_context::AppKind::Ai) => AppKind::Ai,
        Some(voice_context::AppKind::Code) => AppKind::Code,
        Some(voice_context::AppKind::Terminal) => AppKind::Terminal,
        Some(voice_context::AppKind::Browser) => AppKind::Browser,
        Some(voice_context::AppKind::Chat) => AppKind::Messaging,
        Some(voice_context::AppKind::Document) => AppKind::General,
        Some(voice_context::AppKind::Other) => AppKind::General,
    }
}

/// Parse a `--bias-terms a,b,c` value into a clean list: split on commas,
/// trim whitespace, drop empty tokens (so a trailing comma or accidental
/// double-comma doesn't produce a spurious empty `BiasTerm`).
#[must_use]
pub fn split_bias_terms(raw: &[String]) -> Vec<String> {
    raw.iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_bias_terms_trims_and_drops_empties() {
        let raw = vec!["Slack, Chrome ,, Notion".to_string()];
        assert_eq!(split_bias_terms(&raw), vec!["Slack", "Chrome", "Notion"]);
    }

    #[test]
    fn split_bias_terms_of_empty_input_is_empty() {
        let raw: Vec<String> = vec![];
        assert!(split_bias_terms(&raw).is_empty());
    }

    #[test]
    fn model_arg_maps_to_expected_model_ids() {
        assert_eq!(ModelArg::TinyEn.to_model_id(), voice_asr_whisper::ModelId::TinyEn);
        assert_eq!(ModelArg::BaseEn.to_model_id(), voice_asr_whisper::ModelId::BaseEn);
    }

    #[test]
    fn app_kind_arg_maps_ai_coding_kinds_and_prose_to_general() {
        assert_eq!(AppKindArg::Code.to_voice_core(), AppKind::Code);
        assert_eq!(AppKindArg::Ai.to_voice_core(), AppKind::Ai);
        assert_eq!(AppKindArg::Terminal.to_voice_core(), AppKind::Terminal);
        assert_eq!(AppKindArg::Prose.to_voice_core(), AppKind::General);
    }

    #[test]
    fn context_app_kind_maps_ai_coding_and_browser_directly() {
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Ai)), AppKind::Ai);
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Code)), AppKind::Code);
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Terminal)), AppKind::Terminal);
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Browser)), AppKind::Browser);
    }

    #[test]
    fn context_app_kind_maps_chat_to_messaging() {
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Chat)), AppKind::Messaging);
    }

    #[test]
    fn context_app_kind_maps_document_other_and_none_to_general() {
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Document)), AppKind::General);
        assert_eq!(context_app_kind_to_core(Some(voice_context::AppKind::Other)), AppKind::General);
        assert_eq!(context_app_kind_to_core(None), AppKind::General);
    }

    #[test]
    fn context_app_kind_mapping_preserves_is_ai_or_coding_for_the_three_raw_paste_kinds() {
        // SPEC V1.4's raw-paste rule depends on `AppKind::is_ai_or_coding()`
        // staying true for exactly Code/Terminal/Ai after this mapping --
        // pin it so a future edit to either enum can't silently break the
        // raw-paste acceptance case.
        for k in [voice_context::AppKind::Ai, voice_context::AppKind::Code, voice_context::AppKind::Terminal] {
            assert!(context_app_kind_to_core(Some(k)).is_ai_or_coding());
        }
        for k in [voice_context::AppKind::Browser, voice_context::AppKind::Chat, voice_context::AppKind::Document, voice_context::AppKind::Other]
        {
            assert!(!context_app_kind_to_core(Some(k)).is_ai_or_coding());
        }
    }
}
