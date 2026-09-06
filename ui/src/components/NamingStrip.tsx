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
  /// The session the strip was shown for — passed down rather than re-read
  /// from whatever is currently selected, because a recording can finish
  /// while the window is open and the selection can move on without this
  /// strip's target moving with it.
  session: string;
  /// Told after a successful `speakers.name` or `speakers.undo`, so the
  /// sidebar can re-request `sessions` — naming does not change a
  /// `SessionSummary` field today, but the page's rule is to re-request
  /// after every action rather than assume what did or did not change.
  onChanged?: () => void;
}

/// One row per label `speakers.unnamed` reports for `session`, each with a
/// roster-backed combobox (a real `<input list=…>` plus `<datalist>`, the
/// closest match to the deleted native pane's `NSComboBox::setCompletes`) and
/// a Name button. Undo reverts the whole session's naming, matching
/// `speakers.undo`'s shape. The roster is read from `config.get` here rather
/// than passed down from a parent that already has one — two readings of the
/// same roster can disagree, same as Settings' own `config.get` read.
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
            onClick={() => {
              const name = (drafts[speaker.label] ?? "").trim();
              if (name === "") return;
              setError(undefined);
              void bridge
                .call("speakers.name", { session, label: speaker.label, name })
                .then(refresh)
                .then(onChanged)
                .catch(failed);
            }}
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
