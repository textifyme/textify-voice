#!/usr/bin/env bash
# Create a stable self-signed code-signing identity for local development.
#
# WHY THIS EXISTS: macOS ties a TCC permission grant (Microphone,
# Accessibility) to the app's code signature. Under ad-hoc signing that
# identity is the binary's own hash, so every rebuild orphans the grant --
# System Settings keeps showing the app switched on while the new build is
# silently untrusted, which is indistinguishable from the app being broken.
#
# Signing with a stable identity fixes that: the signature changes on each
# build, but the *identity* does not, so the grant follows.
#
# Keychain Access > Certificate Assistant can do this by hand, but it is easy
# to get wrong (the certificate must be type "Code Signing", and a self-signed
# root additionally needs a trust setting before `codesign` will accept it --
# without it you get "no identity found" even though the cert exists).
set -euo pipefail

NAME="${1:-Textify Voice Dev}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

if security find-identity -v -p codesigning 2>/dev/null | grep -q "$NAME"; then
  echo "identity \"$NAME\" already exists and is valid -- nothing to do."
  exit 0
fi

echo "==> generating a self-signed code-signing certificate: $NAME"
openssl req -x509 -newkey rsa:2048 -keyout "$TMP/k.pem" -out "$TMP/c.pem" \
  -days 3650 -nodes -subj "/CN=$NAME" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" 2>/dev/null

# macOS's `security` cannot read OpenSSL 3's default PKCS12 MAC, hence the
# explicit legacy algorithms. Without these the import fails with a misleading
# "MAC verification failed (wrong password?)".
openssl pkcs12 -export -inkey "$TMP/k.pem" -in "$TMP/c.pem" -out "$TMP/c.p12" \
  -passout pass:textify -name "$NAME" \
  -macalg sha1 -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES 2>/dev/null

echo "==> importing into the login keychain"
security import "$TMP/c.p12" -k ~/Library/Keychains/login.keychain-db \
  -P textify -T /usr/bin/codesign -A

# A self-signed root is imported untrusted; codesign then reports
# CSSMERR_TP_NOT_TRUSTED and `find-identity -v` shows zero identities.
echo "==> trusting it for code signing"
security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db "$TMP/c.pem"

echo
security find-identity -v -p codesigning
echo
echo "Now build with:"
echo "  ./packaging/build-bundle.sh --sign-identity \"$NAME\""
echo
echo "Because the signature changes, clear any grant made against an older build:"
echo "  tccutil reset Microphone com.textify.voice"
echo "  tccutil reset Accessibility com.textify.voice"
