import { act, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";
import { BridgeProvider } from "./lib/bridge-context";
import { FakeBridge } from "./test/fake-bridge";

describe("App", () => {
  it("calls init on mount and renders the route it returns", async () => {
    const fake = new FakeBridge();
    fake.answer("init", { route: "sessions" });

    render(
      <BridgeProvider bridge={fake}>
        <App />
      </BridgeProvider>,
    );

    await screen.findByRole("heading", { name: "Sessions" });
    // StrictMode double-invokes the mount effect under vitest's React
    // development build, so `init` fires at least once, not exactly once.
    expect(fake.calls.filter((c) => c.method === "init").length).toBeGreaterThanOrEqual(1);
  });

  it("switches to Settings on a navigate event after mount", async () => {
    const fake = new FakeBridge();
    fake.answer("init", { route: "sessions" });

    render(
      <BridgeProvider bridge={fake}>
        <App />
      </BridgeProvider>,
    );

    await screen.findByRole("heading", { name: "Sessions" });
    act(() => {
      fake.emit("navigate", { page: "settings" });
    });
    await screen.findByRole("heading", { name: "Settings" });
  });
});
