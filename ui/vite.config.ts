import { defineConfig } from "vite";
import { viteSingleFile } from "vite-plugin-singlefile";

// One self-contained file: `src/settings.rs` embeds the result with
// `include_str!`, and a WKWebView loaded via `loadHTMLString` has no origin to
// fetch siblings from, so nothing may stay external.
export default defineConfig({
  plugins: [viteSingleFile()],
  build: {
    target: "safari18",
    assetsInlineLimit: Infinity,
    cssCodeSplit: false,
    reportCompressedSize: false,
  },
});
