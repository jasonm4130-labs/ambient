# Building these docs

The site is [mdBook](https://rust-lang.github.io/mdBook/) with
[mdbook-mermaid](https://github.com/badboy/mdbook-mermaid). Source is the
markdown under `docs/`; `book.toml` at the repo root points `src` there, so the
ADRs keep the conventional citable path `docs/adr/NNNN-slug.md` and every
relative link works identically in GitHub's file view and on the built site.

## Build it locally

```sh
cargo install mdbook mdbook-mermaid     # or download the release binaries
mdbook serve --open                     # live reload on http://localhost:3000
mdbook build                            # → book/
```

`book/` is gitignored.

## Why mdBook

The competition was Astro Starlight, VitePress, Docusaurus, Eleventy and Zola,
and mdBook wins on what it *adds to this repo*: one pinned binary fetched by
`curl`. No `docs/package.json`, no second lockfile, no `node_modules`, no fourth
Dependabot ecosystem. Docusaurus alone is 1153 packages and 233 MB, and it fails
outright against the `"type": "module"` that `ui/package.json` already sets.

Two mdBook features earn their keep beyond that. Admonitions are native as of
0.5.0, so there is no `mdbook-admonish` to add. And an include directive can
lift a named region out of a real source file, which is the one mechanism on
offer for stopping a quoted constant drifting from `src/`. Nothing here uses one
yet, and that is deliberate: prose *about* code drifts slowly and harmlessly,
while a quoted constant that drifts is a lie, so the directive is worth reaching
for only where exactness is load-bearing. Keeping the count near zero is also
what keeps a later move to another generator cheap.

The honest weakness is that mdBook's default theme is the Rust Book's theme. It
is legible, familiar and dark-mode capable, and for a private repo read by a
handful of people that is enough. Overriding it means editing `index.hbs` and
the CSS, and the 0.5 theme files changed incompatibly from 0.4, so any override
is pinned to the 0.5 line.

## Diagrams

Diagrams are fenced ` ```mermaid ` blocks in the markdown. mdBook has no native
Mermaid support — a bare fence renders as a literal `<pre><code>` block — so
`mdbook-mermaid` is a required preprocessor, declared in `book.toml`, with
`mermaid.min.js` and `mermaid-init.js` committed under `docs/`.

Authoring them as text rather than as image files buys two things. They diff in
review, and **GitHub renders them natively in `.md` files**, so `docs/` is a
readable architecture document to anyone browsing the repo whether or not the
site is built. That also bounds the risk of depending on a single-maintainer
preprocessor: if `mdbook-mermaid` ever strands, the failure mode is unrendered
fences on the site, not lost content, and the fix is deleting one stanza from
`book.toml`.

Use plain `flowchart` with `subgraph` boundaries rather than Mermaid's
`C4Context` syntax, which upstream still labels experimental with a syntax that
may change. Diagrams are parse-checked in CI — see the build guard below.

## The build guard

**mdBook exits 0 on a broken include and ships the literal directive into the
HTML.** Measured, not assumed: pointing an include at a nonexistent file logged
`ERROR Error updating …` followed by two `WARN Caused By:` lines, exited 0, and
left the raw directive in the rendered page. Neither half of that is visible in
the exit code, so `docs.yml` checks both — it fails if the build log contains an
`ERROR` line, and it greps the rendered HTML for a directive that survived.

The grep is deliberately narrower than the obvious one. This very page mentions
the directive, so matching on the opening braces alone would fail the build on
its own documentation; the pattern instead requires a directive word followed by
an argument, which only a real unexpanded include produces.

A third check parses every ` ```mermaid ` block with the real Mermaid parser.
`mdbook-mermaid` only *wraps* a fence in `<pre class="mermaid">` — it never
parses the diagram — so a syntax error builds cleanly, passes every other guard
here, and renders as an error box on the published page. It earned its place
immediately: it caught a node named `call`, which is a reserved word in
flowcharts because of the `call` click directive, and which reads as completely
ordinary in a diff. Note the shape of that bug, because it is the reason the
check exists rather than a style rule — `call["Meeting app"]` parses fine, and
only a later bare reference to `call` fails.

This is the one step in the docs workflow that needs node, which is a real cost
against the argument for mdBook above. It is a pinned two-package `npm ci` in
`.github/scripts/`, deliberately separate from `ui/` because it is a CI tool and
not a dependency of anything that ships.

A fourth check asserts that every `docs/adr/[0-9]*.md` appears in
`docs/SUMMARY.md`, because a record absent from the nav is invisible on the site
even though the file is right there in the repo.

## Deployment

Not wired up yet, and the decision is Jason's to make.

GitHub Pages is a hard block rather than a preference: `ambient` is private in
an org on the free plan, and Pages is unavailable for private repos there. Even
on a paid plan the site would be world-readable — private Pages needs Enterprise
Cloud.

The intended target is **Cloudflare Workers Static Assets** by direct upload,
on Cloudflare's own instruction that new projects should use Workers rather than
Pages. It would live in its own `docs.yml` rather than in `ci.yml`, for two
mechanical reasons: `ci.yml` sets `cancel-in-progress: true` at workflow scope,
which can kill a deploy mid-upload, and `paths:` filters are per-workflow rather
than per-job.

Two things must be settled before it is turned on:

1. **`workers.dev` URLs are public by default and this is a private repo.**
   Cloudflare Access has to be attached to the Worker first, scoped to account
   membership or an email domain. Free-tier Zero Trust seat availability is
   unverified.
2. **The two secrets** — `CLOUDFLARE_API_TOKEN` (needs Workers Scripts:Edit) and
   `CLOUDFLARE_ACCOUNT_ID` — get seeded from 1Password rather than pasted:
   `op read 'op://…/CLOUDFLARE_API_TOKEN' | gh secret set CLOUDFLARE_API_TOKEN`.

Until then `docs.yml` builds and verifies the site on every change and uploads
nothing.
