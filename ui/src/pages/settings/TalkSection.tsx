import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import type { TalkSettings } from "./types";

interface TalkSectionProps {
  talk: TalkSettings;
  onEnabledChange: (value: boolean) => void;
  onSpeakChange: (value: boolean) => void;
  onChooseHome: () => void;
}

export function TalkSection({
  talk,
  onEnabledChange,
  onSpeakChange,
  onChooseHome,
}: TalkSectionProps) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="talk-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">
        Talk to firstmate
      </h2>
      <div className="flex items-center justify-between gap-4">
        <div className="flex flex-col">
          <span className="text-sm">Talking mode</span>
          <span className="text-muted-foreground text-xs">
            Hold ⌥Space anywhere to speak to firstmate. While this is on, ⌥Space belongs to Ambient
            and types nothing in other apps. Paused while recording.
          </span>
        </div>
        <Switch
          aria-label="Talking mode"
          data-testid="talk-switch"
          checked={talk.enabled}
          onCheckedChange={onEnabledChange}
        />
      </div>
      <div
        className="flex items-center justify-between gap-4"
        style={{ opacity: talk.enabled ? 1 : 0.4 }}
      >
        <div className="flex flex-col">
          <span className="text-sm">Speak firstmate's replies</span>
          <span className="text-muted-foreground text-xs">
            Off, replies are shown as text only.
          </span>
        </div>
        <Switch
          aria-label="Speak firstmate's replies"
          data-testid="talk-speak-switch"
          disabled={!talk.enabled}
          checked={talk.speak}
          onCheckedChange={onSpeakChange}
        />
      </div>
      <div className="flex items-center justify-between gap-4">
        <div className="flex min-w-0 flex-col">
          <span className="text-sm">firstmate home</span>
          <span className="text-muted-foreground truncate text-xs" data-testid="talk-home">
            {talk.firstmate_home ?? "Not set: talking has nowhere to go."}
          </span>
        </div>
        <Button
          type="button"
          variant="outline"
          data-testid="talk-choose-home"
          onClick={onChooseHome}
        >
          Choose…
        </Button>
      </div>
      <p className="text-muted-foreground text-xs">
        What you say is transcribed on this Mac. Only the text goes to firstmate&apos;s inbox, and
        no audio is kept.
      </p>
    </section>
  );
}
