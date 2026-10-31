import type { TalkPayload, TalkTurn } from "./text";

/// Talking mode on, an agent listening, nothing said yet.
export const on: TalkPayload = {
  enabled: true,
  speak: true,
  listener: { since: "2026-10-31T21:40:00+11:00", push: true, client: "claude-code" },
  chord: "⌥Space",
  hotkey_error: null,
  listening: false,
  paused: null,
  notice: null,
  turns: [],
};

export function turn(state: TalkTurn["state"], extra: Partial<TalkTurn> = {}): TalkTurn {
  return { id: 1, you: "status?", reply: null, reply_text: null, state, note: null, ...extra };
}
