//! Core `LocalAsr` contract (SPEC.md §3.3) and a deterministic in-memory
//! `MockAsr` standing in for the real sherpa-onnx/ort-backed engine.
//!
//! ```text
//! pub trait LocalAsr {
//!     fn capabilities(&self) -> AsrCaps;
//!     fn start_utterance(&mut self, bias: &BiasContext);
//!     fn update_bias(&mut self, bias: &BiasContext);
//!     fn feed_pcm(&mut self, frames: &[i16]);
//!     fn on_partial(&mut self, cb: PartialCallback);
//!     fn finalize(&mut self) -> Result<LocalTranscript, AsrError>;
//! }
//! ```
//! — SPEC.md §3.3, lines 151-157.

use crate::ring_buffer::PcmRingBuffer;

/// Coarse classification of the frontmost app / focused field, used both to
/// gate local formatting (SPEC §3.4: "forced off in AI/coding apps") and as
/// part of `BiasContext` (SPEC §3.3). Variants are the ones the spec's own
/// acceptance criteria name explicitly (V1.4: "Slack/VS Code/Chrome/
/// Notes/Terminal") plus the two AI/coding buckets the format gate must
/// force off.
///
/// RECONCILIATION (integration pass): SPEC.md lines 164 and 186 name
/// `AppKind` as a single type shared by the local `BiasContext` and the
/// wire-crossing `FormatRequest`. `crates/voice-format::types::AppKind` and
/// `crates/voice-context::types::AppKind` (built independently, in this same
/// run) already agree with each other on `Ai/Code/Terminal/Browser/Chat/
/// Document/Other`. This type's `Code`/`Ai`/`Terminal`/`Browser` variants
/// are spelled to match that set exactly, since those are the ones SPEC's
/// own prose names directly ("AI/coding apps"). `General`, `Messaging`,
/// `Email`, and `Unknown` remain voice-core-only extensions: this crate is
/// the *local, on-device* consumer and needs a "couldn't identify the app"
/// fallback state (`Unknown`) that a value already resolved before crossing
/// the wire never needs, plus finer buckets than the "coarse `app_kind`
/// only" wire contract (SPEC line 291) calls for. A future cross-crate
/// wiring pass should decide whether to collapse this superset into the
/// wire type at the serialization boundary, not before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppKind {
    /// Plain text field with no more specific classification (e.g. Notes).
    General,
    Browser,
    /// Chat/DM surfaces (e.g. Slack).
    Messaging,
    Email,
    Terminal,
    Code,
    /// AI chat/coding-assistant surfaces (e.g. an LLM chat pane, an IDE's AI
    /// panel) — distinct from `Code` because it's app identity, not
    /// file-type, that triggers the raw-paste gate.
    Ai,
    Unknown,
}

impl AppKind {
    /// SPEC §3.4: local formatting is "forced off in AI/coding apps (paste
    /// raw)." This is the single source of truth for that rule — both
    /// `format_gate` and any future caller should go through this rather
    /// than re-deriving the app-kind list.
    ///
    /// This governs exactly one thing: whether the deterministic
    /// normalizer's own *editorializing* passes (literal phrase rules, bias
    /// layer 2 phonetic correction) are suppressed in favor of raw paste.
    /// It says nothing about whether whisper's own sentence-style
    /// formatting (leading capital, trailing sentence punctuation) should
    /// additionally be undone — that is a narrower, separate question
    /// answered by [`AppKind::wants_shell_verbatim`]. Conflating the two
    /// was the V1.4 bug: `Ai` belongs in *this* set (an LLM chat prompt
    /// must not have its wording silently "corrected") but must NOT be in
    /// `wants_shell_verbatim`'s set (an LLM chat prompt is prose, and
    /// stripping its capital letters and periods damages it).
    #[must_use]
    pub fn is_ai_or_coding(self) -> bool {
        matches!(self, AppKind::Code | AppKind::Terminal | AppKind::Ai)
    }

    /// Does dictated text in this app kind mean literal shell/code content,
    /// where whisper's own sentence-style formatting (a leading capital on
    /// the first word, a trailing `.`/`!`/`?`/`,`/`;`/`:` on the last word)
    /// is not something the user said and must be undone to keep the text
    /// usable as a command? True only for `Terminal` and `Code` — the two
    /// kinds where the dictated content *is* shell/code syntax, so a
    /// spurious capital or trailing period can break it (`Git status.` is
    /// not a runnable command; `git status` is).
    ///
    /// Deliberately **excludes `Ai`**: an AI chat/coding-assistant surface
    /// is `is_ai_or_coding() == true` (it still must skip literal-rule and
    /// bias-layer-2 editorializing — SPEC.md line 228, V1.4 raw paste) but
    /// the *content* being dictated there is an ordinary prose chat
    /// message, not shell/code. A capital letter starting a sentence and a
    /// period ending it are not ASR artifacts to undo in that context —
    /// they're exactly what the user wants, and stripping them turned
    /// `"Paris is beautiful in the spring."` into
    /// `"paris is beautiful in the spring"` and `"Dr. Smith visited
    /// Paris..."` into `"dr. Smith visited Paris..."` (the V1.4-blocking
    /// regression this method fixes). Two questions were conflated under
    /// one `AppKind` bucket; this is the split: "skip our own formatting
    /// transforms" (`is_ai_or_coding`, broad — any AI/coding surface) vs.
    /// "undo whisper's sentence formatting because the content is
    /// shell/code" (`wants_shell_verbatim`, narrow — only where the
    /// dictated text is actually shell/code syntax).
    #[must_use]
    pub fn wants_shell_verbatim(self) -> bool {
        matches!(self, AppKind::Code | AppKind::Terminal)
    }
}

/// One term available for biasing (an on-screen proper noun, a user
/// dictionary entry, a filename/identifier). `weight` lets callers prefer
/// one candidate over another when several bias terms are plausible matches
/// for the same span (SPEC §3.3 layer 1/2); higher wins ties.
#[derive(Debug, Clone, PartialEq)]
pub struct BiasTerm {
    pub text: String,
    pub weight: f32,
}

impl BiasTerm {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            weight: 1.0,
        }
    }

    #[must_use]
    pub fn weighted(text: impl Into<String>, weight: f32) -> Self {
        Self {
            text: text.into(),
            weight,
        }
    }
}

/// SPEC §3.3: "on-screen proper nouns, user dictionary, filenames/
/// identifiers in code apps, prior-utterance terms. Assembled async by
/// voice-context; NEVER blocks the first audio frame."
///
/// Staleness policy (SPEC §3.3): "an utterance starts with the PREVIOUS
/// context snapshot (usually same app/field); fresh context applies via
/// `update_bias()` where the engine supports it, and always to layers 2-3."
#[derive(Debug, Clone, PartialEq)]
pub struct BiasContext {
    pub terms: Vec<BiasTerm>,
    pub app_kind: AppKind,
    pub prev_terms: Vec<String>,
}

impl BiasContext {
    #[must_use]
    pub fn empty(app_kind: AppKind) -> Self {
        Self {
            terms: Vec::new(),
            app_kind,
            prev_terms: Vec::new(),
        }
    }
}

/// `{streaming, decode_time_bias, punctuation, langs}` — SPEC §3.3 line 152.
#[derive(Debug, Clone, PartialEq)]
pub struct AsrCaps {
    pub streaming: bool,
    /// Whether this engine supports decode-time hotword biasing (bias
    /// pipeline layer 1 — transducer engines only per SPEC §3.3).
    pub decode_time_bias: bool,
    pub punctuation: bool,
    pub langs: Vec<String>,
}

/// One word's per-word confidence, as returned in `LocalTranscript`.
#[derive(Debug, Clone, PartialEq)]
pub struct WordConfidence {
    pub word: String,
    pub confidence: f32,
}

/// `{text, per_word_conf, detected_lang}` — SPEC §3.3 line 157.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalTranscript {
    pub text: String,
    pub per_word_conf: Vec<WordConfidence>,
    pub detected_lang: String,
}

/// A streaming partial result, pushed (not polled) to the callback
/// registered via `LocalAsr::on_partial` — SPEC §3.3 line 156.
#[derive(Debug, Clone, PartialEq)]
pub struct PartialResult {
    pub text: String,
    pub confidence: f32,
    pub stable: bool,
}

/// Push-based partial callback: `on_partial` per SPEC §3.3 line 156 is
/// "push, not poll."
pub type PartialCallback = Box<dyn FnMut(&PartialResult) + Send>;

/// Typed error for every fallible `LocalAsr` operation. Per this run's rule
/// ("no unwrap/expect/panic on any path reachable from library input"),
/// `finalize()` returns this instead of panicking on caller misuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrError {
    /// `finalize()` called before `start_utterance()`.
    NoUtteranceStarted,
    /// `finalize()` called with zero PCM frames fed.
    NoAudioFed,
    /// Engine-internal failure, carrying a human-readable reason. Real
    /// backends (sherpa-onnx/ort) will map runtime failures onto this.
    Internal(String),
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsrError::NoUtteranceStarted => write!(f, "finalize() called before start_utterance()"),
            AsrError::NoAudioFed => write!(f, "finalize() called with no audio fed"),
            AsrError::Internal(reason) => write!(f, "ASR engine error: {reason}"),
        }
    }
}

impl std::error::Error for AsrError {}

/// SPEC.md §3.3, lines 151-157 — the local engine trait; UI & routing are
/// engine-agnostic over it. Real implementations (sherpa-onnx/ort-backed)
/// are native/IO and out of scope for this crate; [`MockAsr`] is the
/// deterministic stand-in used for testing everything downstream of it.
pub trait LocalAsr {
    /// `{streaming, decode_time_bias, punctuation, langs}`.
    fn capabilities(&self) -> AsrCaps;
    /// New utterance; hotwords loaded if supported.
    fn start_utterance(&mut self, bias: &BiasContext);
    /// Late-arriving context (async AX) — best-effort.
    fn update_bias(&mut self, bias: &BiasContext);
    /// 16 kHz mono PCM — NOT Opus; Opus is cloud-replay only.
    fn feed_pcm(&mut self, frames: &[i16]);
    /// Push, not poll: `{text, confidence, stable}`.
    fn on_partial(&mut self, cb: PartialCallback);
    /// `{text, per_word_conf, detected_lang}`.
    fn finalize(&mut self) -> Result<LocalTranscript, AsrError>;
}

/// Deterministic in-memory `LocalAsr` for tests. It never runs a real
/// model: `finalize()` returns a scripted transcript configured at
/// construction (or via [`MockAsr::set_scripted_transcript`]), and it
/// records everything a caller would want to assert on — fed PCM (via a
/// real [`PcmRingBuffer`], dogfooding the type this crate ships), bias
/// staleness handling, and partial-callback invocations.
pub struct MockAsr {
    caps: AsrCaps,
    started: bool,
    pcm: PcmRingBuffer,
    /// Hotwords "loaded into the decoder" — only mutated when
    /// `caps.decode_time_bias` is true, modeling layer 1's engine-dependent
    /// support.
    hotwords_loaded: Vec<String>,
    /// The latest `BiasContext` this engine has seen via
    /// `start_utterance`/`update_bias`, regardless of decode-time support —
    /// this is what a real engine wrapper would hand to bias layers 2-3,
    /// which SPEC §3.3 says "always" get fresh context.
    last_bias_seen: Option<BiasContext>,
    partial_cb: Option<PartialCallback>,
    scripted_transcript: LocalTranscript,
}

impl MockAsr {
    /// `decode_time_bias` controls whether this mock behaves like a
    /// transducer engine (layer 1 hotwords apply) or not.
    #[must_use]
    pub fn new(decode_time_bias: bool, scripted_transcript: LocalTranscript) -> Self {
        Self {
            caps: AsrCaps {
                streaming: true,
                decode_time_bias,
                punctuation: false,
                langs: vec!["en".to_string()],
            },
            started: false,
            pcm: PcmRingBuffer::new(16_000 * 30), // 30s @ 16kHz, generous for tests
            hotwords_loaded: Vec::new(),
            last_bias_seen: None,
            partial_cb: None,
            scripted_transcript,
        }
    }

    #[must_use]
    pub fn hotwords_loaded(&self) -> &[String] {
        &self.hotwords_loaded
    }

    #[must_use]
    pub fn last_bias_seen(&self) -> Option<&BiasContext> {
        self.last_bias_seen.as_ref()
    }

    /// Samples fed so far, oldest first — exercises `PcmRingBuffer::replay`.
    #[must_use]
    pub fn recorded_pcm(&self) -> Vec<i16> {
        self.pcm.replay()
    }

    pub fn set_scripted_transcript(&mut self, transcript: LocalTranscript) {
        self.scripted_transcript = transcript;
    }

    fn apply_bias(&mut self, bias: &BiasContext) {
        self.last_bias_seen = Some(bias.clone());
        if self.caps.decode_time_bias {
            self.hotwords_loaded = bias.terms.iter().map(|t| t.text.clone()).collect();
        }
    }
}

impl LocalAsr for MockAsr {
    fn capabilities(&self) -> AsrCaps {
        self.caps.clone()
    }

    fn start_utterance(&mut self, bias: &BiasContext) {
        self.started = true;
        self.pcm.clear();
        self.hotwords_loaded.clear();
        self.apply_bias(bias);
    }

    fn update_bias(&mut self, bias: &BiasContext) {
        self.apply_bias(bias);
    }

    fn feed_pcm(&mut self, frames: &[i16]) {
        self.pcm.push(frames);
        if let Some(cb) = self.partial_cb.as_mut() {
            let total = self.pcm.len();
            let partial = PartialResult {
                text: format!("<{total} samples>"),
                confidence: (total as f32 / 16_000.0).min(1.0),
                stable: false,
            };
            cb(&partial);
        }
    }

    fn on_partial(&mut self, cb: PartialCallback) {
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
        Ok(self.scripted_transcript.clone())
    }
}

/// Tracks which `BiasContext` is "effective" across `start_utterance` /
/// `update_bias` calls, independent of any particular `LocalAsr` impl.
/// Exists to make SPEC §3.3's staleness policy directly testable: "an
/// utterance starts with the PREVIOUS context snapshot ... fresh context
/// applies via `update_bias()` ... and always to layers 2-3." Bias layers
/// 2-3 (this crate's `bias`/`normalizer` modules) should always read
/// [`BiasContextTracker::effective`], never a snapshot taken at
/// utterance-start time.
#[derive(Debug, Clone)]
pub struct BiasContextTracker {
    current: BiasContext,
}

impl BiasContextTracker {
    #[must_use]
    pub fn start_utterance(initial: BiasContext) -> Self {
        Self { current: initial }
    }

    /// Late-arriving context always wins for layers 2-3, per SPEC §3.3.
    pub fn update_bias(&mut self, fresh: BiasContext) {
        self.current = fresh;
    }

    #[must_use]
    pub fn effective(&self) -> &BiasContext {
        &self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &str) -> LocalTranscript {
        LocalTranscript {
            text: text.to_string(),
            per_word_conf: Vec::new(),
            detected_lang: "en".to_string(),
        }
    }

    #[test]
    fn finalize_before_start_is_a_typed_error_not_a_panic() {
        let mut asr = MockAsr::new(false, transcript("hello"));
        assert_eq!(asr.finalize(), Err(AsrError::NoUtteranceStarted));
    }

    #[test]
    fn finalize_with_no_audio_is_a_typed_error() {
        let mut asr = MockAsr::new(false, transcript("hello"));
        asr.start_utterance(&BiasContext::empty(AppKind::General));
        assert_eq!(asr.finalize(), Err(AsrError::NoAudioFed));
    }

    #[test]
    fn happy_path_finalize_returns_scripted_transcript() {
        let mut asr = MockAsr::new(false, transcript("hello world"));
        asr.start_utterance(&BiasContext::empty(AppKind::General));
        asr.feed_pcm(&[1, 2, 3]);
        let Ok(out) = asr.finalize() else {
            panic!("finalize should succeed once started and fed audio");
        };
        assert_eq!(out.text, "hello world");
    }

    #[test]
    fn ring_buffer_replays_the_full_fed_utterance() {
        let mut asr = MockAsr::new(false, transcript("x"));
        asr.start_utterance(&BiasContext::empty(AppKind::General));
        asr.feed_pcm(&[10, 20]);
        asr.feed_pcm(&[30]);
        assert_eq!(asr.recorded_pcm(), vec![10, 20, 30]);
    }

    #[test]
    fn on_partial_is_pushed_not_polled() {
        // MockAsr's callback type requires Send, so capture through
        // Arc<Mutex<..>> rather than Rc<RefCell<..>>.
        let captured: std::sync::Arc<std::sync::Mutex<Vec<PartialResult>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_clone = std::sync::Arc::clone(&captured);

        let mut asr = MockAsr::new(false, transcript("x"));
        asr.start_utterance(&BiasContext::empty(AppKind::General));
        asr.on_partial(Box::new(move |p: &PartialResult| {
            captured_clone
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(p.clone());
        }));
        asr.feed_pcm(&[1; 100]);
        asr.feed_pcm(&[1; 100]);

        let results = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            results.len(),
            2,
            "callback should fire once per feed_pcm call"
        );
        assert!(results[1].confidence >= results[0].confidence);
    }

    #[test]
    fn decode_time_bias_supported_engine_loads_hotwords_on_start_and_update() {
        let mut asr = MockAsr::new(true, transcript("x"));
        let ctx_a = BiasContext {
            terms: vec![BiasTerm::new("Postgres")],
            app_kind: AppKind::Code,
            prev_terms: vec![],
        };
        asr.start_utterance(&ctx_a);
        assert_eq!(asr.hotwords_loaded(), &["Postgres".to_string()]);

        let ctx_b = BiasContext {
            terms: vec![BiasTerm::new("Kubernetes")],
            app_kind: AppKind::Code,
            prev_terms: vec![],
        };
        asr.update_bias(&ctx_b);
        assert_eq!(asr.hotwords_loaded(), &["Kubernetes".to_string()]);
        assert_eq!(
            asr.last_bias_seen().map(|c| c.terms.clone()),
            Some(ctx_b.terms)
        );
    }

    #[test]
    fn decode_time_bias_unsupported_engine_never_loads_hotwords_but_still_tracks_latest_context() {
        // Models the staleness policy's other half: layer 1 only applies
        // "where the engine supports it," but layers 2-3 always see fresh
        // context (last_bias_seen updates regardless).
        let mut asr = MockAsr::new(false, transcript("x"));
        let ctx_a = BiasContext {
            terms: vec![BiasTerm::new("Postgres")],
            app_kind: AppKind::General,
            prev_terms: vec![],
        };
        asr.start_utterance(&ctx_a);
        assert!(
            asr.hotwords_loaded().is_empty(),
            "non-transducer engine: no decode-time hotwords"
        );
        assert_eq!(asr.last_bias_seen().map(|c| &c.terms), Some(&ctx_a.terms));

        let ctx_b = BiasContext {
            terms: vec![BiasTerm::new("Kubernetes")],
            app_kind: AppKind::General,
            prev_terms: vec![],
        };
        asr.update_bias(&ctx_b);
        assert!(asr.hotwords_loaded().is_empty());
        assert_eq!(
            asr.last_bias_seen().map(|c| &c.terms),
            Some(&ctx_b.terms),
            "layers 2-3 must see the freshest context even when layer 1 can't use it"
        );
    }

    #[test]
    fn bias_context_tracker_starts_with_previous_snapshot_then_prefers_fresh() {
        let prev = BiasContext {
            terms: vec![BiasTerm::new("Foo")],
            app_kind: AppKind::General,
            prev_terms: vec!["Foo".to_string()],
        };
        let mut tracker = BiasContextTracker::start_utterance(prev.clone());
        assert_eq!(tracker.effective(), &prev);

        let fresh = BiasContext {
            terms: vec![BiasTerm::new("Bar")],
            app_kind: AppKind::General,
            prev_terms: vec!["Foo".to_string()],
        };
        tracker.update_bias(fresh.clone());
        assert_eq!(
            tracker.effective(),
            &fresh,
            "late-arriving context must win for bias layers 2-3"
        );
    }

    #[test]
    fn ai_and_coding_app_kinds_are_flagged_for_the_format_gate() {
        assert!(AppKind::Code.is_ai_or_coding());
        assert!(AppKind::Terminal.is_ai_or_coding());
        assert!(AppKind::Ai.is_ai_or_coding());
        assert!(!AppKind::General.is_ai_or_coding());
        assert!(!AppKind::Browser.is_ai_or_coding());
        assert!(!AppKind::Messaging.is_ai_or_coding());
    }

    #[test]
    fn only_terminal_and_code_want_shell_verbatim_undo() {
        // `Ai` is raw-paste (is_ai_or_coding) but is prose, not shell/code
        // content, so it must NOT be in this narrower set — see the
        // `wants_shell_verbatim` doc comment for the V1.4 regression this
        // guards against.
        assert!(AppKind::Terminal.wants_shell_verbatim());
        assert!(AppKind::Code.wants_shell_verbatim());
        assert!(!AppKind::Ai.wants_shell_verbatim());
        assert!(!AppKind::General.wants_shell_verbatim());
        assert!(!AppKind::Browser.wants_shell_verbatim());
        assert!(!AppKind::Messaging.wants_shell_verbatim());
    }
}
