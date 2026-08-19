//! Core value types for local text formatting.
//!
//! Defined locally per task scope — `voice-format` does not depend on
//! `voice-context` or `packages/shared`'s `FormatRequest`/`WritingStyle`;
//! cross-crate/cross-language wiring is deferred. Field shapes mirror SPEC
//! §3.3's `FormatRequest` (`text`, `style?`, `app_kind?`) so a later mapping
//! layer is mechanical.

/// Requested writing style for a formatting pass.
/// SPEC §3.3 `FormatRequest.style?: WritingStyle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WritingStyle {
    /// No stylistic change beyond the deterministic normalizer.
    Plain,
    Professional,
    Casual,
    /// A named custom style (Pro cloud quality tier, SPEC §3.4) — carried
    /// here as an opaque label; the local baseline formatter may ignore it.
    Custom(String),
}

/// Coarse application category, used to force the formatting gate off in
/// AI/coding apps (SPEC §3.4). Defined locally — see module doc.
///
/// RECONCILIATION (integration pass): matches `crates/voice-context::types::
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

/// SPEC §3.3: `FormatRequest { text: string; style?: WritingStyle; app_kind?: AppKind }`.
/// "text only — audio never rides a formatting call."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatRequest {
    pub text: String,
    pub style: Option<WritingStyle>,
    pub app_kind: Option<AppKind>,
}

/// SPEC §3.3: formatting response is `{ formatted_text }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatResponse {
    pub formatted_text: String,
}
