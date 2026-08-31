import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { readClipboard, writeClipboard } from "../lib/clipboard";
import {
  closeSession,
  decodeB64,
  logFrontend,
  openSession,
  resizeSession,
  writeSession,
  type TransportSpec,
} from "../lib/api";

interface Props {
  spec: TransportSpec;
  /** Keychain entry holding this connection's password, if any. */
  secretRef?: string;
  /** One-shot password, used when the user chose not to save it. */
  password?: string;
  /** Jump host credential refs */
  jumpSecretRef?: string;
  jumpPassword?: string;
  active: boolean;
  /** Function called when data is typed in this pane; useful for broadcast input */
  onInputData?: (data: string) => void;
  onReady: (id: string) => void;
  onExit: () => void;
  /** Re-run this pane against the same target. */
  onReconnect: () => void;
  /** Close the tab this pane lives in. */
  onClose: () => void;
}

export function TerminalPane({
  spec,
  secretRef,
  password,
  jumpSecretRef,
  jumpPassword,
  active,
  onInputData,
  onReady,
  onExit,
  onReconnect,
  onClose,
}: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const idRef = useRef<string | null>(null);
  // React StrictMode runs mount -> cleanup -> mount again in dev. Tearing the
  // terminal down on that first cleanup would leave a dead pane (and a naive
  // "already started" guard makes it worse: the second mount skips rebuilding,
  // so the terminal stays disposed forever). Instead we defer teardown by a
  // tick; a genuine unmount lets it fire, a StrictMode remount cancels it and
  // keeps the live session -- which also avoids a second SSH login per tab.
  const teardownRef = useRef<(() => void) | null>(null);
  const teardownTimer = useRef<number | null>(null);
  /** Last size pushed to the PTY, to avoid redundant resize round-trips. */
  const sentSize = useRef<{ cols: number; rows: number } | null>(null);
  /** Set by the main effect so the visibility effect can reuse it. */
  const refitRef = useRef<(() => void) | null>(null);
  /** Banner text once the session is over; null while it is live. */
  const [ended, setEnded] = useState<string | null>(null);
  /** Read inside `onData` to switch keystrokes from "send" to "R/X prompt". */
  const endedRef = useRef(false);
  // The main effect runs once, so it would otherwise capture the first
  // render's callbacks forever. Kept in a ref so the R/X keys always reach the
  // current handlers.
  const cb = useRef({ onExit, onReconnect, onClose, onInputData });
  cb.current = { onExit, onReconnect, onClose, onInputData };

  useEffect(() => {
    const deferTeardown = () => {
      teardownTimer.current = window.setTimeout(() => {
        teardownTimer.current = null;
        const fn = teardownRef.current;
        teardownRef.current = null;
        fn?.();
      }, 0);
    };

    // A pending teardown means this is StrictMode's immediate remount rather
    // than a fresh mount: keep everything that's already running.
    if (teardownTimer.current !== null) {
      clearTimeout(teardownTimer.current);
      teardownTimer.current = null;
      return deferTeardown;
    }
    if (!hostRef.current || teardownRef.current) return deferTeardown;

    const term = new Terminal({
      // Opaque on purpose. Transparent backgrounds trigger a known WebGL
      // ghosting bug in WKWebView, and we lose nothing by staying opaque.
      allowTransparency: false,
      fontFamily:
        '"JetBrains Mono", ui-monospace, Menlo, Consolas, "DejaVu Sans Mono", monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 10000,
      macOptionIsMeta: true,
      theme: {
        background: "#0d111a",
        foreground: "#f3f4f6",
        cursor: "#bef264",
        cursorAccent: "#0d111a",
        selectionBackground: "rgba(190, 242, 100, 0.25)",
      },
    });

    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
    term.loadAddon(new WebLinksAddon());
    term.open(hostRef.current);

    // Renderer fallback chain: WebGL -> DOM (xterm v6's built-in).
    // WebKitGTK on Linux and some GPU/driver combos fail here, so this must
    // degrade rather than throw.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => {
        console.warn("WebGL context lost; falling back to DOM renderer");
        webgl.dispose();
      });
      term.loadAddon(webgl);
    } catch (err) {
      console.warn("WebGL renderer unavailable, using DOM renderer:", err);
    }

    // Same trap as refit(): fitting an unmeasurable pane would open the
    // session at a nonsense size. 80x24 is a sane default until it is shown.
    if (hostRef.current.clientWidth > 0 && hostRef.current.clientHeight > 0) {
      fit.fit();
    }
    termRef.current = term;
    // The session is opened at this size, so record it as already sent.
    sentSize.current = { cols: term.cols, rows: term.rows };

    let disposed = false;
    let sawOutput = false;

    /**
     * Enter the "session is over" state exactly once.
     *
     * Both the exit event and a failed connect land here, and either can be
     * followed by the other (a connect that fails after the backend has
     * already reported exit), so this must be idempotent.
     */
    const finish = (banner: string) => {
      if (endedRef.current) return;
      endedRef.current = true;
      setEnded(banner);
      term.write(`\r\n\x1b[90m${banner}\x1b[0m\r\n`);
      term.write(
        "\x1b[90mpress \x1b[0m\x1b[1;38;2;190;242;100mR\x1b[0m\x1b[90m to reconnect" +
          "   \x1b[0m\x1b[1;38;2;190;242;100mX\x1b[0m\x1b[90m to close this tab\x1b[0m\r\n",
      );
      // Keep the pane focused so R/X work without clicking first.
      term.focus();
      cb.current.onExit();
    };

    // Remote transports can take a moment; say so rather than sitting blank.
    if (spec.kind !== "local") {
      const via = spec.kind === "ssh" && spec.jump_host ? ` (via jump host)` : "";
      term.write(
        `\x1b[90mconnecting to ${spec.host}:${spec.port}${via} ...\x1b[0m\r\n`,
      );
    }

    openSession(
      spec,
      term.cols,
      term.rows,
      (ev) => {
      if (disposed) return;
        if (ev.type === "output") {
          if (!sawOutput) {
            sawOutput = true;
            logFrontend("info", `pane first output (${spec.kind})`);
          }
          term.write(decodeB64(ev.data));
        } else if (ev.type === "exit") {
          finish("[session ended]");
        }
      },
      secretRef,
      password,
      jumpSecretRef,
      jumpPassword,
    )
      .then((id) => {
        idRef.current = id;
        // The pane was already torn down while we were connecting; don't leak
        // the remote session.
        if (disposed) {
          void closeSession(id);
          return;
        }
        onReady(id);
        term.focus();
      })
      .catch((err) => {
        term.write(`\r\n\x1b[31m${String(err)}\x1b[0m\r\n`);
        finish("[connection failed]");
      });

    const onData = term.onData((data) => {
      // Once the session is gone there is nowhere to send keystrokes, so the
      // pane becomes a two-key prompt instead of silently swallowing input.
      if (endedRef.current) {
        const k = data.toLowerCase();
        if (k === "r") cb.current.onReconnect();
        else if (k === "x") cb.current.onClose();
        return;
      }
      if (cb.current.onInputData) {
        cb.current.onInputData(data);
      } else if (idRef.current) {
        void writeSession(idRef.current, data);
      }
    });

    /* ---- copy on select (MobaXterm / PuTTY behaviour) ----
     *
     * `onSelectionChange` fires continuously while dragging, so writing to the
     * clipboard on every event would mean hundreds of writes per selection.
     * Instead we debounce, and flush immediately on mouse release so letting go
     * feels instant.
     */
    let copyTimer: number | undefined;
    let lastCopied = "";

    const copySelection = () => {
      const sel = term.getSelection();
      // An empty or whitespace-only selection must not clobber the clipboard:
      // a plain click clears the selection, and losing what you copied a
      // moment ago because you clicked to focus the pane would be infuriating.
      if (!sel || sel.trim() === "") return;
      if (sel === lastCopied) return;
      lastCopied = sel;
      void writeClipboard(sel).then((ok) => {
        if (!ok) logFrontend("warn", "copy-on-select: clipboard write failed");
      });
    };

    const scheduleCopy = () => {
      window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(copySelection, 120);
    };

    const flushCopy = () => {
      window.clearTimeout(copyTimer);
      copySelection();
    };

    const onSelection = term.onSelectionChange(scheduleCopy);
    // Bound to the document, not the host: a drag very often ends with the
    // pointer outside the terminal, and that mouseup never reaches the host.
    document.addEventListener("mouseup", flushCopy);

    /* ---- paste ----
     *
     * Middle-click everywhere (the X11 idiom, which MobaXterm keeps on
     * Windows too) and right-click on Windows/Linux, matching PuTTY. On macOS
     * right-click is left alone: it is the OS context-menu gesture and
     * hijacking it would surprise people.
     *
     * `term.paste()` rather than writeSession() so xterm applies bracketed
     * paste when the shell has asked for it -- without that, pasting a block
     * of text into an editor like vim runs every line through autoindent.
     */
    const isMac = navigator.platform.toLowerCase().includes("mac");

    const pasteFromClipboard = async () => {
      const text = await readClipboard();
      if (text === null) {
        logFrontend("warn", "paste: clipboard read failed");
        return;
      }
      if (text === "") return;
      term.paste(text);
      term.focus();
    };

    const onMouseDown = (e: MouseEvent) => {
      const middle = e.button === 1;
      const right = e.button === 2 && !isMac;
      if (!middle && !right) return;
      // Middle-click would otherwise trigger the webview's autoscroll, and
      // right-click its context menu.
      e.preventDefault();
      void pasteFromClipboard();
    };

    // Suppress the context menu only where we've taken over right-click.
    const onContextMenu = (e: MouseEvent) => {
      if (!isMac) e.preventDefault();
    };

    // Firefox/WebKitGTK fire `auxclick` for middle button; without this the
    // paste can land twice or not at all depending on the engine.
    const onAuxClick = (e: MouseEvent) => {
      if (e.button === 1) e.preventDefault();
    };

    const host = hostRef.current;
    host.addEventListener("mousedown", onMouseDown);
    host.addEventListener("contextmenu", onContextMenu);
    host.addEventListener("auxclick", onAuxClick);

    /**
     * Resize to fit, but only while the pane actually has layout.
     *
     * A hidden pane (`display:none`) must never be fitted. `getComputedStyle`
     * on a display:none subtree returns the *computed* value -- literally the
     * string "100%" for this element -- and FitAddon does `parseInt` on it,
     * yielding 100 *pixels*. That works out to roughly 11x5, which we would
     * then push to the PTY; the shell redraws its prompt at 11 columns and
     * leaves a truncated fragment behind on every single tab switch.
     *
     * clientWidth/clientHeight are a reliable test: both are 0 whenever the
     * element or any ancestor is display:none, whatever the computed styles.
     */
    const refit = () => {
      const el = hostRef.current;
      if (!el?.isConnected || el.clientWidth === 0 || el.clientHeight === 0) {
        return;
      }
      try {
        fit.fit();
      } catch {
        /* renderer not ready yet */
      }
      const { cols, rows } = term;
      if (!idRef.current) return;
      // Skip no-op resizes. Harmless on a local PTY, but each one is a
      // window-change request over the wire for SSH.
      const last = sentSize.current;
      if (last && last.cols === cols && last.rows === rows) return;
      sentSize.current = { cols, rows };
      void resizeSession(idRef.current, cols, rows);
    };
    refitRef.current = refit;

    /**
     * Coalesce observer-driven refits.
     *
     * The sidebar collapse animates over ~180ms, so the ResizeObserver fires
     * on virtually every frame with a slightly different width. Each distinct
     * width is a *different* column count, so the dedup in refit() does not
     * catch them and we would send a dozen window-change requests over SSH for
     * one toggle -- with the shell redrawing its prompt at each intermediate
     * size. Waiting for the width to settle turns that into a single resize.
     */
    let refitTimer: number | undefined;
    const refitSoon = () => {
      window.clearTimeout(refitTimer);
      refitTimer = window.setTimeout(refit, 80);
    };

    // Observe the host element rather than window: split panes later will
    // resize without a window resize event.
    const ro = new ResizeObserver(refitSoon);
    ro.observe(hostRef.current);

    teardownRef.current = () => {
      disposed = true;
      logFrontend("info", `pane teardown (session=${idRef.current ?? "none"})`);
      ro.disconnect();
      window.clearTimeout(refitTimer);
      onData.dispose();
      onSelection.dispose();
      window.clearTimeout(copyTimer);
      document.removeEventListener("mouseup", flushCopy);
      host.removeEventListener("mousedown", onMouseDown);
      host.removeEventListener("contextmenu", onContextMenu);
      host.removeEventListener("auxclick", onAuxClick);
      if (idRef.current) void closeSession(idRef.current);
      term.dispose();
      termRef.current = null;
      refitRef.current = null;
    };

    return deferTeardown;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Refit when this tab becomes visible: it could not be measured while it was
  // hidden, so its size may be stale. refit() itself guards against running
  // before layout exists.
  useEffect(() => {
    if (!active) return;
    // Deferred a tick so the pane is actually displayed (and therefore
    // measurable) before we try to fit it.
    const t = setTimeout(() => {
      refitRef.current?.();
      termRef.current?.focus();
    }, 0);
    return () => clearTimeout(t);
  }, [active]);

  return (
    <>
      <div ref={hostRef} className="term-host" />
      {ended && (
        <div className="ended-bar" role="status">
          <span className="ended-msg">{ended}</span>
          <button className="ended-btn primary" onClick={onReconnect}>
            <kbd>R</kbd> Reconnect
          </button>
          <button className="ended-btn" onClick={onClose}>
            <kbd>X</kbd> Close
          </button>
        </div>
      )}
    </>
  );
}
