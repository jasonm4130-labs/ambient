import { useEffect, useRef, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useBridge } from "@/lib/bridge-context";
import { ExportMenu } from "./ExportMenu";
import { sessionState, type SessionSummary } from "./SessionList";

export interface SessionHeaderProps {
  summary: SessionSummary;
  /// Fired after any mutation resolves — rename, pin, tag, notes — so the
  /// caller re-requests `sessions` rather than this component keeping the
  /// reply's fields in local state. Matches `NamingStrip`'s shape.
  onChanged: () => void;
  /// Fired instead of `onChanged` once `session.delete` resolves: the
  /// caller must drop the selection *before* it refreshes, or `Transcript`
  /// re-requests a session that no longer exists.
  onDeleted?: () => void;
}

function formatDuration(seconds: number | null): string | null {
  if (seconds === null) return null;
  const total = Math.round(seconds);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}

/// The inline-editable name: `Enter` commits with `session.update {name}`,
/// `Escape` reverts to the prop. Draft state is resynced from `summary.name`
/// whenever it changes underneath — the only way the load-bearing "renders
/// from the `sessions` reply" test can pass.
function NameField({
  summary,
  disabled,
  onChanged,
}: {
  summary: SessionSummary;
  disabled: boolean;
  onChanged: () => void;
}) {
  const bridge = useBridge();
  const [draft, setDraft] = useState(summary.name ?? "");

  useEffect(() => {
    setDraft(summary.name ?? "");
  }, [summary.name]);

  return (
    <Input
      aria-label="Session name"
      data-testid="session-header-name"
      disabled={disabled}
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          void bridge.call("session.update", { session: summary.id, name: draft }).then(onChanged);
        } else if (e.key === "Escape") {
          e.preventDefault();
          setDraft(summary.name ?? "");
        }
      }}
    />
  );
}

/// Tag chips from `summary.tags` plus a one-tag-at-a-time add control — never
/// a whole-array `session.update`, so a tag the CLI added while the page was
/// open survives.
function TagsRow({
  summary,
  disabled,
  onChanged,
}: {
  summary: SessionSummary;
  disabled: boolean;
  onChanged: () => void;
}) {
  const bridge = useBridge();
  const [draft, setDraft] = useState("");

  return (
    <div className="flex flex-wrap items-center gap-1" data-testid="session-header-tags">
      {summary.tags.map((tag) => (
        <Badge key={tag} variant="secondary" className="gap-1">
          {tag}
          <button
            type="button"
            aria-label={`Remove tag ${tag}`}
            disabled={disabled}
            onClick={() => {
              void bridge
                .call("session.update", { session: summary.id, remove_tag: tag })
                .then(onChanged);
            }}
          >
            ×
          </button>
        </Badge>
      ))}
      <Input
        aria-label="Add tag"
        data-testid="session-header-add-tag"
        className="h-7 w-24"
        disabled={disabled}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== "Enter") return;
          e.preventDefault();
          const tag = draft.trim();
          if (tag === "") return;
          void bridge.call("session.update", { session: summary.id, add_tag: tag }).then(onChanged);
          setDraft("");
        }}
      />
    </div>
  );
}

/// Notes start from the saved metadata and save on blur. A failed save keeps
/// the draft available for retry instead of marking it as already saved.
function NotesField({
  summary,
  disabled,
  onChanged,
}: {
  summary: SessionSummary;
  disabled: boolean;
  onChanged: () => void;
}) {
  const bridge = useBridge();
  const [draft, setDraft] = useState(summary.notes ?? "");
  const saved = useRef(summary.notes ?? "");
  const [error, setError] = useState<string>();

  useEffect(() => {
    const next = summary.notes ?? "";
    const previous = saved.current;
    setDraft((current) => (current === previous ? next : current));
    saved.current = next;
  }, [summary.notes]);

  return (
    <div>
      <textarea
        aria-label="Notes"
        data-testid="session-header-notes"
        disabled={disabled}
        className="border-input placeholder:text-muted-foreground min-h-16 w-full rounded-md border bg-transparent px-3 py-1 text-sm shadow-xs outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50"
        placeholder="Notes"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={() => {
          if (draft === saved.current) return;
          setError(undefined);
          void bridge
            .call("session.update", { session: summary.id, notes: draft })
            .then(() => {
              saved.current = draft;
              onChanged();
            })
            .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
        }}
      />
      {error !== undefined && (
        <p role="alert" className="text-destructive text-sm">
          {error}
        </p>
      )}
    </div>
  );
}

/// The confirmation dialog naming the session and its duration, and the
/// `…` menu it hangs off of. Only one of the two is on screen at a time —
/// opening the dialog replaces the menu rather than layering over it — so
/// "Delete session…" (the menu item) and "Delete" (the dialog's confirm
/// button) never collide in the same query.
function DeleteConfirm({
  summary,
  onDeleted,
  onCancel,
}: {
  summary: SessionSummary;
  onDeleted: () => void;
  onCancel: () => void;
}) {
  const bridge = useBridge();
  const duration = formatDuration(summary.duration_s);
  const label = summary.name ?? summary.id;

  return (
    <div data-testid="session-header-delete-dialog" className="flex items-center gap-2 text-sm">
      <span>
        Delete &quot;{label}&quot;{duration !== null && ` (${duration})`}? This can&apos;t be
        undone.
      </span>
      <Button
        type="button"
        variant="destructive"
        size="sm"
        onClick={() => {
          void bridge.call("session.delete", { session: summary.id }).then(onDeleted);
        }}
      >
        Delete
      </Button>
      <Button type="button" variant="outline" size="sm" onClick={onCancel}>
        Cancel
      </Button>
    </div>
  );
}

function DeleteMenu({
  summary,
  disabled,
  onDeleted,
}: {
  summary: SessionSummary;
  disabled: boolean;
  onDeleted: () => void;
}) {
  const bridge = useBridge();
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);

  if (confirming) {
    return (
      <DeleteConfirm
        summary={summary}
        onDeleted={onDeleted}
        onCancel={() => setConfirming(false)}
      />
    );
  }

  return (
    <div className="relative">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        aria-label="More actions"
        data-testid="session-header-menu"
        onClick={() => setOpen((v) => !v)}
      >
        …
      </Button>
      {open && (
        <div className="bg-background absolute right-0 z-10 mt-1 flex flex-col gap-1 rounded-md border p-1 shadow-md">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            data-testid="session-header-reveal"
            onClick={() => {
              setOpen(false);
              void bridge.call("reveal", { session: summary.id });
            }}
          >
            Reveal in Finder
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={disabled}
            data-testid="session-header-delete-trigger"
            onClick={() => {
              setOpen(false);
              setConfirming(true);
            }}
          >
            Delete session…
          </Button>
        </div>
      )}
    </div>
  );
}

/// The bar above `Transcript`: rename, pin, tag, annotate and delete the
/// selected session. Issues zero bridge calls on mount — everything it
/// shows comes from the `summary` prop — and every mutation calls
/// `onChanged` (or, for delete, `onDeleted`) rather than updating local
/// state, so the page's "re-request after every mutation" rule holds here
/// too.
export function SessionHeader({ summary, onChanged, onDeleted }: SessionHeaderProps) {
  const bridge = useBridge();
  const disabled = sessionState(summary) === "live";

  return (
    <div className="flex flex-col gap-2 border-b p-3" data-testid="session-header">
      <div className="flex items-center gap-2">
        <NameField summary={summary} disabled={disabled} onChanged={onChanged} />
        <Button
          type="button"
          variant={summary.pinned ? "secondary" : "outline"}
          size="sm"
          aria-label="Pin"
          aria-pressed={summary.pinned}
          disabled={disabled}
          data-testid="session-header-pin"
          onClick={() => {
            void bridge
              .call("session.update", { session: summary.id, pinned: !summary.pinned })
              .then(onChanged);
          }}
        >
          {summary.pinned ? "Pinned" : "Pin"}
        </Button>
        <DeleteMenu summary={summary} disabled={disabled} onDeleted={onDeleted ?? onChanged} />
        {!disabled && <ExportMenu session={summary.id} />}
      </div>
      <TagsRow summary={summary} disabled={disabled} onChanged={onChanged} />
      <NotesField summary={summary} disabled={disabled} onChanged={onChanged} />
    </div>
  );
}
