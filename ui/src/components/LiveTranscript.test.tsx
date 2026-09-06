import { render, screen } from "@testing-library/react";
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
  return fake.calls.filter((c) => c.method === "transcript" && c.params !== undefined && "since" in c.params);
}

function setupUser() {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  return userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
}

describe("LiveTranscript", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("appends lines across polls, then calls onDone once, with since 0, 1, 2", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    let call = 0;
    fake.answer("transcript", () => {
      call += 1;
      if (call === 1) return { session: "s1", state: "transcribing", next: 1, lines: [line("one")] };
      if (call === 2) return { session: "s1", state: "transcribing", next: 2, lines: [line("two")] };
      return { session: "s1", state: "done", next: 2, lines: [] };
    });
    const onDone = vi.fn();

    render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="s1" onDone={onDone} />
      </BridgeProvider>,
    );

    // `advanceTimersByTimeAsync(0)` flushes the microtasks of the poll
    // already in flight from mount (its call is pushed synchronously, but
    // the `.then` that arms the next `setTimeout` only runs on a microtask
    // tick) without advancing the fake clock.
    await vi.advanceTimersByTimeAsync(0);
    expect(sinceCalls(fake)).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(2000);
    expect(sinceCalls(fake)).toHaveLength(2);
    expect(screen.getAllByText(/^(one|two)$/u)).toHaveLength(2);

    await vi.advanceTimersByTimeAsync(2000);
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

    await vi.advanceTimersByTimeAsync(0);
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

    await vi.advanceTimersByTimeAsync(0);
    expect(screen.getByTestId("live-transcript-empty")).toHaveTextContent("Transcribing…");
  });

  it("resets the cursor when the session prop changes", async () => {
    vi.useFakeTimers();
    const fake = new FakeBridge();
    // Resolve order, not call order: A's `since:0` call resolves first, then
    // B's `since:0` call resolves (A's `since:5` call is held and released
    // last), so the thunk is indexed by resolution, not by which call fired.
    let resolved = 0;
    fake.answer("transcript", () => {
      resolved += 1;
      if (resolved === 1) return { session: "A", state: "transcribing", next: 5, lines: [line("a1")] };
      if (resolved === 2) return { session: "B", state: "transcribing", next: 1, lines: [line("b1")] };
      return { session: "A", state: "done", next: 5, lines: [line("poison")] };
    });
    const { rerender } = render(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="A" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await vi.advanceTimersByTimeAsync(0);
    expect(sinceCalls(fake)).toHaveLength(1);

    // Hold A's *second* call (`since: 5`) rather than its first: `hold`
    // gates the next call to the method, registered here so it catches the
    // poll the 2 s timer is about to fire, not the mount call already made.
    const releaseA2 = fake.hold("transcript");
    await vi.advanceTimersByTimeAsync(2000);
    expect(sinceCalls(fake)).toHaveLength(2);

    rerender(
      <BridgeProvider bridge={fake}>
        <LiveTranscript session="B" onDone={vi.fn()} />
      </BridgeProvider>,
    );

    await vi.advanceTimersByTimeAsync(0);
    expect(sinceCalls(fake)).toHaveLength(3);
    expect(sinceCalls(fake).at(-1)?.params).toEqual({ session: "B", since: 0 });

    // Releasing A's held call resolves it into a component that already
    // unmounted (the `session` prop moved to B), so the effect's `cancelled`
    // guard drops the reply — no new call, and no "poison" text.
    releaseA2();
    await vi.advanceTimersByTimeAsync(0);
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

    // Calls 1-3 are `LiveTranscript`'s polls (carry `since`); call 4+ is
    // `Transcript`'s own mount call ({session, verbatim}, no `since`), fired
    // once the swap happens after `onDone` → `refresh`.
    let transcriptCall = 0;
    fake.answer("transcript", (() => {
      transcriptCall += 1;
      if (transcriptCall === 1) return { session: "s1", state: "transcribing", next: 1, lines: [line("one")] };
      if (transcriptCall === 2) return { session: "s1", state: "transcribing", next: 2, lines: [line("two")] };
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

    await vi.advanceTimersByTimeAsync(0);
    expect(sinceCalls(fake)).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(2000);
    expect(sinceCalls(fake)).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(sinceCalls(fake)).toHaveLength(3);

    await vi.waitFor(() => expect(screen.getByTestId("transcript")).toBeInTheDocument());
    expect(screen.queryByTestId("live-transcript")).not.toBeInTheDocument();
  });
});
