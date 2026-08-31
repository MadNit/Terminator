import { useEffect, useState } from "react";
import { PasswordField } from "./PasswordField";
import { secretsBackend, listProfiles } from "../lib/api";
import type { Profile, SshAuth, TransportSpec } from "../lib/api";

export type NewConnection = {
  name: string;
  spec: TransportSpec;
  /** Password to stash in the keychain, if the user supplied one. */
  password?: string;
  save: boolean;
  /** Present when this replaces an existing profile rather than adding one. */
  editId?: string;
};

/**
 * Dialog for starting or saving a connection.
 *
 * Passwords are handed to the backend for the keychain and are never placed
 * in the profile itself -- the profile only records which auth method to use.
 */
/**
 * Hostnames, usernames, paths and shells are machine identifiers, not prose.
 * WebKit auto-capitalises and auto-corrects text inputs by default, which
 * silently turned "vsdevops" into "Vsdevops" -- and SSH usernames are
 * case-sensitive, so the login just failed.
 */
const verbatim = {
  autoCapitalize: "none",
  autoCorrect: "off",
  spellCheck: false,
} as const;

export function ConnectDialog({
  onCancel,
  onConnect,
  edit,
}: {
  onCancel: () => void;
  onConnect: (c: NewConnection) => void;
  /** When set, the dialog edits this profile instead of creating a new one. */
  edit?: Profile;
}) {
  const e = edit?.spec;
  const [kind, setKind] = useState<"local" | "ssh" | "rdp">(e?.kind ?? "ssh");
  const [name, setName] = useState(edit?.name ?? "");
  const [host, setHost] = useState(e && e.kind !== "local" ? e.host : "");
  const [port, setPort] = useState(e && e.kind !== "local" ? e.port : 22);
  const [user, setUser] = useState(e && e.kind !== "local" ? e.user : "");
  const [method, setMethod] = useState<SshAuth["method"]>(
    e?.kind === "ssh" ? e.auth.method : "agent",
  );
  const [keyPath, setKeyPath] = useState(
    e?.kind === "ssh" && e.auth.method === "key"
      ? e.auth.path
      : "~/.ssh/id_ed25519",
  );
  const [password, setPassword] = useState("");
  const [domain, setDomain] = useState(
    (e?.kind === "rdp" && e.domain) || "",
  );
  // Jump host (ProxyJump / Bastion)
  const [jumpHostId, setJumpHostId] = useState<string>(() => {
    if (e?.kind === "ssh" && e.jump_host && e.jump_host.kind === "ssh") {
      return `${e.jump_host.user}@${e.jump_host.host}:${e.jump_host.port}`;
    }
    return "";
  });
  const [availableJumpProfiles, setAvailableJumpProfiles] = useState<Profile[]>([]);
  // The hint must name the store actually in use; claiming "OS keychain" while
  // running on the encrypted-file fallback is simply wrong.
  const [secretHint, setSecretHint] = useState(
    "Stored in the system credential store, never in the profile.",
  );

  useEffect(() => {
    void listProfiles().then((list) => {
      setAvailableJumpProfiles(list.filter((p) => p.spec.kind === "ssh" && (!edit || p.id !== edit.id)));
    }).catch(() => {});
  }, [edit]);

  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  useEffect(() => {
    void secretsBackend()
      .then((b) =>
        setSecretHint(
          b === "keychain"
            ? "Stored in the OS keychain, never in the profile."
            : "Stored in the encrypted vault, never in the profile.",
        ),
      )
      .catch(() => {});
  }, []);
  const [shell, setShell] = useState(e?.kind === "local" ? (e.shell ?? "") : "");
  // Editing an existing profile always writes it back; offering "don't save"
  // there would just be a confusing way to discard the edit.
  const [save, setSave] = useState(true);
  const [error, setError] = useState("");

  const pickKind = (k: "local" | "ssh" | "rdp") => {
    setKind(k);
    setPort(k === "rdp" ? 3389 : 22);
  };

  const submit = () => {
    if (kind !== "local" && !host.trim()) {
      setError("host is required");
      return;
    }
    if (kind !== "local" && !user.trim()) {
      setError("user is required");
      return;
    }

    // RDP is always password-authenticated; SSH only when that method is
    // selected. Getting this wrong means the credential is silently dropped.
    const usesPassword = kind === "rdp" || method === "password";

    let spec: TransportSpec;
    if (kind === "local") {
      spec = { kind: "local", shell: shell.trim() || null, cwd: null };
    } else if (kind === "ssh") {
      const auth: SshAuth =
        method === "key"
          ? { method: "key", path: keyPath.trim() }
          : { method };
      let jumpSpec: TransportSpec | null = null;
      if (jumpHostId) {
        const found = availableJumpProfiles.find(
          (p) =>
            p.spec.kind === "ssh" &&
            `${p.spec.user}@${p.spec.host}:${p.spec.port}` === jumpHostId,
        );
        if (found) {
          jumpSpec = found.spec;
        }
      }
      spec = {
        kind: "ssh",
        host: host.trim(),
        port,
        user: user.trim(),
        auth,
        jump_host: jumpSpec,
      };
    } else {
      spec = {
        kind: "rdp",
        host: host.trim(),
        port,
        user: user.trim(),
        domain: domain.trim() || null,
      };
    }

    onConnect({
      name: name.trim() || (kind === "local" ? "shell" : `${user}@${host}`),
      spec,
      password: usesPassword && password ? password : undefined,
      save: edit ? true : save,
      editId: edit?.id,
    });
  };

  return (
    <div className="modal-backdrop" onClick={onCancel}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{edit ? "Edit connection" : "New connection"}</h2>

        <div className="seg">
          {(["local", "ssh", "rdp"] as const).map((k) => (
            <button
              key={k}
              className={kind === k ? "on" : ""}
              onClick={() => pickKind(k)}
            >
              {k.toUpperCase()}
            </button>
          ))}
        </div>

        <label>
          Name
          <input
            value={name}
            placeholder="optional"
            onChange={(ev) => setName(ev.target.value)}
          />
        </label>

        {kind === "local" ? (
          <label>
            Shell
            <input
              {...verbatim}
              value={shell}
              placeholder="default shell"
              onChange={(ev) => setShell(ev.target.value)}
            />
          </label>
        ) : (
          <>
            <div className="row">
              <label className="grow">
                Host
                <input
                  {...verbatim}
                  value={host}
                  autoFocus
                  placeholder="server.example.com"
                  onChange={(ev) => setHost(ev.target.value)}
                />
              </label>
              <label className="port">
                Port
                <input
                  type="number"
                  value={port}
                  onChange={(ev) => setPort(Number(ev.target.value))}
                />
              </label>
            </div>
            <label>
              User
              <input
                {...verbatim}
                value={user}
                placeholder="username (case-sensitive)"
                onChange={(ev) => setUser(ev.target.value)}
              />
            </label>
          </>
        )}

        {kind === "ssh" && (
          <>
            <div className="seg small">
              {(["agent", "key", "password"] as const).map((m) => (
                <button
                  key={m}
                  className={method === m ? "on" : ""}
                  onClick={() => setMethod(m)}
                >
                  {m}
                </button>
              ))}
            </div>
            {method === "key" && (
              <label>
                Private key
                <input
                  {...verbatim}
                  value={keyPath}
                  placeholder="~/.ssh/id_ed25519"
                  onChange={(ev) => setKeyPath(ev.target.value)}
                />
              </label>
            )}
            {method === "password" && (
              <label>
                Password
                <PasswordField
                  value={password}
                  onChange={setPassword}
                  autoComplete="current-password"
                  placeholder={edit ? "leave blank to keep current" : undefined}
                />
                <span className="hint">
                  {edit
                    ? `Leave blank to keep the saved password. ${secretHint}`
                    : secretHint}
                </span>
              </label>
            )}

            <label>
              Jump Host / ProxyJump <span className="dim">(Bastion - optional)</span>
              <select
                value={jumpHostId}
                onChange={(ev) => setJumpHostId(ev.target.value)}
                style={{
                  width: "100%",
                  marginTop: "4px",
                  padding: "7px 9px",
                  background: "var(--ink-700)",
                  border: "1px solid var(--ink-600)",
                  borderRadius: "var(--radius)",
                  color: "var(--fg)",
                  font: "inherit",
                  fontSize: "12.5px",
                }}
              >
                <option value="">Direct Connection (No Jump Host)</option>
                {availableJumpProfiles.map((p) => {
                  const val = `${p.spec.kind === "ssh" ? p.spec.user : "" }@${p.spec.kind === "ssh" ? p.spec.host : ""}:${p.spec.kind === "ssh" ? p.spec.port : ""}`;
                  return (
                    <option key={p.id} value={val}>
                      {p.name} ({val})
                    </option>
                  );
                })}
              </select>
              <span className="hint">
                Route SSH connection through an intermediate jump server (ssh -J).
              </span>
            </label>
          </>
        )}

        {kind === "rdp" && (
          <>
            <label>
              Domain <span className="dim">(optional)</span>
              <input
                {...verbatim}
                value={domain}
                placeholder="CORP  — leave blank for a local account"
                onChange={(ev) => setDomain(ev.target.value)}
              />
            </label>
            <label>
              Password
              <PasswordField
                value={password}
                onChange={setPassword}
                autoComplete="current-password"
                placeholder={edit ? "leave blank to keep current" : undefined}
              />
              <span className="hint">
                {edit
                  ? `Leave blank to keep the saved password. ${secretHint}`
                  : secretHint}
              </span>
            </label>
            {/* No agent or key equivalent exists: NLA/CredSSP authenticates
                with the password itself before the desktop is created. */}
            <p className="hint">
              RDP connects with NLA (CredSSP) over TLS, so a password is
              required.
            </p>
          </>
        )}

        {!edit && (
          <label className="check">
            <input
              type="checkbox"
              checked={save}
              onChange={(ev) => setSave(ev.target.checked)}
            />
            Save as a profile
          </label>
        )}

        {error && <p className="hint warn">{error}</p>}

        <div className="actions">
          <button onClick={onCancel}>Cancel</button>
          <button className="primary" onClick={submit}>
            {edit ? "Save" : "Connect"}
          </button>
        </div>
      </div>
    </div>
  );
}
