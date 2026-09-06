import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { SessionList, type SessionSummary } from "./SessionList";

const base: SessionSummary = {
  id: "2026-09-01T1000",
  dir: "/x/2026-09-01T1000",
  name: null,
  started_at: "2026-09-01 10:00",
  duration_s: 125,
  transcribed: false,
  live: false,
  transcribing: false,
  error: null,
  tags: [],
  pinned: false,
};

// Three fixtures chosen to exercise `sessionState`'s precedence: one that is
// `live` *and* `transcribed` still reads `live`, one is `broken` (an error
// outranks `done`), and one is a plain `done`.
const fixtures: SessionSummary[] = [
  { ...base, id: "a", name: "Standup", live: true, transcribed: true },
  { ...base, id: "b", name: "1:1", error: "could not read session.json" },
  { ...base, id: "c", name: "Retro", transcribed: true },
];

describe("SessionList", () => {
  it("renders three summaries with the right state badges", () => {
    render(<SessionList sessions={fixtures} selectedId={null} onSelect={() => {}} />);

    const rows = screen.getAllByTestId("session-row");
    expect(rows).toHaveLength(3);
    expect(rows[0]).toHaveTextContent("live");
    expect(rows[1]).toHaveTextContent("broken");
    expect(rows[2]).toHaveTextContent("done");
  });

  it("calls onSelect on click", async () => {
    const onSelect = vi.fn();
    render(<SessionList sessions={fixtures} selectedId={null} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole("button", { name: /1:1/u }));
    expect(onSelect).toHaveBeenCalledWith("b");
  });

  it("calls onSelect on ArrowDown from a row", async () => {
    const onSelect = vi.fn();
    render(<SessionList sessions={fixtures} selectedId={null} onSelect={onSelect} />);

    screen.getByRole("button", { name: /Standup/u }).focus();
    await userEvent.keyboard("{ArrowDown}");
    expect(onSelect).toHaveBeenCalledWith("b");
  });
});
