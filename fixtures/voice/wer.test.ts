import { describe, expect, it } from "vitest";
import {
  computeWer,
  corpusWer,
  formatResultsTable,
  hardSliceRecall,
  normalizeWords,
  realClips,
  termFoundInHypothesis,
  type EvalClip,
  type EvalManifest,
} from "./wer";

describe("normalizeWords", () => {
  it("lowercases, strips punctuation, collapses whitespace", () => {
    expect(normalizeWords("Hello,   World!!")).toEqual(["hello", "world"]);
  });

  it("keeps internal apostrophes and dotted identifiers, drops leading/trailing punctuation", () => {
    expect(normalizeWords("Don't go to cursor.ai, please.")).toEqual(["don't", "go", "to", "cursor.ai", "please"]);
  });

  it("returns an empty array for whitespace/punctuation-only input", () => {
    expect(normalizeWords("   ...  !! ")).toEqual([]);
  });
});

describe("computeWer — hand-computed cases", () => {
  it("scores identical text as 0 WER", () => {
    const r = computeWer("the quick brown fox", "the quick brown fox");
    expect(r).toMatchObject({ wer: 0, substitutions: 0, deletions: 0, insertions: 0, refWordCount: 4 });
  });

  it("scores one substitution correctly: 1/9 words wrong", () => {
    // ref:  the quick brown fox jumps over the lazy dog   (9 words)
    // hyp:  the quick brown fox jumps over the lazy cat    (dog -> cat)
    const ref = "the quick brown fox jumps over the lazy dog";
    const hyp = "the quick brown fox jumps over the lazy cat";
    const r = computeWer(ref, hyp);
    expect(r.substitutions).toBe(1);
    expect(r.deletions).toBe(0);
    expect(r.insertions).toBe(0);
    expect(r.refWordCount).toBe(9);
    expect(r.wer).toBeCloseTo(1 / 9, 10);
  });

  it("scores one deletion correctly (hypothesis drops a word)", () => {
    // ref: "please call me tomorrow at noon" (6 words) -> hyp drops "at"
    const r = computeWer("please call me tomorrow at noon", "please call me tomorrow noon");
    expect(r.deletions).toBe(1);
    expect(r.substitutions).toBe(0);
    expect(r.insertions).toBe(0);
    expect(r.wer).toBeCloseTo(1 / 6, 10);
  });

  it("scores one insertion correctly (hypothesis adds a stray word)", () => {
    // ref: "open the door" (3 words) -> hyp: "open up the door" (extra "up")
    const r = computeWer("open the door", "open up the door");
    expect(r.insertions).toBe(1);
    expect(r.substitutions).toBe(0);
    expect(r.deletions).toBe(0);
    expect(r.refWordCount).toBe(3);
    expect(r.wer).toBeCloseTo(1 / 3, 10);
  });

  it("scores a mix of sub + deletion + insertion (classic WER textbook example)", () => {
    // ref: "i saw a girl with a telescope" (7 words)
    // hyp: "i saw the girl with telescope"  -> "a"->"the" (sub), "a" deleted, "telescope" unchanged
    // Alignment: i saw [a->the] girl with [a-> deleted] telescope
    const ref = "i saw a girl with a telescope";
    const hyp = "i saw the girl with telescope";
    const r = computeWer(ref, hyp);
    expect(r.substitutions + r.deletions + r.insertions).toBe(2);
    expect(r.refWordCount).toBe(7);
    expect(r.wer).toBeCloseTo(2 / 7, 10);
  });

  it("scores completely disjoint text as WER 1.0 when word counts match", () => {
    const r = computeWer("alpha beta gamma", "delta epsilon zeta");
    expect(r.substitutions).toBe(3);
    expect(r.wer).toBeCloseTo(1, 10);
  });

  it("scores an all-insertion hypothesis against empty reference as WER 1.0", () => {
    const r = computeWer("", "hello there");
    expect(r.refWordCount).toBe(0);
    expect(r.wer).toBe(1);
  });

  it("scores empty vs empty as WER 0", () => {
    const r = computeWer("", "");
    expect(r.wer).toBe(0);
  });

  it("is case- and punctuation-insensitive (normalization applies to both sides)", () => {
    const r = computeWer("Hello, World!", "hello world");
    expect(r.wer).toBe(0);
  });

  it("can exceed 1.0 when insertions dominate a short reference", () => {
    // ref: "yes" (1 word), hyp: "yes yes yes yes" (3 extra words) -> WER = 3/1 = 3.0
    const r = computeWer("yes", "yes yes yes yes");
    expect(r.insertions).toBe(3);
    expect(r.wer).toBeCloseTo(3, 10);
  });
});

describe("corpusWer — micro-average across clips, skip-missing-hypothesis behaviour", () => {
  const clips: EvalClip[] = [
    {
      id: "a",
      audio_path: "audio/a.wav",
      reference_text: "the quick brown fox", // 4 words
      language: "en-US",
      hard_slice_tags: [],
      bias_terms: [],
      placeholder: false,
    },
    {
      id: "b",
      audio_path: "audio/b.wav",
      reference_text: "jumps over the lazy dog", // 5 words
      language: "en-US",
      hard_slice_tags: [],
      bias_terms: [],
      placeholder: false,
    },
  ];

  it("micro-averages: sum of edits over sum of ref words, not mean of per-clip WER", () => {
    // clip a: 1 substitution out of 4 ref words
    // clip b: 1 substitution out of 5 ref words
    // micro WER = (1+1) / (4+5) = 2/9, NOT the mean of (1/4, 1/5) = 0.225
    const hyps = new Map([
      ["a", "the quick brown cat"],
      ["b", "jumps over the lazy fox"],
    ]);
    const { microWer, perClip } = corpusWer(clips, hyps);
    expect(microWer).toBeCloseTo(2 / 9, 10);
    expect(perClip.get("a")!.wer).toBeCloseTo(1 / 4, 10);
    expect(perClip.get("b")!.wer).toBeCloseTo(1 / 5, 10);
  });

  it("skips clips with no supplied hypothesis rather than fabricating a score", () => {
    const hyps = new Map([["a", "the quick brown fox"]]); // no "b"
    const { microWer, perClip } = corpusWer(clips, hyps);
    expect(perClip.has("b")).toBe(false);
    expect(microWer).toBe(0); // only "a" scored, and it's a perfect match
  });
});

describe("termFoundInHypothesis", () => {
  it("finds a single-word term case/punctuation-insensitively", () => {
    expect(termFoundInHypothesis("Postgres", "I set up postgres yesterday")).toBe(true);
    expect(termFoundInHypothesis("Postgres", "I set up mysql yesterday")).toBe(false);
  });

  it("requires the full multi-word term as a contiguous subsequence", () => {
    expect(termFoundInHypothesis("cursor.ai", "go check out cursor.ai today")).toBe(true);
    // "cursor" and "ai" present but not contiguous/adjacent -> not a match
    expect(termFoundInHypothesis("cursor.ai", "the cursor moved, then ai took over")).toBe(false);
  });

  it("does not partial-credit a multi-word term when only some words are present", () => {
    expect(termFoundInHypothesis("DATABASE_URL", "set the database to something else")).toBe(false);
  });

  it("returns false for an empty term", () => {
    expect(termFoundInHypothesis("", "anything at all")).toBe(false);
  });

  it("normalizes underscores to word breaks on both sides, so 'DATABASE_URL' matches 'database url'", () => {
    // Deliberate: normalizeWords() treats '_' as a separator (it's outside the
    // kept-char class), the same on the term and the hypothesis. This means
    // an ASR hypothesis that recovers the WORDS but not the snake_case
    // formatting still counts as a bias-term hit here — the deterministic
    // normalizer (SPEC 3.4) is expected to re-glue casing/underscores
    // separately; this recall metric is about lexical recovery, not
    // formatting fidelity. A hypothesis missing either word entirely still
    // correctly misses.
    expect(termFoundInHypothesis("DATABASE_URL", "check the database url please")).toBe(true);
    expect(termFoundInHypothesis("DATABASE_URL", "check the db connection string")).toBe(false);
  });
});

describe("hardSliceRecall", () => {
  const clips: EvalClip[] = [
    {
      id: "pn1",
      audio_path: "audio/pn1.wav",
      reference_text: "ping Siobhan about the rollout",
      language: "en-US",
      hard_slice_tags: ["proper-noun"],
      bias_terms: [{ term: "Siobhan", kind: "person-name" }],
      placeholder: false,
    },
    {
      id: "pn2",
      audio_path: "audio/pn2.wav",
      reference_text: "loop in Nguyen and Xiaoyu",
      language: "en-IN",
      hard_slice_tags: ["proper-noun", "accented-en"],
      bias_terms: [
        { term: "Nguyen", kind: "person-name" },
        { term: "Xiaoyu", kind: "person-name" },
      ],
      placeholder: false,
    },
    {
      id: "code1",
      audio_path: "audio/code1.wav",
      reference_text: "check the DATABASE_URL env var",
      language: "en-US",
      hard_slice_tags: ["code-identifier"],
      bias_terms: [{ term: "DATABASE_URL", kind: "code-identifier" }],
      placeholder: false,
    },
  ];

  it("computes per-tag recall and lets one clip's terms count toward every tag it's stacked with", () => {
    const hyps = new Map([
      ["pn1", "ping shavon about the rollout"], // Siobhan mis-recognized -> miss
      ["pn2", "loop in Nguyen and Xiaoyu"], // both correct -> hit, hit
      ["code1", "check the db connection string env var"], // DATABASE_URL not recognized at all -> miss
    ]);
    const results = hardSliceRecall(clips, hyps);
    const byTag = new Map(results.map((r) => [r.tag, r] as const));

    // proper-noun tag draws from pn1 (1 term, miss) + pn2 (2 terms, both hit) = 3 total, 2 found
    expect(byTag.get("proper-noun")).toMatchObject({ termsTotal: 3, termsFound: 2 });
    expect(byTag.get("proper-noun")!.recall).toBeCloseTo(2 / 3, 10);

    // accented-en tag only pn2 is tagged with it -> 2 total, 2 found -> 100%
    expect(byTag.get("accented-en")).toMatchObject({ termsTotal: 2, termsFound: 2, recall: 1 });

    // code-identifier: 1 total, 0 found
    expect(byTag.get("code-identifier")).toMatchObject({ termsTotal: 1, termsFound: 0, recall: 0 });

    // whispered never appears in this fixture set -> absent from results, not a zero row
    expect(byTag.has("whispered")).toBe(false);
  });

  it("omits clips with no hypothesis supplied", () => {
    const hyps = new Map([["pn1", "ping siobhan about the rollout"]]);
    const results = hardSliceRecall(clips, hyps);
    const pn = results.find((r) => r.tag === "proper-noun")!;
    expect(pn.termsTotal).toBe(1); // only pn1's one term, pn2 skipped
    expect(pn.termsFound).toBe(1);
  });
});

describe("realClips", () => {
  it("filters out placeholder clips", () => {
    const manifest: EvalManifest = {
      corpus_version: "test",
      clips: [
        { id: "real", audio_path: "a.wav", reference_text: "hi", language: "en-US", hard_slice_tags: [], bias_terms: [], placeholder: false },
        { id: "fake", audio_path: "b.wav", reference_text: "hi", language: "en-US", hard_slice_tags: [], bias_terms: [], placeholder: true },
      ],
    };
    expect(realClips(manifest).map((c) => c.id)).toEqual(["real"]);
  });
});

describe("formatResultsTable", () => {
  it("renders a markdown table with WER and per-tag recall columns", () => {
    const table = formatResultsTable([
      {
        engineName: "whisper-turbo",
        configLabel: "local, bias 1+2 on",
        microWer: 0.083,
        hardSliceRecall: [{ tag: "proper-noun", termsTotal: 10, termsFound: 8, recall: 0.8, details: [] }],
      },
    ]);
    expect(table).toContain("| Engine | Config | WER | proper-noun recall | code-identifier recall | accented-en recall | whispered recall |");
    expect(table).toContain("whisper-turbo");
    expect(table).toContain("8.3%");
    expect(table).toContain("80.0% (8/10)");
    expect(table).toContain("n/a"); // the three tags with no data for this engine
  });

  it("produces one row per engine/config, in call order", () => {
    const table = formatResultsTable([
      { engineName: "engine-a", configLabel: "local", microWer: 0.1, hardSliceRecall: [] },
      { engineName: "engine-b", configLabel: "cloud", microWer: 0.05, hardSliceRecall: [] },
    ]);
    const lines = table.split("\n");
    expect(lines[2]).toContain("engine-a");
    expect(lines[3]).toContain("engine-b");
  });
});
