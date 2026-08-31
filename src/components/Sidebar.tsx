import { useState } from "react";
import type { Profile } from "../lib/api";
import {
  KIND_LABEL,
  KIND_ORDER,
  connectBlockedReason,
  describeTarget,
  kindBadge,
  type Kind,
} from "../lib/transport";

/**
 * Icons share one stroke style so the row reads as a set. `currentColor`
 * lets each button colour its own icon via CSS.
 */
const stroke = {
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.4,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

/** Connect: a play triangle, the clearest "start this" at 14px. */
function PlayIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path d="M5.5 3.4 12 8l-6.5 4.6z" fill="currentColor" />
    </svg>
  );
}

/** Disconnect: the standard power glyph, rather than an ambiguous square. */
function PowerIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path {...stroke} d="M8 2.4v5" />
      <path {...stroke} d="M4.9 4.8a4.4 4.4 0 1 0 6.2 0" />
    </svg>
  );
}

/** Disclosure caret; rotated by CSS so open/closed share one glyph. */
function CaretIcon() {
  return (
    <svg viewBox="0 0 16 16" width="11" height="11" aria-hidden="true">
      <path {...stroke} d="m6 3.5 5 4.5-5 4.5" />
    </svg>
  );
}

function TrashIcon() {
  return (
    <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
      <path {...stroke} d="M2.9 4.3h10.2" />
      <path {...stroke} d="M6.3 4.3V3.1a.8.8 0 0 1 .8-.8h1.8a.8.8 0 0 1 .8.8v1.2" />
      <path {...stroke} d="m4.5 4.3.6 8a1 1 0 0 0 1 .9h3.8a1 1 0 0 0 1-.9l.6-8" />
      <path {...stroke} d="M6.8 6.6v4.2M9.2 6.6v4.2" />
    </svg>
  );
}

/**
 * Saved connections, grouped by transport.
 *
 * Clicking a row *selects* it and shows its details; it deliberately does not
 * connect. Connecting is an explicit action -- the inline plug icon, or the
 * button in the profile view -- so a stray click cannot open a session on a
 * production host.
 */
export function Sidebar({
  profiles,
  selectedId,
  connectedIds,
  query,
  open,
  onSelect,
  onConnect,
  onDisconnect,
  onDelete,
  onNew,
  onOpenTunnels,
  onOpenSnippets,
  onOpenKnownHosts,
  onOpenEditor,
  onOpenTriggers,
  onOpenBackup,
  busy,
}: {
  profiles: Profile[];
  selectedId: string | null;
  connectedIds: Set<string>;
  query: string;
  open: boolean;
  onSelect: (p: Profile) => void;
  onConnect: (p: Profile) => void;
  onDisconnect: (p: Profile) => void;
  onDelete: (p: Profile) => void;
  onNew: () => void;
  onOpenTunnels?: () => void;
  onOpenSnippets?: () => void;
  onOpenKnownHosts?: () => void;
  onOpenEditor?: () => void;
  onOpenTriggers?: () => void;
  onOpenBackup?: () => void;
  busy: boolean;
}) {
  const [collapsed, setCollapsed] = useState<Set<Kind>>(new Set());

  const q = query.trim().toLowerCase();
  const matches = q
    ? profiles.filter(
        (p) =>
          p.name.toLowerCase().includes(q) ||
          describeTarget(p.spec).toLowerCase().includes(q),
      )
    : profiles;

  const byKind = new Map<Kind, Profile[]>();
  for (const p of matches) {
    const k = p.spec.kind;
    if (!byKind.has(k)) byKind.set(k, []);
    byKind.get(k)!.push(p);
  }

  const toggle = (k: Kind) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(k)) next.add(k);
      return next;
    });

  return (
    // `inert` keeps Tab focus out of a sidebar that is slid off-screen. Without
    // it the collapsed panel still swallows keystrokes meant for the terminal.
    <aside className="sidebar" inert={!open ? true : undefined} aria-hidden={!open}>
      <div className="side-head">
        <span>Connections</span>
        <button className="icon-btn" onClick={onNew} disabled={busy} title="New connection" aria-label="New connection">
          +
        </button>
      </div>

      <div className="side-list">
        {profiles.length === 0 && (
          <p className="hint pad">
            No saved connections yet. Use <b>New Session</b> to add an SSH or
            RDP host.
          </p>
        )}

        {profiles.length > 0 && matches.length === 0 && (
          <p className="hint pad">No hosts match “{query.trim()}”.</p>
        )}

        {KIND_ORDER.filter((k) => byKind.has(k)).map((kind) => {
          const items = byKind.get(kind)!;
          // A filtered-down group is always expanded: hiding the only results
          // behind a caret would make the search look broken.
          const open = q !== "" || !collapsed.has(kind);
          return (
            <div key={kind} className="group">
              <button
                className={`group-name ${open ? "open" : ""}`}
                onClick={() => toggle(kind)}
                aria-expanded={open}
              >
                <CaretIcon />
                <span className="group-label">{KIND_LABEL[kind]}</span>
                <span className="group-count">{items.length}</span>
              </button>

              {open && (
                <ul className="group-items">
                  {items.map((p) => {
                    const connected = connectedIds.has(p.id);
                    const blocked = connectBlockedReason(p.spec);
                    return (
                      <li
                        key={p.id}
                        className={`profile ${p.id === selectedId ? "selected" : ""}`}
                        onClick={() => onSelect(p)}
                        title={describeTarget(p.spec)}
                      >
                        <span
                          className={`live-dot ${connected ? "on" : ""}`}
                          title={connected ? "Connected" : "Idle"}
                        />

                        <span className="profile-name">{p.name}</span>

                        <span className={`badge ${p.spec.kind}`}>
                          {kindBadge(p.spec.kind)}
                        </span>

                        <span className="row-actions">
                          <button
                            className={`row-action ${connected ? "stop" : ""}`}
                            title={
                              connected
                                ? `Disconnect ${p.name}`
                                : (blocked ?? `Connect to ${p.name}`)
                            }
                            aria-label={connected ? "Disconnect" : "Connect"}
                            disabled={!connected && blocked !== null}
                            onClick={(e) => {
                              // Without this the row's own onClick fires too
                              // and the selection flickers under the action.
                              e.stopPropagation();
                              if (connected) onDisconnect(p);
                              else onConnect(p);
                            }}
                          >
                            {connected ? <PowerIcon /> : <PlayIcon />}
                          </button>

                          <button
                            className="row-action danger"
                            // Deleting a live profile would strand its tab.
                            disabled={connected}
                            title={
                              connected
                                ? "Disconnect before deleting"
                                : `Delete ${p.name}`
                            }
                            aria-label="Delete"
                            onClick={(e) => {
                              e.stopPropagation();
                              onDelete(p);
                            }}
                          >
                            <TrashIcon />
                          </button>
                        </span>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          );
        })}
      </div>

      <div className="side-foot" style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
        {onOpenEditor && (
          <button
            className="side-foot-btn"
            onClick={onOpenEditor}
            title="Open in-app Mini-IDE Code Editor (⌘E)"
          >
            <span style={{ fontSize: "13px" }}>📝</span>
            <span>Remote Mini-IDE</span>
          </button>
        )}
        {onOpenSnippets && (
          <button
            className="side-foot-btn"
            onClick={onOpenSnippets}
            title="Open Snippets & Command Templates Library"
          >
            <span style={{ fontSize: "13px" }}>⚡</span>
            <span>Snippets Library</span>
          </button>
        )}
        {onOpenTriggers && (
          <button
            className="side-foot-btn"
            onClick={onOpenTriggers}
            title="Terminal Output Triggers & Desktop Alerts"
          >
            <span style={{ fontSize: "13px" }}>🔔</span>
            <span>Triggers & Alerts</span>
          </button>
        )}
        {onOpenBackup && (
          <button
            className="side-foot-btn"
            onClick={onOpenBackup}
            title="Import / Export Profiles, Snippets, Tunnels Backup (JSON)"
          >
            <span style={{ fontSize: "13px" }}>📦</span>
            <span>Backup & Restore</span>
          </button>
        )}
        {onOpenKnownHosts && (
          <button
            className="side-foot-btn"
            onClick={onOpenKnownHosts}
            title="Inspect & manage trusted SSH known hosts"
          >
            <span style={{ fontSize: "13px" }}>🛡️</span>
            <span>SSH Known Hosts</span>
          </button>
        )}
        {onOpenTunnels && (
          <button
            className="side-foot-btn"
            onClick={onOpenTunnels}
            title="Open SSH Port Forwarding & Tunnels Manager"
          >
            <svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="1.4">
              <circle cx="4" cy="8" r="2.5" />
              <circle cx="12" cy="8" r="2.5" />
              <path d="M6.5 8h3" strokeDasharray="1 1" />
            </svg>
            <span>SSH Port Tunnels</span>
          </button>
        )}
      </div>
    </aside>
  );
}
