#!/usr/bin/env bash
# Fetch the app's models (~0.8 GB) through `ambient-engine fetch`, building the
# helper if needed, then verify each against licenses/model-sha256.txt so an
# upstream change stops here rather than reaching a bundle.
set -euo pipefail
cd "$(dirname "$0")"

engine="${AMBIENT_ENGINE:-}"
if [ -z "$engine" ]; then
  echo "building ambient-engine ..."
  engine="$(scripts/build-engine)"
fi
mkdir -p models
echo "fetching the app's models ..."
# A closed stdin is fine: `fetch` does not watch it.
"$engine" fetch --models models </dev/null
# An `if`, not `&&`: set -e does not stop a script at a failed `a && b`.
if ! grep -v '^#' licenses/model-sha256.txt | shasum -a 256 -c --quiet -; then
  echo "a model file does not match licenses/model-sha256.txt" >&2
  exit 1
fi
echo "ready: models/parakeet-ultra, models/nemotron-3-diarization (checksums verified)"
