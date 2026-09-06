import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { Sessions } from "./Sessions";
import { SessionHeader } from "@/components/SessionHeader";
import { Transcript } from "@/components/Transcript";

const summary = {
  id: "a",
  dir: "/x/a",
  name: "Standup",
  started_at: "2026-09-01",
  duration_s: 60,
  transcribed: true,
  live: false,
  transcribing: false,
  error: null,
  tags: [],
  pinned: false,
  notes: "Keep the budget decision",
};

it("loads saved notes before editing them", async () => {
  const fake = new FakeBridge();
  render(
    <BridgeProvider bridge={fake}>
      <SessionHeader summary={summary} onChanged={() => {}} />
    </BridgeProvider>,
  );
  expect(screen.getByRole("textbox", { name: "Notes" })).toHaveValue(summary.notes);
});

it("refreshes the transcript after naming a speaker", async () => {
  const fake = new FakeBridge();
  let named = false;
  fake.answer("sessions", [summary]);
  fake.answer("config.get", { roster: ["Ana"] });
  fake.answer("speakers.unnamed", () => (named ? [] : [{ label: "speaker_0", sample: "hello" }]));
  fake.answer("speakers.name", () => {
    named = true;
    return { renamed: 1 };
  });
  fake.answer("transcript", () => ({
    session: "a",
    state: "done",
    next: 1,
    lines: [
      {
        track: "room",
        start_ms: 0,
        end_ms: 1000,
        speaker: named ? "Ana" : "speaker_0",
        text: "hello",
      },
    ],
  }));
  render(
    <BridgeProvider bridge={fake}>
      <Sessions onSettings={() => {}} />
    </BridgeProvider>,
  );
  await userEvent.click(await screen.findByRole("button", { name: /Standup/u }));
  await userEvent.type(await screen.findByRole("combobox", { name: "Name for speaker_0" }), "Ana");
  await userEvent.click(screen.getByRole("button", { name: "Name", exact: true }));
  await screen.findByText("Ana");
});

it("ignores a stale failure after switching transcripts", async () => {
  const fake = new FakeBridge();
  let rejectOld: (error: Error) => void = () => {};
  fake.answer(
    "transcript",
    () =>
      new Promise((_, reject) => {
        rejectOld = reject;
      }),
  );
  const view = render(
    <BridgeProvider bridge={fake}>
      <Transcript session="a" />
    </BridgeProvider>,
  );
  await waitFor(() => expect(fake.calls).toHaveLength(1));
  fake.answer("transcript", {
    session: "b",
    state: "done",
    next: 1,
    lines: [{ track: "room", start_ms: 0, end_ms: 1000, speaker: null, text: "B's transcript" }],
  });
  view.rerender(
    <BridgeProvider bridge={fake}>
      <Transcript session="b" />
    </BridgeProvider>,
  );
  await screen.findByText("B's transcript");
  await act(async () => rejectOld(new Error("A is gone")));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
