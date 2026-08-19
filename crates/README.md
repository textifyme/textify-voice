# crates/ — Textify Voice Rust workspace

Five crates implement the deterministic spine of local voice dictation and
command mode. Layout per SPEC.md §3.2 and COMMANDS-SPEC.md §3.2.

- **voice-core** — hot path: capture, VAD, ring buffer, local ASR trait, bias
  pipeline layers 1–2, insertion (SPEC §3.2, §3.3).
- **voice-context** — AX/UIA capture + OCR bridge; extended for command mode
  with the actionable-element map (SPEC §3.2; COMMANDS-SPEC §3.2).
- **voice-format** — local formatter: mistral.rs runtime + Apple FM /
  TextRewriter bridges (SPEC §3.2, §3.4).
- **voice-intent** — stage-1 grammar tables + stage-2 constrained-parse
  harness for command mode (COMMANDS-SPEC §3.2, §3.3).
- **voice-act** — ActionRegistry: typed executors, tier policy, undo journal
  (COMMANDS-SPEC §3.2, §3.3).

All native/IO backends (cpal, silero VAD, sherpa-onnx/ort, AXUIElement,
UIAutomation, clipboard, mistral.rs, Apple Foundation Models, global
hotkeys, SQLite) are trait-stubbed with deterministic in-memory
implementations in this phase — no native dependencies, no model downloads,
no runtime network calls.
