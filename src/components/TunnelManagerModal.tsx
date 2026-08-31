import { useEffect, useState, useCallback } from "react";
import {
  listTunnels,
  saveTunnel,
  deleteTunnel,
  activeTunnels,
  startTunnel,
  stopTunnel,
  listProfiles,
  hasSecret,
  type TunnelConfig,
  type TunnelStatus,
  type Profile,
} from "../lib/api";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function TunnelManagerModal({
  onClose,
}: {
  onClose: () => void;
}) {
  const [tunnels, setTunnels] = useState<TunnelConfig[]>([]);
  const [statuses, setStatuses] = useState<Record<string, TunnelStatus>>({});
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [editing, setEditing] = useState<TunnelConfig | null>(null);
  const [startingId, setStartingId] = useState<string | null>(null);
  const [pwPromptFor, setPwPromptFor] = useState<{ tunnel: TunnelConfig; resolve: (pw: string) => void } | null>(null);
  const [passwordInput, setPasswordInput] = useState("");

  const refreshTunnels = useCallback(async () => {
    try {
      const [tList, sList, pList] = await Promise.all([
        listTunnels(),
        activeTunnels(),
        listProfiles(),
      ]);
      setTunnels(tList);
      const statusMap: Record<string, TunnelStatus> = {};
      for (const s of sList) {
        statusMap[s.id] = s;
      }
      setStatuses(statusMap);
      setProfiles(pList.filter((p) => p.spec.kind === "ssh"));
    } catch (e) {
      console.error("Failed loading tunnels:", e);
    }
  }, []);

  useEffect(() => {
    refreshTunnels();
    const interval = setInterval(async () => {
      try {
        const sList = await activeTunnels();
        const statusMap: Record<string, TunnelStatus> = {};
        for (const s of sList) {
          statusMap[s.id] = s;
        }
        setStatuses(statusMap);
      } catch {}
    }, 2000);
    return () => clearInterval(interval);
  }, [refreshTunnels]);

  const handleStart = async (tunnel: TunnelConfig) => {
    setStartingId(tunnel.id);
    try {
      const spec = tunnel.ssh_spec;
      let secretRef: string | undefined;
      let password: string | undefined;

      if (spec.kind === "ssh" && spec.auth.method === "password") {
        const secretKey = `ssh:${spec.user}@${spec.host}:${spec.port}`;
        const stored = await hasSecret(secretKey);
        if (stored) {
          secretRef = secretKey;
        } else {
          // Prompt for password
          password = await new Promise<string>((resolve) => {
            setPwPromptFor({ tunnel, resolve });
          });
        }
      }

      await startTunnel(tunnel, secretRef, password);
      await refreshTunnels();
    } catch (err: any) {
      alert(`Failed to start tunnel: ${err.message || err}`);
    } finally {
      setStartingId(null);
    }
  };

  const handleStop = async (id: string) => {
    try {
      await stopTunnel(id);
      await refreshTunnels();
    } catch (err: any) {
      alert(`Failed to stop tunnel: ${err.message || err}`);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm("Are you sure you want to delete this tunnel configuration?")) return;
    try {
      await deleteTunnel(id);
      await refreshTunnels();
    } catch (err: any) {
      alert(`Failed to delete tunnel: ${err.message || err}`);
    }
  };

  const handleSaveEdit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    try {
      await saveTunnel(editing);
      setEditing(null);
      await refreshTunnels();
    } catch (err: any) {
      alert(`Failed to save tunnel: ${err.message || err}`);
    }
  };

  const openNewTunnel = () => {
    const defaultSsh = profiles[0]?.spec || {
      kind: "ssh",
      host: "127.0.0.1",
      port: 22,
      user: "root",
      auth: { method: "agent" },
    };
    setEditing({
      id: crypto.randomUUID(),
      name: "New Tunnel",
      kind: "local",
      ssh_spec: defaultSsh,
      local_addr: "127.0.0.1",
      local_port: 8080,
      target_host: "127.0.0.1",
      target_port: 80,
    });
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal tunnel-manager-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="history-modal-header">
          <div className="history-tabs">
            <span style={{ fontWeight: 600, fontSize: "13.5px", color: "var(--fg)", display: "flex", alignItems: "center", gap: "6px" }}>
              <span>🔀</span> SSH Port Forwarding & Tunnels
            </span>
          </div>
          <button className="icon-btn" onClick={onClose} title="Close">
            ✕
          </button>
        </div>

        <div className="tunnel-modal-body">
          <div className="tunnel-toolbar">
            <button className="primary sm" onClick={openNewTunnel}>
              + Add Tunnel
            </button>
            <span className="tunnel-count">{tunnels.length} configured tunnel{tunnels.length === 1 ? "" : "s"}</span>
          </div>

          <div className="tunnels-list">
            {tunnels.length === 0 ? (
              <div className="tunnels-empty">
                <p>No SSH port forwarding tunnels configured.</p>
                <p className="tunnels-empty-hint">
                  Create Local (-L), Remote (-R), or Dynamic SOCKS5 (-D) proxy tunnels to access remote resources securely.
                </p>
              </div>
            ) : (
              tunnels.map((t) => {
                const status = statuses[t.id];
                const isActive = status?.active;
                const isStarting = startingId === t.id;

                let kindLabel = "Local (-L)";
                let desc = `${t.local_addr}:${t.local_port} ➔ ${t.target_host}:${t.target_port}`;
                if (t.kind === "remote") {
                  kindLabel = "Remote (-R)";
                  desc = `Remote :${t.target_port} ➔ ${t.local_addr}:${t.local_port}`;
                } else if (t.kind === "dynamic") {
                  kindLabel = "Dynamic SOCKS5 (-D)";
                  desc = `SOCKS5 Proxy on ${t.local_addr}:${t.local_port}`;
                }

                const sshDesc = t.ssh_spec.kind === "ssh"
                  ? `${t.ssh_spec.user}@${t.ssh_spec.host}:${t.ssh_spec.port}${t.ssh_spec.jump_host ? " (via jump)" : ""}`
                  : "SSH Server";

                return (
                  <div key={t.id} className={`tunnel-item ${isActive ? "active" : ""}`}>
                    <div className="tunnel-item-main">
                      <div className="tunnel-item-title-row">
                        <span className={`tunnel-badge ${t.kind}`}>{kindLabel}</span>
                        <strong className="tunnel-name">{t.name}</strong>
                        <span className="tunnel-via">via {sshDesc}</span>
                        {isActive && <span className="tunnel-live-dot" title="Active">● Live</span>}
                      </div>
                      <div className="tunnel-mapping">{desc}</div>
                      {status && (
                        <div className="tunnel-stats">
                          <span>Conns: {status.active_connections}</span>
                          <span>TX: {formatBytes(status.bytes_tx)}</span>
                          <span>RX: {formatBytes(status.bytes_rx)}</span>
                          {status.error && <span className="tunnel-error">⚠️ {status.error}</span>}
                        </div>
                      )}
                    </div>

                    <div className="tunnel-item-actions">
                      {isActive ? (
                        <button
                          className="tunnel-btn stop"
                          onClick={() => handleStop(t.id)}
                          title="Stop Tunnel"
                        >
                          ⏹ Stop
                        </button>
                      ) : (
                        <button
                          className="tunnel-btn start"
                          disabled={isStarting}
                          onClick={() => handleStart(t)}
                          title="Start Tunnel"
                        >
                          {isStarting ? "Starting..." : "▶ Start"}
                        </button>
                      )}
                      <button
                        className="tunnel-btn edit"
                        disabled={isActive}
                        onClick={() => setEditing({ ...t })}
                        title="Edit Tunnel"
                      >
                        ✏️
                      </button>
                      <button
                        className="tunnel-btn delete"
                        disabled={isActive}
                        onClick={() => handleDelete(t.id)}
                        title="Delete Tunnel"
                      >
                        🗑️
                      </button>
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </div>

        {/* Edit / New Tunnel Drawer */}
        {editing && (
          <div className="tunnel-edit-overlay">
            <form className="tunnel-edit-form" onSubmit={handleSaveEdit}>
              <h3>{tunnels.some((t) => t.id === editing.id) ? "Edit Tunnel" : "New Tunnel Configuration"}</h3>

              <div className="form-group">
                <label>Tunnel Name</label>
                <input
                  type="text"
                  required
                  value={editing.name}
                  onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                  placeholder="e.g. Postgres DB or SOCKS Proxy"
                />
              </div>

              <div className="form-group">
                <label>Tunnel Type</label>
                <div className="tunnel-type-selector">
                  <label className={`type-option ${editing.kind === "local" ? "selected" : ""}`}>
                    <input
                      type="radio"
                      name="tunnelKind"
                      checked={editing.kind === "local"}
                      onChange={() => setEditing({ ...editing, kind: "local" })}
                    />
                    <div>
                      <strong>Local (-L)</strong>
                      <small>Forward local port to remote destination</small>
                    </div>
                  </label>

                  <label className={`type-option ${editing.kind === "dynamic" ? "selected" : ""}`}>
                    <input
                      type="radio"
                      name="tunnelKind"
                      checked={editing.kind === "dynamic"}
                      onChange={() => setEditing({ ...editing, kind: "dynamic" })}
                    />
                    <div>
                      <strong>Dynamic SOCKS5 (-D)</strong>
                      <small>Local SOCKS5 proxy via remote SSH server</small>
                    </div>
                  </label>

                  <label className={`type-option ${editing.kind === "remote" ? "selected" : ""}`}>
                    <input
                      type="radio"
                      name="tunnelKind"
                      checked={editing.kind === "remote"}
                      onChange={() => setEditing({ ...editing, kind: "remote" })}
                    />
                    <div>
                      <strong>Remote (-R)</strong>
                      <small>Forward remote port back to local service</small>
                    </div>
                  </label>
                </div>
              </div>

              <div className="form-group">
                <label>SSH Connection Profile / Target</label>
                <select
                  value={
                    editing.ssh_spec.kind === "ssh"
                      ? `${editing.ssh_spec.user}@${editing.ssh_spec.host}:${editing.ssh_spec.port}`
                      : ""
                  }
                  onChange={(e) => {
                    const match = profiles.find(
                      (p) =>
                        p.spec.kind === "ssh" &&
                        `${p.spec.user}@${p.spec.host}:${p.spec.port}` === e.target.value,
                    );
                    if (match) {
                      setEditing({ ...editing, ssh_spec: match.spec });
                    }
                  }}
                >
                  {profiles.map((p) => (
                    <option
                      key={p.id}
                      value={
                        p.spec.kind === "ssh"
                          ? `${p.spec.user}@${p.spec.host}:${p.spec.port}`
                          : ""
                      }
                    >
                      {p.name} ({p.spec.kind === "ssh" ? `${p.spec.user}@${p.spec.host}` : ""})
                    </option>
                  ))}
                </select>
              </div>

              <div className="form-row">
                <div className="form-group flex-1">
                  <label>Local Bind Address</label>
                  <input
                    type="text"
                    required
                    value={editing.local_addr}
                    onChange={(e) => setEditing({ ...editing, local_addr: e.target.value })}
                    placeholder="127.0.0.1"
                  />
                </div>
                <div className="form-group flex-1">
                  <label>Local Port</label>
                  <input
                    type="number"
                    required
                    min={1}
                    max={65535}
                    value={editing.local_port}
                    onChange={(e) => setEditing({ ...editing, local_port: parseInt(e.target.value) || 0 })}
                  />
                </div>
              </div>

              {editing.kind !== "dynamic" && (
                <div className="form-row">
                  <div className="form-group flex-1">
                    <label>{editing.kind === "local" ? "Remote Destination Host" : "Remote Bind Host"}</label>
                    <input
                      type="text"
                      required
                      value={editing.target_host}
                      onChange={(e) => setEditing({ ...editing, target_host: e.target.value })}
                      placeholder="127.0.0.1"
                    />
                  </div>
                  <div className="form-group flex-1">
                    <label>{editing.kind === "local" ? "Remote Destination Port" : "Remote Port"}</label>
                    <input
                      type="number"
                      required
                      min={1}
                      max={65535}
                      value={editing.target_port}
                      onChange={(e) => setEditing({ ...editing, target_port: parseInt(e.target.value) || 0 })}
                    />
                  </div>
                </div>
              )}

              <div className="form-actions">
                <button type="button" className="secondary sm" onClick={() => setEditing(null)}>
                  Cancel
                </button>
                <button type="submit" className="primary sm">
                  Save Tunnel
                </button>
              </div>
            </form>
          </div>
        )}

        {/* SSH Password Prompt for starting tunnel */}
        {pwPromptFor && (
          <div className="tunnel-edit-overlay">
            <form
              className="tunnel-edit-form"
              onSubmit={(e) => {
                e.preventDefault();
                pwPromptFor.resolve(passwordInput);
                setPwPromptFor(null);
                setPasswordInput("");
              }}
            >
              <h3>Enter SSH Password</h3>
              <p>
                Authentication required for{" "}
                <strong>
                  {pwPromptFor.tunnel.ssh_spec.kind === "ssh"
                    ? `${pwPromptFor.tunnel.ssh_spec.user}@${pwPromptFor.tunnel.ssh_spec.host}`
                    : ""}
                </strong>
              </p>
              <div className="form-group">
                <input
                  type="password"
                  autoFocus
                  required
                  placeholder="SSH Password"
                  value={passwordInput}
                  onChange={(e) => setPasswordInput(e.target.value)}
                />
              </div>
              <div className="form-actions">
                <button
                  type="button"
                  className="secondary sm"
                  onClick={() => {
                    setPwPromptFor(null);
                    setPasswordInput("");
                  }}
                >
                  Cancel
                </button>
                <button type="submit" className="primary sm">
                  Connect & Forward
                </button>
              </div>
            </form>
          </div>
        )}
      </div>
    </div>
  );
}
