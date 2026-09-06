import { readFileSync } from "node:fs";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { Welcome, fixes } from "./Welcome";

it("shows failed checks with inline fixes and no recording action", () => {
  render(
    <BridgeProvider bridge={new FakeBridge()}>
      <Welcome
        checks={[
          { name: "models/asr", ok: false, detail: "ASR missing" },
          { name: "config", ok: false, detail: "Invalid JSON" },
        ]}
        onRetry={vi.fn()}
      />
    </BridgeProvider>,
  );
  expect(screen.getByText("ASR missing")).toBeInTheDocument();
  expect(screen.getByText("Invalid JSON")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Start recording" })).not.toBeInTheDocument();
  const documentation = readFileSync("../docs/using/troubleshooting.md", "utf8");
  for (const fix of Object.values(fixes)) expect(documentation).toContain(fix);
});

it("starts recording only when all checks pass", async () => {
  const fake = new FakeBridge();
  fake.answer("record.start", { sent: true });
  render(
    <BridgeProvider bridge={fake}>
      <Welcome checks={[{ name: "config", ok: true, detail: "defaults" }]} onRetry={vi.fn()} />
    </BridgeProvider>,
  );
  await userEvent.click(screen.getByRole("button", { name: "Start recording" }));
  expect(fake.calls).toContainEqual({ method: "record.start", params: undefined });
});
