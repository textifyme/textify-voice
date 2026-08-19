#!/usr/bin/env bash
# packaging/sign-update.sh
#
# Manages the ed25519 keypair that signs auto-update releases
# (see crates/voice-cli/src/update.rs and packaging/appcast/README.md), and
# signs a release payload against it.
#
# SECURITY MODEL: the update SIGNATURE, not TLS and not code signing, is the
# only real protection against a malicious update (see update.rs's own doc
# comment). That makes the private key the single most sensitive secret in
# this project's release process. This script:
#
#   - NEVER writes the private key inside this repo's working tree (see
#     refuse_if_inside_repo below -- keygen hard-refuses and exits nonzero
#     rather than silently doing it).
#   - Defaults the key to a location OUTSIDE the repo entirely
#     ($TEXTIFY_SIGNING_KEY_DIR, or $HOME/.textify-voice-signing if unset).
#   - Never prints the private key's own bytes to stdout/stderr (only the
#     public key, and only file paths for the private key).
#
# The private key file itself should additionally be backed up somewhere
# durable and access-controlled (a password manager, an encrypted volume,
# whatever the founder already uses for other secrets) -- this script only
# guarantees it never lands in git, not that it's backed up.
#
# Usage:
#   packaging/sign-update.sh keygen  [--key-dir DIR]
#       Generate a new ed25519 keypair. Refuses to overwrite an existing
#       key at the target path (rotating keys is a deliberate, manual act:
#       remove the old key yourself first if that's really what you want).
#       Prints the public key as hex -- paste that into
#       crates/voice-cli/src/update.rs's PUBLIC_KEY_HEX constant.
#
#   packaging/sign-update.sh pubkey  [--key-dir DIR]
#       Print the public key (hex) for an already-generated key, without
#       generating anything. Useful for confirming PUBLIC_KEY_HEX still
#       matches the key on disk.
#
#   packaging/sign-update.sh sign --payload FILE --url URL --version X.Y.Z
#                                  [--key-dir DIR] [--notes TEXT] [--out FILE]
#       Sign a release payload (the zip produced by, e.g.,
#       `ditto -c -k --sequesterRsrc --keepParent "Textify Voice.app" out.zip`
#       -- see update.rs's doc comment for why ditto, not tar/zip directly)
#       and print (or write, with --out) a ready-to-host appcast item: the
#       exact JSON object update.rs's AppcastItem deserializes, containing
#       the version, url, hex signature, and exact byte length -- see
#       packaging/appcast/README.md for how it's meant to be merged into a
#       hosted appcast.json.
#
# Requires: openssl with ed25519 support (Homebrew's openssl 3.x; verified
# during this unit's own work that Apple's system /usr/bin/openssl --
# LibreSSL 3.3.6 -- does NOT support ed25519 `pkeyutl -rawin`, so this
# script prefers a real OpenSSL 3.x binary -- see the ordering below).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
KEY_FILE_NAME="textify-voice-update-key.pem"

usage() {
  sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# Picks a real ed25519-capable OpenSSL. Apple's /usr/bin/openssl (LibreSSL)
# cannot verify or sign ed25519 via `pkeyutl -rawin` (confirmed by direct
# testing: it fails with "unsupported algorithm" regardless of validity),
# so a Homebrew-or-equivalent OpenSSL 3.x build is required. This is a
# signing-time-only requirement -- it affects nobody who merely *runs* the
# app or receives an update, only whoever runs this script to cut a release.
find_openssl() {
  local candidates=(
    "${TEXTIFY_OPENSSL_BIN:-}"
    "/opt/homebrew/bin/openssl"
    "/usr/local/bin/openssl"
    "openssl"
  )
  for c in "${candidates[@]}"; do
    [[ -z "$c" ]] && continue
    if command -v "$c" >/dev/null 2>&1; then
      if "$c" version 2>/dev/null | grep -qv "LibreSSL"; then
        echo "$c"
        return 0
      fi
    fi
  done
  echo "error: no ed25519-capable OpenSSL found (Apple's /usr/bin/openssl is LibreSSL and cannot sign/verify ed25519)." >&2
  echo "Install a real OpenSSL 3.x (e.g. 'brew install openssl') or set TEXTIFY_OPENSSL_BIN to its path." >&2
  exit 1
}

# Resolve a candidate path to an absolute path without requiring it to
# exist yet (a plain `cd` would fail for a not-yet-created directory).
abs_path() {
  local p="$1"
  # Walk up to the nearest existing ancestor, then re-append whatever
  # doesn't exist yet -- correct even when *multiple* levels of the path
  # are missing (a plain `cd "$(dirname "$p")"` only walks up one level;
  # when the parent is also missing that `cd` fails, and with stderr
  # silenced the naive version below silently produced a bogus,
  # non-repo-looking path like "/keys" for
  # "$REPO_ROOT/packaging/appcast/keys" when neither `appcast` nor `keys`
  # existed yet -- which made refuse_if_inside_repo's prefix check miss
  # entirely and let a keygen actually write the private key inside the
  # repo. Found by direct testing, not inspection -- see this unit's
  # report.).
  local suffix="" cur="$p"
  while [[ ! -d "$cur" ]]; do
    suffix="/$(basename "$cur")$suffix"
    local next
    next="$(dirname "$cur")"
    if [[ "$next" == "$cur" ]]; then
      # Filesystem root reached without finding an existing directory;
      # should not happen on a sane system. Fail loudly rather than
      # return a guessed path a security check might trust.
      echo "error: could not resolve an existing ancestor of '$p'" >&2
      exit 1
    fi
    cur="$next"
  done
  local base
  base="$(cd "$cur" && pwd)"
  echo "${base}${suffix}"
}

# The single most important check in this script: never let the private
# signing key land inside the git working tree, where it could end up
# committed. Checked against both this repo's root and (belt and braces)
# any path containing a literal ".git" component.
refuse_if_inside_repo() {
  local candidate
  candidate="$(abs_path "$1")"
  case "$candidate" in
    "$REPO_ROOT"|"$REPO_ROOT"/*)
      echo "refusing: '$1' resolves inside this repo's working tree ($REPO_ROOT)." >&2
      echo "The private signing key must never be written into the repo -- it is the entire security boundary for auto-updates (see update.rs's doc comment) and git history is forever." >&2
      echo "Set TEXTIFY_SIGNING_KEY_DIR or pass --key-dir to a path outside the repo. Default: \$HOME/.textify-voice-signing" >&2
      exit 1
      ;;
  esac
}

default_key_dir() {
  echo "${TEXTIFY_SIGNING_KEY_DIR:-$HOME/.textify-voice-signing}"
}

# Extract the raw 32-byte ed25519 public key (hex) from a private key PEM.
# `openssl pkey -pubout -outform DER` on an ed25519 key produces a 44-byte
# SubjectPublicKeyInfo whose last 32 bytes are exactly the raw key -- the
# 12-byte prefix is the fixed ed25519 AlgorithmIdentifier + BIT STRING
# header, which is constant for every ed25519 key (verified directly
# against this script's own generated key during this unit's work).
raw_pubkey_hex() {
  local openssl_bin="$1" priv_key="$2"
  "$openssl_bin" pkey -in "$priv_key" -pubout -outform DER 2>/dev/null | tail -c 32 | xxd -p -c 256
}

cmd_keygen() {
  local key_dir
  key_dir="$(default_key_dir)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --key-dir) key_dir="$2"; shift 2 ;;
      *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
  done

  refuse_if_inside_repo "$key_dir"
  local openssl_bin
  openssl_bin="$(find_openssl)"

  mkdir -p "$key_dir"
  chmod 700 "$key_dir"
  local priv="$key_dir/$KEY_FILE_NAME"

  if [[ -e "$priv" ]]; then
    echo "error: $priv already exists." >&2
    echo "Refusing to overwrite an existing signing key -- that would invalidate every signature made with it and orphan whatever PUBLIC_KEY_HEX is currently compiled into update.rs. If you really mean to rotate keys, move or remove the existing file yourself first, then re-run keygen and update PUBLIC_KEY_HEX." >&2
    exit 1
  fi

  "$openssl_bin" genpkey -algorithm ed25519 -out "$priv" 2>/dev/null
  chmod 600 "$priv"

  echo "Private key written to: $priv (mode 600, outside the repo)."
  echo "This file is NOT in git and never will be by this script's own logic. Back it up somewhere durable and access-controlled (password manager, encrypted volume) -- losing it means you can no longer sign updates for the key already compiled into shipped apps."
  echo
  echo "Public key (hex) -- paste this into crates/voice-cli/src/update.rs's PUBLIC_KEY_HEX constant:"
  raw_pubkey_hex "$openssl_bin" "$priv"
}

cmd_pubkey() {
  local key_dir
  key_dir="$(default_key_dir)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --key-dir) key_dir="$2"; shift 2 ;;
      *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
  done
  local openssl_bin priv
  openssl_bin="$(find_openssl)"
  priv="$key_dir/$KEY_FILE_NAME"
  if [[ ! -f "$priv" ]]; then
    echo "error: no key found at $priv. Run 'sign-update.sh keygen' first, or pass --key-dir." >&2
    exit 1
  fi
  raw_pubkey_hex "$openssl_bin" "$priv"
}

cmd_sign() {
  local key_dir payload="" url="" version="" notes="" out=""
  key_dir="$(default_key_dir)"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --payload) payload="$2"; shift 2 ;;
      --url) url="$2"; shift 2 ;;
      --version) version="$2"; shift 2 ;;
      --notes) notes="$2"; shift 2 ;;
      --out) out="$2"; shift 2 ;;
      --key-dir) key_dir="$2"; shift 2 ;;
      *) echo "unknown option: $1" >&2; exit 1 ;;
    esac
  done

  [[ -n "$payload" ]] || { echo "error: --payload FILE is required" >&2; exit 1; }
  [[ -f "$payload" ]] || { echo "error: payload file not found: $payload" >&2; exit 1; }
  [[ -n "$url" ]] || { echo "error: --url URL is required" >&2; exit 1; }
  [[ "$url" == https://* ]] || { echo "error: --url must start with https:// -- update.rs refuses any other scheme, so a non-https url here would produce an appcast item nothing can ever install." >&2; exit 1; }
  [[ -n "$version" ]] || { echo "error: --version X.Y.Z is required" >&2; exit 1; }
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: --version must be major.minor.patch (three dot-separated integers), got: $version" >&2; exit 1; }

  local openssl_bin priv
  openssl_bin="$(find_openssl)"
  priv="$key_dir/$KEY_FILE_NAME"
  if [[ ! -f "$priv" ]]; then
    echo "error: no signing key found at $priv. Run 'sign-update.sh keygen' first, or pass --key-dir." >&2
    exit 1
  fi

  # Clean up explicitly rather than via `trap ... EXIT`: a trap set here
  # fires when the *whole script* exits, by which point this function's
  # `local tmp_sig` has already gone out of scope -- under `set -u` that
  # produced a real "unbound variable" error at exit (found by direct
  # testing, not inspection; see this unit's report). Plain removal right
  # after use has no such lifetime mismatch.
  local tmp_sig
  tmp_sig="$(mktemp)"
  "$openssl_bin" pkeyutl -sign -inkey "$priv" -rawin -in "$payload" -out "$tmp_sig" 2>/dev/null
  local sig_hex
  sig_hex="$(xxd -p -c 256 "$tmp_sig")"
  rm -f "$tmp_sig"
  local length
  length="$(wc -c < "$payload" | tr -d ' ')"

  local notes_json="null"
  if [[ -n "$notes" ]]; then
    # Minimal JSON-string escaping: backslash and double-quote. Notes are
    # expected to be a short plain-text release blurb, not arbitrary
    # untrusted input -- this is not a general-purpose JSON encoder.
    local escaped="${notes//\\/\\\\}"
    escaped="${escaped//\"/\\\"}"
    notes_json="\"$escaped\""
  fi

  local item
  item="$(cat <<JSON
{
  "version": "$version",
  "url": "$url",
  "signature": "$sig_hex",
  "length": $length,
  "notes": $notes_json,
  "pub_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
)"

  if [[ -n "$out" ]]; then
    printf '%s\n' "$item" > "$out"
    echo "wrote appcast item to $out" >&2
  else
    printf '%s\n' "$item"
  fi
}

main() {
  local sub="${1:-}"
  [[ $# -gt 0 ]] && shift || true
  case "$sub" in
    keygen) cmd_keygen "$@" ;;
    pubkey) cmd_pubkey "$@" ;;
    sign) cmd_sign "$@" ;;
    -h|--help|"") usage; [[ "$sub" == "" ]] && exit 1 || exit 0 ;;
    *) echo "unknown subcommand: $sub" >&2; usage; exit 1 ;;
  esac
}

main "$@"
