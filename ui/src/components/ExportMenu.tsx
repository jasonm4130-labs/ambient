import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { useBridge } from "@/lib/bridge-context";
import type { Bridge } from "@/lib/bridge";
import { useDismiss } from "@/lib/dismiss";

/// How long a success toast stays up before it clears itself.
const TOAST_MS = 4000;

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
function useCopyForAssistant(
  bridge: Bridge,
  session: string,
  onDone: (toast: string | null, error?: string) => void,
) {
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
  onPick,
}: {
  session: string;
  onDone: (toast: string | null, error?: string) => void;
  onPick: () => void;
}) {
  const bridge = useBridge();
  const [open, setOpen] = useState(false);

  const save = useCallback(
    (format: string) => {
      onPick();
      void bridge
        .call<SaveReply>("save", { session, format })
        .then((reply) => onDone(reply.path === null ? null : `Saved to ${reply.path}`))
        .catch((e: unknown) => onDone(null, `Export failed: ${errorMessage(e)}`));
    },
    [bridge, session, onDone, onPick],
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
  const [toast, setToast] = useState<{ text: string }>();
  const [error, setError] = useState<string>();
  const root = useRef<HTMLDivElement>(null);
  const close = useCallback(() => setOpen(false), []);
  useDismiss(open, root, close);
  const dismissError = useCallback(() => setError(undefined), []);
  const start = useCallback(() => {
    close();
    dismissError();
  }, [close, dismissError]);

  const onDone = useCallback((message: string | null, err?: string) => {
    setToast(message === null ? undefined : { text: message });
    setError(err);
  }, []);

  useEffect(() => {
    if (toast === undefined) return;
    const timer = setTimeout(() => setToast(undefined), TOAST_MS);
    return () => clearTimeout(timer);
  }, [toast]);

  useEffect(() => {
    if (error === undefined) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") dismissError();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [error, dismissError]);

  const copyForAssistant = useCopyForAssistant(bridge, session, onDone);

  return (
    <div className="relative" ref={root}>
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
            onClick={() => {
              start();
              copyForAssistant();
            }}
          >
            Copy for an assistant
          </Button>
          <SaveSubmenu session={session} onDone={onDone} onPick={start} />
        </div>
      )}
      {!open && toast !== undefined && (
        <p
          role="status"
          data-testid="export-toast"
          className="bg-background absolute top-full right-0 z-10 mt-1 w-max max-w-80 truncate rounded-md border px-2 py-1 text-sm shadow-md"
          title={toast.text}
        >
          {toast.text}
        </p>
      )}
      {!open && error !== undefined && (
        <div className="bg-background absolute top-full right-0 z-10 mt-1 flex w-max max-w-80 items-start gap-2 rounded-md border px-2 py-1 text-sm shadow-md">
          <p role="alert" data-testid="export-error" className="text-destructive">
            {error}
          </p>
          <button
            type="button"
            aria-label="Dismiss export error"
            data-testid="export-error-dismiss"
            className="text-muted-foreground hover:text-foreground"
            onClick={dismissError}
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}
