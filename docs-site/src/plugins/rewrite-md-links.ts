import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineHastPlugin } from "satteri";

/**
 * Rewrites relative `foo.md` links to the routes Nimbus actually builds.
 *
 * This is the third plugin the "one file, two readers" design needs. A link
 * written as `[when it does not work](troubleshooting.md)` is what GitHub's
 * file view requires — it resolves to the sibling file. Nimbus passes that
 * href through to the rendered HTML untouched, where it resolves against the
 * *page route* instead: from `/using/getting-started/` the browser asks for
 * `/using/getting-started/troubleshooting.md` and gets a 404. Measured on the
 * built site before this plugin existed: 111 such links across 28 of ~30 pages,
 * i.e. essentially every cross-reference in the docs.
 *
 * Moving `docs/` into `docs-site/src/content/docs/` does NOT fix this — that
 * was tested with a three-page fixture in the framework-native location and the
 * hrefs shipped verbatim there too. Link rewriting is not a function of where
 * the content lives, so the escape hatch in content.config.ts stays and this
 * plugin is what closes the gap.
 *
 * The mapping is `docs/`-relative path → lowercased route, `index` dropped:
 *   docs/using/troubleshooting.md  → /using/troubleshooting/
 *   docs/using/index.md            → /using/
 *   docs/adr/README.md             → /adr/readme/
 * The lowercasing is not cosmetic — Nimbus slugs `README.md` to `/adr/readme/`,
 * so a case-preserving rewrite would 404 on exactly one page and look fine on
 * the other 34. `check-routes.mjs` is the guard that holds this mapping to the
 * routes the build really emits, rather than to the rule described here.
 */

const DOCS_ROOT = fileURLToPath(new URL("../../../docs/", import.meta.url));

/** Anything already addressed absolutely: `https:`, `mailto:`, `//host`, `/abs`, `#frag`. */
const NOT_RELATIVE = /^(?:[a-z][a-z0-9+.-]*:|\/\/|\/|#)/i;

/** `docs/`-relative filesystem path → built route, or null if it escapes `docs/`. */
const routeFor = (absTarget: string): string | null => {
	const rel = path.relative(DOCS_ROOT, absTarget);
	if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) return null;
	const segments = rel
		.replace(/\.md$/i, "")
		.split(path.sep)
		.map((s) => s.toLowerCase());
	// A directory's index page is the directory route: `using/index` → `/using/`.
	if (segments[segments.length - 1] === "index") segments.pop();
	return segments.length ? `/${segments.join("/")}/` : "/";
};

export const rewriteMdLinks = defineHastPlugin({
	name: "ambient:rewrite-md-links",
	element: {
		filter: ["a"],
		visit(node, ctx) {
			const href = node.properties?.href;
			if (typeof href !== "string" || NOT_RELATIVE.test(href)) return;

			// Split `page.md#section` into target and the suffix to carry over.
			const parts = href.match(/^([^#?]*)([#?].*)?$/);
			const target = parts?.[1];
			const suffix = parts?.[2] ?? "";
			if (!target || !/\.md$/i.test(target)) return;

			if (!ctx.fileURL) {
				ctx.report({
					message: `cannot rewrite "${href}": the compile supplied no fileURL`,
					node,
					severity: "warning",
				});
				return;
			}

			const from = path.dirname(fileURLToPath(ctx.fileURL));
			const route = routeFor(path.resolve(from, target));
			if (!route) {
				// Outside docs/ — a link into src/ or the repo root. Left alone
				// rather than guessed at, and surfaced so it is not silent.
				ctx.report({
					message: `link "${href}" resolves outside docs/; left unrewritten`,
					node,
					severity: "warning",
				});
				return;
			}

			ctx.setProperty(node, "href", route + suffix);
		},
	},
});
