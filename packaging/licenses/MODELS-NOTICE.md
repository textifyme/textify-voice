# ggml model weights

Textify Voice does not bundle a speech model in the app itself. On first
use, `crates/voice-asr-whisper/src/model.rs` downloads one `ggml-*.bin` file
(`ggml-tiny.en.bin`, ~75 MB, or `ggml-base.en.bin`, ~148 MB, the default) and
caches it under this Mac's platform data directory.

- **Publisher**: the `ggerganov/whisper.cpp` project on HuggingFace --
  ggml-format conversions of OpenAI's own Whisper model weights.
- **License**: MIT. Confirmed live against the HuggingFace API on
  2026-08-18: `GET https://huggingface.co/api/models/ggerganov/whisper.cpp`
  returns `"cardData": {"license": "mit", ...}` and `"tags": [..., "license:mit"]`.
- **Where the download comes from**: see `TEXTIFY_MODEL_BASE_URL` below --
  the base URL is configurable (default: the HuggingFace repo above).
- **Integrity**: each model's SHA-256 is pinned in `ModelId::expected_sha256`
  in `model.rs` and checked after every download, regardless of which base
  URL served the bytes. A mismatch deletes the file and fails the download
  -- it never hands a wrong/corrupted/substituted model to whisper.cpp.

## Model hosting: HuggingFace fallback, R2 override

First-run downloads pull from a third party's repo by default. If that repo
is ever renamed, moved, or rate-limits us, every new install breaks at
once. `TEXTIFY_MODEL_BASE_URL` overrides the base URL with no code change:

```sh
export TEXTIFY_MODEL_BASE_URL="https://<your-r2-bucket-or-domain>"
```

`<base>/<filename>` is requested for each model (e.g.
`<base>/ggml-base.en.bin`). See `packaging/README.md` "Model hosting" for
the exact R2 upload steps and why the SHA-256 pin still applies to an
R2-hosted copy.
