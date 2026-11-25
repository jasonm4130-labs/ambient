import { useCallback, useEffect, useRef, useState } from "react";
import { LivePane, type CaptureConfig, type PhasePayload } from "@/components/LivePane";
import { LiveTranscript } from "@/components/LiveTranscript";
import { NamingStrip } from "@/components/NamingStrip";
import { SearchPalette } from "@/components/SearchPalette";
import { SessionHeader } from "@/components/SessionHeader";
import { SessionList, sessionState, type SessionSummary } from "@/components/SessionList";
import { Transcript } from "@/components/Transcript";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";
import { Welcome, type HealthCheck } from "./Welcome";

interface SessionsProps {
  onSettings: () => void;
  health?: HealthCheck[] | undefined;
  onRetryHealth?: () => void;
}

export function Sessions({ onSettings, health, onRetryHealth }: SessionsProps) {
  const bridge = useBridge();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [phase, setPhase] = useState<PhasePayload>();
  const [searchOpen, setSearchOpen] = useState(false);
  const [highlightIndex, setHighlightIndex] = useState<number>();
  const [transcriptRevision, setTranscriptRevision] = useState(0);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string>();
  const [capture, setCapture] = useState<CaptureConfig>();

  const loadSessions = useLatest(
    useCallback(() => bridge.call<SessionSummary[]>("sessions"), [bridge]),
  );

  const refresh = useCallback(async () => {
    try {
      const next = await loadSessions();
      if (next !== undefined) {
        setSessions(next);
        setLoaded(true);
        setError(undefined);
      }
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [loadSessions]);

  const loadConfig = useLatest(
    useCallback(() => bridge.call<CaptureConfig | undefined>("config.get"), [bridge]),
  );
  const refreshConfig = useCallback(() => {
    void loadConfig()
      .then((next) => {
        if (next !== undefined) setCapture(next);
      })
      .catch(() => setCapture(undefined));
  }, [loadConfig]);

  useEffect(() => {
    void refresh();
    refreshConfig();
  }, [refresh, refreshConfig]);
  useEffect(
    () =>
      bridge.on("config", () => {
        void refresh();
        refreshConfig();
      }),
    [bridge, refresh, refreshConfig],
  );
  // Rust sends no device-change event, so re-read `config.get` on becoming visible.
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") refreshConfig();
    };
    window.addEventListener("focus", refreshConfig);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", refreshConfig);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [refreshConfig]);

  // `phase` fires on a timer, twice a second: reload `sessions` only on an edge.
  const lastEdge = useRef<{ kind: string; liveId: string | null; hadQueue: boolean } | null>(null);
  useEffect(
    () =>
      bridge.on("phase", (payload) => {
        const next = payload as PhasePayload;
        setPhase(next);
        const edge = {
          kind: next.kind,
          liveId: next.live?.id ?? null,
          hadQueue: next.queue !== null,
        };
        const prev = lastEdge.current;
        lastEdge.current = edge;
        // Select a new live session once, on the edge, so picking another mid-recording sticks.
        if (edge.liveId !== null && prev?.liveId !== edge.liveId) {
          setSelected(edge.liveId);
          setHighlightIndex(undefined);
        }
        if (
          prev !== null &&
          (prev.kind !== edge.kind ||
            prev.liveId !== edge.liveId ||
            (prev.hadQueue && !edge.hadQueue))
        ) {
          void refresh();
          refreshConfig();
        }
      }),
    [bridge, refresh, refreshConfig],
  );

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

  // Clear `selected` before the refresh, or `Transcript` re-requests a deleted session.
  const onDeleted = useCallback(() => {
    setSelected(null);
    void refresh();
  }, [refresh]);

  const selectedSummary = sessions.find((s) => s.id === selected) ?? null;
  const liveId = phase?.live?.id ?? null;
  // The phase edge can select the live session before `sessions` lists it.
  const selectedIsLive =
    selectedSummary === null
      ? selected !== null && selected === liveId
      : sessionState(selectedSummary) === "live" ||
        sessionState(selectedSummary) === "transcribing";

  useEffect(
    () =>
      bridge.on("command", (payload) => {
        if (selected !== null || (payload as { action: string }).action !== "reveal") return;
        void bridge.call("reveal").catch((e: unknown) => setError(String(e)));
      }),
    [bridge, selected],
  );

  return (
    <div className="flex h-screen">
      <aside className="bg-sidebar text-sidebar-foreground border-sidebar-border flex w-[260px] shrink-0 flex-col border-r">
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
        <LivePane
          payload={phase}
          config={capture}
          liveName={sessions.find((s) => s.id === liveId)?.name ?? null}
          onOpenLive={() => {
            if (liveId !== null) selectSession(liveId);
          }}
          onAddApps={onSettings}
        />
        <div className="flex min-h-0 flex-1 flex-col">
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
      <main
        className="flex min-w-0 flex-1 flex-col overflow-hidden"
        data-selected-session={selected ?? ""}
      >
        {error !== undefined && (
          <p role="alert" className="text-destructive p-4">
            {error}
          </p>
        )}
        {health !== undefined &&
        (health.some((check) => !check.ok) ||
          (loaded &&
            sessions.length === 0 &&
            (phase?.live === null || phase?.live === undefined))) ? (
          <Welcome
            checks={health}
            onRetry={() => {
              onRetryHealth?.();
              void refresh();
            }}
          />
        ) : selected === null ? (
          <div className="flex flex-1 items-center justify-center p-8">
            <p className="text-muted-foreground text-sm">
              {loaded
                ? sessions.length === 0
                  ? "No sessions yet."
                  : "Select a session to see its transcript."
                : "Loading sessions…"}
            </p>
          </div>
        ) : (
          <>
            {selectedSummary?.error != null && (
              <p
                role="alert"
                data-testid="session-error"
                className="text-destructive bg-accent p-3"
              >
                This session is broken: {selectedSummary.error}
              </p>
            )}
            {(selectedSummary?.warnings ?? []).map((warning) => (
              <p key={warning} role="alert" className="bg-accent p-3">
                {warning}
              </p>
            ))}
            {selectedSummary !== null && (
              <SessionHeader
                key={selectedSummary.id}
                summary={selectedSummary}
                onChanged={refresh}
                onDeleted={onDeleted}
              />
            )}
            {selectedIsLive ? (
              <LiveTranscript key={`live-${selected}`} session={selected} onDone={refresh} />
            ) : (
              <Transcript
                key={`transcript-${selected}`}
                unavailable={
                  selectedSummary?.transcribing === true
                    ? "This session is being transcribed."
                    : selectedSummary?.audio_available === false
                      ? "The audio is no longer available to transcribe again."
                      : undefined
                }
                session={selected}
                revision={transcriptRevision}
                {...(highlightIndex !== undefined && { highlightIndex })}
              />
            )}
            <NamingStrip
              key={`naming-${selected}`}
              session={selected}
              onChanged={() => {
                setTranscriptRevision((revision) => revision + 1);
                void refresh();
              }}
            />
          </>
        )}
      </main>
      {searchOpen && (
        <SearchPalette sessions={sessions} onOpen={openHit} onClose={() => setSearchOpen(false)} />
      )}
    </div>
  );
}
