interface SidebarFiltersProps {
  query: string;
  tag: string | null;
  tags: string[];
  onQuery: (query: string) => void;
  onTag: (tag: string | null) => void;
}

export function SidebarFilters({ query, tag, tags, onQuery, onTag }: SidebarFiltersProps) {
  return (
    <div className="flex shrink-0 flex-col gap-2 p-2" data-testid="sidebar-filters">
      <input
        type="search"
        aria-label="Filter sessions"
        placeholder="Filter sessions…"
        className="border-input w-full min-w-0 rounded-md border bg-transparent px-2 py-1"
        value={query}
        onChange={(event) => onQuery(event.target.value)}
      />
      {tags.length > 0 && (
        <div className="flex gap-1 overflow-x-auto" aria-label="Filter by tag">
          {tags.map((name) => (
            <button
              key={name}
              type="button"
              aria-label={`Filter tag ${name}`}
              aria-pressed={tag === name}
              className="aria-pressed:bg-sidebar-accent shrink-0 rounded border px-2 py-1 text-xs"
              onClick={() => onTag(tag === name ? null : name)}
            >
              {name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
