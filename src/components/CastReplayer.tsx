import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

interface CastHeader {
  version: number;
  width: number;
  height: number;
  timestamp?: number;
  title?: string;
  duration?: number;
}

interface CastEvent {
  time: number;
  type: string;
  data: string;
}

interface CastData {
  header: CastHeader;
  events: CastEvent[];
  duration: number;
}

function parseCast(rawText: string): CastData {
  const lines = rawText.trim().split("\n");
  if (lines.length === 0) {
    throw new Error("Empty cast file");
  }

  let header: CastHeader = { version: 2, width: 80, height: 24 };
  try {
    header = JSON.parse(lines[0]);
  } catch {
    // fallback default header
  }

  const events: CastEvent[] = [];
  for (let i = 1; i < lines.length; i++) {
    const line = lines[i].trim();
    if (!line) continue;
    try {
      const parsed = JSON.parse(line);
      if (Array.isArray(parsed) && parsed.length >= 3) {
        events.push({
          time: Number(parsed[0]),
          type: String(parsed[1]),
          data: String(parsed[2]),
        });
      }
    } catch {
      // ignore malformed line
    }
  }

  const duration = events.length > 0 ? events[events.length - 1].time : 0;
  return { header, events, duration };
}

export function CastReplayer({
  castContent,
  onClose,
}: {
  castContent: string;
  onClose: () => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);

  const [parsed, setParsed] = useState<CastData | null>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [speed, setSpeed] = useState<number>(1);
  const [error, setError] = useState<string | null>(null);

  const currentEventIdxRef = useRef(0);
  const animFrameRef = useRef<number | null>(null);
  const lastTimestampRef = useRef<number | null>(null);
  const isPlayingRef = useRef(false);
  const speedRef = useRef(1);
  const currentTimeRef = useRef(0);

  isPlayingRef.current = isPlaying;
  speedRef.current = speed;
  currentTimeRef.current = currentTime;

  // Parse cast text
  useEffect(() => {
    try {
      const data = parseCast(castContent);
      setParsed(data);
      setError(null);
    } catch (err) {
      setError(String(err));
    }
  }, [castContent]);

  // Setup terminal instance
  useEffect(() => {
    if (!hostRef.current || !parsed) return;

    const term = new Terminal({
      allowTransparency: false,
      fontFamily:
        '"JetBrains Mono", ui-monospace, Menlo, Consolas, "DejaVu Sans Mono", monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: false,
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
    term.open(hostRef.current);
    fit.fit();

    termRef.current = term;
    fitRef.current = fit;

    const onResize = () => fit.fit();
    window.addEventListener("resize", onResize);

    return () => {
      window.removeEventListener("resize", onResize);
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, [parsed]);

  // Fast-forward or replay up to a specific target time
  const renderUpTo = (targetTime: number) => {
    if (!parsed || !termRef.current) return;
    const term = termRef.current;

    // Reset terminal if targetTime is before current pointer
    if (targetTime < currentTimeRef.current || (currentEventIdxRef.current > 0 && targetTime === 0)) {
      term.reset();
      currentEventIdxRef.current = 0;
    }

    const events = parsed.events;
    let idx = currentEventIdxRef.current;

    while (idx < events.length && events[idx].time <= targetTime) {
      const ev = events[idx];
      if (ev.type === "o") {
        term.write(ev.data);
      }
      idx++;
    }

    currentEventIdxRef.current = idx;
    setCurrentTime(targetTime);
  };

  // Animation playback loop
  useEffect(() => {
    if (!isPlaying || !parsed) {
      if (animFrameRef.current) {
        cancelAnimationFrame(animFrameRef.current);
        animFrameRef.current = null;
      }
      lastTimestampRef.current = null;
      return;
    }

    const loop = (timestamp: number) => {
      if (!isPlayingRef.current || !parsed) return;

      if (lastTimestampRef.current === null) {
        lastTimestampRef.current = timestamp;
      }

      const deltaSeconds =
        ((timestamp - lastTimestampRef.current) / 1000) * speedRef.current;
      lastTimestampRef.current = timestamp;

      const nextTime = Math.min(
        currentTimeRef.current + deltaSeconds,
        parsed.duration,
      );

      renderUpTo(nextTime);

      if (nextTime >= parsed.duration) {
        setIsPlaying(false);
      } else {
        animFrameRef.current = requestAnimationFrame(loop);
      }
    };

    animFrameRef.current = requestAnimationFrame(loop);

    return () => {
      if (animFrameRef.current) {
        cancelAnimationFrame(animFrameRef.current);
        animFrameRef.current = null;
      }
    };
  }, [isPlaying, parsed]);

  const handlePlayPause = () => {
    if (!parsed) return;
    if (currentTime >= parsed.duration) {
      termRef.current?.reset();
      currentEventIdxRef.current = 0;
      setCurrentTime(0);
    }
    setIsPlaying((prev) => !prev);
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    const time = parseFloat(e.target.value);
    if (termRef.current) {
      termRef.current.reset();
      currentEventIdxRef.current = 0;
      renderUpTo(time);
    }
  };

  const formatDuration = (sec: number) => {
    const m = Math.floor(sec / 60);
    const s = Math.floor(sec % 60);
    return `${m}:${s.toString().padStart(2, "0")}`;
  };

  return (
    <div className="replayer-container">
      <div className="replayer-header">
        <div className="replayer-title">
          <span className="replayer-tag">REPLAY</span>
          <span>{parsed?.header.title || "Session Recording (.cast)"}</span>
        </div>
        <button className="icon-btn" onClick={onClose} title="Close replay">
          ✕
        </button>
      </div>

      {error ? (
        <div className="replayer-error">Failed to parse recording: {error}</div>
      ) : (
        <>
          <div className="replayer-terminal-host" ref={hostRef} />
          <div className="replayer-controls">
            <button
              className="replayer-btn play-btn"
              onClick={handlePlayPause}
              title={isPlaying ? "Pause" : "Play"}
            >
              {isPlaying ? "⏸ Pause" : "▶ Play"}
            </button>

            <span className="replayer-time">
              {formatDuration(currentTime)} /{" "}
              {formatDuration(parsed?.duration ?? 0)}
            </span>

            <input
              type="range"
              className="replayer-scrubber"
              min={0}
              max={parsed?.duration ?? 0}
              step={0.05}
              value={currentTime}
              onChange={handleSeek}
            />

            <div className="replayer-speeds">
              {[1, 2, 4].map((s) => (
                <button
                  key={s}
                  className={`speed-btn ${speed === s ? "active" : ""}`}
                  onClick={() => setSpeed(s)}
                >
                  {s}x
                </button>
              ))}
            </div>
          </div>
        </>
      )}
    </div>
  );
}
