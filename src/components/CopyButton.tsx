import { useEffect, useRef, useState } from "react";
import { writeClipboard } from "../lib/clipboard";

type State = "idle" | "ok" | "fail";

const RESET_MS = 1200;

/**
 * A small copy-to-clipboard affordance for a single value.
 *
 * Feedback matters more than it looks: a copy that silently fails is
 * indistinguishable from one that worked, and the user only finds out when
 * they paste. So the icon reports the actual result of the write rather than
 * optimistically flipping to a tick.
 */
export function CopyButton({
  value,
  label,
  className = "",
}: {
  value: string;
  /** What is being copied, for the tooltip and screen readers. */
  label: string;
  className?: string;
}) {
  const [state, setState] = useState<State>("idle");
  const timer = useRef<number | undefined>(undefined);

  // A copy right before the modal closes would otherwise leave a timer running
  // against an unmounted component.
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const copy = async (e: React.MouseEvent) => {
    // The row may sit inside a backdrop or a clickable parent; copying must not
    // also dismiss the thing being read.
    e.preventDefault();
    e.stopPropagation();
    const ok = await writeClipboard(value);
    setState(ok ? "ok" : "fail");
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setState("idle"), RESET_MS);
  };

  const title =
    state === "ok"
      ? "Copied"
      : state === "fail"
        ? "Copy failed"
        : `Copy ${label}`;

  return (
    <button
      type="button"
      className={`copy-btn ${state} ${className}`.trim()}
      onClick={copy}
      title={title}
      aria-label={title}
    >
      {state === "ok" ? <TickIcon /> : state === "fail" ? <CrossIcon /> : <CopyIcon />}
      {/* Announce the outcome to screen readers, which cannot see the icon. */}
      <span className="sr-only" role="status">
        {state === "ok" ? "Copied" : state === "fail" ? "Copy failed" : ""}
      </span>
    </button>
  );
}

/* Icons are inline SVG rather than glyphs so they inherit currentColor and stay
 * crisp at any zoom. aria-hidden because the button already has a label. */

function CopyIcon() {
  return (
    <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true" focusable="false">
      <rect
        x="5.5"
        y="5.5"
        width="8"
        height="8"
        rx="1.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
      />
      <path
        d="M10.5 3.5v-.5A1.5 1.5 0 0 0 9 1.5H4A2.5 2.5 0 0 0 1.5 4v5A1.5 1.5 0 0 0 3 10.5h.5"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.4"
        strokeLinecap="round"
      />
    </svg>
  );
}

function TickIcon() {
  return (
    <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true" focusable="false">
      <path
        d="M2.5 8.5l3.5 3.5 7.5-8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.8"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function CrossIcon() {
  return (
    <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true" focusable="false">
      <path
        d="M4 4l8 8M12 4l-8 8"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
    </svg>
  );
}
