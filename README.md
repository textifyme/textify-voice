# Textify Voice

Free, local, open-source voice dictation for macOS. Hold a key, speak, and your
words appear wherever your cursor is — Slack, Mail, VS Code, any text box.
Speech recognition runs entirely on your Mac with a local Whisper model:
no account, no upload, no subscription.

**Download the signed build:** https://textify.me/voice
**License:** GPL-3.0-only (see [LICENSE](LICENSE)).

## Why trust it

"Local" claims are cheap; this repo exists so you don't have to take ours on faith.

- **The only network calls** are the one-time model download and an update check
  you can disable in Settings. Grep the source: the transcription path has no
  network I/O.
- **Updates are fail-closed.** Every update payload is ed25519-signed; the public
  key is compiled into the binary (`crates/voice-cli/src/update.rs`,
  `PUBLIC_KEY_HEX`). A payload that doesn't verify is discarded — nothing is
  staged on failure.
- **Checksums are published.** The SHA-256 of every released DMG is printed on
  the download page; verify with `shasum -a 256`.
- **The accuracy bench is open.** `fixtures/voice/` scores word-error-rate
  against LibriSpeech test-clean (CC-BY, fetched on demand — never
  redistributed here). It is a regression tripwire, not a marketing number.

## Build from source

Requirements: Apple Silicon Mac, macOS 11+, Rust (see `rust-toolchain.toml`),
Xcode command-line tools.

```sh
cargo build --release            # builds the CLI + menu-bar binary
cargo clippy --all-targets -- -D warnings
cargo test
```

To produce a signed .app bundle + DMG, see `packaging/README.md` and
`packaging/build-bundle.sh`. Release builds are signed with a stable identity
so macOS permission grants (Microphone, Accessibility) survive updates.

## Run the accuracy bench

```sh
python3 fixtures/voice/fetch-open-corpus.py   # fetches LibriSpeech test-clean
cargo run -p voice-cli -- bench score --manifest fixtures/voice/manifest.open.json --model base.en
```

## Repository layout

- `crates/voice-core` — capture, VAD, local ASR trait, bias pipeline, insertion
- `crates/voice-asr-whisper` — whisper.cpp integration, pinned model downloads
- `crates/voice-audio` — audio capture + permissions
- `crates/voice-context` / `voice-format` / `voice-intent` / `voice-act` —
  context capture, local formatting, and the (deliberately inert in shipped
  builds) command mode
- `crates/voice-cli` — the binary: CLI, menu-bar agent, onboarding, updates
- `packaging/` — .app bundle, DMG, appcast tooling
- `fixtures/voice/` — the open WER bench

## Relationship to textify.me

This is a read-only mirror of the voice workspace inside the Textify monorepo,
refreshed on every release. Issues and PRs are welcome here; changes land
upstream and flow back with the next release. Design docs live upstream.

The app is free forever — local dictation is never metered or paywalled.
Textify's paid subscription covers optional online services (settings/dictionary
sync, cloud jobs on textify.me); none of it is required to use this app.
