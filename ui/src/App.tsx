import { useCallback, useEffect, useState } from "react";
import { useBridge } from "@/lib/bridge-context";
import { Sessions } from "@/pages/Sessions";
import { Settings } from "@/pages/Settings";
import type { HealthCheck } from "@/pages/Welcome";
import { useLatest } from "@/lib/latest";

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

export function App() {
  useDarkClass();
  const bridge = useBridge();
  const [route, setRoute] = useState<Route>();
  const [checks, setChecks] = useState<HealthCheck[]>();
  const loadHealth = useLatest(useCallback(() => bridge.call<HealthCheck[]>("doctor"), [bridge]));
  const refreshHealth = useCallback(() => {
    void loadHealth()
      .then((next) => {
        if (next !== undefined) setChecks(next);
      })
      .catch((e: unknown) =>
        setChecks([
          { name: "doctor", ok: false, detail: e instanceof Error ? e.message : String(e) },
        ]),
      );
  }, [loadHealth]);

  useEffect(() => {
    if (route === "sessions") refreshHealth();
  }, [route, refreshHealth]);
  useEffect(() => bridge.on("config", refreshHealth), [bridge, refreshHealth]);

  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      if (!event.metaKey || event.altKey || event.shiftKey) return;
      const key = event.key.toLowerCase();
      if (![",", "0", "r", "s"].includes(key)) return;
      event.preventDefault();
      if (key === "," || key === "0") setRoute(key === "," ? "settings" : "sessions");
      else
        void bridge.call(key === "r" ? "record.start" : "record.stop").catch((e: unknown) => {
          setChecks([{ name: "recording", ok: false, detail: String(e) }]);
          setRoute("sessions");
        });
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [bridge]);

  useEffect(() => {
    let live = true;
    bridge
      .call<InitReply>("init")
      .then((reply) => {
        if (live) setRoute(reply.route);
      })
      .catch((e: unknown) => {
        if (live) {
          setChecks([{ name: "bridge", ok: false, detail: String(e) }]);
          setRoute("sessions");
        }
      });
    return () => {
      live = false;
    };
  }, [bridge]);

  useEffect(
    () =>
      bridge.on("navigate", (payload) => {
        const { page } = payload as NavigatePayload;
        setRoute(page);
      }),
    [bridge],
  );

  if (route === undefined) {
    return (
      <div className="flex min-h-screen items-center justify-center p-8">
        <p className="text-muted-foreground text-sm">Loading…</p>
      </div>
    );
  }

  if (route === "settings") {
    return (
      <div className="h-screen overflow-y-auto">
        <button className="m-4 rounded border px-3 py-2" onClick={() => setRoute("sessions")}>
          Back to sessions
        </button>
        <Settings />
      </div>
    );
  }

  return (
    <Sessions
      onSettings={() => setRoute("settings")}
      health={checks}
      onRetryHealth={refreshHealth}
    />
  );
}
