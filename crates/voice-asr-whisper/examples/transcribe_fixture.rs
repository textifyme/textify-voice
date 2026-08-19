//! Loads a real ggml whisper model (downloading it to the cache dir on
//! first run) and transcribes a real fixture WAV end to end through
//! `WhisperLocalAsr`'s `LocalAsr` trait implementation -- the actual
//! caller-facing path (`start_utterance` -> `feed_pcm` -> `finalize`), not
//! a shortcut through whisper-rs directly.
//!
//! Usage:
//!   cargo run -p voice-asr-whisper --example transcribe_fixture -- \
//!       fixtures/audio/short-5s.wav [ggml-tiny.en|ggml-base.en] [bias,terms]
//!
//! Model dir override: set TEXTIFY_WHISPER_MODEL_DIR (see
//! `voice_asr_whisper::CACHE_DIR_ENV_VAR`).
//!
//! The optional 3rd arg (comma-separated bias terms) exists to isolate
//! `BiasContext`'s effect on decode directly against `WhisperLocalAsr`,
//! bypassing the CLI, dictionary loader, and normalizer -- this is how the
//! fix:no-prompt-conditioning unit confirmed a single bias term collapses
//! decode quality on this backend even with no other layer involved (see
//! `whisper_asr.rs`'s "No prompt-conditioning" module-doc section). Note
//! `BiasContext` no longer does anything to decode on this backend, so
//! passing bias terms here is now expected to make NO difference to the
//! transcript -- that itself is the regression check this arg is for.

use std::env;
use std::path::PathBuf;

use voice_asr_whisper::{ModelId, ModelManager, WhisperAsrConfig, WhisperLocalAsr};
use voice_core::{AppKind, BiasContext, LocalAsr};

fn read_wav_i16_mono_16k(path: &std::path::Path) -> anyhow::Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate == 16_000,
        "expected 16 kHz WAV, got {} Hz",
        spec.sample_rate
    );
    anyhow::ensure!(
        spec.channels == 1,
        "expected mono WAV, got {} channels",
        spec.channels
    );
    anyhow::ensure!(
        spec.bits_per_sample == 16,
        "expected 16-bit PCM WAV, got {} bits",
        spec.bits_per_sample
    );
    let samples: Result<Vec<i16>, _> = reader.samples::<i16>().collect();
    Ok(samples?)
}

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let wav_path = args
        .next()
        .unwrap_or_else(|| "fixtures/audio/short-5s.wav".to_string());
    let model_arg = args.next().unwrap_or_else(|| "ggml-base.en".to_string());
    let bias_terms_arg = args.next().unwrap_or_default();
    let model_id = match model_arg.as_str() {
        "ggml-tiny.en" | "tiny" | "tiny.en" => ModelId::TinyEn,
        _ => ModelId::BaseEn,
    };

    eprintln!("== textify voice-asr-whisper: fixture transcription ==");
    eprintln!("wav:   {wav_path}");
    eprintln!("model: {} ({})", model_id.filename(), model_id.url());

    let manager = ModelManager::new()?;
    eprintln!("cache: {}", manager.cache_dir().display());

    let already_cached = manager.is_cached(model_id);
    if !already_cached {
        eprintln!("model not cached -- downloading (this can take a while)...");
    }
    let mut last_pct = u64::MAX;
    let model_path = manager.ensure_downloaded(
        model_id,
        Some(&mut |downloaded: u64, total: u64| {
            if total > 0 {
                let pct = (downloaded * 100) / total;
                if pct != last_pct {
                    last_pct = pct;
                    eprint!("\r  {pct:3}%  ({downloaded}/{total} bytes)");
                }
            } else {
                eprint!("\r  {downloaded} bytes downloaded");
            }
        }),
    )?;
    if !already_cached {
        eprintln!();
    }
    eprintln!("model path: {}", model_path.display());

    let pcm = read_wav_i16_mono_16k(&PathBuf::from(&wav_path))?;
    eprintln!("wav samples: {} ({:.2}s @ 16kHz)", pcm.len(), pcm.len() as f64 / 16_000.0);

    let mut config = WhisperAsrConfig::new(model_path);
    config.pcm_capacity_seconds = 600; // generous headroom for ref-3min.wav too
    eprintln!("threads: {}", config.n_threads);

    let mut asr = WhisperLocalAsr::new(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("caps: {:?}", asr.capabilities());

    let bias_terms: Vec<voice_core::BiasTerm> = bias_terms_arg
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(voice_core::BiasTerm::new)
        .collect();
    let bias = if bias_terms.is_empty() {
        BiasContext::empty(AppKind::General)
    } else {
        BiasContext { terms: bias_terms, app_kind: AppKind::General, prev_terms: Vec::new() }
    };
    eprintln!("bias terms: {:?}", bias.terms.iter().map(|t| t.text.as_str()).collect::<Vec<_>>());
    asr.start_utterance(&bias);
    // Feed in realistic chunks rather than one giant slice, exercising the
    // same `feed_pcm` call pattern a real capture loop would use.
    for chunk in pcm.chunks(1600) {
        asr.feed_pcm(chunk);
    }
    let transcript = asr.finalize().map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("=== TRANSCRIPT ===");
    println!("{}", transcript.text);
    println!("=== detected_lang: {} ===", transcript.detected_lang);
    println!("=== per_word_conf ({} words) ===", transcript.per_word_conf.len());
    for wc in &transcript.per_word_conf {
        println!("  {:<20} {:.3}", wc.word, wc.confidence);
    }

    Ok(())
}
