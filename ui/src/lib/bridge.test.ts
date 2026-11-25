import { act, render } from "@testing-library/react";
import { createElement, useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useLatest } from "./latest";

interface WebKitWindow {
  webkit?: {
    messageHandlers?: {
      ambient?: { postMessage: (body: string) => void };
    };
  };
}

// The id counter is module-scoped: re-import per test so `id: 1` holds.
function freshBridge(): Promise<typeof import("./bridge")> {
  vi.resetModules();
  return import("./bridge");
}

function posted(postMessage: ReturnType<typeof vi.fn>): { id: number } {
  const call = postMessage.mock.calls[0];
  if (call === undefined) throw new Error("postMessage was never called");
  return JSON.parse(call[0] as string) as { id: number };
}

describe("bridge", () => {
  afterEach(() => {
    delete (window as unknown as WebKitWindow).webkit;
  });

  it("posts one message with id 1 and resolves on reply", async () => {
    const postMessage = vi.fn();
    (window as unknown as WebKitWindow).webkit = { messageHandlers: { ambient: { postMessage } } };
    const { call } = await freshBridge();

    const result = call("ping");
    expect(postMessage).toHaveBeenCalledTimes(1);
    expect(posted(postMessage).id).toBe(1);

    window.ambient.reply(1, { result: {} });
    await expect(result).resolves.toEqual({});
  });

  it("rejects with the error message on an {error} reply", async () => {
    const postMessage = vi.fn();
    (window as unknown as WebKitWindow).webkit = { messageHandlers: { ambient: { postMessage } } };
    const { call } = await freshBridge();

    const result = call("boom");
    window.ambient.reply(posted(postMessage).id, {
      error: { kind: "bad", message: "went wrong" },
    });
    await expect(result).rejects.toThrow("went wrong");
  });

  it("fires an event handler from window.ambient.event", async () => {
    const { on } = await freshBridge();
    const handler = vi.fn();
    on("config", handler);
    window.ambient.event("config", { diarize: true });
    expect(handler).toHaveBeenCalledWith({ diarize: true });
  });
});

function Harness({ call }: { call: (...args: unknown[]) => Promise<unknown> }) {
  const wrapped = useLatest(call);
  useEffect(() => {
    capture = wrapped;
  }, [wrapped]);
  return null;
}

let capture: ((...args: unknown[]) => Promise<unknown>) | undefined;

function applyIfLive(seen: { current: unknown }, from: string, value: unknown): void {
  if (value !== undefined) seen.current = { from, value };
}

function noResolver(): void {}

describe("useLatest", () => {
  beforeEach(() => {
    capture = undefined;
  });

  it("drops a reply from an older generation", async () => {
    let resolveA: (value: string) => void = noResolver;
    let resolveB: (value: string) => void = noResolver;
    const call = vi.fn((which: unknown) => {
      if (which === "A")
        return new Promise<string>((resolve) => {
          resolveA = resolve;
        });
      return new Promise<string>((resolve) => {
        resolveB = resolve;
      });
    });

    render(createElement(Harness, { call }));
    if (capture === undefined) throw new Error("hook did not mount");
    const wrapped = capture;

    const a = wrapped("A");
    const b = wrapped("B");
    const seen = { current: undefined as unknown };
    a.then((value) => applyIfLive(seen, "A", value));
    b.then((value) => applyIfLive(seen, "B", value));

    await act(async () => {
      resolveB("b-result");
      await b;
    });
    expect(seen.current).toEqual({ from: "B", value: "b-result" });

    await act(async () => {
      resolveA("a-result");
      await a;
    });
    expect(seen.current).toEqual({ from: "B", value: "b-result" });
  });
});
