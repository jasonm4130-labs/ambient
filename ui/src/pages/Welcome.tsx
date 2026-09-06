import { useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";

export interface HealthCheck {
  name: string;
  ok: boolean;
  detail: string;
}

export const fixes: Record<string, string> = {
  models: "Run ./fetch-models.sh from the Ambient checkout to install the models.",
  config: "Open Settings and correct the configuration, then check again.",
  "sessions/writable": "Choose a writable sessions folder in Settings → Storage.",
  "sessions/stale-lock":
    "Quit Ambient, remove transcribing.lock from the listed session folder, then reopen Ambient.",
  permissions:
    "Open System Settings → Privacy & Security and allow Ambient to use the microphone and system audio.",
  other: "Run ambient doctor in a terminal to inspect the failure, then check again.",
};

export function Welcome({ checks, onRetry }: { checks: HealthCheck[]; onRetry: () => void }) {
  const bridge = useBridge();
  const [error, setError] = useState<string>();
  const [starting, setStarting] = useState(false);
  const ready = checks.length > 0 && checks.every((check) => check.ok);
  return (
    <section
      className="mx-auto flex w-full max-w-2xl flex-col gap-4 overflow-y-auto p-8"
      data-testid="welcome"
    >
      <h2 className="text-xl font-semibold">Welcome to Ambient</h2>
      <p>
        {ready
          ? "No sessions yet. Start a recording when you are ready."
          : "A few things need attention before recording."}
      </p>
      <ul className="flex flex-col gap-3">
        {checks.map((check) => (
          <li key={check.name}>
            <p className="font-medium">
              {check.ok ? "✓" : "!"} {check.name}
            </p>
            {!check.ok && (
              <>
                <p className="text-destructive">{check.detail}</p>
                <p>
                  {fixes[check.name.startsWith("models/") ? "models" : check.name] ?? fixes.other}
                </p>
              </>
            )}
          </li>
        ))}
      </ul>
      {error !== undefined && (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      )}
      <div className="flex gap-2">
        {ready && (
          <Button
            data-testid="start-recording"
            disabled={starting}
            onClick={() => {
              setStarting(true);
              setError(undefined);
              void bridge
                .call<{ sent: boolean }>("record.start")
                .then((reply) => {
                  if (!reply.sent)
                    throw new Error("Recording could not be started. Try again from the menu bar.");
                })
                .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
                .finally(() => setStarting(false));
            }}
          >
            Start recording
          </Button>
        )}
        <Button variant="outline" onClick={onRetry}>
          Check again
        </Button>
      </div>
    </section>
  );
}
