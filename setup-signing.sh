#!/usr/bin/env bash
# One-time: get a stable code-signing identity in place, then re-grant the tap.
#
# Why this exists: `codesign --sign -` (ad-hoc) makes the app's designated
# requirement its own cdhash. Every rebuild changes that hash, so the TCC grant
# for system-audio capture silently stops matching — the permission row still
# says "allowed" while the tap hands back nothing but zeros, with no error and
# no prompt. Signing with a certificate makes the requirement
# `identifier "uk.ambient.cli" and certificate leaf = H"..."`, which survives
# rebuilds.
#
# Two identities are possible and they are not interchangeable:
#
#   Developer ID Application — an Apple-issued certificate. The only one that
#     works on a machine that DOWNLOADED the app, because it is the only one
#     Gatekeeper accepts once the build is notarized. Use ./release.sh for that.
#
#   Ambient Dev — a local self-signed certificate. Fixes the rebuild problem on
#     THIS machine only; a build signed with it is rejected on any other. This
#     is the fallback so a contributor with no Apple account can still work.
#
# Import an Apple-issued certificate with:
#   ./setup-signing.sh --developer-id ~/Downloads/developerID_application.cer
set -euo pipefail
cd "$(dirname "$0")"

NAME="${AMBIENT_SIGN_ID:-Ambient Dev}"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# --- Developer ID import ----------------------------------------------------
if [ "${1:-}" = "--developer-id" ]; then
  CER="${2:-}"
  [ -n "$CER" ] && [ -f "$CER" ] || {
    echo "usage: $0 --developer-id <path to developerID_application.cer>" >&2
    exit 1
  }
  KEY=.signing/developerID.key
  [ -f "$KEY" ] || {
    echo "no $KEY — that private key is the other half of the CSR you uploaded" >&2
    echo "to developer.apple.com, and the certificate is useless without it." >&2
    exit 1
  }

  # Apple hands back DER; the key on disk is PEM.
  openssl x509 -inform DER -in "$CER" -out "$WORK/cert.pem" 2>/dev/null \
    || cp "$CER" "$WORK/cert.pem"

  # Refuse a mismatched pair rather than importing an identity that cannot sign.
  a=$(openssl x509 -in "$WORK/cert.pem" -noout -pubkey | openssl sha256)
  b=$(openssl pkey -in "$KEY" -pubout | openssl sha256)
  [ "$a" = "$b" ] || {
    echo "this certificate does not match $KEY — wrong .cer, or it was issued" >&2
    echo "from a different CSR than the one in .signing/." >&2
    exit 1
  }

  # -legacy: OpenSSL 3 defaults to a PKCS#12 MAC macOS cannot verify, and
  # `security import` then fails with "MAC verification failed (wrong
  # password?)" — which is a lie, the password is fine.
  openssl pkcs12 -export -legacy -inkey "$KEY" -in "$WORK/cert.pem" \
    -out "$WORK/id.p12" -passout pass:ambient -name "developerID" 2>/dev/null \
    || /usr/bin/openssl pkcs12 -export -inkey "$KEY" -in "$WORK/cert.pem" \
       -out "$WORK/id.p12" -passout pass:ambient -name "developerID"

  security import "$WORK/id.p12" -k "$KEYCHAIN" -P ambient -T /usr/bin/codesign -A

  # The leaf alone is not enough. Without Apple's Developer ID intermediate in a
  # searchable keychain, codesign fails with "unable to build chain to
  # self-signed root" and errSecInternalComponent, while `security verify-cert`
  # reports the certificate as fine — the two build the chain differently, and
  # only codesign's opinion matters. Observed on a clean machine; the identity
  # does not even appear under `find-identity -v` until this is present.
  ISSUER_OU=$(openssl x509 -inform DER -in "$CER" -noout -issuer 2>/dev/null | grep -o 'OU *= *G[0-9]' | grep -o 'G[0-9]')
  CA_URL="https://www.apple.com/certificateauthority/DeveloperID${ISSUER_OU:-G2}CA.cer"
  if ! security find-certificate -c "Developer ID Certification Authority" >/dev/null 2>&1; then
    echo "installing Apple's Developer ID ${ISSUER_OU:-G2} intermediate ..."
    if curl -fsSL -o "$WORK/ca.cer" "$CA_URL"; then
      # Only accept it if it is genuinely the issuer of the certificate above,
      # rather than trusting the URL.
      want=$(openssl x509 -inform DER -in "$CER" -noout -issuer | sed 's/^issuer=//')
      got=$(openssl x509 -inform DER -in "$WORK/ca.cer" -noout -subject | sed 's/^subject=//')
      if [ "$want" = "$got" ]; then
        security import "$WORK/ca.cer" -k "$KEYCHAIN"
      else
        echo "  downloaded intermediate is not this certificate's issuer — skipping" >&2
        echo "  wanted: $want" >&2
        echo "  got:    $got" >&2
      fi
    else
      echo "  could not fetch $CA_URL — install it by hand from" >&2
      echo "  https://www.apple.com/certificateauthority/ or signing will fail" >&2
    fi
  fi

  echo
  echo "imported. Identities now available:"
  security find-identity -v -p codesigning | sed 's/^/  /'
  echo
  echo "The private key is still plaintext at $KEY — move it into 1Password and"
  echo "delete it from disk. It can re-sign anything as you until you do."
  exit 0
fi

# --- Local self-signed fallback ---------------------------------------------
if security find-identity -v -p codesigning 2>/dev/null | grep -q "Developer ID Application"; then
  echo "a Developer ID Application identity is already present — nothing to do."
  echo "make-app.sh prefers it automatically."
  security find-identity -v -p codesigning | sed 's/^/  /'
elif security find-identity -v -p codesigning 2>/dev/null | grep -qF "$NAME"; then
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
  # See the -legacy note above.
  if ! openssl pkcs12 -export -legacy -inkey "$WORK/k.pem" -in "$WORK/c.pem" \
       -out "$WORK/id.p12" -passout pass:ambient -name "$NAME" 2>/dev/null; then
    /usr/bin/openssl pkcs12 -export -inkey "$WORK/k.pem" -in "$WORK/c.pem" \
      -out "$WORK/id.p12" -passout pass:ambient -name "$NAME"
  fi

  # Imports into the login keychain. macOS may ask you to allow this.
  security import "$WORK/id.p12" -k "$KEYCHAIN" -P ambient -T /usr/bin/codesign -A
  # Trust it for code signing so `find-identity -v` considers it valid.
  security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/c.pem" || {
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

Done. The next run prompts once for System Audio Recording — click Allow.
Because the app now has a stable signing identity, that grant survives every
later rebuild.

To verify, play some audio and run:

  open -n -a "$PWD/build/Ambient.app" --args tap /tmp/ambient-check.wav 8
  sleep 10
  build/Ambient.app/Contents/MacOS/ambient peak /tmp/ambient-check.*.wav

`ambient peak` exits non-zero if every track is silent, so this is a real check
rather than something to eyeball.

Two details in that recipe are the whole point of it, and an earlier version of
this script got both wrong:

  -n   `open -a` on an app that is already running just activates the running
       copy and silently DROPS --args, doing nothing at all.

  peak The bundle launch is the only one that holds the audio grant, and `open`
       discards its stdout — so the peak it prints goes nowhere. Reading it back
       off the written wav is the only way to see whether sound was captured.
       Do NOT "verify" by running build/Ambient.app/Contents/MacOS/ambient
       directly: TCC blames the terminal for a direct launch, so it returns
       silence by design and the check would always fail.
NOTE
