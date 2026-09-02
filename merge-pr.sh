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
# The checks this repo runs on every PR. `gh pr checks --watch` only waits
# for checks that have already been registered, and a check appears some
# seconds after the push — so a watch started straight after `gh pr create`
# sees only the instant-skip app check, returns, and the PR merges with CI
# still queued. That happened on PR #8. So: wait until every expected check
# has reported, then require each to have passed.
expected=(hygiene ui rust build)
for _ in $(seq 1 60); do
  names=$(gh pr checks "$pr" --json name --jq '.[].name' 2>/dev/null || true)
  missing=()
  for want in "${expected[@]}"; do
    grep -qx "$want" <<<"$names" || missing+=("$want")
  done
  [ "${#missing[@]}" -eq 0 ] && break
  sleep 5
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "checks never appeared on PR #$pr: ${missing[*]} — not merging." >&2
  exit 1
fi
# --fail-fast stops on the first failure.
if ! gh pr checks "$pr" --watch --fail-fast; then
  echo "" >&2
  echo "checks did not pass on PR #$pr — not merging. See: gh pr checks $pr" >&2
  exit 1
fi
failed=$(gh pr checks "$pr" --json name,bucket --jq '.[] | select(.bucket != "pass" and .bucket != "skipping") | .name')
if [ -n "$failed" ]; then
  echo "checks not passing on PR #$pr: $(tr '\n' ' ' <<<"$failed")— not merging." >&2
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
