import { useEffect } from "react";
import type { Profile } from "../lib/api";
import { CopyButton } from "./CopyButton";
import {
  KIND_LABEL,
  connectBlockedReason,
  describeAuth,
  kindBadge,
} from "../lib/transport";

/**
 * Details for the selected connection, shown as a modal over the session.
 *
 * This is what a sidebar click opens. Connecting is a deliberate button press
 * here rather than a side effect of selecting the host.
 */
export function ProfileView({
  profile,
  connected,
  busy,
  onConnect,
  onDisconnect,
  onDelete,
  onEdit,
  onClose,
}: {
  profile: Profile;
  connected: boolean;
  busy: boolean;
  onConnect: () => void;
  onDisconnect: () => void;
  onDelete: () => void;
  onEdit: () => void;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const { spec } = profile;
  const blocked = connectBlockedReason(spec);

  // `copy` marks the rows holding a literal value worth putting on the
  // clipboard. Type and Auth are derived descriptions ("SSH", "password"), so a
  // copy icon there would be noise rather than help.
  type Row = { k: string; v: string; copy?: boolean };
  const rows: Row[] = [{ k: "Type", v: KIND_LABEL[spec.kind] }];
  if (spec.kind === "local") {
    rows.push({ k: "Shell", v: spec.shell || "system default", copy: !!spec.shell });
  } else {
    rows.push(
      { k: "Host", v: spec.host, copy: true },
      { k: "Port", v: String(spec.port), copy: true },
      { k: "User", v: spec.user, copy: true },
      { k: "Auth", v: describeAuth(spec) },
    );
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal profile-view"
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="pv-head">
          <span className={`badge ${spec.kind}`}>{kindBadge(spec.kind)}</span>
          <h2>{profile.name}</h2>
          <span className={`pv-status ${connected ? "on" : ""}`}>
            {connected ? "Connected" : "Not connected"}
          </span>
          <button
            type="button"
            className="pv-close"
            onClick={onClose}
            aria-label="Close"
            title="Close"
          >
            ×
          </button>
        </header>

        <dl className="pv-grid">
          {rows.map(({ k, v, copy }) => (
            <div key={k} className="pv-row">
              <dt>{k}</dt>
              <dd>{v}</dd>
              {copy && <CopyButton value={v} label={k.toLowerCase()} />}
            </div>
          ))}
        </dl>

        {blocked && <p className="hint warn pv-blocked">{blocked}</p>}

        <div className="actions pv-actions">
          <button
            type="button"
            onClick={onDelete}
            // Deleting a profile whose session is live would leave an orphaned
            // tab pointing at a host that is no longer in the sidebar.
            disabled={connected}
            title={
              connected
                ? "Disconnect before deleting this profile"
                : "Delete profile"
            }
          >
            Delete
          </button>

          <span className="spacer" />

          <button type="button" onClick={onEdit}>
            Edit
          </button>

          {connected ? (
            <button type="button" className="danger" onClick={onDisconnect}>
              Disconnect
            </button>
          ) : (
            <button
              type="button"
              className="primary"
              onClick={onConnect}
              disabled={busy || blocked !== null}
            >
              Connect
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
