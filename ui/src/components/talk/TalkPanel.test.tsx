import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { BridgeProvider } from "@/lib/bridge-context";
import { FakeBridge } from "@/test/fake-bridge";
import { on, turn } from "./fixtures";
import { TalkPanel } from "./TalkPanel";

describe("TalkPanel", () => {
  it("shows both sides of every turn as text", () => {
    render(
      <BridgeProvider bridge={new FakeBridge()}>
        <TalkPanel
          payload={{
            ...on,
            turns: [
              turn("done", { id: 1, reply: "All green.", reply_text: "All green." }),
              turn("waiting", { id: 2, you: "and the deploy?", note: "Queued: no agent" }),
              turn("nothing", { id: 3, you: null, note: "Didn't catch that." }),
            ],
          }}
        />
      </BridgeProvider>,
    );
    const turns = screen.getAllByTestId("talk-turn");
    expect(turns).toHaveLength(3);
    expect(turns[0]).toHaveTextContent("You said: status?");
    expect(turns[0]).toHaveTextContent("Reply: All green.");
    expect(turns[1]).toHaveTextContent("Thinking…");
    expect(turns[1]).toHaveTextContent("Queued: no agent");
    expect(turns[2]).toHaveTextContent("Didn't catch that.");
    expect(screen.getByRole("heading", { name: "Talking to your agent" })).toBeInTheDocument();
    expect(screen.getByTestId("talk-listener")).toHaveTextContent("Listening: Claude Code");
  });

  it("the reply bubble shows reply_text without markdown symbols", () => {
    render(
      <BridgeProvider bridge={new FakeBridge()}>
        <TalkPanel
          payload={{
            ...on,
            turns: [
              turn("speaking", {
                reply: "**Green.** See [the run](https://ci.example/1).\n\n- 512 passed",
                reply_text: "Green. See the run.\n\n• 512 passed",
              }),
            ],
          }}
        />
      </BridgeProvider>,
    );
    const bubble = screen.getByTestId("talk-reply");
    expect(bubble).toHaveTextContent("Green. See the run.");
    expect(bubble).toHaveTextContent("• 512 passed");
    expect(bubble.textContent).not.toMatch(/[*[\]]/u);
    expect(screen.getByRole("img", { name: "Being read aloud" })).toBeInTheDocument();
    expect(bubble).toHaveAttribute("aria-busy", "false");
  });

  it("a reply still to come is marked busy", () => {
    render(
      <BridgeProvider bridge={new FakeBridge()}>
        <TalkPanel payload={{ ...on, turns: [turn("waiting")] }} />
      </BridgeProvider>,
    );
    expect(screen.getByTestId("talk-reply")).toHaveAttribute("aria-busy", "true");
  });

  it("the speak switch sets talk.speak", async () => {
    const fake = new FakeBridge();
    render(
      <BridgeProvider bridge={fake}>
        <TalkPanel payload={on} />
      </BridgeProvider>,
    );
    expect(screen.getByTestId("talk-empty")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("switch", { name: "Speak replies" }));
    await waitFor(() =>
      expect(fake.calls).toContainEqual({
        method: "config.set",
        params: { key: "talk.speak", value: "false" },
      }),
    );
  });
});
