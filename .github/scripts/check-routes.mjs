#!/usr/bin/env node
// Verifies the BUILT site's internal links against the routes the build really
// emitted. This is the guard for `rewrite-md-links.ts`.
//
// It exists because the failure it catches shipped once and looked fine: Nimbus
// passes a relative `troubleshooting.md` href through to the HTML untouched,
// where it resolves against the page route rather than the source directory and
// 404s. The build stays green, `astro check` stays green, and every page looks
// correct until a link is clicked — 111 links across 28 pages were broken this
// way before the plugin existed.
//
// check-links.mjs is the sibling check and a different job: it resolves links in
// the SOURCE markdown against files on disk, which is what GitHub's file view
// needs. A link can pass there and 404 here, which is precisely what happened.
//
// Two rules, because existence alone is not enough:
//   1. No relative href ending in `.md` survives in the HTML. Nimbus emits an
//      `index.md` alternate inside every page directory, so an unrewritten
//      `index.md` href — and `docs/` does link to `using/index.md` and
//      `developing/index.md` — resolves to a real file: the raw markdown rather
//      than the page. Measured: with rule 1 disabled, a planted `index.md` link
//      passes rule 2 clean.
//   2. Every internal href resolves to a built file, or a directory holding an
//      index.html. This is what holds the plugin's slug rule to the routes
//      Nimbus actually generates rather than to the rule as documented.
import { readdirSync, statSync, readFileSync, existsSync } from "node:fs";
import path from "node:path";

const DIST = process.argv[2] ?? "docs-site/dist";
const BASE = "/ambient";

if (!existsSync(DIST)) {
	console.error(`check-routes: no build at ${DIST} — run the site build first`);
	process.exit(1);
}

const walk = (dir) =>
	readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
		const p = path.join(dir, e.name);
		return e.isDirectory() ? walk(p) : [p];
	});

const all = walk(DIST);
const pages = all.filter((f) => f.endsWith(".html"));

if (pages.length === 0) {
	console.error(`check-routes: ${DIST} holds no HTML — nothing was verified`);
	process.exit(1);
}

/** A route resolves if it is a built file, or a directory with an index.html. */
const resolves = (route) => {
	if (route !== BASE && !route.startsWith(`${BASE}/`)) return false;
	const rel = decodeURIComponent(route.slice(BASE.length)).replace(/^\//, "");
	const target = path.join(DIST, rel);
	if (existsSync(target) && statSync(target).isFile()) return true;
	return existsSync(path.join(target, "index.html"));
};

const EXTERNAL = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;
const failures = [];

for (const page of pages) {
	// The page's own route directory, which relative hrefs resolve against.
	const routeDir = BASE + "/" + path.relative(DIST, path.dirname(page)).split(path.sep).join("/");
	const html = readFileSync(page, "utf8");

	for (const m of html.matchAll(/\shref="([^"]*)"/g)) {
		const href = m[1];
		if (!href || EXTERNAL.test(href) || href.startsWith("#")) continue;

		const target = href.split(/[#?]/)[0];
		if (!target) continue;

		const rootRelative = target.startsWith("/");

		// Rule 1 — a relative `.md` href is the regression, whether or not it
		// happens to land on one of Nimbus's raw-markdown alternates.
		if (!rootRelative && /\.md$/i.test(target)) {
			failures.push(`${page}: relative markdown link "${href}" — rewrite-md-links did not rewrite it`);
			continue;
		}

		// Rule 2 — everything internal must land on something that was built.
		const route = rootRelative ? target : path.posix.join(routeDir, target);
		if (!resolves(route)) {
			failures.push(`${page}: "${href}" → ${route} was not built`);
		}
	}
}

if (failures.length) {
	console.error(`check-routes: ${failures.length} broken link(s) in ${pages.length} page(s)\n`);
	for (const f of failures) console.error(`  ${f}`);
	process.exit(1);
}

console.log(`check-routes: ${pages.length} pages, every internal link resolves`);
