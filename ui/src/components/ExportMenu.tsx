import { useCallback, useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import type { Bridge } from "@/lib/bridge";

interface ExportReply {
  session: string;
  format: string;
  text: string;
}

interface SaveReply {
  path: string | null;
}

/// The five `save` formats, in the order the "Save as…" submenu lists them.
/// "assistant" is deliberately absent: it is a copy-only format, never a
/// file on disk.
const SAVE_FORMATS: { format: string; label: string; testId: string }[] = [
  { format: "markdown", label: "Markdown", testId: "export-save-markdown" },
  { format: "text", label: "Plain text", testId: "export-save-text" },
  { format: "json", label: "JSON", testId: "export-save-json" },
  { format: "srt", label: "SRT", testId: "export-save-srt" },
  { format: "vtt", label: "VTT", testId: "export-save-vtt" },
];

function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/// "Copy for an assistant": `export {format:"assistant"}` then
/// `clipboard.write {text}`, the same two-call shape `Transcript`'s Copy
/// Markdown button uses.
function useCopyForAssistant(bridge: Bridge, session: string, onDone: (toast: string | null, error?: string) => void) {
  return useCallback(() => {
    void bridge
      .call<ExportReply>("export", { session, format: "assistant" })
      .then((exported) => bridge.call("clipboard.write", { text: exported.text }))
      .then(() => onDone("Copied"))
      .catch((e: unknown) => onDone(null, `Export failed: ${errorMessage(e)}`));
  }, [bridge, session, onDone]);
}

/// The five format buttons, shown once "Save as…" is expanded.
function SaveFormatList({ onPick }: { onPick: (format: string) => void }) {
  return (
    <div className="flex flex-col gap-1 border-t pt-1 pl-2">
      {SAVE_FORMATS.map(({ format, label, testId }) => (
        <Button
          key={format}
          type="button"
          variant="ghost"
          size="sm"
          className="w-full justify-start"
          data-testid={testId}
          onClick={() => onPick(format)}
        >
          {label}
        </Button>
      ))}
    </div>
  );
}

/// The submenu of file formats. Every path (Rust's `save`) runs the export
/// and writes the file itself, so the page issues exactly one bridge call.
function SaveSubmenu({
  session,
  onDone,
}: {
  session: string;
  onDone: (toast: string | null, error?: string) => void;
}) {
  const bridge = useBridge();
  const [open, setOpen] = useState(false);

  const save = useCallback(
    (format: string) => {
      void bridge
        .call<SaveReply>("save", { session, format })
        .then((reply) => onDone(reply.path === null ? null : `Saved to ${reply.path}`))
        .catch((e: unknown) => onDone(null, `Export failed: ${errorMessage(e)}`));
    },
    [bridge, session, onDone],
  );

  return (
    <div>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="w-full justify-start"
        data-testid="export-save-trigger"
        onClick={() => setOpen((v) => !v)}
      >
        Save as…
      </Button>
      {open && <SaveFormatList onPick={save} />}
    </div>
  );
}

/// The dropdown above `Transcript`'s Copy Markdown button: copy a session
/// for pasting into an assistant, or save it as a file through the native
/// `NSSavePanel`. Toast state lives here rather than in a provider — this is
/// the only place it is shown.
export function ExportMenu({ session }: { session: string }) {
  const bridge = useBridge();
  const [open, setOpen] = useState(false);
  const [toast, setToast] = useState<string>();
  const [error, setError] = useState<string>();

  const onDone = useCallback((message: string | null, err?: string) => {
    setToast(message ?? undefined);
    setError(err);
  }, []);

  const copyForAssistant = useCopyForAssistant(bridge, session, onDone);

  return (
    <div className="relative">
      <Button
        type="button"
        variant="outline"
        size="sm"
        data-testid="export-menu-trigger"
        onClick={() => setOpen((v) => !v)}
      >
        Export
      </Button>
      {open && (
        <div className="bg-background absolute right-0 z-10 mt-1 flex w-44 flex-col gap-1 rounded-md border p-1 shadow-md">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="w-full justify-start"
            data-testid="export-copy-assistant"
            onClick={copyForAssistant}
          >
            Copy for an assistant
          </Button>
          <SaveSubmenu session={session} onDone={onDone} />
        </div>
      )}
      {toast !== undefined && <p data-testid="export-toast">{toast}</p>}
      {error !== undefined && (
        <p role="alert" data-testid="export-error" className="text-destructive text-sm">
          {error}
        </p>
      )}
    </div>
  );
}
