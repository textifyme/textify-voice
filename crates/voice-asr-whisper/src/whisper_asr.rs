//! [`WhisperLocalAsr`]: a real `voice_core::LocalAsr` implementation backed
//! by whisper.cpp (via the `whisper-rs` bindings).
//!
//! whisper.cpp's `whisper_full()` is a batch call: give it the whole clip,
//! get back segments. There is no incremental/streaming decode API in
//! whisper-rs to hook `on_partial` up to cheaply, so this backend
//! accumulates fed PCM in a ring buffer and runs the batch decode once, in
//! `finalize()`. That is the correct fit for the MVP's push-to-talk mode:
//! SPEC.md 3.1 makes key-up (== the caller's `finalize()` call) *the*
//! endpoint, so "decode everything on finalize" is not a compromise here,
//! it's the natural shape of the push-to-talk contract. `AsrCaps::streaming`
//! is `false` and is the single source of truth for that -- callers must not
//! expect `on_partial` to ever fire on this backend (it stores the callback
//! to satisfy the trait, but never invokes it: inventing a "partial" out of
//! a batch decoder would be exactly the faked streaming this run's brief
//! rules out).
//!
//! ## Long-form chunking is ON by default above a measured length threshold
//!
//! `finalize()` decides for itself, per call, whether the buffered audio is
//! long enough to need windowed decoding (see [`ChunkingConfig`]) -- this is
//! not a caller-visible mode switch, it is `finalize()`'s own default
//! behavior. Below `ChunkingConfig::threshold_seconds` it runs the exact
//! same single-shot decode this backend always has; at or above it,
//! `finalize()` transparently switches to the same windowed decode-and-
//! stitch [`WhisperLocalAsr::transcribe_long_form`] exposes explicitly (both
//! now share one implementation, `decode_chunked`).
//!
//! Why default it on at all, and why 60s: whisper.cpp's own long-form
//! segment-seek silently *drops* large contiguous spans of words on a
//! single multi-minute `whisper_full()` call (not a guess -- reproduced
//! repeatedly on `fixtures/audio/ref-3min.wav`, see the WER numbers below).
//! A length-swept measurement against that fixture (cutting prefixes with
//! `ffmpeg -t N` at 10s, 15s, 20s, ..., 121s and scoring each with
//! `fixtures/voice/wer.ts`'s `computeWer`) found:
//! - 10s-90s: single-shot and chunked decode are statistically identical
//!   (same WER, same word count, at every length sampled) -- chunking buys
//!   nothing here, it would just be 2 whisper.cpp calls instead of 1.
//! - 90s: single-shot is still clean (WER 0.0032, 1 substitution, 0
//!   deletions); chunked is *worse* here (WER 0.058, 10 deletions from
//!   overlap-dedup noise) -- chunking below its useful range is a pure
//!   latency-and-noise cost, confirming the brief's warning not to chunk
//!   short/PTT-shaped audio.
//! - 100s: single-shot still clean (WER 0.041). 105s: single-shot suddenly
//!   jumps to WER 0.138 (44 deletions, 0 insertions -- the drop, not
//!   mis-hearing). 110s: WER 0.177 (61 deletions). 121s (the full fixture):
//!   WER 0.2488 (101 deletions / 0 insertions against 418 reference words).
//!   Chunked stays flat and safe across this entire range (WER 0.052-0.073,
//!   3-9 deletions, 7-13 insertions from overlap-dedup noise -- never the
//!   large one-sided deletion spike single-shot shows).
//!
//! So the real failure onset on this fixture is ~100-105s, and chunking is
//! quality-neutral (not quality-negative) everywhere below ~90s. 60s is
//! picked with real margin under that onset (this is one fixture's break
//! point, not a proven constant -- content-dependent segment-seek failures
//! are exactly the kind of thing that could trigger a little earlier on
//! different audio) while staying well clear of every length actually
//! measured as chunking-neutral. It also matches the pre-existing 60s
//! `LONG_FORM_WARN_THRESHOLD_SECONDS` `voice-cli`'s `transcribe.rs` already
//! warns at, so the two independent judgment calls agree on where "this
//! isn't push-to-talk-shaped audio anymore" starts. Turn auto-chunking off
//! entirely with `ChunkingConfig::disabled()` / `chunking.enabled = false`.
//!
//! Added latency of chunking on a short clip, measured on
//! `fixtures/audio/short-5s.wav` (3.88s decoded) with a warm model/GPU
//! (3 runs each, `chunk_seconds=10.0` so the whole clip is one window):
//! single-shot 152.0-161.5 ms, chunked (via `transcribe_long_form`)
//! 142.2-165.3 ms -- no measurable difference, because a clip shorter than
//! one window *is* one window either way. This is exactly why the default
//! path below `threshold_seconds` bypasses chunking's window-loop machinery
//! entirely rather than routing every call through it with a large window:
//! short-5s.wav never reaches the loop at all, so it cannot regress.
//!
//! ## No prompt-conditioning: `BiasContext` never reaches whisper's decode
//!
//! An earlier version of this backend turned `BiasContext` terms into a
//! whisper.cpp `initial_prompt` on every decode (a prose-formatted sentence,
//! e.g. `"Some words and names that may come up: Kubernetes."`, per SPEC.md
//! 3.3's guidance that bias terms should read as natural prose, not a bare
//! list). This was a mistake caught by an adversarial audit, and it has been
//! removed rather than fixed, for two reasons:
//!
//! **Reason one: it was never what the spec called for.** SPEC.md 3.3
//! describes layer 1 (decode-time hotwords/biasing) as "transducer engines
//! only" and says whisper-class models "rely on layers 2-3" (deterministic
//! phonetic post-correction, then the constrained LLM judge-editor). The
//! prose-prompt guidance the removed code cited is about the *cloud*
//! escalation path (Groq `prompt` text / AssemblyAI keyterms), not local
//! whisper.cpp. Prompt-conditioning whisper was this backend inventing its
//! own layer-1 substitute for an engine the architecture never asked to
//! have one.
//!
//! **Reason two: it measurably destroyed decode quality**, on the exact
//! shape of input that ships as this app's DEFAULT (the seeded user
//! dictionary has exactly one term, "Kubernetes"; `--no-dictionary` was the
//! only thing masking this in every prior WER run). On
//! `fixtures/audio/ref-3min.wav` (418-word reference), via the real CLI
//! release binary with the default dictionary present: with the
//! dictionary's one term prompt-conditioning every window, 298 words, WER
//! 0.3947 (9 substitutions / 138 deletions / 18 insertions); the identical
//! run with `--no-dictionary`, 419 words, WER 0.0526 (3 substitutions / 9
//! deletions / 10 insertions). Isolated directly against `WhisperLocalAsr`
//! (bypassing the CLI, dictionary loader, and normalizer entirely -- see
//! `examples/transcribe_fixture.rs`'s optional bias-terms argument): a
//! `BiasContext` with the single term `"Kubernetes"` reproduces the same
//! collapse (298 words, with whole windows degenerating into repeated
//! fragments and hallucinated quote marks) that an empty `BiasContext` does
//! not (419 words, clean). The prose formatting already in place at the
//! time of the audit did **not** avoid this -- it is not a "bare list"
//! formatting bug, `initial_prompt` itself is the wrong mechanism for this
//! engine. That rules out keeping it behind a default-off, prose-formatted
//! opt-in: there is nothing here proven safe to opt into.
//!
//! `BiasContext` is still fully honored -- it drives bias layer 2 (Double
//! Metaphone post-correction; see `voice_core::normalize`, which every
//! caller of this backend already runs against transcript output) and
//! layer 3, both engine-agnostic and both unaffected by this change. Only
//! the whisper-specific decode-time prompt hack is gone. `WhisperAsrConfig`
//! has no bias-related fields and the `LocalAsr::start_utterance`/
//! `update_bias` hooks are now no-ops on this backend (kept to satisfy the
//! trait) -- there is no flag to re-enable prompt-conditioning because none
//! of this was ever shown to be safe.

use std::path::PathBuf;

use voice_core::{
    AsrCaps, AsrError, BiasContext, LocalAsr, LocalTranscript, PartialCallback, PcmRingBuffer,
    WordConfidence,
};
use whisper_rs::{
    convert_integer_to_float_audio, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters, WhisperError as WhisperRsError,
};

/// Construction-time failure (model load). Distinct from [`voice_core::AsrError`],
/// which only covers the `LocalAsr` trait's per-utterance failure surface --
/// there is no trait hook for "the engine itself couldn't be built."
#[derive(Debug)]
pub enum WhisperAsrError {
    ModelLoad {
        path: PathBuf,
        source: WhisperRsError,
    },
    /// whisper-rs 0.15.x's `WhisperContext::new_with_params` takes `&str`,
    /// not an OS path -- this crate's `model_path` config field is a
    /// `PathBuf` (matching every other path in this workspace), so a path
    /// that is not valid UTF-8 has to be rejected here rather than silently
    /// lossy-converted into a path that might not exist.
    NonUtf8ModelPath(PathBuf),
}

impl std::fmt::Display for WhisperAsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WhisperAsrError::ModelLoad { path, source } => write!(
                f,
                "failed to load whisper model at {}: {source:?}",
                path.display()
            ),
            WhisperAsrError::NonUtf8ModelPath(path) => write!(
                f,
                "model path is not valid UTF-8 (required by whisper-rs 0.15's new_with_params): {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for WhisperAsrError {}

/// Length, in seconds of buffered PCM, at or above which `finalize()`
/// switches to windowed decode-and-stitch by default. See this module's
/// doc comment for the length-swept measurement that picked 60.0 (real
/// margin under the ~100-105s failure onset measured on
/// `fixtures/audio/ref-3min.wav`, while every length actually measured as
/// chunking-neutral -- 10-90s -- stays well clear of it).
pub const DEFAULT_AUTO_CHUNK_THRESHOLD_SECONDS: f64 = 60.0;

/// Default window size for chunked decode. See
/// [`WhisperLocalAsr::transcribe_long_form`]'s doc comment for the
/// window-size sweep (30s/3s down to 8s/1.5s) that picked 10s/1.5s as the
/// point where whisper.cpp's own long-form deletions are already mostly
/// gone without overlap-dedup insertions climbing back up.
pub const DEFAULT_CHUNK_WINDOW_SECONDS: f64 = 10.0;
/// Default overlap between consecutive chunk windows. See
/// [`DEFAULT_CHUNK_WINDOW_SECONDS`].
pub const DEFAULT_CHUNK_OVERLAP_SECONDS: f64 = 1.5;

/// Controls whether/how `finalize()` auto-chunks long buffered audio. See
/// this module's doc comment for the measurement behind the defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChunkingConfig {
    /// `true` (the default): `finalize()` switches to windowed
    /// decode-and-stitch once buffered audio reaches `threshold_seconds`.
    /// `false`: `finalize()` always runs the single-shot decode, no matter
    /// how long the buffered audio is -- this is the escape hatch this
    /// unit's brief requires ("a way to turn it off").
    pub enabled: bool,
    /// Buffered-audio length, in seconds, at or above which `finalize()`
    /// chunks. Only consulted when `enabled` is `true`.
    pub threshold_seconds: f64,
    /// Window length fed to `decode_chunked` when chunking triggers.
    pub window_seconds: f64,
    /// Overlap between consecutive windows, for `stitch_words` to
    /// de-duplicate against.
    pub overlap_seconds: f64,
}

impl ChunkingConfig {
    /// Auto-chunking on, at the measured default threshold and window
    /// sizing. This is also what `Default` gives you -- named explicitly so
    /// call sites that want to be loud about the choice can write
    /// `ChunkingConfig::enabled_default()` instead of relying on `Default`.
    #[must_use]
    pub fn enabled_default() -> Self {
        Self::default()
    }

    /// Turns auto-chunking off: `finalize()` always single-shot decodes,
    /// regardless of buffered audio length. The explicit
    /// `transcribe_long_form` method is unaffected by this -- it is a
    /// separate, always-available manual entry point.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

impl Default for ChunkingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_seconds: DEFAULT_AUTO_CHUNK_THRESHOLD_SECONDS,
            window_seconds: DEFAULT_CHUNK_WINDOW_SECONDS,
            overlap_seconds: DEFAULT_CHUNK_OVERLAP_SECONDS,
        }
    }
}

/// Decoder configuration. `Default` gives a sensible CPU-friendly baseline;
/// override individual fields for a specific model / hardware.
#[derive(Debug, Clone)]
pub struct WhisperAsrConfig {
    pub model_path: PathBuf,
    /// Defaults to `min(4, available_parallelism)`.
    pub n_threads: i32,
    /// `Some("en")` (etc.) pins the language; `None` leaves whisper.cpp's
    /// own default in place (which is "en" unless overridden -- see
    /// `FullParams::set_language`'s doc comment in whisper-rs). Irrelevant
    /// for the English-only `*.en` ggml models this crate downloads by
    /// default (`ModelId::TinyEn` / `ModelId::BaseEn`), but real for a
    /// caller who points `model_path` at a multilingual model.
    pub language: Option<String>,
    pub translate: bool,
    /// `None` -> greedy decoding (`best_of: 5`, whisper.cpp's own
    /// default). `Some(n)` -> beam search with beam width `n` (slower,
    /// sometimes more accurate).
    pub beam_size: Option<i32>,
    /// Ring buffer capacity for accumulated PCM, in seconds at 16 kHz.
    /// Must comfortably exceed the longest utterance you intend to feed;
    /// past capacity, `PcmRingBuffer` starts silently dropping the oldest
    /// samples (documented sliding-window behavior -- see
    /// `voice_core::PcmRingBuffer`), which for a batch decoder means
    /// silently truncating the start of a too-long utterance rather than
    /// erroring. Defaults to 300s (5 minutes).
    pub pcm_capacity_seconds: u32,
    /// `None` leaves whisper-rs's own default (`cfg!(feature = "_gpu")`,
    /// i.e. on when this crate was built with the `metal` feature enabled
    /// -- see this crate's Cargo.toml). `Some(x)` overrides explicitly.
    pub use_gpu: Option<bool>,
    /// Whether/how `finalize()` auto-chunks long buffered audio. Defaults
    /// to `ChunkingConfig::default()` (auto-chunk ON, 60s threshold) --
    /// see this module's doc comment for the measurement behind that
    /// default, and `ChunkingConfig::disabled()` for the opt-out.
    pub chunking: ChunkingConfig,
}

impl WhisperAsrConfig {
    #[must_use]
    pub fn new(model_path: PathBuf) -> Self {
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get().min(4) as i32)
            .unwrap_or(4);
        Self {
            model_path,
            n_threads,
            language: None,
            translate: false,
            beam_size: None,
            pcm_capacity_seconds: 300,
            use_gpu: None,
            chunking: ChunkingConfig::default(),
        }
    }
}

pub struct WhisperLocalAsr {
    ctx: WhisperContext,
    caps: AsrCaps,
    config: WhisperAsrConfig,
    started: bool,
    pcm: PcmRingBuffer,
    /// Stored to satisfy the `LocalAsr` trait; see the module doc comment
    /// for why this backend never actually invokes it.
    #[allow(dead_code)]
    partial_cb: Option<PartialCallback>,
}

impl WhisperLocalAsr {
    pub fn new(config: WhisperAsrConfig) -> Result<Self, WhisperAsrError> {
        let mut params = WhisperContextParameters::default();
        if let Some(use_gpu) = config.use_gpu {
            params.use_gpu(use_gpu);
        }
        let model_path_str = config
            .model_path
            .to_str()
            .ok_or_else(|| WhisperAsrError::NonUtf8ModelPath(config.model_path.clone()))?;
        let ctx = WhisperContext::new_with_params(model_path_str, params).map_err(|source| {
            WhisperAsrError::ModelLoad {
                path: config.model_path.clone(),
                source,
            }
        })?;

        let langs = if ctx.is_multilingual() {
            vec!["auto".to_string()]
        } else {
            vec!["en".to_string()]
        };

        let caps = AsrCaps {
            // Batch decode-on-finalize, not incremental -- see module doc.
            streaming: false,
            // whisper.cpp has no decode-time hotword/biasing hook -- that's
            // a transducer-engine feature per SPEC 3.3 layer 1, and whisper
            // is not a transducer. This backend previously used
            // `initial_prompt` as a stand-in "layer 1" for whisper, but
            // that was never what the spec called for (whisper-class models
            // "rely on layers 2-3" per SPEC 3.3) and it was measured to
            // actively destroy decode quality on real dictation input -- see
            // this module's doc comment, "No prompt-conditioning" section.
            // `decode_time_bias` stays `false`: bias now flows only through
            // layers 2-3 (post-decode, engine-agnostic), which is the
            // honest answer for this engine either way.
            decode_time_bias: false,
            // whisper.cpp's decoder emits punctuation as part of normal
            // text generation (trained that way) -- this is real, not a
            // guess.
            punctuation: true,
            langs,
        };

        let capacity_samples = (config.pcm_capacity_seconds as usize)
            .saturating_mul(16_000)
            .max(16_000);

        Ok(Self {
            ctx,
            caps,
            started: false,
            pcm: PcmRingBuffer::new(capacity_samples),
            partial_cb: None,
            config,
        })
    }

    /// Long-form workaround: split `pcm` into overlapping windows, run the
    /// normal `start_utterance`/`feed_pcm`/`finalize` cycle on each one
    /// independently (reusing `self`, so no repeated model load), and stitch
    /// the per-window word lists into one transcript, de-duplicating the
    /// words that fall inside each window's overlap with the next.
    ///
    /// This exists because whisper.cpp's OWN long-form handling
    /// (segment-seek across a multi-minute clip fed as one `whisper_full()`
    /// call) silently drops large contiguous spans of content on this build
    /// -- reproduced on `fixtures/audio/ref-3min.wav` at WER 0.2488 (101
    /// deletions / 0 insertions against a 418-word reference: the decoder
    /// stops emitting words partway through rather than mis-hearing them).
    /// See `ModelId::BaseEn`'s doc comment for the fuller repro history
    /// (parameter tuning and model swaps were tried first and ruled out).
    /// Doing the windowing ourselves -- decode a bounded span, throw the
    /// state away, decode the next span -- sidesteps whatever internal
    /// state (segment-seek heuristics, KV-cache growth, hallucination
    /// guards) causes the drop on a single long call, because no individual
    /// call here is long-form from whisper.cpp's point of view.
    ///
    /// `chunk_seconds` should be well inside the regime this build
    /// transcribes reliably (short-form, i.e. push-to-talk-shaped); a few
    /// seconds of `overlap_seconds` on top gives the stitcher a real word
    /// run to de-duplicate at each boundary. The obvious starting guess
    /// (30s windows, matching whisper.cpp's own internal long-form window)
    /// was measured on `fixtures/audio/ref-3min.wav` and still left the
    /// drop mostly intact (WER 0.2488 -> 0.2129); a sweep down to smaller
    /// windows found the drop shrinks with window size -- 15s/2s got to
    /// WER 0.1196, 10s/1.5s to WER 0.0526 (3 substitutions / 9 deletions /
    /// 10 insertions -- deletions nearly gone, at the cost of some
    /// overlap-dedup misses producing insertions instead), 8s/1.5s
    /// regressed slightly (WER 0.0646: deletions to 1, but insertions to
    /// 23 as more, smaller windows means more overlap boundaries for the
    /// stitcher to miss). This crate does not hardcode a "best" value --
    /// see the CLI caller (`voice-cli`'s `transcribe.rs`, `--long-form-
    /// chunking`) for the 10s/1.5s default this sweep picked. Each window
    /// is transcribed on its own PCM slice (not through the shared ring
    /// buffer's sliding-window capacity), so this path is also immune to
    /// the separate ring-buffer-capacity truncation `pcm_capacity_seconds`
    /// documents.
    ///
    /// Caller-visible effect on `LocalTranscript`: `per_word_conf` is the
    /// stitched, de-duplicated word list (this is what
    /// `voice_core::normalize` actually consumes downstream); `text` is
    /// reconstructed by joining those same words with spaces (it does not
    /// attempt to reproduce whisper.cpp's own inter-segment
    /// punctuation/spacing across a window boundary -- callers that need
    /// exact prose formatting should go through the normalizer, as the
    /// existing single-call path's callers already do); `detected_lang` is
    /// taken from the first non-empty window.
    ///
    /// No `bias: &BiasContext` parameter here (removed by the
    /// fix:no-prompt-conditioning unit): this backend no longer feeds
    /// `BiasContext` terms into whisper.cpp's decode at all -- see this
    /// module's doc comment, "No prompt-conditioning" section. Callers that
    /// want bias applied to a long-form transcript still get it exactly the
    /// way every other caller does: run bias layer 2 (`voice_core::
    /// normalize`) against the same `BiasContext` over this method's output.
    pub fn transcribe_long_form(
        &mut self,
        pcm: &[i16],
        chunk_seconds: f64,
        overlap_seconds: f64,
    ) -> Result<LocalTranscript, AsrError> {
        self.decode_chunked(pcm, chunk_seconds, overlap_seconds)
    }

    /// Shared windowing/stitching core behind both `transcribe_long_form`
    /// (explicit manual call) and `finalize()`'s default auto-chunk path
    /// (triggered once buffered audio reaches `ChunkingConfig::
    /// threshold_seconds` -- see this module's doc comment for the
    /// measurement behind that default). Decodes each window with
    /// `decode_window` directly -- not through `start_utterance`/
    /// `feed_pcm`/`finalize` -- so this never touches `self.pcm` or
    /// `self.started`, and so it is safe to call from inside `finalize()`
    /// itself without reentrancy: each window is `create_state()` +
    /// `full()` on its own bounded PCM slice.
    fn decode_chunked(
        &mut self,
        pcm: &[i16],
        chunk_seconds: f64,
        overlap_seconds: f64,
    ) -> Result<LocalTranscript, AsrError> {
        debug_assert!(chunk_seconds > overlap_seconds, "chunk must exceed overlap");
        let window_samples = ((chunk_seconds * 16_000.0).round() as usize).max(16_000);
        let overlap_samples = ((overlap_seconds * 16_000.0).round() as usize).min(window_samples / 2);
        let step_samples = (window_samples - overlap_samples).max(1);

        let mut stitched: Vec<WordConfidence> = Vec::new();
        let mut detected_lang: Option<String> = None;

        if pcm.is_empty() {
            return Err(AsrError::NoAudioFed);
        }

        let mut start = 0usize;
        loop {
            let end = (start + window_samples).min(pcm.len());
            let window_transcript = self.decode_window(&pcm[start..end])?;
            // Actual duration of THIS window, not `chunk_seconds` -- the
            // final window is routinely shorter (whatever's left of `pcm`),
            // and `stitch_words` needs the real denominator to turn this
            // window's own word count into a speaking-rate estimate (see
            // `plausible_overlap_word_bound`'s doc comment).
            let window_duration_seconds = (end - start) as f64 / 16_000.0;

            if detected_lang.is_none() && !window_transcript.per_word_conf.is_empty() {
                detected_lang = Some(window_transcript.detected_lang.clone());
            }
            stitch_words(
                &mut stitched,
                window_transcript.per_word_conf,
                overlap_seconds,
                window_duration_seconds,
            );

            if end >= pcm.len() {
                break;
            }
            start += step_samples;
        }

        let text = stitched
            .iter()
            .map(|w| w.word.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        Ok(LocalTranscript {
            text,
            per_word_conf: stitched,
            detected_lang: detected_lang.unwrap_or_else(|| "en".to_string()),
        })
    }

    /// Single whisper.cpp `whisper_full()` call over exactly the samples
    /// given -- no ring buffer, no chunking decision. This is the one place
    /// that actually talks to whisper-rs; both `finalize()`'s single-shot
    /// path and `decode_chunked`'s per-window path call it, so the two
    /// paths cannot drift in how a window is decoded (config, sampling
    /// strategy, bias prompt, token/word grouping -- all identical either
    /// way; only how many times this runs, and over what slices, differs).
    fn decode_window(&mut self, raw_pcm: &[i16]) -> Result<LocalTranscript, AsrError> {
        let mut samples = vec![0.0f32; raw_pcm.len()];
        convert_integer_to_float_audio(raw_pcm, &mut samples)
            .map_err(|e| AsrError::Internal(format!("PCM i16->f32 conversion failed: {e:?}")))?;

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| AsrError::Internal(format!("whisper create_state() failed: {e:?}")))?;

        let sampling = match self.config.beam_size {
            Some(beam_size) => SamplingStrategy::BeamSearch {
                beam_size,
                patience: -1.0,
            },
            None => SamplingStrategy::Greedy { best_of: 5 },
        };
        let mut params = FullParams::new(sampling);
        params.set_n_threads(self.config.n_threads);
        params.set_translate(self.config.translate);
        // No token-level timestamps: this backend doesn't surface them
        // (LocalTranscript has no timestamp field) and turning them off
        // keeps whisper.cpp's per-token stream to text tokens only, which
        // is what the word-grouping logic below assumes.
        params.set_no_timestamps(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        if let Some(lang) = self.config.language.as_deref() {
            params.set_language(Some(lang));
        }
        // Deliberately NOT calling `params.set_initial_prompt(..)` here --
        // see this module's doc comment, "No prompt-conditioning" section,
        // for the measured reason (a one-term `BiasContext` collapsed real
        // dictation windows to a single hallucinated word).

        state
            .full(params, &samples)
            .map_err(|e| AsrError::Internal(format!("whisper full() failed: {e:?}")))?;

        let (text, per_word_conf) = collect_transcript(&state);

        let detected_lang = if self.ctx.is_multilingual() {
            let lang_id = state.full_lang_id_from_state();
            whisper_rs::get_lang_str(lang_id)
                .unwrap_or("unknown")
                .to_string()
        } else {
            "en".to_string()
        };

        Ok(LocalTranscript {
            text,
            per_word_conf,
            detected_lang,
        })
    }
}

/// Normalize a word for overlap-matching purposes only (comparison, not
/// output): lowercase, and strip leading/trailing punctuation that
/// whisper.cpp may attach (`"Hello,"`, `"world."`) so the same spoken word
/// transcribed once at the tail of one window and once at the head of the
/// next still compares equal even if sentence-boundary punctuation differs
/// between the two independent decodes.
fn normalize_for_overlap(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Hard ceiling on the overlap-match search, independent of the
/// duration-derived bound below: a cost bound on the O(k^2)-ish scan (a
/// window pair with an implausibly high estimated word rate cannot walk the
/// search past this many words either way), and a second line of defense if
/// the rate estimate is ever wildly off. In normal operation the
/// duration-derived bound in [`plausible_overlap_word_bound`] is what
/// actually limits the search -- see that function's doc comment for why a
/// fixed cap alone (this constant, unconditionally, was the *whole* bound
/// before this fix) is not a safe stitching policy on its own.
const MAX_OVERLAP_WORDS: usize = 25;

/// Multiplies the raw duration*rate overlap estimate in
/// [`plausible_overlap_word_bound`] to absorb the fact that a chunk
/// boundary is cut on a fixed *sample* count, not a word boundary: a word
/// straddling the cut, or a shift of whisper.cpp's own segmentation by a
/// token or two near the edge, can put a couple more real overlap words on
/// one side of the cut than a plain linear estimate expects. Picked to be
/// generous enough to cover that slop while staying well under the
/// word-count of a short repeated phrase (this crate's regression tests
/// exercise exactly that boundary -- see `stitch_tests::seam_regressions`).
const OVERLAP_ESTIMATE_SLOP: f64 = 1.5;

/// Small additive floor added on top of `OVERLAP_ESTIMATE_SLOP`'s
/// multiplier, so a short `overlap_seconds` or a slow-speaking window
/// still gets a few words of real search room instead of the
/// multiplicative estimate rounding down below the true overlap.
const OVERLAP_ESTIMATE_CUSHION: usize = 2;

/// How many words of `new_words` could plausibly be the SAME audio as the
/// tail of `acc`, purely from information this call already has: exactly
/// `overlap_seconds` of audio at the start of this window was already
/// decoded once at the end of the previous window (see
/// [`WhisperLocalAsr::transcribe_long_form`]'s doc comment for why windows
/// overlap on purpose), and `word_count` words came out of this window's
/// own `window_seconds` of audio -- so `word_count / window_seconds` is
/// this window's own observed speaking rate, and `overlap_seconds` worth of
/// that rate is the estimate, widened by [`OVERLAP_ESTIMATE_SLOP`] and
/// [`OVERLAP_ESTIMATE_CUSHION`] for real-world slop, then clamped to
/// [`MAX_OVERLAP_WORDS`].
///
/// This is the actual fix `stitch_words` needs: overlap-dedup by exact text
/// match alone cannot tell "the same words because the windows overlap"
/// apart from "the same words because the speaker repeated themselves" --
/// only the AUDIO tells them apart, and whisper.cpp is not asked for
/// per-word timestamps here (`decode_window` sets `no_timestamps(true)`,
/// and whisper.cpp's segment-level timestamps degrade to the whole-window
/// span rather than real audio positions once that flag is set, so they
/// are not a usable substitute without a decode-time change wider than
/// this stitcher). The next best thing this call DOES have is the known
/// overlap duration plus this window's own measured word rate, which is
/// what this function turns into a search bound.
fn plausible_overlap_word_bound(
    overlap_seconds: f64,
    window_seconds: f64,
    word_count: usize,
) -> usize {
    if overlap_seconds <= 0.0 || window_seconds <= 0.0 || word_count == 0 {
        return 0;
    }
    let rate_words_per_second = word_count as f64 / window_seconds;
    let estimate =
        overlap_seconds * rate_words_per_second * OVERLAP_ESTIMATE_SLOP + OVERLAP_ESTIMATE_CUSHION as f64;
    (estimate.ceil() as usize).clamp(1, MAX_OVERLAP_WORDS)
}

/// Append `new_words` to `acc`, skipping the prefix of `new_words` that
/// duplicates the tail of `acc` -- the overlap region every window after
/// the first one re-transcribes on purpose (see `transcribe_long_form`).
///
/// The search for that duplicated prefix is bounded by
/// [`plausible_overlap_word_bound`], NOT by a fixed word count: that bound
/// is what `overlap_seconds` of audio, at this window's own observed
/// speaking rate, could plausibly contain. Anything past it cannot be the
/// re-decoded overlap -- by construction, only `overlap_seconds` of audio
/// was re-decoded -- so it can only be the speaker genuinely repeating
/// themselves, and this function will not touch it. That is the fix for
/// the real defect this doc comment used to describe as already solved and
/// was not: a fixed, audio-duration-blind cap (unconditionally 25 words)
/// let a real repeated phrase get matched and deleted whenever it happened
/// to be that short or shorter -- exactly the silent-content-drop failure
/// this whole mechanism exists to avoid, reproduced on this crate's own
/// regression tests (`stitch_tests::seam_regressions`) and on a real clip
/// (a 12-word sentence repeated back-to-back). Bounding the search to what
/// the overlap could physically contain makes the "prefer duplication over
/// deletion" claim below actually true: a false-positive dedup can now
/// only ever remove approximately one overlap-window's worth of words
/// (a handful), never an entire repeated sentence or paragraph beyond it.
///
/// Within that bound, still find the LONGEST matching (tail of `acc`) ==
/// (prefix of `new_words`) run, so a longer genuine overlap match is
/// preferred over a shorter coincidental one; if no run matches at all
/// (including when the bound itself is 0, e.g. `overlap_seconds <= 0.0`),
/// fall back to appending everything -- no dedup -- which risks a few
/// duplicated words rather than dropping content. That fallback, and the
/// bound above it, are the same policy applied twice: when uncertain,
/// prefer a duplicate over a deletion, never the reverse.
fn stitch_words(
    acc: &mut Vec<WordConfidence>,
    new_words: Vec<WordConfidence>,
    overlap_seconds: f64,
    window_seconds: f64,
) {
    if acc.is_empty() {
        acc.extend(new_words);
        return;
    }
    if new_words.is_empty() {
        return;
    }

    let overlap_bound = plausible_overlap_word_bound(overlap_seconds, window_seconds, new_words.len());
    let max_k = overlap_bound.min(acc.len()).min(new_words.len());
    let mut best_k = 0usize;
    for k in (1..=max_k).rev() {
        let acc_tail = &acc[acc.len() - k..];
        let new_head = &new_words[..k];
        let matches = acc_tail
            .iter()
            .zip(new_head.iter())
            .all(|(a, b)| normalize_for_overlap(&a.word) == normalize_for_overlap(&b.word));
        if matches {
            best_k = k;
            break;
        }
    }

    acc.extend(new_words.into_iter().skip(best_k));
}

impl LocalAsr for WhisperLocalAsr {
    fn capabilities(&self) -> AsrCaps {
        self.caps.clone()
    }

    // `bias` is intentionally unused in both hooks below: this backend used
    // to turn `BiasContext` terms into a whisper.cpp `initial_prompt` here
    // (a stand-in "layer 1" for an engine that, per SPEC 3.3, has no
    // decode-time hotword hook -- layer 1 is transducer-only). That was
    // measured to actively destroy decode quality on real dictation input
    // (see this module's doc comment, "No prompt-conditioning" section), so
    // it has been removed rather than kept behind a default-off knob whose
    // "on" setting is known to be unsafe. Bias still reaches the transcript
    // -- just at layers 2-3 (post-decode, engine-agnostic), which callers
    // apply themselves against the same `BiasContext` (see
    // `voice_core::normalize`). These hooks are kept only to satisfy the
    // `LocalAsr` trait's signature.
    fn start_utterance(&mut self, _bias: &BiasContext) {
        self.started = true;
        self.pcm.clear();
    }

    fn update_bias(&mut self, _bias: &BiasContext) {}

    fn feed_pcm(&mut self, frames: &[i16]) {
        self.pcm.push(frames);
    }

    fn on_partial(&mut self, cb: PartialCallback) {
        // Stored, never called -- see module doc comment: this backend has
        // no cheap partial to offer and will not fake one.
        self.partial_cb = Some(cb);
    }

    fn finalize(&mut self) -> Result<LocalTranscript, AsrError> {
        if !self.started {
            return Err(AsrError::NoUtteranceStarted);
        }
        if self.pcm.is_empty() {
            return Err(AsrError::NoAudioFed);
        }
        self.started = false;

        let raw_pcm = self.pcm.replay();

        // Default auto-chunk decision -- see this module's doc comment for
        // the length-swept measurement behind `threshold_seconds`. Below
        // it, this is the exact same single `decode_window` call this
        // backend has always made on `finalize()` (same code path, same
        // speed -- short-5s.wav-scale audio never reaches the chunking
        // branch at all). At/above it, silently switch to the same
        // windowed decode-and-stitch `transcribe_long_form` exposes
        // explicitly, because whisper.cpp's own long-form decode is known
        // to silently drop large spans of content past that length (this is
        // the actual fix this unit exists to ship: that used to require the
        // caller to opt in).
        let audio_seconds = raw_pcm.len() as f64 / 16_000.0;
        if self.config.chunking.enabled && audio_seconds >= self.config.chunking.threshold_seconds
        {
            let chunk_seconds = self.config.chunking.window_seconds;
            let overlap_seconds = self.config.chunking.overlap_seconds;
            return self.decode_chunked(&raw_pcm, chunk_seconds, overlap_seconds);
        }

        self.decode_window(&raw_pcm)
    }
}

/// Walk every segment/token whisper.cpp produced and build both the flat
/// transcript text and a per-word confidence list, in one pass.
///
/// Word grouping: whisper.cpp's BPE-style tokens each carry their own
/// leading-space-or-not, exactly the way the model was trained to emit
/// them (`" Hello"`, `" world"`, `"'s"` as a continuation with no leading
/// space, etc.) -- a token that starts with a literal space character is
/// the start of a new word; one that doesn't is a continuation of the
/// current word. Concatenating tokens this way reproduces whisper.cpp's
/// own segment-text concatenation exactly, so `text` and the words in
/// `per_word_conf` agree on spelling/punctuation by construction.
///
/// Confidence: `token.token_probability()` is whisper.cpp's real per-token
/// softmax probability (`whisper_full_get_token_p`), not an invented
/// number. A word's confidence is the arithmetic mean of the probabilities
/// of the tokens that compose it.
fn collect_transcript(state: &whisper_rs::WhisperState) -> (String, Vec<WordConfidence>) {
    let mut text = String::new();
    let mut per_word_conf = Vec::new();
    let mut current_word = String::new();
    let mut current_probs: Vec<f32> = Vec::new();

    let n_segments = state.full_n_segments();
    for seg_idx in 0..n_segments {
        let Some(segment) = state.get_segment(seg_idx) else {
            continue;
        };
        if let Ok(seg_text) = segment.to_str_lossy() {
            text.push_str(&seg_text);
        }

        let n_tokens = segment.n_tokens();
        for tok_idx in 0..n_tokens {
            let Some(token) = segment.get_token(tok_idx) else {
                continue;
            };
            let Ok(tok_text) = token.to_str_lossy() else {
                continue;
            };
            if tok_text.is_empty() || is_special_token_text(&tok_text) {
                continue;
            }
            let prob = token.token_probability();
            let starts_new_word = tok_text.starts_with(' ') || current_word.is_empty();
            if starts_new_word && !current_word.is_empty() {
                flush_word(&mut per_word_conf, &mut current_word, &mut current_probs);
            }
            current_word.push_str(&tok_text);
            current_probs.push(prob);
        }
    }
    flush_word(&mut per_word_conf, &mut current_word, &mut current_probs);

    let text = strip_non_speech_markers(text.trim());
    // If every word was a non-speech marker, the confidences describe nothing.
    if text.is_empty() {
        per_word_conf.clear();
    }
    (text, per_word_conf)
}

/// Whisper narrates the absence of speech instead of returning nothing: point
/// it at silence or room tone and it emits literal `[BLANK_AUDIO]`, `[MUSIC]`,
/// `(wind blowing)`, `♪♪♪` and friends. Those are annotations ABOUT the audio,
/// not a transcript of it, and a dictation tool must never treat them as text —
/// pasting "[BLANK_AUDIO]" into someone's editor is worse than doing nothing,
/// because doing nothing is the correct answer.
///
/// This strips bracketed and parenthesised markers plus musical-note runs, and
/// returns empty when that is all there was.
///
/// Deliberately NOT handled here: whisper's other silence behaviour, where it
/// hallucinates plausible sentences ("Thank you.", "Thanks for watching!") on
/// near-silent input. Those are indistinguishable from something a user might
/// genuinely dictate, so filtering them by text would silently eat real
/// utterances. Suppressing those belongs upstream, in endpointing and a
/// speech-energy gate, not in a string filter.
fn strip_non_speech_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    let mut depth_ast = false;

    for ch in text.chars() {
        match ch {
            '[' => depth_sq += 1,
            ']' => depth_sq = (depth_sq - 1).max(0),
            '(' => depth_par += 1,
            ')' => depth_par = (depth_par - 1).max(0),
            // Whisper uses a matched pair of asterisks for actions like
            // *clears throat*; treat it as a toggle rather than a depth.
            '*' => depth_ast = !depth_ast,
            // Musical notes mark instrumental passages.
            '\u{266a}' | '\u{266b}' | '\u{266c}' | '\u{2669}' => {}
            _ if depth_sq == 0 && depth_par == 0 && !depth_ast => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn flush_word(out: &mut Vec<WordConfidence>, word: &mut String, probs: &mut Vec<f32>) {
    let trimmed = word.trim();
    if !trimmed.is_empty() {
        let confidence = if probs.is_empty() {
            0.0
        } else {
            probs.iter().sum::<f32>() / probs.len() as f32
        };
        out.push(WordConfidence {
            word: trimmed.to_string(),
            confidence,
        });
    }
    word.clear();
    probs.clear();
}

/// whisper.cpp's non-text control tokens are rendered (by `to_str`) as
/// bracketed markup rather than real words -- e.g. `[_BEG_]`, `[_TT_123]`,
/// `<|endoftext|>`, `<|0.00|>` timestamp tokens. `set_no_timestamps(true)`
/// keeps most of these out of the token stream already; this is a second,
/// cheap net for whatever slips through (special/control tokens whisper.cpp
/// still emits regardless of that flag).
fn is_special_token_text(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with("<|") && t.ends_with("|>"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod stitch_tests {
    use super::*;

    fn wc(word: &str) -> WordConfidence {
        WordConfidence {
            word: word.to_string(),
            confidence: 0.9,
        }
    }

    fn words(v: &[&str]) -> Vec<WordConfidence> {
        v.iter().map(|w| wc(w)).collect()
    }

    fn text(v: &[WordConfidence]) -> Vec<&str> {
        v.iter().map(|w| w.word.as_str()).collect()
    }

    /// `(overlap_seconds, window_seconds)` deliberately picked so
    /// [`plausible_overlap_word_bound`] saturates at `MAX_OVERLAP_WORDS`
    /// for any word count these older tests use (a handful of words): a
    /// 25s "overlap" against a 1s window is not realistic audio, but it
    /// isolates what these tests actually check (the exact-match/
    /// longest-run search logic itself) from the newer duration-derived
    /// bound, which has its own dedicated tests below
    /// (`seam_regressions`). Real callers never pass anything like this --
    /// see `DEFAULT_CHUNK_OVERLAP_SECONDS`/`DEFAULT_CHUNK_WINDOW_SECONDS`
    /// for the actual defaults.
    const GENEROUS_BOUND: (f64, f64) = (25.0, 1.0);

    #[test]
    fn first_window_is_appended_wholly() {
        let mut acc = Vec::new();
        stitch_words(
            &mut acc,
            words(&["hello", "world"]),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(text(&acc), vec!["hello", "world"]);
    }

    #[test]
    fn exact_overlap_is_deduplicated() {
        let mut acc = words(&["one", "two", "three", "four", "five"]);
        stitch_words(
            &mut acc,
            words(&["four", "five", "six", "seven"]),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(
            text(&acc),
            vec!["one", "two", "three", "four", "five", "six", "seven"]
        );
    }

    #[test]
    fn overlap_matching_ignores_case_and_trailing_punctuation() {
        let mut acc = words(&["one", "two", "Three,"]);
        stitch_words(
            &mut acc,
            words(&["three", "four."]),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(text(&acc), vec!["one", "two", "Three,", "four."]);
    }

    #[test]
    fn longest_overlap_run_is_preferred_over_a_shorter_coincidental_one() {
        // "two" alone would match at k=1, but the real overlap is 3 words --
        // stitching must find the longer run, not stop at the first (shortest)
        // possible match.
        let mut acc = words(&["one", "two", "three", "four", "two"]);
        stitch_words(
            &mut acc,
            words(&["three", "four", "two", "five"]),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(
            text(&acc),
            vec!["one", "two", "three", "four", "two", "five"]
        );
    }

    #[test]
    fn no_overlap_found_appends_everything_rather_than_dropping_content() {
        let mut acc = words(&["one", "two", "three"]);
        stitch_words(
            &mut acc,
            words(&["completely", "different", "words"]),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(
            text(&acc),
            vec!["one", "two", "three", "completely", "different", "words"]
        );
    }

    #[test]
    fn empty_new_window_leaves_accumulator_untouched() {
        let mut acc = words(&["one", "two"]);
        stitch_words(&mut acc, Vec::new(), GENEROUS_BOUND.0, GENEROUS_BOUND.1);
        assert_eq!(text(&acc), vec!["one", "two"]);
    }

    #[test]
    fn overlap_longer_than_max_overlap_words_is_not_detected_at_all() {
        // A genuine 26-word duplicate run between the two windows -- one
        // word past MAX_OVERLAP_WORDS (25), and `GENEROUS_BOUND` puts the
        // duration-derived bound at the same ceiling. The match-length
        // search never tries k=26, and (because every word here is
        // distinct) no shorter k coincidentally matches either, so the
        // whole shared run is appended a second time rather than partially
        // deduplicated. This documents a real, deliberate limit:
        // `overlap_seconds` in practice (a couple seconds of speech)
        // produces far fewer words than the cap once the duration-derived
        // bound is in play (see `seam_regressions` below) -- this test
        // isolates the hard ceiling itself, which still exists as a
        // second line of defense -- see `MAX_OVERLAP_WORDS`'s doc comment.
        let shared: Vec<String> = (0..26).map(|i| format!("w{i}")).collect();
        let mut acc = words(&["lead-in"]);
        acc.extend(shared.iter().map(|w| wc(w)));
        let mut new_words = shared.iter().map(|w| wc(w)).collect::<Vec<_>>();
        new_words.push(wc("trail-out"));

        let before_len = acc.len();
        stitch_words(
            &mut acc,
            new_words.clone(),
            GENEROUS_BOUND.0,
            GENEROUS_BOUND.1,
        );
        assert_eq!(acc.len(), before_len + new_words.len(), "no dedup occurred");
    }

    #[test]
    fn normalize_for_overlap_strips_case_and_punctuation() {
        assert_eq!(normalize_for_overlap("Hello,"), "hello");
        assert_eq!(normalize_for_overlap("WORLD."), "world");
        assert_eq!(normalize_for_overlap("don't"), "don't");
    }

    #[test]
    fn plausible_overlap_word_bound_tracks_observed_rate_not_a_fixed_constant() {
        // 12 words / 10s window = 1.2 words/s; 1.5s of overlap at that rate,
        // widened by the slop/cushion margin, should land well under a
        // 12-word sentence's own length -- otherwise a single repeated
        // sentence could still be swallowed whole by the bound alone.
        let bound = plausible_overlap_word_bound(1.5, 10.0, 12);
        assert!(bound < 12, "bound {bound} should stay under one sentence's length");
        assert!(bound >= 2, "bound {bound} should still leave real search room");
    }

    #[test]
    fn plausible_overlap_word_bound_is_zero_for_a_non_positive_overlap() {
        assert_eq!(plausible_overlap_word_bound(0.0, 10.0, 12), 0);
        assert_eq!(plausible_overlap_word_bound(-1.0, 10.0, 12), 0);
    }

    #[test]
    fn plausible_overlap_word_bound_saturates_at_the_hard_ceiling() {
        // An implausibly high rate (60s window, 6000 words -- 100 words/s)
        // must still be clamped to `MAX_OVERLAP_WORDS`, not left to grow
        // unbounded from the estimate.
        assert_eq!(plausible_overlap_word_bound(5.0, 60.0, 6000), MAX_OVERLAP_WORDS);
    }

    /// Regression tests for the seam-stitching defect this fix exists for:
    /// a fixed, audio-duration-blind search cap (previously
    /// `MAX_OVERLAP_WORDS` unconditionally) could match and delete an
    /// entire genuinely-repeated phrase, not just the audio actually
    /// re-decoded by window overlap. These use realistic
    /// `overlap_seconds`/`window_seconds` (1.5s / 10s, this crate's real
    /// defaults -- see `DEFAULT_CHUNK_OVERLAP_SECONDS`/
    /// `DEFAULT_CHUNK_WINDOW_SECONDS`), not `GENEROUS_BOUND`, because the
    /// whole point is to exercise the duration-derived bound for real.
    mod seam_regressions {
        use super::*;

        const OVERLAP_SECONDS: f64 = 1.5;
        const WINDOW_SECONDS: f64 = 10.0;

        const SENTENCE: [&str; 12] = [
            "keeping",
            "your",
            "workspace",
            "tidy",
            "makes",
            "it",
            "easier",
            "to",
            "find",
            "important",
            "files",
            "quickly",
        ];

        fn sentence() -> Vec<WordConfidence> {
            words(&SENTENCE)
        }

        #[test]
        fn exact_repetition_spanning_a_boundary_dedupes_only_the_true_overlap() {
            // `acc` ends with two back-to-back copies of the 12-word
            // sentence (as if the previous window's decode ended mid
            // repetition). `new_words` opens with the genuine 4-word
            // overlap tail ("find important files quickly" -- roughly what
            // 1.5s at this content's ~2.8 words/s produces) and then the
            // speaker keeps going, saying the SAME sentence twice more.
            // Those two extra repetitions are real content, not overlap,
            // and must survive intact -- the old fixed-25-word cap had
            // enough search room to walk straight through the true overlap
            // and start matching into the genuine repeat instead.
            let mut acc = sentence();
            acc.extend(sentence());

            let mut new_words = words(&["find", "important", "files", "quickly"]);
            new_words.extend(sentence());
            new_words.extend(sentence());
            assert_eq!(new_words.len(), 4 + 12 + 12);

            let before = text(&acc).len();
            stitch_words(&mut acc, new_words, OVERLAP_SECONDS, WINDOW_SECONDS);

            // Only the 4-word true overlap should have been deduplicated;
            // the two trailing genuine repeats (24 words) must both be
            // present in the output, not silently dropped.
            assert_eq!(
                acc.len(),
                before + (4 + 12 + 12) - 4,
                "expected exactly the 4-word true overlap deduplicated, \
                 both genuine repeats preserved -- got {} words",
                acc.len()
            );
            // The tail of the result must contain both trailing repeats
            // verbatim, not a truncated/deleted version of either.
            let tail = text(&acc);
            let last_24: Vec<&str> = tail[tail.len() - 24..].to_vec();
            let mut expected: Vec<&str> = SENTENCE.to_vec();
            expected.extend(SENTENCE.to_vec());
            assert_eq!(last_24, expected, "both trailing repeats must survive verbatim");
        }

        #[test]
        fn near_repetition_is_not_falsely_deduplicated() {
            // The overlap region came back ALMOST identical between the two
            // windows (one word differs: "files" -> "file", the kind of
            // boundary artifact a real re-decode can produce), so it is not
            // a genuine text match at any k. Per the "prefer a duplicate
            // over a deletion" policy, this must NOT be treated as overlap
            // -- everything in `new_words` is kept, even though that means
            // a few near-duplicate words appear twice in the output. Losing
            // real content is the failure this mechanism must never repeat;
            // an extra near-duplicate word is the acceptable cost.
            let mut acc = sentence();
            acc.extend(sentence());

            let mut new_words = words(&["find", "important", "file", "quickly"]);
            new_words.extend(sentence());

            let before = acc.len();
            let new_len = new_words.len();
            stitch_words(&mut acc, new_words, OVERLAP_SECONDS, WINDOW_SECONDS);

            assert_eq!(
                acc.len(),
                before + new_len,
                "a near-match (one differing word) must not be deduplicated -- \
                 nothing may be silently dropped"
            );
        }

        #[test]
        fn normal_non_repeating_seam_dedupes_the_true_overlap_only() {
            // Ordinary prose: a real overlap of a few words, followed by
            // genuinely new (non-repeating) content -- the common case this
            // whole mechanism exists for, exercised with real-world
            // overlap/window durations rather than `GENEROUS_BOUND`.
            let mut acc = words(&[
                "most", "of", "us", "spend", "a", "large", "part", "of", "the", "day",
            ]);
            let mut new_words = words(&["part", "of", "the", "day"]);
            new_words.extend(words(&[
                "in", "front", "of", "a", "screen", "and", "yet", "we", "rarely", "stop",
            ]));

            stitch_words(&mut acc, new_words, OVERLAP_SECONDS, WINDOW_SECONDS);

            assert_eq!(
                text(&acc),
                vec![
                    "most", "of", "us", "spend", "a", "large", "part", "of", "the", "day", "in",
                    "front", "of", "a", "screen", "and", "yet", "we", "rarely", "stop",
                ],
                "true 4-word overlap deduplicated, new content appended once"
            );
        }
    }
}

#[cfg(test)]
mod chunking_config_tests {
    use super::*;

    #[test]
    fn default_is_auto_chunk_on_at_the_measured_threshold_and_window() {
        let cfg = ChunkingConfig::default();
        assert!(cfg.enabled, "auto-chunking must default to ON");
        assert_eq!(cfg.threshold_seconds, DEFAULT_AUTO_CHUNK_THRESHOLD_SECONDS);
        assert_eq!(cfg.window_seconds, DEFAULT_CHUNK_WINDOW_SECONDS);
        assert_eq!(cfg.overlap_seconds, DEFAULT_CHUNK_OVERLAP_SECONDS);
    }

    #[test]
    fn enabled_default_matches_default() {
        assert_eq!(ChunkingConfig::enabled_default(), ChunkingConfig::default());
    }

    #[test]
    fn disabled_turns_off_chunking_but_keeps_the_default_window_sizing() {
        let cfg = ChunkingConfig::disabled();
        assert!(!cfg.enabled, "disabled() must be the real off-switch");
        // The window/threshold fields are irrelevant once `enabled` is
        // false (finalize() never reads them in that case), but they
        // should still be the sane defaults, not zeroed/garbage, in case a
        // caller flips `enabled` back on later without resetting the rest.
        assert_eq!(cfg.threshold_seconds, DEFAULT_AUTO_CHUNK_THRESHOLD_SECONDS);
        assert_eq!(cfg.window_seconds, DEFAULT_CHUNK_WINDOW_SECONDS);
        assert_eq!(cfg.overlap_seconds, DEFAULT_CHUNK_OVERLAP_SECONDS);
    }

    #[test]
    fn whisper_asr_config_new_defaults_to_chunking_config_default() {
        let cfg = WhisperAsrConfig::new(PathBuf::from("/nonexistent/model.bin"));
        assert_eq!(cfg.chunking, ChunkingConfig::default());
    }
}

#[cfg(test)]
mod non_speech_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::strip_non_speech_markers;

    #[test]
    fn the_reported_case_yields_nothing() {
        // Reported from a real dictation: whisper returned exactly this for a
        // key-press with no speech, and it was pasted into the user's editor.
        assert_eq!(strip_non_speech_markers("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn every_common_non_speech_annotation_yields_nothing() {
        for s in [
            "[BLANK_AUDIO]",
            "[ Silence ]",
            "(silence)",
            "[MUSIC]",
            "(gentle music playing)",
            "[NOISE]",
            "(wind blowing)",
            "*clears throat*",
            "♪",
            "♪♪♪",
            "[ Pause ]",
            "(Inaudible)",
            "  [BLANK_AUDIO]  ",
        ] {
            assert_eq!(strip_non_speech_markers(s), "", "not stripped: {s:?}");
        }
    }

    #[test]
    fn real_speech_survives_untouched() {
        for s in [
            "Hello there.",
            "git status",
            "We migrated the pipeline to Elasticsearch last week.",
            "Send it to Sarah and copy me.",
        ] {
            assert_eq!(strip_non_speech_markers(s), s, "damaged: {s:?}");
        }
    }

    #[test]
    fn an_annotation_beside_speech_leaves_the_speech() {
        // Whisper often prefixes a marker to a real utterance.
        assert_eq!(strip_non_speech_markers("[BLANK_AUDIO] Hello there."), "Hello there.");
        assert_eq!(strip_non_speech_markers("(music) open Slack"), "open Slack");
        assert_eq!(
            strip_non_speech_markers("Ship it. [ Silence ] Then tell me."),
            "Ship it. Then tell me."
        );
    }

    #[test]
    fn brackets_that_are_part_of_dictated_content_are_a_known_cost() {
        // Honest limitation, pinned so it is a decision rather than a surprise:
        // someone dictating an array literal loses the bracketed part. Whisper
        // does not distinguish its own annotations from spoken brackets, and
        // erring toward stripping keeps "[BLANK_AUDIO]" out of real documents.
        assert_eq!(strip_non_speech_markers("let x = [1, 2, 3];"), "let x = ;");
    }

    #[test]
    fn unbalanced_markers_do_not_swallow_the_rest_of_the_line() {
        // A stray closing bracket must not drive the depth negative and start
        // discarding real text.
        assert_eq!(strip_non_speech_markers("hello ] world"), "hello world");
    }
}
