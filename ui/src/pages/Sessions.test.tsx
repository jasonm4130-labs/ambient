import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { Sessions } from "./Sessions";

function renderSessions(fake: FakeBridge, onSettings: () => void = () => {}) {
  return render(
    <BridgeProvider bridge={fake}>
      <Sessions onSettings={onSettings} />
    </BridgeProvider>,
  );
}

describe("Sessions", () => {
  it("renders the sidebar heading and switches to a session's transcript on selection", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [
      {
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
      },
    ]);
    fake.answer("transcript", {
      session: "a",
      state: "done",
      next: 1,
      lines: [{ track: "room", start_ms: 0, end_ms: 1000, speaker: "Marcus", text: "hi there" }],
    });
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });

    renderSessions(fake);

    await screen.findByRole("heading", { name: "Sessions" });
    await userEvent.click(await screen.findByRole("button", { name: /Standup/u }));
    await screen.findByText("hi there");
  });

  it("calls onSettings when the Settings row is clicked", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", []);
    const onSettings = vi.fn();

    renderSessions(fake, onSettings);

    await userEvent.click(await screen.findByRole("button", { name: "Settings" }));
    expect(onSettings).toHaveBeenCalled();
  });
});
