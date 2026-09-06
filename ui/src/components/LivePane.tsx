import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";

/// One `Live` (`src/state.rs:60`), projected by `phase_payload`
/// (`src/window.rs`) off the shared `session::Meter` the same way
/// `LiveShot::of` draws the native pane. Levels arrive already clamped to
/// `0.0..=1.0`.
export interface LiveShot {
  id: string;
  elapsed_s: number;
  room_level: number;
  call_level: number;
  audio_arriving: boolean;
  status_line: string;
}

export interface Failure {
  error: string;
  session: string | null;
}

/// The `phase` event's wire shape (`src/window.rs`'s `phase_payload`).
export interface PhasePayload {
  kind: "idle" | "armed" | "recording" | "stopping" | "failed";
  app: string | null;
  live: LiveShot | null;
  queue: string | null;
  failure: Failure | null;
}

interface LivePaneProps {
  payload: PhasePayload | undefined;
}

function clock(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
}

function Meter({ label, level }: { label: string; level: number }) {
  const pct = Math.max(0, Math.min(1, level)) * 100;
  return (
    <div
      className="flex items-center gap-2 text-xs"
      data-testid={`live-meter-${label.toLowerCase()}`}
      data-level={level}
    >
      <span className="text-muted-foreground w-10">{label}</span>
      <div className="bg-muted h-2 flex-1 rounded-full">
        <div className="bg-primary h-2 rounded-full" style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

/// The strip pinned above the session list: what the app is doing *now*,
/// drawn straight off the `phase` event `Sessions` hands it. Pure — the
/// subscription lives in `Sessions`, so this is directly testable with a
/// `FakeBridge` and no timer.
export function LivePane({ payload }: LivePaneProps) {
  const bridge = useBridge();

  if (payload === undefined) return null;

  if (payload.kind === "armed") {
    return (
      <div className="border-sidebar-border border-b p-3" data-testid="live-card">
        <p className="text-sm font-medium">{payload.app}</p>
        <div className="mt-2 flex gap-2">
          <Button
            type="button"
            size="sm"
            data-testid="record-this-call"
            onClick={() => void bridge.call("record.this_call")}
          >
            Record this call
          </Button>
          <Button
            type="button"
            size="sm"
            variant="outline"
            data-testid="record-decline"
            onClick={() => void bridge.call("record.decline")}
          >
            Not this one
          </Button>
        </div>
      </div>
    );
  }

  if (payload.kind === "recording" || payload.kind === "stopping") {
    const live = payload.live;
    return (
      <div className="border-sidebar-border border-b p-3" data-testid="live-card">
        <div className="flex items-center justify-between">
          <span className="font-mono text-sm" data-testid="live-clock">
            {live === null ? "00:00" : clock(live.elapsed_s)}
          </span>
          <Button
            type="button"
            size="sm"
            variant="outline"
            data-testid="record-stop"
            disabled={payload.kind !== "recording"}
            onClick={() => void bridge.call("record.stop")}
          >
            Stop
          </Button>
        </div>
        {live !== null && (
          <div className="mt-2 flex flex-col gap-1">
            <Meter label="Room" level={live.room_level} />
            <Meter label="Call" level={live.call_level} />
            <p className="text-muted-foreground text-xs" data-testid="live-status">
              {live.status_line}
            </p>
            {!live.audio_arriving && (
              <p role="alert" data-testid="live-no-audio" className="text-destructive text-xs">
                No audio is arriving.
              </p>
            )}
          </div>
        )}
      </div>
    );
  }

  if (payload.kind === "failed") {
    return (
      <div className="border-sidebar-border border-b p-3" data-testid="live-card" role="alert">
        <p className="text-destructive text-sm" data-testid="live-failure-error">
          {payload.failure?.error}
        </p>
        {payload.failure?.session !== null && payload.failure?.session !== undefined && (
          <p className="text-muted-foreground text-xs">{payload.failure.session}</p>
        )}
        <Button
          type="button"
          size="sm"
          variant="outline"
          className="mt-2"
          data-testid="dismiss-failure"
          onClick={() => void bridge.call("dismiss")}
        >
          Dismiss
        </Button>
      </div>
    );
  }

  // idle
  if (payload.queue !== null) {
    return (
      <div className="text-muted-foreground border-sidebar-border border-b p-3 text-xs" data-testid="live-queue">
        {payload.queue}
      </div>
    );
  }
  return null;
}
