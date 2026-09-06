import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { NamingStrip } from "./NamingStrip";

function renderStrip(fake: FakeBridge) {
  return render(
    <BridgeProvider bridge={fake}>
      <NamingStrip session="2026-09-01T1000" />
    </BridgeProvider>,
  );
}

describe("NamingStrip", () => {
  it("calls speakers.name with the chosen label and name", async () => {
    const fake = new FakeBridge();
    fake.answer("speakers.unnamed", [{ label: "call-1", sample: "shall we begin" }]);
    fake.answer("config.get", { roster: ["Marcus", "Priya"] });
    fake.answer("speakers.name", { renamed: 3 });
    renderStrip(fake);

    // An `<input list=…>` (the naming combobox) has an implicit ARIA role of
    // `combobox`, not `textbox`, because of the `list` attribute.
    const input = await screen.findByRole("combobox", { name: "Name for call-1" });
    await userEvent.type(input, "Marcus");
    await userEvent.click(screen.getByRole("button", { name: "Name" }));

    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "speakers.name");
      expect(call?.params).toEqual({ session: "2026-09-01T1000", label: "call-1", name: "Marcus" });
    });
  });

  it("Undo calls speakers.undo for the strip's session", async () => {
    const fake = new FakeBridge();
    fake.answer("speakers.unnamed", []);
    fake.answer("config.get", { roster: [] });
    fake.answer("speakers.undo", { reverted: 2 });
    renderStrip(fake);

    await userEvent.click(await screen.findByRole("button", { name: "Undo" }));

    await waitFor(() => {
      const call = fake.calls.find((c) => c.method === "speakers.undo");
      expect(call?.params).toEqual({ session: "2026-09-01T1000" });
    });
  });
});
