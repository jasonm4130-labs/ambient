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
//      Absolute URLs on this site count as internal, and so do the head's
//      `content` URLs (og:image, meta refresh): Nimbus 0.18 began prefixing the
//      base onto markdownUrl and socialImage itself, and a doubled
//      `/ambient/ambient/` there only shows up in absolute metadata URLs.
//
// It also asserts that each published URL now served by a redirect is still
// emitted, pointing where it should. Nothing on the site links to those URLs
// any more, so rule 2 would not notice one disappearing.
import { readdirSync, statSync, readFileSync, existsSync } from "node:fs";
import path from "node:path";

const DIST = process.argv[2] ?? "docs-site/dist";
const BASE = "/ambient";
const SITE = "https://jasonm4130-labs.github.io";
// Old published route → where it now redirects (astro.config.ts `redirects`).
const REDIRECTS = { "/ambient/index/": "/ambient/overview/" };

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

	const contentUrls = [...html.matchAll(/\scontent="(?:\d+;\s*url=)?([^"]*)"/gi)]
		.map((m) => m[1])
		.filter((url) => url.startsWith("/") || url.startsWith(`${SITE}/`));
	const hrefs = [...html.matchAll(/\shref="([^"]*)"/g)].map((m) => m[1]);

	for (const url of [...hrefs, ...contentUrls]) {
		const href = url.startsWith(`${SITE}/`) ? url.slice(SITE.length) : url;
		if (!href || EXTERNAL.test(href) || href.startsWith("#")) continue;
		// 404.html names itself /404/ in its canonical and og:url. GitHub Pages
		// serves it for any miss, never by that route.
		if (path.basename(page) === "404.html" && href === `${BASE}/404/`) continue;

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

for (const [from, to] of Object.entries(REDIRECTS)) {
	const page = path.join(DIST, from.slice(BASE.length), "index.html");
	if (!existsSync(page)) {
		failures.push(`${from}: redirect to ${to} was not emitted`);
		continue;
	}
	const html = readFileSync(page, "utf8");
	const refresh = html.match(/http-equiv="refresh" content="\d+;\s*url=([^"]*)"/i)?.[1];
	const canonical = html.match(/rel="canonical" href="([^"]*)"/)?.[1];
	if (refresh !== to) failures.push(`${page}: refresh goes to "${refresh}", expected "${to}"`);
	if (canonical !== SITE + to) failures.push(`${page}: canonical is "${canonical}", expected "${SITE + to}"`);
}

if (failures.length) {
	console.error(`check-routes: ${failures.length} failure(s) in ${pages.length} page(s)\n`);
	for (const f of failures) console.error(`  ${f}`);
	process.exit(1);
}

console.log(`check-routes: ${pages.length} pages, every internal link resolves, ${Object.keys(REDIRECTS).length} redirect(s) emitted`);
