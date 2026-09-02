---
title: "How changes reach main"
sidebar:
  order: 31
---

# How changes reach main

Every change is a branch, a pull request, a green CI run, and a merge commit.
`main` is never committed to directly. The rule is short; what makes it worth
a page is that nothing on GitHub enforces it here, so it is enforced on the
machine instead.

## The path

```sh
git switch -c <topic>          # a hook refuses commits on main
# … commit, push …
gh pr create --fill
./merge-pr.sh                  # waits for CI, merges, returns you to main
```

`merge-pr.sh` waits for every check on the PR to pass, merges with a merge
commit, and puts the working tree back on an up-to-date `main`. It waits for
the four expected checks to register first, then refuses a PR where any of them
fails or never appears. Passing a number merges a PR other than the current
branch's — a dependabot bump, for instance.

## Why it is enforced locally

The repository is private on a plan without branch protection, so GitHub
cannot require a green check or forbid a push to `main`. Two things stand in
for that:

- `.githooks/pre-commit` refuses a commit while `main` is checked out, and
  refuses staged Rust that rustfmt would change. The global
  `core.hooksPath` dispatches to it; see [what CI checks](ci.md).
- The repository settings allow only merge commits, and delete the branch on
  merge. `gh pr merge --squash` or `--rebase` is refused by GitHub.

`gh pr merge --auto` is not used. Auto-merge needs required checks to wait on,
and with none configured it merges immediately — before CI has run, which is
exactly the gap that let `cargo fmt` drift sit on `main` for six pushes.

## Why a merge commit

A merge commit keeps the branch's history as it was reviewed. A squash loses
the sequence of decisions a branch records, and a rebase rewrites commits
whose messages cite session links and ADR numbers. Nothing here has ever
needed a linear history, and the cost of losing the branch is real every time
someone bisects.

## What is not covered

A `git push origin <topic>:main` bypasses the commit hook, and so does a merge
performed in the GitHub UI before CI finishes. Both are a choice made on
purpose by someone who can see what they are doing; the hook exists to stop
the accidental case, which is the only one that has happened.
