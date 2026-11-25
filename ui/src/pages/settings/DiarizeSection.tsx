import { Switch } from "@/components/ui/switch";

interface DiarizeSectionProps {
  diarize: boolean;
  onDiarizeChange: (value: boolean) => void;
}

export function DiarizeSection({ diarize, onDiarizeChange }: DiarizeSectionProps) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="diarize-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Speakers
      </h2>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Separate voices after recording</span>
          <span className="text-muted-foreground text-xs">
            Adds a few seconds for every ten minutes recorded.
          </span>
        </div>
        <Switch
          aria-label="Separate voices after recording"
          data-testid="diarize-switch"
          checked={diarize}
          onCheckedChange={onDiarizeChange}
        />
      </div>
    </section>
  );
}
