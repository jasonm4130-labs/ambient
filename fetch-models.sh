#!/usr/bin/env bash
# Fetch the models the app runs (ADR 0020): Parakeet Ultra for words and
# Nemotron 3 Diarization fast32 for voices, ~0.8 GB, through `ambient-engine
# fetch`, which builds the helper first if needed. Every file is then checked
# against licenses/model-sha256.txt, so a change upstream stops here rather
# than reaching a bundle.
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
