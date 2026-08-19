//! `voice-format` — local text formatting for Textify Voice.
//!
//! Owns the `Formatter` trait, the formatting-gate integration point, and
//! bias pipeline layer 3 (the constrained local LLM judge-editor, SPEC
//! §3.3). Real model backends (mistral.rs + Qwen3.5/Phi-4-mini, Apple
//! Foundation Models, Windows TextRewriter/Phi Silica) are out of scope for
//! this run — every trait here has a deterministic in-memory stub, and the
//! crate has zero native/network dependencies.
//!
//! Independent of `voice-context` / `voice-core` / `voice-intent` /
//! `voice-act` by design for this phase — see `types.rs` for the local
//! `FormatRequest`/`WritingStyle`-equivalent types this crate defines rather
//! than importing from a sibling crate or `packages/shared`.

pub mod formatter;
pub mod judge;
pub mod types;

pub use formatter::{Formatter, GateClosedReason, GateDecision, GatedFormatter, PassthroughFormatter};
pub use judge::{AmbiguousSpan, JudgeBackend, JudgeOutcome, JudgeProposal, resolve_ambiguous_span};
pub use types::{AppKind, FormatRequest, FormatResponse, WritingStyle};
