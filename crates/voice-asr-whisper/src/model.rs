//! ggml model download/cache management.
//!
//! Downloads a `ggml-*.bin` whisper.cpp model on first use, with visible
//! progress, caches it under the platform data dir (via the `dirs` crate),
//! and never re-downloads a file that is already on disk.
//!
//! The download source defaults to the upstream `ggerganov/whisper.cpp`
//! HuggingFace repo, but is overridable via [`MODEL_BASE_URL_ENV_VAR`] --
//! pointed at our own R2 bucket in production, once credentials for it
//! exist, so a rename/rate-limit/outage on a third party's repo can't take
//! down every new install. See `packaging/README.md` "Model hosting".
//!
//! Integrity is checked two ways: a fast size-range sanity check first
//! (catches a truncated download or an HTML error page cheaply), then a
//! SHA-256 check against a value pinned in this file's source
//! ([`ModelId::expected_sha256`]) -- independent of which base URL served
//! the bytes, so pointing at R2 does not relax the guarantee. A mismatch on
//! either check deletes the bad file and returns `Err`; the caller never
//! gets a path to an unverified file. This is the real integrity gate, not
//! TLS-to-whichever-host-answered: TLS proves the bytes weren't tampered
//! with in transit, not that they're the real model.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Env var that overrides the model cache directory. Checked by
/// [`ModelManager::new`]; the explicit [`ModelManager::with_cache_dir`]
/// constructor bypasses it entirely (a caller who names a path wins over
/// the environment).
pub const CACHE_DIR_ENV_VAR: &str = "TEXTIFY_WHISPER_MODEL_DIR";

/// Env var that overrides the base URL model artifacts are downloaded from.
/// Checked by [`ModelId::url`] on every call (not cached), so setting it
/// before the first `models --download` (or before the first `dictate`/
/// `transcribe` that triggers an implicit download) is enough -- no code
/// change needed. Expected to hold a base URL with no trailing slash and no
/// filename, e.g. `https://models.textify.me` or an R2 `*.r2.dev` /
/// custom-domain URL; `<base>/<filename>` is requested for each model (see
/// [`ModelId::filename`]).
///
/// Falls back to [`MIRROR_MODEL_BASE_URL`] (with [`UPSTREAM_MODEL_BASE_URL`]
/// behind it -- see [`download_sources_from`]) when unset or empty. Files served from an overridden base URL are still
/// checked against [`ModelId::expected_sha256`] -- an R2 upload must be a
/// byte-identical copy of the pinned model, not merely "a model that loads".
pub const MODEL_BASE_URL_ENV_VAR: &str = "TEXTIFY_MODEL_BASE_URL";

/// First-party mirror, and the default primary origin: byte-identical copies
/// of the pinned weights in the `textify-models` R2 bucket, behind
/// `models.textify.me` (see `infra/README.md`).
///
/// Primary rather than fallback for the same reason the browser engines
/// mirror their weights: a third-party origin is a third party's uptime, and
/// an ad/privacy blocker that decides `huggingface.co` is a tracker turns a
/// first-run model download into an inexplicable failure. A real user hit
/// exactly that on the web side.
///
/// Mirroring is only safe because integrity does not depend on origin --
/// every downloaded file is checked against [`ModelId::expected_sha256`], so
/// a mirror serving different bytes fails closed instead of installing.
const MIRROR_MODEL_BASE_URL: &str = "https://models.textify.me/voice";

/// Upstream: the `ggerganov/whisper.cpp` HuggingFace repo, the canonical
/// publisher of these ggml conversions of OpenAI's Whisper weights (both
/// MIT-licensed -- see `packaging/licenses/`). Kept as an automatic fallback
/// so a mirror outage degrades to a slower download rather than a broken
/// first run.
const UPSTREAM_MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// The base-URL policy itself, as a pure function of the override value:
/// `None`, empty, and any non-`https://` value all resolve to
/// [`MIRROR_MODEL_BASE_URL`]. Split out from [`resolve_base_url`] so the
/// policy is testable without mutating the process environment, which
/// `cargo test`'s parallel threads share.
///
/// HTTPS ONLY, same rule the updater enforces. The pinned SHA-256 means a
/// substituted model fails closed rather than being installed, so this is
/// defence in depth rather than the only guard — but an http model fetch
/// still leaks which model a user downloads to anyone on the path, and
/// there is no reason to allow it.
fn resolve_base_url_from(override_value: Option<&str>) -> String {
    override_value
        .filter(|s| !s.is_empty())
        .filter(|s| {
            let ok = s.starts_with("https://");
            if !ok {
                eprintln!(
                    "warning: {MODEL_BASE_URL_ENV_VAR} is not https:// -- ignoring it and using \
                     the default source. Model downloads are https-only."
                );
            }
            ok
        })
        .map(ToString::to_string)
        .unwrap_or_else(|| MIRROR_MODEL_BASE_URL.to_string())
}

fn resolve_base_url() -> String {
    resolve_base_url_from(std::env::var(MODEL_BASE_URL_ENV_VAR).ok().as_deref())
}

/// The origins [`ModelManager::ensure_downloaded`] tries, in order.
///
/// Unset override: mirror first, upstream second. **An explicit, valid
/// override is used alone, with no fallback** -- someone who points this at
/// an internal mirror, a staging bucket, or a local server is testing *that*
/// origin, and silently succeeding against HuggingFace instead would hide
/// exactly the failure they are looking for. An override that is rejected
/// (empty, or not `https://`) is not an override at all and gets the normal
/// pair, consistent with [`resolve_base_url_from`].
fn download_sources_from(override_value: Option<&str>) -> Vec<String> {
    let resolved = resolve_base_url_from(override_value);
    if resolved == MIRROR_MODEL_BASE_URL {
        vec![
            MIRROR_MODEL_BASE_URL.to_string(),
            UPSTREAM_MODEL_BASE_URL.to_string(),
        ]
    } else {
        vec![resolved]
    }
}

fn download_sources() -> Vec<String> {
    download_sources_from(std::env::var(MODEL_BASE_URL_ENV_VAR).ok().as_deref())
}

/// One of the two ggml models this crate knows how to fetch. Both are the
/// English-only ("`.en`") whisper.cpp quantization-free ggml release
/// artifacts published by the upstream `ggerganov/whisper.cpp` HuggingFace
/// repo -- the canonical distribution point for these files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ModelId {
    /// `ggml-tiny.en.bin`, ~75 MB.
    TinyEn,
    /// `ggml-base.en.bin`, ~148 MB. Default: better accuracy than tiny at a
    /// size/latency cost the MVP's push-to-talk flow can absorb (batch
    /// decode happens once, on key-up, not continuously).
    ///
    /// CAVEAT found during independent end-to-end verification: on real
    /// multi-minute audio (`fixtures/audio/ref-3min.wav`), this build's
    /// whisper.cpp decode deterministically drops a large contiguous span
    /// of words (~24% WER) around the 90-100s mark. This is NOT specific to
    /// this model -- `TinyEn`, run through the exact same real pipeline
    /// (`voice_audio::decode_wav_file` -> `feed_pcm` -> `finalize`), drops a
    /// *different* ~100-word span at a near-identical WER (24.6%). Swapping
    /// models does not fix it: it reproduces on both, so the default was
    /// deliberately left as `BaseEn` rather than "fixed" by picking the
    /// other model on the strength of a flawed apples-to-oranges comparison
    /// (an early test run compared `BaseEn` via this real pipeline against
    /// `TinyEn` fed raw via a *different*, round-trip-free PCM path, which
    /// made `TinyEn` look artifically clean -- it is not, once tested
    /// through the same path). See this crate's test coverage and the
    /// verification run's report for the full repro. The real fix is
    /// upstream (whisper.cpp's long-form segment-seek/hallucination-avoidance
    /// heuristics), not a parameter this crate controls safely -- disabling
    /// `no_speech_thold`/`logprob_thold`/`entropy_thold` guards was also
    /// tried and made results *worse* (near-total garbage output), so no
    /// local mitigation was applied here.
    #[default]
    BaseEn,
}

impl ModelId {
    #[must_use]
    pub fn filename(self) -> &'static str {
        match self {
            ModelId::TinyEn => "ggml-tiny.en.bin",
            ModelId::BaseEn => "ggml-base.en.bin",
        }
    }

    /// Full download URL: `<base>/<filename>`, where `<base>` is
    /// [`resolve_base_url`] (the upstream HuggingFace repo, unless
    /// [`MODEL_BASE_URL_ENV_VAR`] overrides it). Resolved fresh on every
    /// call -- not cached -- so an env var set between calls takes effect
    /// immediately, with no `ModelManager` rebuild required.
    #[must_use]
    pub fn url(self) -> String {
        self.url_from_base(&resolve_base_url())
    }

    /// `<base>/<filename>`, tolerating a trailing slash on `base`. Split out
    /// from [`Self::url`] so URL joining is testable without going through
    /// the process environment.
    fn url_from_base(self, base: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), self.filename())
    }

    /// `(min_bytes, max_bytes)` sanity window for the downloaded file size.
    /// Cheap first-pass check, run before the (slower, whole-file) SHA-256
    /// check in [`ModelId::expected_sha256`] -- catches "downloaded an HTML
    /// error page" or "download got truncated" cheaply, before spending a
    /// full-file hash pass on bytes that are obviously wrong.
    #[must_use]
    pub fn expected_size_range(self) -> (u64, u64) {
        match self {
            ModelId::TinyEn => (70_000_000, 80_000_000),
            ModelId::BaseEn => (140_000_000, 156_000_000),
        }
    }

    /// Lower-case hex SHA-256 of the exact model file this crate expects,
    /// independent of which base URL served it. This is the real
    /// integrity gate (see this module's doc comment); [`Self::expected_size_range`]
    /// is only a cheap pre-filter.
    ///
    /// Pinned from the upstream `ggerganov/whisper.cpp` HuggingFace repo's
    /// published git-lfs object ID (itself the SHA-256 of the file's
    /// content, confirmed via the HF tree API's `lfs.oid` field) and
    /// cross-checked against a local `shasum -a 256` of both cached model
    /// files on 2026-08-18 -- both methods agreed exactly. If upstream ever
    /// republishes either file under the same name with different bytes,
    /// this pin (not the upstream repo) is the source of truth, and it must
    /// be updated deliberately, not silently.
    #[must_use]
    pub fn expected_sha256(self) -> &'static str {
        match self {
            ModelId::TinyEn => "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
            ModelId::BaseEn => "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        }
    }

    #[must_use]
    pub fn is_english_only(self) -> bool {
        true
    }
}

#[derive(Debug)]
pub enum ModelError {
    /// Could not resolve a platform data dir and no override was given
    /// (see [`CACHE_DIR_ENV_VAR`] / [`ModelManager::with_cache_dir`]).
    NoCacheDir,
    Io(std::io::Error),
    Http(String),
    /// The file that landed on disk is outside [`ModelId::expected_size_range`]
    /// -- almost certainly a truncated or wrong download. The bad file is
    /// removed so a retry doesn't see a poisoned cache entry.
    SizeMismatch {
        expected_range: (u64, u64),
        actual: u64,
    },
    /// The file that landed on disk has the right size but the wrong
    /// SHA-256 -- silently corrupted, silently substituted, or upstream
    /// changed the file under our feet. This is the integrity failure that
    /// actually matters (a wrong-but-plausibly-sized model is a
    /// silent-wrong-answer machine, not a loud crash). The bad file is
    /// removed; the caller gets `Err`, never a path to it. Fails closed.
    ShaMismatch { expected: String, actual: String },
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::NoCacheDir => write!(
                f,
                "could not resolve a platform data dir for the model cache; set {CACHE_DIR_ENV_VAR} or pass an explicit path"
            ),
            ModelError::Io(e) => write!(f, "model cache I/O error: {e}"),
            ModelError::Http(msg) => write!(f, "model download failed: {msg}"),
            ModelError::SizeMismatch {
                expected_range,
                actual,
            } => write!(
                f,
                "downloaded model size {actual} bytes is outside the expected range {expected_range:?}; download was likely truncated or corrupted"
            ),
            ModelError::ShaMismatch { expected, actual } => write!(
                f,
                "downloaded model SHA-256 mismatch: expected {expected}, got {actual}; refusing to use this file (it was deleted). This means either a corrupted download or a substituted file -- do not retry blindly, verify the source."
            ),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<std::io::Error> for ModelError {
    fn from(e: std::io::Error) -> Self {
        ModelError::Io(e)
    }
}

/// Progress callback: `(bytes_downloaded_so_far, total_bytes_if_known)`.
/// `total_bytes_if_known` is `0` when the server didn't send a
/// `Content-Length` header.
pub type ProgressCallback<'a> = dyn FnMut(u64, u64) + 'a;

pub struct ModelManager {
    cache_dir: PathBuf,
}

impl ModelManager {
    /// Resolve the cache dir from (in order): [`CACHE_DIR_ENV_VAR`], then
    /// `dirs::data_dir()/textify/models`. Does not create the directory
    /// yet -- that happens lazily in [`ensure_downloaded`](Self::ensure_downloaded).
    pub fn new() -> Result<Self, ModelError> {
        if let Ok(dir) = std::env::var(CACHE_DIR_ENV_VAR) {
            if !dir.is_empty() {
                return Ok(Self::with_cache_dir(PathBuf::from(dir)));
            }
        }
        let data_dir = dirs::data_dir().ok_or(ModelError::NoCacheDir)?;
        Ok(Self::with_cache_dir(data_dir.join("textify").join("models")))
    }

    /// Explicit override, bypassing [`CACHE_DIR_ENV_VAR`] entirely -- for
    /// callers (the future CLI's `--model-dir` flag, tests) that want to
    /// name a path programmatically rather than through the environment.
    #[must_use]
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    #[must_use]
    pub fn model_path(&self, id: ModelId) -> PathBuf {
        self.cache_dir.join(id.filename())
    }

    /// True if a plausibly-complete copy of `id` is already cached (file
    /// exists and its size falls inside [`ModelId::expected_size_range`]).
    #[must_use]
    pub fn is_cached(&self, id: ModelId) -> bool {
        let path = self.model_path(id);
        let Ok(meta) = fs::metadata(&path) else {
            return false;
        };
        let (min, max) = id.expected_size_range();
        (min..=max).contains(&meta.len())
    }

    /// Return the path to `id`, downloading it first if it is not already
    /// cached. Never re-downloads a file that passed the size check on a
    /// prior call. `progress`, if given, is called repeatedly during an
    /// actual download (never called at all on a cache hit).
    pub fn ensure_downloaded(
        &self,
        id: ModelId,
        progress: Option<&mut ProgressCallback<'_>>,
    ) -> Result<PathBuf, ModelError> {
        let path = self.model_path(id);
        if self.is_cached(id) {
            return Ok(path);
        }

        fs::create_dir_all(&self.cache_dir)?;

        let tmp_path = path.with_extension("bin.partial");
        let sources = download_sources();
        let last = sources.len().saturating_sub(1);
        let mut progress = progress;
        let mut last_err = None;

        for (i, base) in sources.iter().enumerate() {
            let url = id.url_from_base(base);
            // Reborrow rather than move: the callback has to survive every
            // attempt, so a retry keeps reporting progress instead of going
            // silent exactly when the download is already going badly.
            let attempt = download_to_file(&url, &tmp_path, progress.as_deref_mut())
                .and_then(|()| verify_downloaded_file(id, &tmp_path));
            match attempt {
                Ok(()) => {
                    fs::rename(&tmp_path, &path)?;
                    return Ok(path);
                }
                Err(e) => {
                    // A failed attempt leaves a partial or wrong-content file
                    // behind. The next origin must not resume onto it, and
                    // giving up must not leave it on a user's disk.
                    let _ = fs::remove_file(&tmp_path);
                    if i < last {
                        eprintln!(
                            "warning: model download from {url} failed ({e}) -- retrying against \
                             the next source."
                        );
                    }
                    last_err = Some(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| {
            ModelError::Http("no model download source was configured".to_string())
        }))
    }
}

/// Both integrity checks, cheapest first, on a file that has just landed on
/// disk: the size window catches an HTML error page or a truncated transfer
/// without hashing hundreds of megabytes, then SHA-256 proves these are the
/// expected bytes regardless of which base URL served them. Any failure
/// deletes `path` before returning, so no caller can be handed a path to
/// bytes that did not verify.
///
/// Takes the expectations as arguments rather than reading them off a
/// [`ModelId`] so both failure paths can be exercised against small
/// synthetic files instead of a real ~77 MB download.
fn verify_file_integrity(
    path: &Path,
    expected_range: (u64, u64),
    expected_sha256: &str,
) -> Result<(), ModelError> {
    let size = fs::metadata(path)?.len();
    if !(expected_range.0..=expected_range.1).contains(&size) {
        let _ = fs::remove_file(path);
        return Err(ModelError::SizeMismatch {
            expected_range,
            actual: size,
        });
    }

    let actual_sha256 = sha256_hex_of_file(path)?;
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(path);
        return Err(ModelError::ShaMismatch {
            expected: expected_sha256.to_string(),
            actual: actual_sha256,
        });
    }
    Ok(())
}

/// [`verify_file_integrity`] against the values pinned for `id`.
fn verify_downloaded_file(id: ModelId, path: &Path) -> Result<(), ModelError> {
    verify_file_integrity(path, id.expected_size_range(), id.expected_sha256())
}

/// Streamed SHA-256 of a file's full contents (64 KiB chunks, matching
/// [`download_to_file`]'s own buffer size) -- avoids reading a ~150 MB
/// model into memory at once just to hash it.
fn sha256_hex_of_file(path: &Path) -> Result<String, ModelError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // Writing to a String via fmt::Write is infallible; discard the
        // Result rather than pulling in unwrap/expect (both `warn` at the
        // workspace level, denied under -D warnings).
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

impl Default for ModelManager {
    /// Panics only via `.expect` at the very edge (constructing the
    /// `Default` impl); prefer [`ModelManager::new`] in real call sites,
    /// which surfaces [`ModelError::NoCacheDir`] instead of panicking.
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self::with_cache_dir(PathBuf::from(".textify-models")))
    }
}

fn download_to_file(
    url: &str,
    dest: &Path,
    progress: Option<&mut ProgressCallback<'_>>,
) -> Result<(), ModelError> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .new_agent();

    let resp = agent
        .get(url)
        .header("User-Agent", "textify-voice-asr-whisper/0.1")
        .call()
        .map_err(|e| ModelError::Http(e.to_string()))?;

    let total: u64 = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_body().into_reader();
    let mut file = fs::File::create(dest)?;
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut cb = progress;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        if let Some(cb) = cb.as_deref_mut() {
            cb(downloaded, total);
        }
    }
    file.flush()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Fresh, uniquely-named scratch directory. Per-test-name and per-pid so
    /// two tests (or two concurrent `cargo test` runs) never share one.
    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "textify-voice-asr-whisper-test-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn model_ids_have_distinct_filenames_and_urls() {
        assert_ne!(ModelId::TinyEn.filename(), ModelId::BaseEn.filename());
        assert_ne!(ModelId::TinyEn.url(), ModelId::BaseEn.url());
        // Holds whatever `MODEL_BASE_URL_ENV_VAR` happens to be set to in the
        // developer's shell: a non-https override is discarded by
        // `resolve_base_url_from`, so the resolved URL is https either way.
        assert!(ModelId::TinyEn.url().starts_with("https://"));
        assert!(ModelId::BaseEn.url().starts_with("https://"));
    }

    #[test]
    fn unset_override_tries_the_mirror_first_then_upstream() {
        assert_eq!(
            download_sources_from(None),
            vec![
                MIRROR_MODEL_BASE_URL.to_string(),
                UPSTREAM_MODEL_BASE_URL.to_string()
            ],
            "the first-party mirror must be tried before huggingface.co, or the \
             whole point of mirroring (not depending on a third party's uptime \
             or on it surviving an ad blocker) is lost"
        );
    }

    #[test]
    fn an_explicit_override_is_used_alone_with_no_silent_upstream_fallback() {
        // Falling back here would mean a developer pointing at a staging
        // bucket gets a model from HuggingFace and a passing run, learning
        // nothing about whether their bucket actually works.
        assert_eq!(
            download_sources_from(Some("https://staging.example.test/models")),
            vec!["https://staging.example.test/models".to_string()]
        );
    }

    #[test]
    fn a_rejected_override_is_not_an_override_and_keeps_both_sources() {
        for bad in ["", "http://models.textify.me", "ftp://example.test"] {
            assert_eq!(
                download_sources_from(Some(bad)),
                vec![
                    MIRROR_MODEL_BASE_URL.to_string(),
                    UPSTREAM_MODEL_BASE_URL.to_string()
                ],
                "{bad:?} is discarded by the https-only policy, so it must not \
                 also suppress the upstream fallback"
            );
        }
    }

    #[test]
    fn default_model_is_base_en() {
        assert_eq!(ModelId::default(), ModelId::BaseEn);
    }

    #[test]
    fn with_cache_dir_bypasses_env_and_is_not_cached_when_empty() {
        let tmp = std::env::temp_dir().join(format!(
            "textify-voice-asr-whisper-test-{}",
            std::process::id()
        ));
        let mgr = ModelManager::with_cache_dir(tmp.clone());
        assert_eq!(mgr.cache_dir(), tmp.as_path());
        assert!(!mgr.is_cached(ModelId::TinyEn));
        assert_eq!(
            mgr.model_path(ModelId::TinyEn),
            tmp.join("ggml-tiny.en.bin")
        );
    }

    #[test]
    fn is_cached_true_only_within_expected_size_range() {
        let tmp = std::env::temp_dir().join(format!(
            "textify-voice-asr-whisper-test-size-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).expect("create temp cache dir");
        let mgr = ModelManager::with_cache_dir(tmp.clone());
        let path = mgr.model_path(ModelId::TinyEn);

        // Too small: not cached.
        fs::write(&path, vec![0u8; 100]).expect("write stub file");
        assert!(!mgr.is_cached(ModelId::TinyEn));

        // Inside the expected range: cached.
        let (min, _max) = ModelId::TinyEn.expected_size_range();
        fs::write(&path, vec![0u8; min as usize]).expect("write stub file");
        assert!(mgr.is_cached(ModelId::TinyEn));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn new_honors_cache_dir_env_var_override() {
        let tmp = std::env::temp_dir().join(format!(
            "textify-voice-asr-whisper-test-env-{}",
            std::process::id()
        ));
        // SAFETY (test-only): no other thread in this test binary reads or
        // writes this process's environment concurrently with this test.
        unsafe {
            std::env::set_var(CACHE_DIR_ENV_VAR, &tmp);
        }
        let mgr = ModelManager::new().expect("resolve cache dir from env override");
        unsafe {
            std::env::remove_var(CACHE_DIR_ENV_VAR);
        }
        assert_eq!(mgr.cache_dir(), tmp.as_path());
    }

    #[test]
    fn expected_sha256_values_are_distinct_lowercase_64_char_hex() {
        for id in [ModelId::TinyEn, ModelId::BaseEn] {
            let sha = id.expected_sha256();
            assert_eq!(sha.len(), 64, "{id:?} sha256 must be 64 hex chars");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{id:?} sha256 must be lowercase hex, got {sha}"
            );
        }
        assert_ne!(ModelId::TinyEn.expected_sha256(), ModelId::BaseEn.expected_sha256());
    }

    // ---- base-URL policy: pure, no process env, no network ----

    #[test]
    fn base_url_falls_back_to_upstream_unless_a_real_https_override_is_given() {
        assert_eq!(resolve_base_url_from(None), MIRROR_MODEL_BASE_URL);
        assert_eq!(resolve_base_url_from(Some("")), MIRROR_MODEL_BASE_URL);
        assert_eq!(
            resolve_base_url_from(Some("https://example-r2-bucket.test/models")),
            "https://example-r2-bucket.test/models"
        );
    }

    /// The https-only rule, stated as its own test: a non-https override is
    /// *discarded* (falling back to the default source), not honored and not
    /// an error. Anything pointed at a plain-http host therefore reads from
    /// upstream instead, which is the behavior a caller has to plan around.
    #[test]
    fn a_non_https_override_is_discarded_rather_than_honored() {
        for bad in [
            "http://127.0.0.1:8080",
            "http://models.textify.me",
            "ftp://example.test/models",
            "file:///tmp/models",
        ] {
            assert_eq!(
                resolve_base_url_from(Some(bad)),
                MIRROR_MODEL_BASE_URL,
                "{bad:?} must not be used as a model source"
            );
        }
    }

    #[test]
    fn url_joins_base_and_filename_tolerating_a_trailing_slash() {
        assert_eq!(
            ModelId::TinyEn.url_from_base("https://example-r2-bucket.test/models/"),
            "https://example-r2-bucket.test/models/ggml-tiny.en.bin"
        );
        assert_eq!(
            ModelId::BaseEn.url_from_base("https://example-r2-bucket.test/models"),
            "https://example-r2-bucket.test/models/ggml-base.en.bin"
        );
    }

    #[test]
    fn the_default_source_is_the_upstream_whisper_cpp_repo() {
        assert_eq!(
            ModelId::TinyEn.url_from_base(UPSTREAM_MODEL_BASE_URL),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
        );
        assert_eq!(
            ModelId::BaseEn.url_from_base(UPSTREAM_MODEL_BASE_URL),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
        );
    }

    // ---- integrity verification: real files on disk, no network ----

    /// Known-answer check of the hash itself against values produced by
    /// `shasum -a 256` -- interop with an external tool, not just
    /// self-consistency, since a hash function that is merely consistent
    /// with itself would pin the wrong bytes just as happily.
    #[test]
    fn sha256_of_a_file_matches_an_externally_computed_digest() {
        let dir = temp_dir("sha256-known-answer");

        let empty = dir.join("empty.bin");
        fs::write(&empty, b"").expect("write empty file");
        assert_eq!(
            sha256_hex_of_file(&empty).expect("hash empty file"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let content = dir.join("content.bin");
        fs::write(&content, b"the exact expected bytes").expect("write content file");
        assert_eq!(
            sha256_hex_of_file(&content).expect("hash content file"),
            "e6008deb858ce834a44755f56b0160b488a49751006a1a7e0df4efabf8069b96"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// The invariant the SHA-256 pin exists for: bytes of exactly the right
    /// *size* but the wrong *content* -- a substituted or silently corrupted
    /// model, the one failure a size window cannot catch -- must be rejected,
    /// and the file must not survive for anything else to pick up.
    #[test]
    fn a_right_size_wrong_content_file_fails_the_sha_check_and_is_deleted() {
        let dir = temp_dir("sha-mismatch");
        let path = dir.join("ggml-tiny.en.bin.partial");
        fs::write(&path, b"the wrong bytes entirely").expect("write payload");
        let size = fs::metadata(&path).expect("stat payload").len();

        let expected_sha = "00".repeat(32);
        match verify_file_integrity(&path, (size, size), &expected_sha) {
            Err(ModelError::ShaMismatch { expected, actual }) => {
                assert_eq!(expected, expected_sha);
                assert_ne!(actual, expected, "the payload's sha must differ from the pin");
            }
            other => panic!("expected Err(ShaMismatch), got {other:?}"),
        }
        assert!(
            !path.exists(),
            "a sha-mismatched file must not be left on disk -- fail closed"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_truncated_file_fails_the_size_check_and_is_deleted() {
        let dir = temp_dir("size-mismatch");
        let path = dir.join("ggml-tiny.en.bin.partial");
        fs::write(&path, b"truncated").expect("write payload");

        match verify_file_integrity(&path, (1_000_000, 2_000_000), &"00".repeat(32)) {
            Err(ModelError::SizeMismatch { expected_range, actual }) => {
                assert_eq!(expected_range, (1_000_000, 2_000_000));
                assert_eq!(actual, 9);
            }
            other => panic!("expected Err(SizeMismatch), got {other:?}"),
        }
        assert!(!path.exists(), "a wrong-size file must not be left on disk");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_matching_both_expectations_verifies_and_survives() {
        let dir = temp_dir("verify-ok");
        let path = dir.join("ggml-tiny.en.bin.partial");
        fs::write(&path, b"the exact expected bytes").expect("write payload");
        let size = fs::metadata(&path).expect("stat payload").len();

        verify_file_integrity(
            &path,
            (size, size),
            "e6008deb858ce834a44755f56b0160b488a49751006a1a7e0df4efabf8069b96",
        )
        .expect("a byte-identical file must verify");
        assert!(path.exists(), "a verified file must be left in place");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Proves the pinned per-model values are the ones actually applied on
    /// the real download path: a stub file is far below `tiny.en`'s size
    /// window, so it must fail on the cheap check without ever hashing.
    #[test]
    fn verify_downloaded_file_applies_the_values_pinned_for_the_model() {
        let dir = temp_dir("pinned-values");
        let path = dir.join("ggml-tiny.en.bin.partial");
        fs::write(&path, b"not a model at all").expect("write stub");

        match verify_downloaded_file(ModelId::TinyEn, &path) {
            Err(ModelError::SizeMismatch { expected_range, .. }) => {
                assert_eq!(expected_range, ModelId::TinyEn.expected_size_range());
            }
            other => panic!("expected Err(SizeMismatch), got {other:?}"),
        }
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
