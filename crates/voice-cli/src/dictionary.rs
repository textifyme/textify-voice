//! User dictionary → bias layer 2 terms (SPEC.md §3.3: "on-screen proper
//! nouns, user dictionary, filenames/identifiers ... assembled ... NEVER
//! blocks the first audio frame").
//!
//! Bias layer 2 (`voice_core::bias::correct_spans`) only ever corrects a
//! low-confidence word toward a term that's actually *in* the
//! [`BiasContext`](voice_core::BiasContext) it's given. `dictate.rs`
//! currently builds `BiasContext::empty(...)` (per this unit's dispatch),
//! so that whole phonetic-correction machinery is inert on the live path
//! today, not for lack of a term list but for lack of anywhere to load one
//! from. This module is that source: a small, hand-editable text file the
//! user maintains themselves, parsed into the `Vec<BiasTerm>` /
//! `Vec<LiteralRule>` a `BiasContext` and the normalizer's literal-rule pass
//! both want.
//!
//! **Not wired into `dictate.rs` by this change** -- per this unit's
//! dispatch that is explicitly another agent's job. This module only has to
//! load correctly and be usable.
//!
//! # File location
//!
//! `voice-asr-whisper`'s model cache resolves its directory via the `dirs`
//! crate: `dirs::data_dir()/textify/models` (see
//! `voice-asr-whisper/src/model.rs`'s `ModelManager::new`). This module is
//! asked to follow that same convention, but `voice-cli`'s `Cargo.toml` --
//! which this unit's dispatch does not permit editing (owned files are only
//! `dictionary.rs` and `clipboard.rs`) -- does not currently list `dirs` as
//! a dependency (only `voice-asr-whisper` does). Rather than silently
//! diverge from the requested convention, [`default_path`] below
//! reimplements the same per-OS resolution `dirs::data_dir()` performs,
//! cfg-gated per platform so it stays shaped like the rest of this crate's
//! platform boundary (see `crate::platform`) rather than assuming macOS
//! unconditionally. **On macOS it resolves to exactly the path the task
//! named**: `~/Library/Application Support/textify/dictionary.txt`. Adding
//! `dirs = "6"` to `voice-cli/Cargo.toml` (already pinned at `6.0.0` in the
//! workspace `Cargo.lock` via `voice-asr-whisper`, so this would not move
//! any version) would let a future change replace [`default_path`]'s body
//! with a one-line call to `dirs::data_dir()` -- this module's public API
//! would not need to change.
//!
//! # File format
//!
//! ```text
//! # Lines starting with '#' are comments; blank lines are ignored.
//!
//! Kubernetes
//! Alishah
//!
//! # "spoken form => written form": literal, case-insensitive substitution,
//! # the same mechanism voice-core's built-in literal rules use for
//! # "cursor dot ai" -> "cursor.ai".
//! cursor dot ai => cursor.ai
//! textify voice => Textify Voice
//! ```
//!
//! A plain line becomes one [`BiasTerm`] (the whole trimmed line is the
//! term -- multi-word proper nouns like "Onetelos Textify" are one term,
//! not split). A `spoken => written` line becomes one [`LiteralRule`],
//! split on the *first* `=>` only, spoken words split on whitespace.
//!
//! Malformed lines (an empty spoken or written half, a second stray `=>`)
//! and ambiguous duplicate mappings are collected as [`DictionaryParseError`]s
//! with their 1-indexed line number rather than silently dropped -- per
//! this unit's dispatch, "a dictionary that quietly ignores half its
//! entries is worse than one that complains." Exact duplicate lines (same
//! term, or same spoken form mapped to the same replacement) are not
//! errors -- just redundant -- and are reported separately in
//! [`Dictionary::duplicate_terms`] for transparency without being treated
//! as a parse failure.
//!
//! A missing file is the normal, expected state for a user who has never
//! created one -- [`load`] returns an empty, `found: false` [`Dictionary`]
//! for it, not an error. [`DictionaryError`] is reserved for a file that
//! exists but genuinely could not be read (permissions, I/O failure).

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use voice_core::{BiasTerm, LiteralRule};

/// Overrides [`default_path`] entirely when set to a non-empty value --
/// mirrors `voice-asr-whisper::model::CACHE_DIR_ENV_VAR`'s convention for
/// the same purpose (tests, and a future `--dictionary-path` CLI flag).
pub const DICTIONARY_PATH_ENV_VAR: &str = "TEXTIFY_VOICE_DICTIONARY_PATH";

/// Seeded into a freshly created dictionary file by [`ensure_starter_file`]
/// so the feature is discoverable (a user who goes looking finds a real,
/// working example, not an empty file) rather than invisible. Both example
/// entries are real, valid lines: `load`ing this content back produces one
/// [`BiasTerm`] and one [`LiteralRule`].
pub const STARTER_DICTIONARY: &str = r#"# textify-voice user dictionary
#
# One entry per line. Blank lines and lines starting with '#' are ignored.
#
# A plain line is a term dictation should be biased toward -- a name, a
# product, a proper noun the built-in vocabulary doesn't know. Whatever you
# say that *sounds like* this term (and whisper.cpp got wrong) can be
# corrected toward it:
#
#   Kubernetes
#
# A "spoken form => written form" line is a literal substitution: whatever
# you say on the left is replaced with the exact text on the right,
# whenever it's heard, the same mechanism the built-in "cursor dot ai" ->
# "cursor.ai" rule uses:
#
#   cursor dot ai => cursor.ai
#
# Edit this file directly -- add your own names, products, and phrases
# below. Changes take effect the next time textify-voice starts.

Kubernetes

cursor dot ai => cursor.ai
"#;

/// One line-numbered problem found while parsing -- reported, never
/// silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictionaryParseError {
    /// 1-indexed, matching what a user sees in a text editor.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for DictionaryParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// A redundant (non-erroring) repeat of a term or mapping already seen
/// earlier in the file -- kept out of [`DictionaryParseError`] because
/// re-listing the same entry twice is not ambiguous or broken, just
/// pointless, and demoting it to a hard error would make the "duplicate
/// terms" case this unit's dispatch calls out indistinguishable from a
/// genuine typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateEntry {
    /// 1-indexed line of the *second* (redundant) occurrence.
    pub line: usize,
    /// Human-readable description of what was duplicated, e.g. `"Kubernetes"`
    /// or `"cursor dot ai => cursor.ai"`.
    pub entry: String,
}

/// The result of loading and parsing a dictionary file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dictionary {
    /// Ready to fold into a `BiasContext::terms`.
    pub terms: Vec<BiasTerm>,
    /// Ready to prepend/append to `default_literal_rules()`.
    pub literal_rules: Vec<LiteralRule>,
    /// `false` when the file did not exist at all (the normal first-run
    /// state) -- distinct from an existing-but-empty file, which is also
    /// valid and simply contributes no terms.
    pub found: bool,
    pub errors: Vec<DictionaryParseError>,
    pub duplicate_terms: Vec<DuplicateEntry>,
}

impl Dictionary {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.literal_rules.is_empty()
    }
}

/// Failure resolving or reading the dictionary file. A *missing* file is
/// not one of these -- see [`load`].
#[derive(Debug)]
pub enum DictionaryError {
    /// [`default_path`] could not resolve a platform data directory (e.g.
    /// `$HOME` unset) and [`DICTIONARY_PATH_ENV_VAR`] was not set either.
    NoConfigDir,
    Io(io::Error),
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DictionaryError::NoConfigDir => write!(
                f,
                "could not resolve a platform data directory for the user dictionary; \
                 set {DICTIONARY_PATH_ENV_VAR} or pass an explicit path"
            ),
            DictionaryError::Io(e) => write!(f, "dictionary I/O error: {e}"),
        }
    }
}

impl std::error::Error for DictionaryError {}

impl From<io::Error> for DictionaryError {
    fn from(e: io::Error) -> Self {
        DictionaryError::Io(e)
    }
}

/// Resolve the default dictionary path: [`DICTIONARY_PATH_ENV_VAR`] if set
/// to a non-empty value, else the platform data directory (see module docs
/// for why this is hand-rolled rather than a `dirs::data_dir()` call) --
/// concretely `~/Library/Application Support/textify/dictionary.txt` on
/// macOS. Does not create the directory or file; see [`ensure_starter_file`].
pub fn default_path() -> Result<PathBuf, DictionaryError> {
    if let Ok(p) = std::env::var(DICTIONARY_PATH_ENV_VAR) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(platform_data_dir()?.join("textify").join("dictionary.txt"))
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> Result<PathBuf, DictionaryError> {
    // Matches `dirs::data_dir()` on macOS exactly: there is no separate
    // XDG-style data dir on this platform, Apple's own guidance is
    // `~/Library/Application Support` for both config and data.
    let home = std::env::var_os("HOME").ok_or(DictionaryError::NoConfigDir)?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support"))
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> Result<PathBuf, DictionaryError> {
    // Matches `dirs::data_dir()` on Windows: `%APPDATA%`
    // (`FOLDERID_RoamingAppData`). Untested -- this crate targets macOS
    // first per PORTING.md; written to keep the crate's shape portable
    // rather than as a verified Windows path.
    let appdata = std::env::var_os("APPDATA").ok_or(DictionaryError::NoConfigDir)?;
    Ok(PathBuf::from(appdata))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_data_dir() -> Result<PathBuf, DictionaryError> {
    // Matches `dirs::data_dir()` on Linux/BSD: `$XDG_DATA_HOME`, falling
    // back to `$HOME/.local/share`. Untested here for the same reason as
    // the Windows branch above.
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg));
        }
    }
    let home = std::env::var_os("HOME").ok_or(DictionaryError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".local").join("share"))
}

/// Write [`STARTER_DICTIONARY`] to `path` (creating parent directories as
/// needed) if and only if nothing is there yet. Idempotent and safe to call
/// on every startup: returns `Ok(true)` the one time it actually creates
/// the file, `Ok(false)` on every call after that (or if the user already
/// had their own file there). Never overwrites an existing file, including
/// an empty one -- "the user deleted everything on purpose" and "the user
/// never had a file" must not be treated the same.
pub fn ensure_starter_file(path: &Path) -> Result<bool, DictionaryError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, STARTER_DICTIONARY)?;
    Ok(true)
}

/// Load and parse the dictionary at `path`. A missing file is not an error
/// -- returns `Dictionary { found: false, ..Dictionary::default() }` (an
/// empty, valid, unsurprising result for the common first-run case). Any
/// other read failure (permissions, not-a-file, ...) is a real
/// [`DictionaryError`].
pub fn load(path: &Path) -> Result<Dictionary, DictionaryError> {
    let source = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Dictionary {
                found: false,
                ..Dictionary::default()
            });
        }
        Err(e) => return Err(DictionaryError::Io(e)),
    };
    let mut dict = parse(&source);
    dict.found = true;
    Ok(dict)
}

/// Pure parse of dictionary file *contents* (no I/O) -- the part that's
/// easy to test exhaustively. `found` is always `false` on the returned
/// value; [`load`] sets it after a successful read since "found" is
/// inherently about the filesystem, not the text.
#[must_use]
pub fn parse(source: &str) -> Dictionary {
    let mut terms = Vec::new();
    let mut literal_rules = Vec::new();
    let mut errors = Vec::new();
    let mut duplicate_terms = Vec::new();

    // key: lowercased term -> line it was first seen on.
    let mut seen_terms: HashMap<String, usize> = HashMap::new();
    // key: lowercased, whitespace-joined spoken words -> (line, replacement).
    let mut seen_mappings: HashMap<String, (usize, String)> = HashMap::new();

    for (idx, raw_line) in source.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((left, right)) = trimmed.split_once("=>") {
            let spoken = left.trim();
            let written = right.trim();
            if spoken.is_empty() {
                errors.push(DictionaryParseError {
                    line: line_no,
                    message: "empty spoken form before '=>'".to_string(),
                });
                continue;
            }
            if written.is_empty() {
                errors.push(DictionaryParseError {
                    line: line_no,
                    message: "empty written form after '=>'".to_string(),
                });
                continue;
            }
            if written.contains("=>") {
                errors.push(DictionaryParseError {
                    line: line_no,
                    message: format!(
                        "written form contains a second '=>' -- only one mapping per line is \
                         supported (got {trimmed:?})"
                    ),
                });
                continue;
            }

            let key = spoken.to_lowercase();
            if let Some((first_line, first_replacement)) = seen_mappings.get(&key) {
                if first_replacement == written {
                    duplicate_terms.push(DuplicateEntry {
                        line: line_no,
                        entry: format!("{spoken} => {written}"),
                    });
                } else {
                    errors.push(DictionaryParseError {
                        line: line_no,
                        message: format!(
                            "'{spoken}' was already mapped to '{first_replacement}' on line \
                             {first_line}; ignoring the conflicting mapping to '{written}' here \
                             (ambiguous -- keeping the first)"
                        ),
                    });
                }
                continue;
            }
            seen_mappings.insert(key, (line_no, written.to_string()));

            let words: Vec<&str> = spoken.split_whitespace().collect();
            literal_rules.push(LiteralRule::new(&words, written));
            continue;
        }

        // Plain term line -- the whole trimmed line is one bias term.
        let key = trimmed.to_lowercase();
        if let Some(first_line) = seen_terms.get(&key) {
            let _ = first_line;
            duplicate_terms.push(DuplicateEntry {
                line: line_no,
                entry: trimmed.to_string(),
            });
            continue;
        }
        seen_terms.insert(key, line_no);
        terms.push(BiasTerm::new(trimmed));
    }

    Dictionary {
        terms,
        literal_rules,
        found: false,
        errors,
        duplicate_terms,
    }
}

/// Convenience for the CLI wiring: resolve the default path, create a
/// starter file there if none exists yet, then load it. One call gets a
/// caller from "nothing" to "a populated (or honestly empty-with-errors)
/// [`Dictionary`]" -- not called by this module's own tests, which always
/// pass an explicit temp path instead of touching the real default
/// location (see the `tests` module below).
pub fn load_or_seed_default() -> Result<Dictionary, DictionaryError> {
    let path = default_path()?;
    ensure_starter_file(&path)?;
    load(&path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // `name` (each call site's own literal) plus the process id keeps every
    // test's path distinct from every other test's, and from a concurrent
    // `cargo test` process's, without needing a shared counter.
    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "textify-voice-dictionary-test-{name}-{}",
            std::process::id(),
        ))
    }

    #[test]
    fn missing_file_is_not_an_error_and_says_so() {
        let path = temp_path("missing");
        assert!(!path.exists(), "precondition: path must not exist");
        let dict = load(&path).expect("missing file must not be an Err");
        assert!(!dict.found);
        assert!(dict.is_empty());
        assert!(dict.errors.is_empty());
    }

    #[test]
    fn parses_plain_terms_ignoring_blanks_and_comments() {
        let src = "\n# a comment\nKubernetes\n\n   \nAlishah\n# trailing comment\n";
        let dict = parse(src);
        assert_eq!(
            dict.terms,
            vec![BiasTerm::new("Kubernetes"), BiasTerm::new("Alishah")]
        );
        assert!(dict.literal_rules.is_empty());
        assert!(dict.errors.is_empty());
        assert!(dict.duplicate_terms.is_empty());
    }

    #[test]
    fn parses_a_spoken_to_written_mapping() {
        let dict = parse("cursor dot ai => cursor.ai\n");
        assert_eq!(
            dict.literal_rules,
            vec![LiteralRule::new(&["cursor", "dot", "ai"], "cursor.ai")]
        );
        assert!(dict.terms.is_empty());
        assert!(dict.errors.is_empty());
    }

    #[test]
    fn mapping_arrow_tolerates_extra_surrounding_whitespace() {
        let dict = parse("  textify   voice   =>   Textify Voice  \n");
        assert_eq!(
            dict.literal_rules,
            vec![LiteralRule::new(&["textify", "voice"], "Textify Voice")]
        );
    }

    #[test]
    fn malformed_empty_spoken_form_is_reported_with_line_number() {
        let dict = parse("Kubernetes\n => cursor.ai\n");
        assert_eq!(dict.terms, vec![BiasTerm::new("Kubernetes")]);
        assert_eq!(dict.errors.len(), 1);
        assert_eq!(dict.errors[0].line, 2);
        assert!(dict.errors[0].message.contains("empty spoken form"));
    }

    #[test]
    fn malformed_empty_written_form_is_reported_with_line_number() {
        let dict = parse("cursor dot ai =>   \n");
        assert!(dict.literal_rules.is_empty());
        assert_eq!(dict.errors.len(), 1);
        assert_eq!(dict.errors[0].line, 1);
        assert!(dict.errors[0].message.contains("empty written form"));
    }

    #[test]
    fn malformed_double_arrow_is_reported_and_not_parsed() {
        let dict = parse("a => b => c\n");
        assert!(dict.literal_rules.is_empty());
        assert_eq!(dict.errors.len(), 1);
        assert_eq!(dict.errors[0].line, 1);
        assert!(dict.errors[0].message.contains("second '=>'"));
    }

    #[test]
    fn one_malformed_line_does_not_block_later_valid_lines() {
        let dict = parse(" => nope\nKubernetes\ncursor dot ai => cursor.ai\n");
        assert_eq!(dict.errors.len(), 1);
        assert_eq!(dict.terms, vec![BiasTerm::new("Kubernetes")]);
        assert_eq!(
            dict.literal_rules,
            vec![LiteralRule::new(&["cursor", "dot", "ai"], "cursor.ai")]
        );
    }

    #[test]
    fn exact_duplicate_term_is_reported_as_duplicate_not_error_and_not_repeated() {
        let dict = parse("Kubernetes\nkubernetes\nKUBERNETES\n");
        assert_eq!(
            dict.terms,
            vec![BiasTerm::new("Kubernetes")],
            "only the first occurrence is kept"
        );
        assert!(dict.errors.is_empty());
        assert_eq!(dict.duplicate_terms.len(), 2);
        assert_eq!(dict.duplicate_terms[0].line, 2);
        assert_eq!(dict.duplicate_terms[1].line, 3);
    }

    #[test]
    fn identical_duplicate_mapping_is_reported_as_duplicate_not_error() {
        let dict = parse("cursor dot ai => cursor.ai\nCursor Dot AI => cursor.ai\n");
        assert_eq!(dict.literal_rules.len(), 1);
        assert!(dict.errors.is_empty());
        assert_eq!(dict.duplicate_terms.len(), 1);
        assert_eq!(dict.duplicate_terms[0].line, 2);
    }

    #[test]
    fn conflicting_duplicate_mapping_to_a_different_replacement_is_an_error() {
        let dict = parse("cursor dot ai => cursor.ai\ncursor dot ai => CURSOR.AI\n");
        assert_eq!(
            dict.literal_rules,
            vec![LiteralRule::new(&["cursor", "dot", "ai"], "cursor.ai")],
            "the first mapping wins"
        );
        assert_eq!(dict.errors.len(), 1);
        assert_eq!(dict.errors[0].line, 2);
        assert!(dict.errors[0].message.contains("ambiguous"));
        assert!(
            dict.duplicate_terms.is_empty(),
            "a conflict is an error, not a mere duplicate"
        );
    }

    #[test]
    fn starter_file_is_created_once_and_parses_clean() {
        let path = temp_path("starter");
        let _ = fs::remove_file(&path);
        assert!(ensure_starter_file(&path).expect("create starter"));
        assert!(
            !ensure_starter_file(&path).expect("second call is a no-op"),
            "must not recreate/overwrite"
        );

        let dict = load(&path).expect("load the starter file");
        assert!(dict.found);
        assert!(
            dict.errors.is_empty(),
            "the shipped starter content itself must parse clean: {:?}",
            dict.errors
        );
        assert_eq!(dict.terms, vec![BiasTerm::new("Kubernetes")]);
        assert_eq!(
            dict.literal_rules,
            vec![LiteralRule::new(&["cursor", "dot", "ai"], "cursor.ai")]
        );

        fs::remove_file(&path).expect("cleanup temp starter file");
    }

    #[test]
    fn ensure_starter_file_never_overwrites_an_existing_empty_file() {
        let path = temp_path("preexisting-empty");
        fs::write(&path, "")
            .expect("create empty file to simulate a deliberately emptied dictionary");
        let created = ensure_starter_file(&path).expect("must not error on an existing file");
        assert!(
            !created,
            "must not report creating (or touching) a file that already existed"
        );
        let contents = fs::read_to_string(&path).expect("read back");
        assert_eq!(
            contents, "",
            "must not have overwritten the user's deliberately empty file"
        );
        fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn default_path_env_override_is_honored() {
        let override_path = temp_path("env-override").join("dictionary.txt");
        std::env::set_var(DICTIONARY_PATH_ENV_VAR, &override_path);
        let resolved = default_path().expect("resolve with override set");
        std::env::remove_var(DICTIONARY_PATH_ENV_VAR);
        assert_eq!(resolved, override_path);
    }

    #[test]
    fn load_reads_a_file_written_directly_end_to_end() {
        let path = temp_path("end-to-end");
        fs::write(&path, "Onetelos\ncursor dot ai => cursor.ai\n").expect("write test dictionary");
        let dict = load(&path).expect("load");
        assert!(dict.found);
        assert_eq!(dict.terms, vec![BiasTerm::new("Onetelos")]);
        assert_eq!(dict.literal_rules.len(), 1);
        fs::remove_file(&path).expect("cleanup");
    }
}
