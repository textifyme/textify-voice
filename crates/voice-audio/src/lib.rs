//! Textify Voice audio I/O: the native/IO backend that feeds
//! `voice_core::PcmRingBuffer` real 16 kHz mono `i16` PCM.
//!
//! Two entry points, split by what's verifiable in an automated run:
//!
//! - [`decode`]: WAV file decode (resample + downmix to 16 kHz mono `i16`).
//!   Fully deterministic, fully tested against the real fixture audio in
//!   `fixtures/audio/` — see `decode::tests` and the `decode_report` bin.
//! - [`capture`]: cpal-backed live microphone capture, pre-warmed and
//!   paused so `start()` is just `Stream::play()`. Requires real hardware
//!   and macOS Microphone TCC permission neither of which an automated
//!   agent run has access to; its non-hardware logic (permission gating,
//!   the frame-arrival watchdog, resample wiring) is unit tested, but the
//!   actual `cpal::Stream` data path is not exercised in this run.
//!
//! [`permission`] detects macOS microphone TCC state so callers fail fast
//! with an actionable "open System Settings" message instead of hanging.
//! [`vad_pipeline`] wires `voice_core::Endpointer` to a plain energy VAD
//! (not silero) for toggle-mode endpointing.

pub mod capture;
pub mod decode;
pub mod permission;
pub mod resample;
pub mod vad_pipeline;

pub use capture::{wait_for_first_frame, AudioSource, CaptureError, MicCapture};
pub use decode::{compute_stats, decode_wav_file, downmix_to_mono, f32_to_i16, AudioStats, DecodeError};
pub use permission::{microphone_permission_status, MicPermission, request_microphone_access};
pub use resample::{resample_to_16k, StreamingResampler};
pub use vad_pipeline::ToggleCapturePipeline;
