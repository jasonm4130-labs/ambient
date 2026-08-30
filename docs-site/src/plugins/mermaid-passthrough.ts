import { defineMdastPlugin } from "satteri";

/**
 * Turns a ```mermaid fence into a raw `<pre class="mermaid">` so the
 * client-side renderer in `public/mermaid-init.js` can pick it up.
 *
 * Nimbus has no Mermaid support — it registers `mermaid` as a Shiki *language*,
 * so an untouched fence ships as a syntax-highlighted code listing with no
 * diagram anywhere in the HTML. Verified before writing this: the probe's built
 * `developing/capture/index.html` carried `data-nb-lang="mermaid"` and zero
 * `aria-roledescription`, which is the marker Mermaid's own SVG output writes.
 *
 * This runs at the mdast stage, before Shiki sees the node, which is the same
 * move `mdbook-mermaid` makes. It is deliberately the only one: the fences stay
 * diffable text, GitHub keeps rendering them natively in `docs/`, and
 * `.github/scripts/check-mermaid.mjs` still parses the source. `rehype-mermaid`
 * would render at build time instead and drag a headless browser into CI.
 */
const escapeHtml = (s: string) =>
	s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export const mermaidPassthrough = () =>
	defineMdastPlugin({
		name: "ambient:mermaid-passthrough",
		code(node) {
			if (node.lang !== "mermaid") return;
			return { rawHtml: `<pre class="mermaid">${escapeHtml(node.value)}</pre>` };
		},
	});
