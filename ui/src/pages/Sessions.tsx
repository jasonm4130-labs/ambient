import { useCallback, useEffect, useState } from "react";
import { NamingStrip } from "@/components/NamingStrip";
import { SessionList, type SessionSummary } from "@/components/SessionList";
import { Transcript } from "@/components/Transcript";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";

interface SessionsProps {
  onSettings: () => void;
}

/// The sidebar-plus-content screen: sessions on the left, the selected
/// session's transcript and naming strip on the right, Settings as the last
/// sidebar row. There is no `phase` event until unit 5, so `sessions` is
/// re-requested only on mount and after naming an unnamed speaker — the only
/// action here that can change what the sidebar shows next time it is read
/// elsewhere. Selection is the one piece of page-local state: it is "what is
/// on screen", not anything the bridge holds.
export function Sessions({ onSettings }: SessionsProps) {
  const bridge = useBridge();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);

  const loadSessions = useLatest(useCallback(() => bridge.call<SessionSummary[]>("sessions"), [bridge]));

  const refresh = useCallback(async () => {
    const next = await loadSessions();
    if (next !== undefined) setSessions(next);
  }, [loadSessions]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <div className="flex h-screen">
      <aside className="bg-sidebar text-sidebar-foreground border-sidebar-border flex w-64 flex-col border-r">
        <h1 className="p-4 text-lg font-semibold">Sessions</h1>
        <div className="flex-1 overflow-y-auto px-2">
          <SessionList sessions={sessions} selectedId={selected} onSelect={setSelected} />
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
            <Transcript session={selected} />
            <NamingStrip session={selected} onChanged={refresh} />
          </>
        )}
      </main>
    </div>
  );
}
