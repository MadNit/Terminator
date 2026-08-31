import { useState } from "react";

/**
 * Password input with a reveal toggle.
 *
 * Shared so the connect dialog, the password prompt and the vault unlock gate
 * cannot drift apart in behaviour or appearance.
 */
export function PasswordField({
  value,
  onChange,
  placeholder = "Password",
  autoComplete = "current-password",
  disabled,
  autoFocus,
  inputRef,
  onReveal,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  autoComplete?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  inputRef?: React.Ref<HTMLInputElement>;
  /** Notified when visibility toggles, for callers that mirror a confirm field. */
  onReveal?: (revealed: boolean) => void;
}) {
  const [reveal, setReveal] = useState(false);

  const toggle = () => {
    const next = !reveal;
    setReveal(next);
    onReveal?.(next);
  };

  return (
    <div className="unlock-field">
      <input
        ref={inputRef}
        type={reveal ? "text" : "password"}
        placeholder={placeholder}
        autoComplete={autoComplete}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        autoFocus={autoFocus}
      />
      <button
        type="button"
        className="reveal-toggle"
        onClick={toggle}
        // An unlabelled glyph is invisible to a screen reader.
        aria-label={reveal ? "Hide password" : "Show password"}
        aria-pressed={reveal}
        title={reveal ? "Hide password" : "Show password"}
        // Keep it out of the tab order: tabbing from the password field should
        // reach the submit button, not the toggle.
        tabIndex={-1}
        disabled={disabled}
      >
        {reveal ? <EyeOff /> : <Eye />}
      </button>
    </div>
  );
}

/* Inline SVGs: no icon dependency for two glyphs, and they inherit currentColor. */
function Eye() {
  return (
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none"
      stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

function EyeOff() {
  return (
    <svg viewBox="0 0 24 24" width="16" height="16" fill="none"
      stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}
