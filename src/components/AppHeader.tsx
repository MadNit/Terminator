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
  onOpenTunnels,
  onOpenSnippets,
  onOpenKnownHosts,
  onOpenEditor,
  onOpenTriggers,
  onOpenBackup,
  onOpenMonitor,
  onOpenBatchRunner,
  onOpenThemes,
  splitLayout,
  onSplitLayout,
  broadcast,
  onToggleBroadcast,
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
  onOpenTunnels?: () => void;
  onOpenSnippets?: () => void;
  onOpenKnownHosts?: () => void;
  onOpenEditor?: () => void;
  onOpenTriggers?: () => void;
  onOpenBackup?: () => void;
  onOpenMonitor?: () => void;
  onOpenBatchRunner?: () => void;
  onOpenThemes?: () => void;
  splitLayout?: "1x1" | "1x2" | "2x1" | "2x2";
  onSplitLayout?: (layout: "1x1" | "1x2" | "2x1" | "2x2") => void;
  broadcast?: boolean;
  onToggleBroadcast?: () => void;
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
        <button className="navlink" onClick={onOpenMonitor} title="Remote System Resource Monitor (CPU, RAM, Disk, Processes)">
          📊 Monitor
        </button>
        <button className="navlink" onClick={onOpenBatchRunner} title="Multi-Host Batch Command Execution">
          🚀 Batch
        </button>
        <button className="navlink" onClick={onOpenThemes} title="Terminal Themes & Font Customizer">
          🎨 Themes
        </button>
        <button className="navlink" onClick={onOpenEditor} title="Open in-app Mini-IDE Code Editor (⌘E)">
          📝 Mini-IDE
        </button>
        <button className="navlink" onClick={onOpenSnippets} title="Command snippets library & templates (⌘P / Quick Run)">
          ⚡ Snippets
        </button>
        <button className="navlink" onClick={onOpenTriggers} title="Terminal Output Triggers & Desktop Alerts">
          🔔 Alerts
        </button>
        <button className="navlink" onClick={onOpenBackup} title="Import & Export Profiles, Tunnels, Snippets (JSON)">
          📦 Backup
        </button>
        <button className="navlink" onClick={onOpenKnownHosts} title="SSH Known Hosts & Server Key Fingerprints">
          🛡️ Known Hosts
        </button>
        <button className="navlink" onClick={onOpenHistory} title="Session recordings & command logs (OSC 133)">
          Recordings & Logs
        </button>
        <button className="navlink prominent" onClick={onOpenTunnels} title="SSH Port Forwarding & Tunnel Manager (-L, -R, -D)">
          ⚡ Port Tunnels
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
        {/* Split layout selector */}
        {onSplitLayout && (
          <div className="split-controls" title="Split Pane Layout">
            <button
              className={`split-btn ${splitLayout === "1x1" ? "active" : ""}`}
              onClick={() => onSplitLayout("1x1")}
              title="Single Pane (1x1)"
            >
              <svg viewBox="0 0 16 16" width="13" height="13">
                <rect x="2" y="2" width="12" height="12" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
              </svg>
            </button>
            <button
              className={`split-btn ${splitLayout === "1x2" ? "active" : ""}`}
              onClick={() => onSplitLayout("1x2")}
              title="Split Vertical (2 columns)"
            >
              <svg viewBox="0 0 16 16" width="13" height="13">
                <rect x="2" y="2" width="12" height="12" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
                <path d="M8 2v12" stroke="currentColor" strokeWidth="1.3" />
              </svg>
            </button>
            <button
              className={`split-btn ${splitLayout === "2x1" ? "active" : ""}`}
              onClick={() => onSplitLayout("2x1")}
              title="Split Horizontal (2 rows)"
            >
              <svg viewBox="0 0 16 16" width="13" height="13">
                <rect x="2" y="2" width="12" height="12" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
                <path d="M2 8h12" stroke="currentColor" strokeWidth="1.3" />
              </svg>
            </button>
            <button
              className={`split-btn ${splitLayout === "2x2" ? "active" : ""}`}
              onClick={() => onSplitLayout("2x2")}
              title="Quad Grid (2x2)"
            >
              <svg viewBox="0 0 16 16" width="13" height="13">
                <rect x="2" y="2" width="12" height="12" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.3" />
                <path d="M8 2v12M2 8h12" stroke="currentColor" strokeWidth="1.3" />
              </svg>
            </button>
          </div>
        )}

        {/* Multi-Exec / Broadcast toggle */}
        {onToggleBroadcast && (
          <button
            className={`broadcast-toggle ${broadcast ? "active" : ""}`}
            onClick={onToggleBroadcast}
            title={broadcast ? "Disable Broadcast input mode" : "Enable Multi-Exec / Broadcast input mode (type to all sessions simultaneously)"}
          >
            <span className="broadcast-icon">⚡</span>
            <span>Multi-Exec</span>
          </button>
        )}

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
