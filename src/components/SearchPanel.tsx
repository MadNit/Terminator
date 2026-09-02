import { useEffect, useMemo, useRef, useState } from "react";
import {
  listSessions,
  type SearchResult,
  type DaemonSession,
} from "../lib/api";

type Run = (
  query: string,
  caseSensitive: boolean,
  maxPerSession: number,
) => Promise<{ results: SearchResult[] }>;

type RunRef = { current: Run | null };

/** Cross-session find panel.
 *
 *  Calls the daemon's `GET /search` route, which walks every
 *  live session's scrollback ring buffer. The daemon's
 *  `OutputRingBuffer::search` joins the chunks and splits on
 *  `\n`; we just render the hits grouped by session and
 *  jump-to-pane on click.
 *
 *  Hits are debounced 150ms so a fast typist doesn't burn
 *  the daemon with N requests for a query that's still
 *  mutating.
 */
export function SearchPanel({
  onClose,
  onJumpToSession,
  runRef,
}: {
  onClose: () => void;
  onJumpToSession: (sessionId: string) => void;
  /** The host App passes a ref to its stable `searchSessions`
   *  wrapper so the debounce can call the same closure as
   *  everything else (avoiding the stale-fn footgun of
   *  useEffect + state). */
  runRef: RunRef;
}) {
  const [query, setQuery] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [maxPerSession, setMaxPerSession] = useState(50);
  const [results, setResults] = useState<SearchResult[]>([]);
  const [liveSessions, setLiveSessions] = useState<DaemonSession[]>([]);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const debounceRef = useRef<number | null>(null);
  const reqIdRef = useRef(0);

  // Focus the input on mount; load the live-sessions list
  // so we can show "ssh: user@host:port" instead of just
  // the UUID.
  useEffect(() => {
    inputRef.current?.focus();
    listSessions().then(setLiveSessions).catch(() => {});
  }, []);

  // Group results by session for display; the daemon's
  // search is already a flat list grouped by session_id.
  const grouped = useMemo(() => {
    const byId = new Map<string, DaemonSession>();
    for (const s of liveSessions) byId.set(s.id, s);
    return results.map((r) => ({
      result: r,
      session: byId.get(r.sessionId),
    }));
  }, [results, liveSessions]);

  // Flatten for keyboard navigation: each row in the list
  // is one (session, hit) pair. ArrowUp/Down moves the
  // highlight, Enter jumps to the highlighted session.
  const flat = useMemo(() => {
    const out: Array<{ sessionId: string; hitIndex: number }> = [];
    for (const g of grouped) {
      for (let i = 0; i < g.result.hits.length; i++) {
        out.push({ sessionId: g.result.sessionId, hitIndex: i });
      }
    }
    return out;
  }, [grouped]);

  // Debounce the search: wait 150ms after the last keystroke
  // before hitting the daemon. Bump `reqIdRef` on each new
  // query so an in-flight request for an older query doesn't
  // overwrite a newer one.
  useEffect(() => {
    if (debounceRef.current !== null) {
      window.clearTimeout(debounceRef.current);
      debounceRef.current = null;
    }
    if (!query.trim()) {
      setResults([]);
      setError(null);
      setRunning(false);
      setHighlight(0);
      return;
    }
    setRunning(true);
    const myReq = ++reqIdRef.current;
    debounceRef.current = window.setTimeout(async () => {
      const run = runRef.current;
      if (!run) {
        setRunning(false);
        return;
      }
      try {
        const r = await run(query, caseSensitive, maxPerSession);
        if (myReq !== reqIdRef.current) return; // newer query in flight
        setResults(r.results);
        setError(null);
        setHighlight(0);
      } catch (err) {
        if (myReq !== reqIdRef.current) return;
        setError(String(err));
        setResults([]);
      } finally {
        if (myReq === reqIdRef.current) setRunning(false);
      }
    }, 150);
    return () => {
      if (debounceRef.current !== null) {
        window.clearTimeout(debounceRef.current);
        debounceRef.current = null;
      }
    };
  }, [query, caseSensitive, maxPerSession, runRef]);

  // Re-run immediately when options change for an existing
  // query (no debounce -- the user just toggled a checkbox,
  // they expect instant feedback).
  useEffect(() => {
    if (!query.trim()) return;
    const myReq = ++reqIdRef.current;
    const run = runRef.current;
    if (!run) return;
    setRunning(true);
    run(query, caseSensitive, maxPerSession)
      .then((r) => {
        if (myReq !== reqIdRef.current) return;
        setResults(r.results);
        setError(null);
        setHighlight(0);
      })
      .catch((err) => {
        if (myReq !== reqIdRef.current) return;
        setError(String(err));
        setResults([]);
      })
      .finally(() => {
        if (myReq === reqIdRef.current) setRunning(false);
      });
  }, [caseSensitive, maxPerSession, query, runRef]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlight((h) => (flat.length === 0 ? 0 : (h + 1) % flat.length));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlight((h) => (flat.length === 0 ? 0 : (h - 1 + flat.length) % flat.length));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const target = flat[highlight];
      if (target) {
        onJumpToSession(target.sessionId);
        onClose();
      }
    }
  };

  return (
    <div className="command-palette-backdrop" onClick={onClose}>
      <div
        className="command-palette-modal search-panel"
        onClick={(e) => e.stopPropagation()}
        style={{ maxWidth: "780px", width: "92vw" }}
      >
        <div className="palette-search-row">
          <span style={{ fontSize: "15px", color: "var(--dim)", display: "flex" }}>🔍</span>
          <input
            ref={inputRef}
            type="text"
            className="palette-search-input"
            placeholder="Find in any open tab..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          {running && (
            <span style={{ fontSize: "11px", color: "var(--muted)" }}>…</span>
          )}
          <kbd className="quick-kbd">ESC</kbd>
        </div>

        <div
          className="search-options"
          style={{
            display: "flex",
            gap: "12px",
            alignItems: "center",
            padding: "6px 12px",
            borderBottom: "1px solid var(--border)",
            fontSize: "11.5px",
            color: "var(--muted)",
          }}
        >
          <label style={{ display: "flex", alignItems: "center", gap: "4px", cursor: "pointer" }}>
            <input
              type="checkbox"
              checked={caseSensitive}
              onChange={(e) => setCaseSensitive(e.target.checked)}
            />
            Aa
          </label>
          <span>max per tab:</span>
          <input
            type="number"
            min={1}
            max={500}
            value={maxPerSession}
            onChange={(e) => {
              const v = parseInt(e.target.value, 10);
              if (!isNaN(v) && v >= 1) setMaxPerSession(Math.min(v, 500));
            }}
            style={{
              width: "60px",
              background: "var(--surface)",
              color: "var(--fg)",
              border: "1px solid var(--border)",
              borderRadius: "3px",
              padding: "1px 4px",
            }}
          />
          <span style={{ marginLeft: "auto" }}>
            {flat.length} match{flat.length === 1 ? "" : "es"} across {grouped.length} tab{grouped.length === 1 ? "" : "s"}
          </span>
        </div>

        <div className="palette-items-list" style={{ maxHeight: "60vh" }}>
          {error && (
            <div style={{ padding: "16px", color: "var(--red)", fontSize: "12px" }}>
              {error}
            </div>
          )}
          {!error && !query.trim() && (
            <div style={{ padding: "28px", textAlign: "center", color: "var(--dim)", fontSize: "12.5px" }}>
              Type to search across every open tab's scrollback
            </div>
          )}
          {!error && query.trim() && flat.length === 0 && !running && (
            <div style={{ padding: "28px", textAlign: "center", color: "var(--dim)", fontSize: "12.5px" }}>
              No matches
            </div>
          )}
          {grouped.map((g) => {
            if (g.result.hits.length === 0) return null;
            const sessionLabel = (() => {
              const s = g.session;
              if (!s) return `Session ${g.result.sessionId.slice(0, 8)}…`;
              const spec = s.spec;
              if (spec.kind === "ssh") {
                return `SSH: ${spec.user}@${spec.host}:${spec.port}`;
              }
              if (spec.kind === "rdp") {
                return `RDP: ${spec.user}@${spec.host}:${spec.port}`;
              }
              if (spec.kind === "local") {
                return spec.shell ? `Local: ${spec.shell}` : "Local Shell";
              }
              // Exhaustiveness: if a new spec kind is added
              // to the union without a branch here, this
              // default makes the IIFE keep compiling until
              // the dev adds the new label.
              return "Session";
            })();
            return (
              <div key={g.result.sessionId} style={{ marginBottom: "4px" }}>
                <div
                  style={{
                    padding: "4px 12px",
                    background: "var(--bg-elevated)",
                    fontSize: "10.5px",
                    color: "var(--muted)",
                    textTransform: "uppercase",
                    letterSpacing: "0.04em",
                    fontWeight: 600,
                  }}
                >
                  {sessionLabel} · {g.result.hits.length} match{g.result.hits.length === 1 ? "" : "es"}
                </div>
                {g.result.hits.map((hit, hitIdx) => {
                  // Find the flat index of this hit for
                  // keyboard navigation highlight tracking.
                  const flatIdx = flat.findIndex(
                    (f) => f.sessionId === g.result.sessionId && f.hitIndex === hitIdx,
                  );
                  const isActive = flatIdx === highlight;
                  return (
                    <div
                      key={`${g.result.sessionId}-${hitIdx}`}
                      className={`palette-item ${isActive ? "active" : ""}`}
                      onMouseEnter={() => setHighlight(flatIdx)}
                      onClick={() => {
                        onJumpToSession(g.result.sessionId);
                        onClose();
                      }}
                      style={{ alignItems: "flex-start" }}
                    >
                      <div style={{ display: "flex", alignItems: "center", gap: "10px", minWidth: 0, width: "100%" }}>
                        <span
                          style={{
                            fontSize: "10.5px",
                            color: "var(--dim)",
                            minWidth: "40px",
                            fontFamily: "var(--mono)",
                          }}
                        >
                          L{hit.lineNumber}
                        </span>
                        <div
                          style={{
                            fontSize: "11.5px",
                            fontFamily: "var(--mono)",
                            color: "var(--fg)",
                            overflow: "hidden",
                            textOverflow: "ellipsis",
                            whiteSpace: "nowrap",
                            flex: 1,
                          }}
                        >
                          {highlightMatch(hit.text, query, caseSensitive)}
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>

        <div className="palette-footer">
          <span>
            <kbd className="quick-kbd" style={{ padding: "0 3px" }}>↑</kbd>{" "}
            <kbd className="quick-kbd" style={{ padding: "0 3px" }}>↓</kbd> to navigate,{" "}
            <kbd className="quick-kbd" style={{ padding: "0 4px" }}>Enter</kbd> to jump
          </span>
          <span style={{ color: "var(--lime)", fontWeight: 500 }}>⚡ Cross-tab Find</span>
        </div>
      </div>
    </div>
  );
}

/** Render a line with the matched substring wrapped in a
 *  highlight span. Falls back to a plain string if the
 *  match isn't found (which can happen when the case-folded
 *  comparison differs from the original). */
function highlightMatch(text: string, query: string, caseSensitive: boolean): React.ReactNode {
  if (!query) return text;
  const haystack = caseSensitive ? text : text.toLowerCase();
  const needle = caseSensitive ? query : query.toLowerCase();
  const idx = haystack.indexOf(needle);
  if (idx < 0) return text;
  return (
    <>
      {text.slice(0, idx)}
      <mark
        style={{
          background: "var(--yellow-soft, rgba(255, 213, 79, 0.3))",
          color: "var(--fg)",
          padding: "0 1px",
          borderRadius: "2px",
        }}
      >
        {text.slice(idx, idx + needle.length)}
      </mark>
      {text.slice(idx + needle.length)}
    </>
  );
}
