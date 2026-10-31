import { describe, expect, it } from "vitest";
import { on, turn } from "./fixtures";
import { listenerLine, talkLine, talkState, theirSide } from "./text";

describe("talkState", () => {
  it("names each state the card draws", () => {
    expect(talkState({ ...on, enabled: false })).toBe("off");
    expect(talkState(on)).toBe("idle");
    expect(talkState({ ...on, listening: true })).toBe("listening");
    expect(talkState({ ...on, paused: "recording" })).toBe("paused");
    for (const s of ["hearing", "sending", "waiting"] as const)
      expect(talkState({ ...on, turns: [turn(s)] })).toBe("thinking");
    expect(talkState({ ...on, turns: [turn("speaking", { reply: "Hi." })] })).toBe("speaking");
    expect(talkState({ ...on, listener: null })).toBe("no_agent");
    expect(talkState({ ...on, turns: [turn("failed", { note: "boom" })] })).toBe("error");
    expect(talkState({ ...on, turns: [turn("no_reply")] })).toBe("error");
    expect(talkState({ ...on, turns: [turn("done", { reply: "Hi." })] })).toBe("idle");
  });

  it("listening wins over paused and a turn in flight, which win over nobody listening", () => {
    expect(talkState({ ...on, listening: true, paused: "armed" })).toBe("listening");
    expect(talkState({ ...on, listener: null, turns: [turn("waiting")] })).toBe("thinking");
  });
});

describe("talkLine", () => {
  it("shows the latest reply's first line, a notice first, and the key's absence", () => {
    const done = turn("done", {
      reply: "**PR 12** merged.\nMore.",
      reply_text: "PR 12 merged.\nMore.",
    });
    expect(talkLine({ ...on, turns: [done] })).toBe("Reply: PR 12 merged.");
    expect(talkLine({ ...on, turns: [done], notice: "Talking is paused" })).toBe(
      "Talking is paused",
    );
    expect(talkLine({ ...on, listening: true })).toBe("Listening… let go to send.");
    expect(talkLine({ ...on, hotkey_error: "another app already uses ⌥Space" })).toContain(
      "use this button",
    );
    expect(talkLine(on)).toBeNull();
  });

  it("says thinking, speaking and what went wrong", () => {
    expect(talkLine({ ...on, turns: [turn("hearing")] })).toBe("Hearing you…");
    expect(talkLine({ ...on, turns: [turn("waiting")] })).toBe("Thinking…");
    expect(talkLine({ ...on, turns: [turn("speaking", { reply: "Hi." })] })).toContain(
      "Hold to interrupt",
    );
    const expired = turn("no_reply", { note: "No reply within 5 minutes; the question expired." });
    expect(talkLine({ ...on, turns: [expired] })).toBe(
      "No reply within 5 minutes; the question expired.",
    );
  });
});

describe("theirSide", () => {
  it("prefers the plain reply text and falls back to the raw reply", () => {
    expect(theirSide(turn("done", { reply: "**Hi.**", reply_text: "Hi." }))).toBe("Hi.");
    const { reply_text: _, ...older } = turn("done", { reply: "Hi." });
    expect(theirSide(older)).toBe("Hi.");
    expect(theirSide(turn("waiting"))).toBe("Thinking…");
    expect(theirSide(turn("nothing"))).toBeNull();
  });
});

describe("listenerLine", () => {
  it("names the client and how it hears", () => {
    expect(listenerLine(null)).toBe("No agent is listening.");
    expect(listenerLine(on.listener)).toMatch(/^Listening: Claude Code since .+\.$/u);
    expect(listenerLine({ since: "bad", push: false, client: "codex" })).toBe(
      "Listening: codex, waiting in a loop.",
    );
  });
});
