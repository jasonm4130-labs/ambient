import { useEffect, useState } from "react";
import { useBridge } from "@/lib/bridge-context";

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

type Route = "sessions" | "settings";

interface InitReply {
  route: Route;
}

interface NavigatePayload {
  page: Route;
}

// The route is a placeholder heading in this unit — the real Sessions and
// Settings screens arrive in units 3 and 4. `App` re-requests `init`'s answer
// once, on mount, and otherwise only switches route on a `navigate` event: it
// keeps no state the bridge did not just hand it.
export function App() {
  useDarkClass();
  const bridge = useBridge();
  const [route, setRoute] = useState<Route>();

  useEffect(() => {
    let live = true;
    bridge.call<InitReply>("init").then((reply) => {
      if (live) setRoute(reply.route);
    });
    return () => {
      live = false;
    };
  }, [bridge]);

  useEffect(() => bridge.on("navigate", (payload) => {
    const { page } = payload as NavigatePayload;
    setRoute(page);
  }), [bridge]);

  if (route === undefined) {
    return (
      <div className="flex min-h-screen items-center justify-center p-8">
        <p className="text-muted-foreground text-sm">Loading…</p>
      </div>
    );
  }

  return (
    <div className="flex min-h-screen items-center justify-center p-8">
      <h1 className="text-2xl font-semibold">{route === "settings" ? "Settings" : "Sessions"}</h1>
    </div>
  );
}
