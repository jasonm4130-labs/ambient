// Parse every ```mermaid block in docs/ with the real Mermaid parser.
//
// docs-site/src/plugins/mermaid-passthrough.ts only rewrites a fence to
// <pre class="mermaid">; it never parses the diagram. A syntax error therefore
// builds cleanly, passes every other guard in this workflow, and renders as an
// error box in the reader's browser.
// This caught a node named `call` — a reserved word in flowcharts, because of
// the `call` click directive — which read as perfectly ordinary in the diff.
import { JSDOM } from "jsdom";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const dom = new JSDOM("<!doctype html><body></body>", { pretendToBeVisual: true });
for (const key of ["window", "document", "navigator", "Element", "SVGElement",
                   "HTMLElement", "getComputedStyle", "requestAnimationFrame",
                   "MutationObserver", "Node"]) {
  if (globalThis[key] === undefined && dom.window[key] !== undefined) {
    globalThis[key] = dom.window[key];
  }
}

const mermaid = (await import("mermaid")).default;
mermaid.initialize({ startOnLoad: false });

const walk = (dir) =>
  readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    return statSync(path).isDirectory() ? walk(path) : path.endsWith(".md") ? [path] : [];
  });

let total = 0;
let failed = 0;
for (const file of walk("docs")) {
  const blocks = [...readFileSync(file, "utf8").matchAll(/```mermaid\n([\s\S]*?)```/g)];
  for (const [index, [, body]] of blocks.entries()) {
    total++;
    try {
      await mermaid.parse(body);
    } catch (error) {
      failed++;
      console.log(`::error file=${file}::mermaid block #${index + 1}: ${error.message}`);
    }
  }
}

console.log(`${total} diagram(s), ${failed} failed`);
process.exit(failed ? 1 : 0);
