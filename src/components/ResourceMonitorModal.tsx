import { useState, useEffect, useCallback } from "react";
import { execCommand, type TransportSpec, type Profile, listProfiles } from "../lib/api";

interface Props {
  open: boolean;
  spec?: TransportSpec;
  hostLabel?: string;
  secretRef?: string;
  password?: string;
  jumpSecretRef?: string;
  jumpPassword?: string;
  onClose: () => void;
}

export interface ProcessItem {
  pid: string;
  user: string;
  cpu: number;
  mem: number;
  command: string;
}

export interface DiskItem {
  filesystem: string;
  size: string;
  used: string;
  avail: string;
  percent: number;
  mount: string;
}

export interface SystemMetrics {
  hostname: string;
  os: string;
  uptime: string;
  cpuPercent: number;
  cores: number;
  memTotalMb: number;
  memUsedMb: number;
  memPercent: number;
  loadAvg: string;
  disks: DiskItem[];
  processes: ProcessItem[];
}

const METRIC_SCRIPT = `
# Multi-platform system metric collector
echo "===SYSINFO==="
uname -srm 2>/dev/null || uname -a
hostname 2>/dev/null || echo "localhost"
uptime 2>/dev/null || echo ""

echo "===CPU==="
if [ -f /proc/stat ]; then
  cat /proc/stat | grep '^cpu '
  sleep 0.15
  cat /proc/stat | grep '^cpu '
elif which top >/dev/null 2>&1; then
  top -l 1 -n 0 2>/dev/null | grep -E "CPU usage|Load Avg"
fi

echo "===MEM==="
if which free >/dev/null 2>&1; then
  free -m 2>/dev/null
elif which vm_stat >/dev/null 2>&1; then
  vm_stat 2>/dev/null
  sysctl -n hw.memsize 2>/dev/null
fi

echo "===DISK==="
df -k 2>/dev/null || df -h 2>/dev/null

echo "===PROC==="
if ps -eo pid,user,%cpu,%mem,comm --sort=-%cpu 2>/dev/null; then
  :
elif ps -eo pid,user,%cpu,%mem,comm -r 2>/dev/null; then
  :
else
  ps aux 2>/dev/null
fi
`;

export function ResourceMonitorModal({
  open,
  spec: initialSpec,
  hostLabel: initialHostLabel,
  secretRef,
  password,
  jumpSecretRef,
  jumpPassword,
  onClose,
}: Props) {
  const [activeSpec, setActiveSpec] = useState<TransportSpec>(
    initialSpec || { kind: "local", shell: null, cwd: null }
  );
  const [activeLabel, setActiveLabel] = useState<string>(
    initialHostLabel || "Local Machine"
  );
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [metrics, setMetrics] = useState<SystemMetrics | null>(null);
  const [loading, setLoading] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [autoRefresh, setAutoRefresh] = useState<boolean>(true);
  const [refreshInterval, setRefreshInterval] = useState<number>(3000);
  const [procFilter, setProcFilter] = useState<string>("");
  const [sortBy, setSortBy] = useState<"cpu" | "mem" | "pid">("cpu");
  const [sortOrder, setSortOrder] = useState<"asc" | "desc">("desc");
  const [killPid, setKillPid] = useState<string | null>(null);
  const [killing, setKilling] = useState<boolean>(false);

  useEffect(() => {
    if (initialSpec) {
      setActiveSpec(initialSpec);
      setActiveLabel(initialHostLabel || initialSpec.kind);
    }
  }, [initialSpec, initialHostLabel]);

  useEffect(() => {
    if (open) {
      listProfiles().then(setProfiles).catch(() => {});
    }
  }, [open]);

  const parseMetrics = (raw: string): SystemMetrics => {
    const lines = raw.split("\n");
    let section = "";
    const sections: Record<string, string[]> = {
      SYSINFO: [],
      CPU: [],
      MEM: [],
      DISK: [],
      PROC: [],
    };

    for (const line of lines) {
      const trimmed = line.trim();
      if (trimmed.startsWith("===SYSINFO===")) section = "SYSINFO";
      else if (trimmed.startsWith("===CPU===")) section = "CPU";
      else if (trimmed.startsWith("===MEM===")) section = "MEM";
      else if (trimmed.startsWith("===DISK===")) section = "DISK";
      else if (trimmed.startsWith("===PROC===")) section = "PROC";
      else if (section && trimmed) {
        sections[section].push(trimmed);
      }
    }

    // Parse OS & Hostname
    const os = sections.SYSINFO[0] || "Unknown OS";
    const hostname = sections.SYSINFO[1] || "localhost";
    const uptime = sections.SYSINFO[2] || "";

    // Parse Load Avg
    let loadAvg = "";
    const loadMatch = uptime.match(/load averages?:?\s*([\d.,\s]+)/i);
    if (loadMatch) {
      loadAvg = loadMatch[1].trim();
    }

    // Parse CPU
    let cpuPercent = 0;
    const cpuLines = sections.CPU;
    if (cpuLines.length >= 2 && cpuLines[0].startsWith("cpu ") && cpuLines[1].startsWith("cpu ")) {
      const p1 = cpuLines[0].split(/\s+/).slice(1).map(Number);
      const p2 = cpuLines[1].split(/\s+/).slice(1).map(Number);
      const idle1 = p1[3] + (p1[4] || 0);
      const idle2 = p2[3] + (p2[4] || 0);
      const total1 = p1.reduce((a, b) => a + b, 0);
      const total2 = p2.reduce((a, b) => a + b, 0);
      const totalDelta = total2 - total1;
      const idleDelta = idle2 - idle1;
      if (totalDelta > 0) {
        cpuPercent = Math.max(0, Math.min(100, Math.round(((totalDelta - idleDelta) / totalDelta) * 100)));
      }
    } else {
      const topCpuMatch = cpuLines.find((l) => l.includes("CPU usage") || l.includes("CPU:"));
      if (topCpuMatch) {
        const userMatch = topCpuMatch.match(/([\d.]+)%\s*user/i);
        const sysMatch = topCpuMatch.match(/([\d.]+)%\s*sys/i);
        const user = userMatch ? parseFloat(userMatch[1]) : 0;
        const sys = sysMatch ? parseFloat(sysMatch[1]) : 0;
        cpuPercent = Math.min(100, Math.round(user + sys));
      }
    }

    // Parse MEM
    let memTotalMb = 1024;
    let memUsedMb = 0;
    let memPercent = 0;
    const memLines = sections.MEM;
    const memRow = memLines.find((l) => l.startsWith("Mem:"));
    if (memRow) {
      const parts = memRow.split(/\s+/).slice(1).map(Number);
      memTotalMb = parts[0] || 1024;
      memUsedMb = parts[1] || 0;
      memPercent = Math.round((memUsedMb / memTotalMb) * 100);
    } else {
      // macOS vm_stat
      const memsizeLine = memLines.find((l) => /^\d{10,}$/.test(l));
      if (memsizeLine) {
        memTotalMb = Math.round(parseInt(memsizeLine, 10) / (1024 * 1024));
      }
      const pageActive = memLines.find((l) => l.includes("Pages active"));
      const pageWired = memLines.find((l) => l.includes("Pages wired down"));
      if (pageActive || pageWired) {
        const activeCount = pageActive ? parseInt(pageActive.replace(/\D/g, ""), 10) || 0 : 0;
        const wiredCount = pageWired ? parseInt(pageWired.replace(/\D/g, ""), 10) || 0 : 0;
        const pageSize = 4096;
        memUsedMb = Math.round(((activeCount + wiredCount) * pageSize) / (1024 * 1024));
        memPercent = Math.min(100, Math.round((memUsedMb / memTotalMb) * 100));
      }
    }

    // Parse Disks
    const disks: DiskItem[] = [];
    for (const dLine of sections.DISK.slice(1)) {
      const parts = dLine.split(/\s+/);
      if (parts.length >= 6) {
        const fs = parts[0];
        if (fs.startsWith("devfs") || fs.startsWith("map") || fs.startsWith("tmpfs") && parts[5] === "/dev") continue;
        const totalKb = parseInt(parts[1], 10) || 0;
        const usedKb = parseInt(parts[2], 10) || 0;
        const availKb = parseInt(parts[3], 10) || 0;
        const pctStr = parts[4].replace("%", "");
        const pct = parseInt(pctStr, 10) || (totalKb > 0 ? Math.round((usedKb / totalKb) * 100) : 0);
        const mount = parts[5];

        const formatSize = (kb: number) => {
          if (kb > 1024 * 1024) return `${(kb / (1024 * 1024)).toFixed(1)} GB`;
          return `${(kb / 1024).toFixed(0)} MB`;
        };

        disks.push({
          filesystem: fs,
          size: formatSize(totalKb),
          used: formatSize(usedKb),
          avail: formatSize(availKb),
          percent: pct,
          mount,
        });
      }
    }

    // Parse Processes
    const processes: ProcessItem[] = [];
    for (const pLine of sections.PROC.slice(1)) {
      const parts = pLine.trim().split(/\s+/);
      if (parts.length >= 5) {
        const pid = parts[0];
        const user = parts[1];
        const cpu = parseFloat(parts[2]) || 0;
        const mem = parseFloat(parts[3]) || 0;
        const command = parts.slice(4).join(" ");
        if (/^\d+$/.test(pid)) {
          processes.push({ pid, user, cpu, mem, command });
        }
      }
    }

    return {
      hostname,
      os,
      uptime,
      cpuPercent,
      cores: 4,
      memTotalMb,
      memUsedMb,
      memPercent,
      loadAvg,
      disks,
      processes,
    };
  };

  const fetchMetrics = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await execCommand(
        activeSpec,
        METRIC_SCRIPT,
        secretRef,
        password,
        jumpSecretRef,
        jumpPassword
      );
      if (res.exit_code === 0 || res.stdout.includes("===SYSINFO===")) {
        const parsed = parseMetrics(res.stdout);
        setMetrics(parsed);
      } else {
        setError(res.stderr || `Command exited with code ${res.exit_code}`);
      }
    } catch (err: any) {
      setError(err?.message || String(err));
    } finally {
      setLoading(false);
    }
  }, [activeSpec, secretRef, password, jumpSecretRef, jumpPassword]);

  useEffect(() => {
    if (!open) return;
    fetchMetrics();
    if (!autoRefresh) return;

    const timer = setInterval(() => {
      fetchMetrics();
    }, refreshInterval);

    return () => clearInterval(timer);
  }, [open, autoRefresh, refreshInterval, fetchMetrics]);

  const handleKillProcess = async (pid: string, signal: number = 15) => {
    setKilling(true);
    try {
      await execCommand(
        activeSpec,
        `kill -${signal} ${pid}`,
        secretRef,
        password,
        jumpSecretRef,
        jumpPassword
      );
      setKillPid(null);
      await fetchMetrics();
    } catch (err: any) {
      alert(`Failed to kill process ${pid}: ${err?.message || err}`);
    } finally {
      setKilling(false);
    }
  };

  if (!open) return null;

  const sortedProcesses = (metrics?.processes || [])
    .filter((p) => {
      if (!procFilter) return true;
      const q = procFilter.toLowerCase();
      return (
        p.pid.includes(q) ||
        p.user.toLowerCase().includes(q) ||
        p.command.toLowerCase().includes(q)
      );
    })
    .sort((a, b) => {
      let diff = 0;
      if (sortBy === "cpu") diff = a.cpu - b.cpu;
      else if (sortBy === "mem") diff = a.mem - b.mem;
      else if (sortBy === "pid") diff = parseInt(a.pid, 10) - parseInt(b.pid, 10);
      return sortOrder === "desc" ? -diff : diff;
    });

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-content"
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 900,
          maxWidth: "95vw",
          height: "85vh",
          display: "flex",
          flexDirection: "column",
          padding: 0,
          overflow: "hidden",
          background: "var(--term-bg, #18181b)",
          color: "var(--term-text, #f3f4f6)",
          border: "1px solid var(--term-border, #3f3f46)",
          borderRadius: 8,
          boxShadow: "0 20px 40px rgba(0,0,0,0.6)",
        }}
      >
        {/* Header */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "12px 18px",
            borderBottom: "1px solid var(--term-border, #3f3f46)",
            background: "rgba(0,0,0,0.2)",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <span style={{ fontSize: 18 }}>📊</span>
            <div>
              <div style={{ fontWeight: 600, fontSize: 15 }}>
                System Resource Monitor
              </div>
              <div style={{ fontSize: 11, color: "var(--term-text-muted, #9ca3af)" }}>
                Host: <strong style={{ color: "#60a5fa" }}>{activeLabel}</strong>
                {metrics?.hostname ? ` (${metrics.hostname})` : ""}
              </div>
            </div>
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            {/* Target Host Dropdown */}
            <select
              value={activeLabel}
              onChange={(e) => {
                const label = e.target.value;
                if (label === "Local Machine") {
                  setActiveSpec({ kind: "local", shell: null, cwd: null });
                  setActiveLabel("Local Machine");
                } else {
                  const prof = profiles.find((p) => p.name === label);
                  if (prof) {
                    setActiveSpec(prof.spec);
                    setActiveLabel(prof.name);
                  }
                }
              }}
              style={{
                fontSize: 12,
                padding: "4px 8px",
                background: "rgba(0,0,0,0.4)",
                color: "#f3f4f6",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 4,
              }}
            >
              <option value="Local Machine">💻 Local Machine</option>
              {profiles.map((p) => (
                <option key={p.id} value={p.name}>
                  🌐 {p.name} ({p.spec.kind})
                </option>
              ))}
            </select>

            <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12 }}>
              <label style={{ display: "flex", alignItems: "center", gap: 4, cursor: "pointer" }}>
                <input
                  type="checkbox"
                  checked={autoRefresh}
                  onChange={(e) => setAutoRefresh(e.target.checked)}
                />
                Auto-refresh
              </label>
              {autoRefresh && (
                <select
                  value={refreshInterval}
                  onChange={(e) => setRefreshInterval(Number(e.target.value))}
                  style={{
                    fontSize: 11,
                    padding: "2px 6px",
                    background: "rgba(0,0,0,0.3)",
                    color: "#ccc",
                    border: "1px solid #444",
                    borderRadius: 4,
                  }}
                >
                  <option value={1500}>1.5s</option>
                  <option value={3000}>3s</option>
                  <option value={5000}>5s</option>
                  <option value={10000}>10s</option>
                </select>
              )}
            </div>

            <button
              className="btn-secondary"
              onClick={fetchMetrics}
              disabled={loading}
              style={{ padding: "4px 10px", fontSize: 12 }}
            >
              {loading ? "Refreshing..." : "↻ Refresh"}
            </button>

            <button
              onClick={onClose}
              style={{
                background: "transparent",
                border: "none",
                color: "#9ca3af",
                fontSize: 18,
                cursor: "pointer",
              }}
            >
              ✕
            </button>
          </div>
        </div>

        {/* Content Body */}
        <div style={{ flex: 1, overflowY: "auto", padding: 18, display: "flex", flexDirection: "column", gap: 16 }}>
          {error && (
            <div
              style={{
                padding: "8px 12px",
                background: "rgba(239, 68, 68, 0.15)",
                border: "1px solid #ef4444",
                borderRadius: 6,
                color: "#fca5a5",
                fontSize: 12,
              }}
            >
              ⚠️ {error}
            </div>
          )}

          {/* Gauges row */}
          <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 14 }}>
            {/* CPU Gauge */}
            <div
              style={{
                background: "rgba(255,255,255,0.03)",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 8,
                padding: 14,
                display: "flex",
                flexDirection: "column",
                gap: 8,
              }}
            >
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span style={{ fontSize: 12, color: "#9ca3af", fontWeight: 600 }}>CPU UTILIZATION</span>
                <span style={{ fontSize: 18, fontWeight: 700, color: (metrics?.cpuPercent ?? 0) > 80 ? "#f87171" : "#60a5fa" }}>
                  {metrics?.cpuPercent ?? 0}%
                </span>
              </div>
              <div
                style={{
                  width: "100%",
                  height: 8,
                  background: "rgba(255,255,255,0.1)",
                  borderRadius: 4,
                  overflow: "hidden",
                }}
              >
                <div
                  style={{
                    width: `${metrics?.cpuPercent ?? 0}%`,
                    height: "100%",
                    background: (metrics?.cpuPercent ?? 0) > 80 ? "#ef4444" : "#3b82f6",
                    transition: "width 0.3s ease",
                  }}
                />
              </div>
              <div style={{ fontSize: 11, color: "#9ca3af", display: "flex", justifyContent: "space-between" }}>
                <span>Load Avg: {metrics?.loadAvg || "N/A"}</span>
              </div>
            </div>

            {/* RAM Gauge */}
            <div
              style={{
                background: "rgba(255,255,255,0.03)",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 8,
                padding: 14,
                display: "flex",
                flexDirection: "column",
                gap: 8,
              }}
            >
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span style={{ fontSize: 12, color: "#9ca3af", fontWeight: 600 }}>MEMORY USAGE</span>
                <span style={{ fontSize: 18, fontWeight: 700, color: (metrics?.memPercent ?? 0) > 85 ? "#f87171" : "#34d399" }}>
                  {metrics?.memPercent ?? 0}%
                </span>
              </div>
              <div
                style={{
                  width: "100%",
                  height: 8,
                  background: "rgba(255,255,255,0.1)",
                  borderRadius: 4,
                  overflow: "hidden",
                }}
              >
                <div
                  style={{
                    width: `${metrics?.memPercent ?? 0}%`,
                    height: "100%",
                    background: (metrics?.memPercent ?? 0) > 85 ? "#ef4444" : "#10b981",
                    transition: "width 0.3s ease",
                  }}
                />
              </div>
              <div style={{ fontSize: 11, color: "#9ca3af", display: "flex", justifyContent: "space-between" }}>
                <span>Used: {metrics?.memUsedMb ?? 0} MB</span>
                <span>Total: {metrics?.memTotalMb ?? 0} MB</span>
              </div>
            </div>

            {/* System Info Box */}
            <div
              style={{
                background: "rgba(255,255,255,0.03)",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 8,
                padding: 14,
                display: "flex",
                flexDirection: "column",
                justifyContent: "space-between",
                gap: 6,
              }}
            >
              <div style={{ fontSize: 12, color: "#9ca3af", fontWeight: 600 }}>SYSTEM OVERVIEW</div>
              <div style={{ fontSize: 12, color: "#d1d5db" }}>
                <div><strong>OS:</strong> {metrics?.os || "Loading..."}</div>
                <div><strong>Host:</strong> {metrics?.hostname || "..."}</div>
                <div style={{ fontSize: 11, color: "#9ca3af", marginTop: 2, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {metrics?.uptime || ""}
                </div>
              </div>
            </div>
          </div>

          {/* Disk usage storage breakdown */}
          {metrics?.disks && metrics.disks.length > 0 && (
            <div
              style={{
                background: "rgba(255,255,255,0.02)",
                border: "1px solid var(--term-border, #3f3f46)",
                borderRadius: 8,
                padding: "12px 14px",
              }}
            >
              <div style={{ fontSize: 12, fontWeight: 600, color: "#9ca3af", marginBottom: 8 }}>
                MOUNTED STORAGE & DISKS
              </div>
              <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))", gap: 10 }}>
                {metrics.disks.slice(0, 4).map((d, i) => (
                  <div
                    key={i}
                    style={{
                      background: "rgba(0,0,0,0.25)",
                      padding: 8,
                      borderRadius: 6,
                      border: "1px solid rgba(255,255,255,0.05)",
                    }}
                  >
                    <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11, marginBottom: 4 }}>
                      <strong style={{ color: "#e5e7eb" }}>{d.mount}</strong>
                      <span style={{ color: d.percent > 90 ? "#f87171" : "#9ca3af" }}>
                        {d.used} / {d.size} ({d.percent}%)
                      </span>
                    </div>
                    <div style={{ width: "100%", height: 5, background: "rgba(255,255,255,0.08)", borderRadius: 3, overflow: "hidden" }}>
                      <div
                        style={{
                          width: `${Math.min(100, d.percent)}%`,
                          height: "100%",
                          background: d.percent > 90 ? "#ef4444" : d.percent > 70 ? "#f59e0b" : "#3b82f6",
                        }}
                      />
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Process Manager Section */}
          <div
            style={{
              flex: 1,
              background: "rgba(0,0,0,0.2)",
              border: "1px solid var(--term-border, #3f3f46)",
              borderRadius: 8,
              display: "flex",
              flexDirection: "column",
              overflow: "hidden",
            }}
          >
            {/* Process toolbar */}
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                padding: "8px 12px",
                borderBottom: "1px solid var(--term-border, #3f3f46)",
                background: "rgba(255,255,255,0.02)",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span style={{ fontSize: 13, fontWeight: 600 }}>Active Processes</span>
                <span style={{ fontSize: 11, color: "#9ca3af" }}>
                  ({sortedProcesses.length} shown)
                </span>
              </div>
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  type="text"
                  placeholder="Filter process name, user, PID..."
                  value={procFilter}
                  onChange={(e) => setProcFilter(e.target.value)}
                  style={{
                    fontSize: 11,
                    padding: "4px 8px",
                    background: "rgba(0,0,0,0.3)",
                    color: "#f3f4f6",
                    border: "1px solid #444",
                    borderRadius: 4,
                    width: 200,
                  }}
                />
              </div>
            </div>

            {/* Process Table Header */}
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "70px 90px 70px 70px 1fr 80px",
                padding: "6px 12px",
                fontSize: 11,
                fontWeight: 600,
                color: "#9ca3af",
                background: "rgba(0,0,0,0.3)",
                borderBottom: "1px solid rgba(255,255,255,0.05)",
                userSelect: "none",
              }}
            >
              <div
                style={{ cursor: "pointer" }}
                onClick={() => {
                  setSortBy("pid");
                  setSortOrder(sortOrder === "asc" ? "desc" : "asc");
                }}
              >
                PID {sortBy === "pid" && (sortOrder === "asc" ? "▲" : "▼")}
              </div>
              <div>USER</div>
              <div
                style={{ cursor: "pointer" }}
                onClick={() => {
                  setSortBy("cpu");
                  setSortOrder(sortOrder === "asc" ? "desc" : "asc");
                }}
              >
                %CPU {sortBy === "cpu" && (sortOrder === "asc" ? "▲" : "▼")}
              </div>
              <div
                style={{ cursor: "pointer" }}
                onClick={() => {
                  setSortBy("mem");
                  setSortOrder(sortOrder === "asc" ? "desc" : "asc");
                }}
              >
                %MEM {sortBy === "mem" && (sortOrder === "asc" ? "▲" : "▼")}
              </div>
              <div>COMMAND</div>
              <div style={{ textAlign: "right" }}>ACTION</div>
            </div>

            {/* Process rows */}
            <div style={{ flex: 1, overflowY: "auto", maxHeight: 280 }}>
              {sortedProcesses.length === 0 ? (
                <div style={{ padding: 20, textAlign: "center", color: "#9ca3af", fontSize: 12 }}>
                  {loading ? "Loading processes..." : "No matching processes"}
                </div>
              ) : (
                sortedProcesses.map((p) => (
                  <div
                    key={p.pid}
                    style={{
                      display: "grid",
                      gridTemplateColumns: "70px 90px 70px 70px 1fr 80px",
                      padding: "5px 12px",
                      fontSize: 11,
                      borderBottom: "1px solid rgba(255,255,255,0.03)",
                      alignItems: "center",
                      fontFamily: "monospace",
                    }}
                  >
                    <div style={{ color: "#60a5fa" }}>{p.pid}</div>
                    <div style={{ color: "#d1d5db", overflow: "hidden", textOverflow: "ellipsis" }}>
                      {p.user}
                    </div>
                    <div style={{ color: p.cpu > 20 ? "#f87171" : "#e5e7eb", fontWeight: p.cpu > 20 ? 600 : 400 }}>
                      {p.cpu.toFixed(1)}%
                    </div>
                    <div style={{ color: p.mem > 15 ? "#fbbf24" : "#e5e7eb" }}>
                      {p.mem.toFixed(1)}%
                    </div>
                    <div
                      style={{
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                        color: "#9ca3af",
                      }}
                      title={p.command}
                    >
                      {p.command}
                    </div>
                    <div style={{ textAlign: "right" }}>
                      <button
                        onClick={() => setKillPid(p.pid)}
                        style={{
                          background: "rgba(239, 68, 68, 0.2)",
                          border: "1px solid rgba(239, 68, 68, 0.4)",
                          color: "#f87171",
                          borderRadius: 3,
                          padding: "1px 6px",
                          fontSize: 10,
                          cursor: "pointer",
                        }}
                      >
                        Kill
                      </button>
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>

        {/* Kill Process Dialog */}
        {killPid && (
          <div
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              background: "rgba(0,0,0,0.75)",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              zIndex: 100,
            }}
          >
            <div
              style={{
                background: "#27272a",
                border: "1px solid #52525b",
                borderRadius: 8,
                padding: 18,
                maxWidth: 400,
                width: "90%",
                boxShadow: "0 10px 25px rgba(0,0,0,0.5)",
              }}
            >
              <div style={{ fontWeight: 600, fontSize: 14, color: "#f87171", marginBottom: 8 }}>
                Terminate Process {killPid}?
              </div>
              <p style={{ fontSize: 12, color: "#d1d5db", margin: "0 0 16px 0" }}>
                Are you sure you want to kill PID <strong>{killPid}</strong> on {activeLabel}?
              </p>
              <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
                <button
                  className="btn-secondary"
                  onClick={() => setKillPid(null)}
                  disabled={killing}
                  style={{ fontSize: 12 }}
                >
                  Cancel
                </button>
                <button
                  className="btn-secondary"
                  onClick={() => handleKillProcess(killPid, 15)}
                  disabled={killing}
                  style={{ fontSize: 12, background: "rgba(245, 158, 11, 0.2)", borderColor: "#f59e0b", color: "#fcd34d" }}
                >
                  SIGTERM (15)
                </button>
                <button
                  className="btn-primary"
                  onClick={() => handleKillProcess(killPid, 9)}
                  disabled={killing}
                  style={{ fontSize: 12, background: "#dc2626", borderColor: "#ef4444" }}
                >
                  SIGKILL (9)
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
