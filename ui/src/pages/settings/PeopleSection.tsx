import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

interface PeopleSectionProps {
  roster: string[];
  onAdd: (name: string) => void;
  onRemove: (name: string) => void;
}

// Named once here, then offered whenever a recording needs its speakers
// named. No voice is stored.
export function PeopleSection({ roster, onAdd, onRemove }: PeopleSectionProps) {
  const [name, setName] = useState("");

  const submit = (): void => {
    const trimmed = name.trim();
    if (trimmed === "") return;
    setName("");
    onAdd(trimmed);
  };

  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="people-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Who's who
      </h2>
      <div className="flex flex-wrap gap-2" data-testid="roster-chips">
        {roster.length === 0 && <span className="text-muted-foreground text-xs">Nobody yet.</span>}
        {roster.map((who) => (
          <span
            key={who}
            className="bg-muted flex items-center gap-2 rounded-md px-2 py-1 text-xs"
          >
            {who}
            <button
              type="button"
              aria-label={`Remove ${who}`}
              className="text-muted-foreground hover:text-foreground"
              onClick={() => onRemove(who)}
            >
              ×
            </button>
          </span>
        ))}
      </div>
      <div className="flex gap-2">
        <Input
          aria-label="Add someone"
          data-testid="person-input"
          placeholder="Add someone…"
          autoComplete="off"
          spellCheck={false}
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
          }}
        />
        <Button type="button" data-testid="add-person" onClick={submit}>
          Add
        </Button>
      </div>
    </section>
  );
}
