import { useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useBridge } from "@/lib/bridge-context";
import { cn } from "@/lib/utils";

/// One push-to-talk turn (`src/talk/app.rs`'s `Turn::payload`): both sides as
/// text, and where the turn has got to.
export interface TalkTurn {
  id: number;
  you: string | null;
  reply: string | null;
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

/// The `talk` event's wire shape (`Talk::payload`), sent beside `phase`.
export interface TalkPayload {
  enabled: boolean;
  speak: boolean;
  /// Whether a firstmate home is set; talking is refused without one.
  configured: boolean;
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

const PAUSED: Record<"recording" | "armed", string> = {
  recording: "Paused while recording",
  armed: "Paused while a call waits",
};

/// What the user said, or where the worker is with it.
function yourSide(turn: TalkTurn): string {
  if (turn.you !== null) return turn.you;
  if (turn.state === "hearing") return "Working out what you said…";
  if (turn.state === "nothing") return "Didn't catch that.";
  return "…";
}

/// firstmate's reply, or why there is none yet.
function theirSide(turn: TalkTurn): string | null {
  if (turn.reply !== null) return turn.reply;
  switch (turn.state) {
    case "sending":
      return "Sending…";
    case "waiting":
      return "Thinking…";
    default:
      return null;
  }
}

/// The card's one status line.
export function talkLine(payload: TalkPayload): string | null {
  if (payload.notice !== null) return payload.notice;
  if (payload.listening) return "Listening… let go to send.";
  if (payload.hotkey_error !== null)
    return `${payload.chord} is unavailable (${payload.hotkey_error}); use this button.`;
  const last = payload.turns.at(-1);
  if (last === undefined) return null;
  if (last.state === "speaking") return "firstmate is speaking. Hold again to stop it.";
  if (last.reply !== null)
    return `firstmate: ${last.reply.split("\n").find((l) => l.trim()) ?? ""}`;
  return theirSide(last) ?? last.note ?? yourSide(last);
}

const isKey = (e: KeyboardEvent) => e.key === " " || e.key === "Enter";

/// Hold to talk: down on press, up on release, whether by pointer or by
/// keyboard. A release that arrives without a press (the pointer left the
/// button) is ignored here and in Rust alike.
function HoldButton({ payload }: { payload: TalkPayload }) {
  const bridge = useBridge();
  const held = useRef(false);
  const [error, setError] = useState<string>();

  const send = (method: "talk.press" | "talk.release") => {
    void bridge
      .call<{ sent: boolean } | undefined>(method)
      .then((reply) => {
        if (reply?.sent === false) throw new Error("Ambient did not answer. Try the menu bar.");
        setError(undefined);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  };
  const down = () => {
    if (held.current) return;
    held.current = true;
    send("talk.press");
  };
  const up = () => {
    if (!held.current) return;
    held.current = false;
    send("talk.release");
  };
  const paused = payload.paused === null ? null : PAUSED[payload.paused];
  const disabled = paused !== null || !payload.configured;
  return (
    <>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant={payload.listening ? "destructive" : "secondary"}
          className="flex-1 touch-none select-none"
          data-testid="talk-hold"
          aria-pressed={payload.listening}
          disabled={disabled}
          onPointerDown={(e) => {
            e.currentTarget.setPointerCapture?.(e.pointerId);
            down();
          }}
          onPointerUp={up}
          onPointerCancel={up}
          onLostPointerCapture={up}
          onKeyDown={(e) => {
            if (!isKey(e) || e.repeat) return;
            e.preventDefault();
            down();
          }}
          onKeyUp={(e) => {
            if (!isKey(e)) return;
            e.preventDefault();
            up();
          }}
          onBlur={up}
        >
          <span aria-hidden="true">🎙</span>
          {paused ?? (payload.listening ? "Listening… let go to send" : "Hold to talk")}
        </Button>
        <kbd className="text-muted-foreground rounded border px-1 font-mono text-xs">
          {payload.chord}
        </kbd>
      </div>
      {error !== undefined && (
        <p role="alert" className="text-destructive mt-2 text-xs">
          {error}
        </p>
      )}
    </>
  );
}

/// The sidebar's talk card: hold to talk to firstmate, and the latest word
/// either way. Off, it says how to turn talking mode on.
export function TalkCard({
  payload,
  onOpen,
  onSettings,
}: {
  payload: TalkPayload | undefined;
  onOpen: () => void;
  onSettings: () => void;
}) {
  if (payload === undefined) return null;
  if (!payload.enabled) {
    return (
      <p
        className="border-sidebar-border text-muted-foreground border-b px-3 py-2 text-xs"
        data-testid="talk-off"
      >
        Talking to firstmate is off ·{" "}
        <button
          type="button"
          className="hover:text-foreground underline"
          data-testid="talk-turn-on"
          onClick={onSettings}
        >
          Turn on
        </button>
      </p>
    );
  }
  const line = talkLine(payload);
  return (
    <div className="border-sidebar-border border-b p-3" data-testid="talk-card">
      <HoldButton payload={payload} />
      {!payload.configured && (
        <p className="text-muted-foreground mt-2 text-xs" data-testid="talk-unset">
          Talking has nowhere to go yet ·{" "}
          <button type="button" className="hover:text-foreground underline" onClick={onSettings}>
            Choose firstmate's home
          </button>
        </p>
      )}
      {line !== null && (
        <p className="text-muted-foreground mt-2 line-clamp-2 text-xs" data-testid="talk-line">
          {line}
        </p>
      )}
      <button
        type="button"
        className="text-muted-foreground hover:text-foreground mt-1 text-xs underline-offset-2 hover:underline"
        data-testid="talk-open"
        onClick={onOpen}
      >
        Conversation{payload.turns.length > 0 ? ` (${payload.turns.length})` : ""}
      </button>
    </div>
  );
}

/// The main pane's conversation: every turn this run, both sides as text,
/// newest last. Nothing here is saved; firstmate's inbox keeps the record.
export function TalkPanel({ payload }: { payload: TalkPayload | undefined }) {
  const bridge = useBridge();
  const [error, setError] = useState<string>();
  const turns = payload?.turns ?? [];
  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="talk-panel">
      <header className="flex items-center justify-between gap-4 border-b p-4">
        <div>
          <h2 className="text-lg font-semibold">Talking to firstmate</h2>
          <p className="text-muted-foreground text-xs">
            Hold {payload?.chord ?? "⌥Space"} anywhere and speak. Your words are transcribed on this
            Mac and sent as text; the audio is never kept.
          </p>
        </div>
        {payload !== undefined && (
          <label className="flex shrink-0 items-center gap-2 text-sm">
            Speak replies
            <Switch
              aria-label="Speak firstmate's replies"
              data-testid="talk-speak"
              checked={payload.speak}
              onCheckedChange={(value) => {
                void bridge
                  .call("config.set", { key: "talk.speak", value: value ? "true" : "false" })
                  .then(() => setError(undefined))
                  .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
              }}
            />
          </label>
        )}
      </header>
      {error !== undefined && (
        <p role="alert" className="text-destructive p-4 text-sm">
          {error}
        </p>
      )}
      <ol className="flex flex-1 flex-col gap-4 overflow-y-auto p-4" aria-label="Conversation">
        {turns.length === 0 && (
          <li className="text-muted-foreground text-sm" data-testid="talk-empty">
            Nothing said yet.
          </li>
        )}
        {turns.map((turn) => {
          const theirs = theirSide(turn);
          return (
            <li key={turn.id} className="flex flex-col gap-2" data-testid="talk-turn">
              <div className="bg-primary text-primary-foreground max-w-[75%] self-end rounded-lg px-3 py-2 text-sm whitespace-pre-wrap">
                <span className="sr-only">You: </span>
                {yourSide(turn)}
              </div>
              {theirs !== null && (
                <div
                  className={cn(
                    "bg-muted max-w-[75%] self-start rounded-lg px-3 py-2 text-sm whitespace-pre-wrap",
                    turn.reply === null && "text-muted-foreground italic",
                  )}
                  data-testid="talk-reply"
                >
                  <span className="sr-only">firstmate: </span>
                  {turn.state === "speaking" && <span aria-hidden="true">🔊 </span>}
                  {theirs}
                </div>
              )}
              {turn.note !== null && (
                <p
                  className={cn(
                    "text-muted-foreground text-xs",
                    turn.state === "failed" && "text-destructive",
                  )}
                  data-testid="talk-note"
                >
                  {turn.note}
                </p>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}
