import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import {
  LAUNCH,
  PROMPT,
  listenerLine,
  type TalkListener,
  type TalkPayload,
} from "@/components/talk/text";
import { useBridge } from "@/lib/bridge-context";
import type { TalkSettings } from "./types";

interface TalkSectionProps {
  talk: TalkSettings;
  onEnabledChange: (value: boolean) => void;
  onSpeakChange: (value: boolean) => void;
}

/// The launch line, copyable. The clipboard API may be missing in the
/// app's web view, so a failed copy selects the line for ⌘C instead.
function CopyLine({ text, label }: { text: string; label: string }) {
  const [said, setSaid] = useState<string>();
  const line = useRef<HTMLElement>(null);
  const copy = () => {
    const select = () => {
      const target = line.current;
      if (target === null) return;
      const range = document.createRange();
      range.selectNodeContents(target);
      window.getSelection()?.removeAllRanges();
      window.getSelection()?.addRange(range);
      setSaid("Selected: press ⌘C to copy.");
    };
    if (navigator.clipboard === undefined) {
      select();
      return;
    }
    navigator.clipboard.writeText(text).then(() => setSaid("Copied."), select);
  };
  return (
    <div className="flex min-w-0 items-center gap-2">
      <code
        ref={line}
        className="bg-muted min-w-0 flex-1 truncate rounded px-2 py-1 font-mono text-xs select-all"
        data-testid="talk-launch"
        title={text}
      >
        {text}
      </code>
      <Button type="button" variant="outline" size="sm" aria-label={label} onClick={copy}>
        Copy
      </Button>
      <span role="status" className="text-muted-foreground sr-only">
        {said}
      </span>
    </div>
  );
}

/// One switch with its name and a line saying what it does.
function SwitchRow({
  name,
  help,
  testId,
  checked,
  disabled = false,
  onChange,
}: {
  name: string;
  help: string;
  testId: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <div
      className="flex items-center justify-between gap-4"
      style={{ opacity: disabled ? 0.4 : 1 }}
    >
      <div className="flex flex-col">
        <span className="text-sm">{name}</span>
        <span className="text-muted-foreground text-xs">{help}</span>
      </div>
      <Switch
        aria-label={name}
        data-testid={testId}
        disabled={disabled}
        checked={checked}
        onCheckedChange={onChange}
      />
    </div>
  );
}

/// Who will hear what is said, and how to start a session that will. The
/// listener comes from the window's `talk` event, which the menu bar sends
/// as it reads the heartbeat; `undefined` means none has arrived yet.
function WhoHears() {
  const bridge = useBridge();
  const [listener, setListener] = useState<TalkListener | null>();
  useEffect(
    () => bridge.on("talk", (payload) => setListener((payload as TalkPayload).listener ?? null)),
    [bridge],
  );
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <span className="text-sm">Who hears it</span>
      <p className="text-muted-foreground text-xs" data-testid="talk-listener-status">
        {listener === undefined ? "Checking…" : listenerLine(listener)}
      </p>
      <ol className="text-muted-foreground flex list-decimal flex-col gap-1 pl-4 text-xs">
        <li>
          Start Claude Code with Ambient as a channel (a research preview):
          <CopyLine text={LAUNCH} label="Copy the launch command" />
        </li>
        <li>
          In that session, run <code className="font-mono">{PROMPT}</code>. The last session to run
          it is the one that hears.
        </li>
      </ol>
      <p className="text-muted-foreground text-xs">
        Another agent, or a session started without the flag, can run{" "}
        <code className="font-mono">{PROMPT} wait</code> to wait for questions in a loop instead.
      </p>
    </div>
  );
}

/// Settings → Talk: talking mode, speaking replies, and who will hear what
/// is said.
export function TalkSection({ talk, onEnabledChange, onSpeakChange }: TalkSectionProps) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border p-4" data-testid="talk-section">
      <h2 className="text-muted-foreground text-xs font-semibold tracking-wide uppercase">Talk</h2>
      <SwitchRow
        name="Talking mode"
        help="Hold ⌥Space anywhere to speak to the agent session that is listening. While this is on, ⌥Space belongs to Ambient and types nothing in other apps. Paused while recording."
        testId="talk-switch"
        checked={talk.enabled}
        onChange={onEnabledChange}
      />
      <SwitchRow
        name="Speak replies"
        help="Reads the first paragraph of each reply aloud. Off, replies are shown as text only."
        testId="talk-speak-switch"
        checked={talk.speak}
        disabled={!talk.enabled}
        onChange={onSpeakChange}
      />
      <WhoHears />
      <p className="text-muted-foreground text-xs">
        What you say is transcribed on this Mac. Only the text goes to the agent, and no audio is
        kept. The question waits in Ambient&apos;s settings folder until it is answered or expires.
      </p>
    </section>
  );
}
