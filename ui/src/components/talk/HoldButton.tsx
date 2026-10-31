import { useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import { PAUSED, talkState, type TalkPayload } from "./text";

const isKey = (e: KeyboardEvent) => e.key === " " || e.key === "Enter";

/// Press and release, each sent once however many pointer and key events
/// arrive: a second press while held, or a release with nothing held (the
/// pointer left the button), is ignored here and in Rust alike.
function useHold() {
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
  return { down, up, error };
}

/// The pointer and key events of a hold. The pointer is captured so a drag
/// off the button still releases; losing focus releases too, so a hold never
/// sticks.
function holdEvents(down: () => void, up: () => void) {
  return {
    onPointerDown: (e: PointerEvent<HTMLButtonElement>) => {
      e.currentTarget.setPointerCapture?.(e.pointerId);
      down();
    },
    onPointerUp: up,
    onPointerCancel: up,
    onLostPointerCapture: up,
    onKeyDown: (e: KeyboardEvent) => {
      if (!isKey(e) || e.repeat) return;
      e.preventDefault();
      down();
    },
    onKeyUp: (e: KeyboardEvent) => {
      if (!isKey(e)) return;
      e.preventDefault();
      up();
    },
    onBlur: up,
  };
}

/// What the button says, and whether it can be held. A hold while a reply is
/// read out interrupts it, so only paused and nobody-to-hear refuse; one
/// already listening stays enabled so its release still arrives.
function holdLabel(payload: TalkPayload): { label: string; disabled: boolean } {
  const paused = payload.paused === null ? null : PAUSED[payload.paused];
  const listening = talkState(payload) === "listening";
  return {
    label: paused ?? (listening ? "Let go to send" : "Hold to talk"),
    disabled: paused !== null || (payload.listener === null && !payload.listening),
  };
}

/// Hold to talk: down on press, up on release, by pointer or by Space or
/// Return. `describedBy` names the card's status line, so a screen reader
/// hears where talking is with the button.
export function HoldButton({
  payload,
  describedBy,
}: {
  payload: TalkPayload;
  describedBy?: string;
}) {
  const { down, up, error } = useHold();
  const { label, disabled } = holdLabel(payload);
  return (
    <>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant={payload.listening ? "destructive" : "secondary"}
          className="min-w-0 flex-1 touch-none select-none"
          data-testid="talk-hold"
          aria-pressed={payload.listening}
          aria-describedby={describedBy}
          aria-keyshortcuts="Alt+Space"
          disabled={disabled}
          {...holdEvents(down, up)}
        >
          <span aria-hidden="true">🎙</span>
          <span className="truncate" data-testid="talk-hold-label">
            {label}
          </span>
        </Button>
        <kbd
          aria-hidden="true"
          className="text-muted-foreground shrink-0 rounded border px-1 font-mono text-xs"
        >
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
