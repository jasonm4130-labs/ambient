import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";

interface StorageSectionProps {
  sessionsDir: string | null;
  defaultDir: string;
  audioRetention: string;
  error: string | undefined;
  onChangeDir: () => void;
  onRetentionChange: (value: string) => void;
}

const RETENTION_OPTIONS = [
  { value: "0", label: "Until transcribed" },
  { value: "7", label: "7 days" },
  { value: "30", label: "30 days" },
  { value: "forever", label: "Forever" },
];

export function StorageSection({
  sessionsDir,
  defaultDir,
  audioRetention,
  error,
  onChangeDir,
  onRetentionChange,
}: StorageSectionProps) {
  // A value set from the CLI need not be one of the offered periods; showing
  // the select blank would misreport the setting behind it.
  const known = RETENTION_OPTIONS.some((o) => o.value === audioRetention);

  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="storage-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Export
      </h2>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Sessions folder</span>
          <span className="text-muted-foreground text-xs" data-testid="sessions-dir">
            {sessionsDir ?? defaultDir}
          </span>
          {error !== undefined && (
            <span className="text-destructive text-xs" data-testid="sessions-dir-error">
              {error}
            </span>
          )}
        </div>
        <Button type="button" variant="outline" data-testid="change-dir" onClick={onChangeDir}>
          Change…
        </Button>
      </div>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Keep the audio</span>
          <span className="text-muted-foreground text-xs">
            Transcripts are always kept. The recording itself is the sensitive part,
            and the least useful once it is written down.
          </span>
        </div>
        <Select
          aria-label="Keep the audio"
          data-testid="retention-select"
          value={audioRetention}
          onChange={(e) => onRetentionChange(e.target.value)}
        >
          {RETENTION_OPTIONS.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
          {!known && <option value={audioRetention}>{audioRetention} days</option>}
        </Select>
      </div>
    </section>
  );
}
