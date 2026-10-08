import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { act } from "react";
import { describe, expect, it, vi } from "vitest";
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

    // The menu is still usable: reopening it shows its entries again.
    await openMenu();
    await screen.findByTestId("export-copy-assistant");
  });

  it("closes on an action, on Escape and on an outside click", async () => {
    const fake = new FakeBridge();
    fake.answer("save", { path: null });

    renderMenu(fake);
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-save-trigger"));
    await userEvent.click(await screen.findByTestId("export-save-srt"));
    expect(screen.queryByTestId("export-copy-assistant")).toBeNull();

    await openMenu();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByTestId("export-copy-assistant")).toBeNull();

    await openMenu();
    await userEvent.click(document.body);
    expect(screen.queryByTestId("export-copy-assistant")).toBeNull();
  });

  it("the Saved to toast clears itself", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const fake = new FakeBridge();
      fake.answer("save", { path: "/Users/x/session.srt" });

      renderMenu(fake);
      await openMenu();
      await userEvent.click(await screen.findByTestId("export-save-trigger"));
      await userEvent.click(await screen.findByTestId("export-save-srt"));
      await screen.findByTestId("export-toast");

      act(() => {
        vi.advanceTimersByTime(5000);
      });
      expect(screen.queryByTestId("export-toast")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
  it("a repeated identical toast gets a fresh timeout", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const fake = new FakeBridge();
      fake.answer("save", { path: "/Users/x/session.srt" });

      renderMenu(fake);
      const saveSrt = async () => {
        await openMenu();
        await userEvent.click(await screen.findByTestId("export-save-trigger"));
        await userEvent.click(await screen.findByTestId("export-save-srt"));
      };
      await saveSrt();
      await screen.findByTestId("export-toast");

      act(() => {
        vi.advanceTimersByTime(3000);
      });
      await saveSrt();
      await waitFor(() => {
        expect(fake.calls.filter((c) => c.method === "save")).toHaveLength(2);
      });

      act(() => {
        vi.advanceTimersByTime(2000);
      });
      expect(screen.getByTestId("export-toast")).toBeInTheDocument();

      act(() => {
        vi.advanceTimersByTime(2500);
      });
      expect(screen.queryByTestId("export-toast")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("the export error clears on its dismiss button, on Escape and on the next export action", async () => {
    const fake = new FakeBridge();
    fake.answer("export", () => {
      throw new Error("no such session");
    });
    fake.answer("save", () => new Promise(() => {}));

    renderMenu(fake);
    const fail = async () => {
      await openMenu();
      await userEvent.click(await screen.findByTestId("export-copy-assistant"));
      await screen.findByTestId("export-error");
    };

    await fail();
    await userEvent.click(screen.getByRole("button", { name: "Dismiss export error" }));
    expect(screen.queryByTestId("export-error")).toBeNull();

    await fail();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByTestId("export-error")).toBeNull();

    await fail();
    await openMenu();
    await userEvent.click(await screen.findByTestId("export-save-trigger"));
    await userEvent.click(await screen.findByTestId("export-save-srt"));
    expect(screen.queryByTestId("export-error")).toBeNull();
  });
});
