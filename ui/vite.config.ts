/// <reference types="vitest/config" />
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { viteSingleFile } from "vite-plugin-singlefile";

// One self-contained file: `src/settings.rs` embeds the result with
// `include_str!`, and a WKWebView loaded via `loadHTMLString` has no origin to
// fetch siblings from, so nothing may stay external.
//
// Vitest's config lives here rather than in a separate `vitest.config.ts` so
// the `@/` alias and the React plugin cannot drift between build and test.
export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  resolve: {
    alias: {
      "@": new URL("./src", import.meta.url).pathname,
    },
  },
  build: {
    target: "safari18",
    assetsInlineLimit: Infinity,
    cssCodeSplit: false,
    reportCompressedSize: false,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
  },
});
