import { useCallback, useEffect, useState } from "react";
import { TerminalPane } from "./components/TerminalPane";
import { Sidebar } from "./components/Sidebar";
import { ConnectDialog, type NewConnection } from "./components/ConnectDialog";
import { UnlockGate } from "./components/UnlockGate";
import { PasswordPrompt } from "./components/PasswordPrompt";
import { ProfileView } from "./components/ProfileView";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { AppHeader } from "./components/AppHeader";
import { SessionHistoryModal } from "./components/SessionHistoryModal";
import { TunnelManagerModal } from "./components/TunnelManagerModal";
import { SnippetManagerModal } from "./components/SnippetManagerModal";
import { KnownHostsModal } from "./components/KnownHostsModal";
import { TriggerManagerModal } from "./components/TriggerManagerModal";
import { BackupModal } from "./components/BackupModal";
import { ThemeCustomizerModal } from "./components/ThemeCustomizerModal";
import { ResourceMonitorModal } from "./components/ResourceMonitorModal";
import { BatchRunnerModal } from "./components/BatchRunnerModal";
import { CommandPalette } from "./components/CommandPalette";
import FileDrawer from "./components/FileDrawer";
import { RdpPane } from "./components/RdpPane";
import { RemoteEditorModal, type OpenFileTarget } from "./components/RemoteEditorModal";
import { connectBlockedReason, describeTarget } from "./lib/transport";
import {
  deleteProfile,
  deleteSecret,
  listSessions,
  renameSecret,
  updateProfile,
  listProfiles,
  logDir,
  saveProfile,
  secretsBackend,
  vaultStatus,
  type VaultStatus,
  hasSecret,
  setSecret,
  type DaemonSession,
  type Profile,
  type TransportSpec,
  writeSession,
} from "./lib/api";
import "./App.css";
import "./tunnel.css";
import "./snippet.css";

interface Tab {
  key: number;
  title: string;
  spec: TransportSpec;
  secretRef?: string;
  /** Held in memory only, for "don't remember" connections. */
  password?: string;
  jumpSecretRef?: string;
  jumpPassword?: string;
  /** Links a tab back to the saved profile that opened it, so the sidebar can
   *  show live state and offer Disconnect. Absent for ad-hoc local shells. */
  profileId?: string;
  sessionId: string | null;
  exited: boolean;
  /** Bumped to remount the pane, which is what a reconnect is. */
  gen: number;
  /** Reattach path: this tab was added because the user asked to
   *  reattach to a session the daemon is still hosting, rather
   *  than opening a fresh one. The `reattachId` is the daemon
   *  session id; the pane uses it to call `attachSession` instead
   *  of `openSession` on mount. */
  reattaching?: boolean;
  reattachId?: string;
}

const localSpec = (): TransportSpec => ({
  kind: "local",
  shell: null,
  cwd: null,
});

/** Keychain entry name for a connection's password. */
const secretKeyFor = (spec: TransportSpec) => {
  switch (spec.kind) {
    case "ssh":
      return `ssh:${spec.user}@${spec.host}:${spec.port}`;
    // Namespaced by scheme so an SSH and an RDP profile on the same host and
    // port cannot collide on one keychain entry.
    case "rdp":
      return `rdp:${spec.user}@${spec.host}:${spec.port}`;
    default:
      return "";
  }
};

let nextKey = 1;

export default function App() {
  const [tabs, setTabs] = useState<Tab[]>([
    { key: 0, title: "shell", spec: localSpec(), sessionId: null, exited: false, gen: 0 },
  ]);
  const [active, setActive] = useState(0);
  const [status, setStatus] = useState("");
  const [logs, setLogs] = useState("");
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [dialog, setDialog] = useState(false);
  const [busy, setBusy] = useState(false);
  const [vault, setVault] = useState<VaultStatus | null>(null);
  /** Set when a profile needs a password we don't have stored. */
  const [pwPrompt, setPwPrompt] = useState<Profile | null>(null);
  /** Profile whose details are showing. Null means the terminal is visible. */
  const [selectedId, setSelectedId] = useState<string | null>(null);
  /** Profile awaiting delete confirmation. */
  const [confirmDelete, setConfirmDelete] = useState<Profile | null>(null);
  /** Profile being edited in the connection dialog. */
  const [editing, setEditing] = useState<Profile | null>(null);
  /** Pre-fill the Connect dialog from this spec, without
   *  binding to any saved profile. Used by the "Reopen" button
   *  on a previously-open session. */
  const [prefillSpec, setPrefillSpec] = useState<TransportSpec | null>(null);
  /** Sessions the daemon is still hosting. We poll once on app
   *  start: if the user closed the app while a tab was open,
   *  the daemon kept the PTY alive and these are the tabs we
   *  can reattach to. Empty when the daemon reports nothing. */
  const [liveSessions, setLiveSessions] = useState<DaemonSession[]>([]);
  /** True after we've checked the daemon once. Until then we
   *  don't render the reattach prompt so the empty state
   *  doesn't flash. */
  const [liveSessionsChecked, setLiveSessionsChecked] = useState(false);
  // Quick-connect filter, owned here so the header and sidebar stay in sync.
  const [query, setQuery] = useState("");
  // Sidebar visibility, persisted: a user who works full-width wants it to
  // stay that way across restarts.
  const [sidebarOpen, setSidebarOpen] = useState(
    () => localStorage.getItem("sidebarOpen") !== "0",
  );
  // File drawer, persisted like the sidebar. Closed by default: it is a tool
  // you reach for, not something that should eat width on first launch.
  const [historyOpen, setHistoryOpen] = useState(false);
  const [tunnelsOpen, setTunnelsOpen] = useState(false);
  const [snippetsOpen, setSnippetsOpen] = useState(false);
  const [knownHostsOpen, setKnownHostsOpen] = useState(false);
  const [triggersOpen, setTriggersOpen] = useState(false);
  const [backupOpen, setBackupOpen] = useState(false);
  const [themesOpen, setThemesOpen] = useState(false);
  const [monitorOpen, setMonitorOpen] = useState(false);
  const [batchOpen, setBatchOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editorInitialFile, setEditorInitialFile] = useState<OpenFileTarget | null>(null);
  const [filesOpen, setFilesOpen] = useState(
    () => localStorage.getItem("filesOpen") === "1",
  );
  // Split pane layout: 1x1, 1x2 (vert), 2x1 (horiz), 2x2 (grid)
  const [splitLayout, setSplitLayout] = useState<"1x1" | "1x2" | "2x1" | "2x2">("1x1");
  // Multi-exec broadcast mode
  const [broadcast, setBroadcast] = useState(false);
  // Track focused tab key in multi-pane view
  const [focusedPaneKey, setFocusedPaneKey] = useState<number | null>(null);

  useEffect(() => {
    localStorage.setItem("sidebarOpen", sidebarOpen ? "1" : "0");
  }, [sidebarOpen]);

  useEffect(() => {
    localStorage.setItem("filesOpen", filesOpen ? "1" : "0");
  }, [filesOpen]);

  // Global Keyboard Shortcuts (Cmd/Ctrl+B, Cmd/Ctrl+J, Cmd/Ctrl+K, Cmd/Ctrl+P, Cmd/Ctrl+E)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "k" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
      if (e.key.toLowerCase() === "e" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setEditorOpen((v) => !v);
      }
      if (e.key.toLowerCase() === "p" && (e.metaKey || e.ctrlKey) && e.shiftKey) {
        e.preventDefault();
        setSnippetsOpen((v) => !v);
      }
      if (e.key.toLowerCase() === "b" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setSidebarOpen((v) => !v);
      }
      if (e.key.toLowerCase() === "j" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        setFilesOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const selected = profiles.find((p) => p.id === selectedId) ?? null;
  // The tab the file drawer's remote pane follows.
  const activeTab = tabs.find((t) => t.key === active) ?? null;

  // A profile counts as connected while it has a tab whose session has not
  // exited. Derived rather than stored: a duplicated flag would drift the
  // moment a session died on its own.
  const connectedIds = new Set(
    tabs.filter((t) => t.profileId && !t.exited).map((t) => t.profileId!),
  );

  const refreshProfiles = useCallback(async () => {
    try {
      setProfiles(await listProfiles());
    } catch (err) {
      setStatus(String(err));
    }
  }, []);

  /** Ask the daemon what it's still hosting, once on app start.
   *  Errors are swallowed: a stale daemon that just exited is
   *  not a "reattach to nothing" error, it's just "no
   *  reattach prompt" and we move on. */
  useEffect(() => {
    void (async () => {
      try {
        const list = await listSessions();
        setLiveSessions(list);
      } catch {
        setLiveSessions([]);
      } finally {
        setLiveSessionsChecked(true);
      }
    })();
  }, []);

  /** Reattach a tab to a session the daemon is still hosting. */
  const reattachSession = (s: DaemonSession) => {
    const key = nextKey++;
    setTabs((t) => [
      ...t,
      {
        key,
        title: describeTarget(s.spec) || "session",
        spec: s.spec,
        sessionId: null, // populated by the onReady callback
        exited: false,
        gen: 0,
        // Reattach: the spec carries the credential method
        // but we don't re-prompt for it -- the daemon is
        // already authenticated. The Tauri command is
        // attach_session (not open_session), so it doesn't
        // need a secretRef / password.
        reattaching: true,
        reattachId: s.id,
      },
    ]);
    setActive(key);
    setSelectedId(null);
  };

  useEffect(() => {
    void (async () => {
      try {
        const [backend, dir, v] = await Promise.all([
          secretsBackend(),
          logDir(),
          vaultStatus(),
        ]);
        setStatus(
          backend === "keychain"
            ? "secrets: OS keychain"
            : "secrets: encrypted vault (no OS keychain)",
        );
        setLogs(dir);
        setVault(v);
      } catch (err) {
        setStatus(String(err));
      }
      await refreshProfiles();
    })();
  }, [refreshProfiles]);

  const openTab = (
    title: string,
    spec: TransportSpec,
    secretRef?: string,
    password?: string,
    profileId?: string,
    jumpSecretRef?: string,
    jumpPassword?: string,
  ) => {
    const key = nextKey++;
    setTabs((t) => [
      ...t,
      {
        key,
        title,
        spec,
        secretRef,
        password,
        jumpSecretRef,
        jumpPassword,
        profileId,
        sessionId: null,
        exited: false,
        gen: 0,
      },
    ]);
    setActive(key);
    // Switch away from the profile details to the session that was just
    // opened; staying put would look like the Connect button did nothing.
    setSelectedId(null);
  };

  const addLocalTab = () => openTab("shell", localSpec());

  const closeTab = (key: number) => {
    setTabs((t) => {
      const next = t.filter((x) => x.key !== key);
      if (next.length && key === active) setActive(next[next.length - 1].key);
      return next;
    });
  };

  /** Broadcast keystroke data to all active live terminal sessions */
  const handleBroadcastInput = (originTabKey: number, data: string) => {
    if (!broadcast) {
      // Direct send to this pane only
      const tab = tabs.find((t) => t.key === originTabKey);
      if (tab?.sessionId) {
        void writeSession(tab.sessionId, data);
      }
      return;
    }
    // Fan out to all active sessions across tabs
    for (const tab of tabs) {
      if (tab.sessionId && !tab.exited && tab.spec.kind !== "rdp") {
        void writeSession(tab.sessionId, data);
      }
    }
  };

  /** Execute a command string in the currently active or focused terminal tab */
  const runCommandInActiveTerminal = (command: string) => {
    const targetKey = focusedPaneKey ?? active;
    let targetTab = tabs.find((t) => t.key === targetKey);
    if (!targetTab || targetTab.exited || targetTab.spec.kind === "rdp") {
      targetTab = tabs.find((t) => !t.exited && t.spec.kind !== "rdp");
    }
    if (targetTab && targetTab.sessionId) {
      // Append newline if not present
      const cmdToSend = command.endsWith("\n") || command.endsWith("\r") ? command : command + "\n";
      void writeSession(targetTab.sessionId, cmdToSend);
    } else {
      // No active terminal tab -> spawn local shell tab and schedule execution
      const key = nextKey++;
      setTabs((t) => [
        ...t,
        {
          key,
          title: "shell",
          spec: localSpec(),
          sessionId: null,
          exited: false,
          gen: 0,
        },
      ]);
      setActive(key);
      setSelectedId(null);
      // Wait briefly for pty session to initialize, then write
      setTimeout(() => {
        setTabs((currentTabs) => {
          const newTab = currentTabs.find((x) => x.key === key);
          if (newTab && newTab.sessionId) {
            const cmdToSend = command.endsWith("\n") || command.endsWith("\r") ? command : command + "\n";
            void writeSession(newTab.sessionId, cmdToSend);
          }
          return currentTabs;
        });
      }, 350);
    }
  };

  /**
   * Reconnect a dead tab in place.
   *
   * Bumping `gen` changes the pane's React key, so the old pane unmounts (its
   * teardown closes the stale session) and a fresh one mounts and dials the
   * same target again -- reusing the whole connect path rather than
   * duplicating it. The credential travels on the tab, so a saved keychain
   * ref or a one-shot password both survive without re-prompting.
   */
  const reconnectTab = (key: number) => {
    setTabs((t) =>
      t.map((x) =>
        x.key === key
          ? { ...x, gen: x.gen + 1, exited: false, sessionId: null }
          : x,
      ),
    );
    setActive(key);
    setSelectedId(null);
  };

  /** Disconnect: closing the tab unmounts the pane, which closes the session. */
  const disconnectProfile = (p: Profile) => {
    const tab = tabs.find((t) => t.profileId === p.id && !t.exited);
    if (!tab) return;
    closeTab(tab.key);
    setStatus(`disconnected ${p.name}`);
  };

  const handleConnect = async (c: NewConnection) => {
    setDialog(false);
    setEditing(null);
    setBusy(true);
    try {
      let secretRef: string | undefined;
      if (c.password) {
        // The password goes to the keychain; only its key travels with the tab.
        secretRef = secretKeyFor(c.spec);
        await setSecret(secretRef, c.password);
      }

      if (c.editId) {
        const before = profiles.find((p) => p.id === c.editId);
        await updateProfile(c.editId, c.name, null, c.spec);

        // The secret key is derived from user@host:port, so editing any of
        // those strands the old entry. Move the saved password across, unless
        // a new one was just typed and stored under the new key already.
        const oldRef = before ? secretKeyFor(before.spec) : null;
        const newRef = secretKeyFor(c.spec);
        if (oldRef && oldRef !== newRef) {
          try {
            if (c.password) await deleteSecret(oldRef);
            else await renameSecret(oldRef, newRef);
          } catch {
            // Nothing stored under the old key; nothing to move.
          }
        }

        await refreshProfiles();
        setStatus(`saved ${c.name}`);
        return;
      }

      let profileId: string | undefined;
      if (c.save) {
        // Keep the new id so the tab is linked to its profile straight away;
        // without it the sidebar would not show the host as connected.
        profileId = await saveProfile(c.name, null, c.spec);
        await refreshProfiles();
      }
      let jumpSecretRef: string | undefined;
      if (c.spec.kind === "ssh" && c.spec.jump_host) {
        if (c.spec.jump_host.kind === "ssh" && c.spec.jump_host.auth.method === "password") {
          jumpSecretRef = secretKeyFor(c.spec.jump_host);
        }
      }
      openTab(c.name, c.spec, secretRef, undefined, profileId, jumpSecretRef);
      setStatus(`opening ${c.name}`);
    } catch (err) {
      setStatus(String(err));
    } finally {
      setBusy(false);
    }
  };

  const openProfile = async (p: Profile) => {
    // Reconnecting an already-live profile would silently stack a second
    // session behind the first; focus the existing tab instead.
    const live = tabs.find((t) => t.profileId === p.id && !t.exited);
    if (live) {
      setActive(live.key);
      setSelectedId(null);
      return;
    }

    if (connectBlockedReason(p.spec)) {
      setStatus(connectBlockedReason(p.spec)!);
      return;
    }

    // RDP is always password-based: CredSSP needs the real secret, so there
    // is no agent or key path to fall back on.
    const needsPassword =
      (p.spec.kind === "ssh" && p.spec.auth.method === "password") ||
      p.spec.kind === "rdp";

    let jumpSecretRef: string | undefined;
    if (p.spec.kind === "ssh" && p.spec.jump_host) {
      if (p.spec.jump_host.kind === "ssh" && p.spec.jump_host.auth.method === "password") {
        jumpSecretRef = secretKeyFor(p.spec.jump_host);
      }
    }

    if (!needsPassword) {
      openTab(p.name, p.spec, undefined, undefined, p.id, jumpSecretRef);
      return;
    }

    const ref = secretKeyFor(p.spec);
    // Ask up front when the credential is missing. Connecting anyway would
    // fail deep in the SSH handshake with an error the user cannot act on.
    try {
      if (await hasSecret(ref)) {
        openTab(p.name, p.spec, ref, undefined, p.id, jumpSecretRef);
      } else {
        setPwPrompt(p);
      }
    } catch (err) {
      setStatus(String(err));
    }
  };

  const submitPassword = async (password: string, remember: boolean) => {
    const p = pwPrompt;
    if (!p) return;
    setPwPrompt(null);
    try {
      let jumpSecretRef: string | undefined;
      if (p.spec.kind === "ssh" && p.spec.jump_host) {
        if (p.spec.jump_host.kind === "ssh" && p.spec.jump_host.auth.method === "password") {
          jumpSecretRef = secretKeyFor(p.spec.jump_host);
        }
      }

      if (remember) {
        const ref = secretKeyFor(p.spec);
        await setSecret(ref, password);
        openTab(p.name, p.spec, ref, undefined, p.id, jumpSecretRef);
      } else {
        // Hand the password straight to the session so it never reaches disk.
        openTab(p.name, p.spec, undefined, password, p.id, jumpSecretRef);
      }
    } catch (err) {
      setStatus(String(err));
    }
  };

  const removeProfile = async (p: Profile) => {
    setConfirmDelete(null);
    try {
      await deleteProfile(p.id);
      // Drop the credential too, or it lingers in the keychain/vault with
      // nothing in the UI referencing it.
      const ref = secretKeyFor(p.spec);
      if (ref) {
        try {
          await deleteSecret(ref);
        } catch {
          // A profile saved without a password has nothing to delete; that is
          // not a failure worth surfacing.
        }
      }
      if (selectedId === p.id) setSelectedId(null);
      await refreshProfiles();
      setStatus(`deleted ${p.name}`);
    } catch (err) {
      setStatus(String(err));
    }
  };



  // Render nothing until the vault state is known. Falling through would mount
  // the terminal and spawn a session for a split second before the gate
  // appeared, which defeats the point of gating.
  if (!vault) {
    return <div className="unlock-backdrop" />;
  }

  // Gate the whole app: without the passphrase no saved credential can be
  // read, so letting the user reach the connect dialog would only produce
  // confusing failures further down.
  if (vault.locked) {
    return (
      <UnlockGate
        status={vault}
        onUnlocked={() => {
          setVault({ ...vault, locked: false, initialized: true });
          void refreshProfiles();
        }}
      />
    );
  }

  return (
    <div className="app">
      <AppHeader
        query={query}
        onQuery={setQuery}
        onNew={() => setDialog(true)}
        busy={busy}
        sidebarOpen={sidebarOpen}
        onToggleSidebar={() => setSidebarOpen((v) => !v)}
        filesOpen={filesOpen}
        onToggleFiles={() => setFilesOpen((v) => !v)}
        onOpenHistory={() => setHistoryOpen(true)}
        onOpenTunnels={() => setTunnelsOpen(true)}
        onOpenSnippets={() => setSnippetsOpen(true)}
        onOpenKnownHosts={() => setKnownHostsOpen(true)}
        onOpenTriggers={() => setTriggersOpen(true)}
        onOpenBackup={() => setBackupOpen(true)}
        onOpenMonitor={() => setMonitorOpen(true)}
        onOpenBatchRunner={() => setBatchOpen(true)}
        onOpenThemes={() => setThemesOpen(true)}
        onOpenEditor={() => setEditorOpen(true)}
        splitLayout={splitLayout}
        onSplitLayout={setSplitLayout}
        broadcast={broadcast}
        onToggleBroadcast={() => setBroadcast((b) => !b)}
      />
      {broadcast && (
        <div className="broadcast-banner">
          <span>⚡ <strong>Multi-Exec (Broadcast) Active</strong>: Typed keystrokes are sent to all live terminal sessions simultaneously.</span>
          <div className="broadcast-banner-actions">
            <button className="broadcast-banner-btn" onClick={() => setBroadcast(false)}>
              Turn Off
            </button>
          </div>
        </div>
      )}
      <div className={`body ${sidebarOpen ? "" : "collapsed"} ${filesOpen ? "files-open" : ""}`}>
        <Sidebar
          profiles={profiles}
          selectedId={selectedId}
          connectedIds={connectedIds}
          query={query}
          open={sidebarOpen}
          onSelect={(p) => setSelectedId(p.id)}
          onConnect={(p) => void openProfile(p)}
          onDisconnect={disconnectProfile}
          onDelete={(p) => setConfirmDelete(p)}
          onNew={() => setDialog(true)}
          onOpenTunnels={() => setTunnelsOpen(true)}
          onOpenSnippets={() => setSnippetsOpen(true)}
          onOpenKnownHosts={() => setKnownHostsOpen(true)}
          onOpenTriggers={() => setTriggersOpen(true)}
          onOpenBackup={() => setBackupOpen(true)}
          onOpenMonitor={() => setMonitorOpen(true)}
          onOpenBatchRunner={() => setBatchOpen(true)}
          onOpenThemes={() => setThemesOpen(true)}
          onOpenEditor={() => setEditorOpen(true)}
          busy={busy}
        />

        <div className="main">
          <div className="tabbar">
            {tabs.map((t) => (
              <div
                key={t.key}
                className={`tab ${t.key === active ? "active" : ""}`}
                onClick={() => setActive(t.key)}
              >
                <span className={`dot ${t.exited ? "dead" : "live"}`} />
                <span className="tab-title">{t.title}</span>
                <button
                  className="tab-close"
                  title="Close tab"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(t.key);
                  }}
                >
                  ×
                </button>
              </div>
            ))}
            <button
              className="tab-new"
              onClick={addLocalTab}
              title="New local shell"
            >
              +
            </button>
            <div className="spacer" />
            <button
              className="ghost"
              onClick={() => setHistoryOpen(true)}
              title="Open Session Recordings & Command Logs"
            >
              logs & replay
            </button>
          </div>

          <div className="panes">
            {splitLayout === "1x1" ? (
              <>
                {tabs.map((t) => (
                  <div
                    key={t.key}
                    className="pane"
                    style={{ display: t.key === active ? "block" : "none" }}
                  >
                    {t.spec.kind === "rdp" ? (
                      <RdpPane
                        key={t.gen}
                        spec={t.spec}
                        secretRef={t.secretRef}
                        password={t.password}
                        active={t.key === active}
                        onReady={(id) =>
                          setTabs((xs) =>
                            xs.map((x) =>
                              x.key === t.key ? { ...x, sessionId: id } : x,
                            ),
                          )
                        }
                        onExit={() =>
                          setTabs((xs) =>
                            xs.map((x) =>
                              x.key === t.key ? { ...x, exited: true } : x,
                            ),
                          )
                        }
                        onReconnect={() => reconnectTab(t.key)}
                        onClose={() => closeTab(t.key)}
                      />
                    ) : (
                      <TerminalPane
                        key={t.gen}
                        spec={t.spec}
                        secretRef={t.secretRef}
                        password={t.password}
                        jumpSecretRef={t.jumpSecretRef}
                        jumpPassword={t.jumpPassword}
                        reattachId={t.reattachId}
                        active={t.key === active}
                        onInputData={(data) => handleBroadcastInput(t.key, data)}
                        onReady={(id) =>
                          setTabs((xs) =>
                            xs.map((x) =>
                              x.key === t.key ? { ...x, sessionId: id } : x,
                            ),
                          )
                        }
                        onExit={() =>
                          setTabs((xs) =>
                            xs.map((x) =>
                              x.key === t.key ? { ...x, exited: true } : x,
                            ),
                          )
                        }
                        onReconnect={() => reconnectTab(t.key)}
                        onClose={() => closeTab(t.key)}
                      />
                    )}
                  </div>
                ))}
              </>
            ) : (
              <div className={`panes-grid layout-${splitLayout}`}>
                {(() => {
                  const maxPanes = splitLayout === "2x2" ? 4 : 2;
                  const activeIdx = Math.max(0, tabs.findIndex((t) => t.key === active));
                  // Order tabs starting from current active or front
                  const reordered = [
                    tabs[activeIdx],
                    ...tabs.slice(0, activeIdx),
                    ...tabs.slice(activeIdx + 1),
                  ].filter(Boolean);

                  const displayTabs = reordered.slice(0, maxPanes);

                  return (
                    <>
                      {displayTabs.map((t) => (
                        <div
                          key={t.key}
                          className={`pane-wrapper ${(focusedPaneKey ?? active) === t.key ? "focused" : ""}`}
                          onClick={() => {
                            setFocusedPaneKey(t.key);
                            setActive(t.key);
                          }}
                        >
                          <div className="pane-header">
                            <span className="pane-title">
                              <span className={`dot ${t.exited ? "dead" : "live"}`} />
                              {t.title}
                            </span>
                            <button
                              className="pane-close"
                              title="Close pane"
                              onClick={(e) => {
                                e.stopPropagation();
                                closeTab(t.key);
                              }}
                            >
                              ×
                            </button>
                          </div>
                          <div className="pane-body">
                            {t.spec.kind === "rdp" ? (
                              <RdpPane
                                key={t.gen}
                                spec={t.spec}
                                secretRef={t.secretRef}
                                password={t.password}
                                active={(focusedPaneKey ?? active) === t.key}
                                onReady={(id) =>
                                  setTabs((xs) =>
                                    xs.map((x) =>
                                      x.key === t.key ? { ...x, sessionId: id } : x,
                                    ),
                                  )
                                }
                                onExit={() =>
                                  setTabs((xs) =>
                                    xs.map((x) =>
                                      x.key === t.key ? { ...x, exited: true } : x,
                                    ),
                                  )
                                }
                                onReconnect={() => reconnectTab(t.key)}
                                onClose={() => closeTab(t.key)}
                              />
                            ) : (
                              <TerminalPane
                                key={t.gen}
                                spec={t.spec}
                                secretRef={t.secretRef}
                                password={t.password}
                                jumpSecretRef={t.jumpSecretRef}
                                jumpPassword={t.jumpPassword}
                                reattachId={t.reattachId}
                                active={(focusedPaneKey ?? active) === t.key}
                                onInputData={(data) => handleBroadcastInput(t.key, data)}
                                onReady={(id) =>
                                  setTabs((xs) =>
                                    xs.map((x) =>
                                      x.key === t.key ? { ...x, sessionId: id } : x,
                                    ),
                                  )
                                }
                                onExit={() =>
                                  setTabs((xs) =>
                                    xs.map((x) =>
                                      x.key === t.key ? { ...x, exited: true } : x,
                                    ),
                                  )
                                }
                                onReconnect={() => reconnectTab(t.key)}
                                onClose={() => closeTab(t.key)}
                              />
                            )}
                          </div>
                        </div>
                      ))}
                      {Array.from({ length: Math.max(0, maxPanes - displayTabs.length) }).map((_, idx) => (
                        <div key={`empty-${idx}`} className="pane-wrapper empty-split">
                          <div className="empty">
                            <span>Empty pane slot</span>
                            <button className="tab-new" onClick={addLocalTab} title="Open a new shell">
                              + Open shell
                            </button>
                          </div>
                        </div>
                      ))}
                    </>
                  );
                })()}
              </div>
            )}
            {tabs.length === 0 && (
              <div className="empty">
                {liveSessionsChecked && liveSessions.length > 0 ? (
                  <div className="reattach-prompt">
                    {(() => {
                      const alive = liveSessions.filter((s) => s.alive);
                      const dead = liveSessions.filter((s) => !s.alive);
                      return (
                        <>
                          {alive.length > 0 && (
                            <>
                              <h3>
                                {alive.length} live session{alive.length === 1 ? "" : "s"} still on the daemon
                              </h3>
                              <p>
                                Reattach to pick up where you left off. The daemon has kept the PTY alive and the last ~1 MB of scrollback is ready to replay.
                              </p>
                              <ul>
                                {alive.map((s) => (
                                  <li key={s.id}>
                                    <span className="reattach-target">{describeTarget(s.spec)}</span>
                                    <button onClick={() => reattachSession(s)}>Reattach</button>
                                  </li>
                                ))}
                              </ul>
                            </>
                          )}
                          {dead.length > 0 && (
                            <>
                              {alive.length > 0 && <div className="reattach-divider">also</div>}
                              <h3>{dead.length} previously open session{dead.length === 1 ? "" : "s"}</h3>
                              <p>The daemon restarted since these were last open. Credentials aren't remembered, so reattach isn't possible -- but you can re-open a fresh session with the same target.</p>
                              <ul className="reattach-prev">
                                {dead.map((s) => (
                                  <li key={s.id}>
                                    <span className="reattach-target">{describeTarget(s.spec)}</span>
                                    <span className="reattach-when">
                                      {new Date(s.openedAtMs).toLocaleString()}
                                    </span>
                                    <button onClick={() => setPrefillSpec(s.spec)}>Reopen</button>
                                  </li>
                                ))}
                              </ul>
                            </>
                          )}
                          <div className="reattach-divider">or</div>
                          <button onClick={addLocalTab}>Open a new shell</button>
                        </>
                      );
                    })()}
                  </div>
                ) : (
                  <>
                    No sessions.{" "}
                    <button onClick={addLocalTab}>Open a shell</button>
                  </>
                )}
              </div>
            )}
          </div>
        </div>

        <FileDrawer
          open={filesOpen}
          sessionId={activeTab?.sessionId ?? null}
          remoteCapable={
            activeTab?.spec.kind === "ssh" && !activeTab.exited
          }
          hostLabel={
            activeTab?.spec.kind === "ssh"
              ? `${activeTab.spec.user}@${activeTab.spec.host}`
              : null
          }
          onOpenFile={(entry) => {
            setEditorInitialFile({
              path: entry.path,
              name: entry.name,
              sessionId: activeTab?.sessionId ?? null,
              hostLabel:
                activeTab?.spec.kind === "ssh"
                  ? `${activeTab.spec.user}@${activeTab.spec.host}`
                  : null,
              isLocal: activeTab?.spec.kind === "local",
            });
            setEditorOpen(true);
          }}
          onClose={() => setFilesOpen(false)}
        />
      </div>

      <div className="statusbar">
        <span>{status}</span>
        <span className="spacer" />
        <span className="dim">{logs}</span>
      </div>

      {(dialog || editing || prefillSpec) && (
        <ConnectDialog
          edit={editing ?? undefined}
          prefillSpec={prefillSpec ?? undefined}
          onCancel={() => {
            setDialog(false);
            setEditing(null);
            setPrefillSpec(null);
          }}
          onConnect={(c) => {
            setPrefillSpec(null);
            void handleConnect(c);
          }}
        />
      )}

      {pwPrompt && (
        <PasswordPrompt
          title={pwPrompt.name}
          onCancel={() => setPwPrompt(null)}
          onSubmit={(pw, remember) => void submitPassword(pw, remember)}
        />
      )}
      {/* One modal at a time. Edit and delete-confirm both open *from* the
          profile, so the profile hides while they are up and comes back when
          they close -- two centred modals otherwise overlap. */}
      {selected && !editing && !confirmDelete && (
        <ProfileView
          profile={selected}
          connected={connectedIds.has(selected.id)}
          busy={busy}
          onConnect={() => void openProfile(selected)}
          onDisconnect={() => disconnectProfile(selected)}
          onEdit={() => setEditing(selected)}
          onDelete={() => setConfirmDelete(selected)}
          onClose={() => setSelectedId(null)}
        />
      )}

      {confirmDelete && (
        <ConfirmDialog
          title="Delete connection?"
          message={`"${confirmDelete.name}" (${describeTarget(confirmDelete.spec)}) will be removed.`}
          detail="Any password saved for this host is deleted too. This cannot be undone."
          confirmLabel="Delete"
          onConfirm={() => void removeProfile(confirmDelete)}
          onCancel={() => setConfirmDelete(null)}
        />
      )}

      {historyOpen && (
        <SessionHistoryModal onClose={() => setHistoryOpen(false)} />
      )}

      {tunnelsOpen && (
        <TunnelManagerModal onClose={() => setTunnelsOpen(false)} />
      )}

      {snippetsOpen && (
        <SnippetManagerModal
          onClose={() => setSnippetsOpen(false)}
          onRunSnippet={(cmd) => runCommandInActiveTerminal(cmd)}
        />
      )}

      {knownHostsOpen && (
        <KnownHostsModal open={knownHostsOpen} onClose={() => setKnownHostsOpen(false)} />
      )}

      {triggersOpen && (
        <TriggerManagerModal open={triggersOpen} onClose={() => setTriggersOpen(false)} />
      )}

      {backupOpen && (
        <BackupModal
          open={backupOpen}
          onClose={() => setBackupOpen(false)}
          onRestoreComplete={() => {
            void listProfiles().then(setProfiles);
          }}
        />
      )}

      {themesOpen && (
        <ThemeCustomizerModal
          open={themesOpen}
          onClose={() => setThemesOpen(false)}
        />
      )}

      {monitorOpen && (
        <ResourceMonitorModal
          open={monitorOpen}
          spec={activeTab?.spec}
          hostLabel={activeTab?.title}
          secretRef={activeTab?.secretRef}
          password={activeTab?.password}
          jumpSecretRef={activeTab?.jumpSecretRef}
          jumpPassword={activeTab?.jumpPassword}
          onClose={() => setMonitorOpen(false)}
        />
      )}

      {batchOpen && (
        <BatchRunnerModal
          open={batchOpen}
          onClose={() => setBatchOpen(false)}
        />
      )}

      <RemoteEditorModal
        open={editorOpen}
        initialFile={editorInitialFile}
        sessionId={activeTab?.sessionId ?? null}
        hostLabel={
          activeTab?.spec.kind === "ssh"
            ? `${activeTab.spec.user}@${activeTab.spec.host}`
            : null
        }
        isLocal={activeTab?.spec.kind === "local" || !activeTab}
        spec={activeTab?.spec ?? { kind: "local", shell: null, cwd: null }}
        secretRef={activeTab?.secretRef}
        password={activeTab?.password}
        jumpSecretRef={activeTab?.jumpSecretRef}
        jumpPassword={activeTab?.jumpPassword}
        onClose={() => {
          setEditorOpen(false);
          setEditorInitialFile(null);
        }}
      />

      {paletteOpen && (
        <CommandPalette
          onClose={() => setPaletteOpen(false)}
          onConnectProfile={(p) => void openProfile(p)}
          onRunSnippet={(cmd) => runCommandInActiveTerminal(cmd)}
          actions={[
            {
              id: "open-editor",
              title: "Open Remote Mini-IDE Code Editor",
              subtitle: "VS Code style editor with syntax highlighting, formatting, and SFTP save",
              shortcut: "⌘E",
              icon: "📝",
              perform: () => setEditorOpen(true),
            },
            {
              id: "new-session",
              title: "New Connection / Session",
              subtitle: "Open connection dialog for SSH, RDP, or local shell",
              shortcut: "⌘N",
              icon: "➕",
              perform: () => setDialog(true),
            },
            {
              id: "new-local-shell",
              title: "New Local Shell Tab",
              subtitle: "Open a fresh local terminal tab",
              shortcut: "⌘T",
              icon: "💻",
              perform: () => addLocalTab(),
            },
            {
              id: "open-snippets",
              title: "Open Snippets & Command Library",
              subtitle: "Manage and run parameterized scripts and commands",
              shortcut: "⇧⌘P",
              icon: "📝",
              perform: () => setSnippetsOpen(true),
            },
            {
              id: "open-known-hosts",
              title: "SSH Known Hosts & Host Keys",
              subtitle: "Inspect trusted public keys, fingerprints, and revoke untrusted hosts",
              icon: "🛡️",
              perform: () => setKnownHostsOpen(true),
            },
            {
              id: "open-tunnels",
              title: "Open SSH Port Tunnels Manager",
              subtitle: "Configure Local (-L), Remote (-R), and Dynamic SOCKS5 (-D) tunnels",
              icon: "⚡",
              perform: () => setTunnelsOpen(true),
            },
            {
              id: "open-triggers",
              title: "Terminal Output Triggers & Desktop Alerts",
              subtitle: "Configure regex watchers, audio chimes, and notifications for terminal output",
              icon: "🔔",
              perform: () => setTriggersOpen(true),
            },
            {
              id: "open-monitor",
              title: "Remote System Resource Monitor",
              subtitle: "Live CPU, RAM, Disk utilization gauges and Process Manager",
              icon: "📊",
              perform: () => setMonitorOpen(true),
            },
            {
              id: "open-batch",
              title: "Multi-Host Batch Command Execution",
              subtitle: "Run scripts across multiple SSH hosts simultaneously with aggregated logs",
              icon: "🚀",
              perform: () => setBatchOpen(true),
            },
            {
              id: "open-themes",
              title: "Terminal Themes & Appearance Customizer",
              subtitle: "Catppuccin, Dracula, Nord, Tokyo Night, Solarized, custom fonts, cursor style",
              icon: "🎨",
              perform: () => setThemesOpen(true),
            },
            {
              id: "open-backup",
              title: "Backup & Restore (Import / Export JSON)",
              subtitle: "Export or import connection profiles, SSH tunnels, snippets, and triggers",
              icon: "📦",
              perform: () => setBackupOpen(true),
            },
            {
              id: "open-recordings",
              title: "Session Recordings & Command Logs",
              subtitle: "Search OSC 133 command history and replay asciinema recordings",
              icon: "📼",
              perform: () => setHistoryOpen(true),
            },
            {
              id: "toggle-broadcast",
              title: broadcast ? "Disable Multi-Exec Broadcast Input" : "Enable Multi-Exec Broadcast Input",
              subtitle: "Broadcast typed keystrokes across all open terminal tabs simultaneously",
              icon: "⚡",
              perform: () => setBroadcast((b) => !b),
            },
            {
              id: "toggle-sidebar",
              title: sidebarOpen ? "Hide Connections Sidebar" : "Show Connections Sidebar",
              subtitle: "Toggle the left connections drawer",
              shortcut: "⌘B",
              icon: "📁",
              perform: () => setSidebarOpen((v) => !v),
            },
            {
              id: "toggle-files",
              title: filesOpen ? "Hide Remote SFTP File Drawer" : "Show Remote SFTP File Drawer",
              subtitle: "Toggle the right remote file browser drawer",
              shortcut: "⌘J",
              icon: "🗄️",
              perform: () => setFilesOpen((v) => !v),
            },
            {
              id: "split-1x1",
              title: "Layout: Single Pane (1x1)",
              subtitle: "Switch to single active terminal view",
              icon: "🔲",
              perform: () => setSplitLayout("1x1"),
            },
            {
              id: "split-1x2",
              title: "Layout: Split Vertical (1x2)",
              subtitle: "Show 2 side-by-side terminal panes",
              icon: "🔳",
              perform: () => setSplitLayout("1x2"),
            },
            {
              id: "split-2x1",
              title: "Layout: Split Horizontal (2x1)",
              subtitle: "Show 2 stacked horizontal terminal panes",
              icon: "🟰",
              perform: () => setSplitLayout("2x1"),
            },
            {
              id: "split-2x2",
              title: "Layout: Quad Grid (2x2)",
              subtitle: "Show 4 terminal panes in a 2x2 grid",
              icon: "🪟",
              perform: () => setSplitLayout("2x2"),
            },
          ]}
        />
      )}
    </div>
  );
}
