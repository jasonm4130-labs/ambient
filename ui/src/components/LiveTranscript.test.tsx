import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { LiveTranscript } from "./LiveTranscript";
import type { SessionSummary } from "./SessionList";
import { Sessions } from "@/pages/Sessions";

const line = (text: string) => ({
  track: "room" as const,
  start_ms: 0,
  end_ms: 1000,
  speaker: null,
  text,
});

function sinceCalls(fake: FakeBridge) {
  return fake.calls.filter(
    (c) => c.method === "transcript" && c.params !== undefined && "since" in c.params,
  );
}

function setupUser() {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  return userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
}

describe("LiveTranscript", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("keeps live lines through Stop and appends only the final tail", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    const replies = [
      { session: "s1", state: "live", next: 1, lines: [line("during recording")] },
      { session: "s1", state: "pending", next: 1, lines: [] },
      { session: "s1", state: "transcribing", next: 2, lines: [line("final tail")] },
      { session: "s1", state: "done", next: 2, lines: [] },
    ];
    fake.answer("transcript", () => replies.shift());
    const onDone = vi.fn();
    render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="s1" onDone={onDone} />
      </BridgeProvider>,
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByText("during recording")).toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(6000);
    });
    expect(screen.getAllByText("during recording")).toHaveLength(1);
    expect(screen.getAllByText("final tail")).toHaveLength(1);
    expect(sinceCalls(fake).map((c) => (c.params as { since: number }).since)).toEqual([
      0, 1, 1, 2,
    ]);
    expect(onDone).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(sinceCalls(fake)).toHaveLength(4);
  });

  it("appends lines across polls, then calls onDone once, with since 0, 1, 2", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    let call = 0;
    fake.answer("transcript", () => {
      call += 1;
      if (call === 1)
        return { session: "s1", state: "transcribing", next: 1, lines: [line("one")] };
      if (call === 2)
        return { session: "s1", state: "transcribing", next: 2, lines: [line("two")] };
      return { session: "s1", state: "done", next: 2, lines: [] };
    });
    const onDone = vi.fn();

    render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="s1" onDone={onDone} />
      </BridgeProvider>,
    );

    // Flushes the in-flight poll's microtasks (which arm the next timer)
    // without advancing the fake clock.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sinceCalls(fake)).toHaveLength(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(sinceCalls(fake)).toHaveLength(2);
    expect(screen.getAllByText(/^(one|two)$/u)).toHaveLength(2);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(sinceCalls(fake)).toHaveLength(3);
    expect(onDone).toHaveBeenCalledTimes(1);

    expect(sinceCalls(fake).map((c) => (c.params as { since: number }).since)).toEqual([0, 1, 2]);
  });

  it("renders Listening… for a live session with no lines yet", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    fake.answer("transcript", { session: "s1", state: "live", next: 0, lines: [] });

    render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="s1" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByTestId("live-transcript-empty")).toHaveTextContent("Listening…");
  });

  it("renders Transcribing… for a transcribing session with no lines yet", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    fake.answer("transcript", { session: "s1", state: "transcribing", next: 0, lines: [] });

    render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="s1" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(screen.getByTestId("live-transcript-empty")).toHaveTextContent("Transcribing…");
  });

  it("resets the cursor when the session prop changes", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    // Indexed by resolution order, not call order: A's `since:0`, then B's
    // `since:0`; A's held `since:5` resolves last.
    let resolved = 0;
    fake.answer("transcript", () => {
      resolved += 1;
      if (resolved === 1)
        return { session: "A", state: "transcribing", next: 5, lines: [line("a1")] };
      if (resolved === 2)
        return { session: "B", state: "transcribing", next: 1, lines: [line("b1")] };
      return { session: "A", state: "done", next: 5, lines: [line("poison")] };
    });
    const { rerender } = render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="A" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sinceCalls(fake)).toHaveLength(1);

    // `hold` gates the *next* call, so this catches the 2 s poll, not the mount call.
    const releaseA2 = fake.hold("transcript");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(sinceCalls(fake)).toHaveLength(2);

    rerender(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="B" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sinceCalls(fake)).toHaveLength(3);
    expect(sinceCalls(fake).at(-1)?.params).toEqual({ session: "B", since: 0 });

    releaseA2();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sinceCalls(fake)).toHaveLength(3);

    expect(screen.queryByText("poison")).not.toBeInTheDocument();
    expect(screen.getByText("b1")).toBeInTheDocument();
  });
});

describe("Sessions live swap", () => {
  const base: SessionSummary = {
    id: "s1",
    dir: "/x/s1",
    name: "Standup",
    started_at: "2026-09-06",
    duration_s: 30,
    transcribed: false,
    live: false,
    transcribing: true,
    error: null,
    tags: [],
    pinned: false,
  };

  it("shows live-transcript while transcribing, and swaps to Transcript once done", async () => {
    const user = setupUser();
    const fake = new FakeBridge();

    let sessionsCall = 0;
    fake.answer("sessions", () => {
      sessionsCall += 1;
      if (sessionsCall === 1) return [base];
      return [{ ...base, transcribed: true, transcribing: false }];
    });
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });

    // Calls 1-3 are `LiveTranscript`'s polls; call 4+ is `Transcript`'s mount
    // call after the `onDone` swap.
    let transcriptCall = 0;
    fake.answer("transcript", (() => {
      transcriptCall += 1;
      if (transcriptCall === 1)
        return { session: "s1", state: "transcribing", next: 1, lines: [line("one")] };
      if (transcriptCall === 2)
        return { session: "s1", state: "transcribing", next: 2, lines: [line("two")] };
      if (transcriptCall === 3) return { session: "s1", state: "done", next: 2, lines: [] };
      return { session: "s1", state: "done", next: 2, lines: [line("one"), line("two")] };
    }) as unknown as () => unknown);

    render(
      <BridgeProvider bridge={fake}>
        <Sessions onSettings={vi.fn()} />
      </BridgeProvider>,
    );

    await user.click(await screen.findByText("Standup"));
    expect(await screen.findByTestId("live-transcript")).toBeInTheDocument();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(sinceCalls(fake)).toHaveLength(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(sinceCalls(fake)).toHaveLength(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(sinceCalls(fake)).toHaveLength(3);

    await vi.waitFor(() => expect(screen.getByTestId("transcript")).toBeInTheDocument());
    expect(screen.queryByTestId("live-transcript")).not.toBeInTheDocument();
  });
});
