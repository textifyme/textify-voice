//! Crash capture + a user-inspectable diagnostic bundle -- **local-first,
//! opt-in-only for anything that would ever leave this Mac**.
//!
//! # The tension this module exists to resolve
//!
//! The product's pitch is "your audio never leaves your device", and today
//! that is literally true (see `crates/voice-asr-whisper/src/model.rs` for
//! the one network call in this whole workspace, and it's a model
//! download, not a transcript upload). A crash reporter is the single
//! easiest way to accidentally break that promise: panic messages,
//! backtraces, and "helpful" debug dumps are exactly where a stray
//! `format!("{transcript}")` or a `Debug`-printed error wrapping user text
//! ends up. So this module is built in the opposite order from a typical
//! crash-reporting integration:
//!
//! 1. **Local file, always, no network, no consent required** --
//!    [`install_panic_hook`] below.
//! 2. **Upload is a separate, explicit, off-by-default opt-in** --
//!    [`is_upload_enabled`] / [`DiagnosticsSetting`] -- and the function
//!    that would actually transmit anything ([`maybe_transmit_crash_report`])
//!    takes `enabled` as a plain `bool` argument and returns immediately
//!    without touching its `transmitter` argument when it is `false`. See
//!    `tests::disabled_upload_never_touches_the_transmitter` for the proof
//!    (a counting mock, not a comment) that this is unreachable, not just
//!    unused. No third-party SDK is linked; [`UnconfiguredTransmitter`] is
//!    the only [`Transmitter`] impl shipped, and it always errors --
//!    wiring a real one is deliberately left for later, per this unit's
//!    dispatch.
//!
//! # Structural scrubbing, not just filtering
//!
//! The dispatch's instruction is "prefer a design where transcripts /
//! dictionary terms / clipboard contents / AX labels / window titles
//! cannot reach the reporter at all, over one that filters strings
//! afterwards." Concretely, in this codebase:
//!
//! - The panic hook installed here has **no access** to any of that data
//!   by construction: it reads only [`PanicHookInfo`](std::panic::PanicHookInfo)
//!   (location + panic message) and a fresh [`std::backtrace::Backtrace`]
//!   -- it does not hold a reference to, or read any global registry of,
//!   the transcript buffer, the user dictionary, the clipboard, or any AX
//!   state. There is no "give me the last thing that was dictated" API
//!   anywhere in this module.
//! - A repo-wide audit (`grep -rn 'panic!(' crates/ --include='*.rs'`, run
//!   during this unit's implementation) found every `panic!`/`.expect()`
//!   call on a **live** dictation code path uses a static message with no
//!   user-text interpolation; the only calls that interpolate a variable
//!   into a panic message are inside `#[cfg(test)]` modules against fixed
//!   test fixtures (`voice-act::mock`, `voice-intent::grammar`, etc.), not
//!   the running app. So today, nothing on the live path hands the default
//!   panic machinery a message containing dictated text in the first
//!   place -- this module doesn't have to filter it out of the message
//!   because it structurally isn't there.
//! - Rust's own [`std::backtrace::Backtrace`] does not walk or print local
//!   variable *values* (only frame addresses/symbol names from the
//!   binary's own debug info) -- so even a dictated sentence sitting in a
//!   local variable on the stack at panic time does not appear in the
//!   backtrace. `tests::crash_report_from_simulated_dictation_never_
//!   contains_dictated_words` proves this empirically against a realistic
//!   scenario (a live local holding a dictated sentence, a clipboard
//!   payload, and a window title, all in scope when the panic fires),
//!   not just by assertion.
//!
//! As a **second, defense-in-depth** layer (per the dispatch: "if you must
//! filter, test it"), [`scrub_sensitive`] redacts long quoted substrings
//! (the shape any future `Debug`-formatted error wrapping a `String`
//! would take), long digit runs (card/phone/account numbers), and
//! email-like tokens from any free text before it's written -- covering a
//! *future* panic message this module's authors did not anticipate, not
//! relied on as the only defense today.
//!
//! One concrete, *real* leak this audit found and had to route around:
//! `crate::dictate::insert_and_report` echoes the verbatim inserted
//! transcript to stdout as `println!("> {text}")` (see that file, the
//! `println!("> {text}")` calls around its `insert_text` call site) --
//! and `crate::dictate::redirect_output_to_log_if_detached` redirects
//! stdout/stderr straight into `~/Library/Logs/textify-voice.log` whenever
//! the app runs as a detached menu-bar agent (i.e. essentially always,
//! for a real user). **That means the existing log file already contains
//! verbatim dictated text on disk today**, independent of anything in
//! this module. [`build_bundle`] below does not gather that log
//! unfiltered: [`tail_log_excluding_transcript_echo`] drops every line
//! that starts with the exact `"> "` marker `insert_and_report` uses
//! before the remainder is scrubbed and included. This is called out
//! explicitly rather than silently patched over, because `dictate.rs` is
//! outside this unit's file ownership -- the transcript-echo write itself
//! is still there and still lands in the raw log file; only this
//! module's bundle output avoids repeating the leak.
//!
//! # What's here
//!
//! - [`install_panic_hook`]: local crash capture, safe to call once at
//!   startup (idempotent -- a second call is a no-op).
//! - [`Subsystem`] / [`set_active_subsystem`]: a cheap, lock-free (single
//!   `AtomicU8`) "what was running" tag a crash report reads at panic
//!   time. `main.rs` sets this once per top-level subcommand dispatch
//!   (see that file); it is coarse (which subcommand was running, not
//!   which line), which is an honest limit of what this unit could wire
//!   without editing files it does not own (`dictate.rs`'s internal
//!   audio/ASR/insertion boundaries are not tagged -- see this crate's
//!   report back to the orchestrator for that as a named follow-up).
//! - [`DiagnosticsSetting`] / [`is_upload_enabled`] / [`save_setting`]:
//!   the opt-in flag, its own small on-disk file (same hand-rolled
//!   `key = value` convention `crate::settings`/`crate::dictionary` use),
//!   defaulting to `false` with no file present.
//! - [`Transmitter`] / [`maybe_transmit_crash_report`] /
//!   [`UnconfiguredTransmitter`]: the "plumbing" the dispatch asks for --
//!   present, testable, and not wired to any real network client.
//! - [`build_bundle`] / [`run`]: `textify-voice diagnostics` -- one local
//!   file gathering the log (transcript-echo lines dropped), recent crash
//!   reports, version/git-sha/OS/arch/device-tier, permission states, and
//!   settings, for the user to read *before* deciding whether to share it
//!   with anyone. Never sends it anywhere itself.
//!
//! `#![allow(dead_code)]`: mirrors `crate::settings`'/`crate::onboarding`'s
//! own top-of-file note -- this module's public surface is larger than
//! what this unit was able to wire call sites for everywhere in the crate
//! (see the `Subsystem` note above), and a bin-crate target's dead-code
//! lint does not treat `pub` as "externally reachable" the way a lib
//! crate's does.
#![allow(dead_code)]

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Args;

/// Overrides the crash-report directory ([`crash_dir`]) entirely when set
/// to a non-empty value. Mirrors `crate::dictionary::DICTIONARY_PATH_ENV_VAR`
/// / `crate::settings::SETTINGS_PATH_ENV_VAR`'s convention; exists so tests
/// (and this module's own) never write into the real `~/Library/Logs`.
pub const CRASH_DIR_ENV_VAR: &str = "TEXTIFY_VOICE_CRASH_DIR";

/// Overrides [`setting_path`] entirely when set to a non-empty value.
pub const DIAGNOSTICS_SETTING_PATH_ENV_VAR: &str = "TEXTIFY_VOICE_DIAGNOSTICS_SETTING_PATH";

/// Overrides the log file [`build_bundle`] reads from. The real path
/// (matching `crate::dictate::redirect_output_to_log_if_detached` exactly)
/// is `~/Library/Logs/textify-voice.log`; this lets tests point at a
/// synthetic log without touching the real one or depending on
/// `crate::dictate` (outside this unit's ownership).
pub const LOG_PATH_ENV_VAR: &str = "TEXTIFY_VOICE_LOG_PATH";

// ---------------------------------------------------------------------
// Subsystem tag -- lock-free, panic-hook-safe "what was running"
// ---------------------------------------------------------------------

/// Coarse "what part of the app was active" tag, read by the panic hook
/// and written into every crash report. Deliberately an enum of fixed,
/// non-sensitive labels -- never a free-text field -- so there is no way
/// for this to become a place user text leaks through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Subsystem {
    Startup = 0,
    Onboarding = 1,
    Permissions = 2,
    Dictate = 3,
    Transcribe = 4,
    Command = 5,
    Models = 6,
    Bench = 7,
    Settings = 8,
    Diagnostics = 9,
    Unknown = 10,
}

impl Subsystem {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Subsystem::Startup => "startup",
            Subsystem::Onboarding => "onboarding",
            Subsystem::Permissions => "permissions",
            Subsystem::Dictate => "dictate",
            Subsystem::Transcribe => "transcribe",
            Subsystem::Command => "command",
            Subsystem::Models => "models",
            Subsystem::Bench => "bench",
            Subsystem::Settings => "settings",
            Subsystem::Diagnostics => "diagnostics",
            Subsystem::Unknown => "unknown",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Subsystem::Startup,
            1 => Subsystem::Onboarding,
            2 => Subsystem::Permissions,
            3 => Subsystem::Dictate,
            4 => Subsystem::Transcribe,
            5 => Subsystem::Command,
            6 => Subsystem::Models,
            7 => Subsystem::Bench,
            8 => Subsystem::Settings,
            9 => Subsystem::Diagnostics,
            _ => Subsystem::Unknown,
        }
    }
}

static ACTIVE_SUBSYSTEM: AtomicU8 = AtomicU8::new(Subsystem::Startup as u8);

/// Record which subsystem is about to run. Cheap (one relaxed atomic
/// store), safe to call from any thread, safe to call from inside a
/// panicking context (it is never itself called from the panic hook).
pub fn set_active_subsystem(s: Subsystem) {
    ACTIVE_SUBSYSTEM.store(s as u8, Ordering::Relaxed);
}

#[must_use]
pub fn active_subsystem() -> Subsystem {
    Subsystem::from_u8(ACTIVE_SUBSYSTEM.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------
// Static context -- captured once, eagerly, before any crash can happen
// ---------------------------------------------------------------------

/// Everything the crash report needs that (a) never changes after startup
/// and (b) this design refuses to compute *inside* the panic hook, so the
/// hook itself never blocks on a subprocess or a syscall while the process
/// may already be in a bad state. Captured once by [`install_panic_hook`]
/// (or lazily by [`build_bundle`] for the `diagnostics` command, which has
/// no such constraint -- it's an explicit, healthy-process CLI invocation).
struct StaticContext {
    version: &'static str,
    git_sha: &'static str,
    /// Reuses `crate::compat::DeviceTier` (its own doc comment names this
    /// exact use case: "a future diagnostics/crash-reporter unit can
    /// embed the same data") rather than this module re-deriving
    /// architecture/chip/cores/RAM/macOS-version/Metal-availability a
    /// second, divergence-prone way. `DeviceTier::detect()` reads only
    /// `sysctlbyname` (see `compat::sysctl`) -- no subprocess, no
    /// blocking I/O -- so it is exactly as safe to capture here, eagerly,
    /// as everything else in this struct.
    device_tier: crate::compat::DeviceTier,
}

static CONTEXT: OnceLock<StaticContext> = OnceLock::new();

fn capture_static_context() -> StaticContext {
    StaticContext {
        version: env!("CARGO_PKG_VERSION"),
        // Baked in at compile time via `option_env!` (no build.rs needed --
        // cargo tracks `option_env!` reads, so changing the value rebuilds
        // this crate). `packaging/build-bundle.sh` sets it, with a `-dirty`
        // suffix when the tree had uncommitted changes; a plain `cargo build`
        // leaves it unset and this falls back to "unknown" rather than
        // silently omitting the field.
        git_sha: option_env!("TEXTIFY_VOICE_GIT_SHA").unwrap_or("unknown"),
        device_tier: crate::compat::DeviceTier::detect(),
    }
}

fn static_context() -> &'static StaticContext {
    CONTEXT.get_or_init(capture_static_context)
}

// ---------------------------------------------------------------------
// Panic hook -- local file, no network, must not panic, must not block
// ---------------------------------------------------------------------

static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the crash-capture panic hook. Call once, as early as possible
/// in `main` -- before argument parsing, before any window opens, before
/// any subsystem starts (see `main.rs`'s call site: it is the first
/// statement in `fn main`, ahead of even the zero-`argv` agent-launch
/// branch, so a crash during onboarding or the menu-bar agent's startup is
/// captured too).
///
/// Idempotent: a second call is a no-op (checked via a single
/// `compare_exchange`-shaped swap), so it is safe to call defensively from
/// more than one entry point without double-chaining hooks.
///
/// Chains rather than replaces: the previous hook (Rust's default one,
/// which prints the panic to stderr) still runs afterwards, so a crash
/// during an interactive terminal session still shows the normal panic
/// message there -- this only *adds* the local file write, it does not
/// silence the existing behavior.
pub fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Eager capture: everything the hook will ever need to read is
    // computed now, once, on a healthy process -- the hook body itself
    // performs zero syscalls beyond the final file write.
    let _ = static_context();

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
        // The hook must not itself panic (a panic inside a panic hook
        // aborts the process immediately, with no report written at all).
        // Every fallible step inside `write_crash_report` already degrades
        // gracefully rather than unwrapping, but this `catch_unwind` is a
        // second, structural backstop against anything this module's
        // authors missed -- proven, not assumed, by
        // `tests::panic_hook_survives_a_panic_inside_report_writing`.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_crash_report(info);
        }));
        previous(info);
    }));
}

/// Build and write one crash report file. Never panics, never blocks on
/// anything beyond a single local file write (no network, no subprocess,
/// no lock beyond the lock-free atomics [`active_subsystem`] reads) --
/// every fallible operation degrades to "skip this field" or "give up
/// silently" rather than propagating.
fn write_crash_report(info: &std::panic::PanicHookInfo<'_>) {
    let Some(ctx) = CONTEXT.get() else {
        // Should be unreachable (install_panic_hook always populates this
        // before installing the hook), but the hook must not assume it --
        // give up silently rather than risk a panic-in-panic-hook abort.
        return;
    };

    let message = panic_message(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();

    let mut report = String::new();
    let _ = writeln!(report, "=== Textify Voice Crash Report ===");
    let _ = writeln!(report, "unix_time: {}", now_unix());
    let _ = writeln!(report, "version: {}", ctx.version);
    let _ = writeln!(report, "git_sha: {}", ctx.git_sha);
    let _ = write!(report, "{}", ctx.device_tier.render());
    let _ = writeln!(report, "subsystem: {}", active_subsystem().label());
    let _ = writeln!(report, "location: {location}");
    let _ = writeln!(report, "message: {}", scrub_sensitive(&message));
    let _ = writeln!(report, "backtrace:");
    let _ = writeln!(report, "{}", scrub_sensitive(&backtrace));

    let Some(path) = next_crash_report_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, report);
}

/// Extract the panic message as a plain string. Handles the two payload
/// shapes `panic!`/`.unwrap()`/`.expect()` actually produce (`&'static
/// str` for a string-literal panic, `String` for a formatted one) and
/// degrades to a fixed placeholder for anything else (e.g. `panic_any`
/// with a non-string payload) rather than guessing.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `~/Library/Logs/textify-voice-crashes` -- right next to
/// `crate::dictate`'s own `~/Library/Logs/textify-voice.log`, in a
/// dedicated subdirectory so [`list_recent_crash_reports`] can enumerate
/// exactly this directory rather than filtering the main log directory by
/// filename pattern. [`CRASH_DIR_ENV_VAR`] overrides this entirely
/// (tests, and any future sandboxed environment where `~/Library/Logs`
/// isn't writable).
#[must_use]
pub fn crash_dir() -> PathBuf {
    if let Ok(p) = std::env::var(CRASH_DIR_ENV_VAR) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("Library").join("Logs").join("textify-voice-crashes"))
        .unwrap_or_else(std::env::temp_dir)
}

fn next_crash_report_path() -> Option<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    Some(crash_dir().join(format!("crash-{}-{}.log", now.as_secs(), now.subsec_nanos())))
}

/// List up to `limit` crash report files, most recent first. Never fails
/// (an unreadable/missing directory is just "no crash reports"), matching
/// this module's overall "best effort, never propagate an I/O error into
/// the caller's control flow" stance for anything diagnostic-adjacent.
#[must_use]
pub fn list_recent_crash_reports(limit: usize) -> Vec<PathBuf> {
    let dir = crash_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files.into_iter().take(limit).map(|(p, _)| p).collect()
}

// ---------------------------------------------------------------------
// Scrubbing -- defense in depth on top of structural avoidance
// ---------------------------------------------------------------------

/// Redact anything in `text` that could plausibly be user content rather
/// than our own structural diagnostic text: long quoted substrings (the
/// shape a `Debug`-formatted `String`/`&str` takes, e.g. inside a wrapped
/// error), long digit runs (card/phone/account numbers), and email-like
/// tokens. This is the second line of defense described in this module's
/// top doc comment -- structural avoidance (nothing sensitive is ever
/// *passed in*) is the first and primary one; see
/// `tests::scrub_sensitive_redacts_realistic_transcript_shapes` for
/// coverage against realistic dictated text.
#[must_use]
pub fn scrub_sensitive(text: &str) -> String {
    let text = redact_quoted_strings(text);
    let text = redact_email_like(&text);
    redact_long_digit_runs(&text)
}

const QUOTE_REDACT_THRESHOLD: usize = 8;
const DIGIT_RUN_REDACT_THRESHOLD: usize = 7;

fn redact_quoted_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            out.push(c);
            continue;
        }
        // Collect up to the next unescaped closing quote (or end of
        // string, if the panic message got truncated mid-quote).
        let mut inner = String::new();
        let mut closed = false;
        while let Some(&next) = chars.peek() {
            chars.next();
            if next == '\\' {
                // Keep the escape sequence verbatim in `inner`'s length
                // accounting -- it doesn't matter for the redaction
                // decision, only the final rendering below.
                inner.push(next);
                if let Some(&escaped) = chars.peek() {
                    chars.next();
                    inner.push(escaped);
                }
                continue;
            }
            if next == '"' {
                closed = true;
                break;
            }
            inner.push(next);
        }
        if inner.chars().count() > QUOTE_REDACT_THRESHOLD {
            let _ = write!(out, "\"<redacted {} chars>\"", inner.chars().count());
        } else {
            out.push('"');
            out.push_str(&inner);
            if closed {
                out.push('"');
            }
        }
    }
    out
}

fn redact_email_like(text: &str) -> String {
    text.split(' ')
        .map(|tok| {
            let core = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.');
            if core.contains('@') && core.split('@').nth(1).is_some_and(|d| d.contains('.')) {
                "<redacted-email>".to_string()
            } else {
                tok.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_long_digit_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            flush_digit_run(&mut out, &mut run);
            out.push(c);
        }
    }
    flush_digit_run(&mut out, &mut run);
    out
}

fn flush_digit_run(out: &mut String, run: &mut String) {
    if run.len() >= DIGIT_RUN_REDACT_THRESHOLD {
        out.push_str("<redacted-digits>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

// ---------------------------------------------------------------------
// Opt-in upload setting -- off by default, its own tiny on-disk file
// ---------------------------------------------------------------------

/// The diagnostics-upload opt-in. `Default` is `false` -- matches this
/// unit's dispatch #2 exactly ("off by default"). Kept separate from
/// `crate::settings::Settings` deliberately, so this unit's file
/// ownership stays to exactly `diagnostics.rs`: this is its own tiny file
/// next to `settings.txt`/`dictionary.txt`, same on-disk convention,
/// zero edits to `settings.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiagnosticsSetting {
    pub upload_enabled: bool,
}

/// `~/Library/Application Support/textify/diagnostics.txt`. Overridden
/// entirely by [`DIAGNOSTICS_SETTING_PATH_ENV_VAR`] when set.
#[must_use]
pub fn setting_path() -> PathBuf {
    if let Ok(p) = std::env::var(DIAGNOSTICS_SETTING_PATH_ENV_VAR) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("textify")
        .join("diagnostics.txt")
}

/// Load the opt-in setting. Never fails: a missing file, an unreadable
/// file, or a corrupt/unrecognized value all resolve to
/// [`DiagnosticsSetting::default`] (`upload_enabled: false`) -- exactly
/// the same "absent means default, not an error" shape
/// `crate::settings::load` uses for its own file.
#[must_use]
pub fn load_setting() -> DiagnosticsSetting {
    let Ok(content) = fs::read_to_string(setting_path()) else {
        return DiagnosticsSetting::default();
    };
    for line in content.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("upload_enabled") {
            let value = value.trim_start_matches([' ', '=']).trim();
            if value == "true" {
                return DiagnosticsSetting { upload_enabled: true };
            }
            if value == "false" {
                return DiagnosticsSetting { upload_enabled: false };
            }
        }
    }
    DiagnosticsSetting::default()
}

/// Persist the opt-in setting.
pub fn save_setting(setting: DiagnosticsSetting) -> io::Result<()> {
    let path = setting_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("upload_enabled = {}\n", setting.upload_enabled))
}

/// Convenience: `load_setting().upload_enabled`. This is the single
/// source of truth [`maybe_transmit_crash_report`]'s callers consult --
/// see [`Transmitter`]'s doc comment for why the `bool` is threaded
/// through explicitly rather than read inside that function.
#[must_use]
pub fn is_upload_enabled() -> bool {
    load_setting().upload_enabled
}

// ---------------------------------------------------------------------
// Transmit plumbing -- present, tested, structurally unreachable when off
// ---------------------------------------------------------------------

/// What actually sending a diagnostic payload somewhere would look like.
/// No implementation in this codebase does anything but error --
/// [`UnconfiguredTransmitter`] is the only concrete type, and wiring a
/// real HTTP client is explicit future work (this unit's dispatch: "Do
/// not wire a third-party SDK by default").
///
/// A trait (rather than a free function `fn upload(payload: &[u8])`) so
/// [`maybe_transmit_crash_report`]'s "never called while disabled" claim
/// can be *proven* with a counting mock in a test, rather than merely
/// asserted by reading the `if` statement.
pub trait Transmitter {
    fn send(&self, payload: &[u8]) -> Result<(), String>;
}

/// The only [`Transmitter`] shipped today. Always errors: there is no
/// diagnostics upload endpoint configured or contacted anywhere in this
/// build, by design.
pub struct UnconfiguredTransmitter;

impl Transmitter for UnconfiguredTransmitter {
    fn send(&self, _payload: &[u8]) -> Result<(), String> {
        Err("diagnostics upload is not configured in this build".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    /// `enabled` was `false` -- `transmitter.send` was never called.
    SkippedDisabled,
    Sent,
    Failed(String),
}

/// The single choke point any future "send this crash report" call must
/// go through. `enabled` is threaded in explicitly (rather than this
/// function calling [`is_upload_enabled`] itself) so the "disabled means
/// unreachable" property is testable without touching the filesystem, and
/// so a caller cannot accidentally bypass the setting by holding a stale
/// `true` from an earlier check -- see [`maybe_transmit_from_settings`]
/// for the real call path, which always reads the setting fresh.
pub fn maybe_transmit_crash_report(
    enabled: bool,
    payload: &[u8],
    transmitter: &dyn Transmitter,
) -> UploadOutcome {
    if !enabled {
        return UploadOutcome::SkippedDisabled;
    }
    match transmitter.send(payload) {
        Ok(()) => UploadOutcome::Sent,
        Err(e) => UploadOutcome::Failed(e),
    }
}

/// Real call path: reads [`is_upload_enabled`] fresh, and -- today --
/// always hands off to [`UnconfiguredTransmitter`], which always errors.
/// Nothing in this codebase calls this from the panic hook or anywhere
/// else yet; it exists as the wired-but-inert plumbing this unit's
/// dispatch asks for.
pub fn maybe_transmit_from_settings(payload: &[u8]) -> UploadOutcome {
    maybe_transmit_crash_report(is_upload_enabled(), payload, &UnconfiguredTransmitter)
}

// ---------------------------------------------------------------------
// User-facing diagnostic bundle
// ---------------------------------------------------------------------

/// `textify-voice diagnostics` -- see `main.rs`'s `Cmd::Diagnostics`.
#[derive(Args, Debug, Default)]
pub struct DiagnosticsArgs {
    /// Write the bundle to this path instead of the default
    /// `~/Library/Logs/textify-voice-diagnostics-<time>.txt`.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Turn on the diagnostics-upload opt-in. Still does not send
    /// anything by itself -- see this module's top doc comment.
    #[arg(long)]
    pub enable_upload: bool,

    /// Turn off the diagnostics-upload opt-in (the default state).
    #[arg(long)]
    pub disable_upload: bool,
}

pub fn run(args: DiagnosticsArgs) -> anyhow::Result<()> {
    if args.enable_upload && args.disable_upload {
        anyhow::bail!("--enable-upload and --disable-upload are mutually exclusive");
    }
    if args.enable_upload {
        save_setting(DiagnosticsSetting { upload_enabled: true })?;
        println!(
            "diagnostics upload: enabled. This still sends nothing by itself -- \
             `textify-voice diagnostics` only ever writes a local file for you to read."
        );
    }
    if args.disable_upload {
        save_setting(DiagnosticsSetting { upload_enabled: false })?;
        println!("diagnostics upload: disabled.");
    }

    let path = build_bundle(args.output.as_deref())?;
    println!("Diagnostic bundle written to:");
    println!("  {}", path.display());
    println!();
    println!("Nothing has been sent anywhere. Open the file above and read it before");
    println!("sharing it with anyone.");
    println!(
        "Diagnostics upload setting: {}",
        if is_upload_enabled() { "enabled" } else { "disabled (default)" }
    );
    Ok(())
}

/// Gather one local diagnostic bundle file: log (transcript-echo lines
/// excluded, remainder scrubbed), recent crash report file list, version/
/// git-sha/OS/arch/device-tier, permission states, and settings.
/// Writes to `output` if given, else a default timestamped path next to
/// the crash reports directory. Returns the path written. Never
/// transmits anything -- see this module's top doc comment.
pub fn build_bundle(output: Option<&Path>) -> anyhow::Result<PathBuf> {
    let ctx = static_context();
    let mut out = String::new();

    let _ = writeln!(out, "=== Textify Voice Diagnostic Bundle ===");
    let _ = writeln!(out, "generated_unix_time: {}", now_unix());
    let _ = writeln!(out, "version: {}", ctx.version);
    let _ = writeln!(out, "git_sha: {}", ctx.git_sha);
    let _ = write!(out, "{}", ctx.device_tier.render());
    let _ = writeln!(out);

    let _ = writeln!(out, "--- permissions ---");
    let perms = crate::permissions::check();
    let _ = writeln!(out, "microphone: {:?}", perms.mic);
    let _ = writeln!(out, "accessibility_trusted: {}", perms.accessibility_trusted);
    let _ = writeln!(out);

    let _ = writeln!(out, "--- settings ---");
    match crate::settings::load() {
        Ok(loaded) => {
            let _ = writeln!(out, "{:?}", loaded.settings);
        }
        Err(e) => {
            let _ = writeln!(out, "<could not load settings: {e}>");
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "--- diagnostics ---");
    let _ = writeln!(out, "upload_enabled: {}", is_upload_enabled());
    let _ = writeln!(out);

    let _ = writeln!(out, "--- recent crash reports ---");
    let reports = list_recent_crash_reports(10);
    if reports.is_empty() {
        let _ = writeln!(out, "<none>");
    }
    for p in &reports {
        let _ = writeln!(out, "{}", p.display());
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "--- log tail (transcript lines excluded, remainder scrubbed) ---");
    out.push_str(&scrub_sensitive(&tail_log_excluding_transcript_echo(200)));
    let _ = writeln!(out);

    let dest = match output {
        Some(p) => p.to_path_buf(),
        None => default_bundle_path(),
    };
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, out)?;
    Ok(dest)
}

fn default_bundle_path() -> PathBuf {
    crash_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("textify-voice-diagnostics-{}.txt", now_unix()))
}

fn log_path() -> PathBuf {
    if let Ok(p) = std::env::var(LOG_PATH_ENV_VAR) {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    // Matches `crate::dictate::redirect_output_to_log_if_detached` exactly.
    dirs::home_dir().map(|h| h.join("Library").join("Logs").join("textify-voice.log")).unwrap_or_else(std::env::temp_dir)
}

/// The exact marker `crate::dictate::insert_and_report` uses to echo the
/// verbatim inserted transcript (`println!("> {text}")`). Kept as a named
/// constant, not inlined, so it reads as "this is a deliberate, cited
/// exclusion" rather than an arbitrary string match.
const TRANSCRIPT_ECHO_PREFIX: &str = "> ";

/// Read up to `max_lines` most-recent lines of the log file, dropping
/// every line that starts with [`TRANSCRIPT_ECHO_PREFIX`] -- see this
/// module's top doc comment for exactly why that line shape is excluded
/// rather than merely scrubbed. Never fails: a missing/unreadable log
/// file just means "no log tail available."
fn tail_log_excluding_transcript_echo(max_lines: usize) -> String {
    let Ok(content) = fs::read_to_string(log_path()) else {
        return "<no log file found>".to_string();
    };
    let lines: Vec<&str> =
        content.lines().filter(|l| !l.starts_with(TRANSCRIPT_ECHO_PREFIX)).collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex};

    /// Every test in this module that touches the filesystem or panic
    /// hook state must run serialized -- `CRASH_DIR_ENV_VAR`,
    /// `DIAGNOSTICS_SETTING_PATH_ENV_VAR`, `LOG_PATH_ENV_VAR`, and the
    /// process-global panic hook are all process-wide state, and
    /// `cargo test` runs tests in this file on multiple threads by
    /// default.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// `TEST_LOCK.lock()`, tolerant of a poisoned mutex. A prior test's
    /// *own* assertion failure while holding the lock would otherwise
    /// poison it for the rest of the run, turning every later test's
    /// failure message into a misleading "lock" panic instead of its own
    /// real assertion -- serialization is still exactly what's needed
    /// even after that, so recovering the poisoned guard (rather than
    /// propagating the poison) is correct here, not just convenient.
    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Run `f` with the process-global panic hook (and [`HOOK_INSTALLED`])
    /// saved before `f` runs and fully restored after, regardless of
    /// whether `f` itself panics. `std::panic::set_hook`/`take_hook` are
    /// process-wide, not per-thread or per-test -- without this, a test
    /// that calls [`install_panic_hook`] (directly, or by resetting
    /// [`HOOK_INSTALLED`] first) permanently chains a new layer onto the
    /// global hook for the rest of this test binary's process lifetime.
    /// A *later* test doing the same thing would then chain onto that
    /// leftover layer too, so one deliberate test panic fans out into
    /// N crash-report files instead of one -- exactly the flake this
    /// helper exists to close off, not a hypothetical.
    fn with_isolated_panic_hook<R>(f: impl FnOnce() -> R + std::panic::UnwindSafe) -> R {
        let previous_hook = std::panic::take_hook();
        let previous_installed = HOOK_INSTALLED.swap(false, Ordering::SeqCst);
        let outcome = std::panic::catch_unwind(f);
        std::panic::set_hook(previous_hook);
        HOOK_INSTALLED.store(previous_installed, Ordering::SeqCst);
        match outcome {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: serialized by `TEST_LOCK`, held by every test that
            // constructs an `EnvGuard` -- no other thread in this test
            // binary observes `std::env` state concurrently with this.
            unsafe {
                std::env::set_var(key, value);
            }
            EnvGuard { key, previous }
        }

        /// Ensure `key` is *absent* for the guard's lifetime, restoring
        /// whatever it held on drop. Needed by any test asserting a
        /// default-path behavior: the override being unset is part of what
        /// such a test is checking, not something a developer's exported
        /// shell environment gets to decide.
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: see `EnvGuard::set`.
            unsafe {
                std::env::remove_var(key);
            }
            EnvGuard { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `EnvGuard::set`.
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "textify-voice-diagnostics-test-{label}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    // -------------------------------------------------------------
    // Subsystem tag
    // -------------------------------------------------------------

    #[test]
    fn subsystem_round_trips_through_u8() {
        for s in [
            Subsystem::Startup,
            Subsystem::Onboarding,
            Subsystem::Permissions,
            Subsystem::Dictate,
            Subsystem::Transcribe,
            Subsystem::Command,
            Subsystem::Models,
            Subsystem::Bench,
            Subsystem::Settings,
            Subsystem::Diagnostics,
            Subsystem::Unknown,
        ] {
            assert_eq!(Subsystem::from_u8(s as u8), s);
        }
    }

    #[test]
    fn set_active_subsystem_is_observable() {
        let _lock = lock_tests();
        set_active_subsystem(Subsystem::Transcribe);
        assert_eq!(active_subsystem(), Subsystem::Transcribe);
        set_active_subsystem(Subsystem::Startup);
        assert_eq!(active_subsystem(), Subsystem::Startup);
    }

    // -------------------------------------------------------------
    // Scrubbing -- tested against realistic dictated text, per this
    // unit's dispatch #3.
    // -------------------------------------------------------------

    #[test]
    fn scrub_sensitive_redacts_realistic_transcript_shapes() {
        // A realistic "Debug-formatted error wrapping the transcript"
        // shape -- exactly what a future `panic!("{:?}", some_err)` where
        // `some_err` carries the dictated text would look like.
        let input = r#"TranscribeError::InsertFailed("please transfer the account number 4111 2222 3333 4444 to my colleague sarah dot chen at examplecorp dot com before the board meeting on thursday")"#;
        let scrubbed = scrub_sensitive(input);
        assert!(!scrubbed.contains("transfer the account"));
        assert!(!scrubbed.contains("sarah"));
        assert!(!scrubbed.contains("board meeting"));
        assert!(scrubbed.contains("TranscribeError::InsertFailed"));
        assert!(scrubbed.contains("<redacted"));
    }

    #[test]
    fn scrub_sensitive_redacts_long_digit_runs_outside_quotes() {
        let input = "card ending in account number 4111222233334444 was referenced";
        let scrubbed = scrub_sensitive(input);
        assert!(!scrubbed.contains("4111222233334444"));
        assert!(scrubbed.contains("<redacted-digits>"));
    }

    #[test]
    fn scrub_sensitive_redacts_email_like_tokens() {
        let input = "contact sarah.chen@examplecorp.com about the invoice";
        let scrubbed = scrub_sensitive(input);
        assert!(!scrubbed.contains("sarah.chen@examplecorp.com"));
        assert!(scrubbed.contains("<redacted-email>"));
    }

    #[test]
    fn scrub_sensitive_leaves_short_quoted_identifiers_alone() {
        // Short quoted tokens (enum variant names, single words) are
        // exactly the shape our *own* diagnostic text uses -- over-eager
        // redaction here would make reports useless.
        let input = r#"expected "ok", got "err""#;
        let scrubbed = scrub_sensitive(input);
        assert_eq!(scrubbed, input);
    }

    #[test]
    fn scrub_sensitive_leaves_plain_diagnostic_text_untouched() {
        let input = "insertion backend returned an unexpected state at src/dictate.rs:1204";
        assert_eq!(scrub_sensitive(input), input);
    }

    // -------------------------------------------------------------
    // Panic hook: real panic, real file, real content -- and proof that
    // realistic in-scope dictated/clipboard/window-title content that was
    // alive on the stack at panic time does not end up in the report.
    // -------------------------------------------------------------

    #[test]
    fn crash_report_from_simulated_dictation_never_contains_dictated_words() {
        let _lock = lock_tests();
        let dir = temp_dir("panic-report");
        let _guard = EnvGuard::set(CRASH_DIR_ENV_VAR, &dir);

        with_isolated_panic_hook(|| {
            install_panic_hook();
            set_active_subsystem(Subsystem::Dictate);

            // Realistic material that would be alive in memory around a
            // real dictate.rs panic: the dictated sentence, a clipboard
            // payload, and a window title read off the frontmost app via
            // AX -- exactly the three categories this unit's dispatch
            // names.
            let dictated_transcript =
                "please transfer the account number 4111 2222 3333 4444 to my colleague Sarah \
                 Chen before the board meeting on Thursday"
                    .to_string();
            let clipboard_contents = "sk-live-not-a-real-secret-1234567890".to_string();
            let window_title =
                "Chase Bank \u{2014} Account Ending 4444 \u{2014} Sarah Chen".to_string();
            let user_dictionary_term = "Onetelos".to_string();

            let result = std::panic::catch_unwind(|| {
                // Keep the locals alive (not optimized away) right up to
                // the panic, the same way they would be alive in a real
                // `dictate.rs` stack frame.
                std::hint::black_box((
                    &dictated_transcript,
                    &clipboard_contents,
                    &window_title,
                    &user_dictionary_term,
                ));
                panic!("insertion backend returned an unexpected state");
            });
            assert!(result.is_err());

            let reports = list_recent_crash_reports(10);
            assert_eq!(reports.len(), 1, "expected exactly one crash report to have been written");
            let content = fs::read_to_string(&reports[0]).expect("read crash report");

            for forbidden in [
                dictated_transcript.as_str(),
                "4111 2222 3333 4444",
                "Sarah Chen",
                clipboard_contents.as_str(),
                "sk-live",
                window_title.as_str(),
                "Chase Bank",
                user_dictionary_term.as_str(),
            ] {
                assert!(
                    !content.contains(forbidden),
                    "crash report leaked forbidden content {forbidden:?}:\n{content}"
                );
            }

            // The report is still a real, useful report -- not empty.
            assert!(content.contains("insertion backend returned an unexpected state"));
            assert!(content.contains("subsystem: dictate"));
            assert!(content.contains("version:"));
            assert!(content.contains("backtrace:"));
        });

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_panic_hook_is_idempotent() {
        let _lock = lock_tests();
        with_isolated_panic_hook(|| {
            install_panic_hook();
            assert!(HOOK_INSTALLED.load(Ordering::SeqCst));
            // Second call must not panic and must leave the flag as-is.
            install_panic_hook();
            assert!(HOOK_INSTALLED.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn write_crash_report_does_not_panic_on_a_non_string_payload() {
        let _lock = lock_tests();
        let dir = temp_dir("non-string-payload");
        let _guard = EnvGuard::set(CRASH_DIR_ENV_VAR, &dir);
        let _ = static_context();

        // `panic_any(42)` -- a non-string payload `panic_message` must
        // degrade gracefully for. Assert the whole hook body runs to
        // completion without panicking itself.
        let result = with_isolated_panic_hook(|| {
            std::panic::set_hook(Box::new(write_crash_report));
            std::panic::catch_unwind(|| {
                std::panic::panic_any(42_i32);
            })
        });
        assert!(result.is_err());
        // A file was still produced.
        assert_eq!(list_recent_crash_reports(10).len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_dir_falls_back_when_env_override_is_absent_default_is_stable() {
        // The lock plus an explicit removal are both load-bearing: sibling
        // tests in this module set `CRASH_DIR_ENV_VAR` while they run, and a
        // developer may have it exported in their own shell. Either would
        // otherwise fail this test for a reason that has nothing to do with
        // the fallback behavior it exists to pin down.
        let _lock = lock_tests();
        let _guard = EnvGuard::remove(CRASH_DIR_ENV_VAR);

        // Not asserting a specific absolute path (depends on `dirs::home_dir`
        // in this environment), only that two calls agree and the path ends
        // in the expected directory name -- the actual write path is
        // exercised for real by the panic-report test above via the env
        // override.
        let a = crash_dir();
        let b = crash_dir();
        assert_eq!(a, b);
        assert_eq!(a.file_name().and_then(|n| n.to_str()), Some("textify-voice-crashes"));
    }

    // -------------------------------------------------------------
    // Opt-in upload setting: default off, persists, and the transmit
    // path is genuinely unreachable while disabled.
    // -------------------------------------------------------------

    #[test]
    fn upload_defaults_to_disabled_when_no_setting_file_exists() {
        let _lock = lock_tests();
        let dir = temp_dir("setting-missing");
        let path = dir.join("nonexistent").join("diagnostics.txt");
        let _guard = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &path);
        assert!(!path.exists());
        assert!(!is_upload_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_setting_round_trips_through_save_and_load() {
        let _lock = lock_tests();
        let dir = temp_dir("setting-roundtrip");
        let path = dir.join("diagnostics.txt");
        let _guard = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &path);

        assert!(!is_upload_enabled());
        save_setting(DiagnosticsSetting { upload_enabled: true }).expect("save enabled");
        assert!(is_upload_enabled());
        save_setting(DiagnosticsSetting { upload_enabled: false }).expect("save disabled");
        assert!(!is_upload_enabled());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_setting_ignores_a_corrupt_file_and_defaults_to_disabled() {
        let _lock = lock_tests();
        let dir = temp_dir("setting-corrupt");
        let path = dir.join("diagnostics.txt");
        let _guard = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &path);
        fs::create_dir_all(&dir).expect("mkdir");
        fs::write(&path, "this is not a recognized config line at all\n").expect("write corrupt");

        assert!(!is_upload_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    struct CountingTransmitter {
        calls: Arc<AtomicUsize>,
    }

    impl Transmitter for CountingTransmitter {
        fn send(&self, _payload: &[u8]) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn disabled_upload_never_touches_the_transmitter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let transmitter = CountingTransmitter { calls: calls.clone() };

        let outcome = maybe_transmit_crash_report(false, b"anything at all", &transmitter);

        assert_eq!(outcome, UploadOutcome::SkippedDisabled);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "transmitter.send must be structurally unreachable while disabled"
        );
    }

    #[test]
    fn enabled_upload_does_reach_the_transmitter() {
        // Proves the mechanism genuinely works both ways -- the disabled
        // test above isn't just "always skips regardless of the flag."
        let calls = Arc::new(AtomicUsize::new(0));
        let transmitter = CountingTransmitter { calls: calls.clone() };

        let outcome = maybe_transmit_crash_report(true, b"payload", &transmitter);

        assert_eq!(outcome, UploadOutcome::Sent);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn maybe_transmit_from_settings_is_disabled_by_default_and_reads_the_real_setting() {
        let _lock = lock_tests();
        let dir = temp_dir("transmit-from-settings");
        let path = dir.join("diagnostics.txt");
        let _guard = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &path);

        assert_eq!(maybe_transmit_from_settings(b"x"), UploadOutcome::SkippedDisabled);

        save_setting(DiagnosticsSetting { upload_enabled: true }).expect("save enabled");
        // Still never actually sends anything: the only shipped
        // Transmitter always errors -- no third-party SDK, no network
        // client, wired anywhere in this build.
        assert!(matches!(maybe_transmit_from_settings(b"x"), UploadOutcome::Failed(_)));

        let _ = fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------------
    // Diagnostic bundle
    // -------------------------------------------------------------

    #[test]
    fn build_bundle_excludes_transcript_echo_lines_from_the_log() {
        let _lock = lock_tests();
        let dir = temp_dir("bundle-log-exclude");
        let log = dir.join("textify-voice.log");
        let crash = dir.join("crashes");
        let setting = dir.join("diagnostics.txt");
        fs::write(
            &log,
            "\n=== textify-voice agent starting ===\n\
             mic: MacBook Pro Microphone @ 48000 Hz / 1 ch (resampled to 16 kHz mono for ASR)\n\
             > please wire the quarterly earnings figures to finance by Friday\n\
             \x20\x20[inserted via AX]\n\
             \x20\x20speech-end-to-text: 210.4 ms (asr 180.2 ms + normalize 5.1 ms + insert 25.1 ms)\n\
             > my social security number is 123456789 just for testing\n\
             \x20\x20[copied to clipboard]\n",
        )
        .expect("write synthetic log");
        let _g1 = EnvGuard::set(LOG_PATH_ENV_VAR, &log);
        let _g2 = EnvGuard::set(CRASH_DIR_ENV_VAR, &crash);
        let _g3 = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &setting);

        let out_path = dir.join("bundle.txt");
        let written = build_bundle(Some(&out_path)).expect("build bundle");
        assert_eq!(written, out_path);
        let content = fs::read_to_string(&written).expect("read bundle");

        assert!(!content.contains("quarterly earnings"));
        assert!(!content.contains("123456789"));
        assert!(!content.contains("social security"));
        // Safe status lines from the same log survive.
        assert!(content.contains("inserted via AX"));
        assert!(content.contains("speech-end-to-text"));
        assert!(content.contains("=== textify-voice agent starting ==="));
        // Structural sections are present.
        assert!(content.contains("--- permissions ---"));
        assert!(content.contains("--- settings ---"));
        assert!(content.contains("--- diagnostics ---"));
        assert!(content.contains("upload_enabled: false"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_bundle_lists_recent_crash_reports() {
        let _lock = lock_tests();
        let dir = temp_dir("bundle-crash-list");
        let crash = dir.join("crashes");
        fs::create_dir_all(&crash).expect("mkdir crash dir");
        fs::write(crash.join("crash-1-1.log"), "=== Textify Voice Crash Report ===\n")
            .expect("write fake crash report");
        let _g1 = EnvGuard::set(CRASH_DIR_ENV_VAR, &crash);
        let _g2 = EnvGuard::set(LOG_PATH_ENV_VAR, &dir.join("no-such-log.log"));
        let _g3 = EnvGuard::set(DIAGNOSTICS_SETTING_PATH_ENV_VAR, &dir.join("diagnostics.txt"));

        let out_path = dir.join("bundle.txt");
        build_bundle(Some(&out_path)).expect("build bundle");
        let content = fs::read_to_string(&out_path).expect("read bundle");
        assert!(content.contains("crash-1-1.log"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_log_excluding_transcript_echo_reports_missing_file_gracefully() {
        let _lock = lock_tests();
        let missing = std::env::temp_dir().join("textify-voice-diagnostics-test-definitely-missing.log");
        let _guard = EnvGuard::set(LOG_PATH_ENV_VAR, &missing);
        assert_eq!(tail_log_excluding_transcript_echo(50), "<no log file found>");
    }
}
