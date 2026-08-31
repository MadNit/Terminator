import { invoke, Channel } from "@tauri-apps/api/core";

export type SshAuth =
  | { method: "agent" }
  | { method: "password" }
  | { method: "key"; path: string };

export type TransportSpec =
  | { kind: "local"; shell: string | null; cwd: string | null }
  | { kind: "ssh"; host: string; port: number; user: string; auth: SshAuth }
  | {
      kind: "rdp";
      host: string;
      port: number;
      user: string;
      domain: string | null;
    };

export type Profile = {
  id: string;
  name: string;
  group: string | null;
  spec: TransportSpec;
};

export type SessionEvent =
  | { type: "output"; data: string }
  | { type: "exit" };

// Tauri IPC is JSON, so binary is base64-framed. These helpers avoid
// String.fromCharCode(...bigArray), which blows the call stack on large chunks.
export function decodeB64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function encodeB64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

export async function openSession(
  spec: TransportSpec,
  cols: number,
  rows: number,
  onEvent: (e: SessionEvent) => void,
  secretRef?: string,
  password?: string,
): Promise<string> {
  const channel = new Channel<SessionEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("open_session", {
    spec,
    cols,
    rows,
    secretRef: secretRef ?? null,
    password: password ?? null,
    channel,
  });
}

export const listProfiles = () => invoke<Profile[]>("list_profiles");

export const saveProfile = (
  name: string,
  group: string | null,
  spec: TransportSpec,
) => invoke<string>("save_profile", { name, group, spec });

export const updateProfile = (
  id: string,
  name: string,
  group: string | null,
  spec: TransportSpec,
) => invoke<void>("update_profile", { id, name, group, spec });

export const deleteProfile = (id: string) =>
  invoke("delete_profile", { id });

export const setSecret = (key: string, value: string) =>
  invoke("set_secret", { key, value });

export const deleteSecret = (key: string) => invoke("delete_secret", { key });

/** Moves a stored credential when an edit changes its derived key. */
export const renameSecret = (from: string, to: string) =>
  invoke<void>("rename_secret", { from, to });

export const writeSession = (id: string, data: string) =>
  invoke("write_session", { id, data: encodeB64(data) });

export const resizeSession = (id: string, cols: number, rows: number) =>
  invoke("resize_session", { id, cols, rows });

export const closeSession = (id: string) => invoke("close_session", { id });

export const sessionLogs = (id: string) =>
  invoke<{ cast: string; plain: string }>("session_logs", { id });

export type SessionLogItem = {
  id: string;
  dirName: string;
  timestamp: number;
  castPath: string;
  plainPath: string;
  plainSize: number;
  castSize: number;
};

export const listSessionLogs = () => invoke<SessionLogItem[]>("list_session_logs");
export const readLogFile = (path: string) => invoke<string>("read_log_file", { path });
export const deleteSessionLog = (dirName: string) => invoke<void>("delete_session_log", { dirName });
export const logDir = () => invoke<string>("log_dir");

export const secretsBackend = () => invoke<string>("secrets_backend");

export const searchCommands = (query: string, limit?: number) =>
  invoke<{ command: string; exitCode: number | null }[]>("search_commands", {
    query,
    limit,
  });

export type CommandRecord = {
  command: string;
  exitCode: number | null;
  durationMs: number;
};

/** Per-command history for one session, captured via OSC 133. */
export const sessionCommands = (id: string) =>
  invoke<CommandRecord[]>("session_commands", { id });

export const shellIntegrationSnippet = () =>
  invoke<string>("shell_integration_snippet");

export type VaultStatus = {
  /** "keychain" or "file". */
  backend: string;
  /** True when a passphrase is still required before secrets can be used. */
  locked: boolean;
  /** False on first run, when the vault must be created rather than unlocked. */
  initialized: boolean;
};

export const vaultStatus = () => invoke<VaultStatus>("vault_status");

export const unlockVault = (passphrase: string) =>
  invoke<void>("unlock_vault", { passphrase });

export const lockVault = () => invoke<void>("lock_vault");

export const changeVaultPassphrase = (passphrase: string) =>
  invoke<void>("change_vault_passphrase", { passphrase });

/** Forward a message to the Rust log; the webview has no visible console. */
export const logFrontend = (level: "info" | "warn" | "error", message: string) =>
  invoke<void>("log_frontend", { level, message }).catch(() => {});

/** Whether a credential is already saved, so the UI can prompt before connecting. */
export const hasSecret = (key: string) => invoke<boolean>("has_secret", { key });

// ---------------------------------------------------------------------------
// File browser
// ---------------------------------------------------------------------------

export type EntryKind = "dir" | "file" | "other";

export type FileEntry = {
  name: string;
  path: string;
  kind: EntryKind;
  size: number;
  /** Unix epoch seconds, or null when unavailable. */
  modified: number | null;
  hidden: boolean;
  symlink: boolean;
};

export type Listing = {
  path: string;
  /** null at the filesystem root, which disables the "up" control. */
  parent: string | null;
  entries: FileEntry[];
};

export type TransferEvent =
  | { type: "progress"; transferred: number; total: number }
  | { type: "done"; bytes: number }
  | { type: "failed"; message: string };

export const localHome = () => invoke<string>("local_home");

export const listLocalDir = (path: string) =>
  invoke<Listing>("list_local_dir", { path });

export const remoteHome = (id: string) =>
  invoke<string>("remote_home", { id });

export const listRemoteDir = (id: string, path: string) =>
  invoke<Listing>("list_remote_dir", { id, path });

export const remoteMkdir = (id: string, path: string) =>
  invoke<void>("remote_mkdir", { id, path });

export const remoteRemove = (id: string, path: string, isDir: boolean) =>
  invoke<void>("remote_remove", { id, path, isDir });

export const remoteRename = (id: string, from: string, to: string) =>
  invoke<void>("remote_rename", { id, from, to });

export function uploadFile(
  id: string,
  local: string,
  remote: string,
  onEvent: (e: TransferEvent) => void,
): Promise<number> {
  const channel = new Channel<TransferEvent>();
  channel.onmessage = onEvent;
  return invoke<number>("upload_file", { id, local, remote, channel });
}

export function downloadFile(
  id: string,
  remote: string,
  local: string,
  onEvent: (e: TransferEvent) => void,
): Promise<number> {
  const channel = new Channel<TransferEvent>();
  channel.onmessage = onEvent;
  return invoke<number>("download_file", { id, remote, local, channel });
}

/** Remote paths are POSIX regardless of the client platform. */
export function posixJoin(base: string, name: string): string {
  if (name.startsWith("/")) return name;
  return base.endsWith("/") ? `${base}${name}` : `${base}/${name}`;
}

/**
 * Reserve a local path to download into before starting a native drag.
 *
 * A drag payload has to be a real file on this machine, so a remote file must
 * be fetched somewhere first. The staging area is cleared on every app start.
 */
export const stagePath = (name: string) => invoke<string>("stage_path", { name });

/**
 * Write bytes from the webview to a real file and return its path.
 *
 * Files arriving by paste or by webview drop come through as byte arrays with
 * no filesystem path (the browser deliberately withholds it), but the SFTP
 * upload takes a path -- this bridges the two.
 */
export const stageBytes = (name: string, bytes: Uint8Array) =>
  invoke<string>("stage_bytes", { name, bytes: Array.from(bytes) });

/**
 * Start an OS-level drag carrying local files, so a drop lands in the Finder,
 * Explorer or any other application.
 *
 * Invoked by name rather than through a JS binding: the plugin ships no npm
 * package, only the Rust command.
 */
export async function startNativeDrag(paths: string[], iconPng: string) {
  const onEvent = new Channel<unknown>();
  await invoke("plugin:drag|start_drag", {
    item: paths,
    image: iconPng,
    onEvent,
  });
}

/** Local paths use the platform separator, inferred from the path itself. */
export function localJoin(base: string, name: string): string {
  const sep = base.includes("\\") && !base.includes("/") ? "\\" : "/";
  return base.endsWith(sep) ? `${base}${name}` : `${base}${sep}${name}`;
}

// ---------------------------------------------------------------------------
// RDP
//
// A parallel path to openSession: RDP is a framebuffer protocol, so it moves
// rectangles and input events rather than bytes.
// ---------------------------------------------------------------------------

export type RdpEvent =
  | { type: "frame"; x: number; y: number; w: number; h: number; rgba: string }
  | { type: "resized"; width: number; height: number }
  | { type: "disconnected"; reason: string };

export type RdpInput =
  | { type: "mouseMove"; x: number; y: number }
  | { type: "mouseDown"; button: number }
  | { type: "mouseUp"; button: number }
  | { type: "wheel"; delta: number; horizontal: boolean }
  | { type: "keyDown"; scancode: number }
  | { type: "keyUp"; scancode: number }
  | { type: "unicodeChar"; ch: string }
  | { type: "releaseAll" };

export type RdpOpened = { id: string; width: number; height: number };

export async function openRdp(
  spec: TransportSpec,
  width: number,
  height: number,
  onEvent: (e: RdpEvent) => void,
  opts: { secretRef?: string; password?: string } = {},
): Promise<RdpOpened> {
  const channel = new Channel<RdpEvent>();
  channel.onmessage = onEvent;
  return invoke<RdpOpened>("open_rdp", {
    spec,
    width,
    height,
    secretRef: opts.secretRef ?? null,
    password: opts.password ?? null,
    channel,
  });
}

export const rdpInput = (id: string, ops: RdpInput[]) =>
  invoke<void>("rdp_input", { id, ops });

export const rdpResize = (id: string, width: number, height: number) =>
  invoke<void>("rdp_resize", { id, width, height });

export const closeRdp = (id: string) => invoke<void>("close_rdp", { id });
