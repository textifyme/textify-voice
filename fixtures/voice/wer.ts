/**
 * WP-V0.0 scoring harness (docs/voice/SPEC.md §7, §2 "Week 1-2 (bench, WP-V0.0)").
 *
 * Pure, dependency-free scoring: word error rate, per-hard-slice bias-term
 * recall, and a results-table formatter. This module does NOT run any ASR
 * engine — it scores hypothesis transcripts a human (or a bench script that
 * calls the real engines, out of scope for this unit) already produced.
 * See README.md for exactly what's still missing to get real numbers.
 */

// ---------------------------------------------------------------------------
// Manifest types (mirrors manifest.schema.json — keep the two in sync by hand,
// this package has no JSON-schema-to-TS codegen step and adding one is not
// justified for a handful of fields).
// ---------------------------------------------------------------------------

/** SPEC §7 / WP-V0.0 hard-slice categories. */
export type HardSliceTag = "proper-noun" | "code-identifier" | "accented-en" | "whispered";

export interface BiasTermEntry {
  term: string;
  kind?: "proper-noun" | "code-identifier" | "app-name" | "person-name" | "other";
}

export interface EvalClip {
  id: string;
  audio_path: string;
  reference_text: string;
  language: string;
  hard_slice_tags: HardSliceTag[];
  bias_terms: BiasTermEntry[];
  speaker_accent?: string;
  duration_s?: number;
  placeholder: boolean;
  notes?: string;
}

export interface EvalManifest {
  corpus_version: string;
  notes?: string;
  clips: EvalClip[];
}

/** Drop placeholder clips — they have no audio and must never contribute to a bench number. */
export function realClips(manifest: EvalManifest): EvalClip[] {
  return manifest.clips.filter((c) => !c.placeholder);
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/**
 * Lowercase, strip punctuation (keeping internal apostrophes/hyphens so
 * "don't" and "cursor.ai"-as-dictated survive as one token where relevant),
 * collapse whitespace, and split into words. This is deliberately the SAME
 * normalization for both WER tokens and bias-term lookups, so a term match
 * and a WER-correct word always agree on what "correct" means.
 */
export function normalizeWords(text: string): string[] {
  return text
    .toLowerCase()
    .normalize("NFKC")
    .replace(/[’]/g, "'")
    .replace(/[^\p{L}\p{N}\s'.-]/gu, " ")
    .split(/\s+/)
    .map((w) => w.replace(/^['.-]+|['.-]+$/g, ""))
    .filter((w) => w.length > 0);
}

// ---------------------------------------------------------------------------
// Word Error Rate (NIST-style alignment: substitution/insertion/deletion each
// cost 1, computed from scratch as a classic edit-distance DP with backtrace
// so S/D/I are individually reported, not just the total distance).
// ---------------------------------------------------------------------------

export interface WerScore {
  /** substitutions + deletions + insertions, over reference word count. */
  wer: number;
  substitutions: number;
  deletions: number;
  insertions: number;
  refWordCount: number;
  hypWordCount: number;
}

/**
 * Word error rate between a reference and hypothesis string.
 * `wer` is unbounded above 1.0 when insertions dominate a short reference —
 * that is correct WER behaviour, not a bug (a 3-word reference can score
 * WER > 1 if the hypothesis rambles).
 */
export function computeWer(referenceText: string, hypothesisText: string): WerScore {
  const ref = normalizeWords(referenceText);
  const hyp = normalizeWords(hypothesisText);
  const { substitutions, deletions, insertions } = alignWords(ref, hyp);
  const refWordCount = ref.length;
  // Degenerate case: empty reference. WER is 0 only if the hypothesis is also
  // empty; any hypothesis word against zero reference words is all-insertion
  // and conventionally scored as WER 1.0 (not divide-by-zero).
  const wer = refWordCount === 0 ? (hyp.length === 0 ? 0 : 1) : (substitutions + deletions + insertions) / refWordCount;
  return { wer, substitutions, deletions, insertions, refWordCount, hypWordCount: hyp.length };
}

interface AlignCounts {
  substitutions: number;
  deletions: number;
  insertions: number;
}

/**
 * Classic edit-distance DP with backtrace, computed here from scratch
 * (per the run's rules: own small classic algorithms rather than depend on
 * a package). dp[i][j] = min edits to turn ref[0..i) into hyp[0..j).
 */
function alignWords(ref: string[], hyp: string[]): AlignCounts {
  const n = ref.length;
  const m = hyp.length;
  // dp[i][j]: min edit distance; op[i][j]: which move was chosen, for backtrace.
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  const op: Uint8Array[] = Array.from({ length: n + 1 }, () => new Uint8Array(m + 1));
  // op codes: 0 = start, 1 = match/sub (diagonal), 2 = deletion (up), 3 = insertion (left)
  for (let i = 1; i <= n; i++) {
    dp[i][0] = i;
    op[i][0] = 2;
  }
  for (let j = 1; j <= m; j++) {
    dp[0][j] = j;
    op[0][j] = 3;
  }
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      const costSub = ref[i - 1] === hyp[j - 1] ? 0 : 1;
      const diag = dp[i - 1][j - 1] + costSub;
      const up = dp[i - 1][j] + 1; // deletion (ref word missing from hyp)
      const left = dp[i][j - 1] + 1; // insertion (extra hyp word)
      let best = diag;
      let bestOp = 1;
      if (up < best) {
        best = up;
        bestOp = 2;
      }
      if (left < best) {
        best = left;
        bestOp = 3;
      }
      dp[i][j] = best;
      op[i][j] = bestOp;
    }
  }
  let i = n;
  let j = m;
  let substitutions = 0;
  let deletions = 0;
  let insertions = 0;
  while (i > 0 || j > 0) {
    const code = op[i][j];
    if (i > 0 && j > 0 && code === 1) {
      if (ref[i - 1] !== hyp[j - 1]) substitutions++;
      i--;
      j--;
    } else if (i > 0 && (j === 0 || code === 2)) {
      deletions++;
      i--;
    } else {
      insertions++;
      j--;
    }
  }
  return { substitutions, deletions, insertions };
}

/**
 * Corpus-level WER, micro-averaged (sum of edits / sum of reference words —
 * the standard NIST convention; a few long clips don't get drowned out by
 * many short ones the way a macro-average of per-clip WER would).
 */
export function corpusWer(
  clips: EvalClip[],
  hypotheses: ReadonlyMap<string, string>,
): { microWer: number; perClip: Map<string, WerScore> } {
  let totalEdits = 0;
  let totalRefWords = 0;
  const perClip = new Map<string, WerScore>();
  for (const clip of clips) {
    const hyp = hypotheses.get(clip.id);
    if (hyp === undefined) continue; // no hypothesis supplied for this clip — skip, don't fabricate a score
    const score = computeWer(clip.reference_text, hyp);
    perClip.set(clip.id, score);
    totalEdits += score.substitutions + score.deletions + score.insertions;
    totalRefWords += score.refWordCount;
  }
  const microWer = totalRefWords === 0 ? 0 : totalEdits / totalRefWords;
  return { microWer, perClip };
}

// ---------------------------------------------------------------------------
// Hard-slice bias-term recall (SPEC §7: "the eval slice must be the hard
// cases" — this measures, per tag, whether the SPECIFIC bias terms in that
// slice made it into the hypothesis, not just overall WER).
// ---------------------------------------------------------------------------

export interface TermRecallDetail {
  clipId: string;
  term: string;
  found: boolean;
}

export interface HardSliceRecallResult {
  tag: HardSliceTag;
  termsTotal: number;
  termsFound: number;
  recall: number;
  details: TermRecallDetail[];
}

/**
 * Does `term` (possibly multi-word, e.g. "postgres://localhost:5432/myapp_dev")
 * appear in the hypothesis as a contiguous normalized-word subsequence?
 * Contiguous-subsequence match (not substring-of-raw-text) so word-boundary
 * differences in punctuation/casing don't produce false negatives, and a
 * term that only partially appears (one of three words) doesn't count.
 */
export function termFoundInHypothesis(term: string, hypothesisText: string): boolean {
  const termWords = normalizeWords(term);
  if (termWords.length === 0) return false;
  const hypWords = normalizeWords(hypothesisText);
  outer: for (let start = 0; start <= hypWords.length - termWords.length; start++) {
    for (let k = 0; k < termWords.length; k++) {
      if (hypWords[start + k] !== termWords[k]) continue outer;
    }
    return true;
  }
  return false;
}

/**
 * Per-hard-slice-tag recall across a corpus: for every clip carrying `tag`,
 * every one of its bias_terms is checked against the supplied hypothesis.
 * A clip contributes to EVERY tag it's tagged with (a clip tagged
 * ["accented-en","proper-noun"] counts its terms toward both rows) — that's
 * intentional, it's exactly the intersection SPEC §7 flags as the worst case.
 */
export function hardSliceRecall(
  clips: EvalClip[],
  hypotheses: ReadonlyMap<string, string>,
): HardSliceRecallResult[] {
  const byTag = new Map<HardSliceTag, TermRecallDetail[]>();
  for (const clip of clips) {
    const hyp = hypotheses.get(clip.id);
    if (hyp === undefined) continue;
    if (clip.bias_terms.length === 0) continue;
    for (const tag of clip.hard_slice_tags) {
      const list = byTag.get(tag) ?? [];
      for (const bt of clip.bias_terms) {
        list.push({ clipId: clip.id, term: bt.term, found: termFoundInHypothesis(bt.term, hyp) });
      }
      byTag.set(tag, list);
    }
  }
  const tags: HardSliceTag[] = ["proper-noun", "code-identifier", "accented-en", "whispered"];
  return tags
    .filter((t) => byTag.has(t))
    .map((tag) => {
      const details = byTag.get(tag)!;
      const termsFound = details.filter((d) => d.found).length;
      return { tag, termsTotal: details.length, termsFound, recall: details.length === 0 ? 0 : termsFound / details.length, details };
    });
}

// ---------------------------------------------------------------------------
// Results table formatter
// ---------------------------------------------------------------------------

export interface EngineRunResult {
  engineName: string;
  /** e.g. "local, bias layers 1+2 on" / "cloud (Groq)" / "local, bias off" — SPEC §7 run matrix. */
  configLabel: string;
  microWer: number;
  hardSliceRecall: HardSliceRecallResult[];
}

/** Markdown table: one row per (engine, config), one column per hard-slice tag's recall + overall WER. */
export function formatResultsTable(results: EngineRunResult[]): string {
  const tags: HardSliceTag[] = ["proper-noun", "code-identifier", "accented-en", "whispered"];
  const header = ["Engine", "Config", "WER", ...tags.map((t) => `${t} recall`)];
  const sep = header.map(() => "---");
  const rows = results.map((r) => {
    const recallByTag = new Map(r.hardSliceRecall.map((h) => [h.tag, h] as const));
    const cells = tags.map((t) => {
      const h = recallByTag.get(t);
      return h ? `${(h.recall * 100).toFixed(1)}% (${h.termsFound}/${h.termsTotal})` : "n/a";
    });
    return [r.engineName, r.configLabel, `${(r.microWer * 100).toFixed(1)}%`, ...cells];
  });
  const lines = [header, sep, ...rows].map((row) => `| ${row.join(" | ")} |`);
  return lines.join("\n");
}
