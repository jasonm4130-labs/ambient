---
title: "Building these docs"
sidebar:
  order: 31
---

# Building these docs

The site is [Nimbus](https://nimbus-docs.com) — Cloudflare's Astro-based docs
framework — themed with [Cirrus](https://github.com/jasonm4130/cirrus). The
Astro project lives in `docs-site/`; **the markdown does not.** `docs/` at the
repo root stays the source of truth, and `docs-site/` reads it from outside its
own tree.

## Build it locally

```sh
cd docs-site
npm ci
npm run dev             # http://localhost:4321
npm run build           # → docs-site/dist/
npx astro check         # typecheck
```

`docs-site/dist/`, `node_modules/`, `.astro/` and `.nimbus/` are gitignored.

## Why the markdown stays in `docs/`

Nimbus expects content at `src/content/docs/`. Moving it there would have cost
three things, and none of them was worth the tidiness:

- **The ADRs would lose their citable path.** `adr/README.md` makes the
  filename the record's identity — "never reused and never renamed" — and
  commit messages cite by number. `docs/adr/NNNN-slug.md` is that path.
- **Fifteen inbound links** point into `docs/adr/` from across the tree.
- **`docs/` would stop being readable in GitHub's file view**, which is where
  these pages get read when nobody has built the site.

So `docs-site/src/content.config.ts` escapes its own project instead.
`docsCollection` builds its loader base as `` `./src/content/${base}` ``, so
`base: "../../../docs"` resolves to the sibling directory. It is a relative
escape rather than a documented feature, and it is load-bearing — if a Nimbus
upgrade changes how that base is computed, the build fails loudly at content
sync rather than silently rendering nothing.

## Three plugins, because `docs/` is read two ways

Every page has to work in GitHub's file view *and* on the site, and each reader
needs something the other does not. All three plugins are in
`docs-site/src/plugins/`.

**`rewrite-md-links.ts`** turns a relative `troubleshooting.md` href into the
route Nimbus built for it, `/using/troubleshooting/`. GitHub needs the `.md`
form — it resolves to the sibling file — but Nimbus passes that href through to
the HTML untouched, where the browser resolves it against the *page route* and
gets a 404. This is not a theoretical gap: 111 links across 28 of ~30 pages
shipped broken this way, with the build and every other guard green.

Moving `docs/` into `docs-site/src/content/docs/` does **not** fix it. That was
tested with a three-page fixture in the framework-native location, and the hrefs
shipped verbatim there too — link rewriting is not a function of where the
content lives. The mapping lowercases, because Nimbus slugs `adr/README.md` to
`/adr/readme/`.

**`strip-title-h1.ts`** removes the leading `# Heading` from the rendered body.
On GitHub the H1 is the page title, so it must stay in the file; on the site
Nimbus renders the title from frontmatter, so the same H1 would appear twice.
It is a *hast* plugin, and that is the version verified to work end to end. An
earlier version of this page justified the choice by claiming Sätteri — Astro
7's default markdown processor, which Nimbus wires in — never dispatches
`heading` to user mdast plugins. **That is false**: a probe plugin counted 186
`heading` dispatches in one build. The mdast attempt failed for some other
reason, which was never established; the measurement that "showed" it was a
marker-comment visitor whose output proved nothing.

**`mermaid-passthrough.ts`** rewrites a ` ```mermaid ` fence to a raw
`<pre class="mermaid">` at the mdast stage, before Shiki sees it. This one has
to be mdast for a reason that was checked: once Shiki has run, the diagram
source is spread across per-token `<span>`s.

## Diagrams

Nimbus has **no Mermaid support.** It registers `mermaid` as a Shiki
*language*, so an untouched fence ships as a syntax-highlighted code listing
with no diagram anywhere in the HTML — verified before any of this was written,
by grepping a built page for `aria-roledescription`, the marker Mermaid's own
SVG output writes, and finding zero.

So the fences are rewritten by the plugin above and rendered in the browser by
`docs-site/public/mermaid-init.js`. That script is not the upstream
`mdbook-mermaid` one: Nimbus stamps `data-mode="dark"` on `<html>` rather than
using mdBook's theme element ids, so the theme is observable and diagrams
re-render in place instead of forcing a reload; and Nimbus mounts
`<ClientRouter />`, so navigation never fires `DOMContentLoaded` and the hook is
`astro:page-load`. `mermaid.min.js` is 2.6 MB and only four of ~30 pages carry a
diagram, so it is fetched on demand rather than shipped with every page.

The alternative was `rehype-mermaid`, which renders at build time. It was
rejected for one reason: it needs a headless browser in CI.

Authoring them as text rather than as image files buys two things. They diff in
review, and **GitHub renders them natively in `.md` files**, so `docs/` is a
readable architecture document to anyone browsing the repo whether or not the
site is built.

Use plain `flowchart` with `subgraph` boundaries rather than Mermaid's
`C4Context` syntax, which upstream still labels experimental with a syntax that
may change.

## The build guards

The `docs` job in `ci.yml` runs six checks, and each one exists because something got through
without it.

**Every mermaid fence reached the renderer.** Counts pages with a fence in
`docs/` against pages with a `<pre class="mermaid">` in `dist/`, and fails if a
`data-nb-lang="mermaid"` survives anywhere — that attribute *is* the failure,
because it means Shiki got the fence. The grep is restricted to `--include='*.html'`:
Nimbus emits an `.md` and an `.mdx` alternate beside every page, so an
unrestricted count sees each page two or three times and the comparison becomes
meaningless. That mistake was made once here already.

**`check-mermaid.mjs`** parses every fence with the real Mermaid parser. The
passthrough plugin only *moves* a fence; it never parses the diagram, so a
syntax error builds cleanly and renders as an error box in the browser. It
earned its place immediately: it caught a node named `call`, a reserved word in
flowcharts because of the `call` click directive, which reads as completely
ordinary in a diff. Note the shape of that bug — `call["Meeting app"]` parses
fine, and only a later bare reference to `call` fails.

**`check-links.mjs`** resolves every relative markdown link in `docs/` and
`README.md`. Nothing else does: `nimbus-docs lint` is MDX-only and takes no
path, so it never sees `docs/**/*.md`; the Astro build renders a dead link
happily; and `README.md` sits outside every content pipeline in the repo.

**`check-routes.mjs`** asks the other half of that question: every link in the
*built* HTML must land on a route the build actually emitted. A link can pass
`check-links.mjs` and still 404 on the site, which is exactly how the 111 broken
links above survived. It has two rules, because existence alone is not enough —
Nimbus emits an `index.md` alternate inside every page directory, so an
unrewritten `index.md` href resolves to a real file (the raw markdown) and
existence-checking calls it a pass. Both directions are proven: removing
`rewrite-md-links.ts` from `astro.config.ts` makes the check report exactly
those 111 links.

**Every ADR is a nav-ordered page.** A record with no frontmatter fails the
content schema and takes the build down with it, but one that builds can still
have no `sidebar.order` and land unplaced in the nav. The guard asserts the
record *count* before looping, because `nullglob` off plus an empty directory
used to make it pass vacuously.

**`astro check`** typechecks the project.

## Why not mdBook

This site was mdBook until 2026-08-30, and the argument for it was real: one
pinned binary fetched by `curl`, no `docs/package.json`, no second lockfile, no
`node_modules`. That argument was made when the docs had no design of their own
and mdBook's default Rust Book theme was good enough.

Cirrus is what changed it. Theming mdBook means editing `index.hbs` and the CSS
against a theme layout that broke incompatibly between 0.4 and 0.5, so every
override is pinned to a line that will break again. Nimbus takes the theme as
CSS custom properties, which is what Cirrus already is.

The costs are paid honestly and are listed in
[ADR-0013](../adr/0013-nimbus-over-mdbook.md): a third npm ecosystem in
Dependabot, a lockfile, and a build that needs node. Two of those were already
true — `ui/` and `.github/scripts/` are both npm, and `check-mermaid.mjs` always
needed node.

## Deployment

Not wired up yet, and the decision is Jason's to make. Nothing about the move
to Nimbus changed this section.

GitHub Pages is a hard block rather than a preference: `ambient` is private in
an org on the free plan, and Pages is unavailable for private repos there. Even
on a paid plan the site would be world-readable — private Pages needs Enterprise
Cloud.

The intended target is **Cloudflare Workers Static Assets**, which is what
`docs-site/wrangler.jsonc` already points at (`assets.directory: ./dist`), on
Cloudflare's own instruction that new projects should use Workers rather than
Pages.

Two things must be settled before it is turned on:

1. **`workers.dev` URLs are public by default and this is a private repo.**
   Cloudflare Access has to be attached to the Worker first, scoped to account
   membership or an email domain. Free-tier Zero Trust seat availability is
   unverified.
2. **The two secrets** — `CLOUDFLARE_API_TOKEN` (needs Workers Scripts:Edit) and
   `CLOUDFLARE_ACCOUNT_ID` — get seeded from 1Password rather than pasted:
   `op read 'op://…/CLOUDFLARE_API_TOKEN' | gh secret set CLOUDFLARE_API_TOKEN`.

Until then the `docs` job builds and verifies the site on every change and uploads
the result as an artifact. The `site` value in `docs-site/astro.config.ts` is a
placeholder, and canonical URLs, the sitemap, `robots.txt` and the links in
`/llms.txt` are all wrong until the Worker exists and that value is corrected.
