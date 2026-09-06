import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { SidebarFilters } from "./SidebarFilters";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

/// The wire shape of one `sessions` entry (`session::SessionSummary`,
/// `src/session.rs:975`), which derives plain `Serialize`: there is no
/// `state` field. [`sessionState`] below reproduces
/// `SessionSummary::state()`'s precedence in TypeScript instead of trusting
/// the reply to carry one.
export interface SessionSummary {
  id: string;
  dir: string;
  name: string | null;
  started_at: string | null;
  duration_s: number | null;
  transcribed: boolean;
  live: boolean;
  transcribing: boolean;
  error: string | null;
  tags: string[];
  notes?: string;
  warnings?: string[];
  audio_available?: boolean;
  pinned: boolean;
}

/// `SessionSummary::state()` (`src/session.rs:994`), reproduced exactly: a
/// capture in flight outranks everything else, and a session with an `error`
/// reads `broken` even if it also has a `transcript.md`.
export function sessionState(s: SessionSummary): string {
  if (s.live) return "live";
  if (s.transcribing) return "transcribing";
  if (s.error !== null) return "broken";
  if (s.transcribed) return "done";
  return "awaiting transcript";
}

function formatDuration(seconds: number | null): string | null {
  if (seconds === null) return null;
  const total = Math.round(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

interface SessionListProps {
  sessions: SessionSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

interface Entry {
  key: string;
  offset: number;
  height: number;
  header?: string;
  session?: SessionSummary;
}

function entriesFor(sessions: SessionSummary[]): Entry[] {
  const entries: Entry[] = [];
  const months = new Set<string>();
  let offset = 0;
  for (const session of sessions) {
    const month = session.started_at?.slice(0, 7) ?? "";
    const group = session.pinned ? "Pinned" : month;
    if (!months.has(group)) {
      months.add(group);
      const date = /^\d{4}-\d{2}$/u.test(month) ? new Date(`${month}-01T12:00:00`) : null;
      const header = session.pinned
        ? "Pinned"
        : date !== null && !Number.isNaN(date.getTime())
          ? date.toLocaleDateString("en", { month: "long", year: "numeric" })
          : "Undated";
      entries.push({ key: `month-${group}`, header, offset, height: 28 });
      offset += 28;
    }
    entries.push({ key: session.id, session, offset, height: 56 });
    offset += 56;
  }
  return entries;
}

// Fixed geometry lets the viewport render only intersecting rows and twenty
// neighbours. Pinned sessions retain the order returned by the API.
export function SessionList({ sessions, selectedId, onSelect }: SessionListProps) {
  const [query, setQuery] = useState("");
  const [tag, setTag] = useState<string | null>(null);
  const [viewport, setViewport] = useState({ top: 0, height: 600 });
  const scroller = useRef<HTMLDivElement>(null);
  const rows = useRef(new Map<string, HTMLButtonElement>());
  const focusId = useRef<string | null>(null);
  const tags = useMemo(() => [...new Set(sessions.flatMap((s) => s.tags))].sort(), [sessions]);
  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return sessions.filter(
      (s) =>
        (tag === null || s.tags.includes(tag)) &&
        `${s.name ?? s.id} ${s.tags.join(" ")}`.toLocaleLowerCase().includes(needle),
    );
  }, [sessions, query, tag]);
  const entries = useMemo(() => entriesFor(filtered), [filtered]);
  const total = entries.at(-1) === undefined ? 0 : entries.at(-1)!.offset + entries.at(-1)!.height;
  const top = Math.min(viewport.top, Math.max(0, total - viewport.height));
  const first = Math.max(
    0,
    entries.findIndex((entry) => entry.offset + entry.height > top),
  );
  const start = Math.max(0, first - 10);
  let end = first;
  while (end < entries.length && entries[end]!.offset < top + viewport.height) end += 1;
  end = Math.min(entries.length, end + 10);
  const shown = entries.slice(start, end);
  const bottom = shown.at(-1);

  useEffect(() => {
    const el = scroller.current;
    if (el === null) return;
    const measure = () => setViewport({ top: el.scrollTop, height: el.clientHeight || 600 });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  useLayoutEffect(() => {
    if (focusId.current !== null && rows.current.has(focusId.current)) {
      rows.current.get(focusId.current)?.focus({ preventScroll: true });
      focusId.current = null;
    }
  });

  const move = (id: string, direction: number) => {
    const next = filtered[filtered.findIndex((s) => s.id === id) + direction];
    if (next === undefined) return;
    const entry = entries.find((item) => item.session?.id === next.id)!;
    const el = scroller.current;
    if (el !== null) {
      if (entry.offset < el.scrollTop) el.scrollTop = entry.offset;
      else if (entry.offset + 56 > el.scrollTop + viewport.height)
        el.scrollTop = entry.offset + 56 - viewport.height;
      setViewport({ top: el.scrollTop, height: viewport.height });
    }
    focusId.current = next.id;
    onSelect(next.id);
  };

  const resetScroll = () => {
    if (scroller.current !== null) scroller.current.scrollTop = 0;
    setViewport((current) => ({ ...current, top: 0 }));
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <SidebarFilters
        query={query}
        tag={tag}
        tags={tags}
        onQuery={(value) => {
          setQuery(value);
          resetScroll();
        }}
        onTag={(value) => {
          setTag(value);
          resetScroll();
        }}
      />
      <div
        ref={scroller}
        className="min-h-0 flex-1 overflow-y-auto px-2"
        data-testid="session-list-viewport"
        onScroll={(event) =>
          setViewport({
            top: event.currentTarget.scrollTop,
            height: event.currentTarget.clientHeight || 600,
          })
        }
      >
        {filtered.length === 0 && (
          <p className="text-muted-foreground p-2">
            {sessions.length === 0 ? "No sessions yet." : "No matching sessions."}
          </p>
        )}
        <ul data-testid="session-list">
          <li
            aria-hidden="true"
            data-testid="session-list-top-spacer"
            style={{ height: shown[0]?.offset ?? 0 }}
          />
          {shown.map((entry) => {
            const s = entry.session;
            if (s === undefined)
              return (
                <li
                  key={entry.key}
                  style={{ height: 28 }}
                  className="text-muted-foreground flex items-center px-3 text-xs font-semibold"
                >
                  {entry.header}
                </li>
              );
            return (
              <li key={entry.key}>
                <button
                  type="button"
                  ref={(el) => {
                    if (el === null) rows.current.delete(s.id);
                    else rows.current.set(s.id, el);
                  }}
                  data-testid="session-row"
                  data-session={s.id}
                  aria-current={s.id === selectedId}
                  style={{ height: 56 }}
                  className={cn(
                    "hover:bg-sidebar-accent flex w-full min-w-0 flex-col justify-center gap-1 overflow-hidden rounded-md px-3 text-left",
                    s.id === selectedId && "bg-sidebar-accent",
                  )}
                  onClick={() => onSelect(s.id)}
                  onKeyDown={(event) => {
                    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
                    event.preventDefault();
                    move(s.id, event.key === "ArrowDown" ? 1 : -1);
                  }}
                >
                  <span data-testid="session-row-name" className="w-full truncate font-medium">
                    {s.name ?? s.id}
                  </span>
                  <span className="text-muted-foreground flex w-full min-w-0 items-center gap-1 whitespace-nowrap text-xs">
                    <span>{formatDuration(s.duration_s)}</span>
                    <Badge variant="outline">{sessionState(s)}</Badge>
                    <span className="min-w-0 truncate">{s.tags.slice(0, 2).join(" · ")}</span>
                    {s.tags.length > 2 && <span className="shrink-0">+{s.tags.length - 2}</span>}
                  </span>
                </button>
              </li>
            );
          })}
          <li
            aria-hidden="true"
            style={{
              height: Math.max(
                0,
                total - (bottom === undefined ? 0 : bottom.offset + bottom.height),
              ),
            }}
          />
        </ul>
      </div>
    </div>
  );
}
