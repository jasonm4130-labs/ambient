import { useCallback, useEffect, useRef, useState } from "react";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";

interface TranscriptLine {
  track: "room" | "call";
  start_ms: number;
  end_ms: number;
  speaker: string | null;
  text: string;
}

interface TranscriptReply {
  session: string;
  state: string;
  next: number;
  lines: TranscriptLine[];
}

interface PollState {
  state: string | undefined;
  lines: TranscriptLine[];
  error: string | undefined;
}

/// `reply.next` is the absolute line count, threaded straight back as the next `since`.
function usePollingTranscript(session: string, onDone: () => void): PollState {
  const bridge = useBridge();
  const [state, setState] = useState<string>();
  const [lines, setLines] = useState<TranscriptLine[]>([]);
  const [error, setError] = useState<string>();

  const poll = useLatest(
    useCallback(
      (s: string, since: number) =>
        bridge.call<TranscriptReply>("transcript", { session: s, since }),
      [bridge],
    ),
  );

  const onDoneRef = useRef(onDone);
  useEffect(() => {
    onDoneRef.current = onDone;
  }, [onDone]);

  useEffect(() => {
    let since = 0;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let done = false;

    setState(undefined);
    setLines([]);
    setError(undefined);

    const tick = () => {
      poll(session, since)
        .then((reply) => {
          if (reply === undefined || cancelled) return;
          setError(undefined);
          since = reply.next;
          setState(reply.state);
          setLines((prev) => [...prev, ...reply.lines]);
          if (reply.state === "done") {
            if (!done) {
              done = true;
              onDoneRef.current();
            }
            return;
          }
          timer = setTimeout(tick, 2000);
        })
        .catch((e: unknown) => {
          if (cancelled) return;
          setError(e instanceof Error ? e.message : String(e));
          timer = setTimeout(tick, 2000);
        });
    };

    tick();

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [session, poll]);

  return { state, lines, error };
}

interface LiveTranscriptProps {
  session: string;
  onDone: () => void;
}

export function LiveTranscript({ session, onDone }: LiveTranscriptProps) {
  const { state, lines, error } = usePollingTranscript(session, onDone);

  return (
    <div className="flex flex-1 flex-col gap-3 overflow-y-auto p-6" data-testid="live-transcript">
      {error !== undefined && (
        <p role="alert" data-testid="live-transcript-warning" className="text-destructive text-sm">
          {error}
        </p>
      )}
      {lines.length === 0 ? (
        state !== undefined && (
          <p className="text-muted-foreground text-sm" data-testid="live-transcript-empty">
            {state === "transcribing" ? "Transcribing…" : "Listening…"}
          </p>
        )
      ) : (
        <div className="flex flex-col gap-2" data-testid="live-transcript-lines">
          {lines.map((line, i) => (
            <p key={i} className="text-sm">
              {line.text}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}
