import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { TalkCard, TalkPanel, talkLine, type TalkPayload } from "./Talk";

const on: TalkPayload = {
  enabled: true,
  speak: true,
  configured: true,
  chord: "⌥Space",
  hotkey_error: null,
  listening: false,
  paused: null,
  notice: null,
  turns: [],
};

function card(fake: FakeBridge, payload: TalkPayload | undefined, onSettings = vi.fn()) {
  return render(
    <BridgeProvider bridge={fake}>
      <TalkCard payload={payload} onOpen={vi.fn()} onSettings={onSettings} />
    </BridgeProvider>,
  );
}

const methods = (fake: FakeBridge) => fake.calls.map((c) => c.method);

describe("TalkCard", () => {
  it("holding the button presses, and letting go releases, once each", async () => {
    const fake = new FakeBridge();
    fake.answer("talk.press", { sent: true });
    fake.answer("talk.release", { sent: true });
    card(fake, on);

    const hold = screen.getByRole("button", { name: /Hold to talk/u });
    expect(screen.getByText("⌥Space")).toBeInTheDocument();
    fireEvent.pointerDown(hold, { pointerId: 1 });
    fireEvent.pointerDown(hold, { pointerId: 1 });
    fireEvent.pointerUp(hold, { pointerId: 1 });
    fireEvent.pointerUp(hold, { pointerId: 1 });
    await waitFor(() => expect(methods(fake)).toEqual(["talk.press", "talk.release"]));
  });

  it("works from the keyboard and ignores key repeat", async () => {
    const fake = new FakeBridge();
    card(fake, on);
    const hold = screen.getByRole("button", { name: /Hold to talk/u });
    fireEvent.keyDown(hold, { key: " " });
    fireEvent.keyDown(hold, { key: " ", repeat: true });
    fireEvent.keyUp(hold, { key: " " });
    await waitFor(() => expect(methods(fake)).toEqual(["talk.press", "talk.release"]));
  });

  it("is paused while recording or armed", () => {
    card(new FakeBridge(), { ...on, paused: "recording" });
    expect(screen.getByRole("button", { name: /Paused while recording/u })).toBeDisabled();
  });

  it("says when firstmate's home is not chosen and links to Settings", async () => {
    const onSettings = vi.fn();
    card(new FakeBridge(), { ...on, configured: false }, onSettings);
    expect(screen.getByRole("button", { name: /Hold to talk/u })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Choose firstmate's home" }));
    expect(onSettings).toHaveBeenCalled();
  });

  it("off, offers to turn talking mode on", async () => {
    const onSettings = vi.fn();
    card(new FakeBridge(), { ...on, enabled: false }, onSettings);
    expect(screen.queryByTestId("talk-card")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Turn on" }));
    expect(onSettings).toHaveBeenCalled();
  });

  it("shows the latest reply, a notice first, and the key's absence", () => {
    const turn = {
      id: 1,
      you: "what's merged?",
      reply: "PR 12 merged.\nMore.",
      state: "done" as const,
      note: null,
    };
    expect(talkLine({ ...on, turns: [turn] })).toBe("firstmate: PR 12 merged.");
    expect(talkLine({ ...on, turns: [turn], notice: "Talking is paused" })).toBe(
      "Talking is paused",
    );
    expect(talkLine({ ...on, listening: true })).toBe("Listening… let go to send.");
    expect(talkLine({ ...on, hotkey_error: "another app already uses ⌥Space" })).toContain(
      "use this button",
    );
    expect(talkLine(on)).toBeNull();
  });
});

describe("TalkPanel", () => {
  it("shows both sides of every turn as text", () => {
    render(
      <BridgeProvider bridge={new FakeBridge()}>
        <TalkPanel
          payload={{
            ...on,
            turns: [
              { id: 1, you: "status?", reply: "All green.", state: "done", note: null },
              {
                id: 2,
                you: "and the deploy?",
                reply: null,
                state: "waiting",
                note: "Queued: firstmate is busy",
              },
              { id: 3, you: null, reply: null, state: "nothing", note: "Didn't catch that." },
            ],
          }}
        />
      </BridgeProvider>,
    );
    const turns = screen.getAllByTestId("talk-turn");
    expect(turns).toHaveLength(3);
    expect(turns[0]).toHaveTextContent("You: status?");
    expect(turns[0]).toHaveTextContent("firstmate: All green.");
    expect(turns[1]).toHaveTextContent("Thinking…");
    expect(turns[1]).toHaveTextContent("Queued: firstmate is busy");
    expect(turns[2]).toHaveTextContent("Didn't catch that.");
  });

  it("the speak switch sets talk.speak", async () => {
    const fake = new FakeBridge();
    render(
      <BridgeProvider bridge={fake}>
        <TalkPanel payload={on} />
      </BridgeProvider>,
    );
    expect(screen.getByTestId("talk-empty")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("switch", { name: "Speak firstmate's replies" }));
    await waitFor(() =>
      expect(fake.calls).toContainEqual({
        method: "config.set",
        params: { key: "talk.speak", value: "false" },
      }),
    );
  });
});
