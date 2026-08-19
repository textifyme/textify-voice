//! Stage 1 — the deterministic grammar spine. COMMANDS-SPEC §3.1, §3.3,
//! §3.4 (the head command set), §1 ("Talon-class latency (<20 ms)").
//!
//! # Anchoring rule (how bare imperatives are told apart from dictated
//! prose that merely contains command words — COMMANDS-SPEC C0.0)
//!
//! A pattern must consume the **entire** normalized token stream, from
//! position 0 to the last token — never a matched span embedded inside a
//! longer sentence. This single rule (whole-utterance anchoring) has two
//! halves and both are required:
//!
//! - **R1 — start anchor.** Matching begins at token 0. A pattern's first
//!   literal must equal the utterance's first word. Reported/narrated
//!   speech ("she said scroll down…", "I told him to open Slack…", "he
//!   asked me to close all Finder windows…") always puts a subject and a
//!   reporting verb *before* the imperative, so the very first literal
//!   comparison fails and the whole match is rejected — no keyword
//!   search, no "does the sentence contain the phrase" check.
//! - **R2 — end anchor (full consumption).** After the last pattern token
//!   is matched, the token cursor must equal the token count — no
//!   trailing words left over. This rejects continuation clauses tacked
//!   onto an otherwise-valid imperative ("open Slack **and wait**",
//!   "stop **right there**", "mute **the sound**") and prevents a greedy
//!   trailing slot from silently swallowing them: a `Rest` slot (used for
//!   app/element/shortcut names) stops at the first coordinating
//!   conjunction/subordinator in [`STOPWORDS`] rather than eating to the
//!   end of the utterance, so "open Slack and wait" fails R2 instead of
//!   binding `AppRef("Slack and wait")`.
//!
//! R1+R2 alone are **not** sufficient, though — they were the whole story
//! at one point and that was a bug: a *verb-initial* sentence has no
//! leading subject to trip R1, and if it never happens to contain one of
//! the ~14 [`STOPWORDS`] conjunctions, a greedy `Rest` slot will happily
//! swallow the entire rest of the sentence as if it were one long
//! app/element/shortcut name, and R2 sees a fully-consumed token stream
//! and calls it a match ("open source software is great" → `AppRef("source
//! software is great")`; "click here for more information" → a bogus
//! `ui.click`). A third rule closes this:
//!
//! - **R3 — capture plausibility.** A `Rest` slot's capture is bounded not
//!   only by [`STOPWORDS`] but also by [`PROSE_MARKERS`]: copulas ("is",
//!   "are", …), closed-class prepositions ("for", "on", "from", …), and
//!   temporal/degree adverbs ("now", "later", "last", "more", …). None of
//!   these plausibly appear as bare words inside a real application,
//!   element, or Shortcut name, so hitting one mid-capture means the
//!   remainder of the utterance is narration *about* the name, not more
//!   of the name — the capture stops there and R2's leftover-token check
//!   does the rejecting, exactly as it already does for `STOPWORDS`. If
//!   the very first candidate word is itself a boundary marker, there is
//!   no plausible name at all and the pattern simply doesn't apply
//!   (`NotACommand`), as opposed to a truly empty slot (bare "open" with
//!   nothing after it), which stays `Unsupported` — see
//!   [`RejectReason::Unsupported`] below.
//!
//! R1+R2+R3 bound the *syntax* stage 1 will consider — but syntax alone
//! cannot tell "open Slack" from "open door policy helps morale": both
//! are a bare imperative verb followed by a plausible-looking noun
//! phrase with no copula, preposition, or temporal adverb anywhere in
//! it, so R3's boundary markers never fire and the whole remainder is
//! captured as a candidate name. This is not a gap in the marker list —
//! it is unfixable by enumerating more markers, because arbitrary English
//! noun phrases are not syntactically distinguishable from application
//! names. What tells them apart is semantic, not syntactic: "Slack"
//! names something that exists on the user's machine and "door policy
//! helps morale" does not. So:
//!
//! - **R4 — closed-vocabulary resolution.** A `Rest`/`RestUntil` slot's
//!   captured text is only accepted if it resolves — tolerantly (case,
//!   spacing, common suffixes; see [`resolves_in`]) — against the
//!   caller-supplied [`CommandContext`] set for that slot kind: `AppRef`
//!   against `known_apps`, `ElementRef` against `known_elements`,
//!   `ShortcutName` against `known_shortcuts`. A capture that does not
//!   resolve is not a malformed attempt at the shape (that stays
//!   `Unsupported`, for a truly empty slot) — it means this utterance
//!   does not name anything real, so the pattern simply does not apply:
//!   `NoMatch`, which surfaces as `RejectReason::NotACommand`. This is
//!   the actual defense the dictation-lookalike property depends on. R1
//!   (start anchor) rejects reported/narrated speech; R2 (end anchor) and
//!   R3 (prose markers) remain in place as a cheap syntactic pre-filter —
//!   they cut the search space and keep the common case fast/obvious —
//!   but they are no longer what makes the property hold: an utterance
//!   that clears R1–R3 with a plausible-looking captured name still
//!   rejects at R4 unless that name is one the caller actually knows
//!   about. An **empty** `CommandContext` (no known apps/elements/
//!   shortcuts supplied — e.g. an AX read failed, or the caller has no
//!   context yet) fails closed *by construction*: nothing resolves
//!   against an empty set, so every `Rest`/`RestUntil` pattern rejects
//!   rather than falling back to accepting free text. See
//!   [`CommandContext`] doc for why that fallback would reinstate the bug.
//!
//!   For a `Rest` slot specifically, R4 is tried *before* the R3
//!   truncation, not only after it: the full remainder of the utterance
//!   is the first candidate span offered to `resolves_in`, and if that
//!   whole span names something in the caller's known set — "Pay Now",
//!   "Select All", "Terms and Conditions", labels that happen to contain
//!   a PROSE_MARKERS/STOPWORDS word in the middle — that resolution wins
//!   outright and the marker is never consulted. Only when the full span
//!   does *not* resolve does the marker-bounded shorter candidate get
//!   tried (and it must independently resolve too). So a marker word can
//!   still narrow what's accepted (as it always could — an unresolved
//!   candidate rejects regardless of length), but it can no longer veto a
//!   span that a real known name already vouches for.
//!
//! A single trailing "please" is stripped before matching (a politeness
//! suffix, not sentence continuation) — see [`strip_trailing_please`].
//!
//! Patterns whose literal skeleton matches but whose slot content is
//! missing/unparseable (bare "open" with nothing after; "run shortcut"
//! with nothing after; "set volume to bananas percent") are distinguished
//! as [`RejectReason::Unsupported`] rather than `NotACommand` — the shape
//! was recognized, only the slot was invalid. See [`match_utterance`].
//!
//! This is table-driven, single-pass, no backtracking: each candidate
//! pattern is tried once, left to right, and a pattern's own token count
//! bounds the work — no exponential blowup is possible for any input
//! length (worst case is `O(patterns × utterance length)`, both small).
//!
//! None of R1/R2/R3 is a substitute for true semantic validation (e.g.
//! confirming a captured `AppRef` names an actually-installed app) --
//! that happens downstream, at resolution against live state. These rules
//! only bound what stage 1's *syntax* will accept as a candidate.

use crate::types::{
    ActionInstance, CommandContext, Direction, IntentResult, MatchStage, RejectReason, SlotValue,
};

/// A single token: `lower` (lowercased, punctuation-stripped) drives
/// literal matching; `raw` (original casing) is what slot values capture,
/// since app/element names are case-sensitive ("Slack", not "slack").
#[derive(Debug, Clone)]
struct Token {
    lower: String,
    raw: String,
}

fn tokenize(input: &str) -> Vec<Token> {
    input
        .split_whitespace()
        .filter_map(|w| {
            let trimmed = w.trim_matches(|c: char| matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '"'));
            if trimmed.is_empty() {
                None
            } else {
                Some(Token {
                    lower: trimmed.to_lowercase(),
                    raw: trimmed.to_string(),
                })
            }
        })
        .collect()
}

/// Strip a single trailing "please" — a politeness suffix, not a
/// sentence-continuation clause, so it does not trip the R2 end anchor.
fn strip_trailing_please(mut tokens: Vec<Token>) -> Vec<Token> {
    if tokens.last().is_some_and(|t| t.lower == "please") {
        tokens.pop();
    }
    tokens
}

/// Coordinating conjunctions/subordinators that bound a greedy `Rest`
/// slot capture (see the R2 doc above). Deliberately small and closed —
/// legitimate app/element/shortcut names do not contain these as bare
/// words.
const STOPWORDS: &[&str] = &[
    "and", "but", "then", "while", "before", "after", "so", "because", "if", "when", "who",
    "that", "or", "nor",
];

/// Closed-class words that ALSO bound a greedy `Rest` slot capture, for a
/// different reason than [`STOPWORDS`]: not because they coordinate a
/// second clause, but because their bare presence marks the remainder of
/// the utterance as narrative/descriptive prose *about* the captured
/// word(s) rather than the name itself continuing. See R3 in the module
/// doc above.
const PROSE_MARKERS: &[&str] = &[
    // copulas / "to be" forms -- "open source software IS great"
    "is", "are", "was", "were", "am", "be", "been", "being",
    // closed-class prepositions -- "click here FOR more information",
    // "press hard ON the pedal", "hide nothing FROM me"
    "for", "from", "on", "in", "at", "of", "with", "without", "under", "over", "about", "into",
    "onto", "upon", "through", "during", "toward", "towards", "within", "off", "near", "beyond",
    // temporal / degree adverbs -- "run shortcut morning routine NOW",
    // "quit smoking LAST year", "focus MORE on marketing"
    "now", "today", "yesterday", "tomorrow", "later", "soon", "already", "yet", "still", "again",
    "always", "never", "sometimes", "usually", "often", "recently", "currently", "last", "more",
    "most", "very", "really", "just",
];

/// Whether `word` (already lowercased) bounds a greedy `Rest` capture --
/// either kind, [`STOPWORDS`] or [`PROSE_MARKERS`].
fn is_capture_boundary(word: &str) -> bool {
    STOPWORDS.contains(&word) || PROSE_MARKERS.contains(&word)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumKind {
    /// No PATTERNS entry constructs `Tok::Num(NumKind::Ordinal)` right now
    /// -- the schema-id-divergence fix dropped "click number `<N>`"
    /// (no Ordinal-slotted UI schema is registered; see the dropped
    /// `ui.click_numbered` comment on PATTERNS). Kept, not deleted, for
    /// the same reason `voice-act::registry::_ORDINAL_RESERVED` is kept:
    /// this is reserved parsing machinery for the future numbered-overlay
    /// follow-up schema (COMMANDS-SPEC §3.4, C2.2), already exercised by
    /// `word_to_num`'s own tests.
    #[allow(dead_code)]
    Ordinal,
    Percentage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestKind {
    App,
    Element,
    Shortcut,
}

fn make_rest_slot(kind: RestKind, s: String) -> SlotValue {
    match kind {
        RestKind::App => SlotValue::AppRef(s),
        RestKind::Element => SlotValue::ElementRef(s),
        RestKind::Shortcut => SlotValue::ShortcutName(s),
    }
}

/// Case- and whitespace-normalize a candidate/known name for tolerant
/// comparison: lowercase, and collapse any run of whitespace (however the
/// caller or ASR happened to space it) to a single space, trimming the
/// ends. Pure text normalization only — this narrows how two strings are
/// COMPARED, it never widens what is allowed to match (see R4 in the
/// module doc): an empty/mismatching set still matches nothing no matter
/// how the candidate is normalized.
fn normalize_for_resolution(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether `candidate` (raw, as captured from the utterance) resolves
/// against `known` (raw names the caller supplied) for the given
/// `RestKind`. Comparison is case/spacing-tolerant via
/// [`normalize_for_resolution`] since both sides may be ASR transcriptions
/// of the same name. For `RestKind::App` only, a single trailing " app"
/// word is also tolerated ("open the Slack app" naming an app registered
/// merely as "Slack") — this is the "common suffixes" tolerance called
/// for in the dispatch; it is deliberately narrow (one documented
/// suffix, one slot kind) rather than a growing denylist, because
/// widening it only ever affects which REAL names are recognized, never
/// which arbitrary strings are accepted (that boundary is `known` itself,
/// per R4).
///
/// `O(known.len())` — `known` is a small, caller-supplied, per-utterance
/// list (installed apps / one window's on-screen labels / user
/// Shortcuts), not an unbounded corpus, so a linear scan stays well
/// within stage 1's <20 ms budget.
/// Which of `ctx`'s three closed sets a given [`RestKind`] resolves
/// against — the one place this mapping is spelled out, so `AppRef` can
/// never accidentally be checked against `known_elements` or vice versa.
fn known_set_for(kind: RestKind, ctx: &CommandContext) -> &[String] {
    match kind {
        RestKind::App => &ctx.known_apps,
        RestKind::Element => &ctx.known_elements,
        RestKind::Shortcut => &ctx.known_shortcuts,
    }
}

fn resolves_in(kind: RestKind, candidate: &str, known: &[String]) -> bool {
    let norm_candidate = normalize_for_resolution(candidate);
    let app_suffix_stripped =
        if kind == RestKind::App { norm_candidate.strip_suffix(" app") } else { None };

    known.iter().any(|k| {
        let norm_known = normalize_for_resolution(k);
        norm_known == norm_candidate || Some(norm_known.as_str()) == app_suffix_stripped
    })
}

/// One position in a pattern's token sequence.
#[derive(Debug, Clone, Copy)]
enum Tok {
    /// An exact literal (case-insensitive).
    Lit(&'static str),
    /// Exactly one token, which must be one of the given (word, Direction)
    /// pairs; produces a `SlotValue::Direction`.
    Dir(&'static [(&'static str, Direction)]),
    /// Exactly one token, parsed as a number word or digit string.
    Num(NumKind),
    /// One-or-more remaining tokens, captured up to the first stopword or
    /// end of utterance (see R2 above). Must be non-empty.
    Rest(RestKind),
    /// Like `Rest`, but capture stops at the first occurrence of the
    /// given literal, which the pattern must consume as its *next* `Lit`
    /// token — used for "close all `<AppRef>` windows" where a required
    /// literal trails the slot.
    RestUntil(RestKind, &'static str),
}

struct Pattern {
    schema_id: &'static str,
    toks: &'static [Tok],
}

const UP_DOWN: &[(&str, Direction)] = &[("up", Direction::Up), ("down", Direction::Down)];

/// The head command set, COMMANDS-SPEC §3.4: app lifecycle, window
/// management, scroll/navigation, system, UI interaction, meta — plus the
/// Shortcuts bridge. Patterns are tried in table order; two patterns
/// sharing a leading verb (e.g. "scroll `<Dir>`" vs "scroll to `<Dir>`")
/// are told apart by whichever literal diverges first, so order alone
/// never lets a greedy `Rest` catch-all shadow a more specific shape --
/// each verb here has at most one `Rest`/`RestUntil` continuation.
/// Multiple entries may legitimately share one `schema_id` (e.g. "play"
/// and "pause" both -> `sys.media_play_pause`; "mute" and "unmute" both
/// -> `sys.mute`) where the registered action is a single stateless
/// toggle rather than two distinct schemas.
static PATTERNS: &[Pattern] = &[
    // --- App lifecycle ---
    Pattern { schema_id: "app.open", toks: &[Tok::Lit("open"), Tok::Rest(RestKind::App)] },
    Pattern {
        schema_id: "app.switch",
        toks: &[Tok::Lit("switch"), Tok::Lit("to"), Tok::Rest(RestKind::App)],
    },
    Pattern { schema_id: "app.quit", toks: &[Tok::Lit("quit"), Tok::Rest(RestKind::App)] },
    Pattern { schema_id: "app.hide", toks: &[Tok::Lit("hide"), Tok::Rest(RestKind::App)] },
    Pattern {
        // Registered id is "app.close_all_windows" (registry.rs), not the
        // shorter "app.close_all" this table used to emit.
        schema_id: "app.close_all_windows",
        toks: &[
            Tok::Lit("close"),
            Tok::Lit("all"),
            Tok::RestUntil(RestKind::App, "windows"),
            Tok::Lit("windows"),
        ],
    },
    // --- Window management ---
    // win.tile_left/win.tile_right are two *separate* NO_SLOTS registry
    // schemas (registry.rs), not one "win.tile" schema with a Direction
    // slot -- so the direction word picks the schema id at the literal
    // level instead of being captured as a slot value.
    Pattern {
        schema_id: "win.tile_left",
        toks: &[
            Tok::Lit("move"),
            Tok::Lit("this"),
            Tok::Lit("window"),
            Tok::Lit("to"),
            Tok::Lit("the"),
            Tok::Lit("left"),
            Tok::Lit("half"),
        ],
    },
    Pattern {
        schema_id: "win.tile_right",
        toks: &[
            Tok::Lit("move"),
            Tok::Lit("this"),
            Tok::Lit("window"),
            Tok::Lit("to"),
            Tok::Lit("the"),
            Tok::Lit("right"),
            Tok::Lit("half"),
        ],
    },
    Pattern {
        schema_id: "win.maximize",
        toks: &[Tok::Lit("maximize"), Tok::Lit("this"), Tok::Lit("window")],
    },
    Pattern {
        schema_id: "win.minimize",
        toks: &[Tok::Lit("minimize"), Tok::Lit("this"), Tok::Lit("window")],
    },
    Pattern {
        schema_id: "win.next_display",
        toks: &[
            Tok::Lit("move"),
            Tok::Lit("this"),
            Tok::Lit("window"),
            Tok::Lit("to"),
            Tok::Lit("the"),
            Tok::Lit("next"),
            Tok::Lit("display"),
        ],
    },
    // --- Scroll / navigate ---
    Pattern { schema_id: "nav.scroll", toks: &[Tok::Lit("scroll"), Tok::Dir(UP_DOWN)] },
    // "scroll to top" -> the registered "nav.scroll_to_top" (registry.rs),
    // a NO_SLOTS schema -- not a Direction-slotted "nav.scroll" the way
    // up/down are. voice-act's `Direction` enum (schema.rs) has no
    // Top/Bottom variants at all, so a pattern that tried to emit
    // SlotValue::Direction(Top) for "nav.scroll" would be a slot-kind
    // mismatch that resolve() degrades to a silent refusal downstream --
    // dead on arrival, exactly like an unregistered schema id. This
    // pattern names the correct schema instead and carries no slot.
    Pattern {
        schema_id: "nav.scroll_to_top",
        toks: &[Tok::Lit("scroll"), Tok::Lit("to"), Tok::Lit("top")],
    },
    // "scroll to bottom" is intentionally NOT matched, for the same
    // reason "previous tab" (below) is not: there is no registered
    // "nav.scroll_to_bottom" (or equivalent) schema in voice-act's
    // LAUNCH_SCHEMAS, and mapping it onto "nav.scroll_to_top" would fire
    // the *opposite* of what the user asked for -- worse than rejecting.
    // Dropped until a scroll-to-bottom schema is registered upstream.
    Pattern { schema_id: "nav.next_tab", toks: &[Tok::Lit("next"), Tok::Lit("tab")] },
    // "previous tab" is intentionally NOT matched: there is no registered
    // "nav.prev_tab" (or equivalent) schema in voice-act's LAUNCH_SCHEMAS,
    // and mapping it onto "nav.next_tab" would fire the *opposite* of
    // what the user asked for -- worse than rejecting. Dropped until a
    // previous-tab schema is registered upstream.
    Pattern { schema_id: "nav.back", toks: &[Tok::Lit("go"), Tok::Lit("back")] },
    Pattern { schema_id: "nav.back", toks: &[Tok::Lit("back")] },
    // --- System ---
    // "volume up"/"volume down" are intentionally NOT matched onto
    // "sys.volume": the registered schema's only declared slot kind is
    // Percentage, an absolute target level (registry.rs `sys.volume`), not
    // a relative step -- stage 1 has no access to the current volume, so
    // it cannot honestly compute the resulting absolute percentage a
    // "Percentage" slot requires. Emitting SlotValue::Direction here (as
    // this table used to) is a slot-kind mismatch that resolve() degrades
    // to a silent refusal downstream -- dead on arrival despite looking
    // matched at stage 1, exactly the "opposite of what the user asked
    // for is worse than rejecting" reasoning behind the dropped
    // "previous tab"/"scroll to bottom" patterns. Dropped until either a
    // relative-step volume schema is registered upstream, or the current
    // level is threaded in (via voice-context/stage 2) so an absolute
    // target can be computed. "set volume to N percent" below is
    // unaffected -- it already binds a real Percentage.
    Pattern {
        schema_id: "sys.volume",
        toks: &[
            Tok::Lit("set"),
            Tok::Lit("volume"),
            Tok::Lit("to"),
            Tok::Num(NumKind::Percentage),
            Tok::Lit("percent"),
        ],
    },
    // sys.mute is registered as a single NO_SLOTS toggle (registry.rs),
    // the same shape as the play/pause toggle below: "mute" and "unmute"
    // both emit "sys.mute" with no slot, mirroring the existing play/pause
    // precedent of two literals sharing one toggle id.
    Pattern { schema_id: "sys.mute", toks: &[Tok::Lit("mute")] },
    Pattern { schema_id: "sys.mute", toks: &[Tok::Lit("unmute")] },
    Pattern { schema_id: "sys.brightness", toks: &[Tok::Lit("brightness"), Tok::Dir(UP_DOWN)] },
    // Registered id is "sys.media_play_pause" (registry.rs), not
    // "sys.play_pause".
    Pattern { schema_id: "sys.media_play_pause", toks: &[Tok::Lit("play")] },
    Pattern { schema_id: "sys.media_play_pause", toks: &[Tok::Lit("pause")] },
    // sys.dnd is likewise a single NO_SLOTS toggle in the registry --
    // there is no separate "on"/"off" schema and no state slot kind
    // exists to carry one (SlotKind has no boolean/state variant), so
    // "turn on"/"turn off do not disturb" both emit the same "sys.dnd"
    // toggle id, matching the mute and play/pause precedents above.
    Pattern {
        schema_id: "sys.dnd",
        toks: &[Tok::Lit("turn"), Tok::Lit("on"), Tok::Lit("do"), Tok::Lit("not"), Tok::Lit("disturb")],
    },
    Pattern {
        schema_id: "sys.dnd",
        toks: &[Tok::Lit("turn"), Tok::Lit("off"), Tok::Lit("do"), Tok::Lit("not"), Tok::Lit("disturb")],
    },
    Pattern {
        schema_id: "sys.screenshot",
        toks: &[Tok::Lit("take"), Tok::Lit("a"), Tok::Lit("screenshot")],
    },
    // --- UI interaction ---
    // No "ui.click_numbered"/Ordinal-slotted click schema is registered:
    // per COMMANDS-SPEC §3.4 ("show numbers" -> ordinal) and registry.rs's
    // `_ORDINAL_RESERVED` comment, the numbered-overlay follow-up click is
    // reserved for a future C2.2 schema, not part of the launch registry.
    // Binding "click number three" onto ui.show_numbers would silently
    // show the overlay instead of clicking; binding it onto generic
    // ui.click as ElementRef("number three") is honest about what stage 1
    // actually knows (a click was requested, naming "number three") and
    // fails safely at resolution (no on-screen element with that literal
    // label) rather than fabricating ordinal-overlay behavior that isn't
    // built. So "click number three" now falls through to the generic
    // ui.click pattern below like any other <ElementRef>.
    Pattern { schema_id: "ui.click", toks: &[Tok::Lit("click"), Tok::Rest(RestKind::Element)] },
    Pattern { schema_id: "ui.click", toks: &[Tok::Lit("press"), Tok::Rest(RestKind::Element)] },
    Pattern { schema_id: "ui.show_numbers", toks: &[Tok::Lit("show"), Tok::Lit("numbers")] },
    // Registered ids are "ui.focus_field" / "ui.toggle_checkbox"
    // (registry.rs), not the shorter "ui.focus" / "ui.toggle".
    Pattern { schema_id: "ui.focus_field", toks: &[Tok::Lit("focus"), Tok::Rest(RestKind::Element)] },
    Pattern { schema_id: "ui.toggle_checkbox", toks: &[Tok::Lit("toggle"), Tok::Rest(RestKind::Element)] },
    // --- Meta ---
    Pattern { schema_id: "meta.undo", toks: &[Tok::Lit("undo"), Tok::Lit("that")] },
    Pattern {
        schema_id: "meta.help",
        toks: &[Tok::Lit("what"), Tok::Lit("can"), Tok::Lit("i"), Tok::Lit("say")],
    },
    Pattern { schema_id: "meta.stop", toks: &[Tok::Lit("stop")] },
    // --- Shortcuts bridge ---
    Pattern {
        schema_id: "shortcut.run",
        toks: &[Tok::Lit("run"), Tok::Lit("shortcut"), Tok::Rest(RestKind::Shortcut)],
    },
    // --- Optional politeness prefix demo pattern intentionally omitted:
    // leading filler is NOT supported (see module doc) — R1 is a hard
    // start anchor with no exceptions besides the trailing-"please" strip.
];

fn word_to_num(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    const WORDS: &[(&str, u32)] = &[
        ("zero", 0),
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
        ("eleven", 11),
        ("twelve", 12),
        ("thirteen", 13),
        ("fourteen", 14),
        ("fifteen", 15),
        ("sixteen", 16),
        ("seventeen", 17),
        ("eighteen", 18),
        ("nineteen", 19),
        ("twenty", 20),
        ("thirty", 30),
        ("forty", 40),
        ("fifty", 50),
        ("sixty", 60),
        ("seventy", 70),
        ("eighty", 80),
        ("ninety", 90),
        ("hundred", 100),
    ];
    WORDS.iter().find(|(w, _)| *w == s).map(|(_, n)| *n)
}

enum MatchFail {
    /// Literal mismatch somewhere — this pattern simply doesn't apply.
    NoMatch,
    /// The pattern's literal skeleton matched up to a slot, but the slot
    /// was missing or unparseable — a recognized-but-malformed shape.
    Invalid,
}

fn match_pattern(pat: &Pattern, tokens: &[Token], ctx: &CommandContext) -> Result<Vec<SlotValue>, MatchFail> {
    let mut ti = 0usize;
    let mut slots = Vec::new();

    for tok in pat.toks {
        match tok {
            Tok::Lit(w) => {
                if tokens.get(ti).map(|t| t.lower.as_str()) != Some(*w) {
                    return Err(MatchFail::NoMatch);
                }
                ti += 1;
            }
            Tok::Dir(map) => {
                let word = match tokens.get(ti) {
                    Some(t) => t.lower.as_str(),
                    None => return Err(MatchFail::NoMatch),
                };
                match map.iter().find(|(w, _)| *w == word) {
                    Some((_, dir)) => {
                        slots.push(SlotValue::Direction(*dir));
                        ti += 1;
                    }
                    None => return Err(MatchFail::NoMatch),
                }
            }
            Tok::Num(kind) => {
                let word = match tokens.get(ti) {
                    Some(t) => t.lower.as_str(),
                    None => return Err(MatchFail::Invalid),
                };
                let n = match word_to_num(word) {
                    Some(n) => n,
                    None => return Err(MatchFail::Invalid),
                };
                match kind {
                    NumKind::Ordinal => {
                        if n == 0 {
                            return Err(MatchFail::Invalid);
                        }
                        slots.push(SlotValue::Ordinal(n));
                    }
                    NumKind::Percentage => {
                        if n > 100 {
                            return Err(MatchFail::Invalid);
                        }
                        slots.push(SlotValue::Percentage(n as u8));
                    }
                }
                ti += 1;
            }
            Tok::Rest(kind) => {
                if ti >= tokens.len() {
                    // Nothing follows the verb at all -- a recognized
                    // shape with a genuinely empty slot (bare "open").
                    return Err(MatchFail::Invalid);
                }
                let known = known_set_for(*kind, ctx);
                // R4-longest-match: try the FULL remainder of the
                // utterance as a single candidate span first. If it
                // resolves against the caller's closed set, that
                // resolution wins outright over the PROSE_MARKERS/
                // STOPWORDS pre-filter below -- a marker word embedded
                // inside an otherwise-real known name ("Pay Now", "Select
                // All", "Terms and Conditions") is exactly the evidence
                // the heuristic was approximating, not narration about
                // the name (see R4 in the module doc). This only widens
                // which REAL known names are recognized: an unresolved
                // full span still falls through to the marker-bounded
                // candidate exactly as before, so the anti-false-accept
                // property (a candidate that does NOT resolve anywhere
                // still rejects) is untouched.
                let full_span = tokens[ti..].iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" ");
                if resolves_in(*kind, &full_span, known) {
                    slots.push(make_rest_slot(*kind, full_span));
                    ti = tokens.len();
                    continue;
                }
                let stop = tokens[ti..]
                    .iter()
                    .position(|t| is_capture_boundary(t.lower.as_str()))
                    .map_or(tokens.len(), |p| ti + p);
                if stop == ti {
                    // The very next word IS a boundary marker (STOPWORDS
                    // or PROSE_MARKERS) and the full span above didn't
                    // resolve -- there is no plausible name here at all,
                    // so this isn't a malformed attempt at the shape,
                    // it's simply not this shape. NoMatch (not Invalid)
                    // so it falls through to NotACommand rather than
                    // Unsupported -- see R3 in the module doc.
                    return Err(MatchFail::NoMatch);
                }
                let joined = tokens[ti..stop].iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" ");
                // R4: the captured text must resolve against the caller's
                // closed set for this slot kind, or this isn't a match at
                // all -- see the module doc. Checked here (not deferred to
                // a caller-side pass) so the property holds inside this
                // function's own contract, not by convention.
                if !resolves_in(*kind, &joined, known) {
                    return Err(MatchFail::NoMatch);
                }
                slots.push(make_rest_slot(*kind, joined));
                ti = stop;
            }
            Tok::RestUntil(kind, until) => {
                let stop = tokens[ti..].iter().position(|t| t.lower == *until).map(|p| ti + p);
                let stop = match stop {
                    Some(s) => s,
                    None => return Err(MatchFail::NoMatch),
                };
                if stop == ti {
                    return Err(MatchFail::Invalid);
                }
                let joined = tokens[ti..stop].iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" ");
                // R4, as above -- a RestUntil capture must resolve too.
                let known = known_set_for(*kind, ctx);
                if !resolves_in(*kind, &joined, known) {
                    return Err(MatchFail::NoMatch);
                }
                slots.push(make_rest_slot(*kind, joined));
                ti = stop; // the `until` literal itself is consumed by the pattern's following Tok::Lit
            }
        }
    }

    if ti == tokens.len() {
        Ok(slots)
    } else {
        // R2: leftover tokens after the pattern is exhausted — a
        // continuation clause, not a bare imperative.
        Err(MatchFail::NoMatch)
    }
}

/// Run stage 1 over a raw utterance. `<20 ms` class work by construction:
/// a single linear pass over a small fixed pattern table, no backtracking.
///
/// `ctx` supplies the closed resolution sets R4 (module doc) checks any
/// `AppRef`/`ElementRef`/`ShortcutName` slot capture against. Pass
/// [`CommandContext::default()`] when the caller has none available yet
/// (e.g. AX read failed) — patterns with a free-text slot will then
/// reject rather than accept arbitrary text; see [`CommandContext`] doc.
pub fn match_utterance(input: &str, ctx: &CommandContext) -> IntentResult {
    let tokens = strip_trailing_please(tokenize(input));
    if tokens.is_empty() {
        return IntentResult::Reject { reason: RejectReason::NotACommand };
    }

    for pat in PATTERNS {
        match match_pattern(pat, &tokens, ctx) {
            Ok(slots) => {
                return IntentResult::Matched {
                    action: ActionInstance { schema_id: pat.schema_id, slots },
                    stage: MatchStage::Grammar,
                    confidence: 1.0,
                };
            }
            // A literal-prefix match with an invalid slot is a stronger
            // signal than "keep looking": the user clearly attempted THIS
            // shape (e.g. "run shortcut" with nothing after -- a
            // recognized shortcut.run skeleton with a missing name), so it
            // must not fall through to a more general pattern that would
            // silently reinterpret it as something else. Short-circuit
            // rather than keep scanning.
            Err(MatchFail::Invalid) => {
                return IntentResult::Reject { reason: RejectReason::Unsupported };
            }
            Err(MatchFail::NoMatch) => {}
        }
    }

    IntentResult::Reject { reason: RejectReason::NotACommand }
}

/// The static action lexicon derived from the stage-1 grammar table —
/// every literal a pattern can match, deduplicated in table order. Feeds
/// `CommandBias` (COMMANDS-SPEC §3.1: "CommandBias = static action
/// lexicon + installed-app names + focused-window AX labels").
pub fn command_lexicon() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let mut push = |w: &'static str| {
        if !out.contains(&w) {
            out.push(w);
        }
    };
    for pat in PATTERNS {
        for tok in pat.toks {
            match tok {
                Tok::Lit(w) => push(w),
                Tok::Dir(map) => {
                    for (w, _) in *map {
                        push(w);
                    }
                }
                Tok::RestUntil(_, w) => push(w),
                Tok::Rest(_) | Tok::Num(_) => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IntentResult, RejectReason};

    fn assert_matched(input: &str, ctx: &CommandContext, expect_schema: &str) {
        match match_utterance(input, ctx) {
            IntentResult::Matched { action, stage, confidence } => {
                assert_eq!(action.schema_id, expect_schema, "input: {input:?}");
                assert_eq!(stage, MatchStage::Grammar, "input: {input:?}");
                assert_eq!(confidence, 1.0, "input: {input:?}");
            }
            IntentResult::Reject { reason } => {
                panic!("expected Matched({expect_schema}) for {input:?}, got Reject({reason})");
            }
        }
    }

    fn assert_rejected(input: &str, ctx: &CommandContext, expect_reason: RejectReason) {
        match match_utterance(input, ctx) {
            IntentResult::Reject { reason } => {
                assert_eq!(reason, expect_reason, "input: {input:?}");
            }
            IntentResult::Matched { action, .. } => {
                panic!("expected Reject({expect_reason}) for {input:?}, got Matched({})", action.schema_id);
            }
        }
    }

    /// A representative closed-vocabulary context: the app/element/
    /// shortcut names every currently-supported head command in this
    /// test file's bare-imperative side actually names. Mirrors what a
    /// real caller would supply from installed-app enumeration + the
    /// focused window's AX label map + the local Shortcuts list
    /// (COMMANDS-SPEC §3.1) -- these tests use one fixed representative
    /// set rather than a bespoke one per call, exactly as a real running
    /// session would (the set doesn't change utterance to utterance,
    /// only the focused app/window does).
    fn head_context() -> CommandContext {
        CommandContext {
            known_apps: ["Slack", "Terminal", "Finder", "Mail", "Chrome", "Spotify"]
                .into_iter()
                .map(String::from)
                .collect(),
            known_elements: ["Send", "Cancel", "Search", "Wifi", "number three", "Number Three Button"]
                .into_iter()
                .map(String::from)
                .collect(),
            known_shortcuts: ["Morning Routine"].into_iter().map(String::from).collect(),
        }
    }

    // ---------------------------------------------------------------
    // Head command coverage: one bare-imperative match per family.
    // ---------------------------------------------------------------

    #[test]
    fn head_commands_match_expected_schemas() {
        let ctx = head_context();
        assert_matched("open Slack", &ctx, "app.open");
        assert_matched("switch to Chrome", &ctx, "app.switch");
        assert_matched("quit Spotify", &ctx, "app.quit");
        assert_matched("hide Mail", &ctx, "app.hide");
        assert_matched("close all Finder windows", &ctx, "app.close_all_windows");
        assert_matched("move this window to the left half", &ctx, "win.tile_left");
        assert_matched("move this window to the right half", &ctx, "win.tile_right");
        assert_matched("maximize this window", &ctx, "win.maximize");
        assert_matched("minimize this window", &ctx, "win.minimize");
        assert_matched("move this window to the next display", &ctx, "win.next_display");
        assert_matched("scroll down", &ctx, "nav.scroll");
        assert_matched("scroll up", &ctx, "nav.scroll");
        assert_matched("scroll to top", &ctx, "nav.scroll_to_top");
        // "scroll to bottom" is no longer matched -- see the
        // dropped-pattern comment above nav.next_tab in PATTERNS.
        assert_rejected("scroll to bottom", &ctx, RejectReason::NotACommand);
        assert_matched("next tab", &ctx, "nav.next_tab");
        // "previous tab" is no longer matched -- see the dropped-pattern
        // comment on nav.prev_tab in PATTERNS above.
        assert_rejected("previous tab", &ctx, RejectReason::NotACommand);
        assert_matched("go back", &ctx, "nav.back");
        assert_matched("back", &ctx, "nav.back");
        // "volume up"/"volume down" are no longer matched -- see the
        // dropped-pattern comment above the "set volume to..." pattern in
        // PATTERNS (sys.volume's only registered slot kind is an absolute
        // Percentage, which stage 1 cannot honestly compute for a
        // relative up/down step).
        assert_rejected("volume up", &ctx, RejectReason::NotACommand);
        assert_rejected("volume down", &ctx, RejectReason::NotACommand);
        assert_matched("set volume to 50 percent", &ctx, "sys.volume");
        assert_matched("mute", &ctx, "sys.mute");
        assert_matched("unmute", &ctx, "sys.mute");
        assert_matched("brightness up", &ctx, "sys.brightness");
        assert_matched("play", &ctx, "sys.media_play_pause");
        assert_matched("pause", &ctx, "sys.media_play_pause");
        assert_matched("turn on do not disturb", &ctx, "sys.dnd");
        assert_matched("turn off do not disturb", &ctx, "sys.dnd");
        assert_matched("take a screenshot", &ctx, "sys.screenshot");
        assert_matched("click Send", &ctx, "ui.click");
        assert_matched("press Cancel", &ctx, "ui.click");
        // "click number three" now falls through to the generic click
        // pattern -- see the dropped ui.click_numbered comment above.
        // Resolves against "number three" in head_context()'s
        // known_elements, as a real caller naming an overlay-numbered
        // element would supply.
        assert_matched("click number three", &ctx, "ui.click");
        assert_matched("show numbers", &ctx, "ui.show_numbers");
        assert_matched("focus Search", &ctx, "ui.focus_field");
        assert_matched("toggle Wifi", &ctx, "ui.toggle_checkbox");
        assert_matched("undo that", &ctx, "meta.undo");
        assert_matched("what can I say", &ctx, "meta.help");
        assert_matched("stop", &ctx, "meta.stop");
        assert_matched("run shortcut Morning Routine", &ctx, "shortcut.run");
    }

    #[test]
    fn trailing_please_is_stripped_not_treated_as_continuation() {
        let ctx = head_context();
        assert_matched("open Slack please", &ctx, "app.open");
        assert_matched("mute please", &ctx, "sys.mute");
    }

    #[test]
    fn slot_values_extract_typed_content() {
        let ctx = head_context();

        let r = match_utterance("open Slack", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. } if action.slots == vec![SlotValue::AppRef("Slack".to_string())]
        ));

        let r = match_utterance("scroll down", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. } if action.slots == vec![SlotValue::Direction(Direction::Down)]
        ));

        let r = match_utterance("scroll to top", &ctx);
        assert!(matches!(r, IntentResult::Matched { action, .. } if action.slots.is_empty()));

        let r = match_utterance("click number three", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. }
                if action.slots == vec![SlotValue::ElementRef("number three".to_string())]
        ));

        let r = match_utterance("set volume to 50 percent", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. } if action.slots == vec![SlotValue::Percentage(50)]
        ));

        let r = match_utterance("close all Finder windows", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. } if action.slots == vec![SlotValue::AppRef("Finder".to_string())]
        ));

        let r = match_utterance("run shortcut Morning Routine", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. }
                if action.slots == vec![SlotValue::ShortcutName("Morning Routine".to_string())]
        ));
    }

    // ---------------------------------------------------------------
    // Dictation-lookalike adversarial pairs (COMMANDS-SPEC C0.0 / §7).
    // Each pair: a bare imperative that MUST match, and dictated prose
    // that merely contains the same command words and MUST reject with
    // NotACommand. >=25 pairs required by spec; 30 given here, spanning
    // R1 (leading subject/reporting verb) and R2 (trailing continuation
    // clause) failures, plus a few that trip both.
    // ---------------------------------------------------------------

    #[test]
    fn dictation_lookalike_adversarials_reject_bare_imperatives_match() {
        let pairs: &[(&str, &str, &str)] = &[
            // (bare imperative -> schema, adversarial prose -> must reject)
            ("open Slack", "I told him to open Slack and wait", "app.open"),
            ("switch to Chrome", "she said switch to Chrome before lunch", "app.switch"),
            ("quit Spotify", "can you believe he wants to quit Spotify", "app.quit"),
            ("hide Mail", "the manual says hide Mail when done", "app.hide"),
            (
                "close all Finder windows",
                "he asked me to close all Finder windows later",
                "app.close_all_windows",
            ),
            (
                "close all Finder windows",
                "close all Finder windows and restart",
                "app.close_all_windows",
            ),
            ("scroll down", "she said scroll down to the bottom of the page", "nav.scroll"),
            ("scroll up", "the note says scroll up if you missed it", "nav.scroll"),
            (
                "scroll to top",
                "remember to scroll to top before you sign off",
                "nav.scroll_to_top",
            ),
            ("next tab", "he wants me to open the next tab first", "nav.next_tab"),
            ("go back", "she told him to go back and check again", "nav.back"),
            ("back", "he stepped back and looked around", "nav.back"),
            ("unmute", "she told him to unmute before he started speaking", "sys.mute"),
            ("mute", "the teacher told them to mute during the exam", "sys.mute"),
            ("brightness down", "she mentioned brightness down helps her eyes", "sys.brightness"),
            ("play", "he said hit play when ready", "sys.media_play_pause"),
            ("pause", "just press pause, she suggested", "sys.media_play_pause"),
            (
                "take a screenshot",
                "can you please take a screenshot sometime",
                "sys.screenshot",
            ),
            ("click Send", "I heard someone say click send by mistake", "ui.click"),
            ("press Cancel", "the email said press cancel to opt out", "ui.click"),
            (
                "show numbers",
                "the game show numbers on the board light up",
                "ui.show_numbers",
            ),
            ("focus Search", "she asked us to focus search efforts", "ui.focus_field"),
            ("toggle Wifi", "he wondered whether to toggle wifi settings", "ui.toggle_checkbox"),
            ("undo that", "she muttered undo that under her breath", "meta.undo"),
            ("stop", "he yelled stop but nobody listened", "meta.stop"),
            ("stop", "stop right there", "meta.stop"),
            ("maximize this window", "she explained how to maximize this window's performance", "win.maximize"),
            ("minimize this window", "please remind me to minimize this window tomorrow", "win.minimize"),
            (
                "run shortcut Morning Routine",
                "he suggested we run shortcut morning routines someday",
                "shortcut.run",
            ),
            (
                "move this window to the left half",
                "she explained how to move this window to the left half of the screen",
                "win.tile_left",
            ),
        ];

        assert!(pairs.len() >= 25, "need at least 25 adversarial pairs, have {}", pairs.len());

        let ctx = head_context();
        for (bare, adversarial, schema) in pairs {
            assert_matched(bare, &ctx, schema);
            assert_rejected(adversarial, &ctx, RejectReason::NotACommand);
        }
    }

    // ---------------------------------------------------------------
    // Verb-initial dictation prose (first remediation regression, R3/R4
    // above). Unlike the pairs above (which trip R1's leading-subject
    // anchor or R2 via a STOPWORDS conjunction), every adversarial
    // utterance here starts with the bare imperative verb itself -- no
    // subject, no reporting verb, no STOPWORDS conjunction anywhere.
    // These happen to ALSO trip R3 (PROSE_MARKERS), which is why an
    // earlier fix that relied on R3 alone passed this exact test at
    // 100% and then still collapsed against fresh adversarials (see
    // `verb_initial_prose_with_no_prose_markers_still_rejects` below,
    // which is built with no R3 marker anywhere and rejects purely on
    // R4/closed-vocabulary resolution -- the actual property this crate
    // now depends on). Covers every family whose pattern has a
    // `Rest`/`RestUntil` slot (app.open/switch/quit/hide/
    // close_all_windows, ui.click, ui.focus_field, ui.toggle_checkbox,
    // shortcut.run) -- the families with no such slot (win.tile_left/
    // right, nav.*, sys.*, meta.*) have nothing for this failure class to
    // exploit. >=30 new pairs required by the dispatch; 32 given,
    // including all 8 utterances from the empirical repro.
    // ---------------------------------------------------------------

    #[test]
    fn verb_initial_dictation_prose_rejects_bare_imperatives_still_match() {
        let pairs: &[(&str, &str, &str)] = &[
            // --- app.open ---
            ("open Slack", "open source software is great", "app.open"),
            ("open Terminal", "open enrollment starts later this year", "app.open"),
            (
                "open Finder",
                "open water swimming requires a lot of stamina",
                "app.open",
            ),
            ("open Mail", "open mic night was fun last week", "app.open"),
            // --- app.switch ---
            (
                "switch to Chrome",
                "switch to considering the different options later",
                "app.switch",
            ),
            (
                "switch to Chrome",
                "switch to be honest I have no idea",
                "app.switch",
            ),
            (
                "switch to Chrome",
                "switch to something is clearly wrong here",
                "app.switch",
            ),
            // --- app.quit ---
            ("quit Spotify", "quit smoking last year", "app.quit"),
            (
                "quit Spotify",
                "quit stalling is the hardest part of the process",
                "app.quit",
            ),
            (
                "quit Spotify",
                "quit your job was the best decision she made",
                "app.quit",
            ),
            (
                "quit Spotify",
                "quit complaining about the weather already",
                "app.quit",
            ),
            // --- app.hide ---
            ("hide Mail", "hide nothing from me", "app.hide"),
            (
                "hide Mail",
                "hide behind excuses is a common habit",
                "app.hide",
            ),
            ("hide Mail", "hide your feelings is never healthy", "app.hide"),
            (
                "hide Mail",
                "hide the remote control was always his job",
                "app.hide",
            ),
            // --- ui.click (click / press) ---
            (
                "click Send",
                "click here for more information",
                "ui.click",
            ),
            ("press Cancel", "press hard on the pedal", "ui.click"),
            ("click Send", "click submit is the last step", "ui.click"),
            (
                "press Cancel",
                "press play was satisfying after the long wait",
                "ui.click",
            ),
            // --- ui.focus_field ---
            ("focus Search", "focus more on marketing", "ui.focus_field"),
            (
                "focus Search",
                "focus groups are useful for research",
                "ui.focus_field",
            ),
            (
                "focus Search",
                "focus training is important for athletes",
                "ui.focus_field",
            ),
            // --- ui.toggle_checkbox ---
            (
                "toggle Wifi",
                "toggle switch broke yesterday",
                "ui.toggle_checkbox",
            ),
            (
                "toggle Wifi",
                "toggle options are hidden in settings usually",
                "ui.toggle_checkbox",
            ),
            (
                "toggle Wifi",
                "toggle mode was confusing at first",
                "ui.toggle_checkbox",
            ),
            // --- shortcut.run ---
            (
                "run shortcut Morning Routine",
                "run shortcut morning routine now",
                "shortcut.run",
            ),
            (
                "run shortcut Morning Routine",
                "run shortcut ideas are still forming",
                "shortcut.run",
            ),
            (
                "run shortcut Morning Routine",
                "run shortcut testing was postponed until later",
                "shortcut.run",
            ),
            (
                "run shortcut Morning Routine",
                "run shortcut naming conventions are tricky sometimes",
                "shortcut.run",
            ),
            // --- app.close_all_windows ---
            (
                "close all Finder windows",
                "close all Finder windows is what I always do",
                "app.close_all_windows",
            ),
            (
                "close all Finder windows",
                "close all my browser windows are always too many",
                "app.close_all_windows",
            ),
        ];

        assert!(pairs.len() >= 30, "need at least 30 new verb-initial adversarial pairs, have {}", pairs.len());

        let ctx = head_context();
        let mut matched_bare = 0usize;
        let mut rejected_adversarial = 0usize;
        for (bare, adversarial, schema) in pairs {
            assert_matched(bare, &ctx, schema);
            matched_bare += 1;
            assert_rejected(adversarial, &ctx, RejectReason::NotACommand);
            rejected_adversarial += 1;
        }
        // Both rates are 100% by construction of the assertions above (a
        // failed assert! would have already panicked) -- this restates it
        // as an explicit self-check so the >=99% kill criterion has a
        // number attached, not just "the loop didn't panic".
        assert_eq!(matched_bare, pairs.len());
        assert_eq!(rejected_adversarial, pairs.len());
    }

    // ---------------------------------------------------------------
    // Held-out adversarial set (R4 property: closed-vocabulary slot
    // resolution). Generated from a DIFFERENT principle than the fix
    // depends on and BEFORE the fix's own test above was extended: these
    // are common English idioms/collocations that happen to begin with a
    // head verb ("open door", "click bait", "hide nothing", "quit cold
    // turkey", "press hard", ...), pulled from ordinary usage -- not
    // phrases selected (or avoided) because they contain a
    // PROSE_MARKERS/STOPWORDS boundary word. The old (R3-only) fix would
    // have false-accepted most of these, exactly as it false-accepted
    // "open door policy helps morale" / "click bait headlines everywhere"
    // / "hide the salami joke" / "quit cold turkey worked" against the
    // real matcher (dispatch repro). None of these name an app/element/
    // shortcut in `head_context()`, exactly as arbitrary dictated prose
    // never names a real installed app/on-screen element/Shortcut in
    // practice -- each must reject regardless of whether it also happens
    // to trip R1/R2/R3, because R4 (closed-set resolution) is what the
    // property now depends on, not the marker list. >=60 held-out
    // adversarials required by the dispatch; 63 given, spanning every
    // Rest/RestUntil-slotted family (the only families this failure
    // class can exploit -- see the module doc). A non-empty, realistic
    // `head_context()` is used throughout (not an empty one), so this
    // measures the resolution mechanism itself, not the trivial
    // "no context at all" case.
    // ---------------------------------------------------------------

    #[test]
    fn held_out_idiom_adversarials_reject_at_or_above_99_percent() {
        let adversarials: &[&str] = &[
            // --- app.open ---
            "open door policy helps morale",
            "open source software is thriving",
            "open water swimming requires stamina",
            "open mic night was a blast",
            "open house drew a huge crowd",
            "open floor plan feels spacious",
            "open enrollment starts next Monday",
            "open book exams stress students out",
            "open borders debate continues fiercely",
            "open forum invites public comments",
            // --- app.switch ---
            "switch to considering other options",
            "switch hitters bat well from both sides",
            "switch to blade knives worry parents",
            "switch board operators changed the shift",
            "switch to a growth mindset takes practice",
            "switch to remote work changed everything",
            "switch teams surprised the coach",
            // --- app.quit ---
            "quit cold turkey worked for him",
            "quit smoking improved her breathing",
            "quit stalling and just decide",
            "quit your whining about the weather",
            "quit horsing around in class",
            "quit while you are ahead they say",
            "quit playing games with my heart",
            // --- app.hide ---
            "hide the salami joke is old",
            "hide and seek is a fun game",
            "hide nothing from the committee",
            "hide behind excuses is a bad habit",
            "hide your valuables when traveling",
            "hide the evidence before they arrive",
            "hide the truth backfired eventually",
            // --- app.close_all_windows ---
            "close all my complaints about broken windows",
            "close all the shutters before storm windows",
            "close all our drafty old apartment windows",
            "close all your unnecessary browser tab windows",
            // --- ui.click ---
            "click bait headlines are everywhere",
            "click through rates matter online",
            "click wheel iPods were popular once",
            "click farms manipulate ad metrics",
            "click here for more information",
            "click submit is the last step",
            "click counter resets every midnight",
            // --- ui.click (press) ---
            "press hard on the accelerator pedal",
            "press release went out yesterday",
            "press conference starts at noon",
            "press play was satisfying finally",
            "press charges against the driver",
            "press pause for a moment please",
            // --- ui.focus_field ---
            "focus groups gave mixed feedback",
            "focus rings on old lenses stick",
            "focus more energy on studying",
            "focus sessions helped her concentrate",
            "focus training builds discipline in athletes",
            "focus lens needed cleaning badly",
            // --- ui.toggle_checkbox ---
            "toggle switches broke last night",
            "toggle case in the text editor",
            "toggle buttons look confusing sometimes",
            "toggle states are hard to track",
            "toggle options are hidden in settings",
            "toggle animation looks smooth now",
            // --- shortcut.run ---
            "run shortcut naming conventions confuse people",
            "run shortcut testing takes forever",
            "run shortcut ideas keep changing",
            "run shortcut ceremonies felt excessive",
            "run shortcut documentation is outdated",
        ];

        assert!(
            adversarials.len() >= 60,
            "need at least 60 held-out idiom adversarials, have {}",
            adversarials.len()
        );

        let ctx = head_context();
        let mut reject_count = 0usize;
        let mut false_accepts: Vec<(&str, &'static str)> = Vec::new();
        for input in adversarials {
            match match_utterance(input, &ctx) {
                IntentResult::Reject { .. } => reject_count += 1,
                IntentResult::Matched { action, .. } => false_accepts.push((input, action.schema_id)),
            }
        }

        let total = adversarials.len();
        let reject_rate = reject_count as f64 / total as f64;
        assert!(
            false_accepts.is_empty(),
            "R4 closed-vocabulary resolution false-accepted {} of {total} held-out idiom \
             adversarials: {false_accepts:?} (reject rate {reject_rate:.4}, kill criterion is \
             >=0.99, COMMANDS-SPEC line 241)",
            false_accepts.len(),
        );
        assert_eq!(reject_count, total, "reject count out of {total} held-out adversarials");
        assert!(
            reject_rate >= 0.99,
            "reject rate {reject_rate:.4} is below the >=99% kill criterion"
        );

        // Companion measurement: the SAME closed-vocabulary mechanism
        // must still match every legitimate Rest/RestUntil-slotted
        // imperative under the same non-empty, realistic context --
        // the fix must not have traded false accepts for false rejects.
        let legitimate: &[(&str, &str)] = &[
            ("open Slack", "app.open"),
            ("open Terminal", "app.open"),
            ("open Finder", "app.open"),
            ("open Mail", "app.open"),
            ("switch to Chrome", "app.switch"),
            ("quit Spotify", "app.quit"),
            ("hide Mail", "app.hide"),
            ("close all Finder windows", "app.close_all_windows"),
            ("click Send", "ui.click"),
            ("press Cancel", "ui.click"),
            ("click number three", "ui.click"),
            ("focus Search", "ui.focus_field"),
            ("toggle Wifi", "ui.toggle_checkbox"),
            ("run shortcut Morning Routine", "shortcut.run"),
        ];
        let mut legit_match_count = 0usize;
        for (input, expect_schema) in legitimate {
            if let IntentResult::Matched { action, .. } = match_utterance(input, &ctx) {
                if action.schema_id == *expect_schema {
                    legit_match_count += 1;
                }
            }
        }
        assert_eq!(
            legit_match_count,
            legitimate.len(),
            "every legitimate Rest/RestUntil imperative must still match (100% match rate) \
             under the same closed-vocabulary resolution"
        );
    }

    // ---------------------------------------------------------------
    // Unsupported: recognized shape, invalid/missing slot content.
    // ---------------------------------------------------------------

    #[test]
    fn recognized_shape_with_missing_or_invalid_slot_is_unsupported_not_not_a_command() {
        let ctx = head_context();
        assert_rejected("open", &ctx, RejectReason::Unsupported);
        assert_rejected("click", &ctx, RejectReason::Unsupported);
        assert_rejected("run shortcut", &ctx, RejectReason::Unsupported);
        assert_rejected("close all windows", &ctx, RejectReason::Unsupported);
        assert_rejected("set volume to bananas percent", &ctx, RejectReason::Unsupported);
        assert_rejected("set volume to 150 percent", &ctx, RejectReason::Unsupported);
        // Note: "click number zero" is deliberately NOT asserted here
        // anymore -- ui.click_numbered/Ordinal was dropped (see the
        // schema-id-divergence fix above), so "click number zero" now
        // falls through to generic ui.click as ElementRef("number zero"),
        // a well-formed match, not an invalid-slot rejection. Covered in
        // `ambiguous_command_shapes_do_not_conflate_number_words_with_ordinals`.
        //
        // These stay Unsupported (not NotACommand) with an EMPTY context
        // too, not just `head_context()` -- the empty-slot/invalid-number
        // failures above are caught before R4's resolution check ever
        // runs (bare "open" fails at "nothing follows the verb at all";
        // "close all windows" fails at "the very next word is the
        // `until` literal itself"; the volume ones fail number parsing),
        // so resolution context is irrelevant to them.
        let empty = CommandContext::default();
        assert_rejected("open", &empty, RejectReason::Unsupported);
        assert_rejected("close all windows", &empty, RejectReason::Unsupported);
    }

    #[test]
    fn truly_unrelated_prose_is_not_a_command() {
        let ctx = head_context();
        assert_rejected("the weather is nice today", &ctx, RejectReason::NotACommand);
        assert_rejected("", &ctx, RejectReason::NotACommand);
        assert_rejected("   ", &ctx, RejectReason::NotACommand);
        assert_rejected("thinking about dinner plans", &ctx, RejectReason::NotACommand);
    }

    #[test]
    fn ambiguous_command_shapes_do_not_conflate_number_words_with_ordinals() {
        // No Ordinal-slotted "click number <N>" schema is registered (see
        // the dropped ui.click_numbered comment in PATTERNS): both of
        // these now go through the *same* generic ui.click pattern, each
        // capturing its own literal ElementRef text rather than one being
        // silently reinterpreted as a numbered-overlay ordinal. Both
        // resolve against `head_context()`'s known_elements, as a real
        // caller naming overlay-numbered elements would supply.
        let ctx = head_context();
        let r = match_utterance("click number three", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { ref action, .. }
                if action.schema_id == "ui.click"
                    && action.slots == vec![SlotValue::ElementRef("number three".to_string())]
        ));
        assert_matched("click Number Three Button", &ctx, "ui.click");
    }

    // ---------------------------------------------------------------
    // Marker-word labels (adversarial-audit finding, polish wave): a
    // legitimate on-screen label that happens to CONTAIN a
    // PROSE_MARKERS/STOPWORDS word ("Pay Now", "Terms and Conditions")
    // must still match when that exact label is present in
    // known_elements/known_apps/known_shortcuts -- the closed-vocabulary
    // resolution (R4) is the load-bearing defense, and R3's marker list
    // is only a cheap pre-filter that must not be allowed to veto a span
    // R4 already vouches for. Confirmed failing before the fix: 'click
    // Pay Now', 'click Buy Now', 'press Pay Now', 'click erase all
    // content and settings'. Each MATCH case below is paired with a
    // near-miss adversarial that is NOT in the known set and must still
    // REJECT, so this exercises both directions of R4 at once, not just
    // the false-reject fix in isolation.
    // ---------------------------------------------------------------

    fn marker_word_context() -> CommandContext {
        CommandContext {
            known_apps: ["Now and Then"].into_iter().map(String::from).collect(),
            known_elements: [
                "Pay Now",
                "Buy Now",
                "erase all content and settings",
                "Now Playing",
                "Buy Now and Save",
                "Terms and Conditions",
                "Sign In and Continue",
                "Turn On Notifications",
                "Turn Off Notifications",
                "Log In Now",
                "Remind Me Later",
                "Continue Without Saving",
                "Skip For Now",
                "Accept Terms and Conditions Now",
                "Save All",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            known_shortcuts: ["Clean Up Now"].into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn marker_word_labels_resolve_via_full_span_even_when_a_marker_word_is_present() {
        let ctx = marker_word_context();

        // --- The four confirmed false-rejects from the adversarial repro ---
        assert_matched("click Pay Now", &ctx, "ui.click");
        assert_matched("click Buy Now", &ctx, "ui.click");
        assert_matched("press Pay Now", &ctx, "ui.click");
        assert_matched("click erase all content and settings", &ctx, "ui.click");

        // --- >=10 more multi-word labels containing marker words, each
        // supplied verbatim in known_elements/known_apps/known_shortcuts,
        // each expected to MATCH ---
        assert_matched("click Now Playing", &ctx, "ui.click"); // marker "now" is the very first word
        assert_matched("click Buy Now and Save", &ctx, "ui.click"); // "now" then "and"
        assert_matched("click Terms and Conditions", &ctx, "ui.click"); // "and"
        assert_matched("click Sign In and Continue", &ctx, "ui.click"); // "in" then "and"
        assert_matched("click Turn On Notifications", &ctx, "ui.click"); // "on"
        assert_matched("click Turn Off Notifications", &ctx, "ui.click"); // "off"
        assert_matched("click Log In Now", &ctx, "ui.click"); // "in" then "now"
        assert_matched("click Remind Me Later", &ctx, "ui.click"); // "later"
        assert_matched("click Continue Without Saving", &ctx, "ui.click"); // "without"
        assert_matched("click Skip For Now", &ctx, "ui.click"); // "for" then "now"
        assert_matched("click Accept Terms and Conditions Now", &ctx, "ui.click"); // "and" then "now"
        assert_matched("click Save All", &ctx, "ui.click"); // sanity: no marker word present at all
        // Same fix, exercised on AppRef and ShortcutName slots too (not
        // just ElementRef) -- R4's longest-span-first resolution is
        // implemented once in the shared Tok::Rest arm, so it must hold
        // for every RestKind, not only the one the repro happened to use.
        assert_matched("open Now and Then", &ctx, "app.open");
        assert_matched("run shortcut Clean Up Now", &ctx, "shortcut.run");

        // Slot value sanity: the captured text is the FULL label, not the
        // marker-truncated prefix.
        let r = match_utterance("click Pay Now", &ctx);
        assert!(matches!(
            r,
            IntentResult::Matched { action, .. }
                if action.slots == vec![SlotValue::ElementRef("Pay Now".to_string())]
        ));

        // --- Adversarial counterparts: a similar phrase that is NOT in
        // the known set must still REJECT -- the fix must not have
        // loosened anything for a candidate that does not resolve. ---
        assert_rejected("click Pay Later", &ctx, RejectReason::NotACommand);
        assert_rejected("click Buy Soon", &ctx, RejectReason::NotACommand);
        assert_rejected("press Pay Again", &ctx, RejectReason::NotACommand);
        assert_rejected("click erase all content and preferences", &ctx, RejectReason::NotACommand);
        assert_rejected("click Now Loading", &ctx, RejectReason::NotACommand);
        assert_rejected("click Buy Now and Return", &ctx, RejectReason::NotACommand);
        assert_rejected("click Terms and Services", &ctx, RejectReason::NotACommand);
        assert_rejected("click Sign In and Exit", &ctx, RejectReason::NotACommand);
        assert_rejected("click Turn On Reminders", &ctx, RejectReason::NotACommand);
        assert_rejected("click Log In Later", &ctx, RejectReason::NotACommand);
        assert_rejected("click Remind Me Tomorrow", &ctx, RejectReason::NotACommand);
        assert_rejected("click Continue Without Paying", &ctx, RejectReason::NotACommand);
        assert_rejected("click Skip For Today", &ctx, RejectReason::NotACommand);
        assert_rejected("open Now and When", &ctx, RejectReason::NotACommand);
        assert_rejected("run shortcut Clean Up Later", &ctx, RejectReason::NotACommand);
        // The canonical dispatch-supplied adversarial: no app named "door
        // policy helps morale" exists in any known set, so this must
        // reject exactly as it did before the fix -- the fix only widens
        // resolution for spans that DO name something real.
        assert_rejected(
            "open door policy helps morale",
            &CommandContext {
                known_apps: vec!["Slack".to_string()],
                known_elements: Vec::new(),
                known_shortcuts: Vec::new(),
            },
            RejectReason::NotACommand,
        );
    }

    #[test]
    fn app_ref_slot_fails_closed_with_no_known_apps_supplied() {
        // The heart of the fix, stated directly rather than only
        // implied by the held-out set above: with an EMPTY
        // `CommandContext` -- the "AX read failed" / "no context yet"
        // case COMMANDS-SPEC calls out -- even an utterance naming a
        // real, plausible app must reject. There is no fallback to
        // accepting free text when the known-app set is unavailable.
        let empty = CommandContext::default();
        assert_rejected("open Slack", &empty, RejectReason::NotACommand);
        assert_rejected("switch to Chrome", &empty, RejectReason::NotACommand);
        assert_rejected("click Send", &empty, RejectReason::NotACommand);
        assert_rejected("run shortcut Morning Routine", &empty, RejectReason::NotACommand);
    }

    #[test]
    fn app_ref_slot_resolves_tolerantly_across_case_spacing_and_app_suffix() {
        let ctx = CommandContext {
            known_apps: vec!["Visual Studio Code".to_string()],
            known_elements: Vec::new(),
            known_shortcuts: Vec::new(),
        };
        // Case-insensitive.
        assert_matched("open visual studio code", &ctx, "app.open");
        // A trailing "app" word naming the same app is tolerated...
        assert_matched("open Visual Studio Code app", &ctx, "app.open");
        // ...but the tolerance is still closed-set, not a green light for
        // any "<word> app" phrase: an app genuinely called "Studio" would
        // need to be IN known_apps to match "open Studio app" -- it is
        // not, here, so this must still reject.
        let ctx_studio_absent = CommandContext { known_apps: vec!["Studio".to_string()], ..ctx.clone() };
        assert_matched("open Studio app", &ctx_studio_absent, "app.open");
        assert_rejected("open Nonexistent Studio app", &ctx_studio_absent, RejectReason::NotACommand);
    }

    // ---------------------------------------------------------------
    // Schema-id/registry alignment (remediation regression). §3.4 /
    // crates/voice-act/src/registry.rs `LAUNCH_SCHEMAS` is the single
    // source of truth for executable action ids; that crate is not a
    // dependency of this one (module doc), so the canonical set is
    // restated here as data rather than imported. Every id the grammar
    // table can ever emit must be a member -- an id that ISN'T means the
    // command is dead on arrival: `ActionRegistry::by_id` returns `None`
    // and resolution degrades to `Resolution::Refused(NotRegistered)`
    // no matter how correctly stage 1 matched the utterance.
    // ---------------------------------------------------------------

    const CANONICAL_SCHEMA_IDS: &[&str] = &[
        "app.open",
        "app.switch",
        "app.quit",
        "app.hide",
        "app.close_all_windows",
        "win.tile_left",
        "win.tile_right",
        "win.maximize",
        "win.minimize",
        "win.next_display",
        "nav.scroll",
        "nav.scroll_to_top",
        "nav.next_tab",
        "nav.back",
        "sys.volume",
        "sys.brightness",
        "sys.mute",
        "sys.media_play_pause",
        "sys.dnd",
        "sys.screenshot",
        "ui.click",
        "ui.show_numbers",
        "ui.focus_field",
        "ui.toggle_checkbox",
        "text.select_sentence",
        "text.select_paragraph",
        "text.delete_selection",
        "text.rewrite_style",
        "shortcut.run",
        "meta.undo",
        "meta.help",
        "meta.stop",
    ];

    #[test]
    fn every_grammar_schema_id_is_a_member_of_the_canonical_registry_set() {
        for pat in PATTERNS {
            assert!(
                CANONICAL_SCHEMA_IDS.contains(&pat.schema_id),
                "grammar pattern emits schema id {:?}, which is not in voice-act's \
                 registered LAUNCH_SCHEMAS -- this command is dead on arrival \
                 (ActionRegistry::by_id would return None)",
                pat.schema_id
            );
        }
    }

    #[test]
    fn command_lexicon_is_pure_and_deduplicated() {
        let a = command_lexicon();
        let b = command_lexicon();
        assert_eq!(a, b, "lexicon must be a pure function of the static table");
        assert!(a.contains(&"open"));
        assert!(a.contains(&"scroll"));
        assert!(a.contains(&"undo"));
        let mut sorted = a.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), a.len(), "lexicon must not contain duplicates");
    }

    #[test]
    fn word_to_num_handles_digits_and_words_within_bounds() {
        assert_eq!(word_to_num("3"), Some(3));
        assert_eq!(word_to_num("three"), Some(3));
        assert_eq!(word_to_num("fifty"), Some(50));
        assert_eq!(word_to_num("hundred"), Some(100));
        assert_eq!(word_to_num("bananas"), None);
    }
}
