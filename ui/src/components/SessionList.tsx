import { useRef } from "react";
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

/// Sessions in the order `sessions` returned them (newest first already, no
/// client-side sort). Each row is a real `<button>`, both for
/// `role="button"` queries in tests and so `uicheck` can drive it in a real
/// WKWebView. `onSelect` fires on click and on ArrowDown/ArrowUp from a row,
/// which also moves focus so repeated arrow presses keep working.
export function SessionList({ sessions, selectedId, onSelect }: SessionListProps) {
  const rows = useRef<(HTMLButtonElement | null)[]>([]);

  return (
    <ul className="flex flex-col gap-1" data-testid="session-list">
      {sessions.map((s, i) => {
        const duration = formatDuration(s.duration_s);
        const label = s.name ?? s.id;
        return (
          <li key={s.id}>
            <button
              type="button"
              ref={(el) => {
                rows.current[i] = el;
              }}
              data-testid="session-row"
              aria-current={s.id === selectedId}
              className={cn(
                "hover:bg-sidebar-accent flex w-full flex-col items-start gap-1 rounded-md px-3 py-2 text-left text-sm",
                s.id === selectedId && "bg-sidebar-accent",
              )}
              onClick={() => onSelect(s.id)}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  const next = sessions[i + 1];
                  if (next === undefined) return;
                  rows.current[i + 1]?.focus();
                  onSelect(next.id);
                } else if (e.key === "ArrowUp") {
                  e.preventDefault();
                  const prev = sessions[i - 1];
                  if (prev === undefined) return;
                  rows.current[i - 1]?.focus();
                  onSelect(prev.id);
                }
              }}
            >
              <span className="font-medium">{label}</span>
              <span className="text-muted-foreground flex items-center gap-2 text-xs">
                {s.started_at !== null && <span>{s.started_at}</span>}
                {duration !== null && <span>{duration}</span>}
                <Badge variant="outline">{sessionState(s)}</Badge>
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}
