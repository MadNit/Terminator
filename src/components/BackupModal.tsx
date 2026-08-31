import { useState, useEffect } from "react";
import {
  generateBackupData,
  downloadBackupFile,
  restoreBackupData,
  type ImportResult,
  type TerminatorBackup,
} from "../lib/backup";

interface Props {
  open: boolean;
  onClose: () => void;
  onRestoreComplete?: () => void;
}

export function BackupModal({ open, onClose, onRestoreComplete }: Props) {
  const [activeTab, setActiveTab] = useState<"export" | "import">("export");
  const [pasteJson, setPasteJson] = useState("");
  const [importMode, setImportMode] = useState<"merge" | "replace">("merge");
  const [importResult, setImportResult] = useState<ImportResult | null>(null);
  const [copied, setCopied] = useState(false);
  const [backupData, setBackupData] = useState<TerminatorBackup | null>(null);

  useEffect(() => {
    if (open) {
      void generateBackupData().then(setBackupData);
    }
  }, [open, activeTab]);

  if (!open) return null;

  const handleCopy = () => {
    if (!backupData) return;
    navigator.clipboard.writeText(JSON.stringify(backupData, null, 2));
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleFileUpload = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (event) => {
      const text = event.target?.result as string;
      if (text) {
        setPasteJson(text);
      }
    };
    reader.readAsText(file);
  };

  const handleImport = async () => {
    if (!pasteJson.trim()) return;
    try {
      const parsed = JSON.parse(pasteJson);
      const res = await restoreBackupData(parsed, importMode);
      setImportResult(res);
      if (res.success && onRestoreComplete) {
        onRestoreComplete();
      }
    } catch (err) {
      setImportResult({
        success: false,
        message: `JSON Parse error: ${String(err)}`,
        counts: { profiles: 0, tunnels: 0, snippets: 0, knownHosts: 0, triggers: 0 },
      });
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card"
        style={{ width: 680, maxWidth: "90vw", maxHeight: "85vh", display: "flex", flexDirection: "column" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <span style={{ fontSize: 20 }}>📦</span>
            <h2 className="modal-title" style={{ margin: 0 }}>Profiles & Snippets Import / Export</h2>
          </div>
          <button className="modal-close-btn" onClick={onClose}>
            &times;
          </button>
        </div>

        {/* Tab switcher */}
        <div style={{ display: "flex", borderBottom: "1px solid var(--term-border, #333)", padding: "0 24px" }}>
          <button
            type="button"
            className={`tab-btn ${activeTab === "export" ? "active" : ""}`}
            style={{
              padding: "10px 18px",
              background: "transparent",
              border: "none",
              borderBottom: activeTab === "export" ? "2px solid var(--term-accent, #bef264)" : "2px solid transparent",
              color: activeTab === "export" ? "var(--term-accent, #bef264)" : "#888",
              cursor: "pointer",
              fontWeight: 600,
              fontSize: 13,
            }}
            onClick={() => {
              setActiveTab("export");
              setImportResult(null);
            }}
          >
            Export Backup
          </button>
          <button
            type="button"
            className={`tab-btn ${activeTab === "import" ? "active" : ""}`}
            style={{
              padding: "10px 18px",
              background: "transparent",
              border: "none",
              borderBottom: activeTab === "import" ? "2px solid var(--term-accent, #bef264)" : "2px solid transparent",
              color: activeTab === "import" ? "var(--term-accent, #bef264)" : "#888",
              cursor: "pointer",
              fontWeight: 600,
              fontSize: 13,
            }}
            onClick={() => {
              setActiveTab("import");
              setImportResult(null);
            }}
          >
            Import Backup
          </button>
        </div>

        <div style={{ flex: 1, overflowY: "auto", padding: 24, display: "flex", flexDirection: "column", gap: 16 }}>
          {activeTab === "export" ? (
            <>
              <p style={{ margin: 0, fontSize: 13, color: "var(--term-text-muted, #9ca3af)" }}>
                Export your saved connection profiles, jump host configurations, SSH tunnels, snippet templates, and output triggers into a portable JSON backup file.
              </p>

              <div
                style={{
                  display: "grid",
                  gridTemplateColumns: "repeat(auto-fit, minmax(110px, 1fr))",
                  gap: 10,
                }}
              >
                <div style={{ background: "rgba(255,255,255,0.04)", padding: "10px 12px", borderRadius: 6, border: "1px solid rgba(255,255,255,0.08)" }}>
                  <div style={{ fontSize: 11, color: "#9ca3af" }}>Profiles</div>
                  <div style={{ fontSize: 18, fontWeight: 700, color: "#bef264" }}>{backupData?.profiles.length ?? 0}</div>
                </div>
                <div style={{ background: "rgba(255,255,255,0.04)", padding: "10px 12px", borderRadius: 6, border: "1px solid rgba(255,255,255,0.08)" }}>
                  <div style={{ fontSize: 11, color: "#9ca3af" }}>SSH Tunnels</div>
                  <div style={{ fontSize: 18, fontWeight: 700, color: "#a78bfa" }}>{backupData?.tunnels.length ?? 0}</div>
                </div>
                <div style={{ background: "rgba(255,255,255,0.04)", padding: "10px 12px", borderRadius: 6, border: "1px solid rgba(255,255,255,0.08)" }}>
                  <div style={{ fontSize: 11, color: "#9ca3af" }}>Snippets</div>
                  <div style={{ fontSize: 18, fontWeight: 700, color: "#f472b6" }}>{backupData?.snippets.length ?? 0}</div>
                </div>
                <div style={{ background: "rgba(255,255,255,0.04)", padding: "10px 12px", borderRadius: 6, border: "1px solid rgba(255,255,255,0.08)" }}>
                  <div style={{ fontSize: 11, color: "#9ca3af" }}>Known Hosts</div>
                  <div style={{ fontSize: 18, fontWeight: 700, color: "#60a5fa" }}>{backupData?.knownHosts.length ?? 0}</div>
                </div>
                <div style={{ background: "rgba(255,255,255,0.04)", padding: "10px 12px", borderRadius: 6, border: "1px solid rgba(255,255,255,0.08)" }}>
                  <div style={{ fontSize: 11, color: "#9ca3af" }}>Triggers</div>
                  <div style={{ fontSize: 18, fontWeight: 700, color: "#facc15" }}>{backupData?.triggers.length ?? 0}</div>
                </div>
              </div>

              <div style={{ display: "flex", gap: 10 }}>
                <button
                  type="button"
                  className="btn-primary"
                  style={{ display: "flex", alignItems: "center", gap: 6, padding: "8px 16px" }}
                  onClick={() => void downloadBackupFile()}
                >
                  <span>⬇</span> Download JSON Backup File
                </button>
                <button
                  type="button"
                  className="btn-secondary"
                  style={{ display: "flex", alignItems: "center", gap: 6, padding: "8px 16px" }}
                  onClick={handleCopy}
                >
                  <span>📋</span> {copied ? "Copied!" : "Copy JSON"}
                </button>
              </div>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#9ca3af" }}>
                  JSON Preview
                </label>
                <textarea
                  readOnly
                  value={backupData ? JSON.stringify(backupData, null, 2) : "Loading..."}
                  style={{
                    width: "100%",
                    height: 180,
                    fontFamily: "monospace",
                    fontSize: 11,
                    background: "rgba(0,0,0,0.3)",
                    border: "1px solid var(--term-border, #333)",
                    borderRadius: 6,
                    padding: 8,
                    color: "#9ca3af",
                    resize: "none",
                  }}
                />
              </div>
            </>
          ) : (
            <>
              <p style={{ margin: 0, fontSize: 13, color: "var(--term-text-muted, #9ca3af)" }}>
                Import configurations from another machine or previously saved backup.
              </p>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 6, color: "#ccc" }}>
                  Upload Backup File (.json)
                </label>
                <input
                  type="file"
                  accept=".json"
                  onChange={handleFileUpload}
                  style={{ fontSize: 12, color: "#9ca3af" }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "#ccc" }}>
                  Or Paste JSON Content
                </label>
                <textarea
                  placeholder='{"version": 1, "profiles": [...], "snippets": [...]}'
                  value={pasteJson}
                  onChange={(e) => setPasteJson(e.target.value)}
                  style={{
                    width: "100%",
                    height: 130,
                    fontFamily: "monospace",
                    fontSize: 11,
                    background: "rgba(0,0,0,0.3)",
                    border: "1px solid var(--term-border, #333)",
                    borderRadius: 6,
                    padding: 8,
                    color: "#f3f4f6",
                    resize: "vertical",
                  }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 6, color: "#ccc" }}>
                  Import Strategy
                </label>
                <div style={{ display: "flex", gap: 16 }}>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="importMode"
                      value="merge"
                      checked={importMode === "merge"}
                      onChange={() => setImportMode("merge")}
                    />
                    <span><strong>Merge</strong> with existing items</span>
                  </label>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="importMode"
                      value="replace"
                      checked={importMode === "replace"}
                      onChange={() => setImportMode("replace")}
                    />
                    <span><strong style={{ color: "#f87171" }}>Replace / Overwrite</strong> all items</span>
                  </label>
                </div>
              </div>

              {importResult && (
                <div
                  style={{
                    padding: "10px 14px",
                    borderRadius: 6,
                    background: importResult.success ? "rgba(134, 239, 172, 0.1)" : "rgba(248, 113, 113, 0.1)",
                    border: `1px solid ${importResult.success ? "#86efac" : "#f87171"}`,
                    fontSize: 13,
                    color: importResult.success ? "#86efac" : "#f87171",
                  }}
                >
                  <div style={{ fontWeight: 600 }}>{importResult.message}</div>
                  {importResult.success && (
                    <div style={{ fontSize: 11, marginTop: 4, color: "#d1d5db" }}>
                      Imported: {importResult.counts.profiles} profiles, {importResult.counts.tunnels} tunnels, {importResult.counts.snippets} snippets, {importResult.counts.knownHosts} known hosts, {importResult.counts.triggers} triggers.
                    </div>
                  )}
                </div>
              )}

              <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 8 }}>
                <button
                  type="button"
                  className="btn-primary"
                  disabled={!pasteJson.trim()}
                  onClick={handleImport}
                >
                  Import Configuration Now
                </button>
              </div>
            </>
          )}
        </div>

        <div className="modal-footer">
          <button type="button" className="btn-secondary" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
