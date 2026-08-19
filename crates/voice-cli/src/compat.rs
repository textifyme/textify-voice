//! Platform compatibility: what hardware/OS this binary actually requires,
//! enforced at startup rather than assumed, plus a device-tier report
//! (SPEC.md §3.1's "device/tier detection").
//!
//! ## Why this exists
//!
//! Before this module, `packaging/Info.plist.template` declared
//! `LSMinimumSystemVersion` 13.0 with nothing behind it -- no code checked
//! it, and the built binary's own Mach-O `LC_BUILD_VERSION` said `minos
//! 11.0` (rustc's untouched default for `aarch64-apple-darwin`; no
//! `MACOSX_DEPLOYMENT_TARGET` is set anywhere in this repo). Two numbers,
//! neither enforced, neither derived from the other. Separately, an Intel
//! Mac has *never been compiled*: this box (Homebrew rustc 1.86, no rustup)
//! has no `x86_64-apple-darwin` target installed, so nobody has ever found
//! out whether CPU-only whisper.cpp is even viable for push-to-talk, and a
//! curious Intel user downloading the app today would get either a build
//! failure, or (if someone did manage a cross build) an untested first run
//! with zero warning.
//!
//! This module turns both into decisions, checked for real at process
//! start (see [`check_startup`], wired into `main()`), instead of
//! aspirations or silent crashes.
//!
//! ## Decision 1 — Apple Silicon only, for v1
//!
//! **Decided:** an Intel Mac (native or an Intel *build* run translated
//! under Rosetta 2 on Apple Silicon hardware) is refused at startup with a
//! one-sentence explanation, not a crash and not a silent CPU-only limp.
//!
//! **Why:** `crates/voice-asr-whisper/Cargo.toml` already scopes whisper-rs's
//! `metal` feature to exactly `cfg(all(target_os = "macos", target_arch =
//! "aarch64"))` -- every non-Apple-Silicon target gets CPU-only whisper.cpp.
//! Every latency number this project has ever measured (245 ms
//! speech-end-to-text, ~69x realtime, `docs/voice/PORTING.md`) came from one
//! 32-core-GPU M1 Max using the Metal path; nobody has measured CPU-only
//! decode against push-to-talk's latency budget, on any hardware, because
//! the `x86_64-apple-darwin` target has never been installed on any machine
//! this project has been built on. Shipping an untested, possibly-too-slow
//! CPU fallback and calling it "Intel support" would be worse than not
//! shipping Intel support: a user would get a real download, a real
//! install, and a dictation tool that might be unusably slow with no
//! indication that was ever a known risk. A declared, fail-clear "not
//! supported yet" is honest; a silent bad experience is not.
//!
//! **This is a v1 scope decision, not a permanent one.** If CPU-only
//! whisper.cpp is later measured (on real Intel hardware, which this
//! environment cannot provide -- see this unit's verification notes) to be
//! fast enough, [`ArchSupport::IntelNative`] is already a distinct, named
//! case in [`classify_arch`] ready to be flipped to supported; the
//! Rosetta-translated case stays refused independently (using a translated
//! x86_64 build on Apple Silicon hardware when the real Apple Silicon build
//! exists and is faster is never the right answer, regardless of the
//! native-Intel verdict).
//!
//! ## Decision 2 — macOS floor: 11.0, enforced; degrade rather than raise it
//!
//! **Decided:** `LSMinimumSystemVersion` is 11.0 ([`MIN_MACOS_VERSION`]),
//! down from the previously-unenforced 13.0, matched by a real runtime check
//! ([`floor_blocking_reason`]) so a too-old Mac gets a sentence instead of a
//! missing-selector crash (Objective-C sends to an unavailable selector
//! don't fail at compile time -- they fail live, on someone else's machine).
//!
//! **Why 11.0 and not something lower:** every macOS API this binary
//! actually calls predates 11.0 by a wide margin (`CGEventTap`/`CGEvent`
//! posting ~10.4+, `NSPanel`/non-activating-panel styling 10.6,
//! `CALayer`/`CATransaction` 10.5, `NSSound`/`NSStatusBar`/`NSMenu`/
//! `NSAlert`/`NSWorkspace`/`NSPasteboard`/`NSWindow`/`NSPopUpButton` all
//! pre-10.10, `AXIsProcessTrusted`/`AXUIElement` reads 10.2/10.9,
//! `AVCaptureDevice` mic-permission APIs 10.14) -- but Decision 1 already
//! restricts this build to Apple Silicon, and **no Apple Silicon Mac has
//! ever shipped with an OS older than 11.0 (Big Sur)**, so 11.0 is the real
//! floor Decision 1 implies, not an arbitrary lower number. Going lower
//! would be a floor nothing running this binary could ever be below,
//! which is the same as no floor at all.
//!
//! **Why not 13.0 (the old, unenforced value):** the *only* API found
//! anywhere in this crate that needs 13.0+ is `SMAppService`
//! (`crates/voice-cli/src/login_item.rs`, for launch-at-login) -- and that
//! module is `#![allow(dead_code)]` and explicitly documents itself as "not
//! yet wired into `main.rs`". Raising the shipped floor to cover a feature
//! nothing calls yet would exclude every macOS 11/12 user for no present
//! benefit. Per-API disposition, so the choice is explicit rather than
//! implicit in a single number:
//!
//! | API | Needs | Status here | Disposition |
//! |---|---|---|---|
//! | `CGEventTap`, `NSPanel`, `CALayer`, `NSSound`/`NSStatusBar`/etc., `AXIsProcessTrusted`, `AVCaptureDevice` mic auth | ≤ 10.14 | wired, load-bearing | well under the floor; no action needed |
//! | `NSImage.imageWithSystemSymbolName` (SF Symbols, menu-bar icon) | 11.0 | wired, **optional** | already degrades: `menubar.rs`'s `state_image` falls back to a drawn icon when the call returns `None` (see its own comment: "All five are stock symbols shipped since macOS 11"). At floor 11.0 this never actually degrades on a supported machine; kept as a real fallback anyway since it costs nothing and errs safe if that ever changes. |
//! | `SMAppService` (launch-at-login) | 13.0 | **dead code, not wired into `main.rs`** | **not raising the floor for this.** When a future unit wires it in, it must guard the call with a runtime OS-version check against [`MIN_MACOS_VERSION`]/[`meets_floor`] (or `objc2`'s `available!` macro) and degrade the settings toggle to disabled-with-an-explanation below 13.0 -- exactly the graceful-degradation shape SF Symbols already uses, not a floor bump. Recorded here so that unit inherits the decision instead of re-deriving it. |
//!
//! `objc2` 0.6.4 (this workspace's pin) ships an `available!` macro for
//! exactly this "guard a call newer than the deployment target" situation
//! and documents that skipping the guard risks "crashing e.g. because of an
//! undefined selector" -- confirming the risk this module's runtime check
//! exists to remove for the app as a whole, and the mechanism any future
//! `SMAppService` wiring should reuse for that one API specifically.
//!
//! ## Decision 3 — device tier is a first-class, testable report
//!
//! SPEC.md §3.1 requires device/tier detection; separately, every existing
//! latency number in this project is captioned "from an M1 Max" precisely
//! because it is a ceiling, not a typical result. [`DeviceTier`] answers
//! "what did *this* run actually happen on" -- architecture, chip, physical
//! core split, RAM, macOS version, and whether the build includes the Metal
//! path -- as a real (`sysctlbyname`-backed on macOS), serializable struct:
//! consumed today by the `textify-voice device-tier` subcommand
//! ([`run_device_tier`]) and shaped so a future diagnostics/crash-reporter
//! unit (out of this unit's scope) can embed or serialize it alongside a
//! report, since without it a bug report has no way to say which of "M1
//! Max, 32 GPU cores" vs. "M1, 7 GPU cores" it came from.
//!
//! ## Testability
//!
//! [`classify_arch`], [`parse_os_version`], [`meets_floor`], and
//! [`floor_blocking_reason`] are pure functions over plain values (a build
//! `ARCH` string, `Option<bool>` probe results, version tuples) with no
//! `sysctlbyname`/ObjC call inside them -- the comparison logic is exercised
//! by this module's tests across many synthetic inputs even though the one
//! box this was built on can only ever report a single real
//! (aarch64, macOS 26.6, M1 Max) combination. The real-detection functions
//! (`detect_arch`, `DeviceTier::detect`, …) are thin, deliberately
//! untestable-in-the-abstract wrappers around those pure functions --
//! exactly the seam `permissions.rs` already uses for the same reason (see
//! its own module doc).

use std::fmt::Write as _;

// ---------------------------------------------------------------------
// Architecture
// ---------------------------------------------------------------------

/// The result of classifying this process's build architecture against the
/// hardware it is actually running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchSupport {
    /// Native `aarch64` on Apple Silicon. The only supported case in v1.
    AppleSilicon,
    /// Native `x86_64` on real Intel hardware (no Rosetta involved).
    IntelNative,
    /// An `x86_64` build running translated under Rosetta 2 -- on Apple
    /// Silicon hardware, where the real, faster, supported build exists.
    IntelViaRosetta,
    /// Any `std::env::consts::ARCH` value this binary has never targeted
    /// (kept exhaustive-safe rather than panicking on an unrecognized
    /// future arch string).
    Unknown(String),
}

impl ArchSupport {
    /// True only for [`ArchSupport::AppleSilicon`] -- see this module's
    /// "Decision 1" docs for why every other case is refused in v1.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        matches!(self, ArchSupport::AppleSilicon)
    }

    /// Short human label for the device-tier report.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            ArchSupport::AppleSilicon => "Apple Silicon (arm64)".to_string(),
            ArchSupport::IntelNative => "Intel (x86_64, native)".to_string(),
            ArchSupport::IntelViaRosetta => {
                "x86_64 build under Rosetta 2 on Apple Silicon hardware".to_string()
            }
            ArchSupport::Unknown(arch) => format!("unrecognized architecture ({arch})"),
        }
    }

    /// One sentence explaining why this architecture is refused, or `None`
    /// if it is supported. What [`check_startup`] prints verbatim before
    /// exiting.
    #[must_use]
    pub fn blocking_reason(&self) -> Option<String> {
        match self {
            ArchSupport::AppleSilicon => None,
            ArchSupport::IntelNative => Some(
                "Textify Voice requires an Apple Silicon Mac (M1 or later) -- Intel Macs \
                 are not supported in this release because CPU-only local transcription has \
                 never been measured against push-to-talk's latency budget."
                    .to_string(),
            ),
            ArchSupport::IntelViaRosetta => Some(
                "Textify Voice requires an Apple Silicon Mac (M1 or later) -- this copy was \
                 built for Intel and is running translated under Rosetta 2 on Apple Silicon \
                 hardware; download the Apple Silicon build instead."
                    .to_string(),
            ),
            ArchSupport::Unknown(arch) => Some(format!(
                "Textify Voice requires an Apple Silicon Mac (M1 or later) -- this machine \
                 reports an unrecognized architecture ({arch})."
            )),
        }
    }
}

/// Pure classification: given the build's own architecture string
/// (`std::env::consts::ARCH`) and two optional hardware probes, decide
/// which [`ArchSupport`] case this is. No I/O, no OS calls -- see
/// [`detect_arch`] for the real caller.
///
/// - `hw_optional_arm64`: the `hw.optional.arm64` sysctl, `Some(true)` only
///   on Apple Silicon hardware (native or under Rosetta); `None` when the
///   sysctl doesn't exist at all (real Intel hardware).
/// - `proc_translated`: the `sysctl.proc_translated` sysctl, `Some(true)`
///   only for an x86_64 process currently being translated by Rosetta 2;
///   `None` where the sysctl doesn't exist (real Intel hardware).
#[must_use]
pub fn classify_arch(
    build_arch: &str,
    hw_optional_arm64: Option<bool>,
    proc_translated: Option<bool>,
) -> ArchSupport {
    match build_arch {
        "aarch64" => ArchSupport::AppleSilicon,
        "x86_64" => {
            if proc_translated == Some(true) || hw_optional_arm64 == Some(true) {
                ArchSupport::IntelViaRosetta
            } else {
                ArchSupport::IntelNative
            }
        }
        other => ArchSupport::Unknown(other.to_string()),
    }
}

/// Real detection: this build's own `ARCH` plus (on macOS) two live
/// `sysctlbyname` probes. See [`classify_arch`] for the decision logic this
/// wraps.
#[must_use]
pub fn detect_arch() -> ArchSupport {
    #[cfg(target_os = "macos")]
    {
        classify_arch(
            std::env::consts::ARCH,
            detect_hw_optional_arm64(),
            detect_proc_translated(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        classify_arch(std::env::consts::ARCH, None, None)
    }
}

// ---------------------------------------------------------------------
// macOS version floor
// ---------------------------------------------------------------------

/// `(major, minor, patch)`.
pub type OsVersion = (u32, u32, u32);

/// The macOS version this build actually requires -- see this module's
/// "Decision 2" docs for the derivation. `packaging/Info.plist.template`'s
/// `LSMinimumSystemVersion` is set FROM this value, not the other way
/// around; if you change one, change the other (both files carry a
/// cross-reference comment saying so).
pub const MIN_MACOS_VERSION: OsVersion = (11, 0, 0);

/// Parses a macOS product-version string (`"14.5"`, `"26.6"`, `"11.0.1"`,
/// even a bare `"12"`) into `(major, minor, patch)`. Missing trailing
/// components default to 0, matching how `kern.osproductversion` and
/// `sw_vers -productVersion` actually format (no trailing `.0`). `None` for
/// anything that doesn't parse as a leading non-negative integer -- this is
/// a defensive parser for a string macOS itself controls, not a permissive
/// one for arbitrary input.
#[must_use]
pub fn parse_os_version(raw: &str) -> Option<OsVersion> {
    let mut parts = raw.trim().split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    let patch: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    Some((major, minor, patch))
}

/// `current >= floor`. Tuple `PartialOrd` is lexicographic, which is
/// exactly major.minor.patch version-comparison semantics for a
/// three-component version this shaped.
#[must_use]
pub fn meets_floor(current: OsVersion, floor: OsVersion) -> bool {
    current >= floor
}

/// One sentence if `current` is known and below `floor`; `None` if it meets
/// the floor, **and** `None` if `current` could not be determined at all
/// (non-macOS build, or the `sysctlbyname` read failed). Deliberately not
/// fail-closed on an unreadable version: "we could not read the OS
/// version" is a different, far rarer failure than "the OS is too old," and
/// this gate should not turn an internal detection miss into a false
/// "your Mac is unsupported" message.
#[must_use]
pub fn floor_blocking_reason(current: Option<OsVersion>, floor: OsVersion) -> Option<String> {
    let current = current?;
    if meets_floor(current, floor) {
        None
    } else {
        Some(format!(
            "Textify Voice requires macOS {}.{} or later -- this Mac is running macOS {}.{}. \
             Please update macOS and reinstall.",
            floor.0, floor.1, current.0, current.1
        ))
    }
}

/// Real detection: `kern.osproductversion` via `sysctlbyname` on macOS
/// (`None` on any other target, and `None` if the sysctl read itself
/// fails).
#[must_use]
pub fn detect_macos_version_raw() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        sysctl::read_string("kern.osproductversion")
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// [`detect_macos_version_raw`], parsed via [`parse_os_version`].
#[must_use]
pub fn detect_macos_version() -> Option<OsVersion> {
    detect_macos_version_raw().and_then(|raw| parse_os_version(&raw))
}

// ---------------------------------------------------------------------
// Combined startup gate
// ---------------------------------------------------------------------

/// What `main()` decides to do before running anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupCheck {
    /// Proceed normally.
    Ok,
    /// Print this one-sentence message to stderr and exit(1) -- do not
    /// half-run.
    Blocked(String),
}

/// The one call `main()` makes before doing anything else (except, by
/// design, before dispatching the `device-tier` subcommand itself -- that
/// diagnostic should still work on unsupported hardware, precisely to
/// explain why it's unsupported). Checks architecture first: an unsupported
/// architecture is the more fundamental problem and produces the more
/// specific message; the OS-floor check only runs if architecture already
/// passed.
#[must_use]
pub fn check_startup() -> StartupCheck {
    let arch = detect_arch();
    if let Some(reason) = arch.blocking_reason() {
        return StartupCheck::Blocked(reason);
    }
    if let Some(reason) = floor_blocking_reason(detect_macos_version(), MIN_MACOS_VERSION) {
        return StartupCheck::Blocked(reason);
    }
    StartupCheck::Ok
}

// ---------------------------------------------------------------------
// Device tier report
// ---------------------------------------------------------------------

/// A snapshot of the machine this process is actually running on --
/// architecture, chip, physical core split, RAM, macOS version, and
/// whether this build includes the Metal-accelerated whisper.cpp path. See
/// this module's "Decision 3" docs for why it exists and who consumes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeviceTier {
    pub arch: String,
    pub arch_supported: bool,
    pub chip: Option<String>,
    pub performance_cores: Option<u32>,
    pub efficiency_cores: Option<u32>,
    pub total_cores: Option<u32>,
    pub ram_bytes: Option<u64>,
    pub macos_version: Option<String>,
    pub metal_available: bool,
}

impl DeviceTier {
    /// Real detection. On macOS: chip brand, perf/efficiency physical core
    /// counts, total physical core count, RAM, and OS version, all via
    /// `sysctlbyname` (see this module's `sysctl` submodule) -- the same
    /// mechanism [`detect_arch`]/[`detect_macos_version`] use. On
    /// non-macOS this still returns a value (no panics): `arch`/
    /// `arch_supported` reflect the real `env::consts::ARCH`, every other
    /// field is `None`, and `metal_available` is `false`.
    #[must_use]
    pub fn detect() -> Self {
        let arch = detect_arch();
        DeviceTier {
            arch_supported: arch.is_supported(),
            arch: arch.label(),
            chip: detect_chip_brand(),
            performance_cores: detect_perf_cores(),
            efficiency_cores: detect_efficiency_cores(),
            total_cores: detect_total_cores(),
            ram_bytes: detect_ram_bytes(),
            macos_version: detect_macos_version_raw(),
            metal_available: build_includes_metal(),
        }
    }

    /// Multi-line human-readable report -- what `textify-voice device-tier`
    /// prints. `DeviceTier` also derives `Serialize` so a future
    /// diagnostics/crash-reporter unit can embed the same data as JSON
    /// instead of reformatting it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "architecture     : {} ({})",
            self.arch,
            if self.arch_supported { "supported" } else { "NOT SUPPORTED" }
        );
        let _ = writeln!(out, "chip             : {}", self.chip.as_deref().unwrap_or("unknown"));
        let _ = write!(
            out,
            "cores            : {}",
            self.total_cores.map_or_else(|| "unknown".to_string(), |c| format!("{c} total"))
        );
        if let (Some(p), Some(e)) = (self.performance_cores, self.efficiency_cores) {
            let _ = write!(out, " ({p} performance + {e} efficiency)");
        }
        out.push('\n');
        let _ = writeln!(
            out,
            "memory           : {}",
            self.ram_bytes.map_or_else(|| "unknown".to_string(), format_gib)
        );
        let _ = writeln!(out, "macOS            : {}", self.macos_version.as_deref().unwrap_or("unknown"));
        let _ = writeln!(
            out,
            "Metal (whisper)  : {}",
            if self.metal_available { "available" } else { "not available (CPU-only whisper.cpp)" }
        );
        out
    }
}

fn format_gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
}

/// Mirrors `crates/voice-asr-whisper/Cargo.toml`'s exact target predicate
/// for the `metal` feature (`cfg(all(target_os = "macos", target_arch =
/// "aarch64"))`). Duplicated here deliberately, not derived --
/// `voice-asr-whisper` doesn't expose this as a queryable constant, so this
/// is the one other place in the tree that must be kept in sync with that
/// crate's `Cargo.toml` if its target predicate ever changes.
const fn build_includes_metal() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

/// `textify-voice device-tier` -- prints [`DeviceTier::detect`]'s report.
pub fn run_device_tier() -> anyhow::Result<()> {
    print!("{}", DeviceTier::detect().render());
    Ok(())
}

// ---------------------------------------------------------------------
// macOS sysctl probes
// ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn detect_hw_optional_arm64() -> Option<bool> {
    sysctl::read_i32("hw.optional.arm64").map(|v| v != 0)
}

#[cfg(target_os = "macos")]
fn detect_proc_translated() -> Option<bool> {
    sysctl::read_i32("sysctl.proc_translated").map(|v| v != 0)
}

#[cfg(target_os = "macos")]
fn detect_chip_brand() -> Option<String> {
    sysctl::read_string("machdep.cpu.brand_string")
}
#[cfg(not(target_os = "macos"))]
fn detect_chip_brand() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn detect_perf_cores() -> Option<u32> {
    sysctl::read_i32("hw.perflevel0.physicalcpu").and_then(|v| u32::try_from(v).ok())
}
#[cfg(not(target_os = "macos"))]
fn detect_perf_cores() -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
fn detect_efficiency_cores() -> Option<u32> {
    sysctl::read_i32("hw.perflevel1.physicalcpu").and_then(|v| u32::try_from(v).ok())
}
#[cfg(not(target_os = "macos"))]
fn detect_efficiency_cores() -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
fn detect_total_cores() -> Option<u32> {
    sysctl::read_i32("hw.physicalcpu").and_then(|v| u32::try_from(v).ok())
}
#[cfg(not(target_os = "macos"))]
fn detect_total_cores() -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
fn detect_ram_bytes() -> Option<u64> {
    sysctl::read_u64("hw.memsize")
}
#[cfg(not(target_os = "macos"))]
fn detect_ram_bytes() -> Option<u64> {
    None
}

/// Thin `sysctlbyname` wrappers. Every function here returns `None` on any
/// failure -- a sysctl not existing on this hardware/OS combination is a
/// real, expected outcome (e.g. `sysctl.proc_translated` simply does not
/// exist on real Intel hardware; `hw.perflevel1.physicalcpu` does not exist
/// on a single-cluster chip), not a bug to `unwrap` through.
#[cfg(target_os = "macos")]
mod sysctl {
    use std::ffi::CString;

    /// Reads a NUL-terminated C-string sysctl (e.g.
    /// `"kern.osproductversion"`, `"machdep.cpu.brand_string"`) via the
    /// standard two-call `sysctlbyname` pattern: first call sizes the
    /// buffer, second call fills it.
    pub fn read_string(name: &str) -> Option<String> {
        let cname = CString::new(name).ok()?;
        let mut len: libc::size_t = 0;
        // SAFETY: `cname` is a valid NUL-terminated C string owned for the
        // duration of this call. Passing a null `oldp` with a valid
        // `oldlenp` is the documented way to query the required buffer
        // size without reading any data yet.
        let rc = unsafe {
            libc::sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0)
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` is exactly `len` bytes, matching `oldlenp`;
        // `sysctlbyname` writes at most that many bytes into it and updates
        // `len` to the number actually written.
        let rc = unsafe {
            libc::sysctlbyname(cname.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }

    /// Reads a fixed-size `i32` sysctl (e.g. `"hw.optional.arm64"`,
    /// `"sysctl.proc_translated"`, `"hw.physicalcpu"`).
    pub fn read_i32(name: &str) -> Option<i32> {
        let cname = CString::new(name).ok()?;
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as libc::size_t;
        // SAFETY: `value` is a valid, initialized `i32`; `len` matches its
        // exact size, which is what a fixed-size integer sysctl expects.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::from_mut(&mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len != std::mem::size_of::<i32>() {
            return None;
        }
        Some(value)
    }

    /// Reads a fixed-size `u64` sysctl (e.g. `"hw.memsize"`).
    pub fn read_u64(name: &str) -> Option<u64> {
        let cname = CString::new(name).ok()?;
        let mut value: u64 = 0;
        let mut len = std::mem::size_of::<u64>() as libc::size_t;
        // SAFETY: same contract as `read_i32`, sized for `u64`.
        let rc = unsafe {
            libc::sysctlbyname(
                cname.as_ptr(),
                std::ptr::from_mut(&mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len != std::mem::size_of::<u64>() {
            return None;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    // ---- classify_arch: pure, synthetic inputs ----

    #[test]
    fn aarch64_is_always_apple_silicon_regardless_of_probes() {
        for (arm64, translated) in [
            (None, None),
            (Some(true), Some(false)),
            (Some(false), Some(false)),
            (Some(true), Some(true)),
        ] {
            assert_eq!(classify_arch("aarch64", arm64, translated), ArchSupport::AppleSilicon);
        }
    }

    #[test]
    fn x86_64_on_real_intel_hardware_is_intel_native() {
        // The real Intel-hardware signature: both sysctls absent, because
        // neither exists outside Apple Silicon.
        assert_eq!(classify_arch("x86_64", None, None), ArchSupport::IntelNative);
    }

    #[test]
    fn x86_64_reported_translated_is_rosetta() {
        assert_eq!(
            classify_arch("x86_64", Some(true), Some(true)),
            ArchSupport::IntelViaRosetta
        );
    }

    #[test]
    fn x86_64_on_apple_silicon_hardware_is_rosetta_even_if_proc_translated_is_somehow_false() {
        // Belt-and-suspenders: an x86_64 *build* cannot run natively on
        // Apple-Silicon-only hardware, so `hw_optional_arm64 == Some(true)`
        // alone is enough to call it Rosetta even if the other probe is
        // missing or reports otherwise.
        assert_eq!(
            classify_arch("x86_64", Some(true), None),
            ArchSupport::IntelViaRosetta
        );
        assert_eq!(
            classify_arch("x86_64", Some(true), Some(false)),
            ArchSupport::IntelViaRosetta
        );
    }

    #[test]
    fn unrecognized_arch_string_is_unknown_not_a_panic() {
        assert_eq!(
            classify_arch("riscv64", None, None),
            ArchSupport::Unknown("riscv64".to_string())
        );
    }

    #[test]
    fn only_apple_silicon_is_supported() {
        assert!(ArchSupport::AppleSilicon.is_supported());
        assert!(!ArchSupport::IntelNative.is_supported());
        assert!(!ArchSupport::IntelViaRosetta.is_supported());
        assert!(!ArchSupport::Unknown("riscv64".to_string()).is_supported());
    }

    #[test]
    fn blocking_reason_is_one_sentence_for_every_unsupported_case_and_none_for_supported() {
        assert_eq!(ArchSupport::AppleSilicon.blocking_reason(), None);
        for unsupported in [
            ArchSupport::IntelNative,
            ArchSupport::IntelViaRosetta,
            ArchSupport::Unknown("riscv64".to_string()),
        ] {
            let reason = unsupported.blocking_reason().expect("must be blocked");
            assert!(!reason.is_empty());
            // "One sentence" -- exactly one terminal '.', at the end.
            assert_eq!(reason.matches('.').count(), 1);
            assert!(reason.trim_end().ends_with('.'));
        }
    }

    // ---- parse_os_version: pure, many synthetic version strings ----

    #[test]
    fn parses_real_macos_version_strings_seen_in_the_wild() {
        assert_eq!(parse_os_version("10.15.7"), Some((10, 15, 7)));
        assert_eq!(parse_os_version("11.0"), Some((11, 0, 0)));
        assert_eq!(parse_os_version("11.0.1"), Some((11, 0, 1)));
        assert_eq!(parse_os_version("12.6"), Some((12, 6, 0)));
        assert_eq!(parse_os_version("13.0"), Some((13, 0, 0)));
        assert_eq!(parse_os_version("13.2.1"), Some((13, 2, 1)));
        assert_eq!(parse_os_version("14.5"), Some((14, 5, 0)));
        assert_eq!(parse_os_version("15.4.1"), Some((15, 4, 1)));
        assert_eq!(parse_os_version("26.6"), Some((26, 6, 0)));
    }

    #[test]
    fn parses_a_bare_major_version() {
        assert_eq!(parse_os_version("12"), Some((12, 0, 0)));
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert_eq!(parse_os_version("  14.5 \n"), Some((14, 5, 0)));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "abc", "14.x", ".", "14..", "-1.0"] {
            assert_eq!(parse_os_version(bad), None, "expected None for {bad:?}");
        }
    }

    // ---- meets_floor / floor_blocking_reason: pure ----

    #[test]
    fn meets_floor_covers_above_at_and_below_the_boundary() {
        let floor = (11, 0, 0);
        assert!(meets_floor((11, 0, 0), floor), "exactly at floor");
        assert!(meets_floor((11, 0, 1), floor), "patch above");
        assert!(meets_floor((11, 1, 0), floor), "minor above");
        assert!(meets_floor((12, 0, 0), floor), "major above");
        assert!(meets_floor((26, 6, 0), floor), "far above (this box's real version)");
        assert!(!meets_floor((10, 15, 7), floor), "major below");
        assert!(!meets_floor((10, 15, 7), (10, 15, 8)), "patch below, same major/minor");
    }

    #[test]
    fn floor_blocking_reason_is_none_at_or_above_floor() {
        assert_eq!(floor_blocking_reason(Some((11, 0, 0)), MIN_MACOS_VERSION), None);
        assert_eq!(floor_blocking_reason(Some((26, 6, 0)), MIN_MACOS_VERSION), None);
    }

    #[test]
    fn floor_blocking_reason_is_some_below_floor_and_mentions_both_versions() {
        let reason = floor_blocking_reason(Some((10, 15, 7)), MIN_MACOS_VERSION)
            .expect("macOS 10.15 is below the 11.0 floor");
        assert!(reason.contains("11.0"));
        assert!(reason.contains("10.15"));
    }

    #[test]
    fn floor_blocking_reason_is_none_when_current_is_unknown() {
        // Detection failure is not the same claim as "too old" -- see this
        // function's doc comment.
        assert_eq!(floor_blocking_reason(None, MIN_MACOS_VERSION), None);
    }

    #[test]
    fn every_macos_version_since_big_sur_meets_the_floor() {
        for v in [
            (11, 0, 0),
            (11, 7, 10),
            (12, 7, 6),
            (13, 7, 1),
            (14, 7, 5),
            (15, 5, 0),
            (26, 6, 0),
        ] {
            assert!(meets_floor(v, MIN_MACOS_VERSION), "{v:?} should meet the 11.0 floor");
        }
    }

    #[test]
    fn every_pre_big_sur_version_fails_the_floor() {
        for v in [(10, 9, 0), (10, 13, 6), (10, 14, 6), (10, 15, 7)] {
            assert!(!meets_floor(v, MIN_MACOS_VERSION), "{v:?} should fail the 11.0 floor");
        }
    }

    // ---- check_startup: real detection, on THIS box ----

    #[test]
    fn check_startup_passes_on_this_apple_silicon_box() {
        // This test only proves the real detection path runs and returns
        // `Ok` on the one machine this was built on (aarch64, real macOS,
        // per this unit's verification notes) -- it is not a claim about
        // any other machine. See `detect_arch`/`detect_macos_version`
        // for the seam a future test double would replace.
        assert_eq!(check_startup(), StartupCheck::Ok);
    }

    #[test]
    fn detect_arch_reports_apple_silicon_on_this_box() {
        assert_eq!(detect_arch(), ArchSupport::AppleSilicon);
    }

    #[test]
    fn detect_macos_version_is_present_and_meets_the_floor_on_this_box() {
        let current = detect_macos_version().expect("kern.osproductversion must be readable");
        assert!(meets_floor(current, MIN_MACOS_VERSION));
    }

    // ---- DeviceTier ----

    #[test]
    fn device_tier_detect_runs_for_real_and_reports_this_box_correctly() {
        let tier = DeviceTier::detect();
        assert_eq!(tier.arch, ArchSupport::AppleSilicon.label());
        assert!(tier.arch_supported);
        assert!(tier.metal_available, "this box builds the aarch64+macOS metal path");
        assert!(tier.chip.is_some(), "machdep.cpu.brand_string should read on this box");
        assert!(tier.ram_bytes.is_some(), "hw.memsize should read on this box");
        assert!(tier.macos_version.is_some());
    }

    #[test]
    fn device_tier_render_includes_every_field_label() {
        let report = DeviceTier::detect().render();
        for label in ["architecture", "chip", "cores", "memory", "macOS", "Metal"] {
            assert!(report.contains(label), "report missing {label:?}:\n{report}");
        }
    }

    #[test]
    fn device_tier_render_never_panics_with_all_fields_absent() {
        // Synthetic all-None tier (what a non-macOS build reports) --
        // `render` must still produce a readable, non-panicking string.
        let tier = DeviceTier {
            arch: ArchSupport::Unknown("riscv64".to_string()).label(),
            arch_supported: false,
            chip: None,
            performance_cores: None,
            efficiency_cores: None,
            total_cores: None,
            ram_bytes: None,
            macos_version: None,
            metal_available: false,
        };
        let report = tier.render();
        assert!(report.contains("unknown"));
        assert!(report.contains("NOT SUPPORTED"));
    }

    #[test]
    fn build_includes_metal_matches_this_boxs_known_target() {
        // This box is aarch64-apple-darwin, which is exactly
        // voice-asr-whisper's metal-enabled target predicate.
        assert!(build_includes_metal());
    }
}
