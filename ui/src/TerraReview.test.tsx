import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { LiveTranscript } from "./components/LiveTranscript";
import { SessionHeader } from "./components/SessionHeader";
import { BridgeProvider } from "./lib/bridge-context";
import { FakeBridge } from "./test/fake-bridge";

afterEach(() => vi.useRealTimers());

it("retries a failed live poll with the same cursor and clears the recovered error", async () => {
  vi.useFakeTimers();
  const fake = new FakeBridge();
  let calls = 0;
  fake.answer("transcript", () => {
    calls += 1;
    if (calls === 2) throw new Error("incomplete JSONL record");
    return {
      session: "one",
      state: calls === 3 ? "done" : "transcribing",
      next: calls === 3 ? 2 : 1,
      lines: [
        {
          track: "room",
          start_ms: 0,
          end_ms: 1000,
          speaker: null,
          text: calls === 1 ? "First" : "Second",
        },
      ],
    };
  });
  const onDone = vi.fn();
  render(
    <BridgeProvider bridge={fake}>
      <LiveTranscript session="one" onDone={onDone} />
    </BridgeProvider>,
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(screen.getByText("First")).toBeInTheDocument();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(2000);
  });
  expect(screen.getByRole("alert")).toHaveTextContent("incomplete JSONL record");
  await act(async () => {
    await vi.advanceTimersByTimeAsync(2000);
  });
  expect(screen.getByText("Second")).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(
    fake.calls.filter((call) => call.method === "transcript").map((call) => call.params),
  ).toEqual([
    { session: "one", since: 0 },
    { session: "one", since: 1 },
    { session: "one", since: 1 },
  ]);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(6000);
  });
  expect(onDone).toHaveBeenCalledTimes(1);
  expect(calls).toBe(3);
});

it("explains a refused deletion and lets the user retry after the lock clears", async () => {
  const fake = new FakeBridge();
  fake.answer("session.delete", () => {
    throw new Error("session is transcribing");
  });
  const onDeleted = vi.fn();
  render(
    <BridgeProvider bridge={fake}>
      <SessionHeader
        summary={{
          id: "one",
          dir: "/one",
          name: "One",
          started_at: null,
          duration_s: 10,
          transcribed: false,
          transcribing: true,
          live: false,
          error: null,
          tags: [],
          pinned: false,
        }}
        onChanged={() => {}}
        onDeleted={onDeleted}
      />
    </BridgeProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "More actions" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete session…" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("session is transcribing");
  expect(onDeleted).not.toHaveBeenCalled();
  fake.answer("session.delete", {});
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  });
  expect(onDeleted).toHaveBeenCalledTimes(1);
});
