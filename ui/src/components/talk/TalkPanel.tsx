import { useState } from "react";
import { Switch } from "@/components/ui/switch";
import { useBridge } from "@/lib/bridge-context";
import { cn } from "@/lib/utils";
import {
  listenerLine,
  replyText,
  theirSide,
  yourSide,
  type TalkPayload,
  type TalkTurn,
} from "./text";

/// One turn: what the user said on the right, the reply on the left as plain
/// text, and any note under them.
function TurnItem({ turn }: { turn: TalkTurn }) {
  const theirs = theirSide(turn);
  const pending = replyText(turn) === null;
  return (
    <li className="flex flex-col gap-2" data-testid="talk-turn">
      <div className="bg-primary text-primary-foreground max-w-[75%] self-end rounded-lg px-3 py-2 text-sm whitespace-pre-wrap">
        <span className="sr-only">You said: </span>
        {yourSide(turn)}
      </div>
      {theirs !== null && (
        <div
          className={cn(
            "bg-muted max-w-[75%] self-start rounded-lg px-3 py-2 text-sm break-words whitespace-pre-wrap",
            pending && "text-muted-foreground italic",
          )}
          aria-busy={pending}
          data-testid="talk-reply"
        >
          <span className="sr-only">Reply: </span>
          {turn.state === "speaking" && (
            <span role="img" aria-label="Being read aloud" className="mr-1">
              🔊
            </span>
          )}
          {theirs}
        </div>
      )}
      {turn.note !== null && (
        <p
          className={cn(
            "text-muted-foreground text-xs",
            (turn.state === "failed" || turn.state === "no_reply") && "text-destructive",
          )}
          data-testid="talk-note"
        >
          {turn.note}
        </p>
      )}
    </li>
  );
}

/// The speak-replies switch, which sets `talk.speak`.
function SpeakSwitch({ speak }: { speak: boolean }) {
  const bridge = useBridge();
  const [error, setError] = useState<string>();
  return (
    <div className="flex shrink-0 flex-col items-end gap-1">
      <label className="flex items-center gap-2 text-sm">
        Speak replies
        <Switch
          aria-label="Speak replies"
          data-testid="talk-speak"
          checked={speak}
          onCheckedChange={(value) => {
            void bridge
              .call("config.set", { key: "talk.speak", value: value ? "true" : "false" })
              .then(() => setError(undefined))
              .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
          }}
        />
      </label>
      {error !== undefined && (
        <p role="alert" className="text-destructive text-xs">
          {error}
        </p>
      )}
    </div>
  );
}

/// The main pane's conversation: every turn this run, both sides as text,
/// newest last. Nothing here is saved; the agent's own session keeps the
/// record.
export function TalkPanel({ payload }: { payload: TalkPayload | undefined }) {
  const turns = payload?.turns ?? [];
  return (
    <div className="flex min-h-0 flex-1 flex-col" data-testid="talk-panel">
      <header className="flex items-center justify-between gap-4 border-b p-4">
        <div className="min-w-0">
          <h2 className="text-lg font-semibold">Talking to your agent</h2>
          <p className="text-muted-foreground text-xs">
            Hold {payload?.chord ?? "⌥Space"} anywhere and speak. Your words are transcribed on this
            Mac and sent as text to the agent session that is listening; the audio is never kept.
          </p>
          {payload !== undefined && (
            <p className="text-muted-foreground text-xs" data-testid="talk-listener">
              {listenerLine(payload.listener)}
            </p>
          )}
        </div>
        {payload !== undefined && <SpeakSwitch speak={payload.speak} />}
      </header>
      <ol className="flex flex-1 flex-col gap-4 overflow-y-auto p-4" aria-label="Conversation">
        {turns.length === 0 && (
          <li className="text-muted-foreground text-sm" data-testid="talk-empty">
            Nothing said yet.
          </li>
        )}
        {turns.map((turn) => (
          <TurnItem key={turn.id} turn={turn} />
        ))}
      </ol>
    </div>
  );
}
