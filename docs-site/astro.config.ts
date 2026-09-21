import { defineConfig } from "astro/config";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import nimbus, { defineConfig as defineNimbusConfig } from "@cloudflare/nimbus-docs";
import { tableScroll } from "@cloudflare/nimbus-docs/markdown";
import { stripTitleH1 } from "./src/plugins/strip-title-h1";
import { mermaidPassthrough } from "./src/plugins/mermaid-passthrough";
import { rewriteMdLinks } from "./src/plugins/rewrite-md-links";

const nimbusConfig = defineNimbusConfig({
  site: "https://jasonm4130-labs.github.io",
  title: "Ambient",
  description: "Local-first ambient capture for macOS: records, transcribes and attributes office conversations and calls entirely on-device.",
  locale: "en",
  github: "https://github.com/jasonm4130-labs/ambient",
  socialImageAlt: "Ambient documentation",
});

// Nimbus rewrites some rendered content links after the Markdown plugins.
// Normalize those emitted attributes before indexing the static artifact.
async function prefixBuiltHtml(dir: URL): Promise<void> {
  const root = fileURLToPath(dir);
  const walk = async (current: string): Promise<void> => {
    for (const entry of await readdir(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isDirectory()) await walk(path);
      else if (entry.name.endsWith(".html")) {
        const html = await readFile(path, "utf8");
        const prefixed = html.replace(
          /((?:href|src)=["'])\/(?!\/|ambient(?:\/|["']))/g,
          "$1/ambient/",
        );
        if (prefixed !== html) await writeFile(path, prefixed);
      }
    }
  };
  await walk(root);
}

export default defineConfig({
  output: "static",
  base: "/ambient/",
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
    {
      name: "ambient:github-pages-base",
      hooks: {
        "astro:build:done": async ({ dir }) => prefixBuiltHtml(dir),
      },
    },
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
        hastPlugins: [tableScroll(), stripTitleH1, rewriteMdLinks("/ambient/")],
        // mermaid has to act at mdast, before Shiki turns the fence into
        // highlighted markup. The H1 strip acts at hast — not because `heading`
        // is unreachable at mdast (it is dispatched; that earlier claim was
        // wrong), but because the hast version is the one verified to work.
        // These exist because `docs/` is read two ways — in GitHub's file view
        // and on this site — and each reader needs something the other does
        // not. See the plugin files for the reasoning.
        mdastPlugins: [mermaidPassthrough],
      },
    }),
  ],
});
