---
title: "13. Nimbus and Cirrus, not mdBook"
sidebar:
  order: 53
---

# 13. Nimbus and Cirrus, not mdBook

**Status:** accepted · **Date:** 2026-08-30 · **Supersedes:** —

## Context and Problem Statement

The docs site was mdBook, chosen for what it *added to this repo*: one pinned
binary fetched by `curl`, no `docs/package.json`, no second lockfile, no
`node_modules`. That reasoning was sound and it is why the earlier page argued
against Astro Starlight by name.

It was also made when the docs had no visual identity of their own, so
mdBook's default Rust Book theme was good enough. Cirrus is what changed the
premise. Theming mdBook means editing `index.hbs` and the theme CSS, and the
0.5 theme files changed incompatibly from 0.4 — so any override is pinned to a
line that has already broken once. Cirrus is a flat file of CSS custom
properties, which is a shape mdBook has nowhere to accept.

## Considered Options

**Stay on mdBook and override the theme.** Cheapest in dependencies, most
expensive in coupling: the override targets a template layout upstream has
already broken once, and Cirrus would have to be hand-translated out of custom
properties into whatever `index.hbs` wants.

**Nimbus, with the markdown moved into `docs-site/src/content/docs/`.** The
framework-native layout. Rejected on the ADRs: `adr/README.md` makes the
filename the record's citable identity, fifteen links point into `docs/adr/`,
and `docs/` would stop being readable in GitHub's file view.

**Nimbus, reading `docs/` from outside its own tree.** Chosen.

## Decision Outcome

The site is Nimbus themed with Cirrus, in `docs-site/`. **The markdown stays in
`docs/` and does not move.** `docs-site/src/content.config.ts` reaches it with
`docsCollection({ base: "../../../docs" })`, which works because
`docsCollection` builds its loader base as `` `./src/content/${base}` ``.

Two build-time plugins keep one file serving two readers: the H1 stays in the
file for GitHub and is stripped from the rendered body, and a ` ```mermaid `
fence stays a fence in the file and is rewritten for the client-side renderer.
Both are described in [building these docs](../developing/docs.md).

The fact that decided it: **Nimbus does not render Mermaid** — it registers
`mermaid` as a Shiki *language*, so an untouched fence ships as a code listing.
That was measured on a built page before any of this was committed, by grepping
for `aria-roledescription` and finding zero. Had it not been fixable in a
plugin, this decision would have gone the other way, because five diagrams
turning into code blocks is a worse outcome than an ugly theme.

## Consequences

A third npm ecosystem in `.github/dependabot.yml`, a second lockfile, and a
docs build that needs node. Two of those three were already true — `ui/` and
`.github/scripts/` are both npm, and `check-mermaid.mjs` always needed node —
so the honest new cost is the lockfile and roughly 400 packages.

The relative escape in `content.config.ts` is not a documented Nimbus feature.
It is load-bearing, and a Nimbus upgrade that changes how the loader base is
computed will break it. The failure is loud — content sync fails and the build
stops — rather than a site that renders nothing, which is the only reason this
is acceptable.

`book.toml` and `docs/SUMMARY.md` are gone. Nav order now lives in each page's
`sidebar.order` frontmatter, so the CI guard that asserted every ADR appeared in
`SUMMARY.md` was rewritten to assert frontmatter instead.

## Confirmation

the `docs` job of `.github/workflows/ci.yml` (its own workflow, `docs.yml`, until 2026-09-04). Three of its checks fail if this decision drifts:

- **Every mermaid fence reached the renderer** compares fence count in `docs/`
  against `<pre class="mermaid">` count in `dist/`, and fails outright if a
  `data-nb-lang="mermaid"` survives — that attribute means Shiki got the fence,
  which is precisely the failure this ADR exists to prevent.
- **`check-links.mjs`** resolves every relative link in `docs/` and `README.md`,
  which is the guard that catches the ADRs' citable paths being moved.
- **Every ADR is a nav-ordered page** asserts `title:` and `sidebar.order` on
  each record, after asserting the record count so it cannot pass vacuously.
