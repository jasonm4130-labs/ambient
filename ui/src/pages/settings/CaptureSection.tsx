import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import type { SettingsConfig } from "./types";

interface CaptureSectionProps {
  config: SettingsConfig;
  filtered: boolean;
  onScopeChange: (scope: "all" | "some") => void;
  onAddApp: () => void;
  onRemoveApp: (id: string) => void;
  onMicChange: (device: string) => void;
  onAskBeforeRecordingChange: (value: boolean) => void;
}

export function CaptureSection({
  config,
  filtered,
  onScopeChange,
  onAddApp,
  onRemoveApp,
  onMicChange,
  onAskBeforeRecordingChange,
}: CaptureSectionProps) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="capture-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Recording
      </h2>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Capture</span>
          <span className="text-muted-foreground text-xs">
            Everything the Mac plays, or only these apps.
          </span>
        </div>
        <Select
          aria-label="Capture scope"
          data-testid="scope-select"
          value={filtered ? "some" : "all"}
          onChange={(e) => onScopeChange(e.target.value === "some" ? "some" : "all")}
        >
          <option value="all">Everything</option>
          <option value="some">Selected apps</option>
        </Select>
      </div>
      {filtered && (
        <div className="flex flex-wrap gap-2" data-testid="app-chips">
          {config.apps.map((id) => (
            <span
              key={id}
              className="bg-muted flex items-center gap-2 rounded-md px-2 py-1 text-xs"
            >
              {id}
              <button
                type="button"
                aria-label={`Stop capturing ${id}`}
                className="text-muted-foreground hover:text-foreground"
                onClick={() => onRemoveApp(id)}
              >
                ×
              </button>
            </span>
          ))}
          <Button
            type="button"
            variant="outline"
            size="sm"
            data-testid="add-app"
            onClick={onAddApp}
          >
            + Add
          </Button>
        </div>
      )}
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Microphone</span>
          <span className="text-muted-foreground text-xs">
            Your side of the room. Held separately from the call audio.
          </span>
        </div>
        <Select
          aria-label="Microphone"
          data-testid="mic-select"
          value={config.input_device ?? "default"}
          onChange={(e) => onMicChange(e.target.value)}
        >
          <option value="default">System default</option>
          {config.devices.map((d) => (
            <option key={d} value={d}>
              {d}
            </option>
          ))}
        </Select>
      </div>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Ask before recording a call</span>
          <span className="text-muted-foreground text-xs">
            {filtered
              ? "Wait to be told when one of these apps starts audio."
              : "Only watches the apps listed above — add one to be asked."}
          </span>
        </div>
        <Switch
          aria-label="Ask before recording a call"
          data-testid="ask-before-recording"
          checked={config.ask_before_recording}
          onCheckedChange={onAskBeforeRecordingChange}
        />
      </div>
    </section>
  );
}
