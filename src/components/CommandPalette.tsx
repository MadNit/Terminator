import { useEffect, useState, useMemo, useRef } from "react";
import {
  listSnippets,
  listProfiles,
  type Snippet,
  type Profile,
} from "../lib/api";
import { extractPlaceholders, evaluateSnippet } from "./SnippetManagerModal";

export type PaletteAction =
  | {
      id: string;
      type: "action";
      title: string;
      subtitle: string;
      shortcut?: string;
      icon: string;
      perform: () => void;
    }
  | {
      id: string;
      type: "profile";
      title: string;
      subtitle: string;
      icon: string;
      profile: Profile;
      perform: () => void;
    }
  | {
      id: string;
      type: "snippet";
      title: string;
      subtitle: string;
      icon: string;
      snippet: Snippet;
      perform: () => void;
    };

export function CommandPalette({
  onClose,
  actions,
  onConnectProfile,
  onRunSnippet,
}: {
  onClose: () => void;
  actions: {
    id: string;
    title: string;
    subtitle: string;
    shortcut?: string;
    icon: string;
    perform: () => void;
  }[];
  onConnectProfile?: (profile: Profile) => void;
  onRunSnippet?: (command: string) => void;
}) {
  const [query, setQuery] = useState("");
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [runParamModal, setRunParamModal] = useState<{
    snippet: Snippet;
    params: string[];
    values: Record<string, string>;
  } | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    listSnippets().then(setSnippets).catch(() => {});
    listProfiles().then(setProfiles).catch(() => {});
  }, []);

  const allItems = useMemo<PaletteAction[]>(() => {
    const items: PaletteAction[] = [];

    // 1. Built-in actions
    for (const a of actions) {
      items.push({
        id: `action-${a.id}`,
        type: "action",
        title: a.title,
        subtitle: a.subtitle,
        shortcut: a.shortcut,
        icon: a.icon,
        perform: () => {
          a.perform();
          onClose();
        },
      });
    }

    // 2. Profiles
    for (const p of profiles) {
      let subtitle = "Local Shell";
      let icon = "💻";
      if (p.spec.kind === "ssh") {
        subtitle = `SSH: ${p.spec.user}@${p.spec.host}:${p.spec.port}`;
        icon = "🌐";
      } else if (p.spec.kind === "rdp") {
        subtitle = `RDP: ${p.spec.user}@${p.spec.host}:${p.spec.port}`;
        icon = "🖥️";
      }
      items.push({
        id: `profile-${p.id}`,
        type: "profile",
        title: `Connect: ${p.name}`,
        subtitle,
        icon,
        profile: p,
        perform: () => {
          if (onConnectProfile) {
            onConnectProfile(p);
            onClose();
          }
        },
      });
    }

    // 3. Snippets
    for (const s of snippets) {
      items.push({
        id: `snippet-${s.id}`,
        type: "snippet",
        title: `Snippet: ${s.title}`,
        subtitle: s.command,
        icon: "⚡",
        snippet: s,
        perform: () => {
          const placeholders = extractPlaceholders(s.command);
          if (placeholders.length > 0) {
            const initialValues: Record<string, string> = {};
            placeholders.forEach((pl) => (initialValues[pl] = ""));
            setRunParamModal({
              snippet: s,
              params: placeholders,
              values: initialValues,
            });
          } else {
            if (onRunSnippet) {
              onRunSnippet(s.command);
              onClose();
            }
          }
        },
      });
    }

    return items;
  }, [actions, profiles, snippets, onConnectProfile, onRunSnippet, onClose]);

  const filtered = useMemo(() => {
    const q = query.toLowerCase().trim();
    if (!q) return allItems;
    return allItems.filter((it) => {
      return (
        it.title.toLowerCase().includes(q) ||
        it.subtitle.toLowerCase().includes(q)
      );
    });
  }, [allItems, query]);

  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((prev) => (prev + 1) % (filtered.length || 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((prev) => (prev - 1 + filtered.length) % (filtered.length || 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (filtered[selectedIndex]) {
        filtered[selectedIndex].perform();
      }
    }
  };

  return (
    <div className="command-palette-backdrop" onClick={onClose}>
      <div
        className="command-palette-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="palette-search-row">
          <span style={{ fontSize: "15px", color: "var(--dim)", display: "flex" }}>🔍</span>
          <input
            ref={inputRef}
            type="text"
            className="palette-search-input"
            placeholder="Type a command, snippet, profile, or action..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          <kbd className="quick-kbd">ESC</kbd>
        </div>

        <div ref={listRef} className="palette-items-list">
          {filtered.length === 0 ? (
            <div style={{ padding: "28px", textAlign: "center", color: "var(--dim)", fontSize: "12.5px" }}>
              No matching commands or snippets
            </div>
          ) : (
            filtered.map((item, idx) => {
              const isSelected = idx === selectedIndex;
              return (
                <div
                  key={item.id}
                  className={`palette-item ${isSelected ? "active" : ""}`}
                  onMouseEnter={() => setSelectedIndex(idx)}
                  onClick={() => item.perform()}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: "10px", minWidth: 0 }}>
                    <span style={{ fontSize: "15px", flexShrink: 0 }}>{item.icon}</span>
                    <div style={{ minWidth: 0 }}>
                      <div style={{ fontSize: "12.5px", fontWeight: 500, color: "var(--fg)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {item.title}
                      </div>
                      <div
                        style={{
                          fontSize: "11px",
                          color: isSelected ? "var(--muted)" : "var(--dim)",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                          fontFamily: item.type === "snippet" ? "var(--mono)" : "inherit",
                        }}
                      >
                        {item.subtitle}
                      </div>
                    </div>
                  </div>

                  {item.type === "action" && item.shortcut && (
                    <kbd className="quick-kbd">
                      {item.shortcut}
                    </kbd>
                  )}

                  {item.type === "snippet" && (
                    <span className="snippet-placeholder-tag">
                      Snippet
                    </span>
                  )}

                  {item.type === "profile" && (
                    <span className="tunnel-badge dynamic">
                      Profile
                    </span>
                  )}
                </div>
              );
            })
          )}
        </div>

        <div className="palette-footer">
          <span>Use <kbd className="quick-kbd" style={{ padding: "0 3px" }}>↑</kbd> <kbd className="quick-kbd" style={{ padding: "0 3px" }}>↓</kbd> to navigate, <kbd className="quick-kbd" style={{ padding: "0 4px" }}>Enter</kbd> to select</span>
          <span style={{ color: "var(--lime)", fontWeight: 500 }}>⚡ Terminator Palette</span>
        </div>

        {runParamModal && (
          <div className="modal-backdrop" style={{ zIndex: 300 }} onClick={() => setRunParamModal(null)}>
            <div className="modal snippet-param-modal" onClick={(e) => e.stopPropagation()}>
              <h2 style={{ margin: 0 }}>Evaluate Snippet Parameters</h2>
              <p style={{ margin: 0, fontSize: "12px", color: "var(--muted)" }}>
                Provide values for parameters in <strong>{runParamModal.snippet.title}</strong>:
              </p>
              <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
                {runParamModal.params.map((p) => (
                  <label key={p} style={{ margin: 0 }}>
                    <span style={{ fontWeight: 600, color: "var(--fg)" }}>{p}:</span>
                    <input
                      type="text"
                      autoFocus={runParamModal.params[0] === p}
                      placeholder={`Value for {{${p}}}`}
                      value={runParamModal.values[p] || ""}
                      onChange={(e) => {
                        const val = e.target.value;
                        setRunParamModal((prev) =>
                          prev
                            ? { ...prev, values: { ...prev.values, [p]: val } }
                            : null,
                        );
                      }}
                    />
                  </label>
                ))}
              </div>
              <div className="snippet-code-box" style={{ margin: "2px 0" }}>
                <strong style={{ color: "var(--dim)" }}>Command: </strong>
                {evaluateSnippet(runParamModal.snippet.command, runParamModal.values)}
              </div>
              <div className="actions">
                <button type="button" onClick={() => setRunParamModal(null)}>
                  Cancel
                </button>
                <button
                  type="button"
                  className="primary"
                  onClick={() => {
                    const finalCmd = evaluateSnippet(
                      runParamModal.snippet.command,
                      runParamModal.values,
                    );
                    if (onRunSnippet) {
                      onRunSnippet(finalCmd);
                      setRunParamModal(null);
                      onClose();
                    }
                  }}
                >
                  Run Command
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
