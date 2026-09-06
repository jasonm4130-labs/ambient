import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { fromSlider, toSlider } from "./types";

interface DiarizeSectionProps {
  diarize: boolean;
  threshold: number;
  onDiarizeChange: (value: boolean) => void;
  onThresholdChange: (value: number) => void;
}

export function DiarizeSection({
  diarize,
  threshold,
  onDiarizeChange,
  onThresholdChange,
}: DiarizeSectionProps) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="diarize-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Speakers
      </h2>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Separate voices after recording</span>
          <span className="text-muted-foreground text-xs">
            Adds about a minute for every ten recorded.
          </span>
        </div>
        <Switch
          aria-label="Separate voices after recording"
          data-testid="diarize-switch"
          checked={diarize}
          onCheckedChange={onDiarizeChange}
        />
      </div>
      <div
        className="flex items-center justify-between gap-4"
        style={{ opacity: diarize ? 1 : 0.4 }}
      >
        <div className="flex flex-col">
          <span className="text-sm">Sensitivity</span>
          <span className="text-muted-foreground text-xs">
            How readily two voices are called different people.
          </span>
        </div>
        <div className="flex w-48 items-center gap-2">
          <span className="text-muted-foreground text-[10px]">Fewer</span>
          <Slider
            aria-label="Sensitivity"
            data-testid="sensitivity-slider"
            min={30}
            max={80}
            step={1}
            disabled={!diarize}
            value={toSlider(threshold)}
            onChange={(e) => onThresholdChange(fromSlider(Number(e.target.value)))}
          />
          <span className="text-muted-foreground text-[10px]">More</span>
        </div>
      </div>
    </section>
  );
}
