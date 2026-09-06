import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import type { SessionSummary } from "./SessionList";
import { SearchPalette } from "./SearchPalette";

const sessions: SessionSummary[] = [
  {
    id: "2026-09-05-1200",
    dir: "/x/a",
    name: "Budget review",
    started_at: "2026-09-05",
    duration_s: 60,
    transcribed: true,
    live: false,
    transcribing: false,
    error: null,
    tags: [],
    pinned: false,
  },
  {
    id: "2026-09-06-0900",
    dir: "/x/b",
    name: null,
    started_at: "2026-09-06",
    duration_s: 60,
    transcribed: true,
    live: false,
    transcribing: false,
    error: null,
    tags: [],
    pinned: false,
  },
];

function renderPalette(fake: FakeBridge, onOpen = vi.fn(), onClose = vi.fn()) {
  render(
    <BridgeProvider bridge={fake}>
      <SearchPalette sessions={sessions} onOpen={onOpen} onClose={onClose} />
    </BridgeProvider>,
  );
  return { onOpen, onClose };
}

function setupUser() {
  // `shouldAdvanceTime` is required alongside fake timers here: without it
  // `userEvent`'s own internal waits (used even by a plain click) never
  // resolve, because they schedule a timer this test's own
  // `advanceTimersByTime` calls cannot reach until after the `await` that is
  // waiting on them — a deadlock, not a slow test.
  vi.useFakeTimers({ shouldAdvanceTime: true });
  return userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
}

describe("SearchPalette", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("calls search with the debounced query exactly once", async () => {
    // A synchronous `fireEvent.change` rather than `user.type`: typing six
    // keystrokes under `shouldAdvanceTime`'s real-time-coupled clock risks
    // the 150 ms debounce firing mid-keystroke on a loaded machine.
    vi.useFakeTimers();
    const fake = new FakeBridge();
    fake.answer("search", []);
    renderPalette(fake);

    fireEvent.change(screen.getByRole("searchbox", { name: "Search" }), { target: { value: "budget" } });
    expect(fake.calls.filter((c) => c.method === "search")).toHaveLength(0);

    vi.advanceTimersByTime(150);
    await vi.waitFor(() => {
      expect(fake.calls.filter((c) => c.method === "search")).toHaveLength(1);
    });
    expect(fake.calls.filter((c) => c.method === "search")[0]?.params).toEqual({
      query: "budget",
      limit: 50,
    });
  });

  it("groups hits from different sessions under their session names", async () => {
    const user = setupUser();
    const fake = new FakeBridge();
    fake.answer("search", [
      {
        session: "2026-09-05-1200",
        index: 0,
        track: "room",
        start_ms: 5000,
        speaker: null,
        text: "the budget review is at noon",
      },
      {
        session: "2026-09-06-0900",
        index: 3,
        track: "call",
        start_ms: 12000,
        speaker: "Marcus",
        text: "another budget line",
      },
    ]);
    renderPalette(fake);

    await user.type(screen.getByRole("searchbox", { name: "Search" }), "budget");
    vi.advanceTimersByTime(150);

    await screen.findByText("Budget review");
    await screen.findByText("2026-09-06-0900");
    await screen.findByText("the budget review is at noon");
    await screen.findByText("another budget line");
  });

  it("Enter on the first hit calls onOpen with the session and index", async () => {
    const user = setupUser();
    const fake = new FakeBridge();
    fake.answer("search", [
      {
        session: "2026-09-05-1200",
        index: 0,
        track: "room",
        start_ms: 5000,
        speaker: null,
        text: "the budget review is at noon",
      },
    ]);
    const { onOpen } = renderPalette(fake);

    const input = screen.getByRole("searchbox", { name: "Search" });
    await user.type(input, "budget");
    vi.advanceTimersByTime(150);
    await screen.findByText("the budget review is at noon");

    await user.type(input, "{Enter}");

    expect(onOpen).toHaveBeenCalledWith("2026-09-05-1200", 0);
  });

  it("shows Nothing matches for an empty result", async () => {
    const user = setupUser();
    const fake = new FakeBridge();
    fake.answer("search", []);
    renderPalette(fake);

    await user.type(screen.getByRole("searchbox", { name: "Search" }), "budget");
    vi.advanceTimersByTime(150);

    await screen.findByText("Nothing matches");
  });

  it("Esc calls onClose", async () => {
    const user = setupUser();
    const fake = new FakeBridge();
    fake.answer("search", []);
    const { onClose } = renderPalette(fake);

    await user.type(screen.getByRole("searchbox", { name: "Search" }), "{Escape}");

    expect(onClose).toHaveBeenCalled();
  });
});
