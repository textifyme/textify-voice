//! `textify-voice transcribe` — the fully verifiable path: decode file
//! (voice-audio) -> real local ASR (voice-asr-whisper / whisper.cpp) ->
//! bias layer 2 + normalizer (voice-core, app_kind-aware) -> stdout.
//!
//! This is the whole dictation pipeline minus microphone capture and text
//! insertion, and every stage in it runs for real against the file on disk
//! -- no mocks, no stubs.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Args;

use voice_asr_whisper::{ChunkingConfig, ModelManager, WhisperAsrConfig, WhisperLocalAsr};
use voice_core::{
    default_literal_rules, normalize, BiasContext, BiasTerm, CorrectionThresholds, LocalAsr,
    WordSpan,
};

use crate::common::{split_bias_terms, AppKindArg, ModelArg};

#[derive(Args, Debug)]
pub struct TranscribeArgs {
    /// WAV file to transcribe. Any sample rate / channel count / integer or
    /// float PCM bit depth is decoded and resampled to 16 kHz mono.
    pub file: PathBuf,

    /// Comma-separated proper nouns / jargon to bias the decoder toward
    /// (applied by bias layer 2's phonetic post-correction; whisper is
    /// never prompt-conditioned -- see the note above `LONG_FORM_WARN`) and to bias-layer-2's
    /// phonetic post-correction.
    #[arg(long, value_delimiter = ',')]
    pub bias_terms: Vec<String>,

    /// App-kind context. `code`/`ai`/`terminal` force raw output (no
    /// capitalization/punctuation cleanup, no bias-layer-2 correction --
    /// SPEC.md V1.4: dictated shell/code must survive verbatim). `prose`
    /// (default) runs the full normalizer.
    #[arg(long, value_enum, default_value_t = AppKindArg::Prose)]
    pub app_kind: AppKindArg,

    /// Which cached whisper.cpp model to decode with. Downloaded on first
    /// use if not already cached (see `textify-voice models`). NOTE: an
    /// independent verification run found both tiny.en and base.en
    /// deterministically drop a large (~25%) span of words on real
    /// multi-minute audio on this build -- switching models does not avoid
    /// it. See `voice_asr_whisper::ModelId`'s doc comment.
    #[arg(long, value_enum, default_value_t = ModelArg::BaseEn)]
    pub model: ModelArg,

    /// Turn OFF long-form chunked decoding. `WhisperLocalAsr::finalize()`
    /// now auto-chunks (split into ~10s windows, transcribe each
    /// independently, stitch the results back together de-duplicating the
    /// overlap) once buffered audio reaches ~60s -- ON by default, because
    /// whisper.cpp's own single-shot decode is known to silently drop large
    /// spans of content past that length (measured WER 0.2488 -> 0.0526 on
    /// `fixtures/audio/ref-3min.wav` with `--no-dictionary`, isolating
    /// chunking's own effect from the user dictionary's -- see `run`'s
    /// content-drop warning below for what the dictionary does to that
    /// number on the actual, no-flags-at-all default path; see
    /// `ChunkingConfig`'s and `WhisperLocalAsr::finalize()`'s doc comments
    /// for the length sweep that picked the threshold and window size).
    /// Pass this flag to reproduce the old single-shot-always behavior (and
    /// its known content-drop) instead -- e.g. to compare against it, or if
    /// chunking is ever suspected of introducing its own stitching
    /// artifacts on a particular input.
    #[arg(long)]
    pub no_chunking: bool,

    /// Skip loading the user dictionary (proper nouns / jargon that
    /// bias-layer-2 should correct toward -- see `crate::dictionary`'s doc
    /// comment for the file format). On by default: the same file
    /// `textify-voice dictate` reads, at
    /// `~/Library/Application Support/textify/dictionary.txt` on macOS
    /// (override with `TEXTIFY_VOICE_DICTIONARY_PATH`), created with a
    /// commented starter example on first run either command makes.
    #[arg(long)]
    pub no_dictionary: bool,
}

/// Inputs at or above this length get the "long-form transcription is known
/// to drop content" warning -- see `run`'s stderr warning below and the
/// finding this whole unit exists to make loud rather than fix outright.
/// SPEC's verified-good shape is push-to-talk dictation of short
/// utterances; short-5s.wav-scale audio (WER 0.0) is well under this bar,
/// ref-3min.wav-scale audio (WER 0.2488) is well over it.
const LONG_FORM_WARN_THRESHOLD_SECONDS: f64 = 60.0;

/// `WhisperAsrConfig::pcm_capacity_seconds` is capped at this many seconds
/// (see `run`, matching the cap already applied when sizing the ring
/// buffer) -- inputs longer than this additionally lose the START of the
/// recording to the ring buffer's sliding-window truncation, on top of the
/// long-form content-drop warned about separately above.
const RING_BUFFER_CAP_SECONDS: f64 = 3600.0;

pub fn run(args: TranscribeArgs, verbose: bool) -> Result<()> {
    let model_id = args.model.to_model_id();
    let manager = ModelManager::new().context("resolving the whisper model cache directory")?;

    let t_model = Instant::now();
    if !manager.is_cached(model_id) {
        eprintln!(
            "model {} not cached at {} -- downloading now (one-time)...",
            model_id.filename(),
            manager.cache_dir().display()
        );
    }
    let mut last_pct = u64::MAX;
    let model_path = manager
        .ensure_downloaded(
            model_id,
            Some(&mut |downloaded: u64, total: u64| {
                if total > 0 {
                    let pct = (downloaded * 100) / total;
                    if pct != last_pct {
                        last_pct = pct;
                        eprint!("\r  {pct:3}%  ({downloaded}/{total} bytes)");
                    }
                }
            }),
        )
        .with_context(|| format!("downloading whisper model {}", model_id.filename()))?;
    if last_pct != u64::MAX {
        eprintln!();
    }
    let model_dt = t_model.elapsed();

    let t_decode = Instant::now();
    let pcm = voice_audio::decode_wav_file(&args.file)
        .with_context(|| format!("decoding {}", args.file.display()))?;
    let decode_dt = t_decode.elapsed();
    anyhow::ensure!(
        !pcm.is_empty(),
        "decoded zero audio samples from {} -- is this a valid WAV file?",
        args.file.display()
    );
    let audio_seconds = pcm.len() as f64 / 16_000.0;

    // whisper.cpp hallucinates plausible-looking text on pure digital
    // silence (observed: 1s of all-zero PCM decodes to the single word
    // "You", exit 0) rather than failing -- a real "silent success" that
    // would otherwise ship a fabricated transcript for a broken/empty
    // recording. `peak_amplitude` near zero is genuine digital silence
    // (not just a quiet whisper, which still has real amplitude); refuse
    // clearly here rather than let a hallucination through.
    let stats = voice_audio::compute_stats(&pcm);
    anyhow::ensure!(
        stats.peak_amplitude > 32,
        "{} appears to be silent (peak amplitude {} out of 32767) -- refusing to transcribe: whisper.cpp hallucinates text on pure silence rather than failing, so this guard exists to avoid shipping a fabricated transcript",
        args.file.display(),
        stats.peak_amplitude
    );

    // LOUD, not silent: this build's whisper.cpp decode is known to drop
    // large spans of content on long-form audio when run single-shot
    // (reproduced on fixtures/audio/ref-3min.wav at WER 0.2488 -- 101
    // deletions / 0 insertions against a 418-word reference, i.e. content
    // just stops getting transcribed partway through, not mis-heard). Auto-
    // chunking (see `ChunkingConfig`, ON by default below this threshold)
    // does fix that specific content-drop -- WER 0.0526 on the same
    // fixture, chunking's effect measured in isolation, `--no-dictionary`
    // (see below) so nothing else is in play -- so only warn when the
    // caller has explicitly turned chunking off with `--no-chunking`, which
    // deliberately reproduces the original drop.
    //
    // 0.0526 IS the shipped default's number, re-measured with genuinely
    // default arguments (dictionary present, no flags) against the 418-word
    // reference. It was briefly 0.3947: an earlier build fed dictionary terms
    // into whisper.cpp's `initial_prompt`, and a single starter term deleted
    // 121 words. That mechanism is gone -- SPEC 3.3 puts decode-time bias in
    // layer 1, which is transducer-only, and whisper-class engines are meant
    // to rely on layer 2's phonetic post-correction instead. The episode is
    // why every figure in this file must be reproduced with no flags set.
    if audio_seconds >= LONG_FORM_WARN_THRESHOLD_SECONDS && args.no_chunking {
        eprintln!(
            "WARNING: {audio_seconds:.0}s input exceeds the ~{LONG_FORM_WARN_THRESHOLD_SECONDS:.0}s long-form threshold and --no-chunking is set. This build's whisper.cpp decode is known to silently drop large spans of content on long-form audio when transcribed in one call -- the transcript below may be INCOMPLETE, not just imperfect. Drop --no-chunking (the default) to use the auto-chunked decode path instead."
        );
    }

    let t_asr = Instant::now();
    let mut whisper_config = WhisperAsrConfig::new(model_path);
    // Comfortable headroom over the clip's own length -- PcmRingBuffer
    // silently drops the oldest samples past capacity (documented sliding-
    // window behavior), which for a batch decoder would mean truncating the
    // start of the clip. Cap the request at something sane either way.
    let capacity_seconds_requested = audio_seconds.ceil() + 10.0;
    whisper_config.pcm_capacity_seconds =
        (capacity_seconds_requested.min(RING_BUFFER_CAP_SECONDS)) as u32;

    // The ASR ring buffer is now the ONLY path in (auto-chunking reads from
    // the same `feed_pcm`-fed buffer `finalize()` always has -- see
    // `WhisperLocalAsr::finalize`'s doc comment), so this truncation risk
    // applies unconditionally past the 1-hour cap, independent of chunking.
    if capacity_seconds_requested > RING_BUFFER_CAP_SECONDS {
        let dropped_seconds = capacity_seconds_requested - RING_BUFFER_CAP_SECONDS;
        eprintln!(
            "WARNING: {audio_seconds:.0}s input exceeds the {RING_BUFFER_CAP_SECONDS:.0}s ASR ring-buffer cap -- the FIRST ~{dropped_seconds:.0}s of this recording will be silently dropped (sliding-window truncation) before transcription even starts, independent of the long-form content-drop warning above."
        );
    }

    // Auto-chunking is ON by default (`ChunkingConfig::default()`, already
    // what `WhisperAsrConfig::new` sets) -- `--no-chunking` is the explicit
    // opt-out described on `TranscribeArgs::no_chunking`.
    if args.no_chunking {
        whisper_config.chunking = ChunkingConfig::disabled();
    }

    let mut asr = WhisperLocalAsr::new(whisper_config)
        .map_err(|e| anyhow::anyhow!("loading whisper model: {e}"))?;

    // User dictionary (SPEC §3.3: proper nouns/jargon as a bias source) --
    // on by default, same file `dictate` reads. See `crate::dictionary`'s
    // module doc for the file format and `--no-dictionary` to skip it.
    let dictionary = if args.no_dictionary {
        crate::dictionary::Dictionary::default()
    } else {
        match crate::dictionary::load_or_seed_default() {
            Ok(d) => {
                for err in &d.errors {
                    eprintln!("dictionary warning: {err}");
                }
                if verbose {
                    let path = crate::dictionary::default_path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| "<unresolved>".to_string());
                    eprintln!(
                        "dictionary: {} term(s), {} literal rule(s) loaded from {path}",
                        d.terms.len(),
                        d.literal_rules.len()
                    );
                }
                d
            }
            Err(e) => {
                eprintln!("warning: could not load the user dictionary: {e:#} -- continuing without it");
                crate::dictionary::Dictionary::default()
            }
        }
    };

    let app_kind = args.app_kind.to_voice_core();
    let mut bias_terms: Vec<BiasTerm> =
        split_bias_terms(&args.bias_terms).into_iter().map(BiasTerm::new).collect();
    bias_terms.extend(dictionary.terms.clone());
    let mut literal_rules = default_literal_rules();
    literal_rules.extend(dictionary.literal_rules.clone());
    let bias = BiasContext { terms: bias_terms, app_kind, prev_terms: Vec::new() };

    asr.start_utterance(&bias);
    // Feed in realistic chunks (100 ms at 16 kHz) rather than one giant
    // slice, exercising the same feed_pcm call pattern a real capture loop
    // (voice-audio) would use, even though this backend only actually
    // decodes once, in finalize() (see voice-asr-whisper's module doc).
    for chunk in pcm.chunks(1_600) {
        asr.feed_pcm(chunk);
    }
    let transcript = asr.finalize().map_err(|e| anyhow::anyhow!("whisper finalize: {e}"))?;
    let asr_dt = t_asr.elapsed();

    let t_norm = Instant::now();
    let words: Vec<WordSpan> = transcript
        .per_word_conf
        .iter()
        .map(|w| WordSpan::new(w.word.clone(), w.confidence))
        .collect();
    let result = normalize(&words, &bias, &literal_rules, &CorrectionThresholds::default());
    let norm_dt = t_norm.elapsed();

    println!("{}", result.text);

    if verbose {
        eprintln!();
        eprintln!("-- stage timings (--verbose) --");
        eprintln!("  model load/download : {:>9.1} ms", ms(model_dt));
        eprintln!("  decode (capture)    : {:>9.1} ms", ms(decode_dt));
        eprintln!("  asr (whisper)       : {:>9.1} ms", ms(asr_dt));
        eprintln!("  normalize           : {:>9.1} ms", ms(norm_dt));
        eprintln!("  audio duration      : {audio_seconds:>9.2} s");
        eprintln!("  detected_lang       : {}", transcript.detected_lang);
        eprintln!("  bias-layer-2 corrections applied: {}", result.corrections.len());
        eprintln!("  words (per_word_conf): {}", transcript.per_word_conf.len());
    }

    eprintln!(
        "speech-end-to-text: {:.1} ms (asr {:.1} ms + normalize {:.1} ms, over {:.2}s of audio)",
        ms(asr_dt) + ms(norm_dt),
        ms(asr_dt),
        ms(norm_dt),
        audio_seconds
    );

    Ok(())
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
