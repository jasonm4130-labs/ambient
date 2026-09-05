#!/usr/bin/env bash
# Kept for the docs and the pre-commit hook that name ./merge-pr.sh; the script
# itself moved to loop/ with the rest of Nightshift.
exec "$(dirname "$0")/loop/merge-pr.sh" "$@"
