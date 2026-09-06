import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";
import { App } from "./App";
import { Transcript } from "./components/Transcript";
import { SessionHeader } from "./components/SessionHeader";
import { NamingStrip } from "./components/NamingStrip";
import { BridgeProvider } from "./lib/bridge-context";
import { FakeBridge } from "./test/fake-bridge";

it("shows failed metadata writes and keeps a rejected tag draft", async () => {
  const fake = new FakeBridge();
  fake.answer("session.update", () => {
    throw new Error("session is locked");
  });
  render(
    <BridgeProvider bridge={fake}>
      <SessionHeader
        summary={{
          id: "one",
          dir: "/one",
          name: "One",
          started_at: null,
          duration_s: 10,
          transcribed: true,
          transcribing: false,
          live: false,
          error: null,
          tags: [],
          pinned: false,
        }}
        onChanged={() => {}}
      />
    </BridgeProvider>,
  );
  const tag = screen.getByRole("textbox", { name: "Add tag" });
  fireEvent.change(tag, { target: { value: "team" } });
  fireEvent.keyDown(tag, { key: "Enter" });
  await screen.findByRole("alert");
  expect(screen.getByRole("alert")).toHaveTextContent("session is locked");
  expect(tag).toHaveValue("team");
});

it("shows naming failures and refreshes speaker labels when diarization finishes", async () => {
  const fake = new FakeBridge();
  fake.answer("speakers.unnamed", [{ label: "room-1", sample: "Hello" }]);
  fake.answer("config.get", { roster: [] });
  fake.answer("speakers.name", () => {
    throw new Error("naming is locked");
  });
  render(
    <BridgeProvider bridge={fake}>
      <NamingStrip session="one" />
    </BridgeProvider>,
  );
  const input = await screen.findByRole("combobox", { name: "Name for room-1" });
  fireEvent.change(input, { target: { value: "Pat" } });
  fireEvent.click(screen.getByRole("button", { name: /^Name$/u }));
  await screen.findByText("naming is locked");
  fake.answer("speakers.unnamed", [{ label: "room-2", sample: "New speaker" }]);
  act(() => fake.emit("diarize", { session: "one", state: "done" }));
  await screen.findByRole("combobox", { name: "Name for room-2" });
});

it("shows Welcome for an empty library and supports the menu shortcuts", async () => {
  const fake = new FakeBridge();
  fake.answer("init", { route: "sessions" });
  fake.answer("sessions", []);
  fake.answer("doctor", [{ name: "config", ok: true, detail: "defaults" }]);
  render(
    <BridgeProvider bridge={fake}>
      <App />
    </BridgeProvider>,
  );
  await screen.findByRole("heading", { name: "Welcome to Ambient" });
  expect(screen.getByRole("button", { name: "Start recording" })).toBeEnabled();
  fireEvent.keyDown(window, { key: ",", metaKey: true });
  await screen.findByRole("heading", { name: "Settings" });
  fireEvent.keyDown(window, { key: "0", metaKey: true });
  await screen.findByRole("heading", { name: "Sessions" });
  fireEvent.keyDown(window, { key: "r", metaKey: true });
  fireEvent.keyDown(window, { key: "s", metaKey: true });
  expect(fake.calls.map((call) => call.method)).toEqual(
    expect.arrayContaining(["record.start", "record.stop"]),
  );
});

it("keeps Settings reachable while health fails and rechecks after a repair", async () => {
  const fake = new FakeBridge();
  fake.answer("init", { route: "sessions" });
  fake.answer("sessions", []);
  fake.answer("doctor", [{ name: "config", ok: false, detail: "Broken configuration" }]);
  render(
    <BridgeProvider bridge={fake}>
      <App />
    </BridgeProvider>,
  );
  await screen.findByText("Broken configuration");
  expect(screen.queryByRole("button", { name: "Start recording" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Settings" })).toBeEnabled();
  fake.answer("doctor", [{ name: "config", ok: true, detail: "defaults" }]);
  act(() => fake.emit("config", {}));
  await screen.findByRole("button", { name: "Start recording" });
});

it("names every transcript button and routes native File commands to the selection", async () => {
  const fake = new FakeBridge();
  fake.answer("transcript", {
    session: "one",
    state: "done",
    next: 1,
    lines: [{ track: "room", speaker: "Pat", start_ms: 0, end_ms: 1000, text: "Hello" }],
  });
  fake.answer("export", { text: "# One" });
  render(
    <BridgeProvider bridge={fake}>
      <Transcript session="one" />
    </BridgeProvider>,
  );
  await screen.findByText("Hello");
  for (const button of screen.getAllByRole("button")) expect(button).toHaveAccessibleName();
  act(() => fake.emit("command", { action: "copy-markdown" }));
  act(() => fake.emit("command", { action: "reveal" }));
  await waitFor(() =>
    expect(fake.calls).toContainEqual({ method: "clipboard.write", params: { text: "# One" } }),
  );
  expect(fake.calls).toContainEqual({ method: "reveal", params: { session: "one" } });
});
