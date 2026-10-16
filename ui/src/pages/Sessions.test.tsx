import { act, render, screen } from "@testing-library/react";
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

function sessionSummary(id: string, name: string | null, live: boolean) {
  return {
    id,
    dir: `/x/${id}`,
    name,
    started_at: "2026-10-09",
    duration_s: 60,
    transcribed: !live,
    live,
    transcribing: false,
    error: null,
    tags: [],
    pinned: false,
  };
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

  it("shows a broken session's error above its transcript", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [
      {
        id: "a",
        dir: "/x/a",
        name: "Standup",
        started_at: "2026-09-01",
        duration_s: 60,
        transcribed: false,
        live: false,
        transcribing: false,
        error: "could not read session.json",
        tags: [],
        pinned: false,
      },
    ]);
    fake.answer("transcript", { session: "a", state: "broken", next: 0, lines: [] });
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });

    renderSessions(fake);

    await userEvent.click(await screen.findByRole("button", { name: /Standup/u }));
    expect(await screen.findByTestId("session-error")).toHaveTextContent(
      "could not read session.json",
    );
  });

  it("keeps a single session header after switching sessions", async () => {
    const fake = new FakeBridge();
    const summary = (id: string, name: string) => ({
      id,
      dir: `/x/${id}`,
      name,
      started_at: "2026-09-01",
      duration_s: 60,
      transcribed: true,
      live: false,
      transcribing: false,
      error: null,
      tags: [],
      pinned: false,
    });
    fake.answer("sessions", [summary("a", "Standup"), summary("b", "Retro")]);
    fake.answer("transcript", { session: "a", state: "done", next: 0, lines: [] });
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });

    renderSessions(fake);

    await userEvent.click(await screen.findByRole("button", { name: /Standup/u }));
    await screen.findByDisplayValue("Standup");
    await userEvent.click(await screen.findByRole("button", { name: /Retro/u }));
    await screen.findByDisplayValue("Retro");
    expect(screen.getAllByRole("textbox", { name: "Session name" })).toHaveLength(1);
  });

  it("calls onSettings when the Settings row is clicked", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", []);
    const onSettings = vi.fn();

    renderSessions(fake, onSettings);

    await userEvent.click(await screen.findByRole("button", { name: "Settings" }));
    expect(onSettings).toHaveBeenCalled();
  });
  it("selects a recording's session when it starts, and a later choice sticks", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [
      sessionSummary("live1", null, true),
      sessionSummary("a", "Standup", false),
    ]);
    fake.answer("transcript", { session: "a", state: "done", next: 0, lines: [] });
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });
    const phase = {
      kind: "recording",
      app: null,
      live: {
        id: "live1",
        elapsed_s: 5,
        room_level: 0,
        call_level: 0,
        audio_arriving: true,
        status_line: "Recording",
      },
      queue: null,
      failure: null,
    };

    const { container } = renderSessions(fake);
    await screen.findByRole("button", { name: /Standup/u });
    const main = () => container.querySelector("main")!;

    act(() =>
      fake.emit("phase", { kind: "idle", app: null, live: null, queue: null, failure: null }),
    );
    expect(main()).toHaveAttribute("data-selected-session", "");
    act(() => fake.emit("phase", phase));
    expect(main()).toHaveAttribute("data-selected-session", "live1");

    await userEvent.click(screen.getByRole("button", { name: /Standup/u }));
    act(() => fake.emit("phase", { ...phase, live: { ...phase.live, elapsed_s: 6 } }));
    expect(main()).toHaveAttribute("data-selected-session", "a");
  });

  it("sends Add apps on the idle card to Settings", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", []);
    fake.answer("config.get", { apps: [], devices: [], input_device: null });
    const onSettings = vi.fn();

    renderSessions(fake, onSettings);
    act(() =>
      fake.emit("phase", { kind: "idle", app: null, live: null, queue: null, failure: null }),
    );

    await userEvent.click(await screen.findByRole("button", { name: "Add apps" }));
    expect(onSettings).toHaveBeenCalled();
  });

  it("re-reads the mic when the window regains focus and on a phase edge", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", []);
    fake.answer("config.get", { apps: [], devices: ["USB mic"], input_device: "USB mic" });

    renderSessions(fake);
    const idle = { kind: "idle", app: null, live: null, queue: null, failure: null };
    act(() => fake.emit("phase", idle));
    expect(await screen.findByText("Mic: USB mic")).toBeInTheDocument();

    fake.answer("config.get", { apps: [], devices: [], input_device: "USB mic" });
    act(() => window.dispatchEvent(new Event("focus")));
    expect(
      await screen.findByText("Mic: USB mic is not connected, so the system default is used"),
    ).toBeInTheDocument();

    fake.answer("config.get", { apps: [], devices: ["USB mic"], input_device: "USB mic" });
    act(() => fake.emit("phase", { ...idle, kind: "armed" }));
    act(() => fake.emit("phase", idle));
    expect(await screen.findByText("Mic: USB mic")).toBeInTheDocument();
  });
});
