import { defineConfig } from "astro/config";
import tailwindcss from "@tailwindcss/vite";
import nimbus, { defineConfig as defineNimbusConfig } from "@cloudflare/nimbus-docs";
import { tableScroll } from "@cloudflare/nimbus-docs/markdown";
import { stripTitleH1 } from "./src/plugins/strip-title-h1";
import { mermaidPassthrough } from "./src/plugins/mermaid-passthrough";

const nimbusConfig = defineNimbusConfig({
  // Nothing is deployed yet, so this origin is provisional — it drives
  // canonical URLs, absolute OG image URLs, robots.txt, the sitemap and the
  // links in /llms.txt, and every one of them is wrong until the Worker exists.
  // GitHub Pages is not an option: ambient is private in a free-plan org.
  // See docs/developing/docs.md for what has to be settled first.
  site: "https://ambient-docs.jasonm4130.workers.dev",
  title: "Ambient",
  description: "Local-first ambient capture for macOS: records, transcribes and attributes office conversations and calls entirely on-device.",
  locale: "en",
  github: "https://github.com/jasonm4130-labs/ambient",
  socialImageAlt: "Ambient documentation",
});

export default defineConfig({
  output: "static",
  // Tailwind v4 via its Vite plugin (the integration Astro recommends for
  // Tailwind v4 — replaces the PostCSS plugin, which doesn't build under
  // Astro 7's Vite 8 bundler).
  vite: {
    plugins: [tailwindcss()],
  },
  // Hover-prefetch link targets so full-page navigations feel instant without
  // a client-side router.
  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover",
  },
  integrations: [
    nimbus(nimbusConfig, {
      // Authoring rules are opt-in by design — your repo, your taste. The
      // two below are the load-bearing pair: frontmatter has to validate
      // against the content schema for the page to render properly, and
      // broken internal links are 404s for your readers. Add the others
      // (heading hierarchy, code-block language, style, etc.) when you're
      // ready to enforce them — see `nimbus-docs lint --help`.
      rules: {
        "nimbus/frontmatter-shape": "error",
        "nimbus/internal-link": "error",
      },
      // Wrap wide tables so they scroll instead of overflowing the page
      // (styled by `.nb-table-scroll` in src/styles/prose.css).
      markdown: {
        hastPlugins: [tableScroll(), stripTitleH1],
        // mermaid has to act at mdast, before Shiki turns the fence into
        // highlighted markup; the H1 strip has to act at hast, because Sätteri
        // never dispatches `heading` to user mdast plugins. Both exist because
        // `docs/` is read two ways — in GitHub's file view and on this site —
        // and each reader needs something the other does not. See the plugin
        // files for the reasoning.
        mdastPlugins: [mermaidPassthrough],
      },
    }),
  ],
});
