import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import {
  closeSession,
  decodeB64,
  openSession,
  resizeSession,
  writeSession,
  type TransportSpec,
} from "../lib/api";

interface Props {
  open: boolean;
  spec?: TransportSpec;
  secretRef?: string;
  password?: string;
  jumpSecretRef?: string;
  jumpPassword?: string;
  hostLabel?: string | null;
  onClose: () => void;
}

export function MiniTerminalDrawer({
  open,
  spec,
  secretRef,
  password,
  jumpSecretRef,
  jumpPassword,
  hostLabel,
  onClose,
}: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const idRef = useRef<string | null>(null);
  const [height, setHeight] = useState<number>(240);
  const [isDragging, setIsDragging] = useState<boolean>(false);
  const startYRef = useRef<number>(0);
  const startHeightRef = useRef<number>(240);
  const [sessionKey, setSessionKey] = useState<number>(1);
  const [connected, setConnected] = useState<boolean>(false);

  // Resize handler for bottom drawer
  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    setIsDragging(true);
    startYRef.current = e.clientY;
    startHeightRef.current = height;

    const handleMouseMove = (moveEvent: MouseEvent) => {
      const delta = startYRef.current - moveEvent.clientY;
      setHeight(Math.max(100, Math.min(600, startHeightRef.current + delta)));
      fitRef.current?.fit();
    };

    const handleMouseUp = () => {
      setIsDragging(false);
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
  };

  useEffect(() => {
    if (!open || !hostRef.current) return;

    let unmounted = false;
    let localId: string | null = null;

    const term = new Terminal({
      allowTransparency: false,
      fontFamily:
        '"JetBrains Mono", ui-monospace, Menlo, Consolas, "DejaVu Sans Mono", monospace',
      fontSize: 12,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      macOptionIsMeta: true,
      theme: {
        background: "#0d111a",
        foreground: "#f3f4f6",
        cursor: "#bef264",
        cursorAccent: "#0d111a",
        selectionBackground: "rgba(190, 242, 100, 0.25)",
      },
    });
    termRef.current = term;

    const fit = new FitAddon();
    fitRef.current = fit;
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
    term.loadAddon(
      new WebLinksAddon((_event, uri) => {
        window.open(uri, "_blank", "noopener,noreferrer");
      }),
    );

    term.open(hostRef.current);
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // Fall back to canvas
    }

    const effectiveSpec: TransportSpec = spec || { kind: "local", shell: null, cwd: null };

    const connect = async () => {
      try {
        const { cols, rows } = fit.proposeDimensions() ?? { cols: 80, rows: 24 };
        const id = await openSession(
          effectiveSpec,
          cols,
          rows,
          (evt) => {
            if (evt.type === "output") {
              const bytes = decodeB64(evt.data);
              term.write(bytes);
            } else if (evt.type === "exit") {
              term.writeln("\r\n\x1b[90m[Process completed]\x1b[0m");
              setConnected(false);
            }
          },
          secretRef,
          password,
          jumpSecretRef,
          jumpPassword,
        );

        if (unmounted) {
          void closeSession(id);
          return;
        }

        localId = id;
        idRef.current = id;
        setConnected(true);

        term.onData((data) => {
          if (idRef.current) {
            void writeSession(idRef.current, data);
          }
        });

        term.onResize(({ cols, rows }) => {
          if (idRef.current) {
            void resizeSession(idRef.current, cols, rows);
          }
        });

        fit.fit();
        term.focus();
      } catch (err) {
        if (!unmounted) {
          term.writeln(`\r\n\x1b[31mFailed to start terminal: ${String(err)}\x1b[0m`);
          setConnected(false);
        }
      }
    };

    void connect();

    const handleResize = () => {
      fit.fit();
    };
    window.addEventListener("resize", handleResize);

    return () => {
      unmounted = true;
      window.removeEventListener("resize", handleResize);
      if (localId) {
        void closeSession(localId);
      }
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [open, sessionKey, spec, secretRef, password, jumpSecretRef, jumpPassword]);

  useEffect(() => {
    if (open) {
      setTimeout(() => {
        fitRef.current?.fit();
        termRef.current?.focus();
      }, 50);
    }
  }, [open, height]);

  if (!open) return null;

  return (
    <div className="editor-terminal-drawer" style={{ height }}>
      <div
        className={`editor-terminal-resizer ${isDragging ? "dragging" : ""}`}
        onMouseDown={handleMouseDown}
        title="Drag to resize terminal panel"
      />
      <div className="editor-terminal-header">
        <div className="editor-terminal-title">
          <span>💻 Terminal</span>
          {hostLabel && (
            <span style={{ color: "var(--lime)", fontSize: 10 }}>({hostLabel})</span>
          )}
          <span
            style={{
              display: "inline-block",
              width: 6,
              height: 6,
              borderRadius: "50%",
              background: connected ? "var(--lime)" : "var(--muted)",
            }}
          />
        </div>
        <div className="editor-terminal-actions">
          <button
            className="editor-sidebar-action-btn"
            onClick={() => termRef.current?.clear()}
            title="Clear Terminal Output"
          >
            🧹 Clear
          </button>
          <button
            className="editor-sidebar-action-btn"
            onClick={() => setSessionKey((k) => k + 1)}
            title="Restart Terminal Session"
          >
            🔄 Restart
          </button>
          <button
            className="editor-sidebar-action-btn"
            onClick={onClose}
            title="Close Terminal Drawer (Ctrl+`)"
          >
            ✕
          </button>
        </div>
      </div>
      <div className="editor-terminal-canvas" ref={hostRef} />
    </div>
  );
}
