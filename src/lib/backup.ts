import {
  listProfiles,
  saveProfile,
  listSnippets,
  saveSnippet,
  listTunnels,
  saveTunnel,
  listKnownHosts,
  addKnownHost,
  type Profile,
  type Snippet,
  type TunnelConfig,
  type KnownHostEntry,
} from "./api";
import { loadTriggers, saveTriggers, type TerminalTrigger } from "./triggers";

export interface TerminatorBackup {
  version: 1;
  exportedAt: string;
  app: "Terminator";
  profiles: Profile[];
  tunnels: TunnelConfig[];
  snippets: Snippet[];
  knownHosts: KnownHostEntry[];
  triggers: TerminalTrigger[];
}

export async function generateBackupData(): Promise<TerminatorBackup> {
  let profiles: Profile[] = [];
  let tunnels: TunnelConfig[] = [];
  let snippets: Snippet[] = [];
  let knownHosts: KnownHostEntry[] = [];
  const triggers: TerminalTrigger[] = loadTriggers();

  try {
    profiles = await listProfiles();
  } catch (e) {
    console.error("Failed to load profiles for backup:", e);
  }

  try {
    tunnels = await listTunnels();
  } catch (e) {
    console.error("Failed to load tunnels for backup:", e);
  }

  try {
    snippets = await listSnippets();
  } catch (e) {
    console.error("Failed to load snippets for backup:", e);
  }

  try {
    knownHosts = await listKnownHosts();
  } catch (e) {
    console.error("Failed to load known hosts for backup:", e);
  }

  return {
    version: 1,
    exportedAt: new Date().toISOString(),
    app: "Terminator",
    profiles,
    tunnels,
    snippets,
    knownHosts,
    triggers,
  };
}

export async function downloadBackupFile() {
  const data = await generateBackupData();
  const jsonStr = JSON.stringify(data, null, 2);
  const blob = new Blob([jsonStr], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const dateStr = new Date().toISOString().split("T")[0];

  const a = document.createElement("a");
  a.href = url;
  a.download = `terminator-backup-${dateStr}.json`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

export interface ImportResult {
  success: boolean;
  message: string;
  counts: {
    profiles: number;
    tunnels: number;
    snippets: number;
    knownHosts: number;
    triggers: number;
  };
}

export async function restoreBackupData(
  backup: Partial<TerminatorBackup>,
  mode: "merge" | "replace" = "merge",
): Promise<ImportResult> {
  const counts = {
    profiles: 0,
    tunnels: 0,
    snippets: 0,
    knownHosts: 0,
    triggers: 0,
  };

  try {
    if (Array.isArray(backup.profiles)) {
      for (const p of backup.profiles) {
        if (p.name && p.spec) {
          try {
            await saveProfile(p.name, p.group ?? null, p.spec);
            counts.profiles += 1;
          } catch {
            // Skip duplicates or errors
          }
        }
      }
    }

    if (Array.isArray(backup.tunnels)) {
      for (const t of backup.tunnels) {
        if (t.name && t.kind) {
          try {
            await saveTunnel(t);
            counts.tunnels += 1;
          } catch {
            // Skip errors
          }
        }
      }
    }

    if (Array.isArray(backup.snippets)) {
      for (const s of backup.snippets) {
        if (s.title && s.command) {
          try {
            await saveSnippet(s);
            counts.snippets += 1;
          } catch {
            // Skip errors
          }
        }
      }
    }

    if (Array.isArray(backup.knownHosts)) {
      for (const k of backup.knownHosts) {
        if (k.host_pattern && k.key_type && k.public_key) {
          try {
            await addKnownHost(k.host_pattern, k.key_type, k.public_key, k.comment);
            counts.knownHosts += 1;
          } catch {
            // Skip errors
          }
        }
      }
    }

    if (Array.isArray(backup.triggers)) {
      if (mode === "replace") {
        saveTriggers(backup.triggers);
      } else {
        const existing = loadTriggers();
        const map = new Map<string, TerminalTrigger>();
        for (const t of existing) map.set(t.id, t);
        for (const t of backup.triggers) map.set(t.id, t);
        saveTriggers(Array.from(map.values()));
      }
      counts.triggers = backup.triggers.length;
    }

    return {
      success: true,
      message: `Successfully imported items into Terminator (${mode === "merge" ? "merged" : "restored"}).`,
      counts,
    };
  } catch (err) {
    return {
      success: false,
      message: `Failed to import backup: ${String(err)}`,
      counts,
    };
  }
}
