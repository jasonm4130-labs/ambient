import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { Transcript } from "./Transcript";

function transcriptReply(prefix: string) {
  return {
    session: "x",
    state: "done",
    next: 1,
    lines: [{ track: "room", start_ms: 0, end_ms: 1000, speaker: null, text: `${prefix} one` }],
  };
}

function renderTranscript(fake: FakeBridge, session: string) {
  return render(
    <BridgeProvider bridge={fake}>
      <Transcript session={session} />
    </BridgeProvider>,
  );
}

describe("Transcript — Separate voices", () => {
  it("calls diarize.start with the session, and a diarize done event reloads the transcript", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("before"));
    fake.answer("diarize.start", { started: true });
    renderTranscript(fake, "x");
    await screen.findByText("before one");

    await userEvent.click(screen.getByRole("button", { name: "Separate voices" }));

    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "diarize.start");
      expect(call?.params).toEqual({ session: "x" });
    });

    fake.answer("transcript", transcriptReply("after"));
    fake.emit("diarize", { session: "x", state: "done", error: null });

    await screen.findByText("after one");
  });

  it("a diarize failed event for the shown session renders the error inline", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("before"));
    fake.answer("diarize.start", { started: true });
    renderTranscript(fake, "x");
    await screen.findByText("before one");

    await userEvent.click(screen.getByRole("button", { name: "Separate voices" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "diarize.start")).toBe(true);
    });

    fake.emit("diarize", { session: "x", state: "failed", error: "no transcript to attribute" });

    await screen.findByText("no transcript to attribute");
  });

  it("ignores a diarize event for a different session", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("before"));
    fake.answer("diarize.start", { started: true });
    renderTranscript(fake, "x");
    await screen.findByText("before one");

    await userEvent.click(screen.getByRole("button", { name: "Separate voices" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "diarize.start")).toBe(true);
    });

    fake.emit("diarize", { session: "other-session", state: "failed", error: "boom" });

    expect(screen.queryByText("boom")).not.toBeInTheDocument();
    // Still separating: the event named a session other than the one on
    // screen, so it must not have cleared this button's in-flight state.
    expect(screen.getByTestId("separate-voices")).toBeDisabled();
  });
});
