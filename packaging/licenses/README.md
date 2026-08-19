# Third-party licenses

This directory is copied verbatim into every built bundle at
`Contents/Resources/Licenses/` by `packaging/build-bundle.sh` -- it is the
"way to read them" inside a shipped `Textify Voice.app`: right-click the
app in Finder, **Show Package Contents**, then
`Contents/Resources/Licenses/`.

**Start here: `THIRD-PARTY-NOTICES.txt`.** One file, plain text, every
third-party crate this binary actually links against (name, version, SPDX
license identifier, and the full license text), plus sections for
whisper.cpp and the ggml model weights at the end. It is generated, not
hand-maintained -- see "Regenerating" below.

## Layout

```
licenses/
  README.md                  <- this file
  THIRD-PARTY-NOTICES.txt    <- the one file a human should open
  MODELS-NOTICE.md           <- ggml model weights license + hosting
  whisper.cpp/LICENSE        <- MIT, standalone copy for convenience
  vendor/<crate>-<version>/  <- raw per-crate LICENSE file(s), as published
                                 on crates.io -- the source material
                                 THIRD-PARTY-NOTICES.txt was assembled from
  spdx-texts/*.txt           <- canonical SPDX license texts, used only for
                                 the handful of crates that don't publish
                                 their own LICENSE file in the crates.io
                                 package (see THIRD-PARTY-NOTICES.txt for
                                 exactly which)
```

## How this was generated (2026-08-18)

1. `cargo license --avoid-dev-deps --avoid-build-deps --filter-platform
   aarch64-apple-darwin -j` -- resolves the *exact* dependency graph
   `cargo build --release -p voice-cli` produces for this target from
   `Cargo.lock`, not a hand-kept list. This is deliberately narrower than
   an unfiltered `cargo license` run: without `--filter-platform` the tool
   reports every dependency the workspace's `Cargo.lock` could resolve on
   *any* platform (e.g. `alsa` for Linux, `windows-sys` for Windows), most
   of which are never compiled into this Mac binary and would be noise
   here. Filtered to `aarch64-apple-darwin`: **102 third-party crates**
   (our own 8 workspace crates -- `voice-core`, `voice-context`,
   `voice-format`, `voice-intent`, `voice-act`, `voice-audio`,
   `voice-asr-whisper`, `voice-cli` -- are `UNLICENSED`/proprietary and
   excluded from this bundle).
2. For each crate, its extracted `~/.cargo/registry/src/*/<name>-<version>/`
   directory was searched for a `LICENSE`/`LICENCE`/`COPYING`/`UNLICENSE`-
   family file, copied verbatim into `vendor/`. 73 of 102 crates publish
   one. The 29 that don't (mostly the `objc2-*` platform-binding family,
   which ship a repo-root `LICENSE` not included in the individual
   published packages) fall back to the canonical SPDX text for one of
   their declared license options (see `spdx-texts/`) -- called out
   per-crate in `THIRD-PARTY-NOTICES.txt` so nothing is silently assumed.
3. `whisper.cpp` itself (MIT) is not a crates.io package -- it's vendored
   C/C++ source inside `whisper-rs-sys` (pinned to `0.14.1` in this
   workspace's `Cargo.toml`) and compiled directly into this binary by that
   crate's `build.rs`. Its `LICENSE` file was copied from
   `whisper-rs-sys-0.14.1`'s own vendored copy.
4. The ggml model weights (downloaded at first run, not compiled into the
   binary) were confirmed MIT via a live query against the HuggingFace API
   -- see `MODELS-NOTICE.md`.

## The one non-permissive license: `option-ext` (MPL-2.0)

Every dependency here is permissively licensed (MIT / Apache-2.0 / BSD /
ISC / Zlib / 0BSD / Unlicense / BSL-1.0 / CDLA-Permissive-2.0, or an
OR-combination) **except one**: `option-ext` 0.2.0, a small crate pulled in
unconditionally by `dirs-sys` (via `dirs`, used for the model-cache and
user-dictionary data directories) -- confirmed compiled into the actual
release binary, not just resolved-but-unused
(`target/release/deps/liboption_ext-*.rlib` exists after a real build).

`option-ext` is MPL-2.0: a *file-level* weak-copyleft license, not GPL-style
copyleft. Its own source is unmodified in this build. MPL-2.0 requires that
modifications to *its own covered files* be released under MPL-2.0 if
distributed; it explicitly does not extend that requirement to the rest of
a "Larger Work" that merely links against it (MPL-2.0 section 1.4/3.3). It
does not require Textify Voice itself, or this binary as a whole, to be
MPL-licensed or open-sourced. Its full text is included in
`THIRD-PARTY-NOTICES.txt` and `vendor/option-ext-0.2.0/` like every other
dependency; no code from it has been modified by this project.

No GPL, LGPL, AGPL, or SSPL dependency was found anywhere in the
platform-filtered graph.

## Regenerating

There is no `make regen-licenses` script committed here (the generation
script that produced this directory lived in a scratch dir for this pass,
not the repo, since it shells out to `cargo license` -- an extra installed
tool -- and reads `~/.cargo/registry/src/`, both host-machine state rather
than something that belongs under version control as a build step). To
redo this audit after a dependency bump:

```sh
cargo install cargo-license --locked   # once
cargo license --avoid-dev-deps --avoid-build-deps \
  --filter-platform aarch64-apple-darwin -j
```

then re-diff the crate list against this directory's `vendor/` and
`THIRD-PARTY-NOTICES.txt`'s INDEX section, and re-run the flagging check
(anything that is not MIT/Apache-2.0/BSD/ISC/Zlib/0BSD/Unlicense/BSL-1.0/
CDLA-Permissive-2.0/MPL-2.0, or a new MPL/LGPL/GPL hit) before a release.
