import { useEffect, useRef, useState } from "react";
import { logFrontend, unlockVault, type VaultStatus } from "../lib/api";
import { PasswordField } from "./PasswordField";

/**
 * Blocks the app until the encrypted vault is unlocked.
 *
 * Only shown when the OS keychain is unavailable and we fell back to the
 * file vault -- on macOS/Windows/most desktop Linux this never appears.
 */
export function UnlockGate({
  status,
  onUnlocked,
}: {
  status: VaultStatus;
  onUnlocked: () => void;
}) {
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [reveal, setReveal] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  const firstRun = !status.initialized;

  useEffect(() => {
    inputRef.current?.focus();
    void logFrontend(
      "info",
      `unlock gate shown (${firstRun ? "create" : "unlock"}, backend=${status.backend})`,
    );
  }, [firstRun, status.backend]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return;

    if (firstRun && !reveal && passphrase !== confirm) {
      setError("Passphrases do not match.");
      return;
    }
    if (!passphrase) {
      setError("Enter a passphrase.");
      return;
    }

    setBusy(true);
    setError(null);
    try {
      await unlockVault(passphrase);
      void logFrontend("info", "vault unlocked");
      // Drop the plaintext passphrase from component state immediately.
      setPassphrase("");
      setConfirm("");
      setReveal(false);
      onUnlocked();
    } catch (err) {
      void logFrontend("warn", "vault unlock rejected");
      setError(String(err));
      setPassphrase("");
      setConfirm("");
      inputRef.current?.focus();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="unlock-backdrop">
      <form className="unlock-card" onSubmit={submit}>
        <h2>{firstRun ? "Create a vault passphrase" : "Unlock saved credentials"}</h2>
        <p className="unlock-note">
          No system keychain was found on this machine, so saved passwords are kept
          in an encrypted file. {firstRun
            ? "Choose a passphrase to protect it. There is no recovery if you forget it."
            : "Enter your passphrase to continue."}
        </p>

        <PasswordField
          inputRef={inputRef}
          value={passphrase}
          placeholder="Passphrase"
          autoComplete={firstRun ? "new-password" : "current-password"}
          disabled={busy}
          onChange={(v) => {
            setPassphrase(v);
            // Keep confirm in step while the text is visible, so hiding it
            // again does not resurrect an empty, mismatched field.
            if (reveal) setConfirm(v);
          }}
          onReveal={(r) => {
            setReveal(r);
            if (r) setConfirm(passphrase);
          }}
        />

        {firstRun && !reveal && (
          <input
            type="password"
            placeholder="Confirm passphrase"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            disabled={busy}
          />
        )}

        {error && <div className="unlock-error">{error}</div>}

        <div className="unlock-actions">
          <button type="submit" className="primary" disabled={busy}>
            {busy ? "Working…" : firstRun ? "Create vault" : "Unlock"}
          </button>
        </div>
      </form>
    </div>
  );
}


