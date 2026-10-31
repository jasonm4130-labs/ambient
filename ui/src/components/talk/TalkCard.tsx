import { useId } from "react";
import { cn } from "@/lib/utils";
import { HoldButton } from "./HoldButton";
import { talkLine, talkState, type TalkPayload, type TalkState } from "./text";

/// The dot beside the status line: one colour per state, and a pulse while
/// something is happening (none under reduced motion, by the global rule).
const DOT: Record<TalkState, string> = {
  off: "bg-muted-foreground",
  idle: "bg-muted-foreground",
  no_agent: "bg-muted-foreground",
  paused: "bg-muted-foreground",
  listening: "bg-destructive animate-pulse",
  thinking: "bg-amber-500 animate-pulse",
  speaking: "bg-primary animate-pulse",
  error: "bg-destructive",
};

/// A one-line prompt with a link-styled action after it.
function Prompt({
  text,
  action,
  onClick,
  testId,
  className,
}: {
  text: string;
  action: string;
  onClick: () => void;
  testId: string;
  className: string;
}) {
  return (
    <p className={cn("text-muted-foreground text-xs", className)} data-testid={testId}>
      {text} ·{" "}
      <button type="button" className="hover:text-foreground underline" onClick={onClick}>
        {action}
      </button>
    </p>
  );
}

/// The live region a screen reader hears as talking moves on. Always in the
/// page, even empty, so its first message is announced.
function StatusLine({ id, payload }: { id: string; payload: TalkPayload }) {
  const state = talkState(payload);
  const line = talkLine(payload);
  return (
    <p
      id={id}
      role="status"
      className={cn(
        "text-muted-foreground mt-2 flex min-w-0 items-start gap-1.5 text-xs",
        state === "error" && "text-destructive",
      )}
      data-testid="talk-line"
    >
      {line !== null && (
        <>
          <span
            aria-hidden="true"
            className={cn("mt-1 size-1.5 shrink-0 rounded-full", DOT[state])}
          />
          <span className="line-clamp-2 min-w-0 break-words">{line}</span>
        </>
      )}
    </p>
  );
}

/// The sidebar's talk card: hold to talk to the agent session that is
/// listening, and the latest word either way. Off, it says how to turn
/// talking mode on; with nobody listening, where to find out how to start an
/// agent that will.
export function TalkCard({
  payload,
  onOpen,
  onSettings,
}: {
  payload: TalkPayload | undefined;
  onOpen: () => void;
  onSettings: () => void;
}) {
  const statusId = useId();
  if (payload === undefined) return null;
  if (!payload.enabled)
    return (
      <Prompt
        text="Talking is off"
        action="Turn on"
        onClick={onSettings}
        testId="talk-off"
        className="border-sidebar-border border-b px-3 py-2"
      />
    );
  return (
    <div
      className="border-sidebar-border border-b p-3"
      data-testid="talk-card"
      data-state={talkState(payload)}
    >
      <HoldButton payload={payload} describedBy={statusId} />
      <StatusLine id={statusId} payload={payload} />
      {payload.listener === null && (
        <Prompt
          text="No agent is listening"
          action="How"
          onClick={onSettings}
          testId="talk-no-agent"
          className="mt-1"
        />
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
