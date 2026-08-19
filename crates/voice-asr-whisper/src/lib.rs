//! `voice-asr-whisper` -- a real `voice_core::LocalAsr` backend, replacing
//! `voice_core::MockAsr` as the thing that actually recognises speech.
//!
//! Backed by whisper.cpp via the `whisper-rs` bindings. Batch decode on
//! `finalize()` (whisper.cpp has no incremental decode API) -- see
//! [`whisper_asr`]'s module doc for why that's the correct fit for
//! push-to-talk, not a shortcut. Also owns ggml model download/cache
//! management (see [`model`]).

pub mod model;
pub mod whisper_asr;

pub use model::{ModelError, ModelId, ModelManager, CACHE_DIR_ENV_VAR};
pub use whisper_asr::{
    ChunkingConfig, WhisperAsrConfig, WhisperAsrError, WhisperLocalAsr,
    DEFAULT_AUTO_CHUNK_THRESHOLD_SECONDS, DEFAULT_CHUNK_OVERLAP_SECONDS,
    DEFAULT_CHUNK_WINDOW_SECONDS,
};
