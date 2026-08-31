import { useEffect, useState } from "react";
import {
  listSessionLogs,
  readLogFile,
  deleteSessionLog,
  searchCommands,
  type SessionLogItem,
} from "../lib/api";
import { CastReplayer } from "./CastReplayer";

export function SessionHistoryModal({
  onClose,
}: {
  onClose: () => void;
}) {
  const [tab, setTab] = useState<"sessions" | "commands">("sessions");
  const [logs, setLogs] = useState<SessionLogItem[]>([]);
  const [selectedLog, setSelectedLog] = useState<SessionLogItem | null>(null);
  const [plainContent, setPlainContent] = useState<string | null>(null);
  const [castContent, setCastContent] = useState<string | null>(null);
  const [replaying, setReplaying] = useState(false);
  const [loadingContent, setLoadingContent] = useState(false);

  // Commands tab state
  const [cmdQuery, setCmdQuery] = useState("");
  const [cmdResults, setCmdResults] = useState<{ command: string; exitCode: number | null }[]>([]);
  const [loadingCmds, setLoadingCmds] = useState(false);

  // Fetch session logs list
  const refreshLogs = async () => {
    try {
      const list = await listSessionLogs();
      setLogs(list);
      if (list.length > 0 && !selectedLog) {
        selectLog(list[0]);
      }
    } catch (err) {
      console.error("Failed to list logs:", err);
    }
  };

  useEffect(() => {
    void refreshLogs();
  }, []);

  // Search commands
  useEffect(() => {
    let active = true;
    const runSearch = async () => {
      setLoadingCmds(true);
      try {
        const res = await searchCommands(cmdQuery, 100);
        if (active) setCmdResults(res);
      } catch (err) {
        console.error("Failed to search commands:", err);
      } finally {
        if (active) setLoadingCmds(false);
      }
    };

    const debounce = setTimeout(runSearch, 150);
    return () => {
      active = false;
      clearTimeout(debounce);
    };
  }, [cmdQuery]);

  const selectLog = async (item: SessionLogItem) => {
    setSelectedLog(item);
    setLoadingContent(true);
    setPlainContent(null);
    setCastContent(null);
    setReplaying(false);

    try {
      const [plain, cast] = await Promise.all([
        readLogFile(item.plainPath).catch(() => ""),
        readLogFile(item.castPath).catch(() => ""),
      ]);
      setPlainContent(plain);
      setCastContent(cast);
    } catch (err) {
      console.error("Failed to read log content:", err);
    } finally {
      setLoadingContent(false);
    }
  };

  const handleDeleteLog = async (item: SessionLogItem) => {
    if (!confirm(`Delete recorded session log "${item.dirName}"?`)) return;
    try {
      await deleteSessionLog(item.dirName);
      if (selectedLog?.id === item.id) {
        setSelectedLog(null);
        setPlainContent(null);
        setCastContent(null);
      }
      await refreshLogs();
    } catch (err) {
      alert(`Failed to delete log: ${err}`);
    }
  };

  const formatDate = (ts: number) => {
    if (!ts) return "Unknown date";
    const date = new Date(ts * 1000);
    return date.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  };

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal session-history-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="history-modal-header">
          <div className="history-tabs">
            <button
              className={`history-tab-btn ${tab === "sessions" ? "active" : ""}`}
              onClick={() => setTab("sessions")}
            >
              Recorded Sessions ({logs.length})
            </button>
            <button
              className={`history-tab-btn ${tab === "commands" ? "active" : ""}`}
              onClick={() => setTab("commands")}
            >
              Command History (OSC 133)
            </button>
          </div>
          <button className="icon-btn" onClick={onClose} title="Close">
            ✕
          </button>
        </div>

        <div className="history-modal-body">
          {tab === "sessions" ? (
            <div className="history-layout">
              {/* Left sidebar: log list */}
              <div className="history-sidebar">
                {logs.length === 0 ? (
                  <div className="history-empty">No recorded sessions yet.</div>
                ) : (
                  logs.map((item) => (
                    <div
                      key={item.id}
                      className={`history-item ${
                        selectedLog?.id === item.id ? "active" : ""
                      }`}
                      onClick={() => selectLog(item)}
                    >
                      <div className="history-item-top">
                        <span className="history-item-title">
                          {item.dirName}
                        </span>
                        <button
                          className="history-item-del"
                          onClick={(e) => {
                            e.stopPropagation();
                            void handleDeleteLog(item);
                          }}
                          title="Delete session record"
                        >
                          🗑
                        </button>
                      </div>
                      <div className="history-item-meta">
                        <span>{formatDate(item.timestamp)}</span>
                        <span>{formatSize(item.plainSize)}</span>
                      </div>
                    </div>
                  ))
                )}
              </div>

              {/* Right main area: preview or asciinema replay */}
              <div className="history-main">
                {selectedLog ? (
                  replaying && castContent ? (
                    <CastReplayer
                      castContent={castContent}
                      onClose={() => setReplaying(false)}
                    />
                  ) : (
                    <div className="history-detail">
                      <div className="history-detail-bar">
                        <div className="history-detail-info">
                          <span className="detail-name">
                            {selectedLog.dirName}
                          </span>
                          <span className="detail-date">
                            {formatDate(selectedLog.timestamp)}
                          </span>
                        </div>
                        <div className="history-detail-actions">
                          {castContent && selectedLog.castSize > 0 && (
                            <button
                              className="primary sm play-replay-btn"
                              onClick={() => setReplaying(true)}
                            >
                              ▶ Play Asciinema Replay
                            </button>
                          )}
                        </div>
                      </div>

                      <div className="history-log-preview">
                        {loadingContent ? (
                          <div className="history-loading">
                            Loading log output...
                          </div>
                        ) : plainContent ? (
                          <pre className="log-text">{plainContent}</pre>
                        ) : (
                          <div className="history-empty">
                            No plain text log available for this session.
                          </div>
                        )}
                      </div>
                    </div>
                  )
                ) : (
                  <div className="history-empty">
                    Select a session from the list to view logs.
                  </div>
                )}
              </div>
            </div>
          ) : (
            /* Commands History Tab */
            <div className="command-history-layout">
              <div className="command-search-bar">
                <input
                  type="text"
                  className="cmd-input"
                  placeholder="Search executed commands history across all sessions..."
                  value={cmdQuery}
                  onChange={(e) => setCmdQuery(e.target.value)}
                  autoFocus
                />
              </div>

              <div className="command-results-list">
                {loadingCmds ? (
                  <div className="history-loading">Searching commands...</div>
                ) : cmdResults.length === 0 ? (
                  <div className="history-empty">
                    {cmdQuery
                      ? "No commands found matching query."
                      : "No commands recorded yet. Shell integration with OSC 133 records commands automatically."}
                  </div>
                ) : (
                  cmdResults.map((r, i) => (
                    <div key={i} className="command-row">
                      <span
                        className={`exit-badge ${
                          r.exitCode === 0
                            ? "ok"
                            : r.exitCode === null
                            ? "pending"
                            : "fail"
                        }`}
                      >
                        {r.exitCode !== null ? `exit ${r.exitCode}` : "running"}
                      </span>
                      <code className="cmd-text">{r.command}</code>
                    </div>
                  ))
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
