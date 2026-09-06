import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { LivePane, type PhasePayload } from "./LivePane";

function renderPane(fake: FakeBridge, payload: PhasePayload | undefined) {
  return render(
    <BridgeProvider bridge={fake}>
      <LivePane payload={payload} />
    </BridgeProvider>,
  );
}

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

  it("armed renders Record this call and Not this one", () => {
    const fake = new FakeBridge();
    renderPane(fake, {
      kind: "armed",
      app: "us.zoom.xos",
      live: null,
      queue: null,
      failure: null,
    });

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

  it("idle with a queue string renders it and no card", () => {
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
  });
});
