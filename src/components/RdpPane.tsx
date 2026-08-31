import { useEffect, useRef, useState } from "react";
import {
  closeRdp,
  logFrontend,
  openRdp,
  rdpInput,
  rdpResize,
  type RdpEvent,
  type RdpInput,
  type TransportSpec,
} from "../lib/api";
import { scancodeFor } from "../lib/scancodes";

interface Props {
  spec: TransportSpec;
  secretRef?: string;
  password?: string;
  active: boolean;
  onReady: (id: string) => void;
  onExit: () => void;
  onReconnect: () => void;
  onClose: () => void;
}

/** Input batching window. A mouse drag fires far faster than 60 Hz, and one
 *  IPC call per event would swamp the channel for no visual gain. */
const INPUT_FLUSH_MS = 8;
/** How long the desktop must sit at a stable size before we ask the server to
 *  match it. Resizing is a full renegotiation, so doing it mid-drag would
 *  blank the screen repeatedly. */
const RESIZE_DEBOUNCE_MS = 400;

export function RdpPane({
  spec,
  secretRef,
  password,
  active,
  onReady,
  onExit,
  onReconnect,
  onClose,
}: Props) {
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const idRef = useRef<string | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const [status, setStatus] = useState<string | null>("connecting ...");
  const [ended, setEnded] = useState<string | null>(null);
  const endedRef = useRef(false);
  const cb = useRef({ onExit, onReconnect, onClose, onReady });
  cb.current = { onExit, onReconnect, onClose, onReady };

  // See TerminalPane: StrictMode's mount -> cleanup -> mount would otherwise
  // tear down a live connection and log in a second time.
  const teardownRef = useRef<(() => void) | null>(null);
  const teardownTimer = useRef<number | null>(null);

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
    if (!canvasRef.current || teardownRef.current) return deferTeardown;

    const canvas = canvasRef.current;
    // `alpha: false` lets the compositor skip per-pixel blending on every
    // blit; the desktop is fully opaque anyway.
    const ctx = canvas.getContext("2d", { alpha: false });
    ctxRef.current = ctx;

    let disposed = false;

    const finish = (banner: string) => {
      if (endedRef.current) return;
      endedRef.current = true;
      setStatus(null);
      setEnded(banner);
      cb.current.onExit();
    };

    /* ---- outgoing input, batched ---- */
    let queue: RdpInput[] = [];
    let flushTimer: number | undefined;

    const flush = () => {
      flushTimer = undefined;
      const id = idRef.current;
      if (!id || queue.length === 0) return;
      const ops = queue;
      queue = [];
      void rdpInput(id, ops).catch(() => {
        /* session gone; the disconnect event will explain why */
      });
    };

    const send = (op: RdpInput) => {
      if (endedRef.current) return;
      // Consecutive mouse moves are pure overdraw -- only the newest position
      // matters, and a drag can produce dozens between flushes.
      if (op.type === "mouseMove" && queue.length > 0) {
        const last = queue[queue.length - 1];
        if (last.type === "mouseMove") queue[queue.length - 1] = op;
        else queue.push(op);
      } else {
        queue.push(op);
      }
      if (flushTimer === undefined) {
        flushTimer = window.setTimeout(flush, INPUT_FLUSH_MS);
      }
    };

    /* ---- incoming frames ---- */
    const onEvent = (ev: RdpEvent) => {
      if (disposed) return;
      if (ev.type === "frame") {
        const bin = atob(ev.rgba);
        const buf = new Uint8ClampedArray(bin.length);
        for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
        // The core packs rows tightly to exactly w*h*4, which is what
        // ImageData requires; a strided buffer would shear the image.
        try {
          const img = new ImageData(buf, ev.w, ev.h);
          ctxRef.current?.putImageData(img, ev.x, ev.y);
        } catch (err) {
          logFrontend("warn", `rdp frame blit failed: ${String(err)}`);
        }
      } else if (ev.type === "resized") {
        setStatus(null);
        // Setting width/height clears the canvas, so only touch it on a real
        // change -- otherwise every no-op resize flashes the desktop black.
        if (canvas.width !== ev.width || canvas.height !== ev.height) {
          canvas.width = ev.width;
          canvas.height = ev.height;
        }
      } else if (ev.type === "disconnected") {
        finish(`[${ev.reason}]`);
      }
    };

    // Ask for the pane's current size, falling back to a sane desktop when it
    // has not been laid out yet.
    const el = wrapRef.current;
    const wantW = Math.max(640, Math.round(el?.clientWidth || 1024));
    const wantH = Math.max(480, Math.round(el?.clientHeight || 768));

    openRdp(spec, wantW, wantH, onEvent, { secretRef, password })
      .then((opened) => {
        if (disposed) {
          void closeRdp(opened.id);
          return;
        }
        idRef.current = opened.id;
        canvas.width = opened.width;
        canvas.height = opened.height;
        setStatus(null);
        cb.current.onReady(opened.id);
        canvas.focus();
      })
      .catch((err) => {
        setStatus(null);
        logFrontend("error", `rdp connect failed: ${String(err)}`);
        finish(`[${String(err)}]`);
      });

    /* ---- mouse ----
     *
     * Coordinates are in *desktop* pixels. The canvas is displayed scaled to
     * fit, so a raw clientX would land in the wrong place on any pane that is
     * not exactly the desktop size.
     */
    const toDesktop = (e: MouseEvent) => {
      const r = canvas.getBoundingClientRect();
      if (r.width === 0 || r.height === 0) return null;
      const x = ((e.clientX - r.left) / r.width) * canvas.width;
      const y = ((e.clientY - r.top) / r.height) * canvas.height;
      return {
        x: Math.max(0, Math.min(canvas.width - 1, Math.round(x))),
        y: Math.max(0, Math.min(canvas.height - 1, Math.round(y))),
      };
    };

    const onMouseMove = (e: MouseEvent) => {
      const p = toDesktop(e);
      if (p) send({ type: "mouseMove", x: p.x, y: p.y });
    };

    const onMouseDown = (e: MouseEvent) => {
      e.preventDefault();
      canvas.focus();
      const p = toDesktop(e);
      // Position first: the server applies the click wherever the pointer
      // currently is, and a click can arrive before any move event.
      if (p) send({ type: "mouseMove", x: p.x, y: p.y });
      send({ type: "mouseDown", button: e.button });
    };

    // Bound to the window so a button released outside the pane still gets
    // reported; otherwise the server thinks it is still held and the next
    // click starts a drag.
    const onMouseUp = (e: MouseEvent) => {
      if (!idRef.current) return;
      send({ type: "mouseUp", button: e.button });
    };

    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      // RDP measures rotation in 120ths of a detent, and the sign is inverted
      // relative to the DOM (which counts "content moved down" as positive).
      const unit = 120;
      if (e.deltaY !== 0) {
        send({
          type: "wheel",
          delta: clampI16(-Math.sign(e.deltaY) * unit),
          horizontal: false,
        });
      }
      if (e.deltaX !== 0) {
        send({
          type: "wheel",
          delta: clampI16(-Math.sign(e.deltaX) * unit),
          horizontal: true,
        });
      }
    };

    const onContextMenu = (e: MouseEvent) => e.preventDefault();

    /* ---- keyboard ---- */
    const onKeyDown = (e: KeyboardEvent) => {
      // Once the session is over there is nowhere to send keystrokes, so the
      // keyboard switches to driving the reconnect/close prompt -- same
      // contract as a terminal pane, so R/X works without reaching for the
      // mouse.
      if (endedRef.current) {
        const k = e.key.toLowerCase();
        if (k === "r" || k === "x") {
          e.preventDefault();
          e.stopPropagation();
          if (k === "r") cb.current.onReconnect();
          else cb.current.onClose();
        }
        return;
      }
      const sc = scancodeFor(e.code);
      if (sc === undefined) return;
      // Everything goes to the remote desktop, including Ctrl+W and the like
      // -- swallowing them here is the whole point of a remote session.
      e.preventDefault();
      e.stopPropagation();
      send({ type: "keyDown", scancode: sc });
    };

    const onKeyUp = (e: KeyboardEvent) => {
      if (endedRef.current) return;
      const sc = scancodeFor(e.code);
      if (sc === undefined) return;
      e.preventDefault();
      e.stopPropagation();
      send({ type: "keyUp", scancode: sc });
    };

    /* A modifier held while focus leaves would stay latched on the server
     * forever, so every later keystroke arrives shifted or control-modified.
     * Releasing everything on blur is what real RDP clients do. */
    const onBlur = () => {
      queue.push({ type: "releaseAll" });
      flush();
    };

    canvas.addEventListener("mousemove", onMouseMove);
    canvas.addEventListener("mousedown", onMouseDown);
    window.addEventListener("mouseup", onMouseUp);
    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("contextmenu", onContextMenu);
    canvas.addEventListener("keydown", onKeyDown);
    canvas.addEventListener("keyup", onKeyUp);
    canvas.addEventListener("blur", onBlur);

    /* ---- follow the pane size ---- */
    let resizeTimer: number | undefined;
    const ro = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        const id = idRef.current;
        const box = wrapRef.current;
        if (!id || !box || endedRef.current) return;
        const w = Math.round(box.clientWidth);
        const h = Math.round(box.clientHeight);
        // A hidden pane measures 0; asking for a 0x0 desktop would be
        // rejected at best and would blank the session at worst.
        if (w < 64 || h < 64) return;
        if (w === canvas.width && h === canvas.height) return;
        void rdpResize(id, w, h).catch(() => {
          /* DisplayControl may not be up; the pane keeps its own size */
        });
      }, RESIZE_DEBOUNCE_MS);
    });
    if (wrapRef.current) ro.observe(wrapRef.current);

    teardownRef.current = () => {
      disposed = true;
      ro.disconnect();
      window.clearTimeout(resizeTimer);
      window.clearTimeout(flushTimer);
      canvas.removeEventListener("mousemove", onMouseMove);
      canvas.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("mouseup", onMouseUp);
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("contextmenu", onContextMenu);
      canvas.removeEventListener("keydown", onKeyDown);
      canvas.removeEventListener("keyup", onKeyUp);
      canvas.removeEventListener("blur", onBlur);
      if (idRef.current) void closeRdp(idRef.current);
      ctxRef.current = null;
    };

    return deferTeardown;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!active) return;
    const t = setTimeout(() => canvasRef.current?.focus(), 0);
    return () => clearTimeout(t);
  }, [active]);

  return (
    <div className="rdp-host" ref={wrapRef}>
      {/* tabIndex makes the canvas focusable, which is what lets it receive
          key events at all. */}
      <canvas className="rdp-canvas" ref={canvasRef} tabIndex={0} />
      {status && <div className="rdp-status">{status}</div>}
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
    </div>
  );
}

const clampI16 = (n: number) => Math.max(-32768, Math.min(32767, Math.round(n)));

export default RdpPane;
