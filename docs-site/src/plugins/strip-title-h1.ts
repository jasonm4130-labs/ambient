import { defineHastPlugin } from "satteri";

/**
 * Removes a page's leading `# Heading` from the rendered body.
 *
 * `docs/` is read two ways. On GitHub the H1 *is* the page title, so it has to
 * stay in the file. On this site Nimbus renders the title from frontmatter
 * above the content, so the same H1 appears twice. Stripping it at build time
 * is what lets one file serve both readers without either seeing something
 * wrong — the alternative was hand-editing the H1 out of 32 files and giving up
 * the GitHub view.
 *
 * A hast plugin rather than an mdast one: Sätteri never dispatches `heading` to
 * user mdast plugins (verified — a visitor returning a marker comment produced
 * nothing in the output, while the sibling `code` visitor in
 * mermaid-passthrough.ts fired normally), so headings are only reachable once
 * they are `h1` elements. Nimbus runs hast plugins before its heading-ids pass.
 *
 * A factory, with `stripped` inside it: Sätteri calls the factory once per
 * compile, which is what resets the flag per document. At module scope the
 * first page would strip its heading and every later page would keep one.
 */
export const stripTitleH1 = () => {
	let stripped = false;
	return defineHastPlugin({
		name: "ambient:strip-title-h1",
		element: {
			filter: ["h1"],
			// Only the first h1 goes. Every page in docs/ has exactly one, at the
			// top; a second would be a deliberate in-body heading and is left.
			visit(node, ctx) {
				if (stripped) return;
				stripped = true;
				ctx.removeNode(node);
			},
		},
	});
};
