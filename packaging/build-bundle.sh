#!/usr/bin/env bash
# packaging/build-bundle.sh
#
# Produces "Textify Voice.app" -- a real macOS app bundle wrapping the
# textify-voice binary, so TCC (Microphone, Accessibility) grants attach to
# an app with a stable identity instead of to whatever terminal happens to
# invoke a bare CLI binary.
#
# Usage:
#   packaging/build-bundle.sh [options]
#
# Options:
#   --no-build            Skip `cargo build --release -p voice-cli`; use
#                          whatever is already at target/release/textify-voice.
#   --out DIR              Output directory for the .app (default: packaging/dist)
#   --bundle-id ID          CFBundleIdentifier (default: com.textify.voice --
#                            see packaging/README.md before changing this)
#   --sign-identity ID      codesign identity (default: "-", i.e. ad-hoc).
#                            Pass a real "Developer ID Application: ..." identity
#                            once one exists; see packaging/README.md.
#   --zip                   Also produce "textify-voice-<version>.zip" beside the
#                            built with `ditto -c -k --keepParent` because that
#                            is what the in-app updater unpacks (see update.rs's
#                            unpack_app_zip) and what preserves the signature.
#                            THIS is the update payload.
#   --dmg                   Also produce "textify-voice-<version>.dmg" -- the download
#                            a human gets, with an /Applications symlink so the
#                            install is a drag. Not usable as an update payload.
#   -h, --help              Show this help and exit.
#
# This script only ever reads the workspace; it writes exclusively under
# packaging/ (the .app bundle is produced under --out, default packaging/dist,
# which is gitignored). It does not touch target/ except to read the binary
# cargo already built there, and does not modify any source file.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUILD=1
OUT_DIR="$SCRIPT_DIR/dist"
BUNDLE_ID="com.textify.voice"
SIGN_IDENTITY="-"
APP_DISPLAY_NAME="Textify Voice"
BUNDLE_EXECUTABLE="textify-voice"
# Download filenames are URL-safe on purpose: "Textify Voice-0.1.0.zip" forces
# %20 into the appcast URL, every curl command, and every support email, and
# one un-encoded copy of it is a 404. The volume name and the .app inside keep
# the pretty spelling -- that is what a human actually sees once mounted.
ARTIFACT_BASENAME="textify-voice"
MAKE_ZIP=0
MAKE_DMG=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) BUILD=0; shift ;;
    --zip) MAKE_ZIP=1; shift ;;
    --dmg) MAKE_DMG=1; shift ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --bundle-id) BUNDLE_ID="$2"; shift 2 ;;
    --sign-identity) SIGN_IDENTITY="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,34p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      exit 1
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: app bundles are a macOS concept; this script only runs on Darwin." >&2
  exit 1
fi

RELEASE_BIN="$REPO_ROOT/target/release/textify-voice"

# Identify the source this bundle is built from. Computed before the build,
# not after, because the binary itself needs it: crates/voice-cli's
# diagnostics module reads TEXTIFY_VOICE_GIT_SHA via `option_env!` at compile
# time and stamps it into every crash report and diagnostic bundle. Without it
# passed to cargo below, those reports say "unknown" and a user's bug report
# cannot be tied back to a build -- which is most of the point of collecting
# them.
#
# A bare SHA is not enough to identify an exact build: it says nothing about
# whether the working tree had *uncommitted* changes at build time. Two
# bundles both stamped "abc1234" could be genuinely different binaries if one
# was built with local edits on top of that commit. `git status --porcelain`
# is the real check for that (git diff alone misses untracked new files); a
# dirty tree gets a `-dirty` suffix so the ambiguity is visible instead of
# silently discarded. Empty if this isn't a git checkout (e.g. a source
# tarball).
GIT_SHA="$(cd "$REPO_ROOT" && git rev-parse --short HEAD 2>/dev/null || true)"
GIT_DIRTY=""
if [[ -n "$GIT_SHA" ]]; then
  if [[ -n "$(cd "$REPO_ROOT" && git status --porcelain 2>/dev/null)" ]]; then
    GIT_DIRTY="-dirty"
  fi
fi
BUILD_STAMP="${GIT_SHA:+$GIT_SHA$GIT_DIRTY}"

if [[ "$BUILD" == "1" ]]; then
  echo "==> cargo build --release -p voice-cli (TEXTIFY_VOICE_GIT_SHA=${BUILD_STAMP:-unknown})"
  # Cargo tracks `option_env!`/`env!` reads as build dependencies, so changing
  # this value invalidates the cached artifact and the new stamp really is
  # compiled in -- no clean build or `touch` needed.
  (cd "$REPO_ROOT" && TEXTIFY_VOICE_GIT_SHA="$BUILD_STAMP" cargo build --release -p voice-cli)
elif [[ -n "$BUILD_STAMP" ]]; then
  echo "note: --no-build -- reusing $RELEASE_BIN, whose compiled-in git SHA may predate $BUILD_STAMP." >&2
fi

if [[ ! -x "$RELEASE_BIN" ]]; then
  echo "error: $RELEASE_BIN not found or not executable. Run without --no-build, or build it first with:" >&2
  echo "  cargo build --release -p voice-cli" >&2
  exit 1
fi

# Version comes from the workspace Cargo.toml (single source of truth for
# all crates, per [workspace.package] version = "0.1.0").
CFBUNDLE_SHORT_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
if [[ -z "$CFBUNDLE_SHORT_VERSION" ]]; then
  echo "error: could not read version from $REPO_ROOT/Cargo.toml" >&2
  exit 1
fi

# CFBundleVersion (the build number) is the same source stamp computed above
# and compiled into the binary, so a bundle's Info.plist, its BuildInfo.txt,
# and the crash reports it writes all name one identical build. Falls back to
# the short version itself outside a git checkout.
CFBUNDLE_VERSION="${BUILD_STAMP:-$CFBUNDLE_SHORT_VERSION}"

# Build timestamp, UTC, ISO 8601 -- the other half of "identify an exact
# build" a git SHA alone can't give you: the SHA says which source, this
# says when this specific bundle was produced from it (two builds of the
# identical clean commit, e.g. a rebuild after a toolchain update, are
# still distinguishable). Written to BuildInfo.txt below, not into
# Info.plist -- CFBundleVersion has real ordering/uniqueness expectations
# from LaunchServices and Apple explicitly warns against stuffing
# non-version data into it.
BUILD_TIMESTAMP_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

COPYRIGHT_YEAR="$(date +%Y)"

APP_DIR="$OUT_DIR/$APP_DISPLAY_NAME.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

echo "==> assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

# The bundled executable is the exact same Mach-O binary `cargo build`
# produces for the CLI -- no wrapper script, no second binary. See
# packaging/README.md "One binary, two faces" for the argv-based contract
# that makes this file behave differently launched-as-CLI vs launched-as-app.
cp "$RELEASE_BIN" "$MACOS_DIR/$BUNDLE_EXECUTABLE"
chmod +x "$MACOS_DIR/$BUNDLE_EXECUTABLE"

sed \
  -e "s/__CFBUNDLE_IDENTIFIER__/$BUNDLE_ID/g" \
  -e "s/__CFBUNDLE_NAME__/$APP_DISPLAY_NAME/g" \
  -e "s/__CFBUNDLE_DISPLAY_NAME__/$APP_DISPLAY_NAME/g" \
  -e "s/__CFBUNDLE_EXECUTABLE__/$BUNDLE_EXECUTABLE/g" \
  -e "s/__CFBUNDLE_SHORT_VERSION__/$CFBUNDLE_SHORT_VERSION/g" \
  -e "s/__CFBUNDLE_VERSION__/$CFBUNDLE_VERSION/g" \
  -e "s/__COPYRIGHT__/Copyright (c) $COPYRIGHT_YEAR Textify. All rights reserved./g" \
  "$SCRIPT_DIR/Info.plist.template" > "$CONTENTS_DIR/Info.plist"

echo "==> plutil -lint"
plutil -lint "$CONTENTS_DIR/Info.plist"

# BuildInfo.txt: the "identify an exact build from a bug report" file. Not
# Info.plist metadata (LaunchServices/codesign have opinions about what
# belongs in CFBundleVersion -- see the comment above) -- this is a plain
# text file a user can be asked to open and paste from, or that a support
# script can `cat` over an SSH/screen-share session.
cat > "$RESOURCES_DIR/BuildInfo.txt" <<BUILDINFO
Textify Voice build info
=========================
Version:         $CFBUNDLE_SHORT_VERSION
Build:           $CFBUNDLE_VERSION
Git SHA:         ${GIT_SHA:-unknown (not a git checkout)}
Working tree:    $([ -n "$GIT_DIRTY" ] && echo "dirty (uncommitted changes present at build time)" || echo "clean")
Built (UTC):     $BUILD_TIMESTAMP_UTC
Sign identity:   $SIGN_IDENTITY
Bundle ID:       $BUNDLE_ID

Include this whole file verbatim in any bug report. If "Working tree" says
dirty, the git SHA above does not fully identify the source -- ask whoever
built it what was locally modified.
BUILDINFO
echo "==> wrote $RESOURCES_DIR/BuildInfo.txt"

# Third-party license notices -- see packaging/licenses/README.md for how
# this directory was generated and packaging/README.md "Licenses" for how
# a downloading dev finds it. Copied verbatim, not regenerated at build
# time (regeneration needs cargo-license installed and the full
# ~/.cargo/registry/src checkout, neither of which every build machine has
# -- this is committed, audited content, not derived build output).
LICENSES_SRC="$SCRIPT_DIR/licenses"
if [[ -d "$LICENSES_SRC" ]]; then
  echo "==> copying licenses to $RESOURCES_DIR/Licenses"
  mkdir -p "$RESOURCES_DIR/Licenses"
  cp -R "$LICENSES_SRC/." "$RESOURCES_DIR/Licenses/"
else
  echo "warning: $LICENSES_SRC not found -- shipping without third-party license notices." >&2
fi

# ---------------------------------------------------------------------------
# App icon.
#
# Before this, the bundle declared no CFBundleIconFile at all, so Finder drew
# the generic blank icon -- in the disk image, in Applications, and in the
# "was blocked" row in System Settings. Rendered here rather than committed as
# a binary so the mark stays diffable and regenerable; see app-icon.swift.
#
# Must happen BEFORE codesign: Resources are part of what gets sealed, and an
# icon added afterwards invalidates the signature.
# ---------------------------------------------------------------------------
ICONSET="$(mktemp -d "${TMPDIR:-/tmp}/textify-iconset.XXXXXX")/AppIcon.iconset"
mkdir -p "$ICONSET"
if swift "$SCRIPT_DIR/app-icon.swift" "$ICONSET/icon_1024.png" 1024 >/dev/null 2>&1; then
  # The names are load-bearing: iconutil rejects an iconset whose members are
  # not exactly these, and macOS picks among them by name, not by dimensions.
  for spec in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
              "128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
              "512 512x512" "1024 512x512@2x"; do
    px="${spec%% *}"; name="${spec##* }"
    sips -z "$px" "$px" "$ICONSET/icon_1024.png" --out "$ICONSET/icon_$name.png" >/dev/null 2>&1
  done
  rm -f "$ICONSET/icon_1024.png"
  if iconutil -c icns "$ICONSET" -o "$RESOURCES_DIR/AppIcon.icns" 2>/dev/null; then
    echo "==> wrote $RESOURCES_DIR/AppIcon.icns"
  else
    echo "warning: iconutil failed -- shipping without an app icon." >&2
  fi
else
  echo "warning: could not render the app icon -- shipping without one." >&2
fi
rm -rf "$(dirname "$ICONSET")"

# No --deep: there is nothing nested to sign (one executable, no frameworks,
# no helper bundles), and Apple documents --deep as a debugging convenience
# rather than a normal build step.
if [ "$SIGN_IDENTITY" = "-" ]; then
  cat >&2 <<'ADHOC'

  ---------------------------------------------------------------------------
  WARNING: ad-hoc signing (--sign-identity "-").

  macOS ties a TCC permission grant to the app's code signature. With an
  ad-hoc signature that identity is the binary's own hash, so EVERY REBUILD
  INVALIDATES MICROPHONE AND ACCESSIBILITY. System Settings keeps showing
  "Textify Voice" switched on while the new build is silently untrusted --
  which looks exactly like the app being broken.

  For day-to-day development, sign with a stable self-signed identity instead.
  One-time setup:

    Keychain Access > Certificate Assistant > Create a Certificate...
      Name:              Textify Voice Dev
      Identity Type:     Self Signed Root
      Certificate Type:  Code Signing

  then build with:

    ./packaging/build-bundle.sh --sign-identity "Textify Voice Dev"

  The grant then survives rebuilds, because the signing identity does.
  If you have already granted permissions to an ad-hoc build, clear the stale
  entries first:

    tccutil reset Microphone com.textify.voice
    tccutil reset Accessibility com.textify.voice

  ---------------------------------------------------------------------------

ADHOC
fi

echo "==> codesign --sign $SIGN_IDENTITY"
codesign --force --sign "$SIGN_IDENTITY" \
  --identifier "$BUNDLE_ID" \
  "$APP_DIR"

echo
echo "==> codesign -dv --verbose=4"
codesign -dv --verbose=4 "$APP_DIR" 2>&1

echo
echo "==> codesign --verify --deep --strict"
if codesign --verify --deep --strict --verbose=2 "$APP_DIR" 2>&1; then
  echo "codesign --verify: OK"
else
  echo "codesign --verify: FAILED" >&2
  exit 1
fi

echo
echo "==> spctl --assess --type execute"
if spctl --assess --type execute --verbose=4 "$APP_DIR" 2>&1; then
  echo "spctl: ACCEPTED (unexpected for ad-hoc -- see packaging/README.md)"
else
  echo "spctl: REJECTED -- expected for any unnotarized app (ad-hoc or self-signed)."
  echo "This does NOT block launching it directly (open, double-click while"
  echo "the quarantine bit is absent, or from Terminal); it blocks Gatekeeper's"
  echo "quarantine check on downloaded/quarantined copies. See packaging/README.md."
fi


# ---------------------------------------------------------------------------
# Distributable archives.
#
# Both are produced from the ALREADY-SIGNED bundle above -- archiving first and
# signing after would sign nothing that ships. Neither is notarized; see
# packaging/README.md for what a downloader sees as a result.
# ---------------------------------------------------------------------------

# Byte length and SHA-256 of a finished artifact. The length is not decoration:
# the appcast carries it, and update.rs checks the downloaded payload against it
# before it will even look at the signature.
report_artifact() {
  local path="$1"
  local bytes
  bytes="$(stat -f%z "$path")"
  echo "    path:   $path"
  echo "    length: $bytes"
  echo "    sha256: $(shasum -a 256 "$path" | cut -d' ' -f1)"
}

if [[ "$MAKE_ZIP" -eq 1 ]]; then
  ZIP_PATH="$OUT_DIR/$ARTIFACT_BASENAME-$CFBUNDLE_SHORT_VERSION.zip"
  echo
  echo "==> ditto -c -k --sequesterRsrc --keepParent (update payload)"
  # --keepParent is what puts "Textify Voice.app" at the top level of the
  # archive; without it ditto stores the bundle's CONTENTS at the root and
  # update.rs's unpack_app_zip rejects the payload ("no .app bundle found at
  # the top level"). Plain `zip` is not a substitute -- it does not preserve
  # the symlinks and extended attributes a signed bundle depends on, which
  # breaks the signature on the far side.
  rm -f "$ZIP_PATH"
  ditto -c -k --sequesterRsrc --keepParent "$APP_DIR" "$ZIP_PATH"
  report_artifact "$ZIP_PATH"
fi

if [[ "$MAKE_DMG" -eq 1 ]]; then
  DMG_PATH="$OUT_DIR/$ARTIFACT_BASENAME-$CFBUNDLE_SHORT_VERSION.dmg"
  echo
  echo "==> hdiutil create (human download)"
  DMG_STAGE="$(mktemp -d "${TMPDIR:-/tmp}/textify-dmg.XXXXXX")"
  DMG_RW="$(mktemp -u "${TMPDIR:-/tmp}/textify-dmg-rw.XXXXXX").dmg"
  # Trap covers the failure paths too -- a staging dir holding a copy of the app
  # is not something to leave in TMPDIR.
  trap 'rm -rf "$DMG_STAGE" "$DMG_RW"' EXIT

  # ditto, not cp -R: same reason as the zip. cp -R mangles the bundle's
  # extended attributes and the copy inside the DMG fails codesign --verify.
  ditto "$APP_DIR" "$DMG_STAGE/$APP_DISPLAY_NAME.app"
  ln -s /Applications "$DMG_STAGE/Applications"

  # --------------------------------------------------------------------------
  # Instructions inside the disk image.
  #
  # Not decoration. This build is not notarized, so the first launch is refused
  # -- and on macOS 15 (Sequoia) and later Apple REMOVED the Control-click ->
  # Open bypass, so the dialog the user gets has exactly one button on it
  # ("Done") and no hint that System Settings > Privacy & Security is where the
  # override now lives. A user who doesn't already know that concludes the app
  # is broken. The app cannot tell them itself: it was never allowed to run.
  # The disk image window is the last surface we control before that dead end.
  #
  # Two independent carriers, because the pretty one can fail: the window
  # background (needs Finder automation, which can be denied) and a plain text
  # file (always works, always visible).
  # --------------------------------------------------------------------------
  README_NAME="How to open this app.txt"
  cat > "$DMG_STAGE/$README_NAME" <<READMEEOF
Textify Voice $CFBUNDLE_SHORT_VERSION -- macOS alpha

1. Drag "Textify Voice" onto the Applications folder.

2. Open it. macOS will REFUSE the first time, with:

     "Apple could not verify Textify Voice is free of malware
      that may harm your Mac or compromise your privacy."

   This is expected. This alpha is not yet notarized by Apple, so
   macOS genuinely cannot check it and says so. The dialog has only
   a "Done" button -- click it, then do step 3.

3. Allow it, once:

     System Settings  ->  Privacy & Security
       ->  scroll down to the "Security" section
       ->  next to "Textify Voice was blocked...", click "Open Anyway"
       ->  authenticate with Touch ID or your password
       ->  click "Open Anyway" once more in the dialog that follows

   Do this within an hour of the blocked launch: the button only
   appears for a while after the attempt that triggered it. If it
   isn't there, try opening the app again to bring it back.

   You do this ONCE. It is not needed on later launches or updates.

   Faster, if you are comfortable in Terminal -- this does the same
   thing by removing the "downloaded from the internet" flag:

     xattr -dr com.apple.quarantine "/Applications/Textify Voice.app"

4. Grant Microphone, and Accessibility so it can type into other apps.
   Then hold the left Option key, speak, and release.

Why the warning exists: an Apple Developer ID costs \$99/yr and takes
days to approve. It is in progress; once this app is signed and
notarized, steps 2 and 3 disappear. In the meantime you can verify
you got the file we published -- the SHA-256 checksum is printed on
https://textify.me/voice
READMEEOF

  mkdir -p "$DMG_STAGE/.background"
  DMG_BG_OK=0
  if swift "$SCRIPT_DIR/dmg-background.swift" \
       "$DMG_STAGE/.background/background.png" "$CFBUNDLE_SHORT_VERSION" >/dev/null 2>&1; then
    DMG_BG_OK=1
  else
    echo "warning: could not render the DMG background -- shipping without it." >&2
  fi

  # Build read-write first: Finder can only record a window layout into a
  # writable volume. It is converted to compressed read-only at the end.
  rm -f "$DMG_PATH"
  hdiutil create -volname "$APP_DISPLAY_NAME" -srcfolder "$DMG_STAGE" \
    -ov -format UDRW "$DMG_RW" >/dev/null

  if [[ "$DMG_BG_OK" -eq 1 ]]; then
    echo "==> applying the DMG window layout (Finder)"
    # Mounted browsable, under /Volumes, on purpose. Finder will not persist a
    # .DS_Store for a `-nobrowse` volume, and a temp mountpoint outside /Volumes
    # breaks its file references -- either way the layout is silently dropped and
    # the DMG ships unstyled. `-noautoopen` keeps a window from popping up on
    # whoever is running the build.
    DMG_RW_MOUNT="/Volumes/$APP_DISPLAY_NAME"
    if [[ -e "$DMG_RW_MOUNT" ]]; then
      echo "error: $DMG_RW_MOUNT is already mounted; detach it and re-run." >&2
      exit 1
    fi
    hdiutil attach "$DMG_RW" -noautoopen -mountpoint "$DMG_RW_MOUNT" >/dev/null
    # Icon positions here are placed ON the artwork drawn by
    # dmg-background.swift -- the two are one layout and drift apart silently if
    # only one is edited. Timeout because Finder automation can block on a TCC
    # prompt, and a hung release build is worse than an unstyled one.
    LAYOUT_OK=1
    # `POSIX file "<mount>/..."`, not `file ".background:background.png"`: the
    # volume is mounted at a temp path rather than /Volumes/<name>, and the
    # colon-separated form resolves against the wrong root there and fails with
    # AppleScript error -10006.
    osascript <<APPLESCRIPTEOF >/dev/null 2>&1 || LAYOUT_OK=0
      tell application "Finder"
        tell disk "$APP_DISPLAY_NAME"
          open
          set current view of container window to icon view
          set toolbar visible of container window to false
          set statusbar visible of container window to false
          set the bounds of container window to {200, 120, 840, 560}
          set opts to the icon view options of container window
          set arrangement of opts to not arranged
          set icon size of opts to 96
          set background picture of opts to POSIX file "$DMG_RW_MOUNT/.background/background.png"
          set position of item "$APP_DISPLAY_NAME.app" of container window to {160, 210}
          set position of item "Applications" of container window to {480, 210}
          update without registering applications
          -- Finder writes the layout to .DS_Store lazily. Without this it is
          -- routinely still unwritten when the volume is detached, and the
          -- shipped DMG opens as a plain unstyled window with no instructions
          -- on it -- which is the entire failure this block exists to prevent.
          delay 2
          close
          delay 1
        end tell
      end tell
APPLESCRIPTEOF

    # Positioning the readme is best-effort and deliberately NOT part of the
    # check above: Finder intermittently omits a freshly-created plain file from
    # its listing of a nobrowse-mounted volume, and failing the release over
    # where an icon sits would be absurd. Unpositioned, Finder places it itself;
    # the artwork keeps that corner of the window clear for it.
    osascript <<READMEPOSEOF >/dev/null 2>&1 || true
      tell application "Finder"
        tell disk "$APP_DISPLAY_NAME"
          open
          set position of item "$README_NAME" of container window to {320, 372}
          update without registering applications
          delay 2
          close
          delay 1
        end tell
      end tell
READMEPOSEOF
    if [[ "$LAYOUT_OK" -eq 1 ]]; then
      echo "DMG window layout: applied"
    else
      # Not fatal: the .txt still ships, so the instructions still reach the
      # user. But say so loudly -- silently shipping the plain window is how a
      # release goes out looking unfinished.
      echo "warning: Finder would not apply the DMG window layout (Automation permission?)." >&2
      echo "         The disk image still contains \"$README_NAME\"." >&2
    fi
    sync
    if [[ ! -f "$DMG_RW_MOUNT/.DS_Store" ]]; then
      echo "warning: Finder did not write a .DS_Store -- the DMG will open unstyled." >&2
      LAYOUT_OK=0
    fi
    hdiutil detach "$DMG_RW_MOUNT" >/dev/null
  fi

  hdiutil convert "$DMG_RW" -format UDZO -imagekey zlib-level=9 -o "$DMG_PATH" >/dev/null
  rm -rf "$DMG_STAGE" "$DMG_RW"
  trap - EXIT
  report_artifact "$DMG_PATH"

  # The DMG is a fresh container built after signing; prove the bundle inside it
  # still verifies rather than assuming the copy was faithful.
  echo
  echo "==> verifying the signature survived the DMG round-trip"
  DMG_MOUNT="$(mktemp -d "${TMPDIR:-/tmp}/textify-dmg-mount.XXXXXX")"
  hdiutil attach "$DMG_PATH" -nobrowse -readonly -mountpoint "$DMG_MOUNT" >/dev/null
  if codesign --verify --deep --strict "$DMG_MOUNT/$APP_DISPLAY_NAME.app" 2>&1; then
    echo "codesign --verify (inside DMG): OK"
    DMG_VERIFY_OK=1
  else
    echo "codesign --verify (inside DMG): FAILED" >&2
    DMG_VERIFY_OK=0
  fi
  # The instructions are load-bearing for this release; a DMG that lost them is
  # a defect, not a cosmetic slip.
  if [[ ! -f "$DMG_MOUNT/$README_NAME" ]]; then
    echo "error: the DMG does not contain \"$README_NAME\"." >&2
    DMG_VERIFY_OK=0
  fi
  hdiutil detach "$DMG_MOUNT" >/dev/null
  rmdir "$DMG_MOUNT"
  [[ "$DMG_VERIFY_OK" -eq 1 ]] || exit 1
fi

echo
echo "==> done: $APP_DIR"
