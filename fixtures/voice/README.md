## Remediation-wave note (scope check, read once)

A tier-2/tier-3 audit wave found real defects in `fixtures/commands/`
(schema-id divergence between the grammar, the action registry, and the
fixture corpus; an untested verb-initial-dictation adversarial class; a
homoglyph/fuzzy-match bypass of the destructive-label escalation check) and
that directory's fixtures + tests were remediated accordingly — see
`fixtures/commands/README.md`'s "Remediation wave" section for the details.
**This directory (the WP-V0.0 ASR/WER harness) was not implicated by that
audit and nothing here changed as a result of it** — the two directories
test different layers (`fixtures/voice/` scores transcription accuracy;
`fixtures/commands/` scores command-intent classification and its injection
defenses) and don't share a schema-id registry or any other coupled state.
Noted here only so a reader who saw the commands-side audit doesn't wonder
whether this harness's numbers are affected — they aren't, because there
are still no numbers here at all (see below).

# Voice eval corpus + scoring harness (WP-V0.0)

This directory is the **substrate** for `docs/voice/SPEC.md` §2 "Week 1–2
(bench, WP-V0.0)" and the §7 kill criteria. It is **not** a bench result —
there is no audio here yet, and no engine has been run. What's here:

1. `manifest.schema.json` — the JSON shape every eval clip must follow.
2. `manifest.example.json` — 8 realistic **placeholder** entries (schema
   populated with real reference-text prose, zero attached audio) showing the
   tone/length/tagging a human curator should aim for.
3. `wer.ts` — a dependency-free scoring library: word error rate from
   scratch, per-hard-slice bias-term recall, a corpus-level micro-average,
   and a markdown results-table formatter.
4. `wer.test.ts` — 25 unit tests, including hand-computed WER cases you can
   verify by hand (substitution/deletion/insertion/mixed, degenerate empty
   cases, WER > 1.0).

## What a human must still supply — read this before trusting any number

This harness can score transcripts; it cannot produce them. To get a real
§7 read, a human needs to:

1. **Record or source real audio** for each hard-slice category below, save
   it as 16 kHz mono PCM WAV under `fixtures/voice/audio/` (this format
   matches `LocalAsr::feed_pcm`, SPEC §3.3, directly — no resample step
   needed in the bench loop), and add a manifest entry with `placeholder:
   false`.
2. **Human-verify the reference transcript** for each clip — listen to the
   recording and correct the text by ear. **Never** use an ASR draft (even a
   "corrected" one) as the reference; that contaminates the WER you're
   trying to measure ASR against. `reference_text` must be independent of
   every engine under test.
3. **Get real proper nouns / people who'll say them.** The "proper-noun"
   slice is only meaningful with names an off-the-shelf model has no lexical
   prior for — pull from your own contacts/orgs, not celebrity names an LLM
   has memorized.
4. **Record actual whispers**, not quiet normal speech. Whispered speech
   lacks fundamental frequency (F0) entirely — gain-normalizing a quiet
   recording does not reproduce the same acoustic failure mode.
5. **Get non-synthetic accented-EN speakers.** TTS-voice "accents" are not a
   substitute — they're the wrong failure mode (synthetic prosody, not real
   L2/regional phonology).
6. **Run the actual engines** (see matrix below) and produce a
   `Map<clipId, hypothesisText>` per (engine, config) to pass into
   `corpusWer()` / `hardSliceRecall()`. This harness deliberately has no
   engine-calling code in it — wiring `sherpa-onnx`/`ort`/Groq/etc. is other
   units' scope (voice-core, cloud escalation client) and out of bounds for
   this run (no native/heavy deps, no model downloads, no network — see this
   run's dispatch).

Until all of the above happens, every WER/recall number this harness could
print is **synthetic-input noise**, not a bench result. Do not report a
number from `manifest.example.json` — `realClips()` filters placeholder
entries out specifically so a bench script can't accidentally score them,
but nothing stops a human from bypassing that by hand.

## Which engines V0.0 must bench, and why (SPEC §4, row V0.0)

| Engine | Why it's in the bench |
|---|---|
| **Nemotron-streaming** (local) | Candidate default local ASR; ships built-in punctuation/ITN in the 3.5 variant (SPEC §3.4), which could shrink the deterministic-normalizer step. |
| **Parakeet-v3** (local) | Transducer architecture — the only family that supports SPEC §3.3 bias layer 1 (sherpa-onnx Aho-Corasick hotwords + `modified_beam_search`). Its decode-latency cost under hotwords is exactly what V0.0 must measure against the partial-latency budget (SPEC §7: "if partials slip past 300 ms, hotwords apply to finalize-only"). |
| **Moonshine v2** (local) | Smaller-footprint candidate; relevant to the "resident footprint" kill criterion (RAM/battery/thermals per tier, SPEC §7). |
| **whisper-turbo** (local) | Incumbent open baseline every other local candidate is implicitly compared against; also the fallback path if a candidate's license or footprint disqualifies it. |
| **Groq (cloud)** | The accuracy ceiling the 90%-of-cloud hard-slice-recall kill criterion (SPEC §7) is measured against. Local is not compared to itself — it's compared to what Pro escalation would have gotten. |

**Run matrix, not just engine list.** Per SPEC §7 and §3.3, each engine
should be run in at least these configs so the bias pipeline's *marginal*
contribution is separable from the base acoustic model:
- bias off (layers 1+2+3 disabled) — the raw-model floor.
- layer 2 only (deterministic phonetic post-correction; the "every engine"
  layer — applies even to non-transducer engines like whisper-turbo).
- layers 1+2 (decode-time hotwords + phonetic correction; transducer engines
  only — Parakeet-v3, Nemotron-streaming).
- Groq cloud, as the comparison ceiling for the 90%-of-cloud recall bar.

Also in scope for the same bench week (SPEC §4 row V0.0, not duplicated as
separate fixtures here): Nemotron-3.5 / Parakeet-unified punctuation variants
bake-off, and CF Workers AI vs. Groq for the cloud leg. Those are
engine/infra comparisons, not corpus/scoring concerns — this directory's
job is to make sure whichever engine runs, the same fixtures and the same
scoring function produce the number.

## Hard-slice tags, and what each one is actually testing

- **`proper-noun`** — names/places/orgs with no reliable spelling-to-sound
  mapping (the "Siobhan" case). This is the archetype of SPEC §3.3's
  bias-term-mismatch trigger: confidently wrong, not low-confidence wrong.
- **`code-identifier`** — camelCase/snake_case/dotted identifiers dictated
  either as natural words ("use effect") or as literal spoken punctuation
  ("cursor dot ai"). Tests bias layer 2's literal substitution rules.
- **`accented-en`** — non-US-English accents. Isolate from bias terms where
  possible (see `accented-en-01` vs `accented-en-02` in the example
  manifest) so acoustic-model robustness and bias-pipeline lift are measured
  separately, not conflated.
- **`whispered`** — no F0; a distinct acoustic regime, not just "quiet."

## How the §7 kill criteria consume these numbers

From `docs/voice/SPEC.md` §7 (quoted context, not duplicated logic — read
the source for the full text):

- **Hard-slice recall kill criterion**: "if local term recall < 90% of
  cloud's on the V0.0 bench after tuning all three layers, local-first fails
  as a *default*." This is exactly `hardSliceRecall()`'s output, computed
  once for the local engine (all layers tuned) and once for Groq, then
  compared as a ratio per tag.
- **Hotwords-vs-latency adjust criterion**: "if partials slip past 300 ms,
  hotwords apply to finalize-only." This harness does NOT measure latency
  (that needs real hardware + a running engine) — it only scores accuracy.
  Latency measurement is a separate, still-unbuilt piece of the V0.0 bench
  script that calls the engines directly.
- **General-traffic WER is explicitly called out as a decoy**: "General-
  traffic WER will look fine and hide this; the eval slice must be the hard
  cases." `corpusWer()` gives you the overall number; `hardSliceRecall()` is
  the number that actually gates the decision. Report both, but the kill
  criterion reads the second one.

## How to run this

From the repo root (no new dependencies — uses the root `vitest` /
`typescript` devDependencies already in `package.json`):

```sh
# unit tests for the scoring harness itself
npx vitest run fixtures/voice/wer.test.ts

# typecheck (fixtures/voice/tsconfig.json extends the repo's tsconfig.base.json)
npx tsc --noEmit -p fixtures/voice/tsconfig.json
```

To score a real bench run, import from `wer.ts` in a small script (not
checked in here — it doesn't exist yet because there's no engine output to
feed it):

```ts
import { realClips, corpusWer, hardSliceRecall, formatResultsTable } from "./wer";
// import type { EvalManifest } from "./wer";

const manifest: EvalManifest = JSON.parse(readFileSync("manifest.json", "utf8"));
const clips = realClips(manifest); // placeholders excluded automatically
const hypotheses = new Map<string, string>(/* clipId -> engine output */);
const { microWer } = corpusWer(clips, hypotheses);
const recall = hardSliceRecall(clips, hypotheses);
console.log(formatResultsTable([{ engineName: "parakeet-v3", configLabel: "local, bias 1+2", microWer, hardSliceRecall: recall }]));
```

When the real corpus exists, name it `manifest.json` (sibling to
`manifest.example.json`, which should stay as the documented worked example
and never be treated as data).
