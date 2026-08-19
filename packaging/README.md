# Packaging — Textify Voice.app

This directory produces a real macOS `.app` bundle around the `textify-voice`
binary. That is the whole point of this unit: today, permissions (Microphone,
Accessibility) are granted to **Terminal.app**, because `textify-voice` is a
bare CLI binary invoked from a shell with no bundle identity of its own for
TCC to key a grant against. Wrapping it in a signed `.app` gives it that
identity, so the grant follows *Textify Voice*, not whatever terminal you
happened to launch it from.

## Build it

```sh
packaging/build-bundle.sh
```

This runs `cargo build --release -p voice-cli`, then assembles, ad-hoc-signs,
and verifies `packaging/dist/Textify Voice.app`. Pass `--no-build` to skip the
cargo step and reuse whatever is already at `target/release/textify-voice`.
Run `packaging/build-bundle.sh --help` for all options (`--out`, `--bundle-id`,
`--sign-identity`).

`packaging/dist/` is build output — it is already covered by the repo's
top-level `.gitignore` (`dist/`) and is never committed.

## Where the alpha is actually published

As of the first alpha this is no longer hypothetical. The release lives in
the Cloudflare R2 bucket `textify-downloads`, served from the custom domain
`downloads.textify.me`:

| Object | Purpose | Cache |
| --- | --- | --- |
| `voice/textify-voice-<ver>.dmg` | the human download, linked from `/voice` | immutable |
| `voice/textify-voice-<ver>.zip` | the update payload `update.rs` installs | immutable |
| `voice/appcast.json` | the version manifest every build polls | `max-age=300` |

The whisper weights are mirrored separately, in the existing `textify-models`
bucket under `voice/` (`models.textify.me/voice/ggml-*.bin`), which is the
primary origin for first-run model downloads with HuggingFace kept as an
automatic fallback. Integrity there does not depend on the origin: every
downloaded model is checked against a pinned SHA-256.

Release builds are signed with the **stable self-signed identity**, not
ad-hoc. That does nothing for Gatekeeper (see below) but it is not
cosmetic: macOS keys TCC grants to the code signature, and an ad-hoc
signature's identity is the binary's own hash, so every auto-update would
silently revoke Microphone and Accessibility on every user's machine. A
stable identity is what lets a permission granted to 0.1.0 still hold for
0.1.1. Filenames are URL-safe (`textify-voice-0.1.0.dmg`, not
`Textify Voice-0.1.0.dmg`) so no URL in the appcast, the download page, or a
support email depends on `%20` being encoded correctly.

The full release sequence is in `packaging/appcast/README.md`.

## Beta distribution: what a downloading dev will actually see

This is the beta reality, stated plainly: **there is no Apple Developer
Program account behind this build.** Enrolling ($99/yr, and it isn't
instant — Apple reviews new enrollments) is out of scope for this wave on
purpose; everything else that blocks a real download is in scope, and this
section is that "everything else," documented rather than glossed over.

**What this means concretely: the bundle is signed, but not by an identity
Gatekeeper trusts, and it is not notarized.** Two consequences, and they are
different problems even though they look similar from the dialog box:

1. **Signed ≠ trusted.** `codesign` produces a real, valid signature (verified
   below) — but with no Apple-issued Developer ID behind it, Gatekeeper has
   no chain of trust to check it against. A self-signed identity (see
   `make-dev-cert.sh`) is not a substitute for a Developer ID certificate;
   it solves a *different* problem (a stable identity for your own TCC
   grants to survive rebuilds during development, see below), not
   Gatekeeper trust for someone else's Mac.
2. **Not notarized** — nobody ran `xcrun notarytool submit` against Apple's
   service, so there is no notarization ticket for Gatekeeper to staple or
   check, independent of signing.

**What a downloading dev will see, per Apple's own documented Gatekeeper
behavior** (this is platform behavior described in Apple's own developer
documentation, not something observable in this headless environment — no
sandbox here can complete a real browser download + GUI dialog interaction;
see "What could not be verified" at the end of this file):

1. They download `Textify Voice.app` (most likely inside a `.zip` or
   `.dmg`) via a browser. macOS attaches the `com.apple.quarantine`
   extended attribute to anything downloaded this way — `xattr -l` on a
   quarantined file shows it; this is what actually triggers Gatekeeper's
   check, independent of the app's own signature state.
2. Double-clicking the quarantined app produces a dialog to the effect of
   *""Textify Voice" cannot be opened because Apple cannot check it for
   malicious software."* — the standard unnotarized-app message, with only
   a "Done"/"Move to Trash" choice, no "Open Anyway" button on this first
   dialog (that changed in recent macOS versions; older ones showed an
   "Open Anyway" button directly in System Settings → Privacy & Security
   instead — both variants exist across supported macOS versions, so don't
   be surprised if it looks slightly different).
3. **The workaround — this is the instruction to give a beta tester:**
   **Control-click (or right-click) `Textify Voice.app` in Finder → choose
   "Open" from the context menu → click "Open" again in the dialog that
   appears.** This is a distinct code path from a plain double-click:
   Control-click-Open shows a dialog that *does* offer an explicit "Open"
   button even for an unnotarized app, once per app (macOS remembers the
   decision after that — subsequent double-clicks work normally). This is
   long-standing, intentional Gatekeeper behavior for exactly this
   situation, not a bug or a hidden workaround.
4. This is a one-time step per downloaded copy. A *rebuilt* copy (new
   CDHash, since this is ad-hoc/self-signed — see "What ad-hoc signing does
   and does not give you" below) is a new quarantine decision as far as
   Gatekeeper is concerned, so the same Control-click-Open step applies
   again after every update until Developer ID + notarization exist.

## How to verify what you downloaded

With no notarization, there's no Apple-backed chain proving a downloaded
`.zip`/`.dmg` reached you unmodified — so give beta testers a way to verify
it themselves against what was actually built:

- **Publish a SHA-256 of the release archive** alongside the download link
  (e.g. `shasum -a 256 "Textify Voice.zip"` on the machine that built and
  uploaded it), and tell testers to run the same command and compare before
  opening it. This is the same integrity model this app's own model
  downloader now uses internally — see "Model hosting" below — applied to
  the app itself.
- **Inspect the signature directly**, which works even without Gatekeeper
  trust:
  ```sh
  codesign -dv --verbose=4 "Textify Voice.app"
  spctl --assess --type execute --verbose=4 "Textify Voice.app"
  ```
  `codesign -dv` shows `Identifier=com.textify.voice` and a `CDHash` — a
  content hash of exactly what was signed. `spctl --assess` on an
  unnotarized copy is *expected* to print `rejected`; that's normal, not
  evidence of tampering (see "What ad-hoc signing does and does not give
  you" below for what these two commands do and don't actually prove).
- **Read `Contents/Resources/BuildInfo.txt`** inside the bundle (right-click
  → Show Package Contents) — the exact git SHA, whether the working tree was
  clean at build time, and a UTC build timestamp. See "Build provenance"
  below. This is what should go in a bug report, verbatim.

## Licenses

`Contents/Resources/Licenses/` ships in every build (copied by this script
from `packaging/licenses/`, committed to the repo, not regenerated per
build). **Start at `Licenses/THIRD-PARTY-NOTICES.txt`** — one plain-text
file covering every one of the 102 third-party Rust crates actually
compiled into the aarch64-apple-darwin release binary (not a hand-kept
list — resolved from `Cargo.lock` via `cargo license
--filter-platform aarch64-apple-darwin`), plus whisper.cpp (the vendored
C/C++ library) and the ggml model weights downloaded at first run.

All of it is permissively licensed except one dependency:
**`option-ext` 0.2.0 (pulled in unconditionally by `dirs`/`dirs-sys`) is
MPL-2.0**, a file-level weak-copyleft license — real, worth knowing about,
and does not require this application to be open-sourced (its own source is
unmodified here; MPL-2.0's copyleft applies to modifications of its own
covered files, not to a larger work that merely links against it). Full
detail, including how this was audited and what to re-run after a
dependency bump, is in `packaging/licenses/README.md`.

## Model hosting

First run downloads a ~75–148 MB `ggml-*.bin` whisper.cpp model file. By
default that comes from a third party's HuggingFace repo
(`ggerganov/whisper.cpp`) — fine for now, but a single point of failure:
if that repo is ever renamed, moved, or rate-limits us, every new install
breaks, and we'd find out from users, not from monitoring.

**The base URL is configurable with no code change**, via
`TEXTIFY_MODEL_BASE_URL`:

```sh
export TEXTIFY_MODEL_BASE_URL="https://<your-r2-bucket-or-custom-domain>"
```

`<base>/<filename>` is requested per model (e.g. `<base>/ggml-base.en.bin`).
Unset or empty, it falls back to the HuggingFace URL above — see
`crates/voice-asr-whisper/src/model.rs`'s `MODEL_BASE_URL_ENV_VAR`.

**Integrity is enforced independently of which base URL served the file.**
Each model's SHA-256 is pinned in source (`ModelId::expected_sha256`,
cross-checked against both a local `shasum -a 256` and HuggingFace's own
published git-lfs object ID) and verified after every download. A mismatch
deletes the file and fails the download closed — an R2 copy that's even one
byte different from the pinned HuggingFace original is rejected exactly
like a corrupted download would be, which is a feature: it also catches a
bad upload, not just an attack.

**R2 credentials do not exist in this environment**, so the upload itself
was not (and could not be) performed here. The exact steps whoever holds
R2 credentials needs to run:

```sh
# 1. Confirm you have the exact bytes: HuggingFace and this repo's pinned
#    hash must match before you upload, or you're baking in a mismatch.
curl -fsSL -o ggml-tiny.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
curl -fsSL -o ggml-base.en.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
shasum -a 256 ggml-tiny.en.bin ggml-base.en.bin
# Compare against ModelId::expected_sha256() in model.rs -- must match
# exactly. If it doesn't, STOP: either upstream changed the file (update
# the pin deliberately, see model.rs's doc comment) or your download is
# corrupt -- do not upload a file that fails this check.

# 2. Upload to R2 (bucket layout: filenames directly at the bucket root,
#    matching ModelId::filename() -- ModelId::url() joins <base>/<filename>
#    with no path prefix, so don't nest these under a subdirectory unless
#    you also change TEXTIFY_MODEL_BASE_URL to include that prefix).
wrangler r2 object put your-bucket-name/ggml-tiny.en.bin --file ggml-tiny.en.bin
wrangler r2 object put your-bucket-name/ggml-base.en.bin --file ggml-base.en.bin
# (or the R2 dashboard, or `aws s3 cp` against R2's S3-compatible endpoint --
# whichever this org already uses for apps/web's R2 buckets.)

# 3. Point the app at it, either for local testing or by baking it into
#    packaged builds going forward:
export TEXTIFY_MODEL_BASE_URL="https://<public R2 URL or custom domain>"
```

Once this is done and confirmed working (a real `textify-voice models
--download base.en` against the R2 URL succeeding), promoting it from an
opt-in env var to this script's actual default is a one-line follow-up in
`model.rs` — deliberately not done here, since that would mean *pretending*
R2 hosting exists in this environment when it does not.

## Build provenance: `BuildInfo.txt`

Every build writes `Contents/Resources/BuildInfo.txt`: short git SHA,
whether the working tree was clean or had uncommitted changes at build
time (`git status --porcelain`, not just `git diff` — that also catches new
untracked files `git diff` would silently miss), a UTC build timestamp, the
sign identity used, and the bundle ID. This is what "identify an exact
build from a bug report" means in practice — a bare version number
(`0.1.0`) is shared across every build until the next release bump and
tells you nothing about which of the six-bugs-in-an-hour builds someone is
actually running. `CFBundleVersion` in `Info.plist` also carries the git
SHA (with a `-dirty` suffix on an unclean tree) for the same reason, kept
short since Apple has real expectations about what belongs in that field.

## What you actually grant, and where

The first time the bundled app requests Microphone access (i.e. the first
time you use `dictate` launched *as the app*, once the agent entry point
exists — see "One binary, two faces" below), macOS shows the standard
consent dialog and then lists **Textify Voice** — not Terminal — under
**System Settings → Privacy & Security → Microphone**. The same is true for
**Accessibility**, which the app needs for the global hold-key and the
synthesized paste.

Today, running the CLI directly from a terminal (`cargo run`, or the raw
`target/release/textify-voice` binary) still attributes those same grants to
your terminal app, exactly as it does now — building the bundle doesn't
change that path. Only running the bundled binary *from inside the `.app`*
gets you the new attribution.

## Why `CFBundleIdentifier` is load-bearing

`Info.plist.template`'s `CFBundleIdentifier` (`com.textify.voice` by default)
is not cosmetic. I verified directly on this machine that when `codesign`
signs a bundle, it reads `CFBundleIdentifier` out of `Info.plist` and embeds
it as the `Identifier=` field of the code signature itself
(`codesign -dvvv` on the built bundle shows `Identifier=com.textify.voice`).
TCC's permission database keys grants off that embedded identifier (via the
code signature's designated requirement), not off the app's file path or
display name.

**Consequence: changing `CFBundleIdentifier` after real users have granted
permissions is indistinguishable to macOS from shipping a brand-new app.**
Every previously granted Microphone/Accessibility permission becomes an
orphaned entry in System Settings, and the "new" app has to ask for consent
again from zero. Treat this string as a one-way door once this ships to
anyone — don't rename it for cosmetic reasons, don't fold it into a version
number, don't let it drift between dev and release builds.

## What ad-hoc signing does and does not give you

The build script signs with `codesign --sign -` (ad-hoc) by default, because
there is no Apple Developer account / Developer ID certificate available in
this environment. Concretely, verified on this machine:

- **It does produce a valid code signature.** `codesign --verify --deep
  --strict` passes, and the bundle satisfies its own designated requirement.
- **It does carry an identity `TCC` can key a grant to** — `Identifier=` is
  set from `CFBundleIdentifier` as above.
- **It does NOT carry a stable, cross-rebuild identity.** `codesign -dv`
  reports `TeamIdentifier=not set` for an ad-hoc signature — there is no
  Developer ID / Team ID backing it, only a `CDHash`, which is a pure content
  hash of the compiled bytes. I verified this three ways on this machine
  (identical rebuild → identical CDHash; a one-line source change → a
  different CDHash). **Rust release builds are not guaranteed
  bit-reproducible across separate build invocations** (codegen-unit
  ordering, embedded panic-location strings tied to absolute paths, etc. are
  documented, common sources of nondeterminism), and I did not verify
  `voice-cli`'s own binary is reproducible across two independent full
  rebuilds — that would need to be checked before relying on it.
- **The practical, day-to-day consequence — stated as documented platform
  behavior, not something this environment can directly observe (no TCC
  consent dialog can be completed here): for an ad-hoc-signed app, TCC's**
  synthesized code requirement is understood to fall back to pinning the
  CDHash itself (there is no Team ID to pin to instead). Given the CDHash
  instability just described, that means **you should expect to re-grant
  Microphone and Accessibility after most rebuilds** of an ad-hoc-signed
  bundle, not just after intentional version bumps. This is the same
  behavior widely reported for Homebrew casks and Electron dev builds that
  ship ad-hoc/unsigned binaries. Treat it as a daily-usability tax during
  development, not a footnote.
- **It does NOT satisfy Gatekeeper's quarantine assessment.** `spctl
  --assess --type execute` rejects the ad-hoc-signed bundle (verified
  below) — that's expected and separate from TCC. It only matters for a
  bundle that has been quarantined (e.g. downloaded via a browser); a
  locally built copy launches fine via `open` or double-click, which I also
  verified (see "Verification performed" below).

## Why there's no Accessibility usage-description key

`NSMicrophoneUsageDescription` is required in `Info.plist` — I confirmed
AVFoundation kills a process outright if it requests the microphone without
one. Accessibility has no equivalent key: I grepped the full macOS SDK for
`NSAccessibilityUsageDescription` and it does not exist anywhere in any
framework header, and I checked every installed app on this machine
(including a real shipping dictation app with the same Mic+AX profile) —
none declare one, all of them work. Accessibility consent is purely a System
Settings dialog the OS shows when the app first calls an AX API; there is
nothing to put in the plist for it. This isn't an oversight in
`Info.plist.template` — there's genuinely no key to add.

## One binary, two faces

`Contents/MacOS/textify-voice` inside the bundle is **the exact same Mach-O
binary** `cargo build --release -p voice-cli` produces — the build script
copies it in unmodified, no wrapper script, no second binary. Run from a
terminal with an explicit subcommand, it behaves exactly as it does today
(`transcribe`, `dictate`, `command`, `models` all unchanged — verified below).

**The contract for "launched as an app instead": argv inspection, on
argument count alone.**

When macOS starts an app bundle (double-click, Spotlight, `open -a`),
LaunchServices invokes `Contents/MacOS/<CFBundleExecutable>` with **no
arguments** — `argv` is just `[executable_path]`. That is a real, verified
platform fact, not an assumption: I built this bundle, launched it with
`open`, and confirmed via the unified log that a real `textify-voice`
process was spawned by LaunchServices at that path (see "Verification
performed"). A human always types an explicit subcommand; only an app-style
launch produces zero arguments. So:

> **If `std::env::args().count() == 1` (nothing after argv[0]), enter the
> persistent menu-bar agent loop. Otherwise, parse as today.**

**This is now implemented.** It was a real gap when this was written, and it
was proved to be one rather than assumed: the binary used to hit `Cli::parse()`'s
required `<COMMAND>` and exit 2 on zero arguments, before any of our code ran.
The check therefore has to happen **before** `Cli::parse()` is called, since
`Cli`'s `command: Cmd` field has no default and clap aborts the process on a
missing required subcommand. That is where it now lives.

Whichever unit adds the agent entry point owns adding roughly this to
`main.rs`, ahead of the existing `Cli::parse()` call:

```rust
fn main() {
    if std::env::args().count() == 1 {
        // Launched as a double-clicked/Finder/Dock .app with no subcommand:
        // this is the agent contract packaging/ relies on. See
        // packaging/README.md "One binary, two faces".
        std::process::exit(agent::run());
    }
    let cli = Cli::parse();
    // ... unchanged ...
}
```

I did not add this myself — it's out of this unit's scope
(`packaging/` only) and belongs to whichever unit implements the menu-bar
agent loop itself. This packaging unit's job was to decide the contract, make
sure the bundle actually invokes it (it does — the bundle ships the real
binary as `CFBundleExecutable` with no wrapper, so LaunchServices' normal
zero-arg launch reaches it untouched), and document it precisely enough that
the next unit doesn't have to reverse-engineer the decision.

## What's still missing for real distribution

See also "Beta distribution: what a downloading dev will actually see"
above for what these gaps mean concretely to someone downloading the app
today. Three things, none of which exist in this environment:

1. **A Developer ID Application certificate**, from an enrolled Apple
   Developer Program account (US$99/yr). Once you have one:
   ```sh
   packaging/build-bundle.sh --sign-identity "Developer ID Application: Your Name (TEAMID)"
   ```
   `security find-identity -v -p codesigning` lists installed identities
   once the certificate is in your keychain. This is also what fixes the
   CDHash-instability problem above — a Developer ID signature carries a
   stable Team ID that TCC can key grants to across rebuilds, instead of a
   per-build content hash.

2. **Notarization**, via `xcrun notarytool submit ... --wait` followed by
   `xcrun stapler staple "Textify Voice.app"`, using an app-specific password
   or API key tied to that same Developer account. This is what turns the
   `spctl --assess: rejected` result below into an accepted one for a
   downloaded copy — Gatekeeper checks Apple's notarization ticket, not just
   the presence of a signature.

3. **R2 credentials**, for the model-hosting migration described under
   "Model hosting" above. Not a distribution blocker the way the first two
   are (the HuggingFace fallback works today), but it's the same shape of
   gap: a real dependency this environment cannot provide, documented with
   the exact steps rather than skipped silently.

All three require credentials that don't exist on this machine; this unit
stops at ad-hoc/self signing and documents each gap rather than faking any
of them.

## Verification performed

All of the following was actually executed on this machine (not inferred),
by `packaging/build-bundle.sh` and follow-up checks:

```
$ packaging/build-bundle.sh
==> cargo build --release -p voice-cli
    Finished `release` profile [optimized] target(s) in 0.43s
==> assembling .../packaging/dist/Textify Voice.app
==> plutil -lint
.../Textify Voice.app/Contents/Info.plist: OK
==> codesign --sign -
.../Textify Voice.app: replacing existing signature

==> codesign -dv --verbose=4
Executable=.../Textify Voice.app/Contents/MacOS/textify-voice
Identifier=com.textify.voice
Format=app bundle with Mach-O thin (arm64)
CodeDirectory v=20400 size=11530 flags=0x2(adhoc) hashes=354+3 location=embedded
CDHash=b1d2559742a02582a242cab825315c57c0887f1b
Signature=adhoc
Info.plist entries=14
TeamIdentifier=not set

==> codesign --verify --deep --strict
.../Textify Voice.app: valid on disk
.../Textify Voice.app: satisfies its Designated Requirement
codesign --verify: OK

==> spctl --assess --type execute
.../Textify Voice.app: rejected
spctl: REJECTED -- expected for an ad-hoc-signed, unnotarized app.
```

`plutil -p` on the produced `Info.plist`:

```
{
  "CFBundleDisplayName" => "Textify Voice"
  "CFBundleExecutable" => "textify-voice"
  "CFBundleIdentifier" => "com.textify.voice"
  "CFBundleInfoDictionaryVersion" => "6.0"
  "CFBundleName" => "Textify Voice"
  "CFBundlePackageType" => "APPL"
  "CFBundleShortVersionString" => "0.1.0"
  "CFBundleVersion" => "9ef9fdf"
  "LSApplicationCategoryType" => "public.app-category.productivity"
  "LSMinimumSystemVersion" => "13.0"
  "LSUIElement" => true
  "NSHighResolutionCapable" => true
  "NSHumanReadableCopyright" => "Copyright (c) 2026 Textify. All rights reserved."
  "NSMicrophoneUsageDescription" => "Textify Voice listens while you hold your dictation key so it can turn your speech into text and paste it wherever your cursor is. Your audio is transcribed on this Mac and is never sent anywhere else."
}
```

CLI face unchanged, run directly from the bundle:

```
$ "Textify Voice.app/Contents/MacOS/textify-voice" --version
textify-voice 0.1.0        # exit 0

$ "Textify Voice.app/Contents/MacOS/textify-voice" models --help
Manage the local whisper.cpp model cache (list / download / show path)
...                          # exit 0, identical to the unbundled binary
```

The zero-argument contract, as it behaved when this bundle work was done —
kept because it documents *why* the dispatch lives ahead of `Cli::parse()`:

```
$ "Textify Voice.app/Contents/MacOS/textify-voice"     # before the agent entry point
Usage: textify-voice [OPTIONS] <COMMAND>
...
exit=2
```

The agent entry point has since landed, so zero-arg launch now enters the
menu-bar agent loop instead of erroring.

Launched for real via `open` (the real LaunchServices path, not a simulation)
and confirmed via the unified log that LaunchServices actually spawned the
process from inside the bundle:

```
$ open "Textify Voice.app"; echo "open exit=$?"
open exit=0

$ log show --last 2m --predicate 'process == "textify-voice"' --style compact
Timestamp               Ty Process[PID:TID]
2026-08-18 12:04:14.646 A  textify-voice[52539:...] (libsystem_info.dylib) Retrieve User by ID
2026-08-18 12:04:14.659 A  textify-voice[52540:...] (libsystem_info.dylib) Retrieve User by ID
2026-08-18 12:04:14.670 A  textify-voice[52543:...] (libsystem_info.dylib) Retrieve User by ID
```

Three real PIDs launched and gone within about a second of `ps aux` sampling
— consistent with hitting the zero-arg clap error above and exiting
immediately, which is what happened before the agent loop existed. No crash report was
written to `~/Library/Logs/DiagnosticReports/`, i.e. this is a clean process
exit, not a crash.

**What I could not verify** (explicitly out of reach in this environment):
I cannot complete a TCC consent dialog (no interactive session can click
"Allow"), so I cannot show the app actually appearing under System Settings
→ Privacy & Security → Microphone/Accessibility, and I cannot observe
whether an *existing* grant survives a rebuild versus needing to be re-done
— that would require a real consent flow completed once, then a second
build compared against it. I also cannot notarize (no Apple Developer
credentials exist in this environment) or confirm how Gatekeeper treats a
*notarized* copy, only that an unnotarized ad-hoc one is rejected as
documented above.

## Verification performed: licenses, model hosting, build provenance

Everything below was actually executed on this machine on 2026-08-18, for
the bundle-completeness work (licenses, configurable/verified model
hosting, `BuildInfo.txt`).

**License bundle, built with `cargo license --filter-platform
aarch64-apple-darwin` (real tool run, not a hand-kept list):**

```
$ cargo license --avoid-dev-deps --avoid-build-deps \
    --filter-platform aarch64-apple-darwin -j | ... (grouped by license)
third-party packages actually compiled for aarch64-apple-darwin release: 102
   1  (Apache-2.0 OR MIT) AND Unicode-3.0
   1  0BSD OR Apache-2.0 OR MIT
   2  Apache-2.0
   1  Apache-2.0 AND ISC
   1  Apache-2.0 OR BSD-2-Clause OR MIT
   1  Apache-2.0 OR ISC OR MIT
  53  Apache-2.0 OR MIT
  24  Apache-2.0 OR MIT OR Zlib
   1  BSD-3-Clause
   1  CDLA-Permissive-2.0
   2  ISC
  10  MIT
   1  MIT OR Unlicense
   1  MPL-2.0                <-- the one non-permissive hit, see below
   2  Unlicense
```

The one non-permissive hit, confirmed to be an *actual compiled* dependency
(not just resolved-but-unused) by checking it landed in the real release
build's output:

```
$ cargo tree -p voice-cli -i option-ext --target aarch64-apple-darwin
option-ext v0.2.0
└── dirs-sys v0.5.0
    └── dirs v6.0.0
        ├── voice-asr-whisper v0.1.0 (...)
        │   └── voice-cli v0.1.0 (...)
        └── voice-cli v0.1.0 (...)

$ find target/release -iname "*option-ext*" -o -iname "*option_ext*"
target/release/deps/liboption_ext-27bd8fe35f447f69.rlib
target/release/deps/liboption_ext-27bd8fe35f447f69.rmeta
... (fingerprint files)
```

No GPL/LGPL/AGPL/SSPL dependency was found anywhere in either the full
(unfiltered, 195-package) or platform-filtered (102-package) dependency
graph.

**Built bundle actually contains the license notices, sealed into the code
signature (not just copied and forgotten):**

```
$ packaging/build-bundle.sh --no-build --sign-identity "-"
...
==> copying licenses to .../Textify Voice.app/Contents/Resources/Licenses
...
==> codesign -dv --verbose=4
...
Sealed Resources version=2 rules=13 files=150
...
==> codesign --verify --deep --strict
.../Textify Voice.app: valid on disk
.../Textify Voice.app: satisfies its Designated Requirement
codesign --verify: OK

$ ls "Textify Voice.app/Contents/Resources/Licenses/"
MODELS-NOTICE.md  README.md  spdx-texts  THIRD-PARTY-NOTICES.txt  vendor  whisper.cpp

$ ls "Textify Voice.app/Contents/Resources/Licenses/vendor/" | wc -l
      73
$ grep -A2 "^License: MPL-2.0" ".../Licenses/THIRD-PARTY-NOTICES.txt"
License: MPL-2.0
Authors: Simon Ochsenreither <simon@ochsenreither.de>
Repository: https://github.com/soc/option-ext.git
```

**`BuildInfo.txt`, written by this build (note the correct `-dirty` suffix
— this working tree genuinely had uncommitted changes, from a different
unit's concurrent work, at build time):**

```
$ cat "Textify Voice.app/Contents/Resources/BuildInfo.txt"
Textify Voice build info
=========================
Version:         0.1.0
Build:           52c916a-dirty
Git SHA:         52c916a
Working tree:    dirty (uncommitted changes present at build time)
Built (UTC):     2026-08-18T13:42:48Z
Sign identity:   -
Bundle ID:       com.textify.voice
```

**Configurable model base URL + SHA-256 fail-closed, demonstrated against
the real built `textify-voice models` command** (not just the crate's unit
tests, which also cover this) — a local HTTP server stood in for R2,
serving a file of the *correct expected size* (70,000,000 bytes, matching
`ModelId::TinyEn`'s size range) but *wrong content* (`0xAB` repeated), the
exact "R2 upload got corrupted, or a bucket got compromised" scenario the
SHA-256 pin exists for:

```
$ TEXTIFY_MODEL_BASE_URL="http://127.0.0.1:8991" TEXTIFY_WHISPER_MODEL_DIR=<tmp> \
    "Textify Voice.app/Contents/MacOS/textify-voice" models
  ggml-tiny.en.bin     not cached   expected size 70-80 MB   http://127.0.0.1:8991/ggml-tiny.en.bin
  ggml-base.en.bin     not cached   expected size 140-156 MB   http://127.0.0.1:8991/ggml-base.en.bin

$ TEXTIFY_MODEL_BASE_URL="http://127.0.0.1:8991" TEXTIFY_WHISPER_MODEL_DIR=<tmp> \
    "Textify Voice.app/Contents/MacOS/textify-voice" models --download tiny.en
downloading ggml-tiny.en.bin from http://127.0.0.1:8991/ggml-tiny.en.bin ...
  100%  (70000000/70000000 bytes)Error: downloading ggml-tiny.en.bin: downloaded model
SHA-256 mismatch: expected 921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f,
got 49037514064ba3573d30ef511fcdd3d09f29fb5b5a2494c70ab20eec84d852c4; refusing to use this
file (it was deleted). This means either a corrupted download or a substituted file -- do
not retry blindly, verify the source.
exit=1

$ ls -la <tmp>          # cache dir after the failure
total 0                 # the bad file was NOT left on disk -- fails closed
```

The full 70 MB download completed (progress reached 100%) before the
SHA-256 check ran and rejected it — proving this isn't a short-circuit on
size alone, and that the check runs against the complete downloaded bytes.

**The two SHA-256 pins themselves were cross-verified two independent
ways**, not asserted from memory:

```
$ shasum -a 256 ~/Library/Application\ Support/textify/models/ggml-*.bin
921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f  ggml-tiny.en.bin
a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002  ggml-base.en.bin

$ curl -s "https://huggingface.co/api/models/ggerganov/whisper.cpp/tree/main" | \
    jq '.[] | select(.path=="ggml-tiny.en.bin" or .path=="ggml-base.en.bin") | .lfs.oid'
"921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"
"a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
```

Local `shasum` and HuggingFace's own published git-lfs object ID (itself
the SHA-256 of the file's content) agreed exactly for both models.

**Full workspace gates, re-run after all of the above** (`--test-threads=1`
to avoid an unrelated, pre-existing real-NSPasteboard test flake in
`voice-cli::clipboard` that only reproduces under parallel test execution —
confirmed pre-existing and unrelated to this unit's changes, not something
touched by this unit's files):

```
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.27s
(zero warnings)

$ cargo test --workspace -- --test-threads=1
... 9 test binaries, sum: 498 passed, 0 failed, 1 ignored
```

**What could not be verified for this sub-unit:** signing with the stable
self-signed `"Textify Voice Dev"` identity (the identity `make-dev-cert.sh`
sets up, used for day-to-day TCC-grant-stable development per "What ad-hoc
signing does and does not give you" above) hung indefinitely in this
session: `codesign --sign "Textify Voice Dev"` blocked past "replacing
existing signature" for 5+ minutes with zero further output, and a live
`ps aux` during the hang showed `SecurityAgent` running concurrently with
the blocked `codesign` process — `SecurityAgent` is macOS's own process for
presenting Keychain access-control authorization dialogs, so this is
genuinely a private-key-access GUI prompt this sandboxed, non-interactive
session cannot answer (the same class of "cannot complete a dialog"
limitation already documented above for TCC). All verification above was
therefore performed with `--sign-identity "-"` (ad-hoc) instead, which
signs without touching the Keychain at all and is unaffected — the codesign
output above (`Sealed Resources ... files=150`, `codesign --verify: OK`)
confirms the bundle assembly and license-copy mechanics work correctly
regardless of which identity ultimately signs it; only the "Textify Voice
Dev"-specific signing step itself could not be exercised in this session.
