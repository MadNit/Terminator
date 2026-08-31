import { useEffect, useRef, useState } from "react";
import { PasswordField } from "./PasswordField";

/**
 * Asks for a connection password that isn't in the keychain/vault yet.
 *
 * Without this, a profile whose stored password is missing -- a new machine, a
 * cleared keychain, a switch between secret backends -- fails with an opaque
 * "authentication failed" instead of simply asking.
 */
export function PasswordPrompt({
  title,
  onSubmit,
  onCancel,
}: {
  title: string;
  onSubmit: (password: string, remember: boolean) => void;
  onCancel: () => void;
}) {
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(true);
  const ref = useRef<HTMLInputElement>(null);

  useEffect(() => {
    ref.current?.focus();
  }, []);

  return (
    <div className="modal-backdrop" onClick={onCancel}>
      <form
        className="modal pw-modal"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          if (password) onSubmit(password, remember);
        }}
      >
        <h2>Password</h2>
        <p className="unlock-note">
          No saved password for <strong>{title}</strong>.
        </p>

        <PasswordField
          inputRef={ref}
          value={password}
          onChange={setPassword}
          autoComplete="current-password"
        />

        <label className="pw-remember">
          <input
            type="checkbox"
            checked={remember}
            onChange={(e) => setRemember(e.target.checked)}
          />
          Remember this password
        </label>

        <div className="actions">
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={!password}>
            Connect
          </button>
        </div>
      </form>
    </div>
  );
}
