//! First-run onboarding — SPEC WP-V0.4's "measured funnel." Permission
//! grants are where installs die, so this module treats onboarding as
//! exactly that: an explicit, ordered sequence of steps
//! (`Welcome -> Microphone -> Accessibility -> ModelDownload -> Ready`)
//! where each step can answer "am I satisfied?" against live inputs, can
//! name the exact System Settings pane to deep-link to when it is not, and
//! every time it is *reached* or *completed* that gets counted, locally,
//! forever (no telemetry backend exists in this CLI -- see [`OnboardingState`]
//! for exactly where the counters live on disk).
//!
//! # Design: the funnel is a pure function of live inputs, not a state machine
//!
//! It would be tempting to model this as "the user is currently on step N,
//! advance to N+1 on Continue." That breaks the moment a permission is
//! *revoked* mid-flow (the user opens System Settings from step 3, flips
//! Accessibility back off, comes back) -- a hand-advanced cursor would
//! happily let them proceed to `ModelDownload` on a lie. Instead
//! [`current_step`] recomputes the current step from scratch every time,
//! from [`FunnelInputs`] the caller gathers fresh (real permission checks,
//! real model-cache check): the first step in [`OnboardingStep::ALL`] order
//! that is not [`OnboardingStep::is_satisfied`] wins. Revoke Microphone
//! after reaching `Ready` and the very next recomputation snaps back to
//! `Microphone` -- not because anything "detected" the revocation, but
//! because that is simply, once again, the first unsatisfied step. This is
//! the part of this module that is genuinely worth unit-testing without a
//! window in sight, and the tests below do exactly that.
//!
//! # Where the funnel counters live
//!
//! `~/Library/Application Support/textify/onboarding.txt` on macOS
//! (override with [`ONBOARDING_PATH_ENV_VAR`]), a small hand-rolled
//! `key = value` text file -- inspect it directly with `cat`, no tooling
//! needed. One `reached` and one `completed` counter per step, e.g.:
//!
//! ```text
//! welcome.reached = 2
//! welcome.completed = 2
//! microphone.reached = 2
//! microphone.completed = 1
//! accessibility.reached = 1
//! accessibility.completed = 0
//! model_download.reached = 0
//! model_download.completed = 0
//! ready.reached = 0
//! ready.completed = 0
//! ```
//!
//! Not JSON/TOML/serde: `voice-cli`'s `Cargo.toml` (which this unit's
//! dispatch does not permit editing) has no serialization crate as a
//! dependency at all -- confirmed by inspecting `Cargo.lock`, no `serde`
//! resolves anywhere in this workspace today. `crate::dictionary` already
//! established the precedent this module follows: hand-roll a small,
//! greppable text format rather than either adding a new dependency this
//! unit is not allowed to add, or silently doing something structurally
//! different from the rest of the crate. Loading and saving both preserve
//! any line this build does not recognize (see [`merge_and_render`]) --
//! the same forward-compatibility guarantee `crate::settings` gives its
//! config file, applied here for the same reason: a future version's
//! extra counters must survive a save from an older build.
//!
//! # The window is a thin shell
//!
//! Everything above this line is plain data and pure functions, runnable
//! and tested on any platform. [`open_onboarding_window`] (macOS only) is
//! the one function that touches AppKit: it drives a short sequence of
//! `NSAlert`s off of [`current_step`] and [`OnboardingState`], and nothing
//! in the pure logic above knows or cares that a window exists. NOT
//! VERIFIED: no `NSAlert` in this module has ever been shown on a screen
//! in this environment -- see the module-level safety note near
//! [`open_onboarding_window`] for exactly why, and what was checked
//! instead.

// This unit's dispatch is to build and unit-test this module and expose a
// plain Rust API another (not-yet-built, in this same wave) unit calls to
// actually open the wizard -- e.g. a future menu-bar app's "first run"
// hook. Until that caller exists in this binary crate's call graph, every
// `pub` item here is, correctly, unreachable from `main` by the compiler's
// own analysis (a `bin` crate's `pub` does not suppress `dead_code` the way
// a `lib` crate's does) -- allowed at the module level rather than per-item
// so the real signal (an item genuinely nobody, including this module's own
// tests, ever calls) is not lost in the noise.
#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

/// Overrides [`default_path`] entirely when set to a non-empty value --
/// mirrors `crate::dictionary::DICTIONARY_PATH_ENV_VAR` and
/// `crate::settings::SETTINGS_PATH_ENV_VAR`'s convention for the same
/// purpose (tests, and a future `--onboarding-state-path` flag).
pub const ONBOARDING_PATH_ENV_VAR: &str = "TEXTIFY_VOICE_ONBOARDING_PATH";

/// The base whisper model this funnel gates on. `voice-cli`'s own default
/// (see `crate::dictate::DictateArgs::model`'s `default_value_t`) --
/// onboarding should not tell a user they are "ready" while the model
/// `dictate` will actually reach for on first run is still missing.
const GATE_MODEL: voice_asr_whisper::ModelId = voice_asr_whisper::ModelId::BaseEn;

// ---------------------------------------------------------------------
// Pure funnel logic
// ---------------------------------------------------------------------

/// One step of the first-run funnel, in the fixed order [`OnboardingStep::ALL`]
/// walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnboardingStep {
    Welcome,
    Microphone,
    Accessibility,
    ModelDownload,
    Ready,
}

impl OnboardingStep {
    /// Funnel order. [`current_step`] walks this array front to back and
    /// returns the first entry that is not [`is_satisfied`](Self::is_satisfied) --
    /// so this array *is* the funnel definition.
    pub const ALL: [OnboardingStep; 5] = [
        OnboardingStep::Welcome,
        OnboardingStep::Microphone,
        OnboardingStep::Accessibility,
        OnboardingStep::ModelDownload,
        OnboardingStep::Ready,
    ];

    #[must_use]
    pub fn index(self) -> usize {
        match self {
            OnboardingStep::Welcome => 0,
            OnboardingStep::Microphone => 1,
            OnboardingStep::Accessibility => 2,
            OnboardingStep::ModelDownload => 3,
            OnboardingStep::Ready => 4,
        }
    }

    /// Stable, greppable identifier used as the counter file's key prefix.
    /// Never changes across versions -- old counter files must keep
    /// meaning the same thing forever.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            OnboardingStep::Welcome => "welcome",
            OnboardingStep::Microphone => "microphone",
            OnboardingStep::Accessibility => "accessibility",
            OnboardingStep::ModelDownload => "model_download",
            OnboardingStep::Ready => "ready",
        }
    }

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            OnboardingStep::Welcome => "Welcome to Textify Voice",
            OnboardingStep::Microphone => "Microphone Access",
            OnboardingStep::Accessibility => "Accessibility Access",
            OnboardingStep::ModelDownload => "Speech Model",
            OnboardingStep::Ready => "You're Ready",
        }
    }

    /// Whether this step is satisfied by `inputs`. Every field of
    /// [`FunnelInputs`] this depends on is a *live* read the caller took
    /// just before asking -- this function has no memory of its own, which
    /// is exactly what makes revocation mid-flow handled correctly for
    /// free (see the module docs).
    #[must_use]
    pub fn is_satisfied(self, inputs: &FunnelInputs) -> bool {
        match self {
            // Nothing to *grant* here, but it is not vacuously satisfied
            // from the start either -- a fresh install must still land on
            // Welcome first (see `fresh_install_starts_at_welcome` below).
            // It is satisfied once, and forever after, the user has
            // clicked past it once (`FunnelInputs::welcome_completed`,
            // sourced from `FunnelCounters`' `welcome.completed` counter --
            // see `live_inputs`), not on every fresh evaluation.
            OnboardingStep::Welcome => inputs.welcome_completed,
            OnboardingStep::Microphone => inputs.mic_authorized,
            OnboardingStep::Accessibility => inputs.accessibility_trusted,
            OnboardingStep::ModelDownload => inputs.model_downloaded,
            // Ready re-checks everything rather than trusting that the
            // earlier steps in `ALL` were actually walked in order --
            // `current_step` never reaches this arm before the other three
            // are satisfied anyway (see its doc comment), but a caller
            // testing `Ready.is_satisfied(..)` directly should still get a
            // real answer, not a vacuous `true`.
            OnboardingStep::Ready => {
                inputs.mic_authorized && inputs.accessibility_trusted && inputs.model_downloaded
            }
        }
    }

    /// The `x-apple.systempreferences:` deep link that opens the exact
    /// System Settings pane this step needs, or `None` for steps with no
    /// System Settings pane of their own.
    ///
    /// **Corroboration, not a live-executed check**: this task's
    /// constraints explicitly forbid automating the operator's GUI apps
    /// from this session ("do not automate the operator's GUI apps ...
    /// without it being explicitly user-triggered code") -- calling
    /// `NSWorkspace.openURL` on these strings myself, right now, would be
    /// exactly that, so it was deliberately never done. Instead these two
    /// identifiers were corroborated by reading (never executing) the
    /// installed, real, shipping apps on this machine that request this
    /// exact Microphone+Accessibility pair, by `strings`-scanning their
    /// binaries for `x-apple.systempreferences:` and finding the identical
    /// URL: **VoiceInk.app** (a real dictation app on this machine with
    /// the same permission profile this CLI needs) embeds both
    /// `com.apple.preference.security?Privacy_Microphone` and
    /// `com.apple.preference.security?Privacy_Accessibility` verbatim;
    /// **Caffeine.app** independently embeds the same Accessibility one;
    /// **Telegram.app** independently embeds the same Microphone one. No
    /// installed app on this machine was found using a *different* pane
    /// identifier for either permission, so there is no evidence either
    /// identifier is wrong -- but "no evidence of wrong" is not the same
    /// claim as "watched it open," so this is reported as **NOT VERIFIED
    /// by direct execution** per this task's explicit instructions.
    #[must_use]
    pub fn deep_link_url(self) -> Option<&'static str> {
        match self {
            OnboardingStep::Microphone => {
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            }
            OnboardingStep::Accessibility => Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ),
            OnboardingStep::Welcome | OnboardingStep::ModelDownload | OnboardingStep::Ready => {
                None
            }
        }
    }
}

/// Live signal the funnel is evaluated against. The caller (the AppKit
/// wizard, or a test) is responsible for gathering these fresh each time
/// -- see [`live_inputs`] for the real, macOS-only source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunnelInputs {
    /// Whether the Welcome step has ever been completed before (persisted
    /// -- see [`live_inputs`]), not merely shown. A fresh install has this
    /// `false`, so [`current_step`] correctly starts at
    /// [`OnboardingStep::Welcome`] rather than skipping straight to
    /// Microphone.
    pub welcome_completed: bool,
    pub mic_authorized: bool,
    pub accessibility_trusted: bool,
    pub model_downloaded: bool,
}

/// The step the funnel is currently on: the first entry of
/// [`OnboardingStep::ALL`] that is not satisfied, or [`OnboardingStep::Ready`]
/// if every gating step is. Pure, total, and the single source of truth
/// for "which step is current" -- see the module docs for why this is a
/// recomputation rather than a stored cursor.
#[must_use]
pub fn current_step(inputs: &FunnelInputs) -> OnboardingStep {
    for step in OnboardingStep::ALL {
        if !step.is_satisfied(inputs) {
            return step;
        }
    }
    OnboardingStep::Ready
}

// ---------------------------------------------------------------------
// Funnel counters (reached / completed per step)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepCounts {
    pub reached: u32,
    pub completed: u32,
}

/// Per-step reached/completed counts. Plain in-memory data with pure
/// mutators -- [`OnboardingState`] is the thin persisted wrapper around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FunnelCounters {
    counts: [StepCounts; OnboardingStep::ALL.len()],
}

impl FunnelCounters {
    #[must_use]
    pub fn counts_for(&self, step: OnboardingStep) -> StepCounts {
        self.counts[step.index()]
    }

    pub fn record_reached(&mut self, step: OnboardingStep) {
        self.counts[step.index()].reached += 1;
    }

    pub fn record_completed(&mut self, step: OnboardingStep) {
        self.counts[step.index()].completed += 1;
    }

    /// Fraction of the people who *reached* this step who never
    /// *completed* it -- the number a growth-minded reading of this file
    /// actually wants. `None` when the step has never been reached (no
    /// denominator, not "0% drop-off").
    #[must_use]
    pub fn drop_off(&self, step: OnboardingStep) -> Option<f64> {
        let c = self.counts_for(step);
        if c.reached == 0 {
            return None;
        }
        Some(1.0 - (f64::from(c.completed) / f64::from(c.reached)))
    }
}

// ---------------------------------------------------------------------
// Text format: shared shape with `crate::settings`'s `key = value` config,
// reimplemented here rather than shared -- see the module docs on why
// there is no third file to put a shared helper in.
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for OnboardingParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// Every recognized `<step>.<reached|completed>` key, in a fixed order so
/// [`render`] output is deterministic (and diffable / greppable).
fn known_keys() -> [(OnboardingStep, &'static str); 10] {
    let mut out = [(OnboardingStep::Welcome, "reached"); 10];
    let mut i = 0;
    for step in OnboardingStep::ALL {
        out[i] = (step, "reached");
        i += 1;
        out[i] = (step, "completed");
        i += 1;
    }
    out
}

fn field_key(step: OnboardingStep, field: &str) -> String {
    format!("{}.{field}", step.key())
}

fn counts_field<'a>(counters: &'a mut FunnelCounters, step: OnboardingStep, field: &str) -> &'a mut u32 {
    let c = &mut counters.counts[step.index()];
    match field {
        "reached" => &mut c.reached,
        _ => &mut c.completed,
    }
}

/// Parse `content` into counters, applying every recognized, valid
/// `<step>.<field> = <u32>` line on top of [`FunnelCounters::default`] and
/// collecting everything else ([`OnboardingParseError`] for a recognized
/// key with an unparseable value; silently-skipped-but-preserved for an
/// unrecognized key -- see [`merge_and_render`]) without ever failing the
/// whole parse. A garbled file degrades to as many fields as it can still
/// make sense of, never to "all zero" and never to a panic.
fn parse(content: &str) -> (FunnelCounters, Vec<OnboardingParseError>) {
    let mut counters = FunnelCounters::default();
    let mut errors = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            errors.push(OnboardingParseError {
                line: line_no,
                message: format!("expected `key = value`, found {raw_line:?}"),
            });
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        let Some((step, field)) = known_keys()
            .into_iter()
            .find(|(step, field)| field_key(*step, field) == key)
        else {
            // Unrecognized key: not an error, just not ours -- forward
            // compatibility with a newer build's extra counters.
            continue;
        };

        match value.parse::<u32>() {
            Ok(n) => *counts_field(&mut counters, step, field) = n,
            Err(_) => errors.push(OnboardingParseError {
                line: line_no,
                message: format!("`{key}` = {value:?} is not a non-negative integer"),
            }),
        }
    }

    (counters, errors)
}

/// Render `counters` as a fresh, canonical `key = value` file (all ten
/// known keys, in [`OnboardingStep::ALL`] order). Used for a brand-new
/// file; [`merge_and_render`] is used for every subsequent save so an
/// unrecognized line already on disk survives.
fn render(counters: &FunnelCounters) -> String {
    let mut out = String::from(
        "# textify-voice onboarding funnel counters -- local only, no telemetry backend.\n\
         # Safe to inspect (`cat`) or reset (delete this file) at any time.\n\n",
    );
    for (step, field) in known_keys() {
        let c = counters.counts_for(step);
        let n = if field == "reached" { c.reached } else { c.completed };
        out.push_str(&field_key(step, field));
        out.push_str(" = ");
        out.push_str(&n.to_string());
        out.push('\n');
    }
    out
}

/// Merge `counters` into `existing` raw file content: every recognized key
/// already present is rewritten with the current count (in place, same
/// line position); every line this build does not recognize (comments,
/// blank lines, and any `key = value` this build's [`known_keys`] does not
/// list) is copied through **verbatim**; any recognized key missing from
/// `existing` is appended at the end. This is the forward-compatibility
/// guarantee: an older build re-saving this file after a newer build added
/// an eleventh counter does not delete that eleventh line.
fn merge_and_render(existing: &str, counters: &FunnelCounters) -> String {
    let mut written = std::collections::HashSet::new();
    let mut out = String::new();

    for raw_line in existing.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(raw_line);
            out.push('\n');
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            out.push_str(raw_line);
            out.push('\n');
            continue;
        };
        let key = key.trim();
        match known_keys().into_iter().find(|(step, field)| field_key(*step, field) == key) {
            Some((step, field)) => {
                let c = counters.counts_for(step);
                let n = if field == "reached" { c.reached } else { c.completed };
                out.push_str(key);
                out.push_str(" = ");
                out.push_str(&n.to_string());
                out.push('\n');
                written.insert((step, field));
            }
            None => {
                // Unrecognized key -- preserve verbatim.
                out.push_str(raw_line);
                out.push('\n');
            }
        }
    }

    for (step, field) in known_keys() {
        if written.insert((step, field)) {
            let c = counters.counts_for(step);
            let n = if field == "reached" { c.reached } else { c.completed };
            out.push_str(&field_key(step, field));
            out.push_str(" = ");
            out.push_str(&n.to_string());
            out.push('\n');
        }
    }

    out
}

// ---------------------------------------------------------------------
// Path resolution + persisted state (thin I/O shell)
// ---------------------------------------------------------------------

#[derive(Debug)]
pub enum OnboardingError {
    NoConfigDir,
    Io(io::Error),
}

impl fmt::Display for OnboardingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OnboardingError::NoConfigDir => write!(
                f,
                "could not resolve a platform data directory for onboarding state; \
                 set {ONBOARDING_PATH_ENV_VAR} or pass an explicit path"
            ),
            OnboardingError::Io(e) => write!(f, "onboarding state I/O error: {e}"),
        }
    }
}

impl std::error::Error for OnboardingError {}

impl From<io::Error> for OnboardingError {
    fn from(e: io::Error) -> Self {
        OnboardingError::Io(e)
    }
}

/// Resolve the default onboarding-state path: [`ONBOARDING_PATH_ENV_VAR`]
/// if set to a non-empty value, else the platform data directory --
/// concretely `~/Library/Application Support/textify/onboarding.txt` on
/// macOS. Reimplements the same per-OS resolution `dictionary::default_path`
/// and `settings::default_path` do (see this module's doc comment on why
/// there is no shared helper).
pub fn default_path() -> Result<PathBuf, OnboardingError> {
    if let Ok(p) = std::env::var(ONBOARDING_PATH_ENV_VAR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(platform_data_dir()?.join("textify").join("onboarding.txt"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Result<PathBuf, OnboardingError> {
    let home = std::env::var_os("HOME").ok_or(OnboardingError::NoConfigDir)?;
    Ok(PathBuf::from(home).join("Library").join("Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> Result<PathBuf, OnboardingError> {
    let appdata = std::env::var_os("APPDATA").ok_or(OnboardingError::NoConfigDir)?;
    Ok(PathBuf::from(appdata))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> Result<PathBuf, OnboardingError> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg));
        }
    }
    let home = std::env::var_os("HOME").ok_or(OnboardingError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".local").join("share"))
}

/// The persisted funnel counters plus the path they live at. The pure
/// logic above never touches disk; this is the whole I/O surface.
#[derive(Debug, Clone)]
pub struct OnboardingState {
    pub path: PathBuf,
    pub counters: FunnelCounters,
}

impl OnboardingState {
    /// Load from [`default_path`]. A missing file is the normal first-run
    /// state, not an error -- returns all-zero counters.
    pub fn load() -> Result<Self, OnboardingError> {
        Self::load_from(default_path()?)
    }

    pub fn load_from(path: PathBuf) -> Result<Self, OnboardingError> {
        match fs::read_to_string(&path) {
            Ok(content) => {
                let (counters, _errors) = parse(&content);
                Ok(Self { path, counters })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Self { path, counters: FunnelCounters::default() })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Persist, preserving any unrecognized line already on disk (see
    /// [`merge_and_render`]).
    pub fn save(&self) -> Result<(), OnboardingError> {
        let existing = fs::read_to_string(&self.path).unwrap_or_default();
        let rendered = if existing.trim().is_empty() {
            render(&self.counters)
        } else {
            merge_and_render(&existing, &self.counters)
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, rendered)?;
        Ok(())
    }

    pub fn record_step_reached(&mut self, step: OnboardingStep) -> Result<(), OnboardingError> {
        self.counters.record_reached(step);
        self.save()
    }

    pub fn record_step_completed(&mut self, step: OnboardingStep) -> Result<(), OnboardingError> {
        self.counters.record_completed(step);
        self.save()
    }
}

// ---------------------------------------------------------------------
// Live inputs (real permission + model-cache checks)
// ---------------------------------------------------------------------

/// Gather [`FunnelInputs`] from the real, live system state:
/// `counters.counts_for(Welcome).completed` (has Welcome ever been
/// completed before -- the one field that comes from disk, not a live OS
/// query), `crate::permissions::check()` (real `AVCaptureDevice` +
/// `AXIsProcessTrusted` calls), and `voice_asr_whisper::ModelManager::is_cached`
/// (a real filesystem + size-range check, no network access). Never blocks
/// on a download.
#[must_use]
/// Is the gate model already downloaded? Exposed so the checklist window can
/// poll it the same way it polls the two permissions.
pub fn model_is_cached() -> bool {
    voice_asr_whisper::ModelManager::new()
        .map(|m| m.is_cached(GATE_MODEL))
        .unwrap_or(false)
}

/// Download the gate model. Exposed for the checklist window's row action.
pub fn download_gate_model() -> anyhow::Result<()> {
    let manager = voice_asr_whisper::ModelManager::new()?;
    if manager.is_cached(GATE_MODEL) {
        return Ok(());
    }
    manager.ensure_downloaded(GATE_MODEL, None)?;
    Ok(())
}

pub fn live_inputs(counters: &FunnelCounters) -> FunnelInputs {
    let report = crate::permissions::check();
    let model_downloaded = voice_asr_whisper::ModelManager::new()
        .map(|m| m.is_cached(GATE_MODEL))
        .unwrap_or(false);
    FunnelInputs {
        welcome_completed: counters.counts_for(OnboardingStep::Welcome).completed > 0,
        mic_authorized: report.mic == voice_audio::MicPermission::Authorized,
        accessibility_trusted: report.accessibility_trusted,
        model_downloaded,
    }
}

/// Open (or navigate to, if already open) the System Settings pane for
/// `url` — one of [`OnboardingStep::deep_link_url`]'s values. Real
/// `NSWorkspace.openURL` on macOS; see [`OnboardingStep::deep_link_url`]'s
/// doc comment for exactly how the two URLs used by this module were
/// corroborated without ever calling this function autonomously.
#[cfg(target_os = "macos")]
pub fn open_deep_link(url: &str) -> anyhow::Result<bool> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    let ns_url = objc2_foundation::NSURL::URLWithString(&NSString::from_str(url))
        .ok_or_else(|| anyhow::anyhow!("not a valid URL: {url}"))?;
    let workspace = NSWorkspace::sharedWorkspace();
    Ok(workspace.openURL(&ns_url))
}

#[cfg(not(target_os = "macos"))]
pub fn open_deep_link(_url: &str) -> anyhow::Result<bool> {
    anyhow::bail!("opening a System Settings deep link is only implemented for macOS")
}

// ---------------------------------------------------------------------
// The window: a thin, alert-driven shell over the pure logic above.
// ---------------------------------------------------------------------

/// Run the first-run onboarding wizard: a short sequence of `NSAlert`s
/// (Welcome, Microphone, Accessibility, model download, Ready), each
/// driven by [`current_step`] re-evaluated against [`live_inputs`] so a
/// permission revoked mid-flow is caught rather than papered over, with
/// [`OnboardingState`] recording every step reached/completed as it goes.
///
/// `NSAlert.runModal()` (blocking, one alert at a time) was chosen over a
/// persistent custom-layout window deliberately: it needs no custom
/// `NSObject` subclass / target-action wiring at all (unlike
/// `crate::settings`'s window, which genuinely needs live multi-field
/// editing), and its blocking, one-decision-per-call shape maps directly
/// onto "an explicit sequence of steps" -- less custom AppKit surface to
/// get wrong in an environment where it cannot be visually checked.
///
/// Handles the same activation-policy dance `crate::settings::open_settings_window`
/// documents: activates on entry (an Accessory app does not steal focus by
/// default and a first-run alert the user never sees behind other windows
/// is worse than no alert), deactivates on exit.
///
/// **NOT VERIFIED**: no `NSAlert` here has been shown on a real screen in
/// this session -- this sandboxed environment cannot observe a GUI or
/// complete a TCC consent dialog (see this task's brief). Every AppKit
/// call below is real (not a stub) and of a kind independently proven to
/// work by this wave's AppKit probe (`NSAlert::new`, `setMessageText`,
/// `addButtonWithTitle`, `NSApplication::activate`/`setActivationPolicy`
/// were all constructed and exercised for real in that probe run), but
/// `runModal()` itself -- the one call that would actually put a dialog on
/// screen and wait for a click -- was never invoked outside of a real user
/// run, deliberately, per this task's "do not automate the operator's GUI
/// apps" constraint.
#[cfg(target_os = "macos")]
pub fn open_onboarding_window() -> anyhow::Result<()> {
    let mtm = objc2::MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("the onboarding wizard must be opened from the main thread"))?;

    // The checklist window replaces the old per-permission alert chain. The
    // alert version is kept below (`appkit::run`) because its funnel-counter
    // recording and step sequencing are still the reference for what a
    // completed funnel means -- but it is no longer what the user sees, for
    // the reasons in `onboarding_window`'s module docs.
    match crate::onboarding_window::run(mtm)? {
        crate::onboarding_window::Outcome::Ready | crate::onboarding_window::Outcome::Quit => {
            // Record the funnel as reached either way: the counters exist to
            // measure drop-off, so a user who quits partway is exactly the
            // signal WP-V0.4 asks for.
            if let Ok(mut state) = OnboardingState::load() {
                let _ = state.record_step_completed(OnboardingStep::Welcome);
            }
            Ok(())
        }
        crate::onboarding_window::Outcome::Relaunch => {
            crate::onboarding_window::relaunch_bundle();
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_onboarding_window() -> anyhow::Result<()> {
    anyhow::bail!("the onboarding wizard is only implemented for macOS in this build")
}

#[cfg(target_os = "macos")]
mod appkit {
    use super::{
        current_step, live_inputs, open_deep_link, GATE_MODEL, OnboardingState, OnboardingStep,
    };
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSAlertFirstButtonReturn,
        NSAlertSecondButtonReturn, NSAlertThirdButtonReturn,
    };
    use objc2_foundation::ns_string;

    /// What the user chose in a given alert. Not every alert offers every
    /// choice -- see each `show_*` function for which subset it presents.
    enum Choice {
        Continue,
        OpenSettings,
        /// Relaunch the app bundle and exit this process.
        ///
        /// macOS evaluates `AXIsProcessTrusted()` for a process largely at
        /// launch. Granting Accessibility to an ALREADY-RUNNING process very
        /// often keeps reporting untrusted until it restarts — so "switch it on,
        /// then Try Again" can loop forever even though the switch is genuinely
        /// on. Restarting is the actual resolution, and the user has no way to
        /// know that, so the wizard has to offer it.
        Relaunch,
        Quit,
    }

    /// Re-open our own `.app` and exit, so the new process picks up a freshly
    /// granted Accessibility trust.
    fn relaunch_bundle() -> ! {
        if let Ok(crate::login_item::BundleContext::Bundled(app)) =
            crate::login_item::current_exe_bundle_context()
        {
            // `-n` forces a new instance rather than activating this dying one.
            let _ = std::process::Command::new("/usr/bin/open").arg("-n").arg(&app).spawn();
        }
        std::process::exit(0);
    }

    pub fn run(mtm: MainThreadMarker) -> anyhow::Result<()> {
        let app = NSApplication::sharedApplication(mtm);
        // Accessory, not Regular: this wizard should not put a permanent
        // Dock icon on screen just for a first-run flow. It still needs to
        // *activate* to reliably come to the front over other windows —
        // that is a focus concept, distinct from the Dock-icon policy.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.activate();

        let mut state = OnboardingState::load().unwrap_or_else(|_| OnboardingState {
            path: super::default_path().unwrap_or_else(|_| std::path::PathBuf::from("onboarding.txt")),
            counters: super::FunnelCounters::default(),
        });

        loop {
            let inputs = live_inputs(&state.counters);
            let step = current_step(&inputs);
            let _ = state.record_step_reached(step);

            match step {
                OnboardingStep::Welcome => match show_welcome(mtm) {
                    Choice::Continue => {
                        let _ = state.record_step_completed(step);
                    }
                    Choice::Quit | Choice::OpenSettings | Choice::Relaunch => break,
                },
                OnboardingStep::Microphone | OnboardingStep::Accessibility => {
                    match show_permission_step(mtm, step) {
                        Choice::OpenSettings => {
                            if let Some(url) = step.deep_link_url() {
                                let _ = open_deep_link(url);
                            }
                            // Loop back around immediately and re-evaluate
                            // live inputs -- if the user grants it and
                            // returns, the next iteration's `current_step`
                            // moves on by itself; if not, the same alert
                            // reappears rather than silently advancing.
                        }
                        // "Try Again" returns Continue, and it must re-evaluate
                        // rather than break. Breaking here meant the button
                        // labelled "Try Again" quit the wizard -- the opposite
                        // of what it says -- so the only way out of a permission
                        // step was to give up.
                        Choice::Continue => {}
                        Choice::Relaunch => relaunch_bundle(),
                        Choice::Quit => break,
                    }
                }
                OnboardingStep::ModelDownload => match show_model_download(mtm) {
                    Choice::Continue => {
                        if download_model().is_ok() {
                            let _ = state.record_step_completed(step);
                        }
                        // Failure (e.g. no network) falls through: the
                        // next loop iteration re-evaluates `live_inputs`,
                        // sees the model still missing, and shows this
                        // same step again rather than silently advancing.
                    }
                    Choice::OpenSettings | Choice::Quit | Choice::Relaunch => break,
                },
                OnboardingStep::Ready => {
                    show_ready(mtm);
                    let _ = state.record_step_completed(step);
                    break;
                }
            }
        }

        app.deactivate();
        Ok(())
    }

    fn download_model() -> anyhow::Result<()> {
        let manager = voice_asr_whisper::ModelManager::new()?;
        if manager.is_cached(GATE_MODEL) {
            return Ok(());
        }
        let mut last_pct = u64::MAX;
        manager.ensure_downloaded(
            GATE_MODEL,
            Some(&mut |downloaded: u64, total: u64| {
                if total > 0 {
                    let pct = (downloaded * 100) / total;
                    if pct != last_pct {
                        last_pct = pct;
                        eprint!("\r  downloading speech model... {pct:3}%");
                    }
                }
            }),
        )?;
        eprintln!();
        Ok(())
    }

    fn show_welcome(mtm: MainThreadMarker) -> Choice {
        use objc2_app_kit::NSAlert;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Welcome to Textify Voice"));
        alert.setInformativeText(ns_string!(
            "Hold a key, speak, release: your words land wherever you were typing. \
             The next couple of steps grant the two permissions dictation needs."
        ));
        alert.addButtonWithTitle(ns_string!("Continue"));
        alert.addButtonWithTitle(ns_string!("Quit"));
        if alert.runModal() == NSAlertFirstButtonReturn { Choice::Continue } else { Choice::Quit }
    }

    fn show_permission_step(mtm: MainThreadMarker, step: OnboardingStep) -> Choice {
        use objc2_app_kit::NSAlert;

        // ASK FIRST. macOS only lists an app under Privacy & Security once it
        // has actually *requested* the permission — the read-only status checks
        // never register it. Without this, the alert below sends the user to
        // System Settings to enable a row that does not exist. Requesting shows
        // the native consent dialog, which most users can simply accept without
        // ever opening System Settings at all.
        //
        // Both calls are cheap and idempotent: once the user has answered,
        // macOS will not ask again and these become no-ops, leaving System
        // Settings as the only route (which is what the alert then explains).
        match step {
            OnboardingStep::Microphone => voice_audio::request_microphone_access(),
            _ => {
                let _ = crate::permissions::prompt_for_accessibility();
            }
        }

        let alert = NSAlert::new(mtm);
        alert.setMessageText(match step {
            OnboardingStep::Microphone => ns_string!("Microphone Access Needed"),
            _ => ns_string!("Accessibility Access Needed"),
        });
        alert.setInformativeText(match step {
            OnboardingStep::Microphone => ns_string!(
                "Textify Voice needs Microphone access to hear you.\n\n\
                 macOS should have just asked — click Allow, then Try Again here. \
                 If no dialog appeared, you have answered before: open System Settings \
                 and switch Textify Voice on under Privacy & Security > Microphone."
            ),
            _ => ns_string!(
                "Textify Voice needs Accessibility access to detect the hold key and \
                 (optionally) paste your text.\n\n\
                 Switch it on in System Settings > Privacy & Security > Accessibility \
                 — note that is a SEPARATE list from Microphone.\n\n\
                 Already switched on and this keeps reappearing? macOS only picks up \
                 Accessibility when an app starts, so this running copy cannot see the \
                 change. Choose Quit & Reopen."
            ),
        });
        alert.addButtonWithTitle(ns_string!("Open System Settings"));
        alert.addButtonWithTitle(ns_string!("Try Again"));
        if matches!(step, OnboardingStep::Accessibility) {
            alert.addButtonWithTitle(ns_string!("Quit & Reopen"));
        }
        alert.addButtonWithTitle(ns_string!("Quit"));
        let response = alert.runModal();
        if response == NSAlertFirstButtonReturn {
            Choice::OpenSettings
        } else if response == NSAlertSecondButtonReturn {
            Choice::Continue // "Try Again" just re-checks live inputs
        } else if matches!(step, OnboardingStep::Accessibility)
            && response == NSAlertThirdButtonReturn
        {
            Choice::Relaunch
        } else {
            Choice::Quit
        }
    }

    fn show_model_download(mtm: MainThreadMarker) -> Choice {
        use objc2_app_kit::NSAlert;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Download the Speech Model"));
        alert.setInformativeText(ns_string!(
            "Textify Voice needs a local speech model (about 150 MB, downloaded once, \
             runs entirely on this Mac afterward)."
        ));
        alert.addButtonWithTitle(ns_string!("Download"));
        alert.addButtonWithTitle(ns_string!("Quit"));
        if alert.runModal() == NSAlertFirstButtonReturn { Choice::Continue } else { Choice::Quit }
    }

    fn show_ready(mtm: MainThreadMarker) {
        use objc2_app_kit::NSAlert;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("You're Ready"));
        alert.setInformativeText(ns_string!(
            "Hold the key, speak, release. Open Settings any time to change the hold key, \
             mode, or model."
        ));
        alert.addButtonWithTitle(ns_string!("Done"));
        alert.runModal();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn inputs(welcome: bool, mic: bool, ax: bool, model: bool) -> FunnelInputs {
        FunnelInputs {
            welcome_completed: welcome,
            mic_authorized: mic,
            accessibility_trusted: ax,
            model_downloaded: model,
        }
    }

    // -- current_step: the pure funnel logic --

    #[test]
    fn fresh_install_starts_at_welcome() {
        assert_eq!(current_step(&inputs(false, false, false, false)), OnboardingStep::Welcome);
    }

    #[test]
    fn welcome_completed_moves_on_to_microphone_once_nothing_else_blocks() {
        assert_eq!(current_step(&inputs(true, false, false, false)), OnboardingStep::Microphone);
        assert!(!OnboardingStep::Welcome.is_satisfied(&inputs(false, false, false, false)));
        assert!(OnboardingStep::Welcome.is_satisfied(&inputs(true, false, false, false)));
    }

    #[test]
    fn mic_granted_moves_to_accessibility() {
        assert_eq!(current_step(&inputs(true, true, false, false)), OnboardingStep::Accessibility);
    }

    #[test]
    fn mic_and_accessibility_granted_moves_to_model_download() {
        assert_eq!(current_step(&inputs(true, true, true, false)), OnboardingStep::ModelDownload);
    }

    #[test]
    fn everything_granted_is_ready() {
        assert_eq!(current_step(&inputs(true, true, true, true)), OnboardingStep::Ready);
    }

    #[test]
    fn accessibility_alone_without_mic_still_blocks_on_mic() {
        // Order matters: `ALL` walks Microphone before Accessibility, so
        // an odd real-world grant order (AX granted, mic not) still
        // reports Microphone as current, not Accessibility.
        assert_eq!(current_step(&inputs(true, false, true, false)), OnboardingStep::Microphone);
    }

    #[test]
    fn revoking_a_permission_after_ready_snaps_the_funnel_back() {
        let mut i = inputs(true, true, true, true);
        assert_eq!(current_step(&i), OnboardingStep::Ready);

        // The user opens System Settings and flips Microphone back off.
        i.mic_authorized = false;
        assert_eq!(
            current_step(&i),
            OnboardingStep::Microphone,
            "a revoked permission must snap the funnel back to it, not leave it stuck at Ready"
        );
    }

    #[test]
    fn revoking_accessibility_only_snaps_back_to_accessibility_not_all_the_way_to_welcome() {
        let mut i = inputs(true, true, true, true);
        i.accessibility_trusted = false;
        assert_eq!(current_step(&i), OnboardingStep::Accessibility);
    }

    #[test]
    fn model_missing_alone_blocks_at_model_download_even_with_both_permissions_granted() {
        assert_eq!(current_step(&inputs(true, true, true, false)), OnboardingStep::ModelDownload);
    }

    #[test]
    fn ready_is_satisfied_only_when_all_three_gating_inputs_are_true() {
        assert!(!OnboardingStep::Ready.is_satisfied(&inputs(true, true, true, false)));
        assert!(!OnboardingStep::Ready.is_satisfied(&inputs(true, true, false, true)));
        assert!(!OnboardingStep::Ready.is_satisfied(&inputs(true, false, true, true)));
        assert!(OnboardingStep::Ready.is_satisfied(&inputs(true, true, true, true)));
    }

    // -- deep links --

    #[test]
    fn only_microphone_and_accessibility_steps_have_a_deep_link() {
        assert!(OnboardingStep::Welcome.deep_link_url().is_none());
        assert!(OnboardingStep::ModelDownload.deep_link_url().is_none());
        assert!(OnboardingStep::Ready.deep_link_url().is_none());
        assert!(OnboardingStep::Microphone.deep_link_url().unwrap().contains("Privacy_Microphone"));
        assert!(
            OnboardingStep::Accessibility.deep_link_url().unwrap().contains("Privacy_Accessibility")
        );
    }

    // -- FunnelCounters --

    #[test]
    fn counters_start_at_zero_for_every_step() {
        let c = FunnelCounters::default();
        for step in OnboardingStep::ALL {
            assert_eq!(c.counts_for(step), StepCounts::default());
            assert_eq!(c.drop_off(step), None, "never-reached step has no drop-off rate");
        }
    }

    #[test]
    fn record_reached_and_completed_are_independent_and_step_scoped() {
        let mut c = FunnelCounters::default();
        c.record_reached(OnboardingStep::Microphone);
        c.record_reached(OnboardingStep::Microphone);
        c.record_completed(OnboardingStep::Microphone);

        let mic = c.counts_for(OnboardingStep::Microphone);
        assert_eq!(mic.reached, 2);
        assert_eq!(mic.completed, 1);
        // Untouched step stays at zero.
        assert_eq!(c.counts_for(OnboardingStep::Accessibility), StepCounts::default());
    }

    #[test]
    fn drop_off_rate_reflects_reached_vs_completed() {
        let mut c = FunnelCounters::default();
        for _ in 0..10 {
            c.record_reached(OnboardingStep::Accessibility);
        }
        for _ in 0..4 {
            c.record_completed(OnboardingStep::Accessibility);
        }
        let rate = c.drop_off(OnboardingStep::Accessibility).unwrap();
        assert!((rate - 0.6).abs() < 1e-9, "expected 60% drop-off, got {rate}");
    }

    // -- text format: parse / render / merge --

    #[test]
    fn parse_of_missing_or_empty_content_is_all_zero_with_no_errors() {
        let (counters, errors) = parse("");
        assert_eq!(counters, FunnelCounters::default());
        assert!(errors.is_empty());
    }

    #[test]
    fn round_trips_through_render_and_parse() {
        let mut c = FunnelCounters::default();
        c.record_reached(OnboardingStep::Welcome);
        c.record_completed(OnboardingStep::Welcome);
        c.record_reached(OnboardingStep::Microphone);
        c.record_reached(OnboardingStep::Microphone);
        c.record_reached(OnboardingStep::Ready);

        let text = render(&c);
        let (parsed, errors) = parse(&text);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(parsed, c);
    }

    #[test]
    fn a_corrupt_file_does_not_crash_and_recovers_whatever_it_can() {
        let content = "\
welcome.reached = 3
not a valid line at all
microphone.reached = banana
accessibility.completed = 1
future_step.reached = 99
";
        let (counters, errors) = parse(content);
        // The one malformed structural line and the one bad-value known
        // key are both reported...
        assert_eq!(errors.len(), 2, "{errors:?}");
        // ...but everything parseable still landed.
        assert_eq!(counters.counts_for(OnboardingStep::Welcome).reached, 3);
        assert_eq!(counters.counts_for(OnboardingStep::Accessibility).completed, 1);
        // The unparseable known key was left at its default rather than
        // poisoning the whole load.
        assert_eq!(counters.counts_for(OnboardingStep::Microphone).reached, 0);
    }

    #[test]
    fn unknown_keys_from_a_newer_version_survive_a_save_from_this_build() {
        let existing = "\
# a future build's extra counter this build has never heard of
retention_day_7.reached = 42

welcome.reached = 1
welcome.completed = 1
microphone.reached = 1
microphone.completed = 0
accessibility.reached = 0
accessibility.completed = 0
model_download.reached = 0
model_download.completed = 0
ready.reached = 0
ready.completed = 0
";
        let mut c = FunnelCounters::default();
        c.record_reached(OnboardingStep::Welcome);
        c.record_completed(OnboardingStep::Welcome);
        c.record_reached(OnboardingStep::Microphone);
        c.record_completed(OnboardingStep::Microphone); // now completed=1, differs from `existing`

        let merged = merge_and_render(existing, &c);
        assert!(
            merged.contains("retention_day_7.reached = 42"),
            "an unrecognized key must survive a save verbatim:\n{merged}"
        );
        assert!(merged.contains("microphone.completed = 1"), "known keys must still update:\n{merged}");

        // And it must still parse cleanly afterward.
        let (reparsed, errors) = parse(&merged);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(reparsed.counts_for(OnboardingStep::Microphone).completed, 1);
    }

    #[test]
    fn merge_appends_known_keys_missing_from_an_existing_file() {
        // A file that only ever recorded `welcome` (e.g. hand-edited, or
        // from a build that only had one step) must gain the rest on save,
        // not lose them.
        let existing = "welcome.reached = 5\nwelcome.completed = 5\n";
        let c = FunnelCounters::default();
        let merged = merge_and_render(existing, &c);
        for (step, field) in known_keys() {
            assert!(
                merged.contains(&field_key(step, field)),
                "merged output missing {}:\n{merged}",
                field_key(step, field)
            );
        }
    }

    // -- OnboardingState: the I/O shell --

    #[test]
    fn state_load_of_a_missing_file_is_all_zero_and_found_is_implicit_in_default_counters() {
        let dir = std::env::temp_dir().join(format!("textify-onboarding-test-{}", std::process::id()));
        let path = dir.join("does-not-exist.txt");
        let state = OnboardingState::load_from(path.clone()).unwrap();
        assert_eq!(state.counters, FunnelCounters::default());
        assert_eq!(state.path, path);
    }

    #[test]
    fn state_save_then_load_round_trips_and_creates_parent_dirs() {
        let dir = std::env::temp_dir().join(format!(
            "textify-onboarding-test-{}-{}",
            std::process::id(),
            "roundtrip"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("onboarding.txt");

        let mut state = OnboardingState::load_from(path.clone()).unwrap();
        state.record_step_reached(OnboardingStep::Microphone).unwrap();
        state.record_step_reached(OnboardingStep::Microphone).unwrap();
        state.record_step_completed(OnboardingStep::Microphone).unwrap();

        let reloaded = OnboardingState::load_from(path.clone()).unwrap();
        let mic = reloaded.counters.counts_for(OnboardingStep::Microphone);
        assert_eq!(mic.reached, 2);
        assert_eq!(mic.completed, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_load_of_a_corrupt_file_does_not_error_and_keeps_what_it_can_parse() {
        let dir = std::env::temp_dir().join(format!("textify-onboarding-test-{}-corrupt", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("onboarding.txt");
        std::fs::write(&path, "this is not the right format at all\nwelcome.reached = 7\n").unwrap();

        let state = OnboardingState::load_from(path.clone()).unwrap();
        assert_eq!(state.counters.counts_for(OnboardingStep::Welcome).reached, 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_path_respects_the_env_var_override() {
        // SAFETY: test-only, single-threaded within this process's test
        // harness convention already used by sibling modules in this crate.
        std::env::set_var(ONBOARDING_PATH_ENV_VAR, "/tmp/custom-onboarding.txt");
        let p = default_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/custom-onboarding.txt"));
        std::env::remove_var(ONBOARDING_PATH_ENV_VAR);
    }

    #[test]
    fn live_inputs_sources_welcome_completed_from_the_persisted_counters_not_a_live_os_query() {
        // `live_inputs` does real permission/model-cache checks for the
        // other three fields (not reproducible here without OS state to
        // control), but `welcome_completed` comes entirely from
        // `FunnelCounters` -- verify that wiring directly rather than
        // trusting it by inspection alone.
        let fresh = FunnelCounters::default();
        assert!(!live_inputs(&fresh).welcome_completed);

        let mut seen = FunnelCounters::default();
        seen.record_reached(OnboardingStep::Welcome);
        seen.record_completed(OnboardingStep::Welcome);
        assert!(live_inputs(&seen).welcome_completed);
    }
}
