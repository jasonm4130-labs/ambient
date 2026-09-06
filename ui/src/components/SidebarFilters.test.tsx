import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it } from "vitest";
import { SessionList, type SessionSummary } from "./SessionList";

const sessions: SessionSummary[] = Array.from({ length: 2000 }, (_, i) => ({
  id: `session-${i}`,
  dir: `/x/${i}`,
  name: i === 1999 ? "Last standup" : `Review ${i}`,
  started_at: "2026-09-01",
  duration_s: 60,
  transcribed: true,
  live: false,
  transcribing: false,
  error: null,
  tags: i % 2 === 0 ? ["1:1"] : [],
  pinned: false,
}));

it("virtualises 2,000 sessions and renders the last row at the correct offset", () => {
  render(<SessionList sessions={sessions} selectedId={null} onSelect={() => {}} />);
  expect(screen.getAllByTestId("session-row").length).toBeLessThan(60);
  const viewport = screen.getByTestId("session-list-viewport");
  fireEvent.scroll(viewport, { target: { scrollTop: 28 + 2000 * 56 - 600 } });
  expect(screen.getByText("Last standup")).toBeInTheDocument();
  const spacer = screen.getByTestId("session-list-top-spacer");
  const first = screen.getAllByTestId("session-row")[0]!;
  const index = Number(first.dataset.session?.replace("session-", ""));
  expect(spacer).toHaveStyle({ height: `${28 + index * 56}px` });
});

it("filters names and tags and shows each month header once", async () => {
  render(<SessionList sessions={sessions.slice(-4)} selectedId={null} onSelect={() => {}} />);
  expect(screen.getAllByText("September 2026")).toHaveLength(1);
  await userEvent.type(screen.getByRole("searchbox", { name: "Filter sessions" }), "standup");
  expect(screen.getAllByTestId("session-row")).toHaveLength(1);
  await userEvent.clear(screen.getByRole("searchbox", { name: "Filter sessions" }));
  await userEvent.click(screen.getByRole("button", { name: "Filter tag 1:1" }));
  expect(screen.getAllByTestId("session-row")).toHaveLength(2);
});

it("keeps long names and six tags inside a fixed 56 px row", () => {
  render(
    <SessionList
      sessions={[{ ...sessions[0]!, name: "x".repeat(300), tags: ["a", "b", "c", "d", "e", "f"] }]}
      selectedId={null}
      onSelect={() => {}}
    />,
  );
  expect(screen.getByTestId("session-row")).toHaveStyle({ height: "56px" });
  expect(screen.getByTestId("session-row-name")).toHaveClass("truncate");
  expect(screen.getByText("+4")).toBeInTheDocument();
});
