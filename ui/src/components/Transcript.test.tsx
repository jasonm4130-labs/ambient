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
    next: 3,
    lines: [
      { track: "room", start_ms: 0, end_ms: 1000, speaker: "Marcus", text: `${prefix} one` },
      { track: "room", start_ms: 1000, end_ms: 2000, speaker: "Marcus", text: `${prefix} two` },
      { track: "call", start_ms: 2000, end_ms: 3000, speaker: null, text: `${prefix} three` },
    ],
  };
}

function renderTranscript(fake: FakeBridge, session: string) {
  return render(
    <BridgeProvider bridge={fake}>
      <Transcript session={session} />
    </BridgeProvider>,
  );
}

describe("Transcript", () => {
  it("renders speaker groups, grouping consecutive lines by the same speaker", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("hello"));
    renderTranscript(fake, "x");

    await screen.findByText("hello one");
    await screen.findByText("hello two");
    await screen.findByText("hello three");
    // Marcus's two consecutive lines are one group; the speakerless
    // (track-fallback) line is its own group, headed "call".
    expect(screen.getAllByText("Marcus")).toHaveLength(1);
    expect(screen.getByText("call")).toBeInTheDocument();
  });

  it("toggles verbatim by calling transcript with verbatim: true", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("hello"));
    renderTranscript(fake, "x");
    await screen.findByText("hello one");

    await userEvent.click(screen.getByRole("button", { name: "Verbatim" }));

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "transcript").at(-1);
      expect(call?.params).toEqual({ session: "x", verbatim: true });
    });
  });

  it("Copy Markdown calls export then clipboard.write with the returned text", async () => {
    const fake = new FakeBridge();
    fake.answer("transcript", transcriptReply("hello"));
    fake.answer("export", { session: "x", format: "markdown", text: "# hello" });
    fake.answer("clipboard.write", { done: true });
    renderTranscript(fake, "x");
    await screen.findByText("hello one");

    await userEvent.click(screen.getByRole("button", { name: "Copy Markdown" }));

    await waitFor(() => {
      const exported = fake.calls.find((c) => c.method === "export");
      expect(exported?.params).toEqual({ session: "x", format: "markdown" });
      const clipboard = fake.calls.find((c) => c.method === "clipboard.write");
      expect(clipboard?.params).toEqual({ text: "# hello" });
    });
  });

  it("selecting A then B with A's transcript reply released after B's leaves B's lines on screen", async () => {
    const fake = new FakeBridge();
    // The FIFO gate queue matches call order: the first `transcript` call
    // (A, issued on mount) gets `releaseA`, the second (B, issued on
    // `rerender`) gets `releaseB` — independent of which is released first.
    const releaseA = fake.hold("transcript");
    const releaseB = fake.hold("transcript");

    const { rerender } = renderTranscript(fake, "a");
    rerender(
      <BridgeProvider bridge={fake}>
        <Transcript session="b" />
      </BridgeProvider>,
    );

    fake.answer("transcript", transcriptReply("b-reply"));
    releaseB();
    await screen.findByText("b-reply one");

    fake.answer("transcript", transcriptReply("a-reply"));
    releaseA();

    // `useLatest` drops A's reply: its generation is no longer current once
    // B's request was issued, so it must never overwrite B's lines.
    await new Promise((resolve) => {
      setTimeout(resolve, 0);
    });
    expect(screen.queryByText("a-reply one")).not.toBeInTheDocument();
    expect(screen.getByText("b-reply one")).toBeInTheDocument();
  });
});
