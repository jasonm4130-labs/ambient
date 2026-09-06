import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";
import { cn } from "@/lib/utils";

/// `session::Line` (`src/session.rs:1290`) as `transcript` serialises it.
/// `track` is `"room" | "call"` (`Track`'s `#[serde(rename_all = "lowercase")]`).
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

/// Groups by *consecutive* runs of the same speaker, in the order
/// `transcript` returned the lines (append order) — never a `groupBy` that
/// reorders. The fallback for an unnamed speaker is the track name, matching
/// `transcript_text` (`src/window.rs:552`), not "Unknown". Keyed on each
/// group's starting index rather than `start_ms`: the room and call tracks
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

/// `window.ambient.event("diarize", …)`'s payload.
interface DiarizeEvent {
  session: string;
  state: "done" | "failed";
  error: string | null;
}

interface TranscriptProps {
  session: string;
  /// The absolute append-order index (over `reply.lines`, not group-local)
  /// of a line to scroll into view and highlight for ~2 s — set by a
  /// `SearchPalette` hit. Optional: `Transcript.test.tsx` renders without it.
  highlightIndex?: number;
}

/// The transcript for one session, reloaded through `useLatest` whenever the
/// session or the Tidied/Verbatim choice changes — a later selection's reply
/// always wins over an earlier one still in flight.
///
/// Two divergences from the deleted native pane are correct and intentional:
/// the API never calls `session::dedup_bleed`, so this never re-implements
/// it, and the empty-transcript note here is short because the API does not
/// expose the `Detail` flags the native pane's note picked between.
export function Transcript({ session, highlightIndex }: TranscriptProps) {
  const bridge = useBridge();
  const [verbatim, setVerbatim] = useState(false);
  const [reply, setReply] = useState<TranscriptReply>();
  const [error, setError] = useState<string>();
  const [separating, setSeparating] = useState(false);
  const [flashIndex, setFlashIndex] = useState<number>();
  const lineRefs = useRef(new Map<number, HTMLDivElement>());

  const loadTranscript = useLatest(
    useCallback(
      (s: string, v: boolean) => bridge.call<TranscriptReply>("transcript", { session: s, verbatim: v }),
      [bridge],
    ),
  );

  useEffect(() => {
    setError(undefined);
    loadTranscript(session, verbatim)
      .then((next) => {
        if (next !== undefined) setReply(next);
      })
      .catch((e: unknown) => {
        setError(e instanceof Error ? e.message : String(e));
      });
  }, [session, verbatim, loadTranscript]);

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

  const separateVoices = useCallback(() => {
    setSeparating(true);
    void bridge.call("diarize.start", { session }).catch((e: unknown) => {
      setSeparating(false);
      setError(e instanceof Error ? e.message : String(e));
    });
  }, [bridge, session]);

  // Ignores an event for a session other than the one on screen — the bridge
  // runs one diarize job at a time, but the selection can move on while it
  // is still running.
  useEffect(
    () =>
      bridge.on("diarize", (payload) => {
        const event = payload as DiarizeEvent;
        if (event.session !== session) return;
        setSeparating(false);
        if (event.state === "failed") {
          setError(event.error ?? "separating voices failed");
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

  // Scrolls to and flashes `highlightIndex` for ~2 s. `scrollIntoView` is
  // undefined in jsdom, so it is called only if present.
  useEffect(() => {
    if (highlightIndex === undefined) return;
    const el = lineRefs.current.get(highlightIndex);
    el?.scrollIntoView?.({ block: "center" });
    setFlashIndex(highlightIndex);
    const timer = setTimeout(() => setFlashIndex(undefined), 2000);
    return () => clearTimeout(timer);
    // Also re-runs when `reply` changes: `highlightIndex` and `selected`
    // land in the same state batch, so the first run after a hit fires
    // against the *previous* session's still-mounted lines, before the new
    // session's `transcript` reply has arrived.
  }, [highlightIndex, reply]);

  const groups = reply === undefined ? [] : groupLines(reply.lines);

  return (
    <div className="flex flex-1 flex-col gap-3 overflow-y-auto p-6" data-testid="transcript">
      <div className="flex items-center justify-between gap-4">
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
        <div className="flex gap-2">
          <Button type="button" variant="outline" size="sm" data-testid="copy-markdown" onClick={copyMarkdown}>
            Copy Markdown
          </Button>
          <Button type="button" variant="outline" size="sm" data-testid="reveal-button" onClick={reveal}>
            Reveal in Finder
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="separate-voices"
            disabled={separating}
            onClick={separateVoices}
          >
            {separating ? "Separating voices…" : "Separate voices"}
          </Button>
        </div>
      </div>
      {error !== undefined && (
        <p role="alert" data-testid="transcript-warning" className="text-destructive text-sm">
          {error}
        </p>
      )}
      {reply !== undefined && groups.length === 0 ? (
        <p className="text-muted-foreground text-sm" data-testid="transcript-empty">
          No transcript yet — this session is {reply.state}.
        </p>
      ) : (
        <div className="flex flex-col gap-4" data-testid="transcript-lines">
          {groups.map((group) => (
            <div key={group.key}>
              <div className="text-muted-foreground flex items-baseline gap-2 text-xs font-semibold">
                <span>{gutter(group.startMs)}</span>
                <span>{group.speaker}</span>
              </div>
              {group.lines.map((line, i) => {
                // `group.key` is the absolute index of `group.lines[0]`
                // (`groupLines` only ever appends a consecutive run), so
                // `group.key + i` is this line's absolute index — the same
                // number `search`'s `Hit.index` names.
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
