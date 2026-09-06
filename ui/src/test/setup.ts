import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";
import "@testing-library/jest-dom/vitest";

// `vite.config.ts`'s `test` block has no `globals: true`, so
// `@testing-library/react`'s own auto-cleanup — which looks for a global
// `afterEach` — never registers. Without this, a second `it()` in the same
// file renders on top of the first's still-mounted DOM, and a query can
// silently resolve to the earlier test's stale element instead of throwing on
// an ambiguous match.
afterEach(() => {
  cleanup();
});

// jsdom has no `matchMedia`; the page's dark-class listener
// (`prefers-color-scheme`) needs one to mount at all under the test
// environment, so this stays a stub here rather than the listener moving out
// of the page to dodge it.
if (typeof window.matchMedia !== "function") {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList;
}
