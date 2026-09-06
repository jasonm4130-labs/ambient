import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { ExportMenu } from "./ExportMenu";

function renderMenu(fake: FakeBridge) {
  return render(
    <BridgeProvider bridge={fake}>
      <ExportMenu session="2026-09-05-1200" />
    </BridgeProvider>,
  );
}

async function openMenu() {
  await userEvent.click(await screen.findByTestId("export-menu-trigger"));
}

describe("ExportMenu", () => {
  it("Copy for an assistant calls export then clipboard.write with the returned text, and toasts Copied", async () => {
    const fake = new FakeBridge();
    fake.answer("export", { session: "2026-09-05-1200", format: "assistant", text: "the transcript" });
    fake.answer("clipboard.write", { done: true });

    renderMenu(fake);
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-copy-assistant"));

    await waitFor(() => {
      expect(fake.calls.filter((c) => c.method === "export")).toHaveLength(1);
    });
    expect(fake.calls.find((c) => c.method === "export")?.params).toEqual({
      session: "2026-09-05-1200",
      format: "assistant",
    });

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "clipboard.write").at(-1);
      expect(call?.params).toEqual({ text: "the transcript" });
    });

    await screen.findByText("Copied");
  });

  it("Save as… → SRT calls save with the selected session's id", async () => {
    const fake = new FakeBridge();
    fake.answer("save", { path: "/Users/x/Documents/session.srt" });

    renderMenu(fake);
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-save-trigger"));
    await userEvent.click(await screen.findByTestId("export-save-srt"));

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "save").at(-1);
      expect(call?.params).toEqual({ session: "2026-09-05-1200", format: "srt" });
    });

    await screen.findByText("Saved to /Users/x/Documents/session.srt");
  });

  it("a {path: null} reply (cancelled) shows no toast", async () => {
    const fake = new FakeBridge();
    fake.answer("save", { path: null });

    renderMenu(fake);
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-save-trigger"));
    await userEvent.click(await screen.findByTestId("export-save-srt"));

    await waitFor(() => {
      expect(fake.calls.filter((c) => c.method === "save")).toHaveLength(1);
    });
    expect(screen.queryByText(/Saved to/u)).not.toBeInTheDocument();
    expect(screen.queryByText("Copied")).not.toBeInTheDocument();
  });

  it("a rejected export shows Export failed and leaves the menu usable", async () => {
    const fake = new FakeBridge();
    fake.answer("export", () => {
      throw new Error("no such session");
    });

    renderMenu(fake);
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-copy-assistant"));

    await screen.findByText("Export failed: no such session");

    // The menu is still usable: it stayed open and its entries are still
    // there to click.
    await screen.findByTestId("export-copy-assistant");
  });
});
