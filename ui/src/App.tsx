import { useEffect } from "react";
import { Button } from "@/components/ui/button";

// Light/dark comes from `prefers-color-scheme` alone — no toggle. The
// listener lives here, in the page, rather than being hoisted out to dodge
// jsdom: `ui/src/test/setup.ts` stubs `matchMedia` so this still mounts under
// vitest.
function useDarkClass(): void {
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = (matches: boolean): void => {
      document.documentElement.classList.toggle("dark", matches);
    };
    apply(media.matches);
    const listener = (event: MediaQueryListEvent): void => apply(event.matches);
    media.addEventListener("change", listener);
    return () => media.removeEventListener("change", listener);
  }, []);
}

// `App` does not call `init` yet: that arrives once the Rust side of the
// bridge (unit 2) can answer it. For now this just proves the toolchain —
// React, Tailwind and a shadcn component — renders into `#root`.
export function App() {
  useDarkClass();

  return (
    <div className="flex min-h-screen items-center justify-center p-8">
      <Button>Ambient</Button>
    </div>
  );
}
