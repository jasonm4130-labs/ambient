#!/usr/bin/env bash
# Merge a pull request into main the one way this repo allows.
#
#   ./merge-pr.sh [<pr-number>]      defaults to the PR for the current branch
#
# Waits for every CI check on the PR to pass, merges with a merge commit,
# lets GitHub delete the branch, then puts the working tree back on an
# up-to-date main. Refuses if any check fails or is missing.
#
# Why a script and not `gh pr merge --auto`: auto-merge needs branch
# protection with required checks to wait on, and this repo's plan has no
# branch protection. Without it `--auto` merges at once, before CI has run —
# which is how cargo fmt drift sat on main for six pushes. The wait has to be
# here. Why a merge commit and not squash or rebase: the repo settings allow
# nothing else, so the PR's history survives as it was reviewed.
set -euo pipefail

pr="${1:-}"
if [ -z "$pr" ]; then
  pr=$(gh pr view --json number --jq .number 2>/dev/null || true)
  if [ -z "$pr" ]; then
    echo "no PR for the current branch — pass a number, or: gh pr create" >&2
    exit 1
  fi
fi

base=$(gh pr view "$pr" --json baseRefName,headRefName,state --jq '"\(.state) \(.baseRefName) \(.headRefName)"')
read -r state base_ref head_ref <<<"$base"
if [ "$state" != "OPEN" ]; then
  echo "PR #$pr is $state, not open" >&2
  exit 1
fi
if [ "$base_ref" != "main" ]; then
  echo "PR #$pr targets $base_ref, not main" >&2
  exit 1
fi

echo "PR #$pr: $head_ref → main. Waiting for checks…"
# --fail-fast stops on the first failure; a PR with no checks at all is also
# a failure, because a merge nothing verified is the thing this script exists
# to prevent.
if ! gh pr checks "$pr" --watch --fail-fast; then
  echo "" >&2
  echo "checks did not pass on PR #$pr — not merging. See: gh pr checks $pr" >&2
  exit 1
fi
count=$(gh pr checks "$pr" --json name --jq 'length')
if [ "$count" -eq 0 ]; then
  echo "PR #$pr has no checks — not merging." >&2
  exit 1
fi

gh pr merge "$pr" --merge --delete-branch=false
echo "merged PR #$pr with a merge commit"

# Back to an up-to-date main. `--ff-only` so a local main that has somehow
# diverged is reported, not silently merged.
git switch main
git pull --ff-only origin main
# GitHub deletes the remote branch on merge (repo setting); tidy the local one
# if it was ours and is fully merged.
if [ "$head_ref" != "main" ] && git show-ref --verify --quiet "refs/heads/$head_ref"; then
  git branch -d "$head_ref" || true
fi
git fetch --prune origin
