//! `voice-core` — Textify Voice hot path (SPEC.md §3.2).
//!
//! This crate holds only the parts of the dictation hot path that are pure,
//! platform-independent logic: the [`LocalAsr`](asr::LocalAsr) contract and a
//! deterministic mock implementation, the per-utterance PCM ring buffer, the
//! endpointing state machine, the three-layer bias pipeline's deterministic
//! layer (Double Metaphone + edit distance), the text normalizer, the local
//! formatting gate heuristic, and the text-insertion policy.
//!
//! Every native/IO surface (audio capture, VAD model inference, ASR runtime,
//! accessibility trees, clipboard, on-device LLMs) is a trait with a
//! deterministic in-memory stub here. Real backends land in later work
//! packages and are out of scope for this crate.

pub mod asr;
pub mod bias;
pub mod edit_distance;
pub mod endpoint;
pub mod format_gate;
pub mod insertion;
pub mod metaphone;
pub mod normalizer;
pub mod ring_buffer;

pub use asr::{
    AppKind, AsrCaps, AsrError, BiasContext, BiasContextTracker, BiasTerm, LocalAsr,
    LocalTranscript, MockAsr, PartialCallback, PartialResult, WordConfidence,
};
pub use bias::{correct_spans, CorrectionThresholds, WordSpan};
pub use edit_distance::damerau_levenshtein;
pub use endpoint::{EndpointEvent, EndpointMode, Endpointer, EnergyVad, Vad};
pub use format_gate::format_gate_open;
pub use insertion::{InsertionBackend, InsertionError, InsertionMethod, InsertionTarget};
pub use metaphone::{double_metaphone, MetaphoneCode};
pub use normalizer::{default_literal_rules, normalize, LiteralRule, NormalizeResult};
pub use ring_buffer::PcmRingBuffer;
