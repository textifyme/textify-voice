//! `voice-intent` — the Command Mode intent pipeline: stage 1
//! (deterministic grammar) and the stage 2 trait boundary (constrained
//! local LLM parse). COMMANDS-SPEC §3.1, §3.3.
//!
//! ```text
//!  utterance ──► stage 1: grammar::match_utterance (<20 ms, table-driven)
//!                    │
//!                    ├─ Matched{Grammar}  ──────────────────────────► IntentResult
//!                    │
//!                    └─ Reject ──► (caller may retry via) stage2::ConstrainedParser
//!                                       │
//!                                       ├─ Emit{index in closed set} ─► IntentResult (LocalLlm)
//!                                       └─ Reject ────────────────────► IntentResult
//! ```
//!
//! Scope for this unit (see dispatch): pure, platform-independent logic
//! only. No cpal/ort/sherpa-onnx/AXUIElement/UIAutomation/mistral.rs/
//! Apple Foundation Models/SQLite — those are native/IO surfaces owned by
//! other crates. No dependency on `crates/voice-act`: this crate defines
//! its own minimal `ActionInstance`/`SlotValue` types (see [`types`]) so
//! the two crates stay decoupled; `voice-act` owns the live schema
//! registry, tier policy, and undo journal.

pub mod bias;
pub mod grammar;
pub mod stage2;
pub mod types;

pub use bias::{CommandBias, CommandBiasBuilder};
pub use grammar::{command_lexicon, match_utterance};
pub use stage2::{resolve as resolve_stage2, ConstrainedParser, Stage2Outcome, StubConstrainedParser};
pub use types::{
    ActionInstance, CommandContext, Direction, IntentResult, MatchStage, RejectReason, SlotValue,
};
