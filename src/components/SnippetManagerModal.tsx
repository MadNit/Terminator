import { useEffect, useState, useMemo } from "react";
import { listSnippets, saveSnippet, deleteSnippet, type Snippet } from "../lib/api";

export function extractPlaceholders(command: string): string[] {
  const matches = command.match(/\{\{([a-zA-Z0-9_-]+)\}\}/g);
  if (!matches) return [];
  const set = new Set<string>();
  for (const m of matches) {
    set.add(m.replace(/^\{\{/, "").replace(/\}\}$/, ""));
  }
  return Array.from(set);
}

export function evaluateSnippet(command: string, values: Record<string, string>): string {
  let out = command;
  for (const [key, val] of Object.entries(values)) {
    const re = new RegExp(`\\{\\{${key}\\}\\}`, "g");
    out = out.replace(re, val);
  }
  return out;
}

export function SnippetManagerModal({
  onClose,
  onRunSnippet,
}: {
  onClose: () => void;
  onRunSnippet?: (command: string) => void;
}) {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [search, setSearch] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<string>("All");
  const [editing, setEditing] = useState<Snippet | null>(null);
  const [runParamModal, setRunParamModal] = useState<{
    snippet: Snippet;
    params: string[];
    values: Record<string, string>;
  } | null>(null);

  const refresh = async () => {
    try {
      const list = await listSnippets();
      setSnippets(list);
    } catch (e) {
      console.error("Failed loading snippets:", e);
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const categories = useMemo(() => {
    const set = new Set<string>();
    for (const s of snippets) {
      if (s.category && s.category.trim()) {
        set.add(s.category.trim());
      }
    }
    return ["All", ...Array.from(set).sort()];
  }, [snippets]);

  const filtered = useMemo(() => {
    const q = search.toLowerCase().trim();
    return snippets.filter((s) => {
      if (selectedCategory !== "All" && s.category !== selectedCategory) {
        return false;
      }
      if (!q) return true;
      return (
        s.title.toLowerCase().includes(q) ||
        s.command.toLowerCase().includes(q) ||
        (s.description && s.description.toLowerCase().includes(q)) ||
        s.tags.some((t) => t.toLowerCase().includes(q))
      );
    });
  }, [snippets, search, selectedCategory]);

  const handleExecute = (s: Snippet) => {
    const placeholders = extractPlaceholders(s.command);
    if (placeholders.length > 0) {
      const initialValues: Record<string, string> = {};
      placeholders.forEach((p) => (initialValues[p] = ""));
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
  };

  const openNewSnippet = () => {
    setEditing({
      id: crypto.randomUUID(),
      title: "",
      command: "",
      category: selectedCategory !== "All" ? selectedCategory : "General",
      description: "",
      tags: [],
    });
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal snippet-manager-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="history-modal-header">
          <div className="history-tabs">
            <span style={{ fontWeight: 600, fontSize: "13.5px", color: "var(--fg)", display: "flex", alignItems: "center", gap: "6px" }}>
              <span>⚡</span> Command & Snippets Library
            </span>
          </div>
          <button className="icon-btn" onClick={onClose} title="Close">
            ✕
          </button>
        </div>

        <div className="snippet-modal-body">
          <div className="tunnel-toolbar">
            <div style={{ display: "flex", alignItems: "center", gap: "10px", flex: 1, marginRight: "12px" }}>
              <input
                type="text"
                className="snippet-search-input"
                placeholder="Search snippets by name, command, or tag..."
                value={search}
                onChange={(e) => setSearch(e.target.value)}
              />
              <div className="snippet-category-chips">
                {categories.map((cat) => (
                  <button
                    key={cat}
                    className={`snippet-chip ${selectedCategory === cat ? "active" : ""}`}
                    onClick={() => setSelectedCategory(cat)}
                  >
                    {cat}
                  </button>
                ))}
              </div>
            </div>
            <button className="primary sm" onClick={openNewSnippet}>
              + New Snippet
            </button>
          </div>

          <div className="snippets-grid">
            {filtered.length === 0 ? (
              <div className="tunnels-empty" style={{ gridColumn: "1 / -1" }}>
                <div style={{ fontSize: "28px", marginBottom: "8px" }}>📝</div>
                <p>No snippets found.</p>
                <p className="tunnels-empty-hint">
                  Create reusable commands with parameter placeholders like <code style={{ color: "var(--lime)" }}>{"{{variable}}"}</code> to run in terminal sessions.
                </p>
                <button
                  className="btn btn-secondary"
                  style={{ marginTop: "12px" }}
                  onClick={openNewSnippet}
                >
                  Create your first snippet
                </button>
              </div>
            ) : (
              filtered.map((s) => {
                const placeholders = extractPlaceholders(s.command);
                return (
                  <div key={s.id} className="snippet-card">
                    <div>
                      <div className="snippet-card-top">
                        <div className="snippet-title-row">
                          <span className="snippet-title">{s.title}</span>
                          {s.category && (
                            <span className="snippet-category-badge">{s.category}</span>
                          )}
                        </div>
                        <div style={{ display: "flex", gap: "4px" }}>
                          <button
                            className="btn btn-ghost"
                            style={{ padding: "2px 6px", fontSize: "11px" }}
                            title="Edit snippet"
                            onClick={() => setEditing(s)}
                          >
                            ✏️
                          </button>
                          <button
                            className="btn btn-ghost"
                            style={{ padding: "2px 6px", fontSize: "11px", color: "var(--danger)" }}
                            title="Delete snippet"
                            onClick={async () => {
                              if (confirm(`Delete snippet "${s.title}"?`)) {
                                await deleteSnippet(s.id);
                                refresh();
                              }
                            }}
                          >
                            🗑️
                          </button>
                        </div>
                      </div>

                      {s.description && (
                        <p style={{ margin: "4px 0 0 0", fontSize: "11.5px", color: "var(--dim)" }}>
                          {s.description}
                        </p>
                      )}

                      <div className="snippet-code-box">
                        {s.command}
                      </div>

                      {placeholders.length > 0 && (
                        <div className="snippet-placeholders" style={{ marginBottom: "6px" }}>
                          {placeholders.map((p) => (
                            <span key={p} className="snippet-placeholder-tag">
                              {"{{"} {p} {"}}"}
                            </span>
                          ))}
                        </div>
                      )}
                    </div>

                    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                      <div className="snippet-tags">
                        {s.tags.map((t) => (
                          <span key={t} className="snippet-tag">
                            #{t}
                          </span>
                        ))}
                      </div>

                      {onRunSnippet && (
                        <button
                          className="tunnel-btn start"
                          style={{ fontSize: "11px", padding: "3px 10px" }}
                          onClick={() => handleExecute(s)}
                        >
                          ▶ Run in Terminal
                        </button>
                      )}
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </div>

        {/* Snippet Editor Overlay */}
        {editing && (
          <SnippetEditModal
            snippet={editing}
            categories={categories.filter((c) => c !== "All")}
            onClose={() => setEditing(null)}
            onSave={async (saved) => {
              await saveSnippet(saved);
              setEditing(null);
              refresh();
            }}
          />
        )}

        {/* Dynamic Parameter Prompt Modal */}
        {runParamModal && (
          <div className="modal-backdrop" style={{ zIndex: 120 }} onClick={() => setRunParamModal(null)}>
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

function SnippetEditModal({
  snippet,
  categories,
  onClose,
  onSave,
}: {
  snippet: Snippet;
  categories: string[];
  onClose: () => void;
  onSave: (s: Snippet) => void;
}) {
  const [title, setTitle] = useState(snippet.title);
  const [command, setCommand] = useState(snippet.command);
  const [category, setCategory] = useState(snippet.category || "General");
  const [description, setDescription] = useState(snippet.description || "");
  const [tagInput, setTagInput] = useState(snippet.tags.join(", "));

  return (
    <div className="snippet-edit-overlay" onClick={onClose}>
      <div className="snippet-edit-form" onClick={(e) => e.stopPropagation()}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <h2 style={{ margin: 0, fontSize: "15px", fontWeight: 600, color: "var(--fg)" }}>
            {snippet.title ? "Edit Snippet" : "New Snippet"}
          </h2>
          <button className="icon-btn" onClick={onClose} title="Cancel">
            ✕
          </button>
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: "10px" }}>
          <label style={{ margin: 0 }}>
            Title *
            <input
              type="text"
              placeholder="e.g. Check Disk Usage, Tail Docker Logs"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              required
            />
          </label>

          <label style={{ margin: 0 }}>
            Category
            <input
              type="text"
              list="snippet-categories-list"
              placeholder="e.g. Docker, Git, Kubernetes, Sysadmin"
              value={category}
              onChange={(e) => setCategory(e.target.value)}
            />
            <datalist id="snippet-categories-list">
              {categories.map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>
          </label>

          <label style={{ margin: 0 }}>
            Command / Script *
            <textarea
              rows={4}
              placeholder="e.g. docker logs -f {{container_name}} --tail {{lines}}"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              style={{
                display: "block",
                width: "100%",
                marginTop: "4px",
                padding: "8px 10px",
                background: "var(--ink-700)",
                border: "1px solid var(--ink-600)",
                borderRadius: "var(--radius)",
                color: "var(--fg)",
                fontFamily: "var(--mono)",
                fontSize: "12px",
                outline: "none",
                resize: "vertical",
              }}
            />
            <span className="hint">
              Tip: Wrap placeholders in <code style={{ color: "var(--lime)" }}>{"{{variable_name}}"}</code> to be prompted dynamically before running.
            </span>
          </label>

          <label style={{ margin: 0 }}>
            Description (optional)
            <input
              type="text"
              placeholder="Short notes or instructions"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </label>

          <label style={{ margin: 0 }}>
            Tags (comma separated)
            <input
              type="text"
              placeholder="e.g. dev, prod, debug"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
            />
          </label>
        </div>

        <div className="actions" style={{ marginTop: "14px" }}>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="primary"
            disabled={!title.trim() || !command.trim()}
            onClick={() => {
              const tags = tagInput
                .split(",")
                .map((t) => t.trim())
                .filter((t) => t.length > 0);
              onSave({
                id: snippet.id,
                title: title.trim(),
                command: command.trim(),
                category: category.trim() || "General",
                description: description.trim() || null,
                tags,
              });
            }}
          >
            Save Snippet
          </button>
        </div>
      </div>
    </div>
  );
}
