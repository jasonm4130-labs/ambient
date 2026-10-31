import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import type { TalkPayload } from "./text";
import { on, turn } from "./fixtures";
import { TalkCard } from "./TalkCard";

function card(fake: FakeBridge, payload: TalkPayload | undefined, onSettings = vi.fn()) {
  return render(
    <BridgeProvider bridge={fake}>
      <TalkCard payload={payload} onOpen={vi.fn()} onSettings={onSettings} />
    </BridgeProvider>,
  );
}

const methods = (fake: FakeBridge) => fake.calls.map((c) => c.method);

describe("TalkCard", () => {
  it("press and release are sent once each", async () => {
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
    fireEvent.keyDown(hold, { key: "Enter" });
    fireEvent.keyUp(hold, { key: "Enter" });
    await waitFor(() =>
      expect(methods(fake)).toEqual(["talk.press", "talk.release", "talk.press", "talk.release"]),
    );
  });

  it("losing focus mid-hold releases, so a hold never sticks", async () => {
    const fake = new FakeBridge();
    card(fake, on);
    const hold = screen.getByRole("button", { name: /Hold to talk/u });
    fireEvent.keyDown(hold, { key: " " });
    fireEvent.blur(hold);
    await waitFor(() => expect(methods(fake)).toEqual(["talk.press", "talk.release"]));
  });

  it("the button is pressed while listening, and the status line names the state", () => {
    card(new FakeBridge(), { ...on, listening: true });
    const hold = screen.getByRole("button", { name: /Let go to send/u });
    expect(hold).toHaveAttribute("aria-pressed", "true");
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("Listening… let go to send.");
    expect(hold).toHaveAttribute("aria-describedby", status.id);
    expect(screen.getByTestId("talk-card")).toHaveAttribute("data-state", "listening");
  });

  it("shows thinking, speaking and error as states", () => {
    const { rerender } = card(new FakeBridge(), { ...on, turns: [turn("waiting")] });
    expect(screen.getByTestId("talk-card")).toHaveAttribute("data-state", "thinking");
    expect(screen.getByRole("status")).toHaveTextContent("Thinking…");
    const fake = new FakeBridge();
    const speaking = {
      ...on,
      turns: [turn("speaking", { reply: "Green.", reply_text: "Green." })],
    };
    rerender(
      <BridgeProvider bridge={fake}>
        <TalkCard payload={speaking} onOpen={vi.fn()} onSettings={vi.fn()} />
      </BridgeProvider>,
    );
    expect(screen.getByTestId("talk-card")).toHaveAttribute("data-state", "speaking");
    expect(screen.getByRole("button", { name: /Hold to talk/u })).toBeEnabled();
    const failed = { ...on, turns: [turn("failed", { note: "the voice helper is missing" })] };
    rerender(
      <BridgeProvider bridge={fake}>
        <TalkCard payload={failed} onOpen={vi.fn()} onSettings={vi.fn()} />
      </BridgeProvider>,
    );
    expect(screen.getByTestId("talk-card")).toHaveAttribute("data-state", "error");
    expect(screen.getByRole("status")).toHaveTextContent("the voice helper is missing");
    expect(screen.getByRole("status")).toHaveClass("text-destructive");
  });

  it("the paused label truncates inside the card", () => {
    card(new FakeBridge(), { ...on, paused: "armed" });
    const hold = screen.getByRole("button", { name: /Paused: call waiting/u });
    expect(hold).toBeDisabled();
    expect(hold).toHaveClass("min-w-0");
    expect(screen.getByTestId("talk-hold-label")).toHaveClass("truncate");
    expect(screen.getByTestId("talk-hold-label").textContent!.length).toBeLessThanOrEqual(20);
  });

  it("the card says no agent is listening and disables the hold button when listener is null", async () => {
    const onSettings = vi.fn();
    card(new FakeBridge(), { ...on, listener: null }, onSettings);
    expect(screen.getByRole("button", { name: /Hold to talk/u })).toBeDisabled();
    expect(screen.getByTestId("talk-no-agent")).toHaveTextContent("No agent is listening");
    await userEvent.click(screen.getByRole("button", { name: "How" }));
    expect(onSettings).toHaveBeenCalled();
  });

  it("off, offers to turn talking mode on", async () => {
    const onSettings = vi.fn();
    card(new FakeBridge(), { ...on, enabled: false }, onSettings);
    expect(screen.queryByTestId("talk-card")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Turn on" }));
    expect(onSettings).toHaveBeenCalled();
  });

  it("a press Ambient did not take is shown as an alert", async () => {
    const fake = new FakeBridge();
    fake.answer("talk.press", { sent: false });
    card(fake, on);
    fireEvent.pointerDown(screen.getByRole("button", { name: /Hold to talk/u }), { pointerId: 1 });
    expect(await screen.findByRole("alert")).toHaveTextContent("Ambient did not answer");
  });
});
