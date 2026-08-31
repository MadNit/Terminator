import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { readClipboard, writeClipboard } from "../lib/clipboard";
import { loadTriggers, playChime, sendDesktopNotification } from "../lib/triggers";
import {
  loadAppearanceSettings,
  getThemePreset,
  type TerminalAppearanceSettings,
} from "../lib/themes";
import type { ILink } from "@xterm/xterm";
import {
  closeSession,
  decodeB64,
  logFrontend,
  openSession,
  posixJoin,
  remoteHome,
  resizeSession,
  uploadFile,
  writeSession,
  type TransportSpec,
  type TransferEvent,
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
  /** Open file in Mini-IDE when clicking file:line links */
  onOpenFile?: (path: string, line?: number) => void;
}

function baseName(path: string): string {
  const parts = path.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
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
  onOpenFile,
}: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const idRef = useRef<string | null>(null);
  const teardownRef = useRef<(() => void) | null>(null);
  const teardownTimer = useRef<number | null>(null);
  const sentSize = useRef<{ cols: number; rows: number } | null>(null);
  const refitRef = useRef<(() => void) | null>(null);
  const [ended, setEnded] = useState<string | null>(null);
  const endedRef = useRef(false);

  // Auto-reconnect state
  const [autoReconnectCountdown, setAutoReconnectCountdown] = useState<number | null>(null);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);
  const reconnectTimerRef = useRef<number | null>(null);
  const countdownIntervalRef = useRef<number | null>(null);

  // Drag & drop file upload state
  const [dropHover, setDropHover] = useState(false);
  const homeDirRef = useRef<string | null>(null);
  const currentCwdRef = useRef<string | null>(null);
  const lastTriggerFiredRef = useRef<Map<string, number>>(new Map());
  const [transferStatus, setTransferStatus] = useState<{
    fileName: string;
    transferred: number;
    total: number;
    failed?: string;
  } | null>(null);

  const cb = useRef({ onExit, onReconnect, onClose, onInputData, onOpenFile });
  cb.current = { onExit, onReconnect, onClose, onInputData, onOpenFile };

  const cancelAutoReconnect = () => {
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    if (countdownIntervalRef.current !== null) {
      clearInterval(countdownIntervalRef.current);
      countdownIntervalRef.current = null;
    }
    setAutoReconnectCountdown(null);
  };

  const triggerReconnectNow = () => {
    cancelAutoReconnect();
    cb.current.onReconnect();
  };

  useEffect(() => {
    const deferTeardown = () => {
      teardownTimer.current = window.setTimeout(() => {
        teardownTimer.current = null;
        const fn = teardownRef.current;
        teardownRef.current = null;
        fn?.();
      }, 0);
    };

    if (teardownTimer.current !== null) {
      clearTimeout(teardownTimer.current);
      teardownTimer.current = null;
      return deferTeardown;
    }
    if (!hostRef.current || teardownRef.current) return deferTeardown;

    const initialSettings = loadAppearanceSettings();
    const activePreset = getThemePreset(initialSettings.themeId);

    const term = new Terminal({
      allowTransparency: initialSettings.backgroundOpacity < 1,
      fontFamily: initialSettings.fontFamily,
      fontSize: initialSettings.fontSize,
      lineHeight: initialSettings.lineHeight,
      cursorStyle: initialSettings.cursorStyle,
      cursorBlink: initialSettings.cursorBlink,
      scrollback: 10000,
      macOptionIsMeta: true,
      theme: activePreset.theme,
    });
    termRef.current = term;

    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
    term.loadAddon(
      new WebLinksAddon((_event, uri) => {
        window.open(uri, "_blank", "noopener,noreferrer");
      }),
    );

    // Register Smart Link Provider for file:line jump and IP address copy
    term.registerLinkProvider({
      provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void) {
        const line = term.buffer.active.getLine(bufferLineNumber - 1)?.translateToString(true) ?? "";
        const links: ILink[] = [];

        // Match file:line patterns (e.g. src/App.tsx:42 or ./config.json:12)
        const fileLineRegex = /([a-zA-Z0-9_\-./]+\.[a-zA-Z0-9]+):(\d+)/g;
        let match: RegExpExecArray | null;
        while ((match = fileLineRegex.exec(line)) !== null) {
          const full = match[0];
          const filePath = match[1];
          const lineNum = parseInt(match[2], 10);
          const startX = match.index + 1;
          const endX = startX + full.length;
          links.push({
            range: {
              start: { x: startX, y: bufferLineNumber },
              end: { x: endX, y: bufferLineNumber },
            },
            text: full,
            activate: () => {
              if (cb.current.onOpenFile) {
                cb.current.onOpenFile(filePath, lineNum);
              } else {
                writeClipboard(full);
              }
            },
          });
        }

        // Match IP:port patterns (e.g. 192.168.1.100:3000)
        const ipRegex = /\b(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(?::\d{1,5})?)\b/g;
        while ((match = ipRegex.exec(line)) !== null) {
          const ip = match[1];
          const startX = match.index + 1;
          const endX = startX + ip.length;
          links.push({
            range: {
              start: { x: startX, y: bufferLineNumber },
              end: { x: endX, y: bufferLineNumber },
            },
            text: ip,
            activate: () => {
              writeClipboard(ip);
            },
          });
        }

        callback(links);
      },
    });

    // Listen to appearance live updates
    const onAppearanceChange = (e: Event) => {
      const customEvent = e as CustomEvent<TerminalAppearanceSettings>;
      if (!customEvent.detail) return;
      const s = customEvent.detail;
      const t = getThemePreset(s.themeId);
      term.options.theme = t.theme;
      term.options.fontFamily = s.fontFamily;
      term.options.fontSize = s.fontSize;
      term.options.lineHeight = s.lineHeight;
      term.options.cursorStyle = s.cursorStyle;
      term.options.cursorBlink = s.cursorBlink;
      fit.fit();
    };
    window.addEventListener("terminator:appearance_changed", onAppearanceChange);

    term.open(hostRef.current);
    try {
      term.loadAddon(new WebglAddon());
    } catch (e) {
      logFrontend("warn", `WebGL renderer unavailable, using canvas: ${String(e)}`);
    }

    // Register OSC 7 (current working directory) handler
    term.parser.registerOscHandler(7, (data) => {
      try {
        let p = data.trim();
        if (p.startsWith("file://")) {
          const u = new URL(p);
          p = decodeURIComponent(u.pathname);
        }
        if (p.startsWith("/")) {
          currentCwdRef.current = p;
        }
      } catch {
        // Ignore malformed URI
      }
      return true;
    });

    // Register OSC 133 semantic integration handler for Cwd
    term.parser.registerOscHandler(133, (data) => {
      if (data.includes("Cwd=")) {
        const match = data.match(/Cwd=([^;\x07\x1b]+)/);
        if (match && match[1]) {
          let p = match[1].trim();
          const home = homeDirRef.current;
          if (p.startsWith("~") && home) {
            p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
          }
          if (p.startsWith("/")) {
            currentCwdRef.current = p;
          }
        }
      }
      return false;
    });

    // Track directory changes from window/tab title (common in bash/zsh default prompt configs)
    const onTitle = term.onTitleChange((title) => {
      if (!title) return;
      const home = homeDirRef.current;
      const colonMatch = title.match(/:\s*([/~][^\s:]*)/);
      if (colonMatch && colonMatch[1]) {
        let p = colonMatch[1].trim();
        if (p.startsWith("~") && home) {
          p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
        }
        if (p.startsWith("/")) {
          currentCwdRef.current = p;
        }
        return;
      }
      const bracketMatch = title.match(/\[[^@]+@[^ \]]+\s+([/~][^\]]+)\]/);
      if (bracketMatch && bracketMatch[1]) {
        let p = bracketMatch[1].trim();
        if (p.startsWith("~") && home) {
          p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
        }
        if (p.startsWith("/")) {
          currentCwdRef.current = p;
        }
      }
    });

    let disposed = false;
    let sawOutput = false;

    const finish = (banner: string) => {
      if (endedRef.current) return;
      endedRef.current = true;
      setEnded(banner);
      term.write(`\r\n\x1b[90m${banner}\x1b[0m\r\n`);
      term.write(
        "\x1b[90mpress \x1b[0m\x1b[1;38;2;190;242;100mR\x1b[0m\x1b[90m to reconnect" +
          "   \x1b[0m\x1b[1;38;2;190;242;100mX\x1b[0m\x1b[90m to close this tab\x1b[0m\r\n",
      );
      term.focus();
      cb.current.onExit();

      // For remote sessions (SSH/RDP), schedule auto-reconnection with exponential backoff (1s, 2s, 4s, 8s, up to 16s)
      if (spec.kind !== "local" && !disposed) {
        const nextAttempt = reconnectAttempts + 1;
        setReconnectAttempts(nextAttempt);
        if (nextAttempt <= 5) {
          const delaySecs = Math.min(16, Math.pow(2, nextAttempt - 1));
          let remaining = delaySecs;
          setAutoReconnectCountdown(remaining);

          countdownIntervalRef.current = window.setInterval(() => {
            remaining -= 1;
            if (remaining > 0) {
              setAutoReconnectCountdown(remaining);
            } else {
              if (countdownIntervalRef.current !== null) {
                clearInterval(countdownIntervalRef.current);
                countdownIntervalRef.current = null;
              }
            }
          }, 1000);

          reconnectTimerRef.current = window.setTimeout(() => {
            setAutoReconnectCountdown(null);
            cb.current.onReconnect();
          }, delaySecs * 1000);
        }
      }
    };

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
            setReconnectAttempts(0); // Reset attempts on successful traffic
            cancelAutoReconnect();
            logFrontend("info", `pane first output (${spec.kind})`);
          }
          const bytes = decodeB64(ev.data);
          term.write(bytes);

          // Check terminal output against active triggers
          try {
            const rawStr = new TextDecoder().decode(bytes);
            const plain = rawStr.replace(/\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])/g, "");
            const activeTriggers = loadTriggers();
            const now = Date.now();
            for (const trig of activeTriggers) {
              if (!trig.enabled) continue;
              const lastFired = lastTriggerFiredRef.current.get(trig.id) ?? 0;
              if (now - lastFired < 3000) continue;

              let matched = false;
              if (trig.isRegex) {
                try {
                  const re = new RegExp(trig.pattern, "i");
                  matched = re.test(plain);
                } catch {
                  // Ignore regex syntax errors
                }
              } else {
                matched = plain.toLowerCase().includes(trig.pattern.toLowerCase());
              }

              if (matched) {
                lastTriggerFiredRef.current.set(trig.id, now);
                if (trig.action === "sound" || trig.action === "both") {
                  playChime(trig.id.includes("error") || trig.pattern.toLowerCase().includes("error"));
                }
                if (trig.action === "notify" || trig.action === "both") {
                  const snippet = plain.replace(/[\r\n\t]+/g, " ").trim().slice(0, 100);
                  sendDesktopNotification(
                    `[Terminator] ${trig.name}`,
                    snippet || `Matched output pattern: ${trig.pattern}`,
                  );
                }
              }
            }
          } catch {
            // Non-blocking trigger check
          }
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
        if (disposed) {
          void closeSession(id);
          return;
        }
        if (spec.kind === "ssh") {
          remoteHome(id)
            .then((h) => {
              homeDirRef.current = h;
              if (!currentCwdRef.current) {
                currentCwdRef.current = h;
              }
            })
            .catch(() => {});
        }
        onReady(id);
        term.focus();
      })
      .catch((err) => {
        term.write(`\r\n\x1b[31m${String(err)}\x1b[0m\r
`);
        finish("[connection failed]");
      });

    const onData = term.onData((data) => {
      if (endedRef.current) {
        const k = data.toLowerCase();
        if (k === "r") {
          cancelAutoReconnect();
          cb.current.onReconnect();
        } else if (k === "x") {
          cancelAutoReconnect();
          cb.current.onClose();
        }
        return;
      }
      if (idRef.current) {
        if (cb.current.onInputData) {
          cb.current.onInputData(data);
        } else {
          void writeSession(idRef.current, data);
        }
      }
    });

    let copyTimer: number | undefined;
    let lastCopied = "";

    const copySelection = () => {
      const sel = term.getSelection();
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
    document.addEventListener("mouseup", flushCopy);

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
      e.preventDefault();
      void pasteFromClipboard();
    };

    const onContextMenu = (e: MouseEvent) => {
      if (!isMac) e.preventDefault();
    };

    const onAuxClick = (e: MouseEvent) => {
      if (e.button === 1) e.preventDefault();
    };

    const host = hostRef.current;
    host.addEventListener("mousedown", onMouseDown);
    host.addEventListener("contextmenu", onContextMenu);
    host.addEventListener("auxclick", onAuxClick);

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
      const last = sentSize.current;
      if (last && last.cols === cols && last.rows === rows) return;
      sentSize.current = { cols, rows };
      void resizeSession(idRef.current, cols, rows);
    };
    refitRef.current = refit;

    let refitTimer: number | undefined;
    const refitSoon = () => {
      window.clearTimeout(refitTimer);
      refitTimer = window.setTimeout(refit, 80);
    };

    const ro = new ResizeObserver(refitSoon);
    ro.observe(hostRef.current);

    teardownRef.current = () => {
      disposed = true;
      cancelAutoReconnect();
      logFrontend("info", `pane teardown (session=${idRef.current ?? "none"})`);
      ro.disconnect();
      window.clearTimeout(refitTimer);
      onData.dispose();
      onTitle.dispose();
      onSelection.dispose();
      window.clearTimeout(copyTimer);
      document.removeEventListener("mouseup", flushCopy);
      window.removeEventListener("terminator:appearance_changed", onAppearanceChange);
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

  // Direct Drag & Drop file drop listener for TerminalPane
  useEffect(() => {
    if (!active || spec.kind !== "ssh") return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const overTerminal = (pos: { x: number; y: number }) => {
      const el = hostRef.current;
      if (!el) return false;
      const r = el.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const x = pos.x / dpr;
      const y = pos.y / dpr;
      return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    };

    const resolveTargetDirectory = async (sessionId: string): Promise<string> => {
      let home = homeDirRef.current;
      if (!home) {
        try {
          home = await remoteHome(sessionId);
          homeDirRef.current = home;
        } catch {
          home = ".";
        }
      }

      // Check live terminal buffer prompt for current working directory
      if (termRef.current) {
        try {
          const buf = termRef.current.buffer.active;
          const endY = buf.cursorY + buf.baseY;
          const startY = Math.max(0, endY - 6);
          for (let y = endY; y >= startY; y--) {
            const line = buf.getLine(y)?.translateToString(true) || "";
            if (!line.trim()) continue;

            // Pattern 1: user@host:path[$#%>] or user@host: path[$#%>]
            const m1 = line.match(/[a-zA-Z0-9._-]+@[a-zA-Z0-9._-]+:\s*([/~][^\s$#%>]*)\s*[$#%>]/);
            if (m1 && m1[1]) {
              let p = m1[1].trim();
              if (p.startsWith("~")) {
                p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
              }
              if (p.startsWith("/")) {
                currentCwdRef.current = p;
                return p;
              }
            }

            // Pattern 2: [user@host path][$#%>]
            const m2 = line.match(/\[[a-zA-Z0-9._-]+@[a-zA-Z0-9._-]+\s+([/~][^\s\]$#%>]+)\]\s*[$#%>]/);
            if (m2 && m2[1]) {
              let p = m2[1].trim();
              if (p.startsWith("~")) {
                p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
              }
              if (p.startsWith("/")) {
                currentCwdRef.current = p;
                return p;
              }
            }

            // Pattern 3: general prompt with : path[$#%>]
            const m3 = line.match(/:\s*([/~][^\s$#%>]*)\s*[$#%>]/);
            if (m3 && m3[1]) {
              let p = m3[1].trim();
              if (p.startsWith("~")) {
                p = p === "~" ? home : posixJoin(home, p.slice(1).replace(/^\//, ""));
              }
              if (p.startsWith("/")) {
                currentCwdRef.current = p;
                return p;
              }
            }
          }
        } catch {
          // Ignore buffer scraping errors
        }
      }

      if (currentCwdRef.current) {
        return currentCwdRef.current;
      }

      return home || ".";
    };

    const handleUploadFiles = async (paths: string[]) => {
      const sessionId = idRef.current;
      if (!sessionId || paths.length === 0) return;

      let targetDir = ".";
      try {
        targetDir = await resolveTargetDirectory(sessionId);
      } catch {
        targetDir = homeDirRef.current || ".";
      }

      for (const localPath of paths) {
        const name = baseName(localPath);
        const remoteTarget = posixJoin(targetDir, name);

        setTransferStatus({
          fileName: name,
          transferred: 0,
          total: 100,
        });

        termRef.current?.write(
          `\r\n\x1b[38;2;190;242;100m[SFTP]\x1b[0m Uploading ${name} -> ${remoteTarget} ...\r\n`,
        );

        try {
          await uploadFile(sessionId, localPath, remoteTarget, (ev: TransferEvent) => {
            if (ev.type === "progress") {
              setTransferStatus({
                fileName: name,
                transferred: ev.transferred,
                total: ev.total,
              });
            } else if (ev.type === "done") {
              setTransferStatus({
                fileName: name,
                transferred: ev.bytes,
                total: ev.bytes,
              });
              setTimeout(() => setTransferStatus(null), 2000);
            } else {
              setTransferStatus({
                fileName: name,
                transferred: 0,
                total: 0,
                failed: ev.message,
              });
            }
          });

          termRef.current?.write(
            `\x1b[38;2;190;242;100m[SFTP]\x1b[0m Successfully uploaded ${name} to ${remoteTarget}\r\n`,
          );
        } catch (err) {
          termRef.current?.write(
            `\x1b[31m[SFTP] Failed to upload ${name}: ${String(err)}\x1b[0m\r\n`,
          );
          setTransferStatus({
            fileName: name,
            transferred: 0,
            total: 0,
            failed: String(err),
          });
        }
      }
    };

    void getCurrentWebview()
      .onDragDropEvent((ev) => {
        const p = ev.payload;
        if (p.type === "over") {
          setDropHover(overTerminal(p.position));
        } else if (p.type === "drop") {
          setDropHover(false);
          if (overTerminal(p.position)) {
            void handleUploadFiles(p.paths);
          }
        } else {
          setDropHover(false);
        }
      })
      .then((u) => {
        if (cancelled) u();
        else unlisten = u;
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [active, spec]);

  useEffect(() => {
    if (!active) return;
    const t = setTimeout(() => {
      refitRef.current?.();
      termRef.current?.focus();
    }, 0);
    return () => clearTimeout(t);
  }, [active]);

  return (
    <div style={{ position: "relative", width: "100%", height: "100%", overflow: "hidden" }}>
      <div ref={hostRef} className="term-host" />

      {/* Terminal Dropzone Overlay */}
      {dropHover && (
        <div
          style={{
            position: "absolute",
            inset: 0,
            background: "rgba(11, 15, 25, 0.85)",
            border: "2px dashed var(--lime)",
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 12,
            zIndex: 30,
            pointerEvents: "none",
          }}
        >
          <span style={{ fontSize: 32 }}>📁</span>
          <div style={{ fontSize: 16, fontWeight: 600, color: "var(--lime)" }}>
            Drop files to upload via SFTP
          </div>
          <div style={{ fontSize: 12, color: "var(--muted)" }}>
            Files will be transferred to active remote directory ({currentCwdRef.current || "~"})
          </div>
        </div>
      )}

      {/* Upload Progress Bar Toast */}
      {transferStatus && (
        <div
          style={{
            position: "absolute",
            bottom: 24,
            right: 24,
            background: "var(--ink-800)",
            border: "1px solid var(--ink-600)",
            borderRadius: "var(--radius)",
            padding: "10px 14px",
            boxShadow: "0 8px 24px rgba(0,0,0,0.6)",
            zIndex: 40,
            display: "flex",
            flexDirection: "column",
            gap: 6,
            minWidth: 260,
          }}
        >
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12 }}>
            <span style={{ fontWeight: 600, color: "var(--fg)" }}>
              {transferStatus.failed ? "❌ Upload failed" : `↑ ${transferStatus.fileName}`}
            </span>
            <span style={{ color: "var(--dim)" }}>
              {transferStatus.total > 0
                ? `${Math.round((transferStatus.transferred / transferStatus.total) * 100)}%`
                : ""}
            </span>
          </div>
          {transferStatus.failed ? (
            <div style={{ fontSize: 11, color: "#fca5a5" }}>{transferStatus.failed}</div>
          ) : (
            <div
              style={{
                width: "100%",
                height: 4,
                background: "var(--ink-700)",
                borderRadius: 2,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  height: "100%",
                  background: "var(--lime)",
                  width: `${Math.min(100, Math.max(0, (transferStatus.transferred / (transferStatus.total || 1)) * 100))}%`,
                  transition: "width 0.1s linear",
                }}
              />
            </div>
          )}
        </div>
      )}

      {ended && (
        <div className="ended-bar" role="status">
          <span className="ended-msg">
            {ended}
            {autoReconnectCountdown !== null && (
              <span style={{ marginLeft: 8, color: "var(--lime)", fontWeight: 500 }}>
                (Auto-reconnecting in {autoReconnectCountdown}s... [attempt {reconnectAttempts}/5])
              </span>
            )}
          </span>
          {autoReconnectCountdown !== null ? (
            <>
              <button className="ended-btn primary" onClick={triggerReconnectNow}>
                Reconnect Now
              </button>
              <button className="ended-btn" onClick={cancelAutoReconnect}>
                Cancel Auto
              </button>
            </>
          ) : (
            <button className="ended-btn primary" onClick={() => { cancelAutoReconnect(); cb.current.onReconnect(); }}>
              <kbd>R</kbd> Reconnect
            </button>
          )}
          <button className="ended-btn" onClick={() => { cancelAutoReconnect(); cb.current.onClose(); }}>
            <kbd>X</kbd> Close
          </button>
        </div>
      )}
    </div>
  );
}
