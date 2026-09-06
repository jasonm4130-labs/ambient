import { Select } from "@/components/ui/select";
import type { UnnamedSpeaker } from "./types";

interface NamingSectionProps {
  latestSession: string | null;
  unnamed: UnnamedSpeaker[] | null;
  roster: string[];
  onName: (label: string, name: string) => void;
}

// One row per speaker the last recording could not name, each with the first
// thing that voice said. Choosing a name applies it to every line of that
// speaker; there is no guessing, because nothing is stored that could guess.
export function NamingSection({ latestSession, unnamed, roster, onName }: NamingSectionProps) {
  let hint: string;
  if (latestSession === null) {
    hint = "Nothing recorded yet.";
  } else if (unnamed === null) {
    hint = `${latestSession} could not be read.`;
  } else if (unnamed.length === 0) {
    hint = `Everyone in ${latestSession} has a name.`;
  } else {
    hint = latestSession;
  }

  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="naming-section">
      <div className="flex flex-col">
        <span className="text-sm">Speakers in the last recording</span>
        <span className="text-muted-foreground text-xs" data-testid="naming-hint">
          {hint}
        </span>
      </div>
      {unnamed?.map((speaker) => (
        <div
          key={speaker.label}
          className="flex items-center justify-between gap-4"
          data-testid="naming-row"
        >
          <div className="flex flex-col">
            <span className="text-sm">{speaker.label}</span>
            <span className="text-muted-foreground text-xs italic">
              {speaker.sample === "" ? "(no speech)" : `"${speaker.sample}"`}
            </span>
          </div>
          <Select
            aria-label={`Name for ${speaker.label}`}
            data-testid="naming-select"
            value=""
            onChange={(e) => {
              if (e.target.value !== "") onName(speaker.label, e.target.value);
            }}
          >
            <option value="">Not named</option>
            {roster.map((who) => (
              <option key={who} value={who}>
                {who}
              </option>
            ))}
          </Select>
        </div>
      ))}
    </section>
  );
}
