//! Real-model integration test against a real fixture clip. This downloads
//! (or reuses a cached) ggml model and runs an actual whisper.cpp decode --
//! not mocked, not stubbed. Gated behind an env var so `cargo test
//! --workspace` stays fast and green for anyone without the model already
//! on disk:
//!
//!   TEXTIFY_VOICE_ASR_WHISPER_RUN_MODEL_TESTS=1 \
//!       cargo test -p voice-asr-whisper --test fixture_transcription -- --nocapture

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use voice_asr_whisper::{ChunkingConfig, ModelId, ModelManager, WhisperAsrConfig, WhisperLocalAsr};
use voice_core::{AppKind, BiasContext, LocalAsr};

const ENV_GATE: &str = "TEXTIFY_VOICE_ASR_WHISPER_RUN_MODEL_TESTS";

fn read_wav_i16(path: &Path) -> Vec<i16> {
    let mut reader = hound::WavReader::open(path).expect("open fixture wav");
    reader
        .samples::<i16>()
        .map(|s| s.expect("decode PCM sample"))
        .collect()
}

/// Same loose containment-based recall check `transcribes_short_5s_fixture_
/// against_reference` uses below -- not the strict WER gate in
/// `fixtures/voice/wer.ts`, just "how much of the reference actually made
/// it into the hypothesis, anywhere."
fn word_recall(reference: &str, hypothesis: &str) -> f64 {
    let ref_words: Vec<&str> = reference.split_whitespace().collect();
    let hyp_lower = hypothesis.to_lowercase();
    let matched = ref_words
        .iter()
        .filter(|w| {
            let cleaned: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            !cleaned.is_empty() && hyp_lower.contains(&cleaned.to_lowercase())
        })
        .count();
    matched as f64 / ref_words.len() as f64
}

#[test]
fn transcribes_short_5s_fixture_against_reference() {
    if std::env::var(ENV_GATE).is_err() {
        eprintln!(
            "skipping real-model test (set {ENV_GATE}=1 to run it against a downloaded ggml model)"
        );
        return;
    }

    let manager = ModelManager::new().expect("resolve model cache dir");
    let model_path = manager
        .ensure_downloaded(ModelId::BaseEn, None)
        .expect("download/verify ggml-base.en model");

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let wav_path = repo_root.join("fixtures/audio/short-5s.wav");
    let reference = std::fs::read_to_string(repo_root.join("fixtures/audio/short-5s.txt"))
        .expect("read reference transcript")
        .trim()
        .to_string();

    let pcm = read_wav_i16(&wav_path);
    assert!(!pcm.is_empty(), "fixture WAV decoded to zero samples");

    let config = WhisperAsrConfig::new(model_path);
    let mut asr = WhisperLocalAsr::new(config).expect("load whisper model");

    let bias = BiasContext::empty(AppKind::General);
    asr.start_utterance(&bias);
    for chunk in pcm.chunks(1600) {
        asr.feed_pcm(chunk);
    }
    let transcript = asr.finalize().expect("finalize transcription");

    eprintln!("reference : {reference}");
    eprintln!("hypothesis: {}", transcript.text);

    assert!(
        !transcript.text.trim().is_empty(),
        "transcript must not be empty"
    );
    assert!(
        !transcript.per_word_conf.is_empty(),
        "must return per-word confidence entries"
    );
    for wc in &transcript.per_word_conf {
        assert!(
            (0.0..=1.0).contains(&wc.confidence),
            "confidence for {:?} out of [0,1]: {}",
            wc.word,
            wc.confidence
        );
    }
    assert_eq!(transcript.detected_lang, "en");

    // Loose smoke-test correctness bar -- not the strict WER gate in
    // fixtures/voice/wer.ts, just "did this actually transcribe the clip."
    let ref_words: Vec<&str> = reference.split_whitespace().collect();
    let hyp_lower = transcript.text.to_lowercase();
    let matched = ref_words
        .iter()
        .filter(|w| {
            let cleaned: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            !cleaned.is_empty() && hyp_lower.contains(&cleaned.to_lowercase())
        })
        .count();
    let recall = matched as f64 / ref_words.len() as f64;
    eprintln!(
        "word recall vs reference: {recall:.2} ({matched}/{})",
        ref_words.len()
    );
    assert!(recall >= 0.7, "transcript recall too low: {recall:.2}");
}

/// Proves auto-chunking is really `finalize()`'s *default* behavior on long
/// audio (the fix this unit exists to ship), through the real caller-facing
/// `LocalAsr` trait path (`start_utterance` -> `feed_pcm` -> `finalize`) --
/// no explicit `transcribe_long_form` call, no `--long-form-chunking` flag
/// equivalent, just `WhisperAsrConfig::new`'s own defaults against a clip
/// long enough to trigger them.
///
/// Also proves `ChunkingConfig::disabled()` is a real, working off-switch
/// (this unit's brief requires "a way to turn it off"): on the *same* audio,
/// with chunking explicitly disabled, `finalize()` reproduces whisper.cpp's
/// known long-form content-drop (see `ModelId::BaseEn`'s doc comment and
/// this module's own -- i.e. `whisper_asr`'s -- doc comment for the
/// repeated, measured WER 0.2488 / 101-deletion repro on this exact
/// fixture). Both runs go through the identical `LocalAsr` trait calls;
/// only `WhisperAsrConfig::chunking` differs, isolating the seam this test
/// is actually checking: the auto-chunk decision inside `finalize()`, not
/// some other difference between the two runs.
#[test]
fn finalize_auto_chunks_long_audio_by_default_and_disabling_it_reproduces_the_known_drop() {
    if std::env::var(ENV_GATE).is_err() {
        eprintln!(
            "skipping real-model test (set {ENV_GATE}=1 to run it against a downloaded ggml model)"
        );
        return;
    }

    let manager = ModelManager::new().expect("resolve model cache dir");
    let model_path = manager
        .ensure_downloaded(ModelId::BaseEn, None)
        .expect("download/verify ggml-base.en model");

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let wav_path = repo_root.join("fixtures/audio/ref-3min.wav");
    let reference = std::fs::read_to_string(repo_root.join("fixtures/audio/ref-3min.txt"))
        .expect("read reference transcript")
        .trim()
        .to_string();

    let pcm = read_wav_i16(&wav_path);
    assert!(!pcm.is_empty(), "fixture WAV decoded to zero samples");
    let audio_seconds = pcm.len() as f64 / 16_000.0;
    assert!(
        audio_seconds >= ChunkingConfig::default().threshold_seconds,
        "fixture ({audio_seconds:.1}s) must exceed the default auto-chunk threshold \
         ({:.1}s) for this test to actually exercise the chunking branch",
        ChunkingConfig::default().threshold_seconds
    );

    let run = |config: WhisperAsrConfig| {
        let mut asr = WhisperLocalAsr::new(config).expect("load whisper model");
        let bias = BiasContext::empty(AppKind::General);
        asr.start_utterance(&bias);
        for chunk in pcm.chunks(1600) {
            asr.feed_pcm(chunk);
        }
        asr.finalize().expect("finalize transcription")
    };

    // Default config: chunking is on (ChunkingConfig::default()), no
    // caller opt-in required -- this is the behavior change this unit
    // ships.
    let default_config = WhisperAsrConfig::new(model_path.clone());
    assert!(
        default_config.chunking.enabled,
        "WhisperAsrConfig::new must default to auto-chunking on"
    );
    let default_transcript = run(default_config);
    let default_recall = word_recall(&reference, &default_transcript.text);
    eprintln!(
        "default (auto-chunk ON) recall: {default_recall:.3} ({} words)",
        default_transcript.per_word_conf.len()
    );

    // Same audio, chunking explicitly turned off via the escape hatch.
    let mut disabled_config = WhisperAsrConfig::new(model_path);
    disabled_config.chunking = ChunkingConfig::disabled();
    let disabled_transcript = run(disabled_config);
    let disabled_recall = word_recall(&reference, &disabled_transcript.text);
    eprintln!(
        "chunking disabled recall: {disabled_recall:.3} ({} words)",
        disabled_transcript.per_word_conf.len()
    );

    assert!(
        default_recall >= 0.97,
        "auto-chunked-by-default transcript should recall nearly the whole \
         reference (measured on this fixture: 0.988): got {default_recall:.3}"
    );
    assert!(
        disabled_recall <= 0.93,
        "chunking disabled should reproduce whisper.cpp's known long-form \
         content-drop on this fixture (measured: recall 0.897, WER 0.2488, \
         101/418 reference words deleted): got {disabled_recall:.3} -- if \
         this rose, either the drop stopped reproducing on this build/model \
         or `ChunkingConfig::disabled()` stopped actually disabling chunking"
    );
    assert!(
        default_recall > disabled_recall,
        "auto-chunking must measurably help on audio this long: default \
         recall {default_recall:.3} was not greater than disabled recall \
         {disabled_recall:.3}"
    );
}

/// Regression test for the fix:no-prompt-conditioning unit's BLOCKER: a
/// `BiasContext` with as few as one term (the shipped default dictionary
/// has exactly one, "Kubernetes") must never degrade decode quality on this
/// backend. It used to -- an earlier version of `WhisperLocalAsr` turned
/// `BiasContext` terms into a whisper.cpp `initial_prompt`, which measurably
/// collapsed real dictation windows (298 words instead of 419 on this exact
/// fixture, WER 0.3947 instead of 0.0526 via the real CLI's default path;
/// see `whisper_asr.rs`'s "No prompt-conditioning" module-doc section for
/// the full numbers). This test drives `WhisperLocalAsr` directly (the same
/// engine object the CLI's `transcribe` command uses), with the same
/// single-term shape the shipped default dictionary produces, and asserts
/// the transcript is not meaningfully worse than the same audio decoded
/// with an empty `BiasContext`. If prompt-conditioning (or any other
/// decode-time use of `BiasContext`) is ever reintroduced on this backend
/// without being proven safe first, this test is what should catch it.
#[test]
fn bias_context_does_not_degrade_decode_quality() {
    if std::env::var(ENV_GATE).is_err() {
        eprintln!(
            "skipping real-model test (set {ENV_GATE}=1 to run it against a downloaded ggml model)"
        );
        return;
    }

    let manager = ModelManager::new().expect("resolve model cache dir");
    let model_path = manager
        .ensure_downloaded(ModelId::BaseEn, None)
        .expect("download/verify ggml-base.en model");

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let wav_path = repo_root.join("fixtures/audio/ref-3min.wav");
    let reference = std::fs::read_to_string(repo_root.join("fixtures/audio/ref-3min.txt"))
        .expect("read reference transcript")
        .trim()
        .to_string();

    let pcm = read_wav_i16(&wav_path);
    assert!(!pcm.is_empty(), "fixture WAV decoded to zero samples");

    let run = |bias: BiasContext| {
        let config = WhisperAsrConfig::new(model_path.clone());
        let mut asr = WhisperLocalAsr::new(config).expect("load whisper model");
        asr.start_utterance(&bias);
        for chunk in pcm.chunks(1600) {
            asr.feed_pcm(chunk);
        }
        asr.finalize().expect("finalize transcription")
    };

    let empty_bias_transcript = run(BiasContext::empty(AppKind::General));
    let empty_bias_recall = word_recall(&reference, &empty_bias_transcript.text);
    eprintln!(
        "empty BiasContext recall: {empty_bias_recall:.3} ({} words)",
        empty_bias_transcript.per_word_conf.len()
    );

    // The exact shape of the shipped default: one dictionary term, no
    // prev_terms -- this is deliberately NOT a stress test with many terms,
    // it is the smallest input that reproduced the real blocker.
    let single_term_bias = BiasContext {
        terms: vec![voice_core::BiasTerm::new("Kubernetes")],
        app_kind: AppKind::General,
        prev_terms: Vec::new(),
    };
    let biased_transcript = run(single_term_bias);
    let biased_recall = word_recall(&reference, &biased_transcript.text);
    eprintln!(
        "single-term BiasContext recall: {biased_recall:.3} ({} words)",
        biased_transcript.per_word_conf.len()
    );

    assert!(
        empty_bias_recall >= 0.97,
        "sanity check on the baseline itself (measured: 0.988): got {empty_bias_recall:.3}"
    );

    // Word-count floor: the original bug dropped word count from 419 to
    // 298 (71% of baseline) via mass deletions. 90% is well above the noise
    // this fixture shows between independent clean runs, and well below
    // where the bug landed.
    let word_count_ratio =
        biased_transcript.per_word_conf.len() as f64 / empty_bias_transcript.per_word_conf.len() as f64;
    assert!(
        word_count_ratio >= 0.90,
        "a single-term BiasContext must not cause mass word deletion: biased \
         transcript has {} words vs {} for an empty BiasContext (ratio \
         {word_count_ratio:.3}, floor 0.90) -- this is the exact shape of \
         the prompt-conditioning collapse this test guards against \
         (originally: 298/419 = 0.711)",
        biased_transcript.per_word_conf.len(),
        empty_bias_transcript.per_word_conf.len()
    );

    // Recall floor, in absolute terms and relative to the baseline run:
    // the original bug measured 0.837 vs a 0.988 baseline on this fixture.
    assert!(
        biased_recall >= 0.93,
        "a single-term BiasContext must not measurably degrade transcript \
         recall (measured original bug: 0.837 vs a 0.988 baseline): got \
         {biased_recall:.3}"
    );
    assert!(
        empty_bias_recall - biased_recall <= 0.05,
        "a single-term BiasContext must not measurably degrade transcript \
         recall relative to the same audio with no bias context: empty \
         BiasContext recall {empty_bias_recall:.3}, single-term BiasContext \
         recall {biased_recall:.3} (gap {:.3}, ceiling 0.05)",
        empty_bias_recall - biased_recall
    );
}
