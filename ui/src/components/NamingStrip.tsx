import { useCallback, useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";

interface UnnamedSpeaker {
  label: string;
  sample: string;
}

interface ConfigReply {
  roster: string[];
}

interface NamingStripProps {
  /// Fixed at mount: a recording can finish and move the selection meanwhile.
  session: string;
  onChanged?: () => void;
}

export function NamingStrip({ session, onChanged }: NamingStripProps) {
  const bridge = useBridge();
  const [unnamed, setUnnamed] = useState<UnnamedSpeaker[]>([]);
  const [roster, setRoster] = useState<string[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [error, setError] = useState<string>();
  const failed = (e: unknown) => setError(e instanceof Error ? e.message : String(e));

  const loadUnnamed = useLatest(
    useCallback(
      () => bridge.call<UnnamedSpeaker[]>("speakers.unnamed", { session }),
      [bridge, session],
    ),
  );
  const loadRoster = useLatest(useCallback(() => bridge.call<ConfigReply>("config.get"), [bridge]));

  const refresh = useCallback(async () => {
    const speakers = await loadUnnamed();
    if (speakers !== undefined) setUnnamed(speakers);
    const config = await loadRoster();
    if (config !== undefined) setRoster(config.roster);
  }, [loadUnnamed, loadRoster]);

  const name = (label: string) => {
    const who = (drafts[label] ?? "").trim();
    if (who === "") return;
    setError(undefined);
    void bridge
      .call("speakers.name", { session, label, name: who })
      .then(refresh)
      .then(onChanged)
      .catch(failed);
  };

  useEffect(() => {
    void refresh().catch(failed);
  }, [refresh]);
  useEffect(
    () =>
      bridge.on("diarize", (payload) => {
        const event = payload as { session: string; state: string };
        if (event.session === session && event.state === "done") void refresh().catch(failed);
      }),
    [bridge, session, refresh],
  );

  return (
    <div
      className="flex max-h-[35vh] shrink-0 flex-col gap-2 overflow-y-auto border-t p-3"
      data-testid="naming-strip"
    >
      {error !== undefined && (
        <p role="alert" className="text-destructive">
          {error}
        </p>
      )}
      {unnamed.map((speaker) => (
        <div key={speaker.label} className="flex items-center gap-2" data-testid="naming-strip-row">
          <span className="min-w-0 flex-1 truncate text-sm" title={speaker.sample}>
            {speaker.label} — {speaker.sample === "" ? "(no speech)" : speaker.sample}
          </span>
          <input
            list={`naming-strip-roster-${speaker.label}`}
            aria-label={`Name for ${speaker.label}`}
            data-testid="naming-strip-input"
            className="border-input h-9 w-32 min-w-0 rounded-md border bg-transparent px-2 text-sm"
            value={drafts[speaker.label] ?? ""}
            onChange={(e) => {
              const value = e.target.value;
              setDrafts((prev) => ({ ...prev, [speaker.label]: value }));
            }}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              e.preventDefault();
              name(speaker.label);
            }}
          />
          <datalist id={`naming-strip-roster-${speaker.label}`}>
            {roster.map((who) => (
              <option key={who} value={who} />
            ))}
          </datalist>
          <Button
            type="button"
            size="sm"
            data-testid="naming-strip-name"
            onClick={() => name(speaker.label)}
          >
            Name
          </Button>
        </div>
      ))}
      <Button
        type="button"
        variant="outline"
        size="sm"
        data-testid="naming-strip-undo"
        onClick={() => {
          setError(undefined);
          void bridge
            .call("speakers.undo", { session })
            .then(refresh)
            .then(onChanged)
            .catch(failed);
        }}
      >
        Undo
      </Button>
    </div>
  );
}
