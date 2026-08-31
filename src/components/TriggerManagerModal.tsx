import { useState, useEffect } from "react";
import {
  loadTriggers,
  saveTriggers,
  playChime,
  requestNotificationPermission,
  sendDesktopNotification,
  type TerminalTrigger,
  DEFAULT_TRIGGERS,
} from "../lib/triggers";

interface Props {
  open: boolean;
  onClose: () => void;
}

export function TriggerManagerModal({ open, onClose }: Props) {
  const [triggers, setTriggers] = useState<TerminalTrigger[]>(loadTriggers);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [pattern, setPattern] = useState("");
  const [isRegex, setIsRegex] = useState(true);
  const [action, setAction] = useState<"notify" | "sound" | "both">("both");
  const [hasNotifPerm, setHasNotifPerm] = useState<boolean>(
    typeof Notification !== "undefined" && Notification.permission === "granted",
  );

  useEffect(() => {
    if (open) {
      setTriggers(loadTriggers());
      if (typeof Notification !== "undefined") {
        setHasNotifPerm(Notification.permission === "granted");
      }
    }
  }, [open]);

  if (!open) return null;

  const handleToggle = (id: string) => {
    const next = triggers.map((t) => (t.id === id ? { ...t, enabled: !t.enabled } : t));
    setTriggers(next);
    saveTriggers(next);
  };

  const handleDelete = (id: string) => {
    const next = triggers.filter((t) => t.id !== id);
    setTriggers(next);
    saveTriggers(next);
    if (editingId === id) {
      setEditingId(null);
    }
  };

  const handleResetDefaults = () => {
    setTriggers(DEFAULT_TRIGGERS);
    saveTriggers(DEFAULT_TRIGGERS);
    setEditingId(null);
  };

  const startEdit = (t: TerminalTrigger) => {
    setEditingId(t.id);
    setName(t.name);
    setPattern(t.pattern);
    setIsRegex(t.isRegex);
    setAction(t.action);
  };

  const startNew = () => {
    setEditingId("new");
    setName("");
    setPattern("");
    setIsRegex(true);
    setAction("both");
  };

  const handleSave = (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !pattern.trim()) return;

    let next: TerminalTrigger[];
    if (editingId === "new") {
      const newTrigger: TerminalTrigger = {
        id: `trig-${Date.now()}`,
        name: name.trim(),
        pattern: pattern.trim(),
        isRegex,
        enabled: true,
        action,
        soundBeep: action === "sound" || action === "both",
      };
      next = [...triggers, newTrigger];
    } else {
      next = triggers.map((t) =>
        t.id === editingId
          ? {
              ...t,
              name: name.trim(),
              pattern: pattern.trim(),
              isRegex,
              action,
              soundBeep: action === "sound" || action === "both",
            }
          : t,
      );
    }

    setTriggers(next);
    saveTriggers(next);
    setEditingId(null);
  };

  const handleTestAlert = async (trig: TerminalTrigger) => {
    if (trig.action === "sound" || trig.action === "both") {
      playChime(trig.id.includes("error"));
    }
    if (trig.action === "notify" || trig.action === "both") {
      const perm = await requestNotificationPermission();
      setHasNotifPerm(perm);
      if (perm) {
        sendDesktopNotification(
          `Trigger Test: ${trig.name}`,
          `Sample matched terminal output: [OK] Finished building release package in 1.42s`,
        );
      }
    }
  };

  const handleEnableNotifs = async () => {
    const granted = await requestNotificationPermission();
    setHasNotifPerm(granted);
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
            <span style={{ fontSize: 20 }}>🔔</span>
            <h2 className="modal-title" style={{ margin: 0 }}>Terminal Output Triggers & Alerts</h2>
          </div>
          <button className="modal-close-btn" onClick={onClose}>
            &times;
          </button>
        </div>

        <div style={{ padding: "0 24px 12px 24px", borderBottom: "1px solid var(--term-border, #333)" }}>
          <p style={{ margin: "0 0 10px 0", fontSize: 13, color: "var(--term-text-muted, #888)" }}>
            Automatically monitor live terminal output across your SSH and local sessions. Trigger desktop alerts and audio chimes when builds complete, errors occur, or passwords are requested.
          </p>
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", background: "rgba(255, 255, 255, 0.04)", padding: "8px 12px", borderRadius: 6 }}>
            <div style={{ fontSize: 12, color: hasNotifPerm ? "#86efac" : "#fca5a5" }}>
              {hasNotifPerm ? "✓ Desktop Notifications Enabled" : "⚠ Desktop Notifications are currently disabled or blocked"}
            </div>
            {!hasNotifPerm && (
              <button
                type="button"
                className="btn-secondary"
                style={{ fontSize: 12, padding: "4px 10px" }}
                onClick={() => void handleEnableNotifs()}
              >
                Enable Notifications
              </button>
            )}
          </div>
        </div>

        <div style={{ flex: 1, overflowY: "auto", padding: 24, display: "flex", flexDirection: "column", gap: 16 }}>
          {editingId ? (
            <form onSubmit={handleSave} style={{ display: "flex", flexDirection: "column", gap: 14 }}>
              <div style={{ fontSize: 14, fontWeight: 600, color: "var(--term-accent, #bef264)" }}>
                {editingId === "new" ? "Add New Output Trigger" : "Edit Trigger"}
              </div>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "var(--term-text-muted, #aaa)" }}>
                  Trigger Name
                </label>
                <input
                  type="text"
                  required
                  placeholder="e.g. Build Succeeded / Compilation Error"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  className="input-field"
                  style={{ width: "100%" }}
                />
              </div>

              <div>
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 4 }}>
                  <label style={{ fontSize: 12, color: "var(--term-text-muted, #aaa)" }}>
                    Pattern to Match
                  </label>
                  <label style={{ fontSize: 11, display: "flex", alignItems: "center", gap: 4, cursor: "pointer", color: "var(--term-text-muted, #aaa)" }}>
                    <input
                      type="checkbox"
                      checked={isRegex}
                      onChange={(e) => setIsRegex(e.target.checked)}
                    />
                    Regular Expression (Regex)
                  </label>
                </div>
                <input
                  type="text"
                  required
                  placeholder={isRegex ? "(Finished .* in|Build succeeded)" : "Build succeeded"}
                  value={pattern}
                  onChange={(e) => setPattern(e.target.value)}
                  className="input-field"
                  style={{ width: "100%", fontFamily: "monospace", fontSize: 12 }}
                />
              </div>

              <div>
                <label style={{ display: "block", fontSize: 12, marginBottom: 4, color: "var(--term-text-muted, #aaa)" }}>
                  Action when triggered
                </label>
                <div style={{ display: "flex", gap: 12 }}>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="action"
                      value="both"
                      checked={action === "both"}
                      onChange={() => setAction("both")}
                    />
                    Notification & Audio Chime
                  </label>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="action"
                      value="notify"
                      checked={action === "notify"}
                      onChange={() => setAction("notify")}
                    />
                    Notification Only
                  </label>
                  <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                    <input
                      type="radio"
                      name="action"
                      value="sound"
                      checked={action === "sound"}
                      onChange={() => setAction("sound")}
                    />
                    Audio Chime Only
                  </label>
                </div>
              </div>

              <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
                <button
                  type="button"
                  className="btn-secondary"
                  onClick={() => setEditingId(null)}
                >
                  Cancel
                </button>
                <button type="submit" className="btn-primary">
                  Save Trigger
                </button>
              </div>
            </form>
          ) : (
            <>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span style={{ fontSize: 13, fontWeight: 600, color: "#ccc" }}>Active Watchers ({triggers.length})</span>
                <div style={{ display: "flex", gap: 8 }}>
                  <button
                    type="button"
                    className="btn-secondary"
                    style={{ fontSize: 12, padding: "4px 10px" }}
                    onClick={handleResetDefaults}
                  >
                    Reset Defaults
                  </button>
                  <button
                    type="button"
                    className="btn-primary"
                    style={{ fontSize: 12, padding: "4px 12px" }}
                    onClick={startNew}
                  >
                    + Add Trigger
                  </button>
                </div>
              </div>

              <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                {triggers.map((t) => (
                  <div
                    key={t.id}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "space-between",
                      padding: "10px 14px",
                      borderRadius: 6,
                      background: t.enabled ? "rgba(255, 255, 255, 0.05)" : "rgba(255, 255, 255, 0.015)",
                      border: "1px solid rgba(255, 255, 255, 0.08)",
                      opacity: t.enabled ? 1 : 0.6,
                    }}
                  >
                    <div style={{ flex: 1, minWidth: 0, marginRight: 12 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 2 }}>
                        <span style={{ fontSize: 13, fontWeight: 500, color: "#f3f4f6" }}>{t.name}</span>
                        <span
                          style={{
                            fontSize: 10,
                            padding: "1px 6px",
                            borderRadius: 10,
                            background: "rgba(190, 242, 100, 0.15)",
                            color: "#bef264",
                            fontWeight: 600,
                          }}
                        >
                          {t.action}
                        </span>
                      </div>
                      <div
                        style={{
                          fontSize: 11,
                          fontFamily: "monospace",
                          color: "#9ca3af",
                          overflow: "hidden",
                          textOverflow: "ellipsis",
                          whiteSpace: "nowrap",
                        }}
                      >
                        {t.pattern}
                      </div>
                    </div>

                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <button
                        type="button"
                        title="Test alert"
                        className="btn-secondary"
                        style={{ fontSize: 11, padding: "3px 8px" }}
                        onClick={() => void handleTestAlert(t)}
                      >
                        ▶ Test
                      </button>
                      <button
                        type="button"
                        title="Edit"
                        className="btn-secondary"
                        style={{ fontSize: 11, padding: "3px 8px" }}
                        onClick={() => startEdit(t)}
                      >
                        Edit
                      </button>
                      <button
                        type="button"
                        title="Delete"
                        className="btn-secondary"
                        style={{ fontSize: 11, padding: "3px 8px", color: "#f87171" }}
                        onClick={() => handleDelete(t.id)}
                      >
                        ✕
                      </button>
                      <input
                        type="checkbox"
                        title={t.enabled ? "Disable" : "Enable"}
                        checked={t.enabled}
                        onChange={() => handleToggle(t.id)}
                        style={{ cursor: "pointer", marginLeft: 4 }}
                      />
                    </div>
                  </div>
                ))}
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
