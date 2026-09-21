<!-- codex-baseline:1 -->
# Ambient

Use native Codex instructions and tools for this repository. These instructions
supersede inherited legacy agent-routing and cross-provider-review guidance.
Preserve Claude-owned files; do not load their instructions, hooks or memory.
Preserve the pre-existing untracked `.codex/` files; they are not part of a PR.

- macOS is required for the Rust targets. `scripts/check` runs format, Clippy,
  all-target tests and the native SF Symbol check. Use an isolated
  `CARGO_TARGET_DIR` if a shared Cargo cache serves stale artifacts.
- UI checks: in `ui/`, run `pnpm run check`, `pnpm run lint`, `pnpm run test`
  and `pnpm run build`. Commit the generated `assets/settings.html` with UI edits.
- Docs checks: in `docs-site/`, run `npm run typecheck`, `npm run build` and
  `npm run lint:docs`; then run the three `.github/scripts/check-*.mjs` scripts
  from the repository root. `docs/` is the canonical Markdown source.
- Capture requires a signed app launched through LaunchServices. A build or
  synthetic test does not prove live audio capture. Never record real audio as
  part of an automated check.
- Work on a branch. Preserve the green `gate` check and merge-commit workflow.
  Publishing, notarization, history rewriting and live settings changes require
  scope from the user. Do not launch the legacy unattended loop.
- GitHub issues track current work; `docs/plans/` and ADRs retain historical
  decisions. See `CONTRIBUTING.md` for the contributor workflow.
