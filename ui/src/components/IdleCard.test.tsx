import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { IdleCard, micLine, type CaptureConfig } from "./IdleCard";

function renderCard(fake: FakeBridge, config?: CaptureConfig, onAddApps?: () => void) {
  return render(
    <BridgeProvider bridge={fake}>
      <IdleCard config={config} onAddApps={onAddApps} />
    </BridgeProvider>,
  );
}

describe("IdleCard", () => {
  it("Record calls record.start and shows the ⌘R hint", async () => {
    const fake = new FakeBridge();
    fake.answer("record.start", { sent: true });
    renderCard(fake);

    expect(screen.getByText("⌘R")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Record" }));
    await waitFor(() => {
      expect(fake.calls.some((c) => c.method === "record.start")).toBe(true);
    });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("says so when the menu bar refuses to start", async () => {
    const fake = new FakeBridge();
    fake.answer("record.start", { sent: false });
    renderCard(fake);

    await userEvent.click(screen.getByRole("button", { name: "Record" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("could not be started");
  });

  it("offers Record before config.get has answered, with no mic or watch line", () => {
    renderCard(new FakeBridge());

    expect(screen.getByRole("button", { name: "Record" })).toBeEnabled();
    expect(screen.queryByTestId("live-mic")).not.toBeInTheDocument();
    expect(screen.queryByTestId("live-watch")).not.toBeInTheDocument();
  });

  it("links an empty watch list to Settings", async () => {
    const onAddApps = vi.fn();
    renderCard(new FakeBridge(), { apps: [], devices: [], input_device: null }, onAddApps);

    expect(screen.getByTestId("live-watch")).toHaveTextContent("Not watching any call apps");
    await userEvent.click(screen.getByRole("button", { name: "Add apps" }));
    expect(onAddApps).toHaveBeenCalled();
  });

  it("shows no watch line once an app is watched", () => {
    renderCard(new FakeBridge(), { apps: ["us.zoom.xos"], devices: [], input_device: null });

    expect(screen.queryByTestId("live-watch")).not.toBeInTheDocument();
  });
});

describe("micLine", () => {
  it("names the system default when nothing is saved", () => {
    expect(micLine({ input_device: null, devices: ["MacBook Pro Microphone"] })).toBe(
      "Mic: System default",
    );
  });

  it("names a saved device that is plugged in", () => {
    expect(micLine({ input_device: "USB mic", devices: ["USB mic"] })).toBe("Mic: USB mic");
  });

  it("says a saved device that is unplugged falls back to the system default", () => {
    expect(micLine({ input_device: "USB audio CODEC", devices: ["MacBook Pro Microphone"] })).toBe(
      "Mic: USB audio CODEC is not connected, so the system default is used",
    );
  });

  it("does not claim a device is unplugged when the device list is unknown", () => {
    expect(micLine({ input_device: "USB mic" })).toBe("Mic: USB mic");
  });
});
