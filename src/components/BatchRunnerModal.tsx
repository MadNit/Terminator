import { useState, useEffect, useMemo } from "react";
import {
  batchExec,
  listProfiles,
  listSnippets,
  type Profile,
  type Snippet,
  type BatchExecRequest,
  type BatchExecResult,
} from "../lib/api";

interface Props {
  open: boolean;
  onClose: () => void;
}

const TEMPLATES = [
  {
    name: "System Health & Uptime",
    script: "uptime && uname -a && df -h /",
  },
  {
    name: "Memory & Disk Summary",
    script: "free -m 2>/dev/null || vm_stat; echo '--- Disk ---'; df -h",
  },
  {
    name: "Top 5 CPU Processes",
    script: "ps -eo pid,user,%cpu,%mem,comm --sort=-%cpu 2>/dev/null | head -n 6 || ps aux | head -n 6",
  },
  {
    name: "Docker Container Status",
    script: "docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}\t{{.Ports}}'",
  },
  {
    name: "Check Listening Ports",
    script: "netstat -tuln 2>/dev/null || ss -tuln 2>/dev/null || lsof -i -P -n | grep LISTEN",
  },
  {
    name: "Package Updates Check",
    script: "if which apt-get >/dev/null; then apt list --upgradable 2>/dev/null | head -n 10; elif which yum >/dev/null; then yum check-update; fi",
  },
];

export function BatchRunnerModal({ open, onClose }: Props) {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [selectedProfileIds, setSelectedProfileIds] = useState<Set<string>>(new Set());
  const [command, setCommand] = useState<string>("uptime && df -h /");
  const [running, setRunning] = useState<boolean>(false);
  const [results, setResults] = useState<BatchExecResult[]>([]);
  const [activeHostTab, setActiveHostTab] = useState<string>("all");
  const [filterText, setFilterText] = useState<string>("");
  const [statusFilter, setStatusFilter] = useState<"all" | "success" | "failed">("all");

  useEffect(() => {
    if (open) {
      listProfiles().then((pList) => {
        setProfiles(pList);
        // Default select all SSH profiles if none selected
        if (selectedProfileIds.size === 0) {
          const sshIds = new Set(pList.filter((p) => p.spec.kind === "ssh").map((p) => p.id));
          setSelectedProfileIds(sshIds);
        }
      }).catch(() => {});

      listSnippets().then(setSnippets).catch(() => {});
    }
  }, [open]);

  const toggleSelectProfile = (id: string) => {
    const next = new Set(selectedProfileIds);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setSelectedProfileIds(next);
  };

  const selectAll = () => {
    setSelectedProfileIds(new Set(profiles.map((p) => p.id)));
  };

  const selectNone = () => {
    setSelectedProfileIds(new Set());
  };

  const handleRun = async (failedOnly: boolean = false) => {
    let targets = profiles.filter((p) => selectedProfileIds.has(p.id));
    if (failedOnly) {
      const failedIds = new Set(results.filter((r) => r.exitCode !== 0 || r.error).map((r) => r.id));
      targets = targets.filter((t) => failedIds.has(t.id));
    }

    if (targets.length === 0 || !command.trim()) return;

    setRunning(true);
    const requests: BatchExecRequest[] = targets.map((t) => ({
      id: t.id,
      label: t.name,
      spec: t.spec,
      command: command.trim(),
    }));

    try {
      const execResults = await batchExec(requests);
      setResults(execResults);
    } catch (err: any) {
      alert(`Batch execution failed: ${err?.message || err}`);
    } finally {
      setRunning(false);
    }
  };

  const filteredResults = useMemo(() => {
    return results.filter((r) => {
      if (statusFilter === "success" && (r.exitCode !== 0 || r.error)) return false;
      if (statusFilter === "failed" && r.exitCode === 0 && !r.error) return false;
      if (filterText) {
        const q = filterText.toLowerCase();
        return (
          r.label.toLowerCase().includes(q) ||
          r.stdout.toLowerCase().includes(q) ||
          r.stderr.toLowerCase().includes(q) ||
          (r.error && r.error.toLowerCase().includes(q))
        );
      }
      return true;
    });
  }, [results, statusFilter, filterText]);

  const successCount = results.filter((r) => r.exitCode === 0 && !r.error).length;
  const failureCount = results.filter((r) => r.exitCode !== 0 || r.error).length;

  const exportLogs = () => {
    const combined = results
      .map((r) => `=== HOST: ${r.label} (Exit Code: ${r.exitCode}, Duration: ${r.durationMs}ms) ===\n${r.error ? `ERROR: ${r.error}\n` : ""}${r.stdout}\n${r.stderr ? `STDERR:\n${r.stderr}\n` : ""}`)
      .join("\n\n" + "=".repeat(60) + "\n\n");

    const blob = new Blob([combined], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `batch-exec-results-${Date.now()}.log`;
    a.click();
    URL.revokeObjectURL(url);
  };

  if (!open) return null;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-content"
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 960,
          maxWidth: "95vw",
          height: "85vh",
          display: "flex",
          flexDirection: "column",
          padding: 0,
          overflow: "hidden",
          background: "var(--term-bg, #18181b)",
          color: "var(--term-text, #f3f4f6)",
          border: "1px solid var(--term-border, #3f3f46)",
          borderRadius: 8,
          boxShadow: "0 20px 40px rgba(0,0,0,0.6)",
        }}
      >
        {/* Header */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "12px 18px",
            borderBottom: "1px solid var(--term-border, #3f3f46)",
            background: "rgba(0,0,0,0.2)",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <span style={{ fontSize: 18 }}>🚀</span>
            <div>
              <div style={{ fontWeight: 600, fontSize: 15 }}>
                Multi-Host Batch Command Execution
              </div>
              <div style={{ fontSize: 11, color: "var(--term-text-muted, #9ca3af)" }}>
                Run scripts and snippets across multiple SSH hosts simultaneously with aggregated output
              </div>
            </div>
          </div>

          <button
            onClick={onClose}
            style={{
              background: "transparent",
              border: "none",
              color: "#9ca3af",
              fontSize: 18,
              cursor: "pointer",
            }}
          >
            ✕
          </button>
        </div>

        {/* Top Control Panel: Host Selector & Command Box */}
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "260px 1fr",
            borderBottom: "1px solid var(--term-border, #3f3f46)",
            height: 220,
          }}
        >
          {/* Host list column */}
          <div
            style={{
              borderRight: "1px solid var(--term-border, #3f3f46)",
              display: "flex",
              flexDirection: "column",
              background: "rgba(0,0,0,0.15)",
            }}
          >
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                padding: "8px 12px",
                borderBottom: "1px solid rgba(255,255,255,0.05)",
                fontSize: 11,
                fontWeight: 600,
                color: "#9ca3af",
              }}
            >
              <span>TARGET HOSTS ({selectedProfileIds.size}/{profiles.length})</span>
              <div style={{ display: "flex", gap: 6 }}>
                <button
                  onClick={selectAll}
                  style={{ background: "none", border: "none", color: "#60a5fa", cursor: "pointer", fontSize: 11 }}
                >
                  All
                </button>
                <span>|</span>
                <button
                  onClick={selectNone}
                  style={{ background: "none", border: "none", color: "#9ca3af", cursor: "pointer", fontSize: 11 }}
                >
                  None
                </button>
              </div>
            </div>

            <div style={{ flex: 1, overflowY: "auto", padding: "6px 8px" }}>
              {profiles.length === 0 ? (
                <div style={{ fontSize: 12, color: "#9ca3af", padding: 10 }}>
                  No saved profiles found.
                </div>
              ) : (
                profiles.map((p) => {
                  const isChecked = selectedProfileIds.has(p.id);
                  return (
                    <label
                      key={p.id}
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        padding: "5px 8px",
                        borderRadius: 4,
                        cursor: "pointer",
                        background: isChecked ? "rgba(59, 130, 246, 0.1)" : "transparent",
                        marginBottom: 2,
                        fontSize: 12,
                      }}
                    >
                      <input
                        type="checkbox"
                        checked={isChecked}
                        onChange={() => toggleSelectProfile(p.id)}
                      />
                      <span style={{ fontWeight: isChecked ? 600 : 400, color: isChecked ? "#93c5fd" : "#d1d5db", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {p.name}
                      </span>
                      <span style={{ fontSize: 10, color: "#6b7280", marginLeft: "auto" }}>
                        {p.spec.kind}
                      </span>
                    </label>
                  );
                })
              )}
            </div>
          </div>

          {/* Script Editor & Controls column */}
          <div style={{ display: "flex", flexDirection: "column", padding: "10px 14px", gap: 8 }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span style={{ fontSize: 12, fontWeight: 600, color: "#9ca3af" }}>COMMAND / SCRIPT</span>
              <div style={{ display: "flex", gap: 8 }}>
                {/* Template picker */}
                <select
                  onChange={(e) => {
                    if (e.target.value) setCommand(e.target.value);
                  }}
                  style={{
                    fontSize: 11,
                    padding: "3px 6px",
                    background: "rgba(0,0,0,0.3)",
                    color: "#f3f4f6",
                    border: "1px solid #444",
                    borderRadius: 4,
                  }}
                  defaultValue=""
                >
                  <option value="" disabled>⚡ Insert Script Template...</option>
                  {TEMPLATES.map((t, i) => (
                    <option key={i} value={t.script}>{t.name}</option>
                  ))}
                </select>

                {/* Snippets picker */}
                {snippets.length > 0 && (
                  <select
                    onChange={(e) => {
                      if (e.target.value) setCommand(e.target.value);
                    }}
                    style={{
                      fontSize: 11,
                      padding: "3px 6px",
                      background: "rgba(0,0,0,0.3)",
                      color: "#f3f4f6",
                      border: "1px solid #444",
                      borderRadius: 4,
                    }}
                    defaultValue=""
                  >
                    <option value="" disabled>📋 Insert Saved Snippet...</option>
                    {snippets.map((s) => (
                      <option key={s.id} value={s.command}>{s.title}</option>
                    ))}
                  </select>
                )}
              </div>
            </div>

            <textarea
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="Enter bash / sh commands to run on all selected hosts..."
              style={{
                flex: 1,
                background: "rgba(0,0,0,0.3)",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 6,
                padding: 8,
                fontFamily: "monospace",
                fontSize: 12,
                color: "#f3f4f6",
                resize: "none",
              }}
            />

            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <span style={{ fontSize: 11, color: "#9ca3af" }}>
                Targeting <strong>{selectedProfileIds.size}</strong> host{selectedProfileIds.size !== 1 ? "s" : ""}
              </span>
              <div style={{ display: "flex", gap: 8 }}>
                {failureCount > 0 && (
                  <button
                    className="btn-secondary"
                    onClick={() => handleRun(true)}
                    disabled={running}
                    style={{ fontSize: 12, color: "#f87171", borderColor: "rgba(248, 113, 113, 0.4)" }}
                  >
                    ↻ Re-run Failed ({failureCount})
                  </button>
                )}
                <button
                  className="btn-primary"
                  onClick={() => handleRun(false)}
                  disabled={running || selectedProfileIds.size === 0 || !command.trim()}
                  style={{ display: "flex", alignItems: "center", gap: 6, padding: "6px 14px", fontSize: 12 }}
                >
                  {running ? (
                    <>
                      <span className="spinner" /> Running on {selectedProfileIds.size} hosts...
                    </>
                  ) : (
                    <>
                      <span>▶</span> Run on {selectedProfileIds.size} Hosts
                    </>
                  )}
                </button>
              </div>
            </div>
          </div>
        </div>

        {/* Results & Aggregated Logs Section */}
        <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden" }}>
          {/* Results Toolbar */}
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
              padding: "8px 14px",
              background: "rgba(0,0,0,0.2)",
              borderBottom: "1px solid var(--term-border, #3f3f46)",
            }}
          >
            <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
              <span style={{ fontSize: 12, fontWeight: 600 }}>EXECUTION OUTPUT</span>
              {results.length > 0 && (
                <div style={{ display: "flex", gap: 6, fontSize: 11 }}>
                  <span
                    onClick={() => setStatusFilter("all")}
                    style={{
                      cursor: "pointer",
                      padding: "2px 8px",
                      borderRadius: 10,
                      background: statusFilter === "all" ? "rgba(255,255,255,0.15)" : "transparent",
                      color: "#d1d5db",
                    }}
                  >
                    All ({results.length})
                  </span>
                  <span
                    onClick={() => setStatusFilter("success")}
                    style={{
                      cursor: "pointer",
                      padding: "2px 8px",
                      borderRadius: 10,
                      background: statusFilter === "success" ? "rgba(52, 211, 153, 0.2)" : "transparent",
                      color: "#34d399",
                    }}
                  >
                    ✓ {successCount} Succeeded
                  </span>
                  {failureCount > 0 && (
                    <span
                      onClick={() => setStatusFilter("failed")}
                      style={{
                        cursor: "pointer",
                        padding: "2px 8px",
                        borderRadius: 10,
                        background: statusFilter === "failed" ? "rgba(248, 113, 113, 0.2)" : "transparent",
                        color: "#f87171",
                      }}
                    >
                      ✕ {failureCount} Failed
                    </span>
                  )}
                </div>
              )}
            </div>

            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <input
                type="text"
                placeholder="Filter output..."
                value={filterText}
                onChange={(e) => setFilterText(e.target.value)}
                style={{
                  fontSize: 11,
                  padding: "3px 8px",
                  background: "rgba(0,0,0,0.3)",
                  color: "#f3f4f6",
                  border: "1px solid #444",
                  borderRadius: 4,
                  width: 160,
                }}
              />
              {results.length > 0 && (
                <button
                  className="btn-secondary"
                  onClick={exportLogs}
                  style={{ padding: "3px 8px", fontSize: 11 }}
                >
                  📥 Export Logs
                </button>
              )}
            </div>
          </div>

          {/* Host result chips row */}
          {results.length > 0 && (
            <div
              style={{
                display: "flex",
                gap: 6,
                padding: "6px 14px",
                overflowX: "auto",
                background: "rgba(0,0,0,0.1)",
                borderBottom: "1px solid rgba(255,255,255,0.05)",
              }}
            >
              <button
                onClick={() => setActiveHostTab("all")}
                style={{
                  fontSize: 11,
                  padding: "3px 8px",
                  borderRadius: 4,
                  border: "1px solid",
                  borderColor: activeHostTab === "all" ? "#60a5fa" : "rgba(255,255,255,0.1)",
                  background: activeHostTab === "all" ? "rgba(96, 165, 250, 0.15)" : "transparent",
                  color: activeHostTab === "all" ? "#93c5fd" : "#9ca3af",
                  cursor: "pointer",
                }}
              >
                All Combined ({results.length})
              </button>
              {results.map((r) => {
                const isSuccess = r.exitCode === 0 && !r.error;
                const isSelected = activeHostTab === r.id;
                return (
                  <button
                    key={r.id}
                    onClick={() => setActiveHostTab(r.id)}
                    style={{
                      fontSize: 11,
                      padding: "3px 8px",
                      borderRadius: 4,
                      border: "1px solid",
                      borderColor: isSelected ? (isSuccess ? "#34d399" : "#f87171") : "rgba(255,255,255,0.08)",
                      background: isSelected
                        ? isSuccess
                          ? "rgba(52, 211, 153, 0.15)"
                          : "rgba(248, 113, 113, 0.15)"
                        : "transparent",
                      color: isSuccess ? "#6ee7b7" : "#fca5a5",
                      cursor: "pointer",
                      display: "flex",
                      alignItems: "center",
                      gap: 4,
                    }}
                  >
                    <span>{isSuccess ? "✓" : "✕"}</span>
                    <span>{r.label}</span>
                    <span style={{ fontSize: 9, opacity: 0.7 }}>({r.durationMs}ms)</span>
                  </button>
                );
              })}
            </div>
          )}

          {/* Logs Viewport */}
          <div
            style={{
              flex: 1,
              overflowY: "auto",
              padding: 14,
              fontFamily: "monospace",
              fontSize: 12,
              background: "rgba(0,0,0,0.4)",
            }}
          >
            {results.length === 0 ? (
              <div style={{ color: "#9ca3af", textAlign: "center", padding: 40 }}>
                {running ? "Executing batch commands across selected hosts..." : "Click 'Run' to execute command across hosts."}
              </div>
            ) : (
              (activeHostTab === "all"
                ? filteredResults
                : filteredResults.filter((r) => r.id === activeHostTab)
              ).map((r) => {
                const isSuccess = r.exitCode === 0 && !r.error;
                return (
                  <div
                    key={r.id}
                    style={{
                      marginBottom: 16,
                      background: "rgba(255,255,255,0.02)",
                      border: `1px solid ${isSuccess ? "rgba(52, 211, 153, 0.2)" : "rgba(248, 113, 113, 0.2)"}`,
                      borderRadius: 6,
                      overflow: "hidden",
                    }}
                  >
                    {/* Host result header */}
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                        padding: "6px 12px",
                        background: isSuccess ? "rgba(52, 211, 153, 0.08)" : "rgba(248, 113, 113, 0.08)",
                        borderBottom: "1px solid rgba(255,255,255,0.05)",
                      }}
                    >
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span style={{ color: isSuccess ? "#34d399" : "#f87171", fontWeight: 700 }}>
                          {isSuccess ? "✓ SUCCESS" : "✕ FAILED"}
                        </span>
                        <strong style={{ color: "#f3f4f6" }}>{r.label}</strong>
                      </div>
                      <div style={{ fontSize: 11, color: "#9ca3af", display: "flex", gap: 10 }}>
                        <span>Exit Code: {r.exitCode}</span>
                        <span>Duration: {r.durationMs}ms</span>
                      </div>
                    </div>

                    {/* Output body */}
                    <div style={{ padding: "8px 12px", whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
                      {r.error && (
                        <div style={{ color: "#f87171", marginBottom: 6 }}>
                          ERROR: {r.error}
                        </div>
                      )}
                      {r.stdout ? (
                        <div style={{ color: "#e5e7eb" }}>{r.stdout}</div>
                      ) : (
                        !r.error && !r.stderr && <div style={{ color: "#9ca3af", fontStyle: "italic" }}>(No output)</div>
                      )}
                      {r.stderr && (
                        <div style={{ color: "#fbbf24", marginTop: 6, borderTop: "1px dashed rgba(251, 191, 36, 0.3)", paddingTop: 4 }}>
                          <span style={{ fontSize: 10, color: "#f59e0b" }}>STDERR:</span>
                          <div>{r.stderr}</div>
                        </div>
                      )}
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
