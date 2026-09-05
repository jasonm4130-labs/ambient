import "@testing-library/jest-dom/vitest";

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
