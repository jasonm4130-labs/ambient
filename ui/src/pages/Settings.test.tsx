import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { Settings } from "./Settings";
import type { SettingsConfig } from "./settings/types";

const baseConfig: SettingsConfig = {
  apps: [],
  input_device: null,
  diarize: true,
  threshold: 0.5,
  sessions_dir: null,
  devices: ["MacBook Pro Microphone"],
  default_dir: "/Users/x/Documents/Ambient",
  ask_before_recording: true,
  audio_retention: "7",
  roster: ["Marcus"],
  latest_session: null,
};

function renderSettings(fake: FakeBridge): void {
  render(
    <BridgeProvider bridge={fake}>
      <Settings />
    </BridgeProvider>,
  );
}

describe("Settings", () => {
  it("toggling diarize calls config.set with the flipped value", async () => {
    const fake = new FakeBridge();
    fake.answer("config.get", baseConfig);
    fake.answer("config.set", baseConfig);
    renderSettings(fake);

    const toggle = await screen.findByRole("switch", {
      name: "Separate voices after recording",
    });
    await userEvent.click(toggle);

    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "config.set");
      expect(call?.params).toEqual({ key: "diarize", value: "false" });
    });
  });

  it("choosing Everything and Selected apps sends the right capture.scope", async () => {
    const fake = new FakeBridge();
    fake.answer("config.get", { ...baseConfig, apps: ["us.zoom.xos"] });
    fake.answer("capture.scope", {});
    renderSettings(fake);

    const scope = await screen.findByRole("combobox", { name: "Capture scope" });
    await userEvent.selectOptions(scope, "all");
    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "capture.scope");
      expect(call?.params).toEqual({ everything: true });
    });

    await userEvent.selectOptions(scope, "some");
    await waitFor(() => {
      const calls = fake.calls.filter((c) => c.method === "capture.scope");
      expect(calls.at(-1)?.params).toEqual({ everything: false });
    });
  });

  it("adding a person calls roster.add", async () => {
    const fake = new FakeBridge();
    fake.answer("config.get", baseConfig);
    fake.answer("roster.add", [...baseConfig.roster, "Dana"]);
    renderSettings(fake);

    const input = await screen.findByRole("textbox", { name: "Add someone" });
    await userEvent.type(input, "Dana");
    await userEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "roster.add");
      expect(call?.params).toEqual({ name: "Dana" });
    });
  });

  it("a config.set that rejects while changing the sessions folder renders the refusal", async () => {
    const fake = new FakeBridge();
    fake.answer("config.get", baseConfig);
    fake.answer("pick_dir", { chosen: "/Users/x/Elsewhere" });
    fake.answer("config.set", () => {
      throw new Error("a recording is in progress in /Users/x/Documents/Ambient");
    });
    renderSettings(fake);

    const change = await screen.findByRole("button", { name: "Change…" });
    await userEvent.click(change);

    await screen.findByText(/a recording is in progress/u);
  });

  it("every button and switch on the screen has an accessible name", async () => {
    const fake = new FakeBridge();
    fake.answer("config.get", { ...baseConfig, apps: ["us.zoom.xos"] });
    renderSettings(fake);

    await screen.findByRole("heading", { name: "Settings" });
    // The app chip's remove button only renders once the config with an app
    // in it has been rendered.
    await screen.findByRole("button", { name: "Stop capturing us.zoom.xos" });

    for (const button of screen.getAllByRole("button")) {
      expect(button).toHaveAccessibleName();
    }
    for (const toggle of screen.getAllByRole("switch")) {
      expect(toggle).toHaveAccessibleName();
    }
  });
});
