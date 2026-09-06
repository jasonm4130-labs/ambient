import { useEffect, useMemo, useState } from "react";
import { Input } from "@/components/ui/input";
import { useBridge } from "@/lib/bridge-context";
import type { SessionSummary } from "./SessionList";

/// The wire shape of one `search` result (`session::search`, restated by
/// `api::search`): `index` is the absolute append-order index into that
/// session's transcript, not an offset within the group it renders under.
interface Hit {
  session: string;
  index: number;
  track: "room" | "call";
  start_ms: number;
  speaker: string | null;
  text: string;
}

interface SearchPaletteProps {
  sessions: SessionSummary[];
  onOpen: (session: string, index: number) => void;
  onClose: () => void;
}

function mmss(ms: number): string {
  const secs = Math.floor(ms / 1000);
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
}

/// Groups hits by session, preserving the order `search` returned them in —
/// the same "never reorder" rule `Transcript`'s `groupLines` follows.
function groupHits(hits: Hit[]): [string, Hit[]][] {
  const bySession = new Map<string, Hit[]>();
  for (const hit of hits) {
    const list = bySession.get(hit.session) ?? [];
    list.push(hit);
    bySession.set(hit.session, list);
  }
  return Array.from(bySession.entries());
}

/// A ⌘K command palette over the `search` API. Mounted only while open (its
/// caller renders it conditionally), so there is no `open` prop to reconcile
/// against — unmounting resets every bit of local state for free. Runs
/// `search {query, limit: 50}` 150 ms after the query goes quiet; `Enter`
/// opens the first hit, `Esc` closes.
export function SearchPalette({ sessions, onOpen, onClose }: SearchPaletteProps) {
  const bridge = useBridge();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Hit[]>();

  useEffect(() => {
    if (query === "") {
      setHits(undefined);
      return;
    }
    const timer = setTimeout(() => {
      bridge.call<Hit[]>("search", { query, limit: 50 }).then(setHits);
    }, 150);
    return () => clearTimeout(timer);
  }, [query, bridge]);

  const names = useMemo(() => {
    const map = new Map<string, string>();
    for (const s of sessions) map.set(s.id, s.name ?? s.id);
    return map;
  }, [sessions]);

  const groups = useMemo(() => (hits === undefined ? [] : groupHits(hits)), [hits]);
  const firstHit = groups[0]?.[1][0];

  return (
    <div
      role="presentation"
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-24"
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          onClose();
        } else if (e.key === "Enter" && firstHit !== undefined) {
          onOpen(firstHit.session, firstHit.index);
        }
      }}
    >
      <div className="bg-background w-full max-w-lg rounded-lg border p-4 shadow-lg">
        <Input
          autoFocus
          type="search"
          data-testid="search-input"
          aria-label="Search"
          placeholder="Search transcripts…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="mt-3 max-h-96 overflow-y-auto">
          {hits !== undefined && hits.length === 0 && (
            <p className="text-muted-foreground p-2 text-sm">Nothing matches</p>
          )}
          {groups.map(([session, list]) => (
            <div key={session} className="mb-2">
              <p className="text-muted-foreground px-2 text-xs font-semibold">
                {names.get(session) ?? session}
              </p>
              {list.map((hit) => (
                <button
                  type="button"
                  key={hit.index}
                  className="hover:bg-accent flex w-full items-baseline gap-2 rounded px-2 py-1 text-left text-sm"
                  onClick={() => onOpen(hit.session, hit.index)}
                >
                  <span className="text-muted-foreground text-xs">{mmss(hit.start_ms)}</span>
                  <span>{hit.text}</span>
                </button>
              ))}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
