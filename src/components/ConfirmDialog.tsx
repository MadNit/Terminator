import { useEffect, useRef } from "react";

/**
 * Confirmation for destructive actions.
 *
 * Cancel takes focus rather than the confirm button: for an irreversible
 * action the safe choice should be the one a stray Enter press selects.
 */
export function ConfirmDialog({
  title,
  message,
  detail,
  confirmLabel = "Delete",
  onConfirm,
  onCancel,
}: {
  title: string;
  message: React.ReactNode;
  detail?: React.ReactNode;
  confirmLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <div className="modal-backdrop" onClick={onCancel}>
      <div
        className="modal confirm-modal"
        role="alertdialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{title}</h2>
        <p className="confirm-message">{message}</p>
        {detail && <p className="hint">{detail}</p>}

        <div className="actions">
          <button type="button" ref={cancelRef} onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="danger" onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
