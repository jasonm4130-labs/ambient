import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { LivePane, type PhasePayload } from "./LivePane";

function renderPane(
  fake: FakeBridge,
  payload: PhasePayload | undefined,
  extra: Partial<Parameters<typeof LivePane>[0]> = {},
) {
  return render(
    <BridgeProvider bridge={fake}>
      <LivePane payload={payload} {...extra} />
    </BridgeProvider>,
  );
}

const recording: PhasePayload = {
  kind: "recording",
  app: null,
  live: {
    id: "2026-10-09T2010",
    elapsed_s: 754,
    room_level: 0.4,
    call_level: 0.2,
    audio_arriving: true,
    status_line: "transcribing live",
  },
  queue: null,
  failure: null,
};

describe("LivePane", () => {
  it("recording renders the elapsed time and two meters, and Stop calls record.stop", async () => {
    const fake = new FakeBridge();
    fake.answer("record.stop", { sent: true });
    renderPane(fake, {
      kind: "recording",
      app: "us.zoom.xos",
      live: {
        id: "2026-09-06T1000",
        elapsed_s: 65,
        room_level: 0.3,
        call_level: 0.7,
        audio_arriving: true,
        status_line: "Recording",
      },
      queue: null,
      failure: null,
    });

    expect(screen.getByTestId("live-clock")).toHaveTextContent("01:05");
    expect(screen.getByTestId("live-meter-room")).toHaveAttribute("data-level", "0.3");
    expect(screen.getByTestId("live-meter-call")).toHaveAttribute("data-level", "0.7");

    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "record.stop")).toBe(true);
    });
  });

  it("recording names the live session and opens it", async () => {
    const onOpenLive = vi.fn();
    renderPane(new FakeBridge(), recording, { liveName: "Standup", onOpenLive });

    expect(screen.getByTestId("live-dot")).toBeInTheDocument();
    expect(screen.getByText("Recording")).toBeInTheDocument();
    await userEvent.click(screen.getByTestId("live-open"));
    expect(screen.getByTestId("live-open")).toHaveTextContent("Standup");
    expect(onOpenLive).toHaveBeenCalled();
  });

  it("recording falls back to the session id for an unnamed session", () => {
    renderPane(new FakeBridge(), recording, { liveName: null });

    expect(screen.getByTestId("live-open")).toHaveTextContent("2026-10-09T2010");
  });

  it("armed renders Record this call and Not this one", () => {
    const fake = new FakeBridge();
    renderPane(fake, {
      kind: "armed",
      app: "us.zoom.xos",
      live: null,
      queue: null,
      failure: null,
    });

    expect(screen.getByTestId("armed-question")).toHaveTextContent("Record this call?");
    expect(screen.getByTestId("armed-app")).toHaveTextContent("us.zoom.xos started playing audio.");
    expect(screen.getByRole("button", { name: "Record this call" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Not this one" })).toBeInTheDocument();
  });

  it("failed with live: null renders the error and Dismiss calls dismiss, without crashing", async () => {
    const fake = new FakeBridge();
    fake.answer("dismiss", { sent: true });
    renderPane(fake, {
      kind: "failed",
      app: null,
      live: null,
      queue: null,
      failure: { error: "the models did not load", session: "2026-09-06T0900" },
    });

    expect(screen.getByTestId("live-failure-error")).toHaveTextContent("the models did not load");

    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "dismiss")).toBe(true);
    });
  });

  it("idle renders the Record card even with no queue", () => {
    renderPane(new FakeBridge(), {
      kind: "idle",
      app: null,
      live: null,
      queue: null,
      failure: null,
    });

    expect(screen.getByTestId("idle-card")).toBeInTheDocument();
    expect(screen.queryByTestId("live-queue")).not.toBeInTheDocument();
  });

  it("idle with a queue string renders it and no live card", () => {
    const fake = new FakeBridge();
    renderPane(fake, {
      kind: "idle",
      app: null,
      live: null,
      queue: "2 sessions queued",
      failure: null,
    });

    expect(screen.getByText("2 sessions queued")).toBeInTheDocument();
    expect(screen.queryByTestId("live-card")).not.toBeInTheDocument();
    expect(screen.getByTestId("idle-card")).toBeInTheDocument();
  });
});
