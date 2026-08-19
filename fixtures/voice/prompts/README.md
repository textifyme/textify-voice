# Bench recording prompts

`prompts.json` is the input to `textify-voice bench record` (`crates/voice-cli/src/bench.rs`,
DECISIONS.md D2). It is **not** the eval manifest — `fixtures/voice/manifest.json` (created/updated
by `bench record`, sibling to `manifest.example.json`) is the thing `fixtures/voice/wer.ts` actually
scores, and it conforms exactly to `manifest.schema.json`.

## Why two files

`manifest.schema.json`'s clip object is `"additionalProperties": false` with a fixed field set
(`hard_slice_tags` restricted to the four WP-V0.0 tags). It has no room for "is this a command
utterance," "must this reject in command mode," or "whisper this line" — none of that belongs in
the WER-scoring format, but a recording session still needs it. So:

- `prompts.json` is the **richer** source: every prompt has an `id`, a `kind`
  (`dictation` | `command` | `adversarial`), the text to read, optional `hard_slice_tags`/
  `bias_terms` for dictation prompts, an optional `direction` (e.g. "whisper this"), and `notes`.
- `manifest.json` is the **narrower**, schema-exact projection `bench record` writes one clip into
  per accepted take — reference text, audio path, tags, bias terms, `placeholder: false`. Anything
  from `prompts.json` that doesn't fit the schema (kind, direction, adversarial-must-reject intent)
  is preserved as a plain sentence in the clip's optional `notes` string, which the schema does
  allow, so nothing is lost, but nothing extra is smuggled past `additionalProperties: false`.

## Prompt kinds

- **`dictation`** — the WP-V0.0 hard-slice corpus itself (`proper-noun`, `code-identifier`,
  `accented-en`, `whispered`). These are what `bench score` / `wer.ts` actually measures WER and
  hard-slice term recall against.
- **`command`** — COMMANDS-SPEC C0.0 head commands ("Open Slack.", "Undo that."). Recorded through
  the identical take/WAV/manifest pipeline so their ASR transcripts can be checked too, but
  command-*intent* accuracy (does command mode correctly accept these) is scored in
  `fixtures/commands/`, not here — `bench score` only measures transcription word accuracy.
- **`adversarial`** — COMMANDS-SPEC C0.0 "dictation-lookalike" adversarials: sentences that contain
  a command trigger word but are dictated prose, not directed at the assistant ("I told him to open
  Slack before the standup"), and must be *rejected* by command mode (COMMANDS-SPEC's ≥99% reject
  bar). Same caveat as `command` above: this tool records the audio and reference text; the reject
  decision itself is `fixtures/commands/`'s job.

## Adding prompts

Append an entry to the `prompts` array with a new, stable `id` (never renumber an id once real
audio has been recorded against it — `bench record` uses the id to skip already-completed takes and
as the WAV filename). Write a real sentence a person can say naturally out loud; the corpus is only
as good as the prompt. See `prompts.json`'s existing entries and each hard slice's rationale in
`fixtures/voice/README.md`.
