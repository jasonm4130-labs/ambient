import { useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";

/// The part of `config.get` the idle card reads: which microphone is saved,
/// which inputs exist right now, and which apps are watched for calls. Every
/// field is optional because a partial or failed `config.get` must still let
/// the card offer Record.
export interface CaptureConfig {
  input_device?: string | null;
  devices?: string[];
  apps?: string[];
}

/// One line naming the microphone a recording would use. A saved device that
/// is not plugged in is said out loud, because capture quietly falls back to
/// the system default (`capture::input_device`) and the Settings select can
/// only show "System default" for a name it has no option for.
export function micLine(config: CaptureConfig): string {
  const saved = config.input_device ?? null;
  if (saved === null) return "Mic: System default";
  if (config.devices !== undefined && !config.devices.includes(saved))
    return `Mic: ${saved} is not connected, so the system default is used`;
  return `Mic: ${saved}`;
}

/// Record plus its ⌘R hint. `record.start` replies `{sent: false}` when the
/// menu bar refused to start, which is an error worth showing here.
function RecordButton() {
  const bridge = useBridge();
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string>();

  const start = () => {
    setStarting(true);
    setError(undefined);
    void bridge
      .call<{ sent: boolean } | undefined>("record.start")
      .then((reply) => {
        if (reply?.sent === false)
          throw new Error("Recording could not be started. Try again from the menu bar.");
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setStarting(false));
  };

  return (
    <>
      <div className="flex items-center gap-2">
        <Button
          type="button"
          size="sm"
          variant="destructive"
          className="flex-1"
          data-testid="record-start"
          aria-keyshortcuts="Meta+R"
          disabled={starting}
          onClick={start}
        >
          <span aria-hidden="true" className="size-2 rounded-full bg-white" />
          Record
        </Button>
        <kbd className="text-muted-foreground rounded border px-1 font-mono text-xs">⌘R</kbd>
      </div>
      {error !== undefined && (
        <p role="alert" className="text-destructive mt-2 text-xs">
          {error}
        </p>
      )}
    </>
  );
}

/// The idle card: Record is always one click (or ⌘R) away, whatever is in the
/// session list. Before this card the only in-window way to start was the
/// Welcome screen, which disappears once one session exists. An empty `apps`
/// list watches nothing for calls (ADR 0009), so the card says so and links
/// to Settings, where apps are added.
export function IdleCard({
  config,
  onAddApps,
}: {
  config: CaptureConfig | undefined;
  onAddApps: (() => void) | undefined;
}) {
  const notWatching = config?.apps !== undefined && config.apps.length === 0;
  return (
    <div className="border-sidebar-border border-b p-3" data-testid="idle-card">
      <RecordButton />
      {config !== undefined && (
        <p className="text-muted-foreground mt-2 text-xs" data-testid="live-mic">
          {micLine(config)}
        </p>
      )}
      {notWatching && (
        <p className="text-muted-foreground mt-1 text-xs" data-testid="live-watch">
          Not watching any call apps ·{" "}
          <button
            type="button"
            className="hover:text-foreground underline"
            data-testid="add-apps"
            onClick={onAddApps}
          >
            Add apps
          </button>
        </p>
      )}
    </div>
  );
}
