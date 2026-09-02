import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { ConfirmDialog } from "./ConfirmDialog";
import { writeClipboard } from "../lib/clipboard";
import {
  downloadFile,
  listRemoteDir,
  logFrontend,
  posixJoin,
  remoteHome,
  remoteMkdir,
  remoteRemove,
  stageBytes,
  stagePath,
  startNativeDrag,
  uploadFile,
  type FileEntry,
  type Listing,
  type TransferEvent,
} from "../lib/api";
import { getFileIcon } from "../lib/fileIcons";

/**
 * A remote file has to exist on this machine before the OS will drag it, so a
 * drag is really a download plus a drag. Past this size that download is slow
 * enough that pretending otherwise would be a lie, and the Save dialog is the
 * honest route.
 */
const DRAG_MAX_BYTES = 8 * 1024 * 1024;

/**
 * The drag image. `start_drag` requires one, and the OS shows it under the
 * cursor for the whole gesture.
 */
const DRAG_ICON =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAYAAACM/rhtAAAAWElEQVR42u3YMRGAQBRDwROGHEQgByUYojsFiCCZ+cW+mfRbZy1peM8+3+YAAQEBAQEBAQEBAQEBAQF/dN3HfGATGQO2kFFgAxkHppEVYBJZA6aQDk5N7wP+Veh5ptJs8gAAAABJRU5ErkJggg==";

/** Human-readable size. Terminal users expect KB/MB, not raw bytes. */
function formatSize(entry: FileEntry): string {
  if (entry.kind === "dir") return "--";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let n = entry.size;
  let u = 0;
  while (n >= 1024 && u < units.length - 1) {
    n /= 1024;
    u++;
  }
  return u === 0 ? `${n} B` : `${n.toFixed(n < 10 ? 1 : 0)} ${units[u]}`;
}

function formatDate(secs: number | null): string {
  if (!secs) return "";
  const d = new Date(secs * 1000);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Last path segment, for paths that may come from either OS convention. */
function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

type Props = {
  open: boolean;
  /** Session whose remote filesystem is shown. Null when the tab is local. */
  sessionId: string | null;
  /** True when the active tab can actually do SFTP. */
  remoteCapable: boolean;
  /** user@host of the active tab, shown so the drawer's target is never in doubt. */
  hostLabel: string | null;
  onOpenFile?: (entry: FileEntry) => void;
  onClose: () => void;
};

type Transfer = {
  label: string;
  transferred: number;
  total: number;
  failed?: string;
} | null;

/**
 * Browser for the *remote* filesystem of the active tab.
 *
 * There is deliberately no pane for this computer: the OS already has a
 * perfectly good file manager, and duplicating it inside a terminal only adds
 * a second thing to keep in sync. Files move across the boundary through the
 * mechanisms people already use -- drag and drop, copy and paste, and the
 * native file dialogs -- rather than through a bespoke two-pane UI.
 */
export default function FileDrawer({
  open,
  sessionId,
  remoteCapable,
  hostLabel,
  onOpenFile,
  onClose,
}: Props) {
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [sel, setSel] = useState<string | null>(null);
  // On by default: dotfiles are most of what anyone opens a terminal's file
  // browser to reach. Remembered so the choice survives a restart.
  const [showHidden, setShowHidden] = useState(
    () => localStorage.getItem("files.showHidden") !== "0",
  );
  const [transfer, setTransfer] = useState<Transfer>(null);
  const [hint, setHint] = useState("");
  const [dropActive, setDropActive] = useState(false);
  // In-app dialogs rather than window.prompt/confirm: those are unreliable
  // inside a WKWebView and would look nothing like the rest of the app.
  const [newFolder, setNewFolder] = useState<string | null>(null);
  const [confirmDel, setConfirmDel] = useState<FileEntry | null>(null);

  const asideRef = useRef<HTMLElement | null>(null);
  // Guards against a slow listing for a directory the user already left
  // overwriting the one they are now looking at.
  const req = useRef(0);
  // Remote path -> local copy already fetched for dragging. A drag cannot wait
  // for a download, so the first attempt fetches and the second one flies.
  const stagedRef = useRef(new Map<string, string>());
  // Read inside event callbacks that must not be re-subscribed on every render.
  const listingRef = useRef<Listing | null>(null);
  listingRef.current = listing;
  const busyTransferRef = useRef(false);
  busyTransferRef.current = !!transfer;

  const note = useCallback((text: string) => {
    setHint(text);
    window.setTimeout(() => setHint((h) => (h === text ? "" : h)), 3000);
  }, []);

  const load = useCallback(async (id: string, path: string) => {
    const n = ++req.current;
    setBusy(true);
    try {
      const next = await listRemoteDir(id, path);
      if (n === req.current) {
        setListing(next);
        setError("");
      }
    } catch (err) {
      if (n === req.current) setError(String(err));
    } finally {
      if (n === req.current) setBusy(false);
    }
  }, []);

  const reload = useCallback(async () => {
    const path = listingRef.current?.path;
    if (sessionId && path) await load(sessionId, path);
  }, [sessionId, load]);

  // Follows the active session. Resetting on change matters: the previous
  // host's paths are meaningless on a different machine.
  useEffect(() => {
    if (!open) return;
    setListing(null);
    setSel(null);
    setError("");
    stagedRef.current.clear();
    if (!sessionId || !remoteCapable) return;
    void (async () => {
      try {
        await load(sessionId, await remoteHome(sessionId));
      } catch (err) {
        setError(String(err));
      }
    })();
  }, [open, sessionId, remoteCapable, load]);

  const onTransferEvent = (label: string) => (ev: TransferEvent) => {
    if (ev.type === "progress") {
      setTransfer({ label, transferred: ev.transferred, total: ev.total });
    } else if (ev.type === "done") {
      setTransfer({ label, transferred: ev.bytes, total: ev.bytes });
      // Leave the completed bar up briefly so a fast transfer is still visible.
      setTimeout(() => setTransfer(null), 1200);
    } else {
      setTransfer({ label, transferred: 0, total: 0, failed: ev.message });
    }
  };

  /** Upload local paths into the directory currently on screen. */
  const uploadPaths = useCallback(
    async (paths: string[]) => {
      const dir = listingRef.current?.path;
      if (!sessionId || !dir) return;
      for (const local of paths) {
        const name = baseName(local);
        try {
          await uploadFile(
            sessionId,
            local,
            posixJoin(dir, name),
            onTransferEvent(`↑ ${name}`),
          );
        } catch (err) {
          // Directories are the common case here: SFTP upload is per-file, and
          // the channel does not fire for a failure to even open the source.
          logFrontend("warn", `upload failed: ${String(err)}`);
          setTransfer({
            label: `↑ ${name}`,
            transferred: 0,
            total: 0,
            failed: String(err),
          });
          break;
        }
      }
      await reload();
    },
    [sessionId, reload],
  );

  // --- OS -> remote: files dropped onto the drawer -------------------------
  //
  // Tauri intercepts file drops at the window level, so the webview never sees
  // an HTML5 drop event for them. That also means the event arrives wherever
  // it lands in the window, hence the bounds check: a file dropped on the
  // terminal must not silently upload.
  useEffect(() => {
    if (!open || !sessionId || !remoteCapable) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const overDrawer = (pos: { x: number; y: number }) => {
      const el = asideRef.current;
      if (!el) return false;
      const r = el.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const x = pos.x / dpr;
      const y = pos.y / dpr;
      return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    };

    void getCurrentWebview()
      .onDragDropEvent((ev) => {
        const p = ev.payload;
        if (p.type === "over") {
          setDropActive(overDrawer(p.position));
        } else if (p.type === "drop") {
          setDropActive(false);
          if (overDrawer(p.position)) void uploadPaths(p.paths);
        } else {
          setDropActive(false);
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
  }, [open, sessionId, remoteCapable, uploadPaths]);

  // --- remote -> OS: dragging a row out ------------------------------------
  const onDragStart = (ev: React.DragEvent, entry: FileEntry) => {
    // The HTML5 drag can only carry text, so it is always cancelled in favour
    // of a real OS drag started from Rust.
    ev.preventDefault();
    if (!sessionId || entry.kind !== "file") return;

    const ready = stagedRef.current.get(entry.path);
    if (ready) {
      void startNativeDrag([ready], DRAG_ICON).catch((err) =>
        logFrontend("warn", `drag failed: ${String(err)}`),
      );
      return;
    }

    if (entry.size > DRAG_MAX_BYTES) {
      note(`${entry.name} is too large to drag -- use Save to…`);
      return;
    }
    if (busyTransferRef.current) return;

    void (async () => {
      try {
        const local = await stagePath(entry.name);
        await downloadFile(
          sessionId,
          entry.path,
          local,
          onTransferEvent(`↓ ${entry.name}`),
        );
        stagedRef.current.set(entry.path, local);
        // The gesture that asked for this is long over: a drag cannot be
        // started for a mouse button the user has already released.
        note(`${entry.name} is ready -- drag it again`);
      } catch (err) {
        logFrontend("warn", `stage for drag failed: ${String(err)}`);
      }
    })();
  };

  // --- clipboard -----------------------------------------------------------
  //
  // Pasting a file copied in Finder/Explorer gives bytes but no path, so the
  // bytes are staged to a real file and uploaded through the normal route.
  const onPaste = (ev: React.ClipboardEvent) => {
    const files = Array.from(ev.clipboardData?.files ?? []);
    if (files.length === 0 || !sessionId || !listing) return;
    ev.preventDefault();
    void (async () => {
      const staged: string[] = [];
      for (const file of files) {
        try {
          const buf = new Uint8Array(await file.arrayBuffer());
          staged.push(await stageBytes(file.name, buf));
        } catch (err) {
          logFrontend("warn", `paste stage failed: ${String(err)}`);
        }
      }
      if (staged.length) await uploadPaths(staged);
    })();
  };

  const selected = listing?.entries.find((e) => e.path === sel) ?? null;

  const copyPath = async () => {
    if (!selected) return;
    note((await writeClipboard(selected.path)) ? "Path copied" : "Copy failed");
  };

  const onKeyDown = (ev: React.KeyboardEvent) => {
    if ((ev.metaKey || ev.ctrlKey) && ev.key === "c" && selected) {
      ev.preventDefault();
      void copyPath();
    }
  };

  // --- explicit transfers via the native dialogs ---------------------------
  const doDownload = async () => {
    if (!sessionId || !selected || selected.kind !== "file") return;
    const target = await saveDialog({ defaultPath: selected.name });
    if (!target) return;
    try {
      await downloadFile(
        sessionId,
        selected.path,
        target,
        onTransferEvent(`↓ ${selected.name}`),
      );
    } catch (err) {
      logFrontend("warn", `download failed: ${String(err)}`);
    }
  };

  const doUpload = async () => {
    if (!sessionId || !listing) return;
    const picked = await openDialog({ multiple: true, directory: false });
    if (!picked) return;
    await uploadPaths(Array.isArray(picked) ? picked : [picked]);
  };

  const doMkdir = async (name: string) => {
    if (!sessionId || !listing) return;
    try {
      await remoteMkdir(sessionId, posixJoin(listing.path, name));
      await reload();
    } catch (err) {
      setError(String(err));
    }
  };

  const doDelete = async (entry: FileEntry) => {
    if (!sessionId) return;
    try {
      await remoteRemove(sessionId, entry.path, entry.kind === "dir");
      setSel(null);
      await reload();
    } catch (err) {
      setError(String(err));
    }
  };

  const entries = (listing?.entries ?? []).filter(
    (e) => showHidden || !e.hidden,
  );
  const pct =
    transfer && transfer.total > 0
      ? Math.min(100, Math.round((transfer.transferred / transfer.total) * 100))
      : 0;
  const connected = remoteCapable && !!sessionId;

  return (
    <aside
      ref={asideRef}
      className={`filedrawer ${open ? "" : "closed"} ${dropActive ? "dropping" : ""}`}
      // Keeps the collapsed panel out of the tab order and off screen readers.
      inert={!open}
      aria-label="Remote files"
      onPaste={onPaste}
      onKeyDown={onKeyDown}
      tabIndex={-1}
    >
      <header className="fdrawer-head">
        <div className="fdrawer-heading">
          <span className="fdrawer-title">Remote files</span>
          {hostLabel && <span className="fdrawer-host">{hostLabel}</span>}
        </div>
        <div className="fpane-tools">
          <button
            className={`ficon ${showHidden ? "on" : ""}`}
            onClick={() =>
              setShowHidden((v) => {
                localStorage.setItem("files.showHidden", v ? "0" : "1");
                return !v;
              })
            }
            title={showHidden ? "Hide hidden files" : "Show hidden files"}
            aria-pressed={showHidden}
          >
            ⋯
          </button>
          <button className="ficon" onClick={onClose} title="Close (⌘J)">
            ✕
          </button>
        </div>
      </header>

      {transfer && (
        <div className={`ftransfer ${transfer.failed ? "error" : ""}`}>
          <div className="ftransfer-label">
            {transfer.failed ? `Failed: ${transfer.failed}` : transfer.label}
          </div>
          {!transfer.failed && (
            <div className="fbar">
              <div className="fbar-fill" style={{ width: `${pct}%` }} />
            </div>
          )}
        </div>
      )}

      {hint && <div className="fnote hint">{hint}</div>}

      {connected ? (
        <section className="fpane">
          <header className="fpane-head">
            <div className="fpane-tools">
              <button
                className="fbtn"
                onClick={() => void doUpload()}
                disabled={!listing || !!transfer}
                title="Upload files from this computer"
              >
                Upload…
              </button>
              <button
                className="fbtn"
                onClick={() => void doDownload()}
                disabled={selected?.kind !== "file" || !!transfer}
                title={
                  selected?.kind === "dir"
                    ? "Folder download is not supported yet"
                    : "Save the selected file to this computer"
                }
              >
                Save to…
              </button>
              <button
                className="fbtn"
                onClick={() => selected && selected.kind === "file" && onOpenFile?.(selected)}
                disabled={selected?.kind !== "file"}
                title="Open file in Mini-IDE Remote Editor"
              >
                📝 Edit
              </button>
            </div>
            <div className="fpane-tools">
              <button
                className="ficon"
                onClick={() =>
                  listing?.parent && void load(sessionId, listing.parent)
                }
                disabled={!listing?.parent}
                title="Up one level"
              >
                ↑
              </button>
              <button
                className="ficon"
                onClick={() => void reload()}
                title="Refresh"
              >
                ⟳
              </button>
              <button
                className="ficon"
                onClick={() => setNewFolder("")}
                title="New folder"
              >
                +
              </button>
              <button
                className="ficon danger"
                onClick={() => selected && setConfirmDel(selected)}
                disabled={!selected}
                title="Delete on remote"
              >
                🗑
              </button>
            </div>
          </header>

          <div className="fpath" title={listing?.path ?? ""}>
            {listing?.path ?? ""}
          </div>

          {error && <div className="fnote error">{error}</div>}
          {busy && !listing && <div className="fnote">Loading…</div>}

          <ul className="flist">
            {listing?.parent && (
              // A real row rather than only the toolbar arrow: going up is the
              // most frequent action in a file list, and every file manager
              // puts it here. Single click, since it is navigation and there is
              // nothing to select.
              <li
                className="frow updir"
                onClick={() => void load(sessionId, listing.parent!)}
                title={listing.parent}
              >
                <span className="fkind">▴</span>
                <span className="fname">..</span>
                <span className="fsize" />
                <span className="fdate" />
              </li>
            )}
            {entries.map((entry) => (
              <li
                key={entry.path}
                className={`frow ${sel === entry.path ? "sel" : ""} ${
                  entry.hidden ? "hidden-entry" : ""
                }`}
                draggable={entry.kind === "file"}
                onDragStart={(ev) => onDragStart(ev, entry)}
                onClick={() => setSel(entry.path)}
                onDoubleClick={() => {
                  if (entry.kind === "dir") {
                    void load(sessionId, entry.path);
                  } else if (entry.kind === "file") {
                    onOpenFile?.(entry);
                  }
                }}
                title={entry.path}
              >
                <span className="fkind">
                  {entry.kind === "dir" ? "📁" : entry.symlink ? "↗" : getFileIcon(entry.name)}
                </span>
                <span className="fname">{entry.name}</span>
                <span className="fsize">{formatSize(entry)}</span>
                <span className="fdate">{formatDate(entry.modified)}</span>
              </li>
            ))}
            {listing && entries.length === 0 && (
              <li className="fnote">Empty directory</li>
            )}
          </ul>

          <footer className="fdrawer-foot">
            Drop files here to upload · drag a file out to copy it here
          </footer>
        </section>
      ) : (
        <section className="fpane">
          <div className="fnote">
            Connect an SSH session to browse and transfer files.
          </div>
        </section>
      )}

      {newFolder !== null && (
        <form
          className="fnewfolder"
          onSubmit={(ev) => {
            ev.preventDefault();
            const name = newFolder.trim();
            setNewFolder(null);
            if (name) void doMkdir(name);
          }}
        >
          <input
            autoFocus
            className="fnewfolder-input"
            value={newFolder}
            placeholder="New remote folder name"
            spellCheck={false}
            onChange={(ev) => setNewFolder(ev.target.value)}
            onKeyDown={(ev) => {
              if (ev.key === "Escape") {
                ev.stopPropagation();
                setNewFolder(null);
              }
            }}
          />
          <button className="fbtn" type="submit" disabled={!newFolder.trim()}>
            Create
          </button>
          <button
            className="ficon"
            type="button"
            onClick={() => setNewFolder(null)}
          >
            ✕
          </button>
        </form>
      )}

      {confirmDel && (
        <ConfirmDialog
          title="Delete on remote host"
          message={
            <>
              Delete <strong>{confirmDel.name}</strong> on the remote host?
            </>
          }
          detail={
            confirmDel.kind === "dir"
              ? "Only empty directories can be removed."
              : confirmDel.path
          }
          onCancel={() => setConfirmDel(null)}
          onConfirm={() => {
            const entry = confirmDel;
            setConfirmDel(null);
            void doDelete(entry);
          }}
        />
      )}
    </aside>
  );
}
