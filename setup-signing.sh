#!/usr/bin/env bash
# One-time: create a stable local code-signing identity, then re-grant the tap.
#
# Why this exists: `codesign --sign -` (ad-hoc) makes the app's designated
# requirement its own cdhash. Every rebuild changes that hash, so the TCC grant
# for system-audio capture silently stops matching — the permission row still
# says "allowed" while the tap hands back nothing but zeros, with no error and
# no prompt. Signing with a certificate makes the requirement
# `identifier "uk.ambient.cli" and certificate leaf = H"..."`, which survives
# rebuilds.
set -euo pipefail
cd "$(dirname "$0")"

NAME="${AMBIENT_SIGN_ID:-Ambient Dev}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if security find-identity -v -p codesigning 2>/dev/null | grep -qF "$NAME"; then
  echo "identity '$NAME' already present"
else
  echo "creating self-signed code-signing certificate '$NAME' ..."
  cat > "$WORK/ext.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = v3
[dn]
CN = $NAME
[v3]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$WORK/k.pem" -out "$WORK/c.pem" -config "$WORK/ext.cnf" 2>/dev/null
  # -legacy matters: OpenSSL 3 defaults to an AES/SHA-256 PKCS#12 MAC that
  # macOS's Security framework cannot verify, and `security import` fails with
  # "MAC verification failed (wrong password?)" — which is a lie, the password
  # is fine. Fall back to the system LibreSSL if this build has no -legacy.
  if ! openssl pkcs12 -export -legacy -inkey "$WORK/k.pem" -in "$WORK/c.pem" \
       -out "$WORK/id.p12" -passout pass:ambient -name "$NAME" 2>/dev/null; then
    /usr/bin/openssl pkcs12 -export -inkey "$WORK/k.pem" -in "$WORK/c.pem" \
      -out "$WORK/id.p12" -passout pass:ambient -name "$NAME"
  fi

  # Imports into the login keychain. macOS may ask you to allow this.
  security import "$WORK/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P ambient -T /usr/bin/codesign -A
  # Trust it for code signing so `find-identity -v` considers it valid.
  security add-trusted-cert -r trustRoot -p codeSign \
    -k "$HOME/Library/Keychains/login.keychain-db" "$WORK/c.pem" || {
      echo "NOTE: could not set trust automatically — open Keychain Access, find"
      echo "      '$NAME', and set 'Code Signing' to 'Always Trust'."
    }
fi

echo
echo "identities now available:"
security find-identity -v -p codesigning | sed 's/^/  /'

echo
echo "rebuilding with the stable identity ..."
./make-app.sh

echo
echo "clearing the stale system-audio grant ..."
tccutil reset AudioCapture uk.ambient.cli || true

cat <<'NOTE'

Done. Next run will prompt once for System Audio Recording — click Allow.
Because the app now has a stable signing identity, that grant survives every
later rebuild. Verify with:

  ./make-app.sh
  build/Ambient.app/Contents/MacOS/ambient tap /tmp/check.wav 8
  # play something during those 8 seconds; ch1 should be non-zero
NOTE
