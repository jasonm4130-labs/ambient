import { useCallback, useEffect, useRef, useState } from "react";
import { LivePane, type PhasePayload } from "@/components/LivePane";
import { LiveTranscript } from "@/components/LiveTranscript";
import { NamingStrip } from "@/components/NamingStrip";
import { SearchPalette } from "@/components/SearchPalette";
import { SessionHeader } from "@/components/SessionHeader";
import { SessionList, sessionState, type SessionSummary } from "@/components/SessionList";
import { Transcript } from "@/components/Transcript";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";

interface SessionsProps {
  onSettings: () => void;
}

/// The sidebar-plus-content screen: sessions on the left, the selected
/// session's transcript and naming strip on the right, Settings as the last
/// sidebar row. `sessions` is re-requested on mount, after naming an unnamed
/// speaker, and on a `phase` edge (below) — never on every `phase` event,
/// which fires twice a second. Selection is the one piece of page-local
/// state: it is "what is on screen", not anything the bridge holds.
export function Sessions({ onSettings }: SessionsProps) {
  const bridge = useBridge();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [phase, setPhase] = useState<PhasePayload>();
  const [searchOpen, setSearchOpen] = useState(false);
  const [highlightIndex, setHighlightIndex] = useState<number>();

  const loadSessions = useLatest(useCallback(() => bridge.call<SessionSummary[]>("sessions"), [bridge]));

  const refresh = useCallback(async () => {
    const next = await loadSessions();
    if (next !== undefined) setSessions(next);
  }, [loadSessions]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // The phase payload is the state Rust handed over, and the event fires on
  // a timer rather than on a change — reloading `sessions` on every one of
  // them would be two folder walks a second for a payload that already
  // carries everything the strip draws. Reload only on an edge: `kind`
  // changed, `live.id` changed, or `queue` went set→unset.
  const lastEdge = useRef<{ kind: string; liveId: string | null; hadQueue: boolean } | null>(null);
  useEffect(
    () =>
      bridge.on("phase", (payload) => {
        const next = payload as PhasePayload;
        setPhase(next);
        const edge = { kind: next.kind, liveId: next.live?.id ?? null, hadQueue: next.queue !== null };
        const prev = lastEdge.current;
        lastEdge.current = edge;
        if (
          prev !== null &&
          (prev.kind !== edge.kind || prev.liveId !== edge.liveId || (prev.hadQueue && !edge.hadQueue))
        ) {
          void refresh();
        }
      }),
    [bridge, refresh],
  );

  // ⌘K opens the search palette from anywhere on the page.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === "k") {
        e.preventDefault();
        setSearchOpen(true);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const openHit = useCallback((session: string, index: number) => {
    setSelected(session);
    setHighlightIndex(index);
    setSearchOpen(false);
  }, []);

  const selectSession = useCallback((session: string) => {
    setSelected(session);
    setHighlightIndex(undefined);
  }, []);

  // A deleted session must leave `selected` before the next `sessions`
  // refresh, or `Transcript` immediately re-requests a session that no
  // longer exists.
  const onDeleted = useCallback(() => {
    setSelected(null);
    void refresh();
  }, [refresh]);

  const selectedSummary = sessions.find((s) => s.id === selected) ?? null;

  return (
    <div className="flex h-screen">
      <aside className="bg-sidebar text-sidebar-foreground border-sidebar-border flex w-64 flex-col border-r">
        <div className="flex items-center justify-between p-4">
          <h1 className="text-lg font-semibold">Sessions</h1>
          <button
            type="button"
            data-testid="search-open"
            aria-label="Search"
            className="hover:bg-sidebar-accent rounded p-1 text-sm"
            onClick={() => setSearchOpen(true)}
          >
            Search
          </button>
        </div>
        <LivePane payload={phase} />
        <div className="flex-1 overflow-y-auto px-2">
          <SessionList sessions={sessions} selectedId={selected} onSelect={selectSession} />
        </div>
        <button
          type="button"
          data-testid="nav-settings"
          className="border-sidebar-border hover:bg-sidebar-accent border-t p-3 text-left text-sm"
          onClick={onSettings}
        >
          Settings
        </button>
      </aside>
      <main className="flex flex-1 flex-col overflow-hidden">
        {selected === null ? (
          <div className="flex flex-1 items-center justify-center p-8">
            <p className="text-muted-foreground text-sm">Select a session to see its transcript.</p>
          </div>
        ) : (
          <>
            {selectedSummary !== null && (
              <SessionHeader
                key={selectedSummary.id}
                summary={selectedSummary}
                onChanged={refresh}
                onDeleted={onDeleted}
              />
            )}
            {selectedSummary !== null &&
            (sessionState(selectedSummary) === "live" || sessionState(selectedSummary) === "transcribing") ? (
              <LiveTranscript key={selected} session={selected} onDone={refresh} />
            ) : (
              <Transcript session={selected} {...(highlightIndex !== undefined && { highlightIndex })} />
            )}
            <NamingStrip session={selected} onChanged={refresh} />
          </>
        )}
      </main>
      {searchOpen && (
        <SearchPalette sessions={sessions} onOpen={openHit} onClose={() => setSearchOpen(false)} />
      )}
    </div>
  );
}
