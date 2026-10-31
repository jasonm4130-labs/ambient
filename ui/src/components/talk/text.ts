/// One push-to-talk turn (`src/talk/app.rs`'s `Turn::payload`): both sides as
/// text, and where the turn has got to.
export interface TalkTurn {
  id: number;
  you: string | null;
  /// The agent's reply as it wrote it, markdown and all.
  reply: string | null;
  /// The same reply as plain text (`talk::readable`): what the window shows.
  /// Optional only so a payload from an older build still renders.
  reply_text?: string | null;
  state:
    | "hearing"
    | "sending"
    | "waiting"
    | "speaking"
    | "done"
    | "nothing"
    | "no_reply"
    | "stopped"
    | "failed";
  note: string | null;
}

/// The agent session listening through `ambient mcp`, from its heartbeat.
export interface TalkListener {
  /// RFC 3339.
  since: string;
  /// Questions are pushed as Claude Code channel messages, rather than
  /// collected with `wait_for_talk`.
  push: boolean;
  /// The MCP client's name for itself, such as `claude-code`.
  client: string | null;
}

/// The `talk` event's wire shape (`Talk::payload`), sent beside `phase`.
export interface TalkPayload {
  enabled: boolean;
  speak: boolean;
  /// Who will hear a question; talking is refused while nobody is.
  listener: TalkListener | null;
  chord: string;
  /// Why the hold-to-talk key could not be registered, if it could not.
  hotkey_error: string | null;
  listening: boolean;
  /// Talking is paused while recording or while a call waits to be
  /// recorded: the microphone belongs to the meeting then.
  paused: "recording" | "armed" | null;
  notice: string | null;
  turns: TalkTurn[];
}

/// Where talking is right now, as one word the card is drawn from.
export type TalkState =
  | "off"
  | "listening"
  | "paused"
  | "thinking"
  | "speaking"
  | "no_agent"
  | "error"
  | "idle";

const WORKING: ReadonlySet<TalkTurn["state"]> = new Set(["hearing", "sending", "waiting"]);
const FAILED: ReadonlySet<TalkTurn["state"]> = new Set(["failed", "no_reply"]);

export function talkState(payload: TalkPayload): TalkState {
  if (!payload.enabled) return "off";
  if (payload.listening) return "listening";
  if (payload.paused !== null) return "paused";
  const last = payload.turns.at(-1);
  if (last?.state === "speaking") return "speaking";
  if (last !== undefined && WORKING.has(last.state)) return "thinking";
  if (payload.listener === null) return "no_agent";
  if (last !== undefined && FAILED.has(last.state)) return "error";
  return "idle";
}

/// The hold button's label while paused: short enough for the 260 px
/// sidebar beside the chord.
export const PAUSED: Record<"recording" | "armed", string> = {
  recording: "Paused: recording",
  armed: "Paused: call waiting",
};

/// The reply as the window shows it: plain text, no markdown symbols.
export function replyText(turn: TalkTurn): string | null {
  return turn.reply_text ?? turn.reply;
}

/// What the user said, or where the worker is with it.
export function yourSide(turn: TalkTurn): string {
  if (turn.you !== null) return turn.you;
  if (turn.state === "hearing") return "Working out what you said…";
  if (turn.state === "nothing") return "Didn't catch that.";
  return "…";
}

/// The agent's reply, or why there is none yet.
export function theirSide(turn: TalkTurn): string | null {
  const reply = replyText(turn);
  if (reply !== null) return reply;
  switch (turn.state) {
    case "sending":
      return "Sending…";
    case "waiting":
      return "Thinking…";
    default:
      return null;
  }
}

function firstLine(text: string): string {
  return text.split("\n").find((l) => l.trim()) ?? "";
}

/// The card's one status line, which screen readers hear as it changes.
export function talkLine(payload: TalkPayload): string | null {
  if (payload.notice !== null) return payload.notice;
  const state = talkState(payload);
  if (state === "listening") return "Listening… let go to send.";
  if (payload.hotkey_error !== null)
    return `${payload.chord} is unavailable (${payload.hotkey_error}); use this button.`;
  const last = payload.turns.at(-1);
  if (state === "speaking") return "Speaking the reply. Hold to interrupt.";
  if (state === "thinking") return last?.state === "hearing" ? "Hearing you…" : "Thinking…";
  if (last === undefined) return null;
  if (state === "error" || last.state === "stopped" || last.state === "nothing")
    return last.note ?? yourSide(last);
  const reply = replyText(last);
  if (reply !== null) return `Reply: ${firstLine(reply)}`;
  return last.note;
}

/// Who is listening, in words: "Claude Code since 21:40".
export function listenerLine(listener: TalkListener | null): string {
  if (listener === null) return "No agent is listening.";
  const who = listener.client === "claude-code" ? "Claude Code" : (listener.client ?? "An agent");
  const at = new Date(listener.since);
  const since = Number.isNaN(at.getTime())
    ? ""
    : ` since ${at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
  const how = listener.push ? "" : ", waiting in a loop";
  return `Listening: ${who}${since}${how}.`;
}

/// How to start a Claude Code session that can hear questions. Channels are
/// a research preview; a server of one's own needs the development flag.
export const LAUNCH = "claude --dangerously-load-development-channels server:ambient";
export const PROMPT = "/mcp__ambient__talk";
