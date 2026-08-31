import { useEffect, useRef } from "react";

/**
 * Top chrome: brand, quick-connect search, and the mockup's nav cluster.
 *
 * The search box is wired to the sidebar filter. "Bookmarks" and "Stack" have
 * no feature behind them yet and there are no user accounts, so those and the
 * avatar are inert placeholders -- kept for the layout the design calls for.
 */
export function AppHeader({
  query,
  onQuery,
  onNew,
  busy,
  sidebarOpen,
  onToggleSidebar,
  filesOpen,
  onToggleFiles,
  onOpenHistory,
}: {
  query: string;
  onQuery: (q: string) => void;
  onNew: () => void;
  busy: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  filesOpen: boolean;
  onToggleFiles: () => void;
  onOpenHistory?: () => void;
}) {
  const searchRef = useRef<HTMLInputElement>(null);

  // Cmd/Ctrl+K focuses search, matching the hint rendered in the field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <header className="appbar">
      {/* Panel glyph: an outlined rect with a filled rail, so open vs closed
          reads at a glance without swapping icons. */}
      <button
        className={`panel-toggle ${sidebarOpen ? "on" : ""}`}
        onClick={onToggleSidebar}
        title={`${sidebarOpen ? "Hide" : "Show"} connections (⌘B)`}
        aria-label="Toggle sidebar"
        aria-expanded={sidebarOpen}
      >
        <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
          <rect
            x="2"
            y="3"
            width="12"
            height="10"
            rx="2"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.4"
          />
          <path
            d="M6.4 3.4v9.2"
            stroke="currentColor"
            strokeWidth="1.4"
            strokeLinecap="round"
          />
        </svg>
      </button>

      <div className="brand">
        <span className="brand-mark" aria-hidden="true">
          <svg viewBox="0 0 16 16" width="15" height="15">
            <path
              d="M3 4.2 6.6 8 3 11.8"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
            <path
              d="M8.4 12h4.4"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            />
          </svg>
        </span>
        <span className="brand-name">Terminator</span>
      </div>

      <div className="quick">
        <span className="quick-icon" aria-hidden="true">
          <svg viewBox="0 0 16 16" width="13" height="13">
            <circle
              cx="7"
              cy="7"
              r="4.2"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.4"
            />
            <path
              d="m10.2 10.2 3 3"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
          </svg>
        </span>
        <input
          ref={searchRef}
          className="quick-input"
          type="text"
          value={query}
          placeholder="Quick connect - search hosts"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          onChange={(e) => onQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.stopPropagation();
              onQuery("");
            }
          }}
        />
        <kbd className="quick-kbd">⌘K</kbd>
      </div>

      <nav className="appnav">
        {/* Placeholders: the design calls for them, but neither has a feature
            behind it yet, so they are marked disabled rather than faked. */}
        <button className="navlink" disabled title="Not implemented yet">
          Bookmarks
        </button>
        <button className="navlink" onClick={onOpenHistory} title="Session recordings & command logs (OSC 133)">
          Recordings & Logs
        </button>
        {/* Mirror of the sidebar glyph, flipped so the rail sits on the right
            and the two toggles read as a matched pair. */}
        <button
          className={`panel-toggle ${filesOpen ? "on" : ""}`}
          onClick={onToggleFiles}
          title={`${filesOpen ? "Hide" : "Show"} files (⌘J)`}
          aria-label="Toggle file browser"
          aria-expanded={filesOpen}
        >
          <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
            <rect
              x="2"
              y="3"
              width="12"
              height="10"
              rx="2"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.4"
            />
            <path
              d="M9.6 3.4v9.2"
              stroke="currentColor"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
          </svg>
        </button>
        <button className="primary sm" onClick={onNew} disabled={busy}>
          New Session
        </button>
        <span className="avatar" title="Local user" aria-hidden="true">
          T
        </span>
      </nav>
    </header>
  );
}
