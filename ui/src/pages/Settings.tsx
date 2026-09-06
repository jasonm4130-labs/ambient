import { useCallback, useEffect, useState } from "react";
import { useBridge } from "@/lib/bridge-context";
import { useLatest } from "@/lib/latest";
import { CaptureSection } from "./settings/CaptureSection";
import { DiarizeSection } from "./settings/DiarizeSection";
import { NamingSection } from "./settings/NamingSection";
import { PeopleSection } from "./settings/PeopleSection";
import { StorageSection } from "./settings/StorageSection";
import type { SettingsConfig, UnnamedSpeaker } from "./settings/types";

type Scope = "all" | "some";

/// The Settings screen, at parity with the deleted vanilla page. The Rust
/// bridge holds the state; this component owns the `config` event
/// subscription and re-requests `config.get` (and, when there is a latest
/// session, `speakers.unnamed`) on mount and after every event. The only
/// page-local state is which capture mode was picked and what is being
/// typed — "Selected apps, none chosen yet" and "Everything" are the same
/// empty `apps` list in the config, so that choice cannot be read back from
/// it.
export function Settings() {
  const bridge = useBridge();
  const [config, setConfig] = useState<SettingsConfig>();
  const [unnamed, setUnnamed] = useState<UnnamedSpeaker[] | null>(null);
  const [scopeChoice, setScopeChoice] = useState<Scope | null>(null);
  const [sessionsDirError, setSessionsDirError] = useState<string>();

  const loadConfig = useLatest(
    useCallback(() => bridge.call<SettingsConfig>("config.get"), [bridge]),
  );
  const loadUnnamed = useLatest(
    useCallback(
      (session: string) => bridge.call<UnnamedSpeaker[]>("speakers.unnamed", { session }),
      [bridge],
    ),
  );

  const refresh = useCallback(async () => {
    const next = await loadConfig();
    if (next === undefined) return;
    setConfig(next);
    if (next.apps.length > 0) setScopeChoice("some");
    if (next.latest_session === null) {
      setUnnamed(null);
      return;
    }
    const speakers = await loadUnnamed(next.latest_session);
    if (speakers !== undefined) setUnnamed(speakers);
  }, [loadConfig, loadUnnamed]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => bridge.on("config", () => void refresh()), [bridge, refresh]);

  const setKey = useCallback(
    async (key: string, value: string) => {
      const next = await bridge.call<SettingsConfig>("config.set", { key, value });
      setConfig(next);
    },
    [bridge],
  );

  return (
    <div className="mx-auto flex max-w-2xl flex-col gap-6 p-6">
      <h1 className="text-2xl font-semibold">Settings</h1>
      {config === undefined ? (
        <p className="text-muted-foreground text-sm">Loading…</p>
      ) : (
        <>
          <CaptureSection
            config={config}
            filtered={scopeChoice === "some"}
            onScopeChange={(scope) => {
              setScopeChoice(scope);
              void bridge
                .call("capture.scope", { everything: scope === "all" })
                .then(() => refresh());
            }}
            onAddApp={() => {
              void bridge.call<{ chosen: string | null }>("pick_app").then(({ chosen }) => {
                if (chosen === null) return;
                const apps = config.apps.includes(chosen) ? config.apps : [...config.apps, chosen];
                return setKey("apps", apps.join(","));
              });
            }}
            onRemoveApp={(id) => {
              void setKey("apps", config.apps.filter((a) => a !== id).join(","));
            }}
            onMicChange={(device) => {
              void setKey("input_device", device);
            }}
            onAskBeforeRecordingChange={(value) => {
              void setKey("ask_before_recording", value ? "true" : "false");
            }}
          />
          <PeopleSection
            roster={config.roster}
            onAdd={(name) => {
              void bridge
                .call<string[]>("roster.add", { name })
                .then((roster) => setConfig((prev) => (prev === undefined ? prev : { ...prev, roster })));
            }}
            onRemove={(name) => {
              void bridge
                .call<string[]>("roster.remove", { name })
                .then((roster) => setConfig((prev) => (prev === undefined ? prev : { ...prev, roster })));
            }}
          />
          <NamingSection
            latestSession={config.latest_session}
            unnamed={unnamed}
            roster={config.roster}
            onName={(label, name) => {
              const session = config.latest_session;
              if (session === null) return;
              void bridge
                .call("speakers.name", { session, label, name })
                .then(() => loadUnnamed(session))
                .then((speakers) => {
                  if (speakers !== undefined) setUnnamed(speakers);
                });
            }}
          />
          <DiarizeSection
            diarize={config.diarize}
            threshold={config.threshold}
            onDiarizeChange={(value) => {
              void setKey("diarize", value ? "true" : "false");
            }}
            onThresholdChange={(value) => {
              void setKey("threshold", String(value));
            }}
          />
          <StorageSection
            sessionsDir={config.sessions_dir}
            defaultDir={config.default_dir}
            audioRetention={config.audio_retention}
            error={sessionsDirError}
            onChangeDir={() => {
              void bridge.call<{ chosen: string | null }>("pick_dir").then(({ chosen }) => {
                if (chosen === null) return;
                setSessionsDirError(undefined);
                return setKey("sessions_dir", chosen).catch((e: unknown) => {
                  setSessionsDirError(e instanceof Error ? e.message : String(e));
                });
              });
            }}
            onRetentionChange={(value) => {
              void setKey("audio_retention_days", value);
            }}
          />
        </>
      )}
    </div>
  );
}
