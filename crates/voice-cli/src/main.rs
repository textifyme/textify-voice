//! `textify-voice` — the MVP CLI. Wires the tested-but-previously-stubbed
//! voice crates into a binary the founder can actually run on this Mac:
//! real audio in, real local ASR, real text out.
//!
//! Subcommands:
//! - `transcribe <FILE>`  — fully verifiable: file -> decode -> ASR ->
//!   normalize -> stdout. No mic, no OS permissions needed.
//! - `dictate`             — the real product loop (mic + global hotkey +
//!   insertion). Requires macOS TCC grants this CLI checks for up front and
//!   refuses to half-run without.
//! - `command "<utterance>"` — dry-run demo of the Command Mode spine
//!   (voice-intent -> voice-act -> tier gate). Never touches the OS.
//! - `models`              — whisper.cpp model cache management.
//! - `bench record` / `bench score` — WP-V0.0 / COMMANDS-SPEC C0.0 corpus
//!   recording and scoring.
//! - `settings` / `onboarding` — open the same native windows the menu-bar
//!   agent's "Settings…" item and first-run wizard open, from a terminal.
//!
//! ## One binary, two faces
//!
//! This binary is also what `packaging/build-bundle.sh` puts inside
//! `Textify Voice.app` unmodified — no wrapper script. Launched with an
//! explicit subcommand (from a terminal, or a script), it behaves exactly
//! as documented above. Launched with **zero** `argv` — which is exactly
//! how LaunchServices starts a double-clicked/Finder/Dock `.app` — `main`
//! below intercepts that shape *before* `Cli::parse()` (which has no
//! `<COMMAND>` default and would otherwise hard-error, see
//! `packaging/README.md`'s "One binary, two faces" for the reproduced
//! `exit=2` this replaces) and hands off to `dictate::run_agent`: the
//! persistent menu-bar agent loop (onboarding on first run, then arm
//! dictation and track real state in the status item — see
//! `dictate.rs`'s `run_agent`/`run_agent_macos` for the implementation).

mod compat;
mod platform;

#[cfg(target_os = "macos")]
mod holdkey;
#[cfg(target_os = "macos")]
mod login_item;
#[cfg(target_os = "macos")]
mod hud;
#[cfg(target_os = "macos")]
mod menubar;
#[cfg(target_os = "macos")]
mod sound;
mod bench;
mod clipboard;
mod command;
mod common;
mod dictate;
mod dictionary;
mod diagnostics;
mod models;
mod onboarding;
#[cfg(target_os = "macos")]
mod onboarding_window;
mod paste;
mod permissions;
mod settings;
mod transcribe;
mod update;

use clap::{Parser, Subcommand};

/// textify-voice — local, real-time dictation and a safe Command Mode demo.
///
/// Permissions this binary may need, depending on the subcommand:
///   - `transcribe` / `command` / `models`: none. These never touch the
///     microphone, Accessibility APIs, or the clipboard.
///   - `dictate`: Microphone (System Settings -> Privacy & Security ->
///     Microphone), and Accessibility (System Settings -> Privacy & Security
///     -> Accessibility) for the global hotkey and (with `--paste`) the
///     synthesized keystroke. `dictate` checks both at startup and prints
///     exactly what's missing rather than half-running.
#[derive(Parser, Debug)]
#[command(name = "textify-voice", version, about, long_about = None)]
struct Cli {
    /// Show stage timings (capture -> asr -> normalize -> insert) on
    /// stderr. SPEC.md's latency budget is the whole point of this MVP --
    /// this flag is how you see where the time actually goes.
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Transcribe an audio file end to end through the real local pipeline
    /// (decode -> whisper.cpp -> bias layer 2 + normalizer). Fully
    /// verifiable, no permissions required.
    Transcribe(transcribe::TranscribeArgs),

    /// Run the live dictation loop: global hotkey -> mic capture -> ASR ->
    /// normalize -> insert. Requires Microphone + Accessibility. Reads a
    /// user dictionary of proper nouns/jargon and custom substitutions from
    /// `~/Library/Application Support/textify/dictionary.txt` on macOS
    /// (override with `TEXTIFY_VOICE_DICTIONARY_PATH`), created with a
    /// commented starter example on first run if nothing exists yet --
    /// edit it directly, see the file's own comments for the format.
    Dictate(dictate::DictateArgs),

    /// Dry-run the Command Mode spine on one utterance. Prints the matched
    /// schema, resolved target, effective tier, and gate decision. NEVER
    /// performs any OS action, regardless of tier.
    Command(command::CommandArgs),

    /// Manage the local whisper.cpp model cache (list / download / show path).
    Models(models::ModelsArgs),

    /// Report this Mac's device tier -- architecture, chip, physical core
    /// split, RAM, macOS version, and whether this build includes the
    /// Metal-accelerated whisper.cpp path (SPEC.md §3.1's device/tier
    /// detection; see `compat.rs`). Always runs, even on hardware the
    /// startup gate below would otherwise refuse, since explaining *why*
    /// a machine is unsupported is exactly what this is for.
    DeviceTier,

    /// Record and score the WP-V0.0 / COMMANDS-SPEC C0.0 bench corpora
    /// (DECISIONS.md D2): `bench record` prompts + records real takes into
    /// `fixtures/voice/manifest.json`; `bench score` runs them through the
    /// real local ASR pipeline and the existing `fixtures/voice/wer.ts`
    /// harness.
    Bench(bench::BenchArgs),

    /// Open the Settings window (hold key, mode, paste vs. clipboard, HUD,
    /// sound, model) — the same window the menu-bar agent's "Settings…"
    /// menu item opens. Persists to
    /// `~/Library/Application Support/textify/settings.txt`.
    Settings,

    /// Run the first-run onboarding wizard (Microphone, Accessibility,
    /// model download) manually — the same wizard the menu-bar agent shows
    /// automatically until it's `Ready`.
    Onboarding,

    /// Write one local diagnostic bundle (log, recent crash reports,
    /// version, permissions, settings, device tier) to a file you can
    /// read before deciding whether to share it with anyone. Never sends
    /// anything anywhere by itself; `--enable-upload`/`--disable-upload`
    /// toggle the separate, off-by-default opt-in a future release may
    /// use. See `diagnostics.rs`.
    Diagnostics(diagnostics::DiagnosticsArgs),

    /// Check for a newer release right now over HTTPS and print the
    /// result -- up to date, an available version, or why the check
    /// failed (exit code 1 in that last case). Never downloads or
    /// installs anything by itself: that only happens from the menu-bar
    /// agent's "Check for Updates…" item, where a human decides to
    /// trigger it. See `update.rs`'s module doc for the appcast/
    /// signature design and `dictate::run_update_check` for this
    /// subcommand's implementation. Exempt from the compat gate below,
    /// same as `device-tier`/`diagnostics`: a machine the gate blocks may
    /// still want to know whether a fixed build exists.
    UpdateCheck,
}

fn main() {
    // First statement, unconditionally, ahead of even the zero-argv agent
    // branch below: a crash during onboarding or the menu-bar agent's own
    // startup must be captured too. See `diagnostics.rs`'s module doc for
    // why this is local-only (no network, no consent needed) and safe to
    // install this early.
    diagnostics::install_panic_hook();

    // Launched as the app (double-click, Spotlight, `open -a`, Dock):
    // LaunchServices invokes `Contents/MacOS/<CFBundleExecutable>` with no
    // arguments at all -- `argv` is just `[executable_path]`. That is the
    // one shape a human typing an explicit subcommand never produces, and
    // it must be intercepted *before* `Cli::parse()`: `Cli::command` has no
    // default and clap hard-errors (exit 2) on a missing `<COMMAND>`. See
    // this file's module doc and `packaging/README.md`'s "One binary, two
    // faces" for the reproduced gap this closes.
    if std::env::args().count() == 1 {
        // Zero-arg agent mode always goes through `dictate::run_agent`, which
        // needs the mic/ASR path -- so the compat gate applies here too, not
        // just to explicit subcommands. See `compat.rs`.
        if let compat::StartupCheck::Blocked(reason) = compat::check_startup() {
            eprintln!("{reason}");
            std::process::exit(1);
        }

        diagnostics::set_active_subsystem(diagnostics::Subsystem::Dictate);
        let result = dictate::run_agent(false);
        std::process::exit(match result {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("Error: {err:#}");
                1
            }
        });
    }

    let cli = Cli::parse();

    // `device-tier` and `diagnostics` are deliberately exempt: both exist
    // to *explain* a machine, including an unsupported one, so both must
    // still run on one. Every other subcommand is gated -- see
    // `compat.rs`'s module doc for why this check is architecture-then-floor
    // and what each case means.
    if !matches!(cli.command, Cmd::DeviceTier | Cmd::Diagnostics(_) | Cmd::UpdateCheck) {
        if let compat::StartupCheck::Blocked(reason) = compat::check_startup() {
            eprintln!("{reason}");
            std::process::exit(1);
        }
    }

    diagnostics::set_active_subsystem(match &cli.command {
        Cmd::Transcribe(_) => diagnostics::Subsystem::Transcribe,
        Cmd::Dictate(_) => diagnostics::Subsystem::Dictate,
        Cmd::Command(_) => diagnostics::Subsystem::Command,
        Cmd::Models(_) => diagnostics::Subsystem::Models,
        Cmd::DeviceTier => diagnostics::Subsystem::Unknown,
        Cmd::Bench(_) => diagnostics::Subsystem::Bench,
        Cmd::Settings => diagnostics::Subsystem::Settings,
        Cmd::Onboarding => diagnostics::Subsystem::Onboarding,
        Cmd::Diagnostics(_) => diagnostics::Subsystem::Diagnostics,
        // No dedicated `Subsystem` variant exists for the updater
        // (`diagnostics.rs` is outside this unit's owns-list) -- `Unknown`
        // is the same reuse `DeviceTier` above already makes for an
        // administrative, non-pipeline command.
        Cmd::UpdateCheck => diagnostics::Subsystem::Unknown,
    });

    let result = match cli.command {
        Cmd::Transcribe(args) => transcribe::run(args, cli.verbose),
        Cmd::Dictate(args) => dictate::run(args, cli.verbose),
        Cmd::Command(args) => command::run(args),
        Cmd::Models(args) => models::run(args),
        Cmd::DeviceTier => compat::run_device_tier(),
        Cmd::Bench(args) => bench::run(args, cli.verbose),
        Cmd::Settings => settings::open_settings_window(),
        Cmd::Onboarding => onboarding::open_onboarding_window(),
        Cmd::Diagnostics(args) => diagnostics::run(args),
        Cmd::UpdateCheck => dictate::run_update_check(),
    };

    if let Err(err) = result {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
