# The Textify Voice appcast

This is the document `crates/voice-cli/src/update.rs`'s `check_for_update`
fetches (over HTTPS only) to learn whether a newer release exists. It is
hosted wherever releases are hosted (a static file on the same place the
release `.zip`s live -- an R2/S3 bucket, GitHub Pages, whatever) -- nothing
in this repo serves it; this directory only holds the **template** and the
tooling that produces a real one.

## Why the update signature is the whole security story

The app is self-signed (no Apple Developer ID yet), so macOS code signing
gives an update payload **zero** protection: whoever can write to wherever
this JSON file is hosted can point `url` at any binary they like. TLS on
the appcast/download fetches only proves "unmodified in transit from
whichever server answered" -- not "from the real developer." The **ed25519
signature on each item, verified against the public key compiled into the
app**, is the only real boundary. Every other design choice in this
directory and in `update.rs` exists to protect that one property. See
`update.rs`'s own doc comment for the full breakdown.

## Format

```json
{
  "items": [
    {
      "version": "0.2.0",
      "url": "https://downloads.textify.me/voice/textify-voice-0.2.0.zip",
      "signature": "<128 hex chars -- 64-byte ed25519 signature over the exact bytes at url>",
      "length": 12345678,
      "notes": "optional human-readable release notes",
      "pub_date": "2026-08-18T00:00:00Z"
    }
  ]
}
```

- **`version`** -- `major.minor.patch` (three dot-separated non-negative
  integers, nothing else -- no `v` prefix, no prerelease suffix). Compared
  numerically, not lexicographically (`1.9.0 < 1.10.0`).
- **`url`** -- where to download the release payload. **Must be
  `https://`** -- `update.rs` refuses anything else before it dials, both
  for this URL and for the appcast's own fetch URL.
- **`signature`** -- the detached ed25519 signature over the *exact* bytes
  served at `url`, as 128 lower-case hex characters. Produced by
  `../sign-update.sh sign`, never by hand.
- **`length`** -- the exact byte size of the payload at `url`. Checked
  before signature verification, as a cheap early rejection of an
  obviously truncated download; the signature check is what actually
  matters and runs regardless of whether this catches anything.
- **`notes`** / **`pub_date`** -- optional, informational only. Not
  security-relevant, not currently surfaced anywhere in the app's UI
  (that's a separate, not-yet-built unit; see this repo's
  `crates/voice-cli/src/update.rs` doc comment for what does and doesn't
  exist yet).

`items` is an array so a future appcast can list more than one historical
release, but today's tooling (`sign-update.sh sign`) only ever emits one
item at a time, and `update.rs`'s `latest_item` always picks whichever
entry has the highest `version` -- so the simplest correct thing to host
is **one item: the current latest release.** Keeping old items around is
harmless (they're just never selected) if you want a changelog-shaped
history instead.

`appcast.template.json` in this directory intentionally does **not**
parse successfully as-is: its `signature` field is the literal string
`REPLACE_WITH_128_HEX_CHAR_ED25519_SIGNATURE_FROM_SIGN_UPDATE_SH`, not
valid hex. This is deliberate, not an oversight -- `update.rs`'s appcast
parser validates that every item's signature is well-formed hex of the
right length, so a real appcast that still had the placeholder text in it
(a "forgot to fill this in" release mistake) would be caught as a
malformed appcast rather than silently served. Confirmed directly: feeding
the template's literal contents through `update_module::parse_appcast`
returns `Err(MalformedAppcast(..))` (bad hex encoding), not a panic and not
a wrongly-accepted document.

## Cutting a release: the actual workflow

This is the whole sequence, in order. Steps 1-2 are one command now: the
archiving that used to be a hand-run `ditto` line lives in the build script.

1. **Bump the version** in the workspace `Cargo.toml`. `update.rs`'s
   `Version::current()` reads `CARGO_PKG_VERSION`, so this is what the
   shipped build compares the appcast against, and
   `build-bundle.sh` names the artifacts from it.

2. **Build, sign, and archive:**

   ```sh
   packaging/build-bundle.sh --sign-identity "Textify Voice Dev" --zip --dmg
   ```

   `--zip` runs `ditto -c -k --sequesterRsrc --keepParent` -- not `zip` or
   `tar`, because ditto is the one archiving method confirmed by round-trip
   testing against this repo's actual bundle to preserve everything
   `codesign --verify --deep --strict` cares about, and `--keepParent` is
   what puts `Textify Voice.app` at the archive root where
   `update.rs`'s `unpack_app_zip` requires exactly one top-level `.app`.
   The script prints each artifact's byte length and SHA-256; keep them.

   **Sign with the same identity every time.** macOS keys TCC grants
   (Microphone, Accessibility) to the code signature, so a release signed
   with a different identity than the one it replaces silently loses both
   permissions on every user's machine -- the switches keep showing as on
   while the new build is untrusted, which looks exactly like the app being
   broken.

3. **Upload the payload first, appcast second.** In that order: an appcast
   naming a URL that 404s turns every user's background check into a
   failure, and the window where that is true should be zero.

   ```sh
   npx wrangler r2 object put "textify-downloads/voice/textify-voice-<VER>.zip" \
     --file "packaging/dist/textify-voice-<VER>.zip" \
     --content-type application/zip \
     --cache-control "public, max-age=31536000, immutable" --remote
   npx wrangler r2 object put "textify-downloads/voice/textify-voice-<VER>.dmg" \
     --file "packaging/dist/textify-voice-<VER>.dmg" \
     --content-type application/x-apple-diskimage \
     --cache-control "public, max-age=31536000, immutable" --remote
   ```

   Immutable caching is correct because the keys carry the version: a
   release is never overwritten in place, it is superseded.

4. **Sign the payload and build the appcast:**

   ```sh
   packaging/sign-update.sh sign \
     --payload "packaging/dist/textify-voice-<VER>.zip" \
     --url https://downloads.textify.me/voice/textify-voice-<VER>.zip \
     --version <VER> \
     --notes "What changed, in one sentence a user would care about." \
     --out appcast-item.json
   ```

   Wrap that item in `{"items": [ ... ]}` and upload it as
   `voice/appcast.json` with a SHORT cache lifetime -- `max-age=300`, not
   the immutable header the payloads get. The appcast is the one mutable
   object in the bucket; cache it hard and a published fix reaches nobody.

5. **Update the download page.** `apps/web/src/pages/voice.astro` holds
   `VERSION`, `DMG_SHA256`, and `DMG_SIZE` as constants, and the checksum it
   publishes is what people verify their download against. A stale checksum
   there is worse than none: it tells an honest user their good download is
   corrupt.

6. **Verify against the live URL, not the local file:**

   ```sh
   ./target/release/textify-voice update-check
   ```

   An older build should now report the new version as available. Checking
   only the file you just uploaded proves nothing about what the CDN serves.

7. **Never sign a version that is not strictly greater than the previous
   release.** Nothing stops you generating a valid signature over an old
   build, but `update.rs`'s `evaluate_appcast` and `download_and_verify`
   both refuse to treat a non-newer `version` as installable -- so it would
   be a signed item that never does anything, not a rollback. An emergency
   rollback has to be re-released under a new, higher version number.

## Where the private key lives

Nowhere in this repo, ever. `packaging/sign-update.sh keygen` refuses to
write it inside the working tree (checked at multiple path-resolution
levels, not just a string prefix match) and defaults to
`$HOME/.textify-voice-signing`, overridable via `$TEXTIFY_SIGNING_KEY_DIR`.
See that script's own header comment for the full security rationale --
the short version is that this key, not TLS and not code signing, is what
actually stops someone else from shipping updates to Textify Voice's
users, so it deserves the same handling as any other root-of-trust secret
(offline backup, restricted access, no CI environment variable, etc.).

The **public** key is the opposite: it's meant to be public, and it is
baked into the app at compile time as
`crates/voice-cli/src/update.rs::PUBLIC_KEY_HEX`. Rotating keys means
generating a new keypair, updating that constant, and shipping a build
with the new constant *signed by the old key* (so existing installs can
still verify the update that carries the new key going forward) -- there
is no other migration path today, which is worth knowing before an
emergency rotation, not during one.
