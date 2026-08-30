// Renders the <pre class="mermaid"> blocks that src/plugins/mermaid-passthrough.ts
// emits. Nimbus ships no Mermaid support, so without this the fences would be
// syntax-highlighted code listings.
//
// Not the upstream mdbook-mermaid init: that one keys on mdBook's own theme
// element ids (mdbook-theme-ayu and friends) and reloads the page to restyle.
// Two things here are different because Nimbus is different — it stamps
// data-mode="dark" on <html>, so the theme is observable and diagrams can be
// re-rendered in place; and it mounts <ClientRouter />, so navigation does not
// fire DOMContentLoaded and the hook has to be astro:page-load.
//
// mermaid.min.js is 2.6 MB and only five of ~30 pages carry a diagram, so it is
// fetched on demand rather than shipped with every page.
(() => {
	const SRC = "/mermaid.min.js";
	const sources = new WeakMap();
	let loading = null;

	const load = () =>
		(loading ??= new Promise((resolve, reject) => {
			const s = document.createElement("script");
			s.src = SRC;
			s.onload = resolve;
			s.onerror = () => reject(new Error(`failed to load ${SRC}`));
			document.head.appendChild(s);
		}));

	const isDark = () =>
		document.documentElement.dataset.mode === "dark" ||
		(!document.documentElement.dataset.mode &&
			window.matchMedia("(prefers-color-scheme: dark)").matches);

	async function render() {
		const blocks = document.querySelectorAll("pre.mermaid");
		if (!blocks.length) return;
		await load();

		// Mermaid replaces the element's content with an <svg>, so the diagram
		// source has to be kept before the first render or a theme change has
		// nothing left to re-render from.
		for (const el of blocks) {
			if (!sources.has(el)) sources.set(el, el.textContent);
			el.innerHTML = sources.get(el);
			el.removeAttribute("data-processed");
		}

		window.mermaid.initialize({
			startOnLoad: false,
			theme: isDark() ? "dark" : "default",
			securityLevel: "strict",
		});
		await window.mermaid.run({ nodes: blocks });
	}

	// astro:page-load fires on the initial load and on every client-side
	// navigation; the DOMContentLoaded branch is the no-ClientRouter fallback.
	document.addEventListener("astro:page-load", render);
	if (document.readyState !== "loading") render();
	else document.addEventListener("DOMContentLoaded", render, { once: true });

	// The theme toggle mutates data-mode rather than navigating, so re-render
	// instead of waiting for a page load that never comes.
	new MutationObserver(render).observe(document.documentElement, {
		attributes: true,
		attributeFilter: ["data-mode"],
	});
})();
