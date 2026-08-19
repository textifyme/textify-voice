//! `textify-voice bench` — DECISIONS.md D2: "Bench corpora come from a
//! recording tool, not a data-collection project."
//!
//! SPEC WP-V0.0 and COMMANDS-SPEC WP-C0.0 gate every downstream phase on
//! kill-criteria numbers (SPEC §7), and the scoring harness for those
//! numbers already exists and is tested (`fixtures/voice/wer.ts`, 25 unit
//! tests) — but it has never had a real corpus to score, because building
//! one the traditional way means recording audio first and then paying a
//! human to transcribe it by ear, which no automated process can do.
//!
//! The unlock D2 names: a recording tool that **supplies the prompt**. The
//! reference transcript is known before the take even happens — it's the
//! prompt text itself — so the expensive half of corpus building (post-hoc
//! human transcription) disappears entirely. What's left is just recording.
//!
//! Two subcommands:
//! - `bench record` — interactive terminal loop: show a prompt from
//!   `fixtures/voice/prompts/prompts.json`, record a take, quality-check
//!   it, accept/redo/skip/quit, write real 16 kHz mono WAV + update
//!   `fixtures/voice/manifest.json` in place (schema-exact,
//!   `manifest.schema.json`; resumable — never reprocesses an id already
//!   recorded with `placeholder: false`).
//! - `bench score` — runs the recorded manifest through the real local ASR
//!   pipeline (the same `voice-asr-whisper` / `voice-core` path
//!   `transcribe` uses) to produce hypotheses, then hands those and the
//!   manifest to the REAL, existing `fixtures/voice/wer.ts` (via `tsx`, an
//!   external Node/TS runtime — see `score::run`'s doc comment for why
//!   that's a shell-out rather than a Rust reimplementation) and prints the
//!   per-hard-slice-tag recall table SPEC §7 needs, exactly as
//!   `wer.ts::formatResultsTable` renders it. This is the same "small
//!   script" `fixtures/voice/README.md`'s own "How to run this" section
//!   sketches — `bench score` is that script, wired to real recorded audio
//!   instead of a hand-built hypotheses map.
//!
//! `prompts.json` is a richer format than `manifest.schema.json` allows
//! (`kind: command`/`adversarial` prompts, `direction` for "whisper this")
//! — see `fixtures/voice/prompts/README.md` for why that split exists and
//! what does/doesn't get preserved in the schema-exact manifest.

use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use voice_asr_whisper::{ModelManager, WhisperAsrConfig, WhisperLocalAsr};
use voice_audio::{compute_stats, AudioSource, AudioStats, MicCapture};
use voice_core::{
    default_literal_rules, normalize, AppKind, BiasContext, BiasTerm, CorrectionThresholds,
    LocalAsr, WordSpan,
};

use crate::common::ModelArg;

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct BenchArgs {
    #[command(subcommand)]
    pub action: BenchAction,
}

#[derive(Subcommand, Debug)]
pub enum BenchAction {
    /// Interactive recording loop over `fixtures/voice/prompts/prompts.json`
    /// — records real takes, quality-checks them, and writes
    /// `fixtures/voice/manifest.json` (schema-exact, resumable).
    Record(RecordArgs),
    /// Run the recorded manifest's real clips through the local ASR
    /// pipeline and score them with the existing `fixtures/voice/wer.ts`
    /// harness — prints the SPEC §7 per-hard-slice-tag table.
    Score(ScoreArgs),
}

/// A prompt's category. `Dictation` prompts are the WP-V0.0 hard-slice
/// corpus; `Command`/`Adversarial` are COMMANDS-SPEC C0.0 utterances
/// recorded through the same pipeline so their ASR transcripts can be
/// checked too, but command-*intent* accuracy is scored in
/// `fixtures/commands/`, not by this tool — see `prompts/README.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum PromptKind {
    Dictation,
    Command,
    Adversarial,
}

impl PromptKind {
    fn label(self) -> &'static str {
        match self {
            PromptKind::Dictation => "dictation",
            PromptKind::Command => "command",
            PromptKind::Adversarial => "adversarial",
        }
    }
}

#[derive(Args, Debug)]
pub struct RecordArgs {
    /// Prompt set to record from.
    #[arg(long, default_value = "fixtures/voice/prompts/prompts.json")]
    pub prompts: PathBuf,

    /// Manifest to update in place (created fresh if it doesn't exist yet).
    #[arg(long, default_value = "fixtures/voice/manifest.json")]
    pub manifest: PathBuf,

    /// Directory takes are written under, as `<id>.wav`.
    #[arg(long, default_value = "fixtures/voice/audio")]
    pub audio_dir: PathBuf,

    /// `corpus_version` stamped on a FRESHLY created manifest only — if
    /// `--manifest` already exists, its existing `corpus_version` is left
    /// alone (bumping it is a human curation decision per
    /// `manifest.schema.json`'s own description of the field, not
    /// something a recording session should do silently).
    #[arg(long, default_value = "0.1.0-bench-record")]
    pub corpus_version: String,

    /// Only record prompts of this kind.
    #[arg(long, value_enum)]
    pub kind: Option<PromptKind>,

    /// Only record prompts carrying this hard-slice tag (e.g. `whispered`).
    #[arg(long)]
    pub tag: Option<String>,

    /// Only record this one prompt id (overrides `--kind`/`--tag`).
    #[arg(long)]
    pub only: Option<String>,

    /// Free-text accent label stamped on every dictation clip accepted this
    /// session (manifest's `speaker_accent` field, e.g. `en-IN`). Leave
    /// unset to omit the field (the schema's own default for a US-English
    /// speaker).
    #[arg(long)]
    pub speaker_accent: Option<String>,
}

#[derive(Args, Debug)]
pub struct ScoreArgs {
    /// Manifest to score.
    #[arg(long, default_value = "fixtures/voice/manifest.json")]
    pub manifest: PathBuf,

    /// Path to the real `fixtures/voice/wer.ts` scoring library this
    /// command shells out to — never reimplemented in Rust, see
    /// `score::run`'s doc comment.
    #[arg(long, default_value = "fixtures/voice/wer.ts")]
    pub wer_harness: PathBuf,

    /// Which cached whisper.cpp model to score with.
    #[arg(long, value_enum, default_value_t = ModelArg::BaseEn)]
    pub model: ModelArg,

    /// Also load the local user dictionary (`crate::dictionary`) as extra
    /// bias terms. Off by default — a bench score should be reproducible
    /// independent of whichever machine ran it, and the user dictionary is
    /// machine-local, unversioned state.
    #[arg(long)]
    pub use_dictionary: bool,

    /// Label for the results table's "Config" column (SPEC §7 run-matrix
    /// vocabulary, e.g. "local base.en, bias layer 2").
    #[arg(long)]
    pub config_label: Option<String>,
}

pub fn run(args: BenchArgs, verbose: bool) -> Result<()> {
    match args.action {
        BenchAction::Record(a) => record::run(a),
        BenchAction::Score(a) => score::run(a, verbose),
    }
}

// ---------------------------------------------------------------------------
// Shared data shapes
// ---------------------------------------------------------------------------

/// One `{term, kind?}` pair — identical shape in both `prompts.json` and
/// `manifest.schema.json`'s `bias_term`, so one type serves both.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BiasTermEntry {
    term: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptSet {
    #[allow(dead_code)]
    #[serde(default)]
    version: Option<String>,
    prompts: Vec<Prompt>,
}

#[derive(Debug, Clone, Deserialize)]
struct Prompt {
    id: String,
    kind: PromptKind,
    text: String,
    #[serde(default)]
    hard_slice_tags: Vec<String>,
    #[serde(default)]
    bias_terms: Vec<BiasTermEntry>,
    #[serde(default = "default_language")]
    language: String,
    /// Reading direction shown before the take starts, e.g. "whisper this
    /// line" — not part of `manifest.schema.json`, see
    /// `fixtures/voice/prompts/README.md`.
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

fn default_language() -> String {
    "en-US".to_string()
}

/// Mirrors `fixtures/voice/manifest.schema.json` exactly — field-for-field,
/// same required/optional split, same `additionalProperties: false`
/// discipline (achieved here by simply never adding an extra field, not by
/// a runtime check — `score::run`'s `ajv`-style verification lives in this
/// unit's own manual test run, not in this struct).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    corpus_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    clips: Vec<Clip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Clip {
    id: String,
    audio_path: String,
    reference_text: String,
    language: String,
    hard_slice_tags: Vec<String>,
    bias_terms: Vec<BiasTermEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker_accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_s: Option<f64>,
    placeholder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

fn load_prompts(path: &Path) -> Result<Vec<Prompt>> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("reading prompt set {}", path.display()))?;
    let set: PromptSet = serde_json::from_str(&source)
        .with_context(|| format!("parsing prompt set {} as JSON", path.display()))?;
    anyhow::ensure!(!set.prompts.is_empty(), "{} has zero prompts", path.display());
    let mut seen = HashSet::new();
    for p in &set.prompts {
        anyhow::ensure!(
            seen.insert(p.id.clone()),
            "prompt id {:?} appears more than once in {}",
            p.id,
            path.display()
        );
    }
    Ok(set.prompts)
}

/// Load `path` as a [`Manifest`]; a missing file is not an error — it's the
/// normal first run — and returns a fresh, empty manifest stamped with
/// `corpus_version`.
fn load_or_init_manifest(path: &Path, corpus_version: &str) -> Result<Manifest> {
    match fs::read_to_string(path) {
        Ok(source) => serde_json::from_str(&source)
            .with_context(|| format!("parsing existing manifest {} as JSON", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Manifest {
            corpus_version: corpus_version.to_string(),
            notes: None,
            clips: Vec::new(),
        }),
        Err(e) => Err(e).with_context(|| format!("reading manifest {}", path.display())),
    }
}

fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating manifest directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(manifest).context("serializing manifest to JSON")?;
    fs::write(path, json).with_context(|| format!("writing manifest {}", path.display()))
}

/// `audio_path` as `manifest.schema.json` documents it: relative to the
/// manifest file's own directory (which is `fixtures/voice/` for the
/// default paths this crate ships, matching the schema's stated
/// convention, e.g. `'audio/proper-noun-01.wav'`). Falls back to the
/// absolute path (with a note printed by the caller) if `audio_dir` isn't
/// actually nested under the manifest's directory.
fn relative_audio_path(audio_dir: &Path, manifest_path: &Path, filename: &str) -> Result<String> {
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let full = audio_dir.join(filename);
    match audio_dir.strip_prefix(manifest_dir) {
        Ok(rel) => Ok(rel.join(filename).to_string_lossy().replace('\\', "/")),
        Err(_) => Ok(full.to_string_lossy().replace('\\', "/")),
    }
}

// ---------------------------------------------------------------------------
// `bench record`
// ---------------------------------------------------------------------------

mod record {
    use super::*;

    /// Quality-guard thresholds, expressed purely in terms of
    /// `voice_audio::compute_stats`'s own `AudioStats` fields (peak/RMS) —
    /// per this unit's dispatch: "reuse voice-audio's RMS/peak helpers
    /// rather than reimplementing." Nothing here recomputes peak or RMS;
    /// these are just the bars a take is checked against.
    pub(super) const SILENCE_PEAK_THRESHOLD: i16 = 300;
    pub(super) const CLIPPING_PEAK_THRESHOLD: i16 = 32_760;
    const MIN_TAKE_DURATION_S: f64 = 0.3;

    pub fn run(args: RecordArgs) -> Result<()> {
        let prompts = load_prompts(&args.prompts)?;
        let mut manifest = load_or_init_manifest(&args.manifest, &args.corpus_version)?;

        let selected: Vec<&Prompt> = prompts
            .iter()
            .filter(|p| args.kind.is_none_or(|k| k == p.kind))
            .filter(|p| {
                args.tag
                    .as_deref()
                    .is_none_or(|t| p.hard_slice_tags.iter().any(|pt| pt == t))
            })
            .filter(|p| args.only.as_deref().is_none_or(|id| id == p.id))
            .collect();
        anyhow::ensure!(
            !selected.is_empty(),
            "no prompts matched the given --kind/--tag/--only filters"
        );

        fs::create_dir_all(&args.audio_dir)
            .with_context(|| format!("creating audio directory {}", args.audio_dir.display()))?;

        println!("textify-voice bench record");
        println!(
            "  prompts  : {} ({} selected of {} total)",
            args.prompts.display(),
            selected.len(),
            prompts.len()
        );
        println!("  manifest : {}", args.manifest.display());
        println!("  audio    : {}", args.audio_dir.display());
        println!();
        print_progress(&prompts, &manifest);
        println!();

        // One capture stream for the whole session (SPEC V1.1's "already
        // built and warm before the key is pressed" rationale applies just
        // as much to a recording loop with many takes back to back as it
        // does to the live dictate loop -- see voice_audio::capture's own
        // doc comment). `pcm_buf` is cleared and re-filled once per take.
        let pcm_buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
        let cb_buf = Arc::clone(&pcm_buf);
        let mut mic = MicCapture::new(move |frames: &[i16]| {
            if let Ok(mut buf) = cb_buf.lock() {
                buf.extend_from_slice(frames);
            }
        })
        .context(
            "opening the microphone for bench record (check System Settings > Privacy & \
             Security > Microphone access for the app/terminal you launched this from)",
        )?;
        println!(
            "mic: {} ({} Hz, {} ch)",
            mic.device_name(),
            mic.native_sample_rate(),
            mic.native_channels()
        );
        println!();

        let already_done: HashSet<String> = manifest
            .clips
            .iter()
            .filter(|c| !c.placeholder)
            .map(|c| c.id.clone())
            .collect();

        let mut recorded_this_session = 0usize;
        'prompts: for prompt in selected {
            if already_done.contains(&prompt.id) {
                continue;
            }

            println!("--- {} [{}] ---", prompt.id, prompt.kind.label());
            if !prompt.hard_slice_tags.is_empty() {
                println!("tags: {}", prompt.hard_slice_tags.join(", "));
            }
            if let Some(dir) = &prompt.direction {
                println!("direction: {dir}");
            }
            println!("read aloud:");
            println!("  \"{}\"", prompt.text);
            println!();

            'take: loop {
                let start = read_line("[Enter] start recording   q = quit session: ")?;
                if is_quit(&start) {
                    break 'prompts;
                }

                if let Ok(mut buf) = pcm_buf.lock() {
                    buf.clear();
                }
                mic.start().context("starting microphone capture")?;
                println!("recording... [Enter] stop");
                let _ = read_line("");
                mic.stop().context("stopping microphone capture")?;

                let pcm = match pcm_buf.lock() {
                    Ok(buf) => buf.clone(),
                    Err(_) => {
                        println!("(mic buffer lock was poisoned -- treating this take as empty)");
                        Vec::new()
                    }
                };
                let stats = compute_stats(&pcm);
                print_quality_warnings(&stats);

                let decision = read_line(
                    "[Enter] accept   r = redo   s = skip this prompt   q = quit session: ",
                )?;
                if is_quit(&decision) {
                    break 'prompts;
                }
                if decision.eq_ignore_ascii_case("r") {
                    println!("redoing this take...");
                    continue 'take;
                }
                if decision.eq_ignore_ascii_case("s") {
                    println!("skipped {} -- will be offered again next run.", prompt.id);
                    continue 'prompts;
                }

                // Anything else (including plain Enter) accepts.
                let wav_path = args.audio_dir.join(format!("{}.wav", prompt.id));
                write_wav(&wav_path, &pcm)
                    .with_context(|| format!("writing {}", wav_path.display()))?;
                let audio_path = relative_audio_path(
                    &args.audio_dir,
                    &args.manifest,
                    &format!("{}.wav", prompt.id),
                )?;

                let clip_notes = clip_notes(prompt);
                let clip = Clip {
                    id: prompt.id.clone(),
                    audio_path,
                    reference_text: prompt.text.clone(),
                    language: prompt.language.clone(),
                    hard_slice_tags: prompt.hard_slice_tags.clone(),
                    bias_terms: prompt.bias_terms.clone(),
                    speaker_accent: if prompt.hard_slice_tags.iter().any(|t| t == "accented-en") {
                        args.speaker_accent.clone()
                    } else {
                        None
                    },
                    duration_s: Some(stats.duration_s),
                    placeholder: false,
                    notes: clip_notes,
                };
                manifest.clips.retain(|c| c.id != clip.id);
                manifest.clips.push(clip);
                // Persist after every accepted take, not just at the end --
                // per this unit's dispatch ("resumable: re-running must not
                // clobber completed takes"), a crash/quit mid-session must
                // not lose already-accepted work.
                save_manifest(&args.manifest, &manifest)?;
                recorded_this_session += 1;
                println!("saved {} ({:.2}s)", wav_path.display(), stats.duration_s);
                println!();
                break 'take;
            }
        }

        println!();
        println!("session done -- {recorded_this_session} take(s) recorded this run.");
        print_progress(&prompts, &manifest);
        Ok(())
    }

    fn clip_notes(prompt: &Prompt) -> Option<String> {
        let mut parts = Vec::new();
        if prompt.kind != PromptKind::Dictation {
            parts.push(format!(
                "bench prompt kind: {} (see fixtures/voice/prompts/README.md)",
                prompt.kind.label()
            ));
        }
        if let Some(dir) = &prompt.direction {
            parts.push(format!("recording direction: {dir}"));
        }
        if let Some(n) = &prompt.notes {
            parts.push(n.clone());
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" | "))
        }
    }

    fn is_quit(s: &str) -> bool {
        matches!(s.trim(), "q" | "Q" | "quit")
    }

    fn read_line(prompt: &str) -> Result<String> {
        if !prompt.is_empty() {
            print!("{prompt}");
            io::stdout().flush().ok();
        }
        let mut buf = String::new();
        let n = io::stdin()
            .lock()
            .read_line(&mut buf)
            .context("reading from stdin")?;
        if n == 0 {
            // EOF (piped/closed stdin, e.g. Ctrl-D) -- treat exactly like
            // an explicit quit rather than looping forever.
            return Ok("q".to_string());
        }
        Ok(buf.trim().to_string())
    }

    fn print_quality_warnings(stats: &AudioStats) {
        println!(
            "  take: {:.2}s, peak {}/32767, rms {:.1}",
            stats.duration_s, stats.peak_amplitude, stats.rms_amplitude
        );
        if stats.duration_s < MIN_TAKE_DURATION_S {
            println!(
                "  WARNING: only {:.2}s captured (< {MIN_TAKE_DURATION_S}s) -- did recording \
                 actually start? This looks too short to contain real speech.",
                stats.duration_s
            );
        }
        if stats.peak_amplitude < SILENCE_PEAK_THRESHOLD {
            println!(
                "  WARNING: this take looks SILENT (peak {} out of 32767, below the \
                 {SILENCE_PEAK_THRESHOLD} guard) -- check the mic is unmuted and the right \
                 input device is selected.",
                stats.peak_amplitude
            );
        }
        if stats.peak_amplitude >= CLIPPING_PEAK_THRESHOLD {
            println!(
                "  WARNING: this take looks CLIPPED (peak {} out of 32767, at/near full scale) \
                 -- input gain is probably too hot; consider redoing quieter or further from \
                 the mic.",
                stats.peak_amplitude
            );
        }
    }

    fn write_wav(path: &Path, pcm: &[i16]) -> Result<()> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer =
            hound::WavWriter::create(path, spec).context("creating WAV writer")?;
        for &sample in pcm {
            writer.write_sample(sample).context("writing PCM sample")?;
        }
        writer.finalize().context("finalizing WAV file")?;
        Ok(())
    }

    fn print_progress(prompts: &[Prompt], manifest: &Manifest) {
        let done: HashSet<&str> = manifest
            .clips
            .iter()
            .filter(|c| !c.placeholder)
            .map(|c| c.id.as_str())
            .collect();

        let tags = ["proper-noun", "code-identifier", "accented-en", "whispered"];
        println!("progress by hard-slice tag:");
        for tag in tags {
            let total = prompts.iter().filter(|p| p.hard_slice_tags.iter().any(|t| t == tag)).count();
            let have = prompts
                .iter()
                .filter(|p| p.hard_slice_tags.iter().any(|t| t == tag) && done.contains(p.id.as_str()))
                .count();
            println!("  {tag:<14} {have}/{total}");
        }

        println!("progress by kind:");
        for kind in [PromptKind::Dictation, PromptKind::Command, PromptKind::Adversarial] {
            let total = prompts.iter().filter(|p| p.kind == kind).count();
            let have = prompts
                .iter()
                .filter(|p| p.kind == kind && done.contains(p.id.as_str()))
                .count();
            println!("  {:<14} {have}/{total}", kind.label());
        }

        let have = prompts.iter().filter(|p| done.contains(p.id.as_str())).count();
        println!("overall: {have}/{} prompts recorded", prompts.len());
    }
}

// ---------------------------------------------------------------------------
// `bench score`
// ---------------------------------------------------------------------------

mod score {
    use super::*;

    /// Runs the manifest's real (`placeholder: false`) clips through the
    /// same local ASR pipeline `transcribe` uses (`voice-asr-whisper` /
    /// `voice-core`, real whisper.cpp decode + bias layer 2 + normalizer —
    /// no mock, no stub), then hands the resulting hypotheses to the REAL
    /// `fixtures/voice/wer.ts` — not a Rust reimplementation of its WER/
    /// recall math. `wer.ts` is a `.ts` ES module with no build step of its
    /// own; the only way to run its actual exported functions (rather than
    /// a hand-copied approximation that could silently drift from the
    /// tested original) from a Rust binary is to shell out to a JS/TS
    /// runtime. This shells out to `tsx` (already used by this repo's own
    /// TypeScript tooling — see `fixtures/voice/README.md`'s own "How to
    /// run this" section, which sketches exactly this "small script"
    /// pattern) against a small, generated runner script that imports
    /// `realClips`/`corpusWer`/`hardSliceRecall`/`formatResultsTable`
    /// straight from the real file at `--wer-harness` and prints the
    /// per-hard-slice-tag table those functions produce.
    pub fn run(args: ScoreArgs, verbose: bool) -> Result<()> {
        let manifest_json = fs::read_to_string(&args.manifest)
            .with_context(|| format!("reading manifest {}", args.manifest.display()))?;
        let manifest: Manifest = serde_json::from_str(&manifest_json)
            .with_context(|| format!("parsing manifest {} as JSON", args.manifest.display()))?;

        let real_clips: Vec<&Clip> = manifest.clips.iter().filter(|c| !c.placeholder).collect();
        anyhow::ensure!(
            !real_clips.is_empty(),
            "{} has zero non-placeholder clips -- nothing to score. Run `textify-voice bench \
             record` first.",
            args.manifest.display()
        );

        let manifest_dir = args.manifest.parent().unwrap_or_else(|| Path::new("."));

        let model_id = args.model.to_model_id();
        let manager = ModelManager::new().context("resolving the whisper model cache directory")?;
        if !manager.is_cached(model_id) {
            eprintln!(
                "model {} not cached at {} -- downloading now (one-time)...",
                model_id.filename(),
                manager.cache_dir().display()
            );
        }
        let model_path = manager
            .ensure_downloaded(model_id, None)
            .with_context(|| format!("downloading whisper model {}", model_id.filename()))?;

        let dictionary = if args.use_dictionary {
            crate::dictionary::load_or_seed_default().unwrap_or_else(|e| {
                eprintln!("warning: could not load the user dictionary: {e:#}");
                crate::dictionary::Dictionary::default()
            })
        } else {
            crate::dictionary::Dictionary::default()
        };
        let mut literal_rules = default_literal_rules();
        literal_rules.extend(dictionary.literal_rules.clone());

        let whisper_config = WhisperAsrConfig::new(model_path);
        let mut asr = WhisperLocalAsr::new(whisper_config)
            .map_err(|e| anyhow::anyhow!("loading whisper model: {e}"))?;

        let mut hypotheses: Vec<(String, String)> = Vec::new();
        for clip in &real_clips {
            let audio_path = manifest_dir.join(&clip.audio_path);
            let pcm = match voice_audio::decode_wav_file(&audio_path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!(
                        "warning: skipping clip {:?} -- could not decode {}: {e}",
                        clip.id,
                        audio_path.display()
                    );
                    continue;
                }
            };
            if pcm.is_empty() {
                eprintln!("warning: skipping clip {:?} -- decoded zero samples", clip.id);
                continue;
            }

            let mut bias_terms: Vec<BiasTerm> = clip
                .bias_terms
                .iter()
                .map(|b| BiasTerm::new(b.term.clone()))
                .collect();
            bias_terms.extend(dictionary.terms.clone());
            let bias = BiasContext {
                terms: bias_terms,
                app_kind: AppKind::General,
                prev_terms: Vec::new(),
            };

            asr.start_utterance(&bias);
            for chunk in pcm.chunks(1_600) {
                asr.feed_pcm(chunk);
            }
            let transcript = match asr.finalize() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("warning: skipping clip {:?} -- ASR finalize failed: {e}", clip.id);
                    continue;
                }
            };

            let words: Vec<WordSpan> = transcript
                .per_word_conf
                .iter()
                .map(|w| WordSpan::new(w.word.clone(), w.confidence))
                .collect();
            let result = normalize(&words, &bias, &literal_rules, &CorrectionThresholds::default());

            if verbose {
                eprintln!("  {} -> {:?}", clip.id, result.text);
            }
            hypotheses.push((clip.id.clone(), result.text));
        }

        anyhow::ensure!(
            !hypotheses.is_empty(),
            "every real clip failed to decode/transcribe -- nothing to score (see warnings above)"
        );

        let wer_harness = args
            .wer_harness
            .canonicalize()
            .with_context(|| format!("resolving {}", args.wer_harness.display()))?;

        let engine_name = model_id.filename().to_string();
        let config_label = args
            .config_label
            .clone()
            .unwrap_or_else(|| format!("local {}, bias layer 2{}", model_id.filename(), if args.use_dictionary { " + user dictionary" } else { "" }));

        let output = run_wer_harness(&manifest_json, &hypotheses, &wer_harness, &engine_name, &config_label)?;
        print!("{output}");
        Ok(())
    }

    /// Writes the manifest + hypotheses to a temp dir, generates a tiny
    /// runner `.mjs` that imports the REAL `wer.ts` functions by absolute
    /// path and calls them, runs it under `tsx`, and returns its captured
    /// stdout (the results table + summary lines this fn's caller prints
    /// verbatim). Nothing here computes WER or recall itself.
    fn run_wer_harness(
        manifest_json: &str,
        hypotheses: &[(String, String)],
        wer_harness: &Path,
        engine_name: &str,
        config_label: &str,
    ) -> Result<String> {
        let session = format!(
            "textify-voice-bench-score-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(session);
        fs::create_dir_all(&dir).context("creating temp dir for the wer.ts run")?;

        let manifest_path = dir.join("manifest.json");
        fs::write(&manifest_path, manifest_json).context("writing temp manifest.json")?;

        let hyp_map: std::collections::BTreeMap<&str, &str> =
            hypotheses.iter().map(|(id, text)| (id.as_str(), text.as_str())).collect();
        let hyp_json = serde_json::to_string_pretty(&hyp_map).context("serializing hypotheses")?;
        let hyp_path = dir.join("hypotheses.json");
        fs::write(&hyp_path, hyp_json).context("writing temp hypotheses.json")?;

        let wer_harness_str = wer_harness.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        let runner = format!(
            r#"import {{ readFileSync }} from "node:fs";
import {{ realClips, corpusWer, hardSliceRecall, formatResultsTable }} from "{wer_harness_str}";

const [, , manifestPath, hypothesesPath, engineName, configLabel] = process.argv;
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
const hypothesesRaw = JSON.parse(readFileSync(hypothesesPath, "utf8"));
const hypotheses = new Map(Object.entries(hypothesesRaw));

const clips = realClips(manifest);
if (clips.length === 0) {{
  console.error("no non-placeholder clips in manifest -- nothing to score");
  process.exit(1);
}}

const scored = clips.filter((c) => hypotheses.has(c.id)).length;
const {{ microWer }} = corpusWer(clips, hypotheses);
const recall = hardSliceRecall(clips, hypotheses);

console.log(`clips scored: ${{scored}}/${{clips.length}}`);
console.log(`corpus micro-WER: ${{(microWer * 100).toFixed(1)}}%`);
console.log("");
console.log(formatResultsTable([{{ engineName, configLabel, microWer, hardSliceRecall: recall }}]));
"#
        );
        let runner_path = dir.join("run.mjs");
        fs::write(&runner_path, runner).context("writing temp runner script")?;

        let result = ProcessCommand::new("tsx")
            .arg(&runner_path)
            .arg(&manifest_path)
            .arg(&hyp_path)
            .arg(engine_name)
            .arg(config_label)
            .output();

        let output = match result {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let _ = fs::remove_dir_all(&dir);
                bail!(
                    "`tsx` was not found on PATH -- bench score shells out to it to run the \
                     real fixtures/voice/wer.ts (see this fn's doc comment for why). Install it \
                     with `npm i -g tsx` or ensure it's reachable on PATH, then retry."
                );
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                return Err(e).context("spawning tsx");
            }
        };
        let _ = fs::remove_dir_all(&dir);

        if !output.status.success() {
            bail!(
                "wer.ts scoring run failed (tsx exit status {:?}):\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if !output.stderr.is_empty() {
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            corpus_version: "0.0.0-test".to_string(),
            notes: None,
            clips: vec![Clip {
                id: "proper-noun-01".to_string(),
                audio_path: "audio/proper-noun-01.wav".to_string(),
                reference_text: "Please forward the contract to Farrukh.".to_string(),
                language: "en-US".to_string(),
                hard_slice_tags: vec!["proper-noun".to_string()],
                bias_terms: vec![BiasTermEntry {
                    term: "Farrukh".to_string(),
                    kind: Some("person-name".to_string()),
                }],
                speaker_accent: None,
                duration_s: Some(3.2),
                placeholder: false,
                notes: None,
            }],
        }
    }

    #[test]
    fn manifest_round_trips_through_json_with_exactly_the_schema_required_fields() {
        let manifest = sample_manifest();
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(value.get("corpus_version").is_some());
        assert!(value.get("clips").is_some());
        let clip = &value["clips"][0];
        for required in [
            "id",
            "audio_path",
            "reference_text",
            "language",
            "hard_slice_tags",
            "bias_terms",
            "placeholder",
        ] {
            assert!(clip.get(required).is_some(), "missing required field {required:?}");
        }
        // Optional fields that were None must be OMITTED, not null --
        // additionalProperties:false schemas are fine with omission but a
        // stray `"notes": null` would still be a present key of the wrong
        // type against the schema's `"type": "string"`.
        assert!(clip.get("speaker_accent").is_none());
        assert!(clip.get("notes").is_none());

        let round_tripped: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.clips[0].id, "proper-noun-01");
        assert_eq!(round_tripped.clips[0].reference_text, manifest.clips[0].reference_text);
    }

    #[test]
    fn prompts_json_parses_and_every_id_is_unique() {
        let prompts = load_prompts(Path::new("../../fixtures/voice/prompts/prompts.json"))
            .expect("the real, checked-in prompt set must parse");
        assert!(prompts.len() >= 20, "expected a real corpus, got {}", prompts.len());
        let ids: HashSet<&str> = prompts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.len(), prompts.len(), "duplicate prompt id present");
    }

    #[test]
    fn prompts_json_covers_every_hard_slice_tag_and_every_kind() {
        let prompts = load_prompts(Path::new("../../fixtures/voice/prompts/prompts.json")).unwrap();
        for tag in ["proper-noun", "code-identifier", "accented-en", "whispered"] {
            assert!(
                prompts.iter().any(|p| p.hard_slice_tags.iter().any(|t| t == tag)),
                "no prompt carries hard_slice_tag {tag:?}"
            );
        }
        for kind in [PromptKind::Dictation, PromptKind::Command, PromptKind::Adversarial] {
            assert!(prompts.iter().any(|p| p.kind == kind), "no prompt of kind {kind:?}");
        }
    }

    #[test]
    fn relative_audio_path_produces_the_documented_default_shape() {
        let manifest = Path::new("fixtures/voice/manifest.json");
        let audio_dir = Path::new("fixtures/voice/audio");
        let rel = relative_audio_path(audio_dir, manifest, "whispered-01.wav").unwrap();
        assert_eq!(rel, "audio/whispered-01.wav");
    }

    #[test]
    fn relative_audio_path_falls_back_to_the_full_path_when_not_nested() {
        let manifest = Path::new("/somewhere/else/manifest.json");
        let audio_dir = Path::new("/completely/different/audio");
        let rel = relative_audio_path(audio_dir, manifest, "x.wav").unwrap();
        assert_eq!(rel, "/completely/different/audio/x.wav");
    }

    #[test]
    fn load_or_init_manifest_of_a_missing_path_is_a_fresh_empty_manifest_not_an_error() {
        let path = std::env::temp_dir().join(format!(
            "textify-voice-bench-test-missing-{}.json",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let manifest = load_or_init_manifest(&path, "9.9.9-test").expect("must not error");
        assert_eq!(manifest.corpus_version, "9.9.9-test");
        assert!(manifest.clips.is_empty());
    }

    #[test]
    fn save_then_load_manifest_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "textify-voice-bench-test-roundtrip-{}.json",
            std::process::id()
        ));
        let manifest = sample_manifest();
        save_manifest(&path, &manifest).expect("save");
        let loaded = load_or_init_manifest(&path, "unused").expect("load back");
        assert_eq!(loaded.corpus_version, manifest.corpus_version);
        assert_eq!(loaded.clips.len(), 1);
        assert_eq!(loaded.clips[0].id, "proper-noun-01");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn quality_guard_thresholds_flag_silence_and_clipping_from_real_compute_stats_output() {
        // Silence: all-zero PCM.
        let silent = vec![0i16; 16_000];
        let stats = compute_stats(&silent);
        assert!(stats.peak_amplitude < record::SILENCE_PEAK_THRESHOLD);

        // Clipping: PCM pinned at full scale.
        let clipped = vec![32_767i16; 16_000];
        let stats = compute_stats(&clipped);
        assert!(stats.peak_amplitude >= record::CLIPPING_PEAK_THRESHOLD);

        // A normal-looking tone should trip neither guard.
        let tone: Vec<i16> = (0..16_000)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                ((2.0 * std::f64::consts::PI * 220.0 * t).sin() * 10_000.0) as i16
            })
            .collect();
        let stats = compute_stats(&tone);
        assert!(stats.peak_amplitude >= record::SILENCE_PEAK_THRESHOLD);
        assert!(stats.peak_amplitude < record::CLIPPING_PEAK_THRESHOLD);
    }

    #[test]
    fn load_prompts_rejects_duplicate_ids() {
        let path = std::env::temp_dir().join(format!(
            "textify-voice-bench-test-dupe-{}.json",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"{"prompts":[
                {"id":"a","kind":"dictation","text":"hi"},
                {"id":"a","kind":"dictation","text":"there"}
            ]}"#,
        )
        .unwrap();
        let err = load_prompts(&path).unwrap_err();
        assert!(format!("{err:#}").contains("appears more than once"));
        fs::remove_file(&path).ok();
    }
}
