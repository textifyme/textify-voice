# textify-voice

The MVP CLI **and** the menu-bar app. This is one binary — `target/release/
textify-voice` — that turns the tested-but-stubbed `voice-*` crates into
something the founder can actually run: real audio in, real local ASR
(whisper.cpp via Metal on Apple Silicon), real text out. Launched with a
subcommand it's a CLI; launched with zero arguments (which is exactly how
`Textify Voice.app` launches it — see
[Running as the app](#running-as-the-app-menu-bar-agent) below) it's a
persistent menu-bar agent.

Core subcommands. Two are fully real and fully verifiable today
(`transcribe`, `command`). One (`models`) is real infrastructure with no
permission gate. One (`dictate`) is real code that, on this particular
development machine, has both Microphone and Accessibility already granted
**to the terminal** (see
[What changes about permissions](#what-changes-about-permissions) below for
why that's the wrong long-term shape) — which meant this session could
actually watch its live loop reach "ready" for real, not just compile; see
the `dictate` section below for exactly what that run showed and what it
still doesn't prove (nobody spoke into the mic). `bench record`/
`bench score`, `settings`, and `onboarding` are documented in their own
sections below. This build also wires an auto-update mechanism, local crash
reporting, and a platform-compatibility gate — see
[Auto-updates](#auto-updates), [Diagnostics and crash reporting](#diagnostics-and-crash-reporting),
and [Platform floor and device compatibility](#platform-floor-and-device-compatibility)
below.

## Build

```
cargo build -p voice-cli --release
```

First build compiles `whisper.cpp` via `whisper-rs`/`cmake` with Metal
acceleration on Apple Silicon — this can take a few minutes the first time
(and every time `whisper-rs-sys`'s C/C++ side has to rebuild from a clean
target dir). Subsequent builds are fast.

Requires: Rust 1.86+ (this crate's dependency tree, notably `arboard` with
default features off and `whisper-rs` pinned to `0.15.1`, was specifically
chosen to build clean under 1.86 — see `voice-asr-whisper/Cargo.toml`'s
comment on why `0.16.0` doesn't). macOS on Apple Silicon is the only
platform this has actually been built and run on; the `dictate` live loop is
explicitly macOS-only in this MVP (see below), the rest of the binary is not
macOS-specific in principle but has not been tested on Linux/Windows/Intel
Mac.

The binary is `target/release/textify-voice`.

## Get the model

```
target/release/textify-voice models
target/release/textify-voice models --download base.en   # ~148 MB, the default
target/release/textify-voice models --download tiny.en   # ~75 MB, faster/less accurate
target/release/textify-voice models --path                # print the cache dir
```

Models download from the canonical `ggerganov/whisper.cpp` HuggingFace repo
and cache under `~/Library/Application Support/textify/models/` (override
with `TEXTIFY_WHISPER_MODEL_DIR`). `transcribe` and `dictate` both
download-on-first-use automatically if you skip this step — `models
--download` just lets you do it up front and see progress.

## User dictionary

`dictate` (always) and `transcribe` (unless `--no-dictionary`) load your own
proper nouns, jargon, and custom substitutions into bias layer 2 from a
plain text file you maintain yourself:

```
~/Library/Application Support/textify/dictionary.txt
```

Override the path with `TEXTIFY_VOICE_DICTIONARY_PATH`. If the file does
not exist yet, it is created on first run with a commented starter example
(`Kubernetes` and a `cursor dot ai => cursor.ai` line) — this is meant to
be discoverable, not something you have to already know to go create by
hand. `dictate` has no dictionary opt-out flag; `transcribe --no-dictionary`
skips loading it for one run.

Format, one entry per line:

```
# Lines starting with '#' are comments; blank lines are ignored.

Kubernetes
Alishah

# "spoken form => written form": literal, case-insensitive substitution --
# the same mechanism the built-in "cursor dot ai" -> "cursor.ai" rule uses.
cursor dot ai => cursor.ai
textify voice => Textify Voice
```

- A **plain line** becomes one bias-layer-2 term: dictation phonetically
  close to it gets corrected *toward* it. A multi-word line like "Onetelos
  Textify" is one term, not split into two.
- A **`spoken => written`** line becomes a literal, deterministic
  substitution (split on the first `=>` only) — applied before bias layer
  2 sees the words, at confidence 1.0, so layer 2 never reconsiders it.
- Malformed lines (empty spoken/written half, a stray second `=>`) are
  reported with their 1-indexed line number rather than silently dropped or
  silently misapplied; printed as `dictionary warning: ...` at startup —
  it does not stop `dictate`/`transcribe` from running with whatever did
  parse.
- Loaded **once** at startup, not per-utterance — a file read must not sit
  on the "never blocks the first audio frame" path.

## `transcribe` — fully verifiable, no permissions needed

```
textify-voice transcribe fixtures/audio/short-5s.wav
textify-voice transcribe path/to/clip.wav --bias-terms "Kubernetes,Nginx" --app-kind code --verbose
```

Real pipeline, every stage: `voice-audio` decodes the WAV (any sample rate /
channel count / integer-or-float PCM depth, resampled to 16 kHz mono) ->
`voice-asr-whisper` runs the actual whisper.cpp batch decode -> `voice-core`
runs bias-layer-2 phonetic correction + the deterministic normalizer
(app-kind-aware: `--app-kind code|ai|terminal` forces raw output, matching
SPEC.md's "AI/coding apps get raw paste" rule) -> the result prints to
stdout. `--verbose` prints per-stage timings to stderr; every run (verbose
or not) prints a `speech-end-to-text:` line.

### Actually run, actual output (this session)

```
$ textify-voice transcribe fixtures/audio/short-5s.wav
The Morning Report shows every server is running without any problems today.
speech-end-to-text: 4279.5 ms (asr 4279.5 ms + normalize 0.0 ms, over 3.88s of audio)
```

Reference: *"The morning report shows every server is running without any
problems today."* — 12/12 words correct; the only difference is whisper's
own capitalization of "Morning Report," not a transcription error. (First
run after a cold model load; a warm process decodes this same 3.88 s clip in
~260 ms once the model and Metal are already initialized — see the
`--bias-terms` example run below.)

```
$ textify-voice transcribe fixtures/audio/ref-3min.wav --verbose
Most of us spend a large part of the day in front of a screen, and yet we
rarely stop to think about how we keep our work in order. [...] The first
rabbits you build around them.

-- stage timings (--verbose) --
  model load/download :       0.1 ms
  decode (capture)    :      22.5 ms
  asr (whisper)       :    1467.6 ms
  normalize            :      0.1 ms
  audio duration       :    121.36 s
  detected_lang        : en
  bias-layer-2 corrections applied: 0
  words (per_word_conf): 317
speech-end-to-text: 1467.7 ms (asr 1467.6 ms + normalize 0.1 ms, over 121.36s of audio)
```

~121 s of real audio decoded in ~1.5 s wall time on an Apple M1 Max via
Metal — roughly 80x real-time. ("The first rabbits you build around them" at
the very end is whisper's own mis-hearing of "the habits you build around
them" — a genuine ASR imperfection on a run-on sentence, reported honestly,
not edited out.)

```
$ textify-voice transcribe fixtures/audio/short-5s.wav --model tiny.en
The morning report shows every server is running without any problems today.
```

`tiny.en` got this particular clip's capitalization exactly right where
`base.en` didn't — not a general claim that tiny beats base, just what
actually happened on this file.

Also verified: a nonexistent file produces a clean `Error: decoding ...: No
such file or directory` on stderr and exit code 1, not a panic.

### Long-form audio: chunking is on by default above 60s

`WhisperLocalAsr::finalize()` decides for itself, per call, whether to
single-shot decode or switch to windowed decode-and-stitch
(`ChunkingConfig`, `voice-asr-whisper`) — this is not a CLI flag. Below
`threshold_seconds` (default **60.0s**) it runs the same single-shot decode
this backend always has; at or above it, it transparently switches to a
10s-window / 1.5s-overlap chunked decode.

The threshold isn't arbitrary: whisper.cpp's own long-form segment-seek was
measured (against `fixtures/audio/ref-3min.wav`, length-swept in 5-10s
increments and scored with `fixtures/voice/wer.ts`) to silently *drop*
large contiguous spans of words once a single `whisper_full()` call runs
long enough — clean through ~100s, then a WER jump from ~0.04 to 0.14+ by
105s, worsening to 0.25 (101 deletions out of 418 reference words) at the
full 121s clip. Below ~90s, chunked and single-shot are statistically
identical — chunking there is pure overhead, not a quality win — which is
why the default only turns it on well clear of the measured failure onset,
not unconditionally. That sweep (and the module doc's "chunking stays flat"
figures) was run with an empty bias context, `--no-dictionary`-equivalent —
see `voice-asr-whisper/src/whisper_asr.rs`'s module doc for the full numbers,
and read the next paragraph before assuming they hold on the command you're
about to run.

**Measured on the shipped default path** (release binary, no flags, the
starter dictionary present) against `fixtures/audio/ref-3min.wav`, whose
human reference is 418 words:

| Command (default arguments unless stated) | WER |
|---|---|
| `transcribe ref-3min.wav` | **0.0526** |
| `transcribe ref-3min.wav --no-chunking` | 0.2488 |

There is history worth keeping here. An earlier build fed dictionary terms
into whisper.cpp's `initial_prompt`, and that measured **0.3947** on this
same fixture — one starter term ("Kubernetes") deleted 121 of 418 words, with
whole 10-second windows collapsing to the single word "The". Prompt
conditioning was removed rather than tuned: SPEC §3.3 places decode-time bias
in layer 1, which is *transducer-only*, and says whisper-class engines "rely
on layers 2–3". Bias now runs entirely through layer 2's phonetic
post-correction, which is engine-agnostic and demonstrably works.

The lesson that cost three waves: **every number here must be reproduced with
genuinely default arguments.** The 0.0526 figure was previously quoted from
runs that passed `--no-dictionary`, so the shipped default was never actually
measured.

**Push-to-talk utterances bypass this by construction.** `dictate`'s
hold-to-talk clips are seconds long, far under the 60s threshold, so they
always take the single-shot path — chunking only engages for long
`transcribe` input files (or the explicit `transcribe_long_form` API), not
for anything dictated live.

### Real-model integration tests: soft-skip by default

`crates/voice-asr-whisper/tests/fixture_transcription.rs` has two tests
that run a real whisper.cpp decode against the real fixture clips (the
source of the WER numbers a few paragraphs up) — but both check
`TEXTIFY_VOICE_ASR_WHISPER_RUN_MODEL_TESTS` first and, if it isn't set to
`1`, print a skip notice to stderr and return `Ok` immediately, without
downloading a model or decoding anything. **The standard `cargo test
--workspace` gate does not set this variable**, so its passing count
includes these two tests as a silent no-op, not a real check — `cargo
test -p voice-asr-whisper --test fixture_transcription` alone shows this
plainly (verified this session):

```
running 2 tests
test transcribes_short_5s_fixture_against_reference ... ok
test finalize_auto_chunks_long_audio_by_default_and_disabling_it_reproduces_the_known_drop ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`finished in 0.00s` for two tests that each load a whisper model and
decode real audio is the tell. To actually run them (also verified this
session — real model, real decode, 8.64s):

```
TEXTIFY_VOICE_ASR_WHISPER_RUN_MODEL_TESTS=1 \
    cargo test -p voice-asr-whisper --test fixture_transcription -- --nocapture
```

```
test finalize_auto_chunks_long_audio_by_default_and_disabling_it_reproduces_the_known_drop ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.64s
```

Both tests construct `BiasContext::empty(AppKind::General)` directly
through the library's `LocalAsr` trait — no dictionary, no CLI, no
`initial_prompt` bias content — so even run for real with the env var set,
neither one exercises the shipped-default (dictionary-loaded) path the
WER table above measured through the actual release binary. That gap —
every automated ASR measurement in this codebase using either
`--no-dictionary` or this env-gated library harness, never the CLI's
actual default arguments — is exactly why the chunking regression above
had to be found by re-running the release binary by hand rather than by
the test suite going red.

## `command` — dry-run demo of the Command Mode spine, never touches the OS

```
textify-voice command "open Slack" --apps Slack,Chrome
textify-voice command "click Delete" --labels Delete,Cancel
textify-voice command "open door policy helps morale" --apps Slack
```

Runs the real `voice-intent` grammar matcher against a `CommandContext` built
from `--apps`/`--labels`/`--shortcuts`, then the real `voice-act`
`MockDesktopExecutor::resolve()` (tier escalation included — a label that
binds to a destructive word like "Delete" genuinely escalates to T2), then
the real `gate::decide` — and prints the schema, bound target, effective
tier, and gate decision. It is architecturally incapable of executing
anything: no `Authorized` token (the only thing `ActionExecutor::execute`
accepts) is ever minted in this command.

### Actually run, actual output (this session)

```
$ textify-voice command "open Slack" --apps Slack,Chrome
stage 1 (grammar) : MATCHED
  schema_id  : app.open
resolve()  : BOUND
  target     : id=Some("app-0") label=Some("Slack") secure=false
  effective tier: T1
gate decision: EXECUTE_AND_ANNOUNCE (T1 -- disruptive-but-recoverable)

$ textify-voice command "click Delete" --labels Delete,Cancel
stage 1 (grammar) : MATCHED
  schema_id  : ui.click
resolve()  : BOUND
  target     : id=Some("el-0") label=Some("Delete") secure=false
  effective tier: T2
gate decision: REQUIRE_CONFIRM (T2 -- consequential)
  a real run would show a HUD confirm prompt ("say yes / no") and default-deny after 5s...

$ textify-voice command "open door policy helps morale" --apps Slack
stage 1 (grammar) : REJECTED -- reason = not-a-command
result: REJECTED. Nothing was matched, nothing was resolved, nothing was executed.
```

Exactly the three behaviors this command exists to demonstrate: an ordinary
app-open resolves at T1, a destructive-labeled click escalates to T2 and
requires confirmation, and dictation-lookalike prose that merely *contains*
"open" correctly rejects rather than firing on a coincidental verb.

## `dictate` — the real product loop

```
textify-voice dictate                          # hold left Option, speak, release
textify-voice dictate --paste                  # ...and auto-⌘V into the focused field
textify-voice dictate --hold-key right-option  # pick a different modifier
textify-voice dictate --no-hud                 # no floating waveform
textify-voice dictate --no-sound               # no press/release tones
```

**Hold a bare modifier — left Option by default.** Press and hold, a small
waveform panel appears above the Dock and moves with your voice, release and
the text lands where you were typing.

`global-hotkey` cannot express a lone modifier (it parses "modifiers + one
key"), so this runs on a dedicated `CGEventTap` watching `flagsChanged`
(`crate::holdkey`). Three properties of that tap matter:

- It is **`ListenOnly`** — it observes, never swallows. Option keeps working
  as a typing modifier system-wide.
- **Pressing another key while the modifier is held cancels the utterance.**
  `Option+e` is how you type `´`, not a 200 ms recording. A second modifier
  joining (`⌘⌥…`) cancels for the same reason.
- The OS silently disables a tap that responds slowly. That is surfaced and
  re-armed rather than leaving the hold key mysteriously dead.

A short rising tone marks the press and a falling one the release. The ear
confirms the press faster than the eye can, and you are looking at another
window when you press; the direction of the sweep is what makes "started" and
"stopped" distinguishable without looking. They are synthesized at startup
(sine sweeps with a soft attack and exponential decay, rendered to an in-memory
WAV for `NSSound`) — no audio assets in the repo. Quiet by design, peaking
around 0.15 full scale. `--no-sound` disables them.

The waveform is driven by **real RMS from the capture callback**, not a canned
animation — if the bars do not move, audio genuinely is not arriving. The panel
is a `NonactivatingPanel` under an `Accessory` activation policy and is shown
with `orderFrontRegardless`, so it can never take key focus; if it could, the
synthesized ⌘V would land in the panel instead of your text field. Pass
`--no-hud` to run headless.

Captured audio goes through the same real whisper.cpp `WhisperLocalAsr` that
`transcribe` uses, then the same real normalizer, then `voice-core`'s real
`insert_text` policy: clipboard by default, plus a synthesized ⌘V if you pass
`--paste`. **The clipboard is always written first**, so even if the paste
fails the text is still on your clipboard.

Threading: the main thread owns AppKit and the event tap; whisper runs on a
worker thread. That is not incidental — whisper blocks for a few hundred ms,
which would freeze the waveform and starve the tap badly enough for the OS to
disable it.

### What this command needs, and why it refuses to half-run

`dictate` checks two macOS permissions **before** registering the hotkey or
touching the microphone, and refuses (exit code 1, nothing armed) if either
is missing, rather than silently doing nothing when you later press the key:

| Permission | Why | Grant it here |
|---|---|---|
| **Microphone** | `MicCapture` opens a real `cpal` input stream | System Settings → Privacy & Security → Microphone |
| **Accessibility** | the hold-key `CGEventTap`, and `--paste`'s synthesized ⌘V | System Settings → Privacy & Security → Accessibility |

(If the hold key is granted-but-still-never-fires, also check System
Settings → Privacy & Security → Input Monitoring for the same app — some
macOS versions gate event-tap *listening* there even when Accessibility
covers *posting*.)

### Actually run, actual output (this session)

Earlier development sessions on this machine only had Microphone granted to
the terminal, not Accessibility, so `dictate` correctly refused at the
permission gate (exit 1) without ever reaching the live loop — that
refusal-path run is preserved below since it's still the correct behavior
to expect on a fresh machine. **This session's machine has since had both
permissions granted to the terminal** (a fact stated in this unit's own
dispatch, and visible below), which meant `dictate` could be run for real
and its live loop actually reached:

```
$ textify-voice dictate
textify-voice dictate -- mode=Ptt  paste=false

Checking permissions...
  [OK] Microphone            : Authorized
  [OK] Accessibility         : granted

dictionary: 1 term(s), 1 literal rule(s) loaded from ~/Library/Application Support/textify/dictionary.txt
[... whisper.cpp/Metal model-load log ...]
mic: EarPods Microphone @ 44100 Hz / 1 ch (resampled to 16 kHz mono for ASR)
hold left Option (⌥) to talk (Ptt mode), clipboard only
Ctrl-C to quit.
```

Left running, then stopped with `SIGINT` (Ctrl-C) after confirming it was
alive and steady: exited cleanly, exit code reported `0`, no crash report
written to `~/Library/Logs/DiagnosticReports/`. That's real: a real
`cpal` input stream opened against a real device (`EarPods Microphone`), a
real whisper.cpp/Metal model loaded, and the ~60 Hz `CFRunLoop` pump held
steady with no hold-key event ever delivered (nothing pressed the key).

**What this still does not prove**: nobody held the key and spoke into the
mic in this session. The refusal-path run below (correct on a machine where
Accessibility genuinely isn't granted) is preserved for reference; the
capture → ASR → insert path — the part that needs a human to actually press
a key — remains unexercised by any automated session. See
[Known gaps](#known-gaps-in-dictate) below.

The historical refusal-path run, from before Accessibility was granted:

```
$ textify-voice dictate
textify-voice dictate -- mode=Ptt  paste=false

Checking permissions...
  [OK] Microphone            : Authorized
  [MISSING] Accessibility         : not granted
        -> Open System Settings > Privacy & Security > Accessibility, enable access for
           this terminal/app (the app you launched textify-voice from), then relaunch. ...

Error: one or more required permissions are missing (see above). Grant them in System
Settings, then re-run `textify-voice dictate`. ...
```
Exit code: `1`.

### To actually test dictate for real

1. `cargo build -p voice-cli --release`
2. Grant the terminal/app you'll run `textify-voice` from both permissions
   above (System Settings → Privacy & Security → Microphone /
   Accessibility) — already true on this machine, per above.
3. `./target/release/textify-voice dictate` — you should see a `mic: ...`
   line and `hold left Option (⌥) to talk` instead of the permission error
   (confirmed above).
4. Hold left Option, say something, release it. The waveform panel should
   appear and move while you talk, turn amber while transcribing, then vanish,
   and you should see `> <your transcript>` and a `[copied to clipboard]` line.
   If the panel appears but the bars stay flat, audio is not reaching the
   callback — that is a Microphone problem, not an ASR problem.
5. Paste (⌘V) into any text field to confirm the clipboard actually has it.
   Add `--paste` to skip that manual step (requires the same Accessibility
   grant, already covered by step 2).

**Step 4 specifically was not done in this session** — no automated agent
session can hold a physical key down and speak. Steps 1–3 (build, permission
grant, reaching the live "ready" state) were genuinely executed above, on
this real machine, not simulated. Don't take the rest on faith — run step 4
yourself.

### Known gaps in `dictate`

- **The live loop reaches "ready" for real on this machine now; nobody has
  spoken into it.** Earlier revisions of this README said the whole live
  path had "never executed on real hardware" — that was true when both
  permissions weren't granted to this session's terminal. They now are (see
  above), and `dictate` genuinely opens the mic, loads the whisper model,
  installs the hold-key tap, and idles in its `CFRunLoop` pump waiting for a
  press — confirmed by an actual run, stopped cleanly with `SIGINT`, no
  crash. What remains genuinely unproven: no automated session can hold a
  physical key and speak, so the capture → ASR → normalize → insert path
  past "ready" has never fired end to end. Treat that specific path as
  unproven until a human runs it.
- **App-kind raw paste is real for terminal/code/AI apps. Verbatim de-formatting (stripping whisper's own capitalization and trailing punctuation) applies to Terminal and Code only — AI chat apps are prose and keep their capitals. "Verbatim" now
  also undoes whisper's own casing/punctuation.** The frontmost app's kind —
  from the same live AX/`NSWorkspace` read that feeds secure-field
  detection — drives `voice_core::normalizer::normalize()`: for
  `AppKind::Terminal`, `Code`, and `Ai` (`is_ai_or_coding()`), literal-rule
  substitution and bias layer 2 are skipped entirely, and the words go
  through `verbatim_words()` before being joined with a single space
  (`join_raw`) — unit-tested in `voice-core::normalizer`. whisper.cpp itself
  decodes with `punctuation: true` and its own trained sentence-initial
  capitalization (visible in this README's own `transcribe` example above,
  where whisper capitalized "Morning Report" on its own, not via any of our
  code); simply skipping *our* transforms was not enough on its own to make
  `git status` come out as `git status` rather than `Git status.`.
  `verbatim_words()` closes that gap: it strips a single ASR-added trailing
  sentence-final punctuation mark from the last word (leaving genuine
  content like `cd ..`'s trailing `..` alone — no alphanumeric character
  precedes the final `.`), and de-capitalizes a leading capital on the first
  word unless it's the pronoun `I` or a bias/dictionary term (so `Docker
  compose up.` still keeps its capital `D`). The normalizer's own tests now
  feed whisper-shaped tokens — capitalized, punctuation glued onto the last
  word (`WordSpan::new("Git", ...)`, `WordSpan::new("status.", ...)`), not
  pre-cleaned lowercase input — covering `git status`, `ls -la`, `cd ..`,
  `npm run dev`, `cargo test --workspace`, and the dictionary-term-keeps-its-
  capital case. Whether this generalizes past what those unit tests cover
  (real, live dictated shell commands into a real terminal) is still
  unexercised end to end on real hardware — see the "Never actually run"
  gap above — but the codebase-level defect ("every raw-paste test fed
  already-lowercase words") is fixed, not open.
- **Toggle mode is manual, not VAD-auto-ended.** `voice-audio` ships a real
  energy-VAD-driven auto-endpoint (`ToggleCapturePipeline`, wired to
  `voice-core`'s `Endpointer`); wiring its polling into the main loop is a
  follow-up. Toggle today is strictly tap-to-start / tap-to-stop.
- **The waveform is a level meter, not a spectrum.** Bars scroll right-to-left
  showing recent RMS. It answers "is it hearing me", which is the question
  that matters; it is not a real-time spectrogram.
- **`fn` as a hold key may not work.** macOS claims the globe/fn key for its
  own dictation and input switching on many configurations.
- **Secure-field detection is real but has never been proven against a live
  password field.** `CliInsertionBackend::current_target()` now performs a
  genuine `voice_context::MacosContextProvider` AX read — bounded by
  `DEFAULT_AX_TIMEOUT` (~300ms) — immediately before every insertion
  decision, and reports `is_secure_field: true` when the focused element's
  `AXSubrole` is `AXSecureTextField`. That subrole mapping is unit-tested
  (`voice-context/src/macos/mod.rs`), but only against a synthetic
  `RawFocusedElement` constructed in the test itself — no automated run in
  this codebase has ever focused a real `AXSecureTextField` in a running
  app and captured its AX state live. Treat the *policy* (refuse to insert
  into a secure field) as real, wired, and tested at the unit level; treat
  "it will always catch a real password field on this machine" as
  designed-but-unproven end to end. **The fail-open timeout edge is fixed.**
  `current_target()` no longer falls back to `pending.wait().unwrap_or(capture.snapshot)`
  on a timed-out AX read; it calls
  `voice_context::ContextCapture::wait_secure_field_status()`, which returns
  a tri-state `SecureFieldStatus` (`Known(bool)` / `Unknown`) instead of a
  bare `bool`, and `secure_status_to_target()` maps `Unknown` — every
  degraded case: timeout, missing Accessibility permission, no focused
  element — to `is_secure_field: true` (refuse), never to `false`. This is
  verified against the real, live `MacosContextProvider` (not a synthetic
  snapshot): `crates/voice-context/examples/probe_forced_timeout.rs` forces
  a 0ns AX-read budget against this machine's actual desktop and asserts
  the new logic never reproduces the old fail-open result. Run 10 times
  this session: 10/10 confirm the old-logic replica reproduces
  `is_secure_field = false` (the original bug) on the forced timeout, and
  10/10 confirm the new logic instead resolves `SecureFieldStatus::Unknown`
  on that same timeout, which `insert_text()` refuses to type into. **Do
  not rely on this to protect a password field** — the subrole mapping
  itself is still only unit-tested
  against a synthetic `RawFocusedElement`, so "detects a real
  `AXSecureTextField`" remains unproven end to end even though the timeout
  fallback defect is closed.
- **AX-insert is never attempted — by design, not by gap.** `current_target()`
  deliberately still always reports `is_ax_writable: false`, even though the
  context provider it reads (`ActionableElement::writable`) has a real bit
  for this now. There is no live focused-`AXUIElement` *write* path anywhere
  in this codebase (`voice-context` only ever reads), so wiring the real
  writable bit through would route ordinary writable fields into `ax_insert()`
  and hit a hard `AxWriteFailed` error instead of the clipboard path that
  actually works today — see `dictate.rs`'s doc comment on
  `CliInsertionBackend`. Every insertion goes through clipboard, matching
  the spec's clipboard-first framing (PORTING.md §2.2), but `dictate` never
  even tries direct AX insertion today, even into an app where it would work.
- **Clipboard snapshot/restore closes the "clobbered your clipboard" gap,
  with one irreducible race.** `crate::clipboard::ClipboardGuard` snapshots
  *every* pasteboard type present before writing the transcript (not just
  text — a copied image or file reference round-trips byte-for-byte too),
  then after a ~150ms settle delay restores that snapshot only if the
  pasteboard's `changeCount` still matches what this guard itself last
  wrote. macOS exposes no "paste confirmed" signal, so this is a bounded
  heuristic delay plus an optimistic-concurrency guard, not a proof: a
  target app slow to read the pasteboard can still see its old clipboard
  restored out from under it before it pastes, and an active clipboard
  manager can make restores get skipped more than expected (leaving the
  transcript on the clipboard permanently instead of restoring the user's
  prior content — the accepted, safer failure mode of the two). See
  `crate::clipboard`'s module doc for the full accounting.
- **macOS only.** The live loop (`dictate`'s actual capture/hotkey/paste
  code) is behind `#[cfg(target_os = "macos")]` and returns a clear error on
  every other platform. `transcribe`, `command`, and `models` are not
  macOS-specific in their own logic, but have only been built/run on macOS
  in this session.
- **First-frame capture latency is unmeasured.** `MicCapture` is
  architecturally pre-warmed (built-and-paused before the hotkey can even
  fire, so `start()` on key-down is just `Stream::play()`), matching
  SPEC.md V1.1's <10 ms target, but the actual number has never been
  measured — that requires a live device and a timer around a real key-down,
  neither of which this session has.

## Running as the app (menu-bar agent)

`textify-voice` is one binary with two faces, decided by how many
arguments it's launched with — see `main.rs`'s module doc and
`packaging/README.md`'s "One binary, two faces" for the platform fact this
is built on (LaunchServices launches a double-clicked/Finder/Dock `.app`
with **zero** `argv`, a shape a human typing a subcommand never produces):

```
textify-voice                # zero args -> menu-bar agent (see below)
textify-voice dictate ...    # a subcommand -> exactly the CLI documented above
```

`Textify Voice.app` (see [Building the app bundle](#building-the-app-bundle)
below) ships the same binary unmodified, so launching the app *is* the
zero-arg case. Running the raw binary from a terminal with no arguments
hits the exact same code path.

### What the agent does, in order

1. **Onboarding, if not already `Ready`.** `crate::onboarding` is a pure
   function of live state (see that module's doc) — a completed funnel
   (Microphone + Accessibility granted, model downloaded, Welcome
   dismissed once) is skipped silently; anything short of that shows the
   same permissions-funnel wizard `textify-voice onboarding` opens
   manually (see [Onboarding](#onboarding) below).
2. **Stay resident even if permissions are still missing.** Unlike the
   terminal `dictate` command (which refuses to half-run and exits), the
   agent does **not** quit if Microphone/Accessibility still aren't both
   granted after onboarding (the user quit partway through, or declined
   and came back later). Quitting here would be worse than refusing:
   granting Accessibility means leaving the app, flipping a switch in
   System Settings, and coming back — an app that quit while you were
   away isn't there to come back to, which reads as a crash (the exact
   failure mode this behavior replaced). Instead it parks in the menu bar
   showing "Permissions needed", keeps polling, and arms itself the
   moment both grants land — no relaunch needed. The background update
   checker (item 5 below) keeps running throughout this wait, since
   checking for updates needs neither permission.
3. **Arm dictation and show the status item.** A menu-bar icon
   (`crates/voice-cli/src/menubar.rs`, an `NSStatusItem`) tracks the real
   state of the loop — Idle / Listening / Transcribing / Error — via a
   platform-agnostic `StatusUi` trait (`platform/mod.rs`), not a guess: the
   same `dictate.rs` code paths that drive the HUD panel
   (`hud.show_listening()`/`show_transcribing()`/`hide()`) push the matching
   `StatusUiState` to the menu bar in the same places. The menu also shows the active hold key,
   the current update status, a "Dictation Armed" checkbox (`ToggleArmed`
   pauses/resumes capture without quitting the app), "Settings…", "Check
   for Updates…", and "Quit".
4. **Pump the same ~60 Hz run loop `dictate` uses**, with the status item's
   events (`ToggleArmed`/`OpenSettings`/`CheckForUpdates`/`Quit`) drained
   alongside hold-key events every tick — see `dictate.rs`'s
   `run_agent_macos`.
5. **Check for updates in the background.** See
   [Auto-updates](#auto-updates) below for the full design; in short, a
   background thread checks the appcast on a timer (default on, toggled by
   `Settings::update_check_enabled`) and pushes what it finds to the menu
   bar's "Update:" row over the same non-blocking channel pattern
   `dictate.rs` already uses for the worker thread and the status item's
   own events.

### Settings apply from disk at launch; most changes apply live

Opening "Settings…" from the menu and changing something reloads
`crate::settings` the moment the window closes and applies **mode, paste
vs. clipboard-only, HUD on/off, and sound on/off immediately** — none of
those touch the input tap or the loaded ASR model, so swapping them in
place (a fresh cue/indicator, a reassigned local) is safe. **Hold key and
model changes are saved to disk but only take effect the next time Textify
Voice is (re)launched** — deliberately, not an oversight:
`crate::holdkey::HoldKeyTap` (outside this unit's file ownership) has no
`Drop` that tears down its `CGEventTap`, so a second `install()` call while
the first tap is still alive risks leaking or double-delivering a live
event tap rather than cleanly replacing it. The agent prints a note to this
effect when it detects the difference after the Settings window closes.

### Platform boundary: the menu bar is behind a trait, like the HUD

Per `docs/voice/PORTING.md`'s existing shape, `dictate.rs` never talks to
`NSStatusItem` directly — it talks to `platform::StatusUi`
(`set_state`/`set_hold_key`/`set_armed`/`poll_events`), the macOS adapter
(`platform::macos::MacStatusUi`) wraps `crate::menubar::MenuBar`, and
`platform::NullStatusUi` is what a platform without a tray host uses (a new
`PlatformCaps::can_show_status_ui` flag says whether one exists at all — see
`platform/caps.rs`). A Windows tray icon or Linux `StatusNotifierItem` are
the obvious future implementations, added beside `platform/macos.rs`
without touching `dictate.rs`'s loop, the same story `PORTING.md` already
tells for `Indicator`/`Cues`/`HoldKeySource`.

### Actually run, actual output (this session)

With both permissions already granted to this terminal (see the `dictate`
section above) and the onboarding funnel pre-satisfied (a `welcome.completed
= 1` counter — everything else in the funnel reads live state that was
already true), the zero-arg path was run for real, in the background, and
watched:

```
$ ./target/release/textify-voice
dictionary: 1 term(s), 1 literal rule(s) loaded from .../dictionary.txt
[... whisper.cpp/Metal model-load log ...]
mic: EarPods Microphone @ 44100 Hz / 1 ch (resampled to 16 kHz mono for ASR)
menu-bar agent ready -- hold left Option (⌥) to talk (Ptt mode), clipboard only
```

`osascript`/System Events confirmed the running process as `background
only: true` (the `Accessory` activation policy `hud.rs`'s panel
construction sets, so no Dock icon) — real evidence the process came up in
the correct agent shape, not just that it didn't crash. Stopped with
`SIGTERM`: clean exit, no crash report.

Separately, the **real app bundle** was built (`packaging/build-bundle.sh
--no-build`) and launched via `open` — the actual LaunchServices path, not
a simulation. The unified log confirms LaunchServices spawned the real
`textify-voice` process from inside `Textify Voice.app/Contents/MacOS/`,
and it stayed alive (rather than clap's old `exit=2` — see
`packaging/README.md`'s reproduced gap this closes) until stopped with
`SIGTERM` several seconds later, again with no crash report. Because this
was the bundle's `com.textify.voice` identity's *first* launch on this
machine (a fresh code identity has no prior permission grant, unlike the
terminal), the onboarding funnel genuinely had something to do: the real,
on-disk `~/Library/Application Support/textify/onboarding.txt` recorded
`welcome.reached = 1, welcome.completed = 0` afterward — direct evidence a
real "Welcome to Textify Voice" `NSAlert` was shown and was still on screen,
unclicked, when the process was stopped (removed afterward, restoring this
machine's real config directory to how it was found).

**NOT VERIFIED**: nobody looked at the menu bar or the onboarding alert —
this environment has no way to screenshot the real screen. `System Events`
confirmed the process's activation policy but could not enumerate its
status-item UI element the way it can for some other apps (queried, got an
empty result — inconclusive, not a negative result: `MenuBar::new()`
printed none of its own error paths, meaning the underlying
`NSStatusBar`/`NSMenu`/`NSMenuItem` construction that this wave's own AppKit
recon already proved dispatches for real did not fail). Clicking a menu
item, clicking through the onboarding wizard, and opening the Settings
window interactively were deliberately never done — each pops a real, blocking
`runModal()`/`NSAlert` on the operator's actual screen, and completing one
requires a real click no automated session can provide; see `settings.rs`'s
and `onboarding.rs`'s own module docs for the same discipline followed
throughout this wave. The Settings hot-reload logic above (mode/paste/HUD/
sound apply live, hold key/model don't) is real, compiled code exercising
real `crate::settings` types, but was not driven through an actual opened-
and-closed Settings window in this session.

**This wave's own re-verification** (after wiring the panic hook, the
compat gate, the updater, and the two new Settings checkboxes into this
same agent loop): `cargo build -p voice-cli --release` and
`packaging/build-bundle.sh` both succeed, `codesign --verify --deep
--strict` passes, and `open "Textify Voice.app"` launches a process that
stays resident (confirmed alive in `ps` several seconds later, then
stopped cleanly with `kill`) — reproducing exactly the fresh-identity
"onboarding alert blocking" shape described just above, since an ad-hoc
rebuild gets a new code identity every time (see [What changes about
permissions](#what-changes-about-permissions) below) and this session has
no way to click the resulting `NSAlert`. `~/Library/Logs/textify-voice.log`
shows the panic hook installing and the log redirect firing (`=== textify-
voice agent starting ===`) before anything else runs, and no crash
report was ever written — genuine evidence the startup sequence item 1 of
this unit's dispatch asks for (panic hook + log redirect first) executed,
and that adding the updater did not destabilize the agent's startup path.
**NOT VERIFIED, same reason as above:** the menu bar's new "Update:" row
and "Check for Updates…" item, and the `StatusUiEvent::CheckForUpdates`
click handling in `run_agent_macos` (download → stage → relaunch) —
compiles, is exercised by `menubar.rs`'s own unit tests for the
non-AppKit parts (tag routing, event mapping), but nobody has clicked it
on a real screen.

## Building the app bundle

```sh
packaging/build-bundle.sh                       # cargo build --release, then bundle + ad-hoc sign
packaging/build-bundle.sh --no-build             # rebundle whatever's already at target/release
```

Produces `packaging/dist/Textify Voice.app` — see `packaging/README.md`
for the full account of what this does and doesn't prove (ad-hoc signing,
`CFBundleIdentifier` as the load-bearing TCC key, why `spctl` rejects an
unnotarized copy, and exactly what was and wasn't verified building it).
The short version, re-verified in this unit:

- `plutil -lint`, `codesign --sign -` (ad-hoc), `codesign --verify --deep
  --strict` all pass.
- `spctl --assess` **rejects** the bundle — expected for ad-hoc/unnotarized;
  this blocks the Gatekeeper quarantine check on a *downloaded* copy, not a
  direct `open`/double-click launch of a local, unquarantined build (both
  confirmed working above).
- `open "Textify Voice.app"` really launches it via LaunchServices
  (confirmed via the unified log, above) into the agent loop this unit
  added — the zero-arg contract `packaging/README.md` specified and left
  unimplemented is now implemented and exercised end to end.

### What changes about permissions

Running the **bundled app** is *designed* to get Microphone/Accessibility
grants attributed to `Textify Voice` (via its stable `CFBundleIdentifier`,
`com.textify.voice` by default) under System Settings → Privacy & Security
rather than to
whatever terminal happens to invoke the bare binary — which is the whole
point of bundling it (see `packaging/README.md`'s "Why `CFBundleIdentifier`
is load-bearing"). **Not observed:** no TCC consent dialog has ever been
completed for the `com.textify.voice` identity in any automated run, so the
attribution above is documented macOS behaviour, not something anyone here
watched happen. You will be the first to see it. Running the **raw `target/release/textify-voice` binary**
directly from a terminal (`cargo run`, or the bare path) still attributes
grants to that terminal app, exactly as documented throughout this README
— that's the mechanism this session's own terminal ended up with both
permissions in the first place, real evidence of the "wrong security model"
`packaging/README.md` describes.

One caveat carried over from `packaging/README.md`, not re-litigated here:
because this build is **ad-hoc-signed** (no Developer ID certificate exists
in this environment), whether a grant made to `Textify Voice.app` survives
a rebuild is unverified — ad-hoc signatures are understood to key TCC
grants off a per-build content hash (`CDHash`) rather than a stable Team
ID, so re-granting after every rebuild is the documented, expected risk
until real Developer ID signing exists. See that file for the full
CDHash-reproducibility investigation.

## Settings

```sh
textify-voice settings          # open the Settings window from a terminal
```

The same window the menu-bar agent's "Settings…" item opens
(`crate::settings::open_settings_window`). Edits hold key, mode (PTT/
toggle), model (`tiny.en`/`base.en`), paste-vs-clipboard-only, HUD on/off,
sound on/off, **automatic update checking** on/off, and **automatic crash
report upload** on/off; every control saves on change. A live permissions
panel (`crate::permissions::check()`, on demand) and a "Reveal in Finder"
button for the dictionary path are also in the window.

The last two are privacy-relevant, so they default and persist
differently from the rest — see [Auto-updates](#auto-updates) and
[Diagnostics and crash reporting](#diagnostics-and-crash-reporting) below
for the full story:

- **"Automatically check for updates"** is `Settings::update_check_enabled`,
  persisted in the same `settings.txt` as everything else above, **on by
  default**. A check sends nothing about you or this machine — one HTTPS
  GET of a static, public JSON file — so leaving it on is what actually
  gets a bug fix to a beta user who never opens this window.
- **"Automatically send crash reports"** is a *separate* setting,
  persisted by `crate::diagnostics` (not this file), **off by default**.
  Unlike an update check, a crash report can contain real information
  about what you were doing when something broke, so it stays opt-in.
  Today it's also a no-op even switched on — no server is configured
  anywhere in this build (see [Diagnostics and crash reporting](#diagnostics-and-crash-reporting)) — the checkbox only records your
  preference for whenever that changes.

Persists to a plain, hand-rolled `key = value` text file (not JSON/TOML —
this crate had no serialization dependency when `settings.rs` was written;
see that file's module doc):

```
~/Library/Application Support/textify/settings.txt
```

Override with `TEXTIFY_VOICE_SETTINGS_PATH`. Missing file = every field
defaults to exactly what `dictate`'s own CLI flag defaults are (`--hold-key
left-option`, PTT mode, `base.en`, clipboard-only, HUD+sound on). A corrupt
or partially-unreadable file degrades field-by-field rather than wiping
every setting or refusing to start. See
[Running as the app](#running-as-the-app-menu-bar-agent) above for exactly
which settings changes the agent applies immediately versus on next
relaunch.

**NOT VERIFIED**: the window itself has never been shown on a real screen
in any session (this one included — see above); `runModal()` was
deliberately never called interactively.

## Onboarding

```sh
textify-voice onboarding        # run the wizard from a terminal
```

The same first-run wizard the menu-bar agent shows automatically until it's
`Ready` (`crate::onboarding::open_onboarding_window`) — a short sequence of
`NSAlert`s: Welcome → Microphone → Accessibility → model download → Ready,
each step re-evaluated against **live** permission/model-cache state every
time (so revoking a permission mid-flow, or between agent launches, snaps
the funnel back to the right step automatically rather than trusting a
stale "already done" flag — see that module's doc for why this is a pure
recomputation, not a stored cursor). "Open System Settings" deep-links to
the exact Privacy & Security pane via `x-apple.systempreferences:` URLs
corroborated (not executed) by `strings`-scanning real installed apps with
the same permission profile on this machine (VoiceInk.app, Caffeine.app,
Telegram.app).

Reached/completed counts per step persist, forever, locally (no telemetry
backend exists in this CLI), to the same kind of hand-rolled text file as
Settings:

```
~/Library/Application Support/textify/onboarding.txt
```

Override with `TEXTIFY_VOICE_ONBOARDING_PATH`. Safe to `cat` or delete (delete
= reset the funnel to a fresh-install state) at any time.

**Actually run, actual output (this session)**: launching the real app
bundle for the first time (see [Building the app bundle](#building-the-app-bundle)
above) genuinely reached and showed the Welcome alert (confirmed via the
real, on-disk counter file afterward — `welcome.reached = 1`) before being
stopped. Clicking through the rest of the funnel — the Microphone/
Accessibility/model-download/Ready steps — was not done in this session,
for the same "no automated click on a real modal" reason documented
throughout.

## Auto-updates

```sh
textify-voice update-check          # check right now, from a terminal or a script
```

Before this wave, there was no way to fix a bug for anyone who had already
downloaded the app — a real gap for a self-signed beta with no Apple
Developer ID (so no Sparkle-style, code-signing-integrated updater to
lean on either; see `crates/voice-cli/src/update.rs`'s own doc comment for
the recon behind that decision). What exists now:

- **Appcast**: a small JSON document (`packaging/appcast/README.md`
  documents the format and the release workflow) fetched over
  **HTTPS only** — both the appcast URL and the payload URL are refused
  pre-flight if they aren't `https://`, before any process is even
  spawned to fetch them.
- **The real security boundary is a signature, not TLS or code signing.**
  Because this app is self-signed, macOS code signing gives an update
  payload *zero* protection — whoever can write to wherever the appcast
  is hosted could point it at any binary. Every payload is verified with
  **ed25519** (via the `ring` crate) against a public key **compiled into
  this binary**, before a single byte of it is unpacked, executed, or
  moved anywhere near the installed app. A failed verification deletes
  the download; nothing downstream ever sees an unverified payload.
- **Downgrade prevention** is enforced twice, independently: once when
  deciding whether an update is even reported as available, and again
  right before installing — so a caller that skips the first check still
  can't be tricked into "upgrading" to an older, signed build.
- **Where the appcast lives**: `https://downloads.textify.me/voice/appcast.json`,
  an R2 bucket behind a Cloudflare custom domain. That is the compiled-in
  default (`dictate::DEFAULT_UPDATE_APPCAST_URL`);
  `TEXTIFY_UPDATE_APPCAST_URL` overrides it for testing. Note the domain is
  `textify.me` -- earlier drafts of this README and the appcast template said
  `updates.textify.app`, which this project has never owned. See
  `packaging/appcast/README.md`'s "Cutting a release" for how the file is
  produced and published.

  ```
  $ textify-voice update-check
  checking https://downloads.textify.me/voice/appcast.json for updates (current version 0.1.0)...
  up to date.
  ```

  Pointed at a real HTTPS URL that isn't a valid appcast, the check still
  runs the real network fetch and fails at the right, later stage (parse,
  not DNS):

  ```
  $ TEXTIFY_UPDATE_APPCAST_URL="https://raw.githubusercontent.com/sparkle-project/Sparkle/master/LICENSE" textify-voice update-check
  checking https://raw.githubusercontent.com/.../LICENSE for updates (current version 0.1.0)...
  Error: malformed appcast: expected value at line 1 column 1
  ```

  And a plain `http://` URL is refused before any network call at all:

  ```
  $ TEXTIFY_UPDATE_APPCAST_URL="http://example.com/appcast.json" textify-voice update-check
  checking http://example.com/appcast.json for updates (current version 0.1.0)...
  Error: refusing a non-https URL: http://example.com/appcast.json
  ```

- **The menu-bar agent** (see [Running as the app](#running-as-the-app-menu-bar-agent)
  above) runs this same check on a background thread every 6 hours by
  default — more frequent than most desktop apps' once-a-day default, on
  purpose: a beta that just shipped its first fix should be able to reach
  a running install within hours, not a day. `Settings::update_check_enabled`
  (on by default — see [Settings](#settings) above) turns the background
  timer off without disabling the manual check. The "Update:" row and
  "Check for Updates…" item track `update::UpdateState`: up to date, available,
  downloading (with live byte progress), ready to relaunch, or failed
  with a message. Clicking "Check for Updates…" means something different
  depending on that state — check now, start the download, or install and
  relaunch — all handled in `dictate.rs`'s `run_agent_macos`.
- **Installing** unpacks the verified payload with `ditto` (confirmed
  elsewhere in this codebase to be the one archiving method that
  round-trips a macOS app bundle's code signature intact), strips the
  quarantine flag, then writes and spawns a small detached `/bin/sh`
  helper script that waits for this process to exit, atomically swaps the
  staged app into place (rolling back to a backup on any failure so the
  installed app is never left half-replaced or missing), and relaunches —
  the same out-of-process pattern Sparkle uses for the same reason: a
  running process cannot atomically overwrite its own bundle on disk.

**NOT VERIFIED**: nobody has clicked "Check for Updates…" on a real
screen, so the full download → stage → relaunch path triggered from a
live menu click has never been observed end to end — only its individual
pieces (signature verification, HTTPS enforcement, downgrade prevention,
the swap script's own logic against synthetic directories) were, each
with real, automated tests (`cargo test -p voice-cli update::` /
`menubar::`). No real appcast has ever been hosted anywhere, so no real
release has ever been checked for, downloaded, or installed by this
mechanism — only the failure paths above, which are the only paths
reachable without one.

## Diagnostics and crash reporting

```sh
textify-voice diagnostics                 # write one bundle, print its path
textify-voice diagnostics --enable-upload   # opt in to automatic upload (see below)
textify-voice diagnostics --disable-upload  # opt back out (the default)
```

A panic hook is installed as the **first statement of `main()`** — before
`Cli::parse()`, before the zero-argv agent-mode check, before anything
that could fail — so a crash during startup (the one place this project
previously had zero visibility) is captured too. A crash writes one report
to `~/Library/Application Support/textify/crash-reports/` with a
timestamp, version, git SHA (see the caveat below), device tier, which
subsystem was active (`dictate`, `transcribe`, `settings`, …), the panic
location and message, and a backtrace.

**What it collects, concretely**: the tail of `~/Library/Logs/textify-voice.log`
(with dictated-transcript lines excluded — see the leak this closes,
below), the most recent crash reports, this build's version/git-SHA/device
tier, live permission state (granted/not, never *why* or *for which app
you were dictating into*), and your current settings. `textify-voice
diagnostics` writes this to a single file **you read yourself** before
deciding whether to share it with anyone — nothing is sent automatically
unless you explicitly opt in (see below), and even then, see the next
paragraph.

**What it never collects**: your dictated speech or its transcript,
clipboard contents, Accessibility-read window titles/labels, or your user
dictionary. `dictate.rs`'s own log line (`> {text}`, the verbatim
transcript, written so a headless agent's output is visible somewhere) is
a real, pre-existing leak into the exact log file this bundle reads from —
found during this wave and closed at the diagnostics layer (lines matching
that marker are excluded before the rest of the log is included), with the
leak itself flagged as a follow-up for whoever next touches `dictate.rs`.
`scrub_sensitive()` is a second, independent pass over everything that
does get included, redacting long digit runs and email-like tokens as
defense in depth.

**Upload is off by default, and today doesn't do anything even switched
on.** `Settings`'s "Automatically send crash reports" checkbox (see
[Settings](#settings) above) and `--enable-upload`/`--disable-upload`
both write the same on-disk flag `crate::diagnostics` reads. But no
network client or third-party SDK is wired into this build at all — the
one `Transmitter` implementation that exists always errors — so turning
this on today only records your preference for whenever a real backend
exists; it does not make anything leave your machine.

**Known gap**: the git SHA in a crash report reads `unknown` in the
**real packaged app** today. `option_env!("TEXTIFY_VOICE_GIT_SHA")` is
correctly plumbed at compile time, but nothing sets that environment
variable before `cargo build` runs — `packaging/build-bundle.sh` computes
the git SHA and stamps it into `CFBundleVersion`/`BuildInfo.txt`
separately, after the binary is already built, so the two don't yet
share one source of truth. A future build-tooling change setting
`TEXTIFY_VOICE_GIT_SHA=$(git rev-parse --short HEAD)` ahead of the build
closes this; not done here (outside this unit's owns-list).

**NOT VERIFIED**: crash capture during an actual live `dictate` session
(mic/ASR/insertion threads) — the hook is thread-agnostic by construction
(Rust panic hooks fire on whichever thread panics) but no real panic was
triggered from inside a running session in this environment. No
TCC-denied state was exercised for the permissions section of a bundle
(this development machine already has both grants).

## Platform floor and device compatibility

```sh
textify-voice device-tier          # architecture, chip, cores, RAM, macOS version, Metal availability
```

Two platform facts are now enforced at startup, checked before any
subcommand except `device-tier`/`diagnostics`/`update-check` runs (those
three exist to *explain* a machine, including an unsupported one, so all
three still run on one):

- **Apple Silicon only, for v1.** An Intel Mac — native or an Intel build
  running translated under Rosetta 2 — is refused at startup with a
  one-sentence explanation rather than a silent, untested CPU-only limp.
  `voice-asr-whisper`'s Metal acceleration is scoped to exactly `aarch64`
  macOS; every latency number this project has ever measured came from
  Apple Silicon, and CPU-only whisper.cpp has never been measured against
  push-to-talk's latency budget on any hardware, because the
  `x86_64-apple-darwin` target has never been installed on any machine
  this project has been built on (no `rustup` on this box). A declared
  "not supported yet" beats a real download that might be unusably slow
  with no warning it was ever a known risk.
- **`LSMinimumSystemVersion` 11.0, and enforced, not just declared.**
  Previously 13.0 in `Info.plist` with nothing behind it, while the
  compiled Mach-O's own `LC_BUILD_VERSION` said `minos 11.0` — two
  numbers, neither derived from the other, neither checked at runtime.
  11.0 is the real floor Apple Silicon implies (no arm64 Mac has ever
  shipped below Big Sur), matches every macOS API this codebase actually
  calls (all comfortably pre-11.0 except SF Symbols, which already
  degrades to a hand-drawn fallback icon when unavailable), and is now a
  real runtime check, not just a Launch Services hint.

`device-tier` always runs, on any machine the gate above would otherwise
refuse, because explaining *why* a machine is unsupported is exactly what
it's for. Real output on this development machine:

```
$ textify-voice device-tier
architecture     : Apple Silicon (arm64) (supported)
chip             : Apple M1 Max
cores            : 10 total (8 performance + 2 efficiency)
memory           : 64.0 GB
macOS            : 26.6
Metal (whisper)  : available
```

**NOT VERIFIED**: behavior on an actual Intel Mac (native or Rosetta) or
on macOS 12–15 — no Intel hardware and no buildable `x86_64-apple-darwin`
target exist in this environment (`rustup` is not installed), and this
machine only ever reports macOS 26.6. The classification logic itself
(`classify_arch`/`floor_blocking_reason`) is covered by unit tests against
many synthetic inputs, including both exact-boundary cases, but was never
exercised against a real old-OS or real Intel machine.

## Bench workflow

```sh
textify-voice bench record      # interactive: record real takes into fixtures/voice/manifest.json
textify-voice bench score       # run the recorded manifest through the real ASR pipeline + WER
```

Records/scores the WP-V0.0 (hard-slice: proper-noun, code-identifier,
accented-en, whispered) and COMMANDS-SPEC C0.0 (command, dictation-lookalike
adversarial) prompt corpus at `fixtures/voice/prompts/prompts.json` — see
that directory's own `README.md` for the corpus and `bench record --help`/
`bench score --help` for every flag (prompt filtering by `--kind`/`--tag`/
`--only`, output paths, `--model`, `--use-dictionary`). `bench record` is a
plain-terminal Enter-to-start/Enter-to-stop loop that quality-checks each
take's peak/RMS (flags silence/clipping/too-short takes) before you
accept/redo/skip it, writes real 16 kHz mono WAV via `hound`, and updates
the manifest in place — resumable, never re-records an already-accepted
clip. `bench score` decodes the recorded clips through the same real
whisper.cpp + bias-layer-2 + normalizer path `dictate`/`transcribe` use,
then scores them with the existing `fixtures/voice/wer.ts` harness (via
`tsx`, which must be on `PATH`) and prints the SPEC §7 per-hard-slice-tag
results table.

**Actually run, actual output (this session)**, against a scratch manifest/
audio directory (the real `fixtures/voice/manifest.json` and
`fixtures/voice/audio/` were never touched):

```
$ textify-voice bench record --only proper-noun-01 --manifest <scratch> --audio-dir <scratch>
...
mic: EarPods Microphone (44100 Hz, 1 ch)

--- proper-noun-01 [dictation] ---
read aloud:
  "Please forward the Nairobi contract to Farrukh Otieno before you leave for lunch."

[Enter] start recording   q = quit session: recording... [Enter] stop
  take: 0.96s, peak 169/32767, rms 40.1
  WARNING: this take looks SILENT (peak 169 out of 32767, below the 300 guard) -- ...
[Enter] accept   r = redo   s = skip this prompt   q = quit session: saved .../proper-noun-01.wav (0.96s)

session done -- 1 take(s) recorded this run.
```

Real `cpal` capture against the real device, a real 16 kHz mono WAV written
(confirmed with `file`), and a schema-shaped manifest entry — the SILENT
warning is correct and expected, since no automated session spoke into the
mic; it's a pipeline-plumbing proof, not an accuracy measurement. `bench
score` itself (whisper decode → normalize → `wer.ts`) was not independently
re-run in this unit's verification pass, since `bench.rs` was not modified
here — see the `build:bench-recorder` unit's own report for its full,
separately-verified end-to-end run (including a real `bench score` pass).

## `--verbose`

Every subcommand accepts a global `--verbose` (put it before or after the
subcommand name). For `transcribe` and `dictate` it prints per-stage timings
(model load, decode/capture, ASR, normalize, insert) to stderr; every run,
verbose or not, prints a `speech-end-to-text:` summary line, since SPEC.md's
whole latency budget is the point of measuring this at all.

## What works today (summary)

| Subcommand | Status |
|---|---|
| `transcribe` | **Real, wired end to end.** File → decode → whisper.cpp → normalize → stdout, actually run against both fixture clips, output reported above. Measured with genuinely default arguments: `short-5s.wav` WER 0.0, `ref-3min.wav` WER 0.0526 — see [Long-form audio](#long-form-audio-chunking-is-on-by-default-above-60s) above. |
| `command` | **Real, fully verified.** Dry-run only by construction; all three required example behaviors (resolve/escalate/reject) actually run, output reported above. |
| `models` | **Real, fully verified.** Actually downloaded `tiny.en` in this session; cache-hit-skips-redownload also verified. |
| `dictate` | **Real code, live loop reached, human step unverified.** Permission gate exercised both ways (refused when Accessibility was missing; passed once granted, see above). The live loop was actually run and reached "ready" for real; the capture→ASR→insert path past that point needs a human to hold a key and speak, which no automated session did — see [Known gaps](#known-gaps-in-dictate) above. |
| zero-arg agent | **Real code, run for real (background), never watched.** `main.rs`'s zero-arg interception, `dictate::run_agent`/`run_agent_macos`, the menu-bar status item, and Settings hot-reload for mode/paste/HUD/sound are real, compiled, and were run for real (both the raw binary and the bundled `.app` via `open`/LaunchServices) reaching "menu-bar agent ready" — see [Running as the app](#running-as-the-app-menu-bar-agent) above. Nobody has looked at the menu bar, clicked a menu item, or driven the onboarding/Settings windows interactively. |
| `bench record` / `bench score` | **Real, run end to end** (by the unit that built it, and re-confirmed here with a scratch manifest) — real mic capture, real WAV, real whisper.cpp decode, real `wer.ts` scoring. No human speech has been recorded by any automated session, so WER/recall numbers from these runs measure pipeline plumbing, not ASR accuracy. |
| `settings` / `onboarding` | **Real code, compiles and type-checks against real AppKit bindings, deliberately never driven through an interactive `runModal()`.** Each opens a real, blocking native window — completing that requires a real click no automated session can provide. Now includes the automatic-update-check and diagnostics-upload checkboxes. |
| `update-check` | **Real, fully verified for every path this environment can reach.** A real HTTPS fetch runs end to end (confirmed against a real, non-appcast HTTPS URL, failing at parse rather than DNS); a plain `http://` URL is refused before any network call; no real appcast exists anywhere yet, so "up to date"/"available" have never been produced by a real check — only the failure paths above, output reported in [Auto-updates](#auto-updates). |
| `diagnostics` | **Real, fully verified.** Actually wrote a bundle on this machine with real permission/device-tier/settings data; upload stays off by default and the shipped `Transmitter` always errors even when turned on — see [Diagnostics and crash reporting](#diagnostics-and-crash-reporting). |
| `device-tier` | **Real, fully verified on this machine.** Output reported in [Platform floor and device compatibility](#platform-floor-and-device-compatibility); Intel/Rosetta/old-macOS paths are unit-tested against synthetic inputs only — no such hardware exists in this environment. |
| menu-bar updater (background checker, "Check for Updates…", download/stage/relaunch) | **Real code, compiles, unit-tested piece by piece, never clicked.** Same "reaches the gate this environment can run up to, not through" shape as the rest of the menu bar — see the re-verification note at the end of [Running as the app](#running-as-the-app-menu-bar-agent). |

Don't take "the code exists" as "it works" for anything gated on a human
clicking something real — that's exactly the distinction this README is
trying to keep honest. `transcribe` and `command` are the load-bearing proof
that the underlying pipeline (real ASR, real normalizer, real command spine)
is genuinely wired end to end; `dictate` and the agent loop are that same
pipeline plus OS-permission-gated and human-interaction-gated surfaces this
environment can run up to, but not click through.
