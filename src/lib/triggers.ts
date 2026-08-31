export interface TerminalTrigger {
  id: string;
  name: string;
  pattern: string;
  isRegex: boolean;
  enabled: boolean;
  action: "notify" | "sound" | "both";
  soundBeep: boolean;
}

export const DEFAULT_TRIGGERS: TerminalTrigger[] = [
  {
    id: "trig-build-done",
    name: "Build / Compilation Finished",
    pattern: "(Finished .* in|Build complete|Build succeeded|Done in \\d+|Compilation successful|Compiled successfully)",
    isRegex: true,
    enabled: true,
    action: "both",
    soundBeep: true,
  },
  {
    id: "trig-error-fatal",
    name: "Error / Fatal Exception",
    pattern: "(ERROR:|FATAL:|panic: |Traceback \\(most recent call last\\):|Unhandled exception)",
    isRegex: true,
    enabled: true,
    action: "both",
    soundBeep: true,
  },
  {
    id: "trig-password-prompt",
    name: "Password / Passphrase Prompt",
    pattern: "([pP]assword for|[pP]assword:|[vV]erification code:|Enter passphrase)",
    isRegex: true,
    enabled: true,
    action: "notify",
    soundBeep: false,
  },
  {
    id: "trig-tests-pass",
    name: "Test Suite Passed",
    pattern: "(test result: ok|All \\d+ tests passed|Tests: \\d+ passed)",
    isRegex: true,
    enabled: true,
    action: "both",
    soundBeep: true,
  },
];

const STORAGE_KEY = "terminator_terminal_triggers";

export function loadTriggers(): TerminalTrigger[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_TRIGGERS;
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) && parsed.length > 0 ? parsed : DEFAULT_TRIGGERS;
  } catch {
    return DEFAULT_TRIGGERS;
  }
}

export function saveTriggers(triggers: TerminalTrigger[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(triggers));
  } catch {
    // Ignore storage quota error
  }
}

// Play pleasant audio chime on desktop
export function playChime(isError = false) {
  try {
    const ctx = new (window.AudioContext || (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext)();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();

    osc.type = isError ? "sawtooth" : "sine";
    const now = ctx.currentTime;

    if (isError) {
      osc.frequency.setValueAtTime(440, now);
      osc.frequency.setValueAtTime(330, now + 0.1);
      gain.gain.setValueAtTime(0.15, now);
      gain.gain.exponentialRampToValueAtTime(0.01, now + 0.3);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now);
      osc.stop(now + 0.3);
    } else {
      osc.frequency.setValueAtTime(587.33, now); // D5
      osc.frequency.setValueAtTime(880, now + 0.08); // A5
      gain.gain.setValueAtTime(0.15, now);
      gain.gain.exponentialRampToValueAtTime(0.01, now + 0.25);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now);
      osc.stop(now + 0.25);
    }
  } catch {
    // Audio context may be restricted before user gesture
  }
}

export async function requestNotificationPermission(): Promise<boolean> {
  if (!("Notification" in window)) return false;
  if (Notification.permission === "granted") return true;
  if (Notification.permission !== "denied") {
    const perm = await Notification.requestPermission();
    return perm === "granted";
  }
  return false;
}

export function sendDesktopNotification(title: string, body: string) {
  if (!("Notification" in window) || Notification.permission !== "granted") return;
  try {
    new Notification(title, {
      body,
      silent: true, // We manage our own sound chime
    });
  } catch {
    // Fall back quietly
  }
}
