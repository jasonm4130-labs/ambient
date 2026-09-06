import { useCallback, useEffect, useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import type { SessionSummary } from "./SessionList";
import { SessionHeader } from "./SessionHeader";

function summary(overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: "a",
    dir: "/x/a",
    name: "Standup",
    started_at: "2026-09-01",
    duration_s: 125,
    transcribed: true,
    live: false,
    transcribing: false,
    error: null,
    tags: [],
    pinned: false,
    ...overrides,
  };
}

/// Owns the `sessions` reply and re-requests it after `onChanged`, the same
/// contract `Sessions` follows — this is what makes the "renders from the
/// `sessions` reply, not local state" assertion below load-bearing: a header
/// that kept the typed name in local state would still pass every other
/// test here.
function Harness({ bridge, onDeleted }: { bridge: FakeBridge; onDeleted?: () => void }) {
  const [current, setCurrent] = useState<SessionSummary | null>(null);

  const refresh = useCallback(async () => {
    const list = await bridge.call<SessionSummary[]>("sessions");
    setCurrent(list[0] ?? null);
  }, [bridge]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (current === null) return null;
  return <SessionHeader summary={current} onChanged={refresh} {...(onDeleted && { onDeleted })} />;
}

function renderHeader(bridge: FakeBridge, onDeleted?: () => void) {
  return render(
    <BridgeProvider bridge={bridge}>
      <Harness bridge={bridge} {...(onDeleted && { onDeleted })} />
    </BridgeProvider>,
  );
}

describe("SessionHeader", () => {
  it("renames on Enter and re-renders the committed name from the sessions reply, not local state", async () => {
    const fake = new FakeBridge();
    let calls = 0;
    fake.answer("sessions", () => {
      calls += 1;
      return [summary({ name: calls === 1 ? "Standup" : "Standup (renamed)" })];
    });
    fake.answer("session.update", { id: "a", name: "Standup", ended_at: "" });

    renderHeader(fake);

    const name = await screen.findByRole("textbox", { name: "Session name" });
    await userEvent.clear(name);
    await userEvent.type(name, "Standup{Enter}");

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "session.update").at(-1);
      expect(call?.params).toEqual({ session: "a", name: "Standup" });
    });

    // The refetch triggered by `onChanged` returns a *different* name than
    // what was typed — only a component that re-renders from the reply
    // shows it.
    await screen.findByDisplayValue("Standup (renamed)");
  });

  it("cancels an edit on Escape without calling session.update", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);

    renderHeader(fake);

    const name = await screen.findByRole("textbox", { name: "Session name" });
    await userEvent.clear(name);
    await userEvent.type(name, "Something else{Escape}");

    expect(fake.calls.some((c) => c.method === "session.update")).toBe(false);
    await screen.findByDisplayValue("Standup");
  });

  it("adds one tag at a time and removes one tag at a time, never an array", async () => {
    const fake = new FakeBridge();
    let calls = 0;
    fake.answer("sessions", () => {
      calls += 1;
      return [summary({ tags: calls === 1 ? [] : ["1:1"] })];
    });
    fake.answer("session.update", { id: "a", name: null, ended_at: "" });

    renderHeader(fake);

    const tagInput = await screen.findByLabelText("Add tag");
    await userEvent.type(tagInput, "1:1{Enter}");

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "session.update").at(-1);
      expect(call?.params).toEqual({ session: "a", add_tag: "1:1" });
    });

    const chipRemove = await screen.findByRole("button", { name: "Remove tag 1:1" });
    await userEvent.click(chipRemove);

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "session.update").at(-1);
      expect(call?.params).toEqual({ session: "a", remove_tag: "1:1" });
    });
  });

  it("saves notes on blur", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);
    fake.answer("session.update", { id: "a", name: null, ended_at: "" });

    renderHeader(fake);

    const notes = await screen.findByRole("textbox", { name: "Notes" });
    await userEvent.type(notes, "call back tomorrow");
    await userEvent.tab();

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "session.update").at(-1);
      expect(call?.params).toEqual({ session: "a", notes: "call back tomorrow" });
    });
  });

  it("does not call session.update on a blur with nothing typed", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);

    renderHeader(fake);

    const notes = await screen.findByRole("textbox", { name: "Notes" });
    await userEvent.click(notes);
    await userEvent.tab();

    expect(fake.calls.some((c) => c.method === "session.update")).toBe(false);
  });

  it("Delete then Confirm calls session.delete", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);
    fake.answer("session.delete", { session: "a", deleted: true });
    const onDeleted = vi.fn();

    renderHeader(fake, onDeleted);

    await userEvent.click(await screen.findByRole("button", { name: "More actions" }));
    await userEvent.click(await screen.findByRole("button", { name: "Delete session…" }));
    await screen.findByText(/Delete "Standup"/u);
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));

    await waitFor(() => {
      expect(fake.calls.filter((c) => c.method === "session.delete")).toHaveLength(1);
    });
    await waitFor(() => expect(onDeleted).toHaveBeenCalled());
  });

  it("Delete then Cancel calls nothing", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);

    renderHeader(fake);

    await userEvent.click(await screen.findByRole("button", { name: "More actions" }));
    await userEvent.click(await screen.findByRole("button", { name: "Delete session…" }));
    await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(fake.calls.filter((c) => c.method === "session.delete")).toHaveLength(0);
  });

  it("Reveal in Finder calls the reveal bridge method", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary()]);
    fake.answer("reveal", { ok: true });

    renderHeader(fake);

    await userEvent.click(await screen.findByRole("button", { name: "More actions" }));
    await userEvent.click(await screen.findByRole("button", { name: "Reveal in Finder" }));

    await waitFor(() => {
      const call = fake.calls.filter((c) => c.method === "reveal").at(-1);
      expect(call?.params).toEqual({ session: "a" });
    });
  });

  it("a live summary disables editing but leaves Reveal enabled", async () => {
    const fake = new FakeBridge();
    fake.answer("sessions", [summary({ live: true })]);

    renderHeader(fake);

    const name = await screen.findByRole("textbox", { name: "Session name" });
    expect(name).toBeDisabled();
    expect(screen.getByRole("button", { name: "Pin" })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: "More actions" }));
    expect(screen.getByRole("button", { name: "Reveal in Finder" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete session…" })).toBeDisabled();
  });
});
