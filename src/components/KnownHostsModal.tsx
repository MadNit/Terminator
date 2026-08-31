import { useState, useEffect } from "react";
import {
  KnownHostEntry,
  listKnownHosts,
  deleteKnownHost,
  addKnownHost,
} from "../lib/api";

type Props = {
  open: boolean;
  onClose: () => void;
};

export function KnownHostsModal({ open, onClose }: Props) {
  const [entries, setEntries] = useState<KnownHostEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [keyTypeFilter, setKeyTypeFilter] = useState("all");

  // Manual host key add form state
  const [isAdding, setIsAdding] = useState(false);
  const [newHost, setNewHost] = useState("");
  const [newKeyType, setNewKeyType] = useState("ssh-ed25519");
  const [newPublicKey, setNewPublicKey] = useState("");
  const [newComment, setNewComment] = useState("");
  const [addError, setAddError] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const fetchKnownHosts = async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await listKnownHosts();
      setEntries(data);
    } catch (err: any) {
      setError(err?.message || String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    if (open) {
      fetchKnownHosts();
    }
  }, [open]);

  if (!open) return null;

  const filteredEntries = entries.filter((e) => {
    const matchesSearch =
      filter === "" ||
      e.host_pattern.toLowerCase().includes(filter.toLowerCase()) ||
      e.fingerprint_sha256.toLowerCase().includes(filter.toLowerCase()) ||
      (e.comment && e.comment.toLowerCase().includes(filter.toLowerCase()));

    const matchesType =
      keyTypeFilter === "all" || e.key_type.toLowerCase() === keyTypeFilter.toLowerCase();

    return matchesSearch && matchesType;
  });

  const uniqueKeyTypes = Array.from(new Set(entries.map((e) => e.key_type))).filter(Boolean);

  const handleRevoke = async (entry: KnownHostEntry) => {
    const confirmed = window.confirm(
      `Revoke host key for "${entry.host_pattern}" (${entry.key_type})?\n\nThis will remove the host from known_hosts. Next time you connect, you will be prompted or TOFU will re-learn the key.`,
    );
    if (!confirmed) return;

    try {
      await deleteKnownHost(entry.line_number, entry.host_pattern);
      await fetchKnownHosts();
    } catch (err: any) {
      setError(err?.message || String(err));
    }
  };

  const handleAddHostKey = async (e: React.FormEvent) => {
    e.preventDefault();
    setAddError(null);

    if (!newHost.trim() || !newPublicKey.trim()) {
      setAddError("Host name/IP and Public Key are required.");
      return;
    }

    // If user pasted a full line like 'ssh-ed25519 AAAAC3... user@host'
    let finalType = newKeyType;
    let finalKey = newPublicKey.trim();
    let finalComment = newComment.trim();

    const parts = finalKey.split(/\s+/);
    if (parts.length >= 2 && parts[0].startsWith("ssh-") || parts[0].startsWith("ecdsa-")) {
      finalType = parts[0];
      finalKey = parts[1];
      if (parts.length >= 3 && !finalComment) {
        finalComment = parts.slice(2).join(" ");
      }
    }

    try {
      await addKnownHost(
        newHost.trim(),
        finalType.trim(),
        finalKey,
        finalComment || null,
      );
      setIsAdding(false);
      setNewHost("");
      setNewPublicKey("");
      setNewComment("");
      await fetchKnownHosts();
    } catch (err: any) {
      setAddError(err?.message || String(err));
    }
  };

  const copyFingerprint = (id: string, text: string) => {
    navigator.clipboard.writeText(text);
    setCopiedId(id);
    setTimeout(() => setCopiedId(null), 2000);
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal known-hosts-modal"
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 880,
          maxWidth: "94vw",
          height: 620,
          maxHeight: "88vh",
          display: "flex",
          flexDirection: "column",
          padding: 0,
          overflow: "hidden",
          background: "var(--ink-850)",
        }}
      >
        {/* Header */}
        <div className="modal-header">
          <div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <span style={{ fontSize: 16 }}>🛡️</span>
              <h2 style={{ fontSize: 14, fontWeight: 600 }}>SSH Known Hosts & Host Keys</h2>
            </div>
            <p className="modal-subtitle">
              Inspect trusted server public keys, SHA256 fingerprints, and revoke untrusted/modified hosts.
            </p>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            {!isAdding && (
              <button
                type="button"
                className="btn btn-primary"
                style={{ padding: "4px 10px", fontSize: 12 }}
                onClick={() => setIsAdding(true)}
              >
                + Trust New Key
              </button>
            )}
            <button type="button" className="modal-close" onClick={onClose}>
              ✕
            </button>
          </div>
        </div>

        {/* Body */}
        <div
          style={{
            padding: "14px 18px",
            display: "flex",
            flexDirection: "column",
            flex: 1,
            minHeight: 0,
            background: "var(--ink-900)",
            gap: 12,
          }}
        >
          {error && (
            <div
              style={{
                padding: "8px 12px",
                background: "rgba(239, 68, 68, 0.1)",
                border: "1px solid rgba(239, 68, 68, 0.3)",
                borderRadius: "var(--radius)",
                color: "#fca5a5",
                fontSize: 12,
              }}
            >
              {error}
            </div>
          )}

          {/* Add Key Panel */}
          {isAdding ? (
            <form
              onSubmit={handleAddHostKey}
              style={{
                background: "var(--ink-800)",
                border: "1px solid var(--ink-600)",
                borderRadius: "var(--radius)",
                padding: 14,
                display: "flex",
                flexDirection: "column",
                gap: 10,
              }}
            >
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span style={{ fontSize: 13, fontWeight: 600, color: "var(--fg)" }}>
                  Trust & Add Server Public Key
                </span>
                <button
                  type="button"
                  className="btn btn-ghost"
                  style={{ fontSize: 11, padding: "2px 6px" }}
                  onClick={() => setIsAdding(false)}
                >
                  Cancel
                </button>
              </div>

              {addError && (
                <div style={{ color: "#fca5a5", fontSize: 12 }}>{addError}</div>
              )}

              <div style={{ display: "flex", gap: 10 }}>
                <div style={{ flex: 2 }}>
                  <label className="field-label">Host / IP / Pattern</label>
                  <input
                    type="text"
                    className="field-input"
                    placeholder="e.g. 192.168.1.100 or bastion.example.com"
                    value={newHost}
                    onChange={(e) => setNewHost(e.target.value)}
                    required
                  />
                </div>
                <div style={{ flex: 1 }}>
                  <label className="field-label">Key Algorithm</label>
                  <select
                    className="field-input"
                    value={newKeyType}
                    onChange={(e) => setNewKeyType(e.target.value)}
                  >
                    <option value="ssh-ed25519">ssh-ed25519</option>
                    <option value="ecdsa-sha2-nistp256">ecdsa-sha2-nistp256</option>
                    <option value="ecdsa-sha2-nistp384">ecdsa-sha2-nistp384</option>
                    <option value="ecdsa-sha2-nistp521">ecdsa-sha2-nistp521</option>
                    <option value="rsa-sha2-512">rsa-sha2-512</option>
                    <option value="rsa-sha2-256">rsa-sha2-256</option>
                    <option value="ssh-rsa">ssh-rsa</option>
                  </select>
                </div>
              </div>

              <div>
                <label className="field-label">Public Key (Base64 or OpenSSH format)</label>
                <textarea
                  className="field-input"
                  rows={2}
                  placeholder="AAAAC3NzaC1lZDI1NTE5AAAAI... or paste entire ssh-ed25519 AAAAC3..."
                  value={newPublicKey}
                  onChange={(e) => setNewPublicKey(e.target.value)}
                  style={{ fontFamily: "var(--mono)", fontSize: 11 }}
                  required
                />
              </div>

              <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                <div style={{ flex: 1 }}>
                  <input
                    type="text"
                    className="field-input"
                    placeholder="Optional comment / server tag"
                    value={newComment}
                    onChange={(e) => setNewComment(e.target.value)}
                  />
                </div>
                <button type="submit" className="btn btn-primary" style={{ padding: "6px 14px" }}>
                  Save to known_hosts
                </button>
              </div>
            </form>
          ) : (
            /* Search and Filter bar */
            <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
              <input
                type="text"
                placeholder="Search by hostname, IP pattern, SHA256 fingerprint, or comment..."
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                style={{
                  flex: 1,
                  padding: "7px 10px",
                  background: "var(--ink-700)",
                  border: "1px solid var(--ink-600)",
                  borderRadius: "var(--radius)",
                  color: "var(--fg)",
                  fontSize: 12.5,
                  outline: "none",
                }}
              />
              <select
                value={keyTypeFilter}
                onChange={(e) => setKeyTypeFilter(e.target.value)}
                style={{
                  padding: "7px 10px",
                  background: "var(--ink-700)",
                  border: "1px solid var(--ink-600)",
                  borderRadius: "var(--radius)",
                  color: "var(--fg)",
                  fontSize: 12,
                  outline: "none",
                }}
              >
                <option value="all">All Key Types ({entries.length})</option>
                {uniqueKeyTypes.map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
              </select>
            </div>
          )}

          {/* List Entries */}
          <div
            style={{
              flex: 1,
              minHeight: 0,
              overflowY: "auto",
              display: "flex",
              flexDirection: "column",
              gap: 8,
              paddingRight: 4,
            }}
          >
            {loading ? (
              <div style={{ padding: 24, textAlign: "center", color: "var(--dim)", fontSize: 13 }}>
                Loading known hosts...
              </div>
            ) : filteredEntries.length === 0 ? (
              <div
                style={{
                  padding: 32,
                  textAlign: "center",
                  color: "var(--dim)",
                  background: "var(--ink-800)",
                  borderRadius: "var(--radius)",
                  border: "1px dashed var(--ink-600)",
                  fontSize: 13,
                }}
              >
                {entries.length === 0
                  ? "No known host entries found. Keys are learned automatically upon SSH connection (TOFU) or can be trusted manually."
                  : "No host keys match the current search filter."}
              </div>
            ) : (
              filteredEntries.map((entry) => (
                <div
                  key={entry.id}
                  style={{
                    background: "var(--ink-800)",
                    border: "1px solid var(--ink-650)",
                    borderRadius: "var(--radius)",
                    padding: "10px 14px",
                    display: "flex",
                    flexDirection: "column",
                    gap: 6,
                    transition: "border-color 0.15s",
                  }}
                >
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                      <span
                        style={{
                          fontWeight: 600,
                          fontSize: 13,
                          color: "var(--fg)",
                          fontFamily: "var(--mono)",
                        }}
                      >
                        {entry.host_pattern}
                      </span>
                      <span
                        style={{
                          fontSize: 10.5,
                          padding: "1px 6px",
                          background: "var(--ink-700)",
                          border: "1px solid var(--ink-600)",
                          borderRadius: "var(--radius)",
                          color: "var(--lime)",
                          fontFamily: "var(--mono)",
                        }}
                      >
                        {entry.key_type}
                      </span>
                      {entry.is_hashed && (
                        <span
                          style={{
                            fontSize: 10,
                            padding: "1px 5px",
                            background: "rgba(168, 85, 247, 0.15)",
                            border: "1px solid rgba(168, 85, 247, 0.3)",
                            borderRadius: "var(--radius)",
                            color: "#c084fc",
                          }}
                        >
                          Hashed Host
                        </span>
                      )}
                      {entry.comment && (
                        <span style={{ fontSize: 11, color: "var(--dim)" }}>
                          ({entry.comment})
                        </span>
                      )}
                    </div>
                    <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        style={{
                          fontSize: 11,
                          padding: "3px 8px",
                          color: copiedId === entry.id ? "var(--lime)" : "var(--muted)",
                        }}
                        onClick={() => copyFingerprint(entry.id, entry.fingerprint_sha256)}
                        title="Copy SHA256 Fingerprint"
                      >
                        {copiedId === entry.id ? "✓ Copied" : "Copy FP"}
                      </button>
                      <button
                        type="button"
                        className="btn btn-ghost"
                        style={{
                          fontSize: 11,
                          padding: "3px 8px",
                          color: "#f87171",
                        }}
                        onClick={() => handleRevoke(entry)}
                        title="Revoke and delete this host key"
                      >
                        Revoke
                      </button>
                    </div>
                  </div>

                  {/* Fingerprint details */}
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 12,
                      fontSize: 11,
                      color: "var(--muted)",
                      fontFamily: "var(--mono)",
                    }}
                  >
                    <span>
                      <strong style={{ color: "var(--dim)" }}>SHA256:</strong>{" "}
                      {entry.fingerprint_sha256}
                    </span>
                    <span style={{ fontSize: 10, color: "var(--dim)" }}>
                      Line #{entry.line_number}
                    </span>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Footer */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "8px 16px",
            background: "var(--ink-850)",
            borderTop: "1px solid var(--ink-600)",
            fontSize: 11,
            color: "var(--dim)",
          }}
        >
          <div>
            Total: <strong>{entries.length}</strong> trusted host keys | TOFU Policy: <strong>Accept Unknown & Verify Unchanged</strong>
          </div>
          <button type="button" className="btn btn-ghost" style={{ fontSize: 11, padding: "3px 8px" }} onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
