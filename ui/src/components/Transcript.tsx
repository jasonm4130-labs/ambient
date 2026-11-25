import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";
import { cn } from "@/lib/utils";

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

interface ExportReply {
  session: string;
  format: string;
  text: string;
}

interface Group {
  key: number;
  speaker: string;
  startMs: number;
  lines: TranscriptLine[];
}

/// Keyed on each group's start index, not `start_ms`: the room and call tracks
/// have independent clocks and can repeat a timestamp.
function groupLines(lines: TranscriptLine[]): Group[] {
  const groups: Group[] = [];
  lines.forEach((line, i) => {
    const speaker = line.speaker ?? line.track;
    const last = groups.at(-1);
    if (last !== undefined && last.speaker === speaker) {
      last.lines.push(line);
    } else {
      groups.push({ key: i, speaker, startMs: line.start_ms, lines: [line] });
    }
  });
  return groups;
}

function gutter(ms: number): string {
  const secs = Math.floor(ms / 1000);
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `[${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}]`;
}

interface DiarizeEvent {
  session: string;
  state: "done" | "failed";
  error: string | null;
}

interface TranscriptProps {
  session: string;
  unavailable?: string | undefined;
  revision?: number;
  /// Absolute index into `reply.lines`, not group-local.
  highlightIndex?: number;
}

export function Transcript({ session, revision, highlightIndex, unavailable }: TranscriptProps) {
  const bridge = useBridge();
  const [verbatim, setVerbatim] = useState(false);
  const [reply, setReply] = useState<TranscriptReply>();
  const [error, setError] = useState<string>();
  const [separating, setSeparating] = useState(false);
  const [flashIndex, setFlashIndex] = useState<number>();
  const lineRefs = useRef(new Map<number, HTMLDivElement>());

  const loadTranscript = useLatest(
    useCallback(
      (s: string, v: boolean) =>
        bridge.call<TranscriptReply>("transcript", { session: s, verbatim: v }),
      [bridge],
    ),
  );

  useEffect(() => {
    setError(undefined);
    setReply(undefined);
    loadTranscript(session, verbatim)
      .then((next) => {
        if (next !== undefined) setReply(next);
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
      });
  }, [session, verbatim, revision, loadTranscript]);

  const copyMarkdown = useCallback(() => {
    void bridge
      .call<ExportReply>("export", { session, format: "markdown" })
      .then((exported) => bridge.call("clipboard.write", { text: exported.text }))
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
      });
  }, [bridge, session]);

  const reveal = useCallback(() => {
    void bridge.call("reveal", { session }).catch((e: unknown) => {
      setError(e instanceof Error ? e.message : String(e));
    });
  }, [bridge, session]);

  // A queued session belongs to the queue; a second job beside it would collide.
  const waiting =
    reply !== undefined && reply.state !== "done" && reply.state !== "failed"
      ? "This session is waiting its turn to be transcribed."
      : undefined;
  const blocked = unavailable ?? waiting;

  const separateVoices = useCallback(() => {
    setSeparating(true);
    void bridge.call("diarize.start", { session }).catch((e: unknown) => {
      setSeparating(false);
      setError(e instanceof Error ? e.message : String(e));
    });
  }, [bridge, session]);

  useEffect(
    () =>
      bridge.on("command", (payload) => {
        const { action } = payload as { action: string };
        if (action === "copy-markdown") copyMarkdown();
        else if (action === "reveal") reveal();
      }),
    [bridge, copyMarkdown, reveal],
  );

  // The selection can move on while a diarize job for another session runs.
  useEffect(
    () =>
      bridge.on("diarize", (payload) => {
        const event = payload as DiarizeEvent;
        if (event.session !== session) return;
        setSeparating(false);
        if (event.state === "failed") {
          setError(event.error ?? "transcription failed");
        } else {
          loadTranscript(session, verbatim)
            .then((next) => {
              if (next !== undefined) setReply(next);
            })
            .catch((e: unknown) => {
              setError(e instanceof Error ? e.message : String(e));
            });
        }
      }),
    [bridge, session, verbatim, loadTranscript],
  );

  // `scrollIntoView` is undefined in jsdom.
  useEffect(() => {
    if (highlightIndex === undefined) return;
    const el = lineRefs.current.get(highlightIndex);
    el?.scrollIntoView?.({ block: "center" });
    setFlashIndex(highlightIndex);
    const timer = setTimeout(() => setFlashIndex(undefined), 2000);
    return () => clearTimeout(timer);
    // Also keyed on `reply`: `highlightIndex` lands in the same batch as the new
    // selection, so the first run hits the previous session's lines.
  }, [highlightIndex, reply]);

  const groups = reply === undefined ? [] : groupLines(reply.lines);

  return (
    <div className="flex flex-1 flex-col gap-3 overflow-y-auto p-6" data-testid="transcript">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div role="group" aria-label="Transcript detail" className="inline-flex rounded-md border">
          <button
            type="button"
            data-testid="toggle-tidied"
            aria-pressed={!verbatim}
            className={cn("rounded-l-md px-3 py-1 text-sm", !verbatim && "bg-accent")}
            onClick={() => setVerbatim(false)}
          >
            Tidied
          </button>
          <button
            type="button"
            data-testid="toggle-verbatim"
            aria-pressed={verbatim}
            className={cn("rounded-r-md px-3 py-1 text-sm", verbatim && "bg-accent")}
            onClick={() => setVerbatim(true)}
          >
            Verbatim
          </button>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="copy-markdown"
            onClick={copyMarkdown}
          >
            Copy Markdown
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="reveal-button"
            onClick={reveal}
          >
            Reveal in Finder
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="separate-voices"
            disabled={separating || blocked !== undefined}
            title={blocked}
            onClick={separateVoices}
          >
            {separating ? "Transcribing…" : reply?.state === "done" ? "Transcribe again" : "Retry"}
          </Button>
        </div>
      </div>
      {blocked !== undefined && <p className="text-muted-foreground text-xs">{blocked}</p>}
      {error !== undefined && (
        <p role="alert" data-testid="transcript-warning" className="text-destructive text-sm">
          {error}
        </p>
      )}
      {reply === undefined && error === undefined && (
        <div
          role="status"
          aria-label="Loading transcript"
          className="flex max-w-[68ch] animate-pulse flex-col gap-3"
        >
          {[0, 1, 2].map((line) => (
            <div key={line} className="bg-muted h-4 rounded" />
          ))}
        </div>
      )}
      {reply !== undefined && groups.length === 0 ? (
        <p className="text-muted-foreground text-sm" data-testid="transcript-empty">
          {reply.state === "failed"
            ? "Transcription failed. Retry runs it again from the last finished step."
            : `No transcript yet — this session is ${reply.state}.`}
        </p>
      ) : (
        <div className="flex max-w-[68ch] flex-col gap-4" data-testid="transcript-lines">
          {groups.map((group) => (
            <div key={group.key}>
              <div className="text-muted-foreground flex items-baseline gap-2 text-xs font-semibold">
                <span>{gutter(group.startMs)}</span>
                <span>{group.speaker}</span>
              </div>
              {group.lines.map((line, i) => {
                // `group.key` is the absolute index of the group's first line.
                const absoluteIndex = group.key + i;
                return (
                  <div
                    key={i}
                    ref={(el) => {
                      if (el === null) lineRefs.current.delete(absoluteIndex);
                      else lineRefs.current.set(absoluteIndex, el);
                    }}
                    className={cn(
                      "rounded px-1 transition-colors",
                      flashIndex === absoluteIndex && "bg-yellow-200 dark:bg-yellow-900",
                    )}
                  >
                    <p className="text-sm">{line.text}</p>
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
