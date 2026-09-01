//! Daemon-side session bookkeeping on top of `core::session::SessionManager`.
//!
//! The HTTP layer talks to this; this talks to the core. The split is
//! so we can add scrollback buffering and SQLite-persisted metadata
//! in later sessions without touching the wire format.

use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use crate::persist::SessionStore;
use crate::ringbuffer::OutputRingBuffer;
use terminator_core::session::{Credentials, SessionManager};
use terminator_core::transport::TransportSpec;

/// What an HTTP/SSE client receives per output chunk.
///
/// Wire shape is deliberately the same as the Tauri `SessionEvent` that
/// `src-tauri/src/lib.rs` already publishes to the webview's Tauri
/// Channel, so a future Tauri-side rewrite can just forward the SSE
/// event through the same channel without translation.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutputEvent {
    /// Raw PTY/SSH bytes, base64. Tauri IPC serialises `Vec<u8>` as a
    /// JSON number array, which is far larger and slower to parse at
    /// terminal throughput.
    Output { data: String },
    /// Process exited. Always the last event on a channel.
    Exit,
}

/// Per-session metadata exposed via the list/detail endpoints.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub spec: TransportSpec,
    pub alive: bool,
    /// Wall-clock time this session was opened, milliseconds since
    /// the Unix epoch. Used by the UI to sort "most recent first".
    pub opened_at_ms: i64,
}

pub struct DaemonSessionManager {
    core: Arc<SessionManager>,
    /// Per-session broadcast channel that the SSE handler subscribes
    /// to. Bounded so a stuck HTTP client cannot make the session
    /// queue up indefinitely. The capacity is small: with 1 KB
    /// output chunks it buffers about 1 MB of recent scrollback.
    channels: Mutex<std::collections::HashMap<Uuid, broadcast::Sender<OutputEvent>>>,
    /// Per-session scrollback buffer. When a new SSE subscriber
    /// connects, the buffer is replayed first so the terminal
    /// isn't blank after a UI restart. Lives alongside the
    /// broadcast channel: the broadcast carries live events,
    /// the ring buffer carries the last ~1 MB of history.
    buffers: Mutex<std::collections::HashMap<Uuid, Arc<OutputRingBuffer>>>,
    /// Wall-clock time each session was opened, for `SessionInfo`.
    opened_at: Mutex<std::collections::HashMap<Uuid, i64>>,
    /// SQLite-backed metadata store. Cross-restart memory:
    /// even if the daemon dies, the reattach prompt knows
    /// which sessions were recently alive. Optional so the
    /// manager can be constructed without a path (e.g. in
    /// tests); the production `main.rs` always passes one.
    store: Option<Arc<SessionStore>>,
}

impl DaemonSessionManager {
    pub fn new(core: Arc<SessionManager>, store: Option<Arc<SessionStore>>) -> Self {
        Self {
            core,
            channels: Mutex::new(std::collections::HashMap::new()),
            buffers: Mutex::new(std::collections::HashMap::new()),
            opened_at: Mutex::new(std::collections::HashMap::new()),
            store,
        }
    }

    pub fn core(&self) -> &Arc<SessionManager> {
        &self.core
    }

    /// Open a new session. Returns `(id, fresh_receiver)`. The
    /// receiver yields `OutputEvent`s from the moment the function
    /// returns -- useful so the SSE handler can subscribe before the
    /// HTTP response is even framed, eliminating the race where the
    /// shell's first prompt would otherwise arrive between the
    /// open call and the SSE subscribe.
    pub async fn open(
        &self,
        spec: TransportSpec,
        cols: u16,
        rows: u16,
        creds: Credentials,
    ) -> Result<(Uuid, broadcast::Receiver<OutputEvent>)> {
        let (tx, rx) = broadcast::channel::<OutputEvent>(1024);

        // Bridge the per-session broadcast to the two core sinks.
        // `on_output` and `on_exit` run on whatever task the core
        // uses to drain the transport, so we have to be Send+Sync.
        let tx_out = tx.clone();
        let ringbuf = Arc::new(OutputRingBuffer::new());
        let ringbuf_out = ringbuf.clone();
        let on_output: Arc<dyn Fn(Bytes) + Send + Sync> = Arc::new(move |data: Bytes| {
            // Capture for reattach first: a slow SSE client
            // shouldn't cause us to lose scrollback, so we
            // record into the ring buffer before fanning out.
            ringbuf_out.push(data.clone());
            let ev = OutputEvent::Output {
                data: base64_encode(&data),
            };
            // Lagged receivers are a slow HTTP client, not a bug.
            // Dropping the event matches the old Tauri-channel
            // semantics: a subscriber that can't keep up loses data.
            let _ = tx_out.send(ev);
        });
        // Clone `tx` one last time for the on_exit closure so the
        // broadcast sender can stay in `self.channels` for later
        // subscribe() calls. Broadcast senders are cheap to clone
        // (a refcount bump).
        let on_exit: Arc<dyn Fn() + Send + Sync> = {
            let tx = tx.clone();
            Arc::new(move || {
                let _ = tx.send(OutputEvent::Exit);
            })
        };

        let id = self
            .core
            .open_with(spec.clone(), cols, rows, creds, on_output, on_exit)
            .await?;

        self.channels.lock().await.insert(id, tx);
        self.buffers.lock().await.insert(id, ringbuf);
        let opened_at = now_ms();
        self.opened_at.lock().await.insert(id, opened_at);

        // Persist AFTER the in-memory bookkeeping succeeds. A
        // crash between core.open_with and here is acceptable:
        // we'd just lose one session from the reattach prompt.
        if let Some(store) = &self.store {
            if let Err(e) = store.record_open(id, &spec, opened_at).await {
                tracing::warn!("failed to persist session {id}: {e:#}");
            }
        }

        Ok((id, rx))
    }

    pub async fn write(&self, id: Uuid, data: Bytes) -> Result<()> {
        // `core::SessionManager::write` is sync; wrap in spawn_blocking
        // so we don't hold the runtime on a slow stdin write.
        let core = self.core.clone();
        tokio::task::spawn_blocking(move || core.write(id, data))
            .await
            .map_err(|e| anyhow::anyhow!("write task panicked: {e}"))?
    }

    pub async fn resize(&self, id: Uuid, cols: u16, rows: u16) -> Result<()> {
        let core = self.core.clone();
        tokio::task::spawn_blocking(move || core.resize(id, cols, rows))
            .await
            .map_err(|e| anyhow::anyhow!("resize task panicked: {e}"))?
    }

    pub async fn close(&self, id: Uuid) -> Result<()> {
        let core = self.core.clone();
        // `core.close` always returns Ok -- any failure mode is
        // "the transport shutdown task panicked", which is a
        // join error rather than the inner Result. We still want
        // to fall through to the persistent-store close below
        // even if the join fails, so discard the inner Ok
        // explicitly.
        let _ = tokio::task::spawn_blocking(move || core.close(id))
            .await
            .map_err(|e| anyhow::anyhow!("close task panicked: {e}"))?;
        // Mark closed in the persistent store. Idempotent and
        // best-effort: the in-memory close has already happened
        // by the time we get here, and a stale "still alive" row
        // is just a cosmetic bug, not a correctness one.
        if let Some(store) = &self.store {
            if let Err(e) = store.record_close(id, now_ms()).await {
                tracing::warn!("failed to mark session {id} closed: {e:#}");
            }
        }
        Ok(())
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        // Merge two sources:
        //   1. The persistent store (every session we've ever
        //      opened, whether currently alive or not). This is
        //      the source of truth across daemon restarts.
        //   2. The core's live map (the in-memory list of
        //      sessions that still have a live transport).
        // The `alive` flag is `id ∈ core` for each persisted
        // row. Sessions that exist only in core (e.g. opened
        // before the store was wired up) fall through into the
        // second pass below.
        let mut out: Vec<SessionInfo> = Vec::new();
        let mut live_ids: std::collections::HashSet<Uuid> =
            std::collections::HashSet::new();

        if let Some(store) = &self.store {
            match store.list_all().await {
                Ok(persisted) => {
                    for p in persisted {
                        let id = match Uuid::parse_str(&p.id) {
                            Ok(u) => u,
                            Err(_) => continue, // bad data, skip
                        };
                        let alive = self
                            .core
                            .spec(id)
                            .is_some();
                        if alive {
                            live_ids.insert(id);
                        }
                        out.push(SessionInfo {
                            id: p.id,
                            spec: p.spec,
                            alive,
                            opened_at_ms: p.opened_at_ms,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to list persisted sessions: {e:#}");
                }
            }
        }

        // Always merge in anything currently alive in the core,
        // even if the store didn't have it. This is the
        // "opened in this run, before persistence happened"
        // case; rare but possible if a `record_open` failed.
        let core_ids: Vec<Uuid> = tokio::task::spawn_blocking({
            let core = self.core.clone();
            move || core.list_sessions()
        })
        .await
        .unwrap_or_default();
        for id in &core_ids {
            if live_ids.contains(id) {
                continue;
            }
            let spec = self
                .core
                .spec(*id)
                .unwrap_or(TransportSpec::Local {
                    shell: None,
                    cwd: None,
                });
            out.push(SessionInfo {
                id: id.to_string(),
                spec,
                alive: true,
                opened_at_ms: *self.opened_at.lock().await.get(id).unwrap_or(&0),
            });
        }

        out.sort_by(|a, b| b.opened_at_ms.cmp(&a.opened_at_ms));
        out
    }

    pub async fn subscribe(&self, id: Uuid) -> Result<broadcast::Receiver<OutputEvent>> {
        let channels = self.channels.lock().await;
        let tx = channels
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown session {id}"))?;
        Ok(tx.subscribe())
    }

    /// Look up the `RemoteFs` for a live session. The file browser
    /// and exec command on the Tauri side go through here, so
    /// every file operation is performed by the same process
    /// that owns the SSH connection -- no cross-process tricks
    /// for SFTP, no second hop through the Tauri process.
    pub async fn files(&self, id: Uuid) -> Result<Arc<dyn terminator_core::files::RemoteFs>> {
        self.core.files(id).await
    }

    /// One-shot command execution. Same path the Tauri side
    /// used before the daemon existed; we keep it here so
    /// RemoteEditorModal, ResourceMonitorModal, and the
    /// batch-runner continue to work for SSH sessions.
    pub async fn exec_command(
        &self,
        spec: &TransportSpec,
        command: &str,
        creds: terminator_core::session::Credentials,
        cwd: Option<&str>,
    ) -> Result<terminator_core::session::ExecResult> {
        self.core
            .exec_command(spec, command, creds, cwd)
            .await
    }

    /// Snapshot of the buffered output for reattach. Returns an
    /// empty `Vec` for an unknown session id so the HTTP handler
    /// can answer with 200 + `[]` rather than 404.
    pub async fn scrollback(&self, id: Uuid) -> Vec<Bytes> {
        let buffers = self.buffers.lock().await;
        match buffers.get(&id) {
            Some(buf) => buf.snapshot(),
            None => Vec::new(),
        }
    }

    pub async fn is_alive(&self, id: Uuid) -> bool {
        // "Alive" is just "still tracked by the core". The core
        // reaps sessions after their process exits, so a
        // successful `spec` lookup is a reliable signal. We
        // could also use the transport's is_alive() if we wanted
        // to detect the brief race between process exit and
        // reap, but it doesn't matter for the UI.
        self.core.spec(id).is_some()
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
