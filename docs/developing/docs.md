---
title: "Building these docs"
sidebar:
  order: 31
---

# Building these docs

The site uses [Nimbus](https://nimbus-docs.com), an Astro documentation framework,
with the [Cirrus](https://github.com/jasonm4130/cirrus) theme. The Astro project
lives in `docs-site/`; canonical Markdown stays in `docs/` so the guides and
[decision records](../adr/README.md) also work in GitHub's file view.

## Build and check

From the repository root, with Node 24 installed:

```sh
npm ci --prefix docs-site
npm run typecheck --prefix docs-site
npm run build --prefix docs-site
npm run lint:docs --prefix docs-site
npm ci --prefix .github/scripts
node .github/scripts/check-links.mjs
node .github/scripts/check-mermaid.mjs
node .github/scripts/check-routes.mjs
```

For a live preview, run `npm run dev --prefix docs-site` and open the local URL
printed by Astro. Build output is in `docs-site/dist/`. The build also copies
the project license and third-party notices into the artifact; footer links
work in offline previews and hosted copies alike.

The generated `dist/`, `node_modules/`, `.astro/` and `.nimbus/` directories are
gitignored. Edit source files, not their generated copies.

## Author a page

Add Markdown under `docs/` with a frontmatter `title` and `sidebar.order` where
the page belongs in navigation. Keep the leading `# Heading`: GitHub uses it,
and the site removes the duplicate after rendering its frontmatter title.
Use relative `.md` links to other source pages and meaningful image alt text.
Use synthetic data in screenshots.

The content loader in `docs-site/src/content.config.ts` reads the sibling
`docs/` directory. Three plugins in `docs-site/src/plugins/` adapt that source
for the site:

| Plugin | Purpose |
| --- | --- |
| `rewrite-md-links.ts` | Converts relative Markdown links to the emitted site routes, including lowercased slugs. |
| `strip-title-h1.ts` | Removes the first body H1 when the layout already supplies the page title. |
| `mermaid-passthrough.ts` | Preserves Mermaid source in a `pre.mermaid` block before syntax highlighting. |

Keep all three when upgrading Nimbus. A successful content build alone does
not prove that source links point to the correct HTML pages.

## Diagrams

Write diagrams in fenced `mermaid` blocks. GitHub renders them in source view;
the site loads the committed Mermaid runtime only on pages that need it.
`docs-site/public/mermaid-init.js` renders diagrams after Astro navigation and
rerenders them when the theme changes.

Prefer ordinary `flowchart` diagrams with `subgraph` boundaries. Verify diagrams
with `check-mermaid.mjs` and inspect the result in the browser for clipped labels,
readability and theme contrast. Update the bundled runtime and its license
notice together.

## What CI checks

The docs job typechecks and builds the site, then checks source links, emitted
routes and Mermaid syntax. It also asserts that fenced diagrams reach the
renderer and that ADR pages have navigation ordering. See [CI](ci.md) for the
job filters and aggregate merge gate.

`check-links.mjs` validates source targets. `check-routes.mjs` validates links in
the built HTML and catches ordinary navigation accidentally pointing to a raw
Markdown alternate. Both checks are needed because the same source serves
GitHub and the site.

## Source timestamps

`docs-site/src/lib/last-updated.ts` queries Git for each actual Markdown source
file; frontmatter can override the result. A source archive without Git history
has no inferred timestamp. CI fetches history for the docs build.

## Deployment

The documentation publishes to [GitHub Pages](https://jasonm4130-labs.github.io/ambient/).
The `ci` workflow deploys changes from `main` only after the docs job and aggregate
`gate` pass. Pull requests build a review artifact but cannot deploy.

Astro's site origin is `https://jasonm4130-labs.github.io` and its base path is
`/ambient`. Keep navigation, search, images and Markdown alternates under that
base path. The route check rejects links that escape it.

GitHub Pages must use **GitHub Actions** as its publishing source. The deployment
job uses the `github-pages` environment and narrowly scoped `pages: write` and
`id-token: write` permissions. It needs no stored deployment credential.

To redeploy the current `main`, run the `ci` workflow manually. The manual run
always builds docs and still requires the aggregate gate before deployment.
