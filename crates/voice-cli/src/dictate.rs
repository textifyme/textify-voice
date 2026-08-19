//! `textify-voice dictate` — the real product loop: hold a bare modifier
//! (Option by default), a floating waveform shows you it is hearing you,
//! release, and the text lands where you were typing.
//!
//! Threading is the shape of this file, and it is not incidental:
//!
//! * **Main thread** owns AppKit (the HUD panel) and the `CGEventTap` run
//!   loop, and performs insertion. AppKit is main-thread-only, and the tap
//!   delivers to whichever run loop installed it.
//! * **Worker thread** owns the whisper engine and the mic stream. Whisper
//!   blocks for a few hundred ms; running it on the main thread would freeze
//!   the waveform mid-utterance and — worse — starve the event tap, which the
//!   OS then silently disables for being slow.
//! * **Audio callback thread** (cpal's) only appends PCM and stores one
//!   atomic level. It must never block, so it never takes the HUD's path.
//!
//! NOT VERIFIABLE IN THIS ENVIRONMENT. Every piece below is real code
//! against real crates (`crate::holdkey`'s CGEventTap, `voice-audio::MicCapture`,
//! `voice-asr-whisper::WhisperLocalAsr`, `arboard`, and -- for `--paste` --
//! `objc2-core-graphics` `CGEvent` synthesis), not a mock or a stub. But
//! running it end to end requires two macOS TCC grants (Microphone,
//! Accessibility) this sandboxed agent session does not have and cannot
//! grant, plus an attached audio device and a live desktop session. This
//! command's startup permission check (see `crate::permissions`) refuses to
//! proceed past that gate, so the live loop below has compiled but never
//! actually run on real hardware in this session. See this crate's README
//! for exactly what the founder needs to grant, and how to test it after
//! granting it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Args, ValueEnum};

use voice_core::insertion::insert_text;
use voice_core::{
    default_literal_rules, normalize, AppKind, BiasContext, BiasTerm, CorrectionThresholds,
    InsertionBackend, InsertionError, InsertionMethod, InsertionTarget, LiteralRule, WordSpan,
};

use crate::common::{context_app_kind_to_core, ModelArg};
use crate::permissions;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum DictateMode {
    /// Hold the key to talk; release ends the utterance immediately
    /// (SPEC.md 3.1: "PTT: key-up = endpoint, no VAD wait"). Default.
    #[default]
    Ptt,
    /// Tap once to start listening, tap again to stop. Manual stop only in
    /// this MVP -- `voice-audio` ships a real VAD-driven auto-endpoint
    /// (`ToggleCapturePipeline`); wiring it in is a follow-up.
    Toggle,
}

/// Hold a bare modifier, speak, release: text lands where you were typing.
///
/// User dictionary: proper nouns and jargon that bias-layer-2 should
/// correct toward, plus custom literal substitutions, are read from a plain
/// text file at `~/Library/Application Support/textify/dictionary.txt` on
/// macOS (override with the `TEXTIFY_VOICE_DICTIONARY_PATH` environment
/// variable). A commented starter file with two working example entries is
/// created there automatically the first time you run `dictate` (or
/// `transcribe`) if nothing exists yet -- edit it directly, in the format
/// its own comments describe; changes take effect the next time you start
/// `dictate`.
#[derive(Args, Debug)]
pub struct DictateArgs {
    #[arg(long, value_enum, default_value_t = DictateMode::Ptt)]
    pub mode: DictateMode,

    /// Which bare modifier to hold while talking. A lone modifier cannot be
    /// expressed as a `global-hotkey` chord, so each platform serves this from
    /// its own input backend (see `crate::platform`).
    #[arg(long, value_enum, default_value_t = crate::platform::HoldKey::LeftOption)]
    pub hold_key: crate::platform::HoldKey,

    /// Suppress the floating waveform panel and run headless.
    #[arg(long)]
    pub no_hud: bool,

    /// Suppress the press/release tones.
    #[arg(long)]
    pub no_sound: bool,

    /// Synthesize a ⌘V keystroke after writing to the clipboard. Requires
    /// the Accessibility grant. Off by default.
    #[arg(long, conflicts_with = "clipboard_only")]
    pub paste: bool,

    /// Explicit, redundant-with-the-default spelling of "do not synthesize
    /// a keystroke" -- accepted so `--clipboard-only` and `--paste` read as
    /// the mutually exclusive pair the spec describes.
    #[arg(long, conflicts_with = "paste")]
    pub clipboard_only: bool,

    #[arg(long, value_enum, default_value_t = ModelArg::BaseEn)]
    pub model: ModelArg,
}

pub fn run(args: DictateArgs, verbose: bool) -> Result<()> {
    println!("textify-voice dictate -- mode={:?}  paste={}", args.mode, args.paste);
    println!();
    println!("Checking permissions...");
    let report = permissions::check();
    report.print();
    println!();

    if !report.all_granted() {
        anyhow::bail!(
            "one or more required permissions are missing (see above). Grant them in System \
             Settings, then re-run `textify-voice dictate`. Refusing to half-run: registering a \
             input tap without Accessibility, or opening a mic stream without Microphone access, \
             would silently do nothing when you press the key rather than failing loudly."
        );
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = verbose;
        anyhow::bail!("textify-voice dictate's live loop is only implemented for macOS in this MVP build");
    }

    #[cfg(target_os = "macos")]
    {
        run_macos(args, verbose)
    }
}

// ---------------------------------------------------------------------
// Auto-update wiring
// ---------------------------------------------------------------------
//
// `crate::update` (see that module's own doc comment) owns appcast
// parsing, ed25519 verification, download/stage, and the swap-and-
// relaunch script -- everything that does not need to know this crate's
// menu bar, settings, or CLI exist. What's here is the wiring that
// connects those portable primitives to a running `textify-voice`: where
// the appcast lives, how often the menu-bar agent checks automatically,
// and the `update-check` CLI subcommand `main.rs` dispatches to. The
// menu-bar agent's own use of these primitives -- the background
// checker, the "Check for Updates…" menu item, and download + install +
// relaunch on click -- lives in `run_agent_macos` below, since that is
// where the menu bar and a running run loop actually exist.

/// Overrides where the menu-bar agent's background checker and the
/// `update-check` CLI subcommand fetch the appcast from. Mirrors
/// `voice_asr_whisper::model::MODEL_BASE_URL_ENV_VAR`'s convention: an
/// env var, not a CLI flag, so the menu-bar agent -- which takes no
/// flags at all -- can be pointed at a staging appcast too (e.g. before
/// a real release is cut).
pub const UPDATE_APPCAST_URL_ENV_VAR: &str = "TEXTIFY_UPDATE_APPCAST_URL";

/// Where this build fetches its appcast from (see
/// `packaging/appcast/README.md`'s "Cutting a release" workflow for how the
/// file at the other end is produced).
///
/// Note the domain: `textify.me`, not `textify.app`. Earlier drafts of the
/// appcast template and the model-mirror docs said `.app`; that was never a
/// domain this project owns.
///
/// `Some(..)` rather than `None` is a commitment, not a default. It must not
/// be set until something is actually hosted there: a hostname that 404s
/// makes every background check fail, and a failed check is a message in the
/// user's menu bar every few hours about a problem they cannot do anything
/// about -- an updater that is *not configured* is a different state from one
/// that is *broken*, and the UI says so (see `run_agent_macos`). Setting this
/// is also what arms `update::key_guard`'s check that a real signing key ships
/// alongside it.
///
/// Every appcast fetch refuses a non-`https://` URL before dialing
/// (`update::require_https`) regardless of what this constant says.
pub(crate) const DEFAULT_UPDATE_APPCAST_URL: Option<&str> =
    Some("https://downloads.textify.me/voice/appcast.json");

/// Effective appcast URL: [`UPDATE_APPCAST_URL_ENV_VAR`] if set to a
/// non-empty value, [`DEFAULT_UPDATE_APPCAST_URL`] otherwise, and `None`
/// when this build has neither -- meaning update checking is switched off
/// entirely, not merely failing.
pub fn update_appcast_url() -> Option<String> {
    std::env::var(UPDATE_APPCAST_URL_ENV_VAR)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| DEFAULT_UPDATE_APPCAST_URL.map(ToString::to_string))
}

/// What the menu bar's update row and the `update-check` subcommand say
/// when this build has no appcast to check against. Deliberately not
/// phrased as an error: nothing failed.
pub(crate) const UPDATE_NOT_CONFIGURED_MESSAGE: &str =
    "automatic updates are not configured in this build";

/// How often the menu-bar agent re-checks in the background when
/// `Settings::update_check_enabled` is on. Deliberately more frequent
/// than most desktop apps' once-a-day default: this unit's own dispatch
/// names shipping with no update path at all as the top-priority gap a
/// beta cannot ship with ("one hour of real use... surfaced six
/// user-visible bugs"), so a fix should be able to reach a running
/// install within hours, not a day.
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// `textify-voice update-check` -- perform exactly one appcast check
/// right now and print the result. Never downloads or installs anything
/// (that's `update::download_and_verify`/`stage_update`, wired only into
/// the menu-bar agent's "Check for Updates…" click handler below, where
/// a human decides to trigger it) -- this exists so "is an update
/// available" is answerable from a terminal or a script, without the
/// GUI, per this unit's dispatch.
pub fn run_update_check() -> Result<()> {
    let Some(url) = update_appcast_url() else {
        // Not an error: there is nothing to check against, and saying so is
        // the honest answer. Set `UPDATE_APPCAST_URL_ENV_VAR` to point at a
        // staging appcast before one is hosted for real.
        println!("{UPDATE_NOT_CONFIGURED_MESSAGE}.");
        println!(
            "set {UPDATE_APPCAST_URL_ENV_VAR}=https://<host>/appcast.json to check against one."
        );
        return Ok(());
    };
    println!(
        "checking {url} for updates (current version {})...",
        crate::update::Version::current()
    );
    match crate::update::check_now(&url) {
        crate::update::UpdateState::Failed(msg) => anyhow::bail!("{msg}"),
        state => {
            println!("{state}");
            Ok(())
        }
    }
}

/// Main -> worker.
#[cfg(target_os = "macos")]
enum ToWorker {
    Start,
    /// Endpoint and transcribe what has been captured.
    Stop,
    /// Throw the in-flight utterance away without transcribing.
    Cancel,
}

/// Worker -> main.
#[cfg(target_os = "macos")]
enum FromWorker {
    Ready { device: String, sample_rate: u32, channels: u16 },
    StartFailed(String),
    /// Capture began; the HUD can go live.
    Listening,
    /// Transcription finished. Insertion happens on the main thread.
    Text { text: String, capture: Duration, asr: Duration, normalize: Duration },
    /// The key was released before a single audio callback landed.
    NoAudio,
    Failed(String),
}

/// Print every capability this platform lacks (`PlatformCaps::gaps`), if
/// any, BEFORE arming anything. Shared by `run_macos` (the CLI `dictate`
/// loop) and `run_agent_macos` (the menu-bar agent loop) so both report
/// identically -- a capability we lack must be visible, and that rule does
/// not change depending on how the loop was entered.
#[cfg(target_os = "macos")]
fn print_platform_gaps(caps: &crate::platform::PlatformCaps) {
    for gap in caps.gaps() {
        println!("note: {gap}");
    }
    if !caps.gaps().is_empty() {
        println!();
    }
}

/// Load the user dictionary, reporting parse warnings and a one-line
/// summary the same way for every live-loop entry point. Falls back to an
/// empty dictionary (never blocks startup) if the file can't be read.
#[cfg(target_os = "macos")]
fn load_dictionary_reporting() -> crate::dictionary::Dictionary {
    match crate::dictionary::load_or_seed_default() {
        Ok(d) => {
            for err in &d.errors {
                eprintln!("dictionary warning: {err}");
            }
            let path = crate::dictionary::default_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unresolved>".to_string());
            if d.is_empty() {
                println!("dictionary: {path} has no entries yet -- edit it to add proper nouns or jargon");
            } else {
                println!(
                    "dictionary: {} term(s), {} literal rule(s) loaded from {path}",
                    d.terms.len(),
                    d.literal_rules.len()
                );
            }
            d
        }
        Err(e) => {
            eprintln!("[user dictionary unavailable: {e:#} -- continuing without it]");
            crate::dictionary::Dictionary::default()
        }
    }
}

/// Start the menu-bar agent's background updater: one immediate check off
/// this thread (so the agent does not silently wait out a whole
/// [`UPDATE_CHECK_INTERVAL`] before ever checking -- `spawn_background_
/// checker`'s own doc comment is explicit that it does not fire
/// immediately), then the real interval-based checker. Both push every
/// state onto `tx`, which `run_agent_macos`'s loop polls the same
/// non-blocking way it already polls `from_rx`/`status_ui.poll_events()`.
/// Returns the background thread's handle (not joined -- see the call
/// site) and a fresh stop flag; `run_agent_macos` calls this again with
/// a fresh flag if auto-checking is turned back on after being turned
/// off, since a flag already set to stop cannot be reused to start a new
/// checker.
#[cfg(target_os = "macos")]
fn spawn_update_checker(
    appcast_url: String,
    tx: &std::sync::mpsc::Sender<crate::update::UpdateState>,
) -> (std::thread::JoinHandle<()>, Arc<std::sync::atomic::AtomicBool>) {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let tx0 = tx.clone();
    let immediate_url = appcast_url.clone();
    std::thread::spawn(move || {
        let _ = tx0.send(crate::update::check_now(&immediate_url));
    });

    let tx1 = tx.clone();
    let handle = crate::update::spawn_background_checker(
        appcast_url,
        UPDATE_CHECK_INTERVAL,
        Arc::clone(&stop),
        move |state| {
            let _ = tx1.send(state);
        },
    );
    (handle, stop)
}

/// Start the background updater only if this build actually has an appcast
/// to check ([`update_appcast_url`]) *and* the user hasn't turned automatic
/// checking off. `None` means no checker thread exists at all -- so an
/// unconfigured build makes no network calls and, crucially, never pushes a
/// `Failed` state into the menu bar for a check it was never able to make.
#[cfg(target_os = "macos")]
fn spawn_update_checker_if_configured(
    enabled: bool,
    tx: &std::sync::mpsc::Sender<crate::update::UpdateState>,
) -> Option<(std::thread::JoinHandle<()>, Arc<std::sync::atomic::AtomicBool>)> {
    if !enabled {
        return None;
    }
    let url = update_appcast_url()?;
    Some(spawn_update_checker(url, tx))
}

/// Download, verify, and stage one appcast item -- the blocking part of
/// "Check for Updates…" clicked while an update is known to be
/// available. Runs on a background thread (spawned at the call site in
/// `run_agent_macos`); every intermediate [`crate::update::UpdateState::
/// Downloading`] tick is pushed onto `tx` as it happens (via the
/// `progress` callback `download_and_verify` already supports), and the
/// final [`crate::update::UpdateState`] (`ReadyToRelaunch` or `Failed`)
/// is returned for the caller to push once more. Never touches the
/// installed app -- see `crate::update::stage_update`'s own doc comment.
#[cfg(target_os = "macos")]
fn download_and_stage_update(
    item: &crate::update::AppcastItem,
    tx: &std::sync::mpsc::Sender<crate::update::UpdateState>,
) -> crate::update::UpdateState {
    let current = crate::update::Version::current();
    let verifier = match crate::update::Verifier::from_compiled_in() {
        Ok(v) => v,
        Err(e) => return crate::update::UpdateState::Failed(e.to_string()),
    };
    let Some(staging_dir) = crate::update::default_updates_dir() else {
        return crate::update::UpdateState::Failed(
            "could not determine where to stage the downloaded update".to_string(),
        );
    };
    let mut progress = |downloaded: u64, total: u64| {
        let _ = tx.send(crate::update::UpdateState::Downloading { downloaded, total });
    };
    let verified = match crate::update::download_and_verify(
        item,
        &current,
        &verifier,
        &staging_dir,
        Some(&mut progress),
    ) {
        Ok(p) => p,
        Err(e) => return crate::update::UpdateState::Failed(e.to_string()),
    };
    let dest_dir = staging_dir.join(format!("staged-{}", item.version));
    match crate::update::stage_update(&verified, &dest_dir) {
        Ok(staged_app) => crate::update::UpdateState::ReadyToRelaunch { staged_app },
        Err(e) => crate::update::UpdateState::Failed(e.to_string()),
    }
}

/// "Check for Updates…" clicked while a download has already finished
/// and staged: write the swap-and-relaunch helper script (`crate::update
/// ::build_relaunch_script`, the Sparkle-precedent out-of-process helper
/// -- see that module's doc comment for why a running app cannot swap
/// its own bundle) and spawn it, detached, waiting on *this* process's
/// pid. The caller is responsible for exiting shortly after this returns
/// `Ok` -- see the call site in `run_agent_macos`, which returns from
/// the agent loop immediately afterward, the same clean-shutdown path
/// `StatusUiEvent::Quit` already uses.
#[cfg(target_os = "macos")]
fn install_staged_update_and_relaunch(staged_app: &std::path::Path) -> Result<()> {
    let installed = crate::update::current_bundle_root().ok_or_else(|| {
        anyhow::anyhow!("not running from inside an installed .app bundle -- nothing to swap")
    })?;
    let relaunch_cmd = crate::update::default_relaunch_cmd(&installed);
    let script = crate::update::build_relaunch_script(
        std::process::id(),
        &installed,
        staged_app,
        &relaunch_cmd,
    );
    let updates_dir = crate::update::default_updates_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine the updates directory"))?;
    std::fs::create_dir_all(&updates_dir)?;
    let script_path = updates_dir.join("relaunch.sh");
    crate::update::spawn_relaunch_helper(&script, &script_path)?;
    Ok(())
}

/// Spawn the worker thread (whisper model load + mic open) and block until
/// it reports ready, or a startup failure. Shared by `run_macos` and
/// `run_agent_macos` so both start (and, for the agent, restart on a
/// settings change) a live session identically; see `dictate_worker`'s doc
/// comment for why this takes `model` on its own rather than a whole
/// `DictateArgs`.
#[cfg(target_os = "macos")]
#[allow(clippy::type_complexity)]
fn spawn_worker(
    model: ModelArg,
    verbose: bool,
    dictionary: crate::dictionary::Dictionary,
    level_bits: Arc<std::sync::atomic::AtomicU32>,
    context_provider: Arc<voice_context::MacosContextProvider>,
) -> Result<(std::thread::JoinHandle<()>, std::sync::mpsc::Sender<ToWorker>, std::sync::mpsc::Receiver<FromWorker>)> {
    let (to_tx, to_rx) = std::sync::mpsc::channel::<ToWorker>();
    let (from_tx, from_rx) = std::sync::mpsc::channel::<FromWorker>();

    let worker = std::thread::spawn(move || {
        dictate_worker(model, verbose, to_rx, from_tx, level_bits, dictionary, context_provider);
    });

    // Block until the worker has the model loaded and the mic open, so the
    // "ready" line is honest and the first press cannot race model loading.
    match from_rx.recv() {
        Ok(FromWorker::Ready { device, sample_rate, channels }) => {
            println!("mic: {device} @ {sample_rate} Hz / {channels} ch (resampled to 16 kHz mono for ASR)");
        }
        Ok(FromWorker::StartFailed(e)) => anyhow::bail!("{e}"),
        Ok(_) => anyhow::bail!("dictate worker sent an unexpected first message"),
        Err(_) => anyhow::bail!("dictate worker exited before becoming ready"),
    }

    Ok((worker, to_tx, from_rx))
}

/// Build the press/release cues, or `NullCues` if `want_sound` is false --
/// short-circuits before touching AppKit at all in that case (unlike the
/// tuple-match this replaced, which constructed `MacCues` unconditionally
/// and only then discarded it; behavior is identical either way since a
/// discarded `MacCues` is never `.press()`ed, but this skips the wasted
/// sine-sweep synthesis).
#[cfg(target_os = "macos")]
fn build_cues(want_sound: bool) -> Box<dyn crate::platform::Cues> {
    if !want_sound {
        return Box::new(crate::platform::NullCues);
    }
    match crate::platform::macos::MacCues::new() {
        Ok(c) => Box::new(c),
        Err(e) => {
            eprintln!("[audio cues unavailable: {e:#} -- continuing silently]");
            Box::new(crate::platform::NullCues)
        }
    }
}

/// Build the waveform indicator, or `NullIndicator` if `want_hud` is false
/// (the caller is responsible for folding `PlatformCaps::can_overlay` into
/// `want_hud` before calling this, same as `run_macos` always has).
#[cfg(target_os = "macos")]
fn build_hud(want_hud: bool) -> Box<dyn crate::platform::Indicator> {
    if !want_hud {
        return Box::new(crate::platform::NullIndicator);
    }
    match crate::platform::macos::MacIndicator::new() {
        Ok(h) => Box::new(h),
        Err(e) => {
            eprintln!("[indicator unavailable: {e:#} -- continuing without it]");
            Box::new(crate::platform::NullIndicator)
        }
    }
}

#[cfg(target_os = "macos")]
fn run_macos(args: DictateArgs, verbose: bool) -> Result<()> {
    use crate::platform::{self, HoldEvent, HoldKeySource as _};
    use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRunLoop};
    use std::sync::atomic::{AtomicU32, Ordering};

    let paste_enabled = args.paste && !args.clipboard_only;
    let hold_key = args.hold_key;
    let mode = args.mode;
    let want_hud = !args.no_hud;
    let want_sound = !args.no_sound;

    let caps = platform::current_caps();
    // Print what this platform cannot do BEFORE arming anything. A capability
    // we lack must be visible; a dictation tool that silently drops text is
    // worse than one that says it cannot type here.
    print_platform_gaps(&caps);

    // Installed on THIS thread's run loop, which is the one pumped below.
    let tap = platform::macos::MacHoldKey::install(hold_key)?;

    // Written by the audio callback, read by the HUD each frame. An atomic
    // rather than a lock because the callback is on cpal's realtime thread and
    // must never wait on the UI.
    let level_bits = Arc::new(AtomicU32::new(0));

    // User dictionary (SPEC §3.3: "user dictionary ... prior-utterance
    // terms" as BiasContext sources). Loaded ONCE here, before the loop --
    // not per-utterance -- so a file read never sits on the "must never
    // block the first audio frame" path. See `crate::dictionary`'s module
    // doc for the file format; `dictate --help` and this crate's README
    // also point at the path.
    let dictionary = load_dictionary_reporting();

    // Real on-screen context: frontmost app (-> AppKind, drives bias-layer-2
    // and SPEC V1.4's raw-paste rule) and the focused element (secure-field
    // refusal). One long-lived provider, shared (via `Arc`) with the worker
    // thread (which needs AppKind to build each utterance's `BiasContext`)
    // and with `CliInsertionBackend` on this thread (which needs the
    // focused element's secure/writable bits at insertion time). Every
    // `capture()` call is non-blocking by contract (SPEC §3.1) and also
    // kicks a fresh background AX read for next time.
    let context_provider = Arc::new(voice_context::MacosContextProvider::new());

    let (worker, to_tx, from_rx) = spawn_worker(
        args.model,
        verbose,
        dictionary,
        Arc::clone(&level_bits),
        Arc::clone(&context_provider),
    )?;

    let cues: Box<dyn platform::Cues> = build_cues(want_sound);
    let mut hud: Box<dyn platform::Indicator> = build_hud(want_hud && caps.can_overlay);

    // A backend that cannot distinguish key-down from key-up cannot do
    // push-to-talk at all; fall back rather than silently never firing.
    let mode = if matches!(mode, DictateMode::Ptt) && !tap.supports_hold() {
        println!("note: this input backend cannot report key-release -- using toggle mode");
        DictateMode::Toggle
    } else {
        mode
    };
    println!(
        "hold {} to talk ({:?} mode){}",
        hold_key.describe(),
        mode,
        if paste_enabled { ", auto-paste ON" } else { ", clipboard only" }
    );
    println!("Ctrl-C to quit.");
    println!();

    let mut backend = CliInsertionBackend {
        paste_enabled,
        verbose,
        context_provider: Arc::clone(&context_provider),
        // Honest zero-state: nothing has been checked yet, and this is only
        // ever read back after `current_target()` has already set it (from
        // `insert_and_report`, right after `insert_text()` returns).
        // Defaulting to `Unknown` rather than `Known(false)` matches this
        // whole unit's rule that "no answer yet" is never "safe".
        last_status: std::cell::Cell::new(voice_context::SecureFieldStatus::Unknown),
    };
    let mut listening = false;

    // ~60 Hz: pump the run loop briefly (this is what delivers tap callbacks
    // and lets Core Animation commit), then advance the HUD and drain both
    // channels. Never block here -- a stalled main thread means a dead tap.
    let mode_ref = unsafe { kCFRunLoopDefaultMode };
    loop {
        CFRunLoop::run_in_mode(mode_ref, 1.0 / 60.0, false);

        for event in tap.poll() {
            match event {
                HoldEvent::SourceDisabled => {
                    eprintln!("[the OS stopped delivering hold-key events -- re-arming]");
                    tap.re_arm();
                }
                HoldEvent::Down => {
                    let start = match mode {
                        DictateMode::Ptt => !listening,
                        DictateMode::Toggle => !listening,
                    };
                    let stop = matches!(mode, DictateMode::Toggle) && listening;
                    if start {
                        listening = true;
                        // Play before anything else: the ear is the fastest
                        // confirmation that the press registered, and the user
                        // is looking at another window.
                        cues.press();
                        let _ = to_tx.send(ToWorker::Start);
                    } else if stop {
                        listening = false;
                        cues.release();
                        let _ = to_tx.send(ToWorker::Stop);
                        hud.show_transcribing();
                    }
                }
                HoldEvent::Up => {
                    if matches!(mode, DictateMode::Ptt) && listening {
                        listening = false;
                        cues.release();
                        let _ = to_tx.send(ToWorker::Stop);
                        hud.show_transcribing();
                    }
                }
                HoldEvent::Cancel(reason) => {
                    if listening {
                        listening = false;
                        let _ = to_tx.send(ToWorker::Cancel);
                        hud.hide();
                        if verbose {
                            println!("[cancelled: {reason}]");
                        }
                    }
                }
            }
        }

        for msg in from_rx.try_iter() {
            match msg {
                FromWorker::Listening => {
                    hud.show_listening();
                }
                FromWorker::Text { text, capture, asr, normalize } => {
                    // Hide BEFORE pasting. The panel is non-activating so it
                    // should never hold focus, but there is no reason for it to
                    // be on screen while a ⌘V goes somewhere else.
                    hud.hide();
                    insert_and_report(&mut backend, &text, capture, asr, normalize, verbose);
                }
                FromWorker::NoAudio => {
                    hud.hide();
                    println!("[nothing heard -- either no speech, or the key was released before the first audio callback]");
                }
                FromWorker::Failed(e) => {
                    hud.hide();
                    eprintln!("[utterance failed: {e}]");
                }
                FromWorker::StartFailed(e) => eprintln!("[capture failed to start: {e}]"),
                FromWorker::Ready { .. } => {}
            }
        }

        hud.tick(f32::from_bits(level_bits.load(Ordering::Relaxed)));

        if worker.is_finished() {
            anyhow::bail!("the dictate worker thread exited unexpectedly");
        }
    }
}

/// Dequeue and dispatch any pending AppKit events.
///
/// A bare `CFRunLoop` pump is enough for the HUD panel (click-through,
/// non-activating, drawn from CALayer frames) but NOT for anything the user
/// interacts with: menu-bar clicks, window buttons and redraw all arrive as
/// `NSEvent`s that only `NSApplication` dequeues. Without this, those events
/// queue up untouched and macOS eventually marks the process "Not Responding".
///
/// Non-blocking: `untilDate: nil` returns immediately when the queue is empty,
/// so the caller keeps control of its own pacing.
#[cfg(target_os = "macos")]
fn pump_appkit_events(mtm: objc2::MainThreadMarker) {
    use objc2_app_kit::{NSApplication, NSEventMask};
    use objc2_foundation::NSDefaultRunLoopMode;

    let app = NSApplication::sharedApplication(mtm);
    while let Some(event) = unsafe {
        app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            None,
            NSDefaultRunLoopMode,
            true,
        )
    } {
        app.sendEvent(&event);
    }
}

/// When launched as an app there is no terminal, so every `println!` in the
/// agent — "listening", the transcript, "[copied to clipboard]", whisper's own
/// diagnostics — goes nowhere. That leaves the user with no way to tell a
/// mis-heard utterance from a mic that never opened, which is the single most
/// confusing failure this tool can have.
///
/// So when stdout is not a terminal, redirect both streams to a log file.
/// Running from a terminal is unaffected: output still goes to the terminal.
#[cfg(target_os = "macos")]
fn redirect_output_to_log_if_detached() -> Option<std::path::PathBuf> {
    // SAFETY: isatty on a fixed, always-valid fd.
    if unsafe { libc::isatty(1) } == 1 {
        return None;
    }
    let path = dirs::home_dir()?.join("Library/Logs/textify-voice.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    // SAFETY: `fd` is open for the duration of this call; dup2 onto stdout and
    // stderr is the standard daemon idiom.
    unsafe {
        libc::dup2(fd, 1);
        libc::dup2(fd, 2);
    }
    std::mem::forget(file);
    Some(path)
}

/// Entry point for "launched as the app" — see `packaging/README.md`'s "One
/// binary, two faces": when this binary is started with zero `argv`
/// (LaunchServices' real launch shape for a double-clicked/Finder/Dock
/// `.app`, verified there), `main.rs` calls this instead of `Cli::parse()`.
/// Launched with an explicit subcommand from a terminal, none of this runs
/// — behavior is exactly `run_macos`/the rest of this crate, unchanged.
///
/// Order of operations, per this unit's dispatch:
/// 1. Run the onboarding funnel if it is not already `Ready` (permissions +
///    model download) — `crate::onboarding` is a pure function of live
///    state (see its module doc), so this is "on first run" in the sense
///    that a completed funnel skips straight past it, not a stored
///    "have I ever shown this" flag.
/// 2. Re-check permissions for real. `open_onboarding_window()` returns
///    `Ok(())` whether the user finished the funnel or quit partway
///    through (see that function's `Choice::Quit` handling) — this CLI
///    does not rely on its return value to know which happened.
/// 3. Hand off to `run_agent_macos`, which parks in the menu bar showing
///    `PermissionsMissing` until both grants land rather than quitting —
///    see its "STAY RESIDENT" note for why an agent must not exit here.
#[cfg(target_os = "macos")]
pub fn run_agent(verbose: bool) -> Result<()> {
    let log_path = redirect_output_to_log_if_detached();
    if let Some(p) = &log_path {
        println!("\n=== textify-voice agent starting ===");
        println!("log: {}", p.display());
    }
    let needs_onboarding = crate::onboarding::OnboardingState::load()
        .map(|state| {
            let inputs = crate::onboarding::live_inputs(&state.counters);
            crate::onboarding::current_step(&inputs) != crate::onboarding::OnboardingStep::Ready
        })
        // No counters on disk yet is exactly the "never onboarded" case --
        // show the wizard rather than silently skipping it.
        .unwrap_or(true);

    if needs_onboarding {
        if let Err(e) = crate::onboarding::open_onboarding_window() {
            eprintln!("[onboarding wizard failed to open: {e:#}]");
        }
    }

    // DO NOT QUIT ON MISSING PERMISSIONS. The terminal `dictate` command
    // refuses to half-run and exits, which is right for a foreground command
    // you just typed. It is wrong for a menu-bar agent: granting Accessibility
    // means leaving the app, toggling a switch in System Settings, and coming
    // back — and an app that has quit by the time you return cannot be come
    // back to. It just vanishes, which reads as a crash.
    //
    // So the agent stays resident and keeps re-checking. run_agent_macos shows
    // a PermissionsNeeded state in the menu bar and arms dictation the moment
    // both grants land, with no relaunch required.
    run_agent_macos(verbose)
}

#[cfg(not(target_os = "macos"))]
pub fn run_agent(_verbose: bool) -> Result<()> {
    anyhow::bail!("the Textify Voice menu-bar agent is only implemented for macOS in this build")
}

/// The menu-bar agent's live loop. Structurally `run_macos` plus three
/// things a terminal invocation does not need: a `platform::StatusUi`
/// (`crate::menubar::MenuBar` behind the trait boundary, degrading to
/// `NullStatusUi` if the status item can't be constructed or
/// `PlatformCaps::can_show_status_ui` is false), an armed/unarmed toggle
/// (pause dictation without quitting), and settings sourced from
/// `crate::settings` on disk instead of CLI flags.
///
/// **Hot-reload scope, stated precisely rather than implied:** closing the
/// Settings window reloads `mode`, paste-vs-clipboard, HUD, and sound
/// immediately — none of those touch the input tap or the loaded ASR
/// model, so swapping them in place (a fresh `Box<dyn Indicator/Cues>`, a
/// reassigned local) is safe. Hold key and model are deliberately **not**
/// hot-swapped: `crate::holdkey::HoldKeyTap` (outside this unit's
/// ownership) has no `Drop` impl that tears down its `CGEventTap`, so a
/// second `MacHoldKey::install` call while the first is still alive would
/// risk leaking or double-delivering a live event tap rather than cleanly
/// replacing it — a correctness risk, not a cosmetic one. A changed hold
/// key or model is saved to disk (the Settings window itself already
/// writes it) and takes effect the next time Textify Voice is (re)launched;
/// this function prints a note to that effect when it detects the
/// difference on reload.
#[cfg(target_os = "macos")]
fn run_agent_macos(verbose: bool) -> Result<()> {
    use crate::platform::{self, HoldEvent, HoldKeySource as _, StatusUiEvent, StatusUiState};
    use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRunLoop};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    let caps = platform::current_caps();
    print_platform_gaps(&caps);

    let dictionary = load_dictionary_reporting();
    let context_provider = Arc::new(voice_context::MacosContextProvider::new());
    let level_bits = Arc::new(AtomicU32::new(0));

    let mut status_ui: Box<dyn platform::StatusUi> = if caps.can_show_status_ui {
        match platform::macos::MacStatusUi::new() {
            Ok(ui) => Box::new(ui),
            Err(e) => {
                eprintln!("[menu bar unavailable: {e:#} -- continuing without one]");
                Box::new(platform::NullStatusUi)
            }
        }
    } else {
        Box::new(platform::NullStatusUi)
    };

    let load = crate::settings::load().unwrap_or_else(|e| {
        eprintln!("[settings unavailable: {e:#} -- using defaults]");
        crate::settings::LoadResult {
            settings: crate::settings::Settings::default(),
            found: false,
            unknown_keys: Vec::new(),
            errors: Vec::new(),
        }
    });
    for err in &load.errors {
        eprintln!("settings warning: {err}");
    }
    let settings = load.settings;

    let hold_key = settings.hold_key;
    let model = settings.model;
    let mut mode = settings.mode;
    let mut paste_enabled = matches!(settings.insertion, crate::settings::InsertionMode::Paste);
    let mut hud_enabled = settings.hud_enabled;
    let mut sound_enabled = settings.sound_enabled;
    let mut update_check_enabled = settings.update_check_enabled;

    // Auto-update: a channel-based background checker, the same
    // non-blocking shape `from_rx`/`status_ui.poll_events()` already use
    // below -- every AppKit call (including `status_ui.set_update_text`)
    // must happen on this thread, so a background thread pushes
    // `UpdateState`s onto `update_tx` rather than touching the UI itself.
    // See `spawn_update_checker`'s doc comment for why this starts before
    // the permissions-wait loop below (checking for updates needs
    // neither Microphone nor Accessibility).
    let (update_tx, update_rx) = std::sync::mpsc::channel::<crate::update::UpdateState>();
    let updates_configured = update_appcast_url().is_some();
    let mut update_checker: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> =
        spawn_update_checker_if_configured(update_check_enabled, &update_tx);
    let mut latest_update_state: Option<crate::update::UpdateState> = None;
    let mut update_op_in_flight = false;
    if !updates_configured {
        // Say so once, up front, instead of leaving the menu's default
        // "not checked yet" sitting there forever waiting on a check that
        // will never be attempted.
        status_ui.set_update_text(UPDATE_NOT_CONFIGURED_MESSAGE);
    }

    status_ui.set_hold_key(hold_key.describe());
    let mut armed = true;
    status_ui.set_armed(armed);
    status_ui.set_state(StatusUiState::Idle);

    // STAY RESIDENT UNTIL THE GRANTS LAND.
    //
    // Installing the hold-key tap needs Accessibility and opening the mic needs
    // Microphone, so arming before either is granted just fails. The terminal
    // `dictate` command exits in that situation, which is right for a command
    // you just typed. It is wrong here: granting Accessibility means leaving the
    // app, flipping a switch in System Settings, and coming back — and an app
    // that quit while you were away is not there to come back to. It reads as a
    // crash, which is exactly how this first presented.
    //
    // So park in the menu bar showing PermissionsMissing, keep pumping the run
    // loop so Quit still works, and arm the moment both grants appear. No
    // relaunch, no lost app.
    let mode_ref = unsafe { kCFRunLoopDefaultMode };
    {
        let mut announced = false;
        loop {
            let report = permissions::check();
            if report.all_granted() {
                if announced {
                    println!("permissions granted -- arming dictation.");
                }
                break;
            }
            if !announced {
                status_ui.set_state(StatusUiState::PermissionsMissing);
                println!(
                    "waiting for permissions. Grant Microphone and Accessibility to \"Textify \
                     Voice\" in System Settings > Privacy & Security; this app will arm itself \
                     the moment both land. (Quit from the menu bar icon.)"
                );
                announced = true;
            }
            CFRunLoop::run_in_mode(mode_ref, 0.25, false);
            if let Some(mtm) = objc2::MainThreadMarker::new() {
                pump_appkit_events(mtm);
            }
            // The background update checker (started above, before this
            // loop) runs regardless of permission state -- checking for
            // updates needs neither Microphone nor Accessibility -- so
            // its results are drained here too, not just in the main
            // loop below. `CheckForUpdates` clicks are ignored while
            // waiting on permissions (the `if`s below only look at `Quit`
            // and `OpenSettings`), same as any other status-UI event not
            // named here.
            for state in update_rx.try_iter() {
                status_ui.set_update_text(&state.to_string());
                latest_update_state = Some(state);
            }
            for event in status_ui.poll_events() {
                if matches!(event, StatusUiEvent::Quit) {
                    if let Some((_, stop)) = update_checker.take() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    return Ok(());
                }
                if matches!(event, StatusUiEvent::OpenSettings) {
                    let _ = crate::settings::open_settings_window();
                }
            }
        }
        status_ui.set_state(StatusUiState::Idle);
    }

    // Installed on THIS thread's run loop, same as `run_macos`.
    let tap = platform::macos::MacHoldKey::install(hold_key)?;

    let (worker, to_tx, from_rx) = spawn_worker(
        model,
        verbose,
        dictionary,
        Arc::clone(&level_bits),
        Arc::clone(&context_provider),
    )?;

    let mut cues: Box<dyn platform::Cues> = build_cues(sound_enabled);
    let mut hud: Box<dyn platform::Indicator> = build_hud(hud_enabled && caps.can_overlay);

    if matches!(mode, DictateMode::Ptt) && !tap.supports_hold() {
        println!("note: this input backend cannot report key-release -- using toggle mode");
        mode = DictateMode::Toggle;
    }
    println!(
        "menu-bar agent ready -- hold {} to talk ({:?} mode){}",
        hold_key.describe(),
        mode,
        if paste_enabled { ", auto-paste ON" } else { ", clipboard only" }
    );

    let mut backend = CliInsertionBackend {
        paste_enabled,
        verbose,
        context_provider: Arc::clone(&context_provider),
        last_status: std::cell::Cell::new(voice_context::SecureFieldStatus::Unknown),
    };
    let mut listening = false;

    let mode_ref = unsafe { kCFRunLoopDefaultMode };
    loop {
        CFRunLoop::run_in_mode(mode_ref, 1.0 / 60.0, false);
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            pump_appkit_events(mtm);
        }

        for event in tap.poll() {
            if matches!(event, HoldEvent::SourceDisabled) {
                eprintln!("[the OS stopped delivering hold-key events -- re-arming]");
                tap.re_arm();
                continue;
            }
            if !armed {
                // "Dictation Armed" is unchecked -- the tap keeps running
                // (so `SourceDisabled` above still gets re-armed), but
                // presses are ignored entirely: no cues, no capture, no
                // status change. This is the whole point of the toggle.
                continue;
            }
            match event {
                HoldEvent::SourceDisabled => unreachable!("handled above"),
                HoldEvent::Down => {
                    let start = !listening;
                    let stop = matches!(mode, DictateMode::Toggle) && listening;
                    if start {
                        listening = true;
                        cues.press();
                        let _ = to_tx.send(ToWorker::Start);
                    } else if stop {
                        listening = false;
                        cues.release();
                        let _ = to_tx.send(ToWorker::Stop);
                        hud.show_transcribing();
                        status_ui.set_state(StatusUiState::Transcribing);
                    }
                }
                HoldEvent::Up => {
                    if matches!(mode, DictateMode::Ptt) && listening {
                        listening = false;
                        cues.release();
                        let _ = to_tx.send(ToWorker::Stop);
                        hud.show_transcribing();
                        status_ui.set_state(StatusUiState::Transcribing);
                    }
                }
                HoldEvent::Cancel(reason) => {
                    if listening {
                        listening = false;
                        let _ = to_tx.send(ToWorker::Cancel);
                        hud.hide();
                        status_ui.set_state(StatusUiState::Idle);
                        if verbose {
                            println!("[cancelled: {reason}]");
                        }
                    }
                }
            }
        }

        for msg in from_rx.try_iter() {
            match msg {
                FromWorker::Listening => {
                    hud.show_listening();
                    status_ui.set_state(StatusUiState::Listening);
                }
                FromWorker::Text { text, capture, asr, normalize } => {
                    hud.hide();
                    status_ui.set_state(StatusUiState::Idle);
                    insert_and_report(&mut backend, &text, capture, asr, normalize, verbose);
                }
                FromWorker::NoAudio => {
                    hud.hide();
                    status_ui.set_state(StatusUiState::Idle);
                    println!("[nothing heard -- either no speech, or the key was released before the first audio callback]");
                }
                FromWorker::Failed(e) => {
                    hud.hide();
                    status_ui.set_state(StatusUiState::Error);
                    eprintln!("[utterance failed: {e}]");
                }
                FromWorker::StartFailed(e) => {
                    status_ui.set_state(StatusUiState::Error);
                    eprintln!("[capture failed to start: {e}]");
                }
                FromWorker::Ready { .. } => {}
            }
        }

        for state in update_rx.try_iter() {
            // A `Downloading` tick is progress, not a finished operation --
            // everything else (up to date / available / ready to relaunch
            // / failed) means whatever background thread was in flight has
            // finished, so a click on "Check for Updates…" is safe again.
            if !matches!(state, crate::update::UpdateState::Downloading { .. }) {
                update_op_in_flight = false;
            }
            if matches!(state, crate::update::UpdateState::Available(_)) {
                println!("[{state} -- click \"Check for Updates…\" in the menu bar to download it]");
            }
            status_ui.set_update_text(&state.to_string());
            latest_update_state = Some(state);
        }

        hud.tick(f32::from_bits(level_bits.load(Ordering::Relaxed)));

        for event in status_ui.poll_events() {
            match event {
                StatusUiEvent::ToggleArmed => {
                    armed = !armed;
                    status_ui.set_armed(armed);
                    if !armed {
                        status_ui.set_state(StatusUiState::Idle);
                    }
                }
                StatusUiEvent::OpenSettings => {
                    // Blocking modal -- the run loop simply isn't pumped
                    // while this is open, same as any other modal AppKit
                    // call already on this thread (see `insert_and_report`
                    // and `crate::paste::synthesize_cmd_v`, which also
                    // block this same thread for their own real reasons).
                    if let Err(e) = crate::settings::open_settings_window() {
                        eprintln!("[settings window failed: {e:#}]");
                    }
                    match crate::settings::load() {
                        Ok(reloaded) => {
                            for err in &reloaded.errors {
                                eprintln!("settings warning: {err}");
                            }
                            let new_settings = reloaded.settings;
                            if new_settings.hold_key != hold_key || new_settings.model != model {
                                println!(
                                    "[settings saved -- hold key / model changes take effect the \
                                     next time Textify Voice is relaunched; everything else \
                                     applied now]"
                                );
                            }
                            mode = new_settings.mode;
                            if matches!(mode, DictateMode::Ptt) && !tap.supports_hold() {
                                mode = DictateMode::Toggle;
                            }
                            paste_enabled =
                                matches!(new_settings.insertion, crate::settings::InsertionMode::Paste);
                            backend.paste_enabled = paste_enabled;
                            if new_settings.hud_enabled != hud_enabled {
                                hud_enabled = new_settings.hud_enabled;
                                hud = build_hud(hud_enabled && caps.can_overlay);
                            }
                            if new_settings.sound_enabled != sound_enabled {
                                sound_enabled = new_settings.sound_enabled;
                                cues = build_cues(sound_enabled);
                            }
                            if new_settings.update_check_enabled != update_check_enabled {
                                update_check_enabled = new_settings.update_check_enabled;
                                if update_check_enabled {
                                    update_checker = spawn_update_checker_if_configured(
                                        update_check_enabled,
                                        &update_tx,
                                    );
                                } else if let Some((_, stop)) = update_checker.take() {
                                    // Not joined -- the background thread
                                    // notices within about a second (see
                                    // `update::spawn_background_checker`'s
                                    // own doc comment on its chunked sleep)
                                    // and exits on its own; nothing here
                                    // needs to observe that happening.
                                    stop.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(e) => eprintln!("[settings reload after closing the window failed: {e:#}]"),
                    }
                }
                StatusUiEvent::CheckForUpdates => {
                    if !updates_configured {
                        // The row already says this; repeat it in the log so a
                        // click that appears to do nothing has an explanation
                        // somewhere the user can find.
                        println!("[{UPDATE_NOT_CONFIGURED_MESSAGE}]");
                        status_ui.set_update_text(UPDATE_NOT_CONFIGURED_MESSAGE);
                    } else if update_op_in_flight {
                        println!("[an update check or download is already in progress]");
                    } else {
                        match latest_update_state.clone() {
                            Some(crate::update::UpdateState::Available(item)) => {
                                update_op_in_flight = true;
                                status_ui.set_update_text(&format!("downloading update {}...", item.version));
                                let tx = update_tx.clone();
                                std::thread::spawn(move || {
                                    let final_state = download_and_stage_update(&item, &tx);
                                    let _ = tx.send(final_state);
                                });
                            }
                            Some(crate::update::UpdateState::ReadyToRelaunch { staged_app }) => {
                                match install_staged_update_and_relaunch(&staged_app) {
                                    Ok(()) => {
                                        println!(
                                            "[installing update -- quitting so the relaunch helper \
                                             can finish]"
                                        );
                                        if let Some((_, stop)) = update_checker.take() {
                                            stop.store(true, Ordering::Relaxed);
                                        }
                                        drop(to_tx);
                                        return Ok(());
                                    }
                                    Err(e) => {
                                        eprintln!("[update install failed: {e:#}]");
                                        status_ui.set_update_text(&format!("update install failed: {e}"));
                                    }
                                }
                            }
                            _ => {
                                // `updates_configured` was checked above, so
                                // there is a URL here; falling back to no-op
                                // rather than unwrapping keeps that coupling
                                // from becoming a panic if it ever changes.
                                if let Some(url) = update_appcast_url() {
                                    update_op_in_flight = true;
                                    status_ui.set_update_text("checking for updates...");
                                    let tx = update_tx.clone();
                                    std::thread::spawn(move || {
                                        let _ = tx.send(crate::update::check_now(&url));
                                    });
                                }
                            }
                        }
                    }
                }
                StatusUiEvent::Quit => {
                    // Dropping `to_tx` lets the worker thread's `rx.recv()`
                    // return `Err` and exit its loop cleanly; `status_ui`'s
                    // own `Drop` (see `crate::menubar::MenuBar`) removes
                    // the status item. No `std::process::exit` needed --
                    // returning `Ok(())` here unwinds back through
                    // `run_agent` to `main`, which exits 0.
                    if let Some((_, stop)) = update_checker.take() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    drop(to_tx);
                    return Ok(());
                }
            }
        }

        if worker.is_finished() {
            status_ui.set_state(StatusUiState::Error);
            anyhow::bail!("the dictate worker thread exited unexpectedly");
        }
    }
}

/// Owns the whisper engine and the mic stream. Everything here is off the main
/// thread precisely because `finalize()` blocks.
///
/// Takes `model` on its own (not the whole `DictateArgs`) so `spawn_worker`
/// below can be reused to (re)start a session from either `run_macos` (the
/// CLI path) or `run_agent_macos` (the menu-bar agent), which builds its
/// model choice from `crate::settings::Settings`, not from a `DictateArgs`.
#[cfg(target_os = "macos")]
fn dictate_worker(
    model: ModelArg,
    verbose: bool,
    rx: std::sync::mpsc::Receiver<ToWorker>,
    tx: std::sync::mpsc::Sender<FromWorker>,
    level_bits: Arc<std::sync::atomic::AtomicU32>,
    dictionary: crate::dictionary::Dictionary,
    context_provider: Arc<voice_context::MacosContextProvider>,
) {
    use std::sync::atomic::Ordering;
    use voice_asr_whisper::{ModelManager, WhisperAsrConfig, WhisperLocalAsr};
    use voice_audio::{AudioSource, MicCapture};
    use voice_context::ContextProvider as _;

    // Bias-layer-2 terms + custom literal substitutions from the user
    // dictionary, folded in ONCE (the file was already read on the main
    // thread before this thread was spawned -- see `run_macos`). Combined
    // with the built-in literal rules the same way `transcribe` does.
    let dictionary_terms: Vec<BiasTerm> = dictionary.terms.clone();
    let mut literal_rules: Vec<LiteralRule> = default_literal_rules();
    literal_rules.extend(dictionary.literal_rules.clone());

    // SPEC §3.3 staleness policy: "an utterance starts with the PREVIOUS
    // context snapshot." Updated at the top of `ToWorker::Start` below, from
    // whatever `context_provider` has already resolved by then; never
    // blocks. `prev_terms` (also SPEC §3.3, "prior-utterance terms") is
    // this utterance's own free bias source -- the previous utterance's
    // finished (post-normalize) words.
    let mut current_app_kind = AppKind::General;
    let mut prev_terms: Vec<String> = Vec::new();

    macro_rules! fail {
        ($($t:tt)*) => {{
            let _ = tx.send(FromWorker::StartFailed(format!($($t)*)));
            return;
        }};
    }

    let model_manager = match ModelManager::new() {
        Ok(m) => m,
        Err(e) => fail!("resolving the whisper model cache directory: {e}"),
    };
    let model_id = model.to_model_id();
    if !model_manager.is_cached(model_id) {
        println!("model {} not cached -- downloading now (one-time)...", model_id.filename());
    }
    let model_path = match model_manager.ensure_downloaded(model_id, None) {
        Ok(p) => p,
        Err(e) => fail!("downloading whisper model {}: {e}", model_id.filename()),
    };

    let mut whisper_config = WhisperAsrConfig::new(model_path);
    whisper_config.pcm_capacity_seconds = 120;
    let mut asr = match WhisperLocalAsr::new(whisper_config) {
        Ok(a) => a,
        Err(e) => fail!("loading whisper model: {e}"),
    };

    let pcm_buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::new()));
    let cb_buf = Arc::clone(&pcm_buf);
    let cb_level = Arc::clone(&level_bits);
    let mut capture = match MicCapture::new(move |frames: &[i16]| {
        // Realtime thread: append and publish one level. No allocation beyond
        // the Vec growth, no locks held across anything slow, no UI calls.
        cb_level.store(crate::hud::rms_level(frames).to_bits(), Ordering::Relaxed);
        if let Ok(mut buf) = cb_buf.lock() {
            buf.extend_from_slice(frames);
        }
    }) {
        Ok(c) => c,
        Err(e) => fail!("opening the microphone capture stream (check Microphone permission): {e}"),
    };

    let _ = tx.send(FromWorker::Ready {
        device: capture.device_name().to_string(),
        sample_rate: capture.native_sample_rate(),
        channels: capture.native_channels(),
    });

    let mut started_at = Instant::now();

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ToWorker::Start => {
                if let Ok(mut buf) = pcm_buf.lock() {
                    buf.clear();
                }
                level_bits.store(0f32.to_bits(), Ordering::Relaxed);

                // Non-blocking context capture (SPEC §3.1: "never blocks the
                // first audio frame") -- hands back whatever the provider
                // already resolved, and fires a background refresh for the
                // NEXT utterance. Deliberately happens before `capture.start()`
                // rather than after: if this were ever changed to block, the
                // ordering below would still protect the first audio frame.
                let ctx = context_provider.capture();
                current_app_kind =
                    context_app_kind_to_core(ctx.snapshot.frontmost_app.as_ref().map(|a| a.kind));
                if verbose {
                    match &ctx.snapshot.frontmost_app {
                        Some(app) => println!(
                            "[context: frontmost=\"{}\" kind={:?} -> app_kind={current_app_kind:?}]",
                            app.name, app.kind
                        ),
                        None => println!(
                            "[context: no frontmost app resolved yet -> app_kind={current_app_kind:?}]"
                        ),
                    }
                }

                started_at = Instant::now();
                if let Err(e) = capture.start() {
                    let _ = tx.send(FromWorker::StartFailed(e.to_string()));
                    continue;
                }
                let _ = tx.send(FromWorker::Listening);
            }
            ToWorker::Cancel => {
                let _ = capture.stop();
                level_bits.store(0f32.to_bits(), Ordering::Relaxed);
                if let Ok(mut buf) = pcm_buf.lock() {
                    buf.clear();
                }
            }
            ToWorker::Stop => {
                let capture_dt = started_at.elapsed();
                let _ = capture.stop();
                level_bits.store(0f32.to_bits(), Ordering::Relaxed);

                let pcm = match pcm_buf.lock() {
                    Ok(buf) => buf.clone(),
                    Err(_) => {
                        let _ = tx.send(FromWorker::Failed(
                            "pcm buffer poisoned -- dropping this utterance".to_string(),
                        ));
                        continue;
                    }
                };
                if pcm.is_empty() {
                    let _ = tx.send(FromWorker::NoAudio);
                    continue;
                }

                match transcribe_utterance(
                    &mut asr,
                    &pcm,
                    current_app_kind,
                    &dictionary_terms,
                    &prev_terms,
                    &literal_rules,
                ) {
                    Ok((text, asr_dt, norm_dt)) => {
                        // SPEC §3.3 "prior-utterance terms": carry this
                        // utterance's finished words forward as the next
                        // one's free, lower-weight bias fallback (see
                        // `voice_core::bias::effective_terms`).
                        // NO SPEECH IS NOT A TRANSCRIPT. Whisper narrates the
                        // absence of speech ("[BLANK_AUDIO]", "[MUSIC]"), which
                        // voice-asr-whisper strips — leaving an empty string.
                        // Inserting that would clobber the user's clipboard and
                        // paste nothing into their document; the correct
                        // response to hearing nothing is to do nothing.
                        if text.trim().is_empty() {
                            let _ = tx.send(FromWorker::NoAudio);
                            continue;
                        }
                        prev_terms = text.split_whitespace().map(str::to_string).collect();
                        let _ = tx.send(FromWorker::Text {
                            text,
                            capture: capture_dt,
                            asr: asr_dt,
                            normalize: norm_dt,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(FromWorker::Failed(format!("{e:#}")));
                    }
                }
                let _ = verbose;
            }
        }
    }
}

/// ASR + normalizer. Returns the finished text and the two stage timings.
///
/// `bias_terms`/`literal_rules` come from the user dictionary (loaded once
/// at startup, see `run_macos`); `app_kind` and `prev_terms` are per-
/// utterance (real frontmost-app context, and the previous utterance's own
/// words -- see the `ToWorker::Start`/`ToWorker::Stop` handlers above).
/// `BiasContext::empty(AppKind::General)` is gone: this is the real context
/// wiring SPEC §3.3 describes, not a stand-in.
#[cfg(target_os = "macos")]
fn transcribe_utterance(
    asr: &mut voice_asr_whisper::WhisperLocalAsr,
    pcm: &[i16],
    app_kind: AppKind,
    bias_terms: &[BiasTerm],
    prev_terms: &[String],
    literal_rules: &[LiteralRule],
) -> Result<(String, Duration, Duration)> {
    use voice_core::LocalAsr;

    let bias = BiasContext { terms: bias_terms.to_vec(), app_kind, prev_terms: prev_terms.to_vec() };

    let t_asr = Instant::now();
    asr.start_utterance(&bias);
    asr.feed_pcm(pcm);
    let transcript = asr.finalize().map_err(|e| anyhow::anyhow!("asr finalize: {e}"))?;
    let asr_dt = t_asr.elapsed();

    let t_norm = Instant::now();
    let words: Vec<WordSpan> = transcript
        .per_word_conf
        .iter()
        .map(|w| WordSpan::new(w.word.clone(), w.confidence))
        .collect();
    let result = normalize(&words, &bias, literal_rules, &CorrectionThresholds::default());
    let norm_dt = t_norm.elapsed();

    Ok((result.text, asr_dt, norm_dt))
}

/// How long an `Unknown`-secure-field clipboard stage (see
/// `stage_unknown_secure_field_clipboard` below) is left on the system
/// pasteboard before it is automatically restored to whatever was there
/// before this utterance. Bounded deliberately: `Unknown` is usually a
/// benign timeout (see `insert_and_report`'s `Refused` handling), so the
/// transcript is worth staging for the user to paste manually -- but on the
/// rare genuine secure field, leaving a spoken secret on a world-readable
/// pasteboard *indefinitely* is the actual privacy hole this constant
/// closes. 45s is long enough for a normal "release the key, then hit
/// ⌘V" human reaction, matching the same order of magnitude password
/// managers (e.g. `pass`'s 45s default) use for the identical "clipboard
/// exposure window" tradeoff.
#[cfg(target_os = "macos")]
const UNKNOWN_SECURE_FIELD_CLIPBOARD_CLEAR: Duration = Duration::from_secs(45);

/// Stage `text` to the clipboard for the `Unknown`-secure-field case
/// (`insert_and_report`'s `Refused` handling below) and arrange for it to be
/// restored to whatever was on the clipboard before, automatically, after
/// `clear_after` -- so an unconfirmed target that really was a secure field
/// (AX timed out while a secure field was focused) never leaves the spoken
/// transcript sitting on the world-readable pasteboard forever.
///
/// Runs the delayed restore on a **background thread**, not the caller's:
/// `ClipboardGuard::restore_after_delay` blocks on `std::thread::sleep`, and
/// `insert_and_report` runs on the main thread, which also owns the HUD
/// panel and the `CGEventTap` run loop -- sleeping there for
/// `UNKNOWN_SECURE_FIELD_CLIPBOARD_CLEAR` would freeze both for the whole
/// window (see `ClipboardGuard::restore_after_delay`'s own doc comment,
/// which names exactly this as the reason to prefer a background-thread
/// caller). The spawned thread owns the guard for its whole lifetime and
/// exits after the one restore attempt; `restore_after_delay`'s own
/// `changeCount` guard still applies, so a user who copies something else
/// (or pastes and then copies something new) before the timer fires is never
/// clobbered.
///
/// Returns the human-readable clause `insert_and_report` prints, so the
/// message on screen always names the real, current bound rather than
/// risking drifting out of sync with the constant above.
///
/// **Verification note**: this function's real-pasteboard behavior (staged
/// during the bound, restored after it, message text matches the real bound,
/// never claims an auto-paste) was verified against the actual macOS
/// `NSPasteboard` -- before/during/after -- via a `#[test]` written and run
/// during this unit's work (single-threaded: `cargo test -p voice-cli
/// dictate::unknown_secure_field_clipboard_tests -- --test-threads=1`, both
/// cases passed). It is **not** committed to this file: run alongside
/// `crate::clipboard`'s own pre-existing real-pasteboard tests under this
/// crate's default (parallel) `cargo test`, the two independently-locked
/// test modules raced the same live `NSPasteboard` singleton and reliably
/// crashed the process (confirmed reproduced: a `SIGSEGV`/`SIGABRT` and,
/// isolated further, an actual `NSPasteboard`-state assertion failure
/// between the two modules' tests) -- `clipboard.rs`'s own serialization
/// `Mutex` is private to that file, which is explicitly outside this unit's
/// ownership, so this file cannot reach or extend it to coordinate safely.
/// Shipping that test would have broken the `cargo test --workspace` gate
/// for everyone, which is a worse outcome than not shipping it; see this
/// unit's final report for the literal reproduction and passing output.
#[cfg(target_os = "macos")]
fn stage_unknown_secure_field_clipboard(text: &str, clear_after: Duration) -> String {
    match crate::clipboard::ClipboardGuard::stage(text) {
        Ok(guard) => {
            std::thread::spawn(move || {
                let _ = guard.restore_after_delay(clear_after);
            });
            format!(
                "nothing was typed or auto-pasted, but the transcript was copied to the \
                 clipboard for you to paste manually -- it will be cleared automatically in \
                 {}s if you don't paste it first (or if you copy something else, whichever \
                 comes first)",
                clear_after.as_secs()
            )
        }
        Err(e) => format!("nothing was typed, and staging it to the clipboard also failed: {e}"),
    }
}

/// Insertion runs on the main thread: it touches the pasteboard and posts a
/// synthetic key event, and it happens right after the HUD is hidden.
/// Print the transcript only where it is safe to.
///
/// When stdout is a terminal the user is watching their own screen and wants to
/// see what was heard. When it is NOT a terminal, stdout has been redirected to
/// ~/Library/Logs/textify-voice.log — and writing transcripts there puts every
/// dictated sentence on disk in plaintext, indefinitely, in a file no one
/// thinks of as sensitive. SPEC 3.1 is explicit that "transcripts are
/// sensitive; plaintext-at-rest undermines the brand", and that applies to a
/// log file exactly as much as to the history store it was written about.
///
/// This was introduced by the log-redirection fix: solving "the app tells me
/// nothing" by writing everything to disk also wrote the one thing that must
/// not be. So the log gets a shape without content — enough to debug a silent
/// failure, nothing anyone would mind leaking.
#[cfg(target_os = "macos")]
fn report_transcript(text: &str) {
    // SAFETY: isatty on a fixed, always-valid fd.
    if unsafe { libc::isatty(1) } == 1 {
        println!("> {text}");
    } else {
        println!("> [{} words, {} chars]", text.split_whitespace().count(), text.chars().count());
    }
}

#[cfg(target_os = "macos")]
fn insert_and_report(
    backend: &mut CliInsertionBackend,
    text: &str,
    capture_dt: Duration,
    asr_dt: Duration,
    norm_dt: Duration,
    verbose: bool,
) {
    if text.trim().is_empty() {
        // Second guard on the same invariant: nothing reaches the clipboard or
        // the keyboard for an utterance with no words in it.
        println!("[nothing heard -- clipboard untouched]");
        return;
    }

    let t_ins = Instant::now();
    let method = match insert_text(backend, text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[insertion failed: {e:?}]");
            report_transcript(text);
            return;
        }
    };
    let ins_dt = t_ins.elapsed();

    report_transcript(text);
    match method {
        InsertionMethod::AxInsert => println!("  [inserted via AX]"),
        InsertionMethod::ClipboardPaste => {
            println!(
                "  [copied to clipboard{}]",
                if backend.paste_enabled { " + pasted (\u{2318}V)" } else { "" }
            );
        }
        InsertionMethod::Refused(reason) => {
            // `insert_text()` refused without calling the backend at all --
            // per `voice_core`'s own test (`secure_field_is_refused_outright_
            // no_backend_call_made`), neither `ax_insert` nor
            // `clipboard_paste` ran, so nothing has been typed OR copied yet.
            //
            // That is exactly right for a KNOWN secure field (SPEC §3.1: "no
            // clicking, no typing, no reading" -- the transcript must not
            // touch the system clipboard either, since any other app can
            // read it). It is one notch too eager for `Unknown`: most
            // `Unknown`s are an ordinary field the AX read just couldn't
            // confirm in time, and losing the words entirely is the kind of
            // over-refusal that makes people disable dictation. So: stage a
            // clipboard-only copy (never auto-pasted, regardless of
            // `--paste`) for that case specifically, using
            // `backend.last_status` -- the side channel this struct's doc
            // comment explains is standing in for a `voice_core` shape
            // change. `InsertionMethod::Refused` still means "no keystroke
            // was ever synthesized" either way; this only ever ADDS a
            // clipboard write, never a paste -- and, per
            // `stage_unknown_secure_field_clipboard` above, never a
            // *permanent* one on the `Unknown` path: if that unconfirmed
            // target really was a secure field (AX timed out while a secure
            // field was focused), the spoken transcript does not sit on the
            // world-readable pasteboard indefinitely.
            match backend.last_status.get() {
                voice_context::SecureFieldStatus::Unknown => {
                    let detail = stage_unknown_secure_field_clipboard(
                        text,
                        UNKNOWN_SECURE_FIELD_CLIPBOARD_CLEAR,
                    );
                    println!(
                        "  [insertion refused: {reason:?} (could not confirm this field is safe) -- {detail}]"
                    );
                }
                voice_context::SecureFieldStatus::Known(_) => {
                    println!(
                        "  [insertion refused: {reason:?} -- nothing was typed or copied to the \
                         clipboard (this is a known secure field)]"
                    );
                }
            }
        }
    }

    if verbose {
        println!(
            "  -- capture {:.1} ms | asr {:.1} ms | normalize {:.1} ms | insert {:.1} ms",
            ms(capture_dt),
            ms(asr_dt),
            ms(norm_dt),
            ms(ins_dt)
        );
    }
    println!(
        "  speech-end-to-text: {:.1} ms (asr {:.1} ms + normalize {:.1} ms + insert {:.1} ms)",
        ms(asr_dt) + ms(norm_dt) + ms(ins_dt),
        ms(asr_dt),
        ms(norm_dt),
        ms(ins_dt)
    );
    println!();
}

#[cfg(target_os = "macos")]
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Wires `voice_core::insert_text`'s policy (AX-insert-if-writable, else
/// clipboard, refuse outright on a secure field) to this CLI's real
/// backends: `voice_context::MacosContextProvider` for the live focus read
/// that decides secure-field refusal, and `crate::clipboard::ClipboardGuard`
/// (snapshot -> write -> synthesized ⌘V -> restore) for delivery.
///
/// `is_secure_field` is now real: `current_target()` blocks briefly
/// (bounded by `voice_context::DEFAULT_AX_TIMEOUT`, ~300ms) on the
/// provider's in-flight AX read for the freshest possible answer right
/// before the SAFETY decision of whether to type at all -- see
/// `current_target`'s own comment for why blocking here, unlike everywhere
/// else in this loop, is the deliberate right call.
///
/// ## Fail-closed on Unknown (fix-wave blocker)
///
/// `voice_context::ContextCapture::wait_secure_field_status()` is the ONLY
/// thing this backend asks for that decision -- it returns a
/// `voice_context::SecureFieldStatus`, never a bare `bool`, precisely so
/// "I could not determine this" cannot collapse to "this is not a secure
/// field" the way it used to (see git history / DECISIONS.md for the
/// incident: a timed-out AX read during a stalled target app defaulted to
/// `is_secure_field: false`, and a password field got the transcript
/// clipboard-pasted into it). `SecureFieldStatus::Unknown` is mapped to
/// `is_secure_field: true` below -- the only value `InsertionTarget`'s
/// `bool` can use to make `insert_text()` refuse outright, via the exact
/// same, already-tested "no backend call is made at all" path a genuinely
/// known secure field takes. `voice_core::InsertionTarget` has no room for
/// a third state itself (see the doc comment on `last_status` below for
/// what a real fix there would look like); collapsing `Unknown` into
/// `is_secure_field: true` at this boundary is the ONE safe direction to
/// collapse it in, and is not a magic-boolean special case that a later
/// refactor could quietly drop -- it flows through the same field
/// `insert_text()` already refuses on.
///
/// `is_ax_writable` is DELIBERATELY still always `false`, not wired to the
/// real `ActionableElement::writable` bit the context provider reads. This
/// is an intentional, documented deviation, not an oversight: `ax_insert`
/// below has no real implementation (no live focused-`AXUIElement` *write*
/// path exists anywhere in this codebase -- `voice-context` only ever reads
/// and, by design, hands back plain data, not a live element handle a
/// second call could write through). If `current_target()` reported the
/// real writable bit, `insert_text`'s policy would route every ordinary
/// writable text field through `ax_insert()` and get a hard
/// `AxWriteFailed` error INSTEAD of the clipboard path that works today --
/// an active regression, not an improvement. Keeping this `false` is what
/// keeps clipboard-first the load-bearing path PORTING.md 2.2 requires
/// ("clipboard-first insertion is the portability keystone and must not
/// become paste-only" -- and, transitively, must not become "broken
/// outright because a bit it doesn't act on turned true").
#[cfg(target_os = "macos")]
struct CliInsertionBackend {
    paste_enabled: bool,
    verbose: bool,
    context_provider: Arc<voice_context::MacosContextProvider>,
    /// The `SecureFieldStatus` `current_target()` most recently computed,
    /// remembered so `insert_and_report` can tell "refused because this is
    /// a KNOWN secure field" (never touch the clipboard either) apart from
    /// "refused because we could not confirm either way" (clipboard-first
    /// recoverability is still owed to the user per PORTING.md 2.2) --
    /// `voice_core::InsertionMethod::Refused` and `RefusalReason` carry only
    /// `SecureField`, with no room for that distinction, so it has to
    /// survive out-of-band. `Cell`, not a plain field, because
    /// `InsertionBackend::current_target` takes `&self`. See this struct's
    /// doc comment: the RIGHT long-term fix is `voice_core::InsertionTarget`
    /// itself carrying a `SecureFieldStatus`-shaped tri-state (or
    /// `RefusalReason` gaining an `Unknown` variant) so callers don't need
    /// this side channel at all -- reported upstream, not mine to change
    /// here.
    last_status: std::cell::Cell<voice_context::SecureFieldStatus>,
}

#[cfg(target_os = "macos")]
impl InsertionBackend for CliInsertionBackend {
    fn current_target(&self) -> InsertionTarget {
        use voice_context::ContextProvider as _;

        // SAFETY: unlike every other `capture()` call in this file, this one
        // is allowed to wait for the in-flight read (`PendingContext::wait`,
        // via `wait_secure_field_status`) rather than taking whatever was
        // already resolved. Secure-field refusal is the one decision here
        // where "possibly one utterance stale" is not good enough -- the
        // user could have clicked into a password field in the time it took
        // to speak and get transcribed. The wait is bounded by the
        // provider's own per-read timeout (`voice_context::DEFAULT_AX_TIMEOUT`,
        // ~300ms; observed single-digit ms in practice against real apps --
        // see this unit's verification notes) and happens only once, right
        // here, immediately before the decision that gates whether anything
        // gets typed at all.
        //
        // `wait_secure_field_status()` (not a hand-rolled `unwrap_or`) is
        // what fixes the two defects a fix-wave audit found here: (1) it
        // never treats a timed-out/degraded fresh read as "not secure" --
        // every degraded case maps to `SecureFieldStatus::Unknown`, not
        // `Known(false)`; and (2) it never lets that fresh Unknown silently
        // discard a previously-resolved `Known(true)` -- see
        // `SecureFieldStatus::merge_after_fresh_read`'s doc comment for the
        // asymmetric staleness rule.
        let status = self.context_provider.capture().wait_secure_field_status();
        self.last_status.set(status);
        secure_status_to_target(status)
    }

    fn ax_insert(&mut self, _text: &str) -> Result<(), InsertionError> {
        Err(InsertionError::AxWriteFailed(
            "AX insertion is not implemented in this CLI -- current_target() always reports \
             is_ax_writable: false precisely so insert_text()'s policy never routes here; see \
             this struct's doc comment"
                .to_string(),
        ))
    }

    fn clipboard_paste(&mut self, text: &str) -> Result<(), InsertionError> {
        use crate::clipboard::{ClipboardGuard, RestoreOutcome};

        // Clipboard-first (PORTING.md 2.2): the transcript is on the real
        // pasteboard, and therefore recoverable by the user, before any
        // paste is even attempted.
        let mut guard =
            ClipboardGuard::stage(text).map_err(|e| InsertionError::ClipboardFailed(e.to_string()))?;
        if self.verbose {
            eprintln!(
                "  [clipboard: staged {} byte(s) (previous snapshot {} item(s) at changeCount \
                 {}, empty={}), changeCount now {}, armed={}, live changeCount {:?}]",
                text.len(),
                guard.snapshot().items.len(),
                guard.snapshot().change_count(),
                guard.snapshot().is_empty(),
                guard.written_change_count(),
                guard.is_armed(),
                crate::clipboard::current_change_count()
            );
        }

        if !self.paste_enabled {
            // --clipboard-only (or --paste not given): leaving the
            // transcript on the clipboard IS the desired end state here,
            // not an accident to undo.
            guard.disarm();
            return Ok(());
        }

        if let Err(e) = crate::paste::synthesize_cmd_v() {
            // The synthesized paste failed. Per PORTING.md 2.2 the text
            // must stay recoverable either way -- disarm rather than
            // restore, so the transcript (not the caller's older clipboard
            // contents) is what's left for the user to paste by hand.
            guard.disarm();
            return Err(InsertionError::ClipboardFailed(format!(
                "clipboard set successfully, but the synthesized \u{2318}V paste failed: {e} -- \
                 the transcript remains on the clipboard for you to paste manually"
            )));
        }

        // Bounded heuristic delay for the synthesized paste to be dispatched
        // and read by the target app, then the guarded restore -- see
        // `crate::clipboard`'s module doc for exactly what this does and
        // does not guarantee (there is no true "paste confirmed" signal on
        // macOS). `restore_after_delay` sleeps synchronously; that is
        // accepted here the same way the CGEvent synthesis just above it
        // already blocks this thread -- `HoldEvent::SourceDisabled`'s
        // re-arm handles the case where this stalls the tap long enough for
        // the OS to disable it.
        match guard.restore_after_delay(Duration::from_millis(150)) {
            RestoreOutcome::Restored | RestoreOutcome::RestoredEmpty | RestoreOutcome::Disarmed => {}
            RestoreOutcome::SkippedChanged { expected, found } => {
                eprintln!(
                    "[clipboard restore skipped -- something else wrote to the clipboard first \
                     (expected changeCount {expected}, found {found}); your previous clipboard \
                     contents were not restored]"
                );
            }
            RestoreOutcome::Failed(e) => {
                eprintln!(
                    "[clipboard restore failed: {e} -- your previous clipboard contents were not restored]"
                );
            }
        }
        Ok(())
    }
}

/// The one place `voice_context::SecureFieldStatus`'s tri-state gets
/// collapsed into `voice_core::InsertionTarget`'s `bool` -- pulled out of
/// `current_target()` specifically so this mapping is unit-testable on its
/// own, with no AX/live-desktop dependency. This is the exact boundary the
/// fix-wave blocker lived at (`focused_element: None` used to collapse to
/// `is_secure_field: false`); see this function's tests.
#[cfg(target_os = "macos")]
fn secure_status_to_target(status: voice_context::SecureFieldStatus) -> InsertionTarget {
    match status {
        voice_context::SecureFieldStatus::Known(secure) => {
            InsertionTarget { is_secure_field: secure, is_ax_writable: false }
        }
        // The one safe collapse: voice_core's InsertionTarget cannot express
        // "unknown", so "assume secure" (which insert_text() refuses
        // outright, calling neither ax_insert nor clipboard_paste) is the
        // only direction it is safe to guess in.
        voice_context::SecureFieldStatus::Unknown => InsertionTarget { is_secure_field: true, is_ax_writable: false },
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use voice_context::SecureFieldStatus;

    // -- secure_status_to_target: the blocker fix, pinned at the exact
    // boundary this file owns (`voice_context::SecureFieldStatus` ->
    // `voice_core::InsertionTarget`). Each case below feeds straight into
    // `voice_core::insert_text`'s already-tested policy
    // (`secure_field_is_refused_outright_no_backend_call_made` in
    // `voice-core/src/insertion.rs`): `is_secure_field: true` refuses
    // outright, calling neither `ax_insert` nor `clipboard_paste` -- i.e.
    // "must refuse to type".

    #[test]
    fn unknown_refuses_to_type_timeout_no_permission_no_focused_element_alike() {
        // `SecureFieldStatus::Unknown` is what `ContextSnapshot::secure_
        // field_status`/`wait_secure_field_status` produce for ALL of a
        // timed-out read, a missing Accessibility permission, and no
        // focused element resolving (see voice-context/src/provider.rs's
        // own regression tests for each of those three cases individually)
        // -- there is exactly one collapsed representation for this file to
        // get right, and it must refuse.
        let target = secure_status_to_target(SecureFieldStatus::Unknown);
        assert!(target.is_secure_field, "Unknown must map to is_secure_field: true (refuse), never false (the original fail-open bug)");
    }

    #[test]
    fn known_secure_refuses_to_type() {
        let target = secure_status_to_target(SecureFieldStatus::Known(true));
        assert!(target.is_secure_field);
    }

    #[test]
    fn known_not_secure_is_allowed_to_type() {
        // Non-regression: the fix must not make ordinary, confirmed-safe
        // targets refuse too -- that would make the product useless.
        let target = secure_status_to_target(SecureFieldStatus::Known(false));
        assert!(!target.is_secure_field);
    }

    #[test]
    fn never_reports_ax_writable_regardless_of_status() {
        // Pinned alongside the secure-field cases because it's the other
        // half of the same struct literal and easy to regress by accident:
        // see this file's `CliInsertionBackend` doc comment for why
        // `is_ax_writable` must stay `false` unconditionally.
        for status in [SecureFieldStatus::Known(true), SecureFieldStatus::Known(false), SecureFieldStatus::Unknown] {
            assert!(!secure_status_to_target(status).is_ax_writable);
        }
    }
}
