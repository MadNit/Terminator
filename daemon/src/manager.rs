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

use terminator_core::session::{Credentials, SessionManager};
use terminator_core::transport::TransportSpec;

/// What an HTTP/SSE client receives per output chunk.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OutputEvent {
    /// Raw PTY/SSH bytes. Base64 because Tauri IPC / SSE serialise as
    /// JSON; binary would have to be re-framed.
    Output { data_b64: String },
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
    /// Wall-clock time each session was opened, for `SessionInfo`.
    opened_at: Mutex<std::collections::HashMap<Uuid, i64>>,
    /// Sub-set of sessions the daemon reports as alive. Set true on
    /// open, flipped to false from the exit sink. Wrapped in Arc so
    /// the on_exit callback (which captures the map by value, not by
    /// reference) can still update it from its own task.
    alive: Arc<Mutex<std::collections::HashMap<Uuid, bool>>>,
}

impl DaemonSessionManager {
    pub fn new(core: Arc<SessionManager>) -> Self {
        Self {
            core,
            channels: Mutex::new(std::collections::HashMap::new()),
            opened_at: Mutex::new(std::collections::HashMap::new()),
            alive: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
        let tx_exit = tx.clone();
        let on_output: Arc<dyn Fn(Bytes) + Send + Sync> = Arc::new(move |data: Bytes| {
            let ev = OutputEvent::Output {
                data_b64: base64_encode(&data),
            };
            // Lagged receivers are a slow HTTP client, not a bug.
            // Dropping the event matches the old Tauri-channel
            // semantics: a subscriber that can't keep up loses data.
            let _ = tx_out.send(ev);
        });
        let on_exit: Arc<dyn Fn() + Send + Sync> = {
            let tx = tx.clone();
            let alive_map = self.alive.clone();
            let id_for_exit = id;
            Arc::new(move || {
                let _ = tx.send(OutputEvent::Exit);
                // Flip the alive flag too. We can't hold the mutex
                // across the broadcast send, but spawning a tiny
                // task for the mutex update is fine.
                let alive_map = alive_map.clone();
                tokio::spawn(async move {
                    alive_map.lock().await.insert(id_for_exit, false);
                });
            })
        };

        let id = self
            .core
            .open_with(spec.clone(), cols, rows, creds, on_output, on_exit)
            .await?;

        self.channels.lock().await.insert(id, tx);
        self.opened_at.lock().await.insert(id, now_ms());
        self.alive.lock().await.insert(id, true);

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
        tokio::task::spawn_blocking(move || core.close(id))
            .await
            .map_err(|e| anyhow::anyhow!("close task panicked: {e}"))?
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let core = self.core.clone();
        // core.list_sessions returns ids; the rest of the metadata
        // comes from our side tables. We pull on spawn_blocking because
        // core keeps a Mutex on the session map.
        let ids: Vec<Uuid> = tokio::task::spawn_blocking(move || core.list_sessions())
            .await
            .unwrap_or_default();
        let mut out = Vec::with_capacity(ids.len());
        let opened = self.opened_at.lock().await;
        let alive = self.alive.lock().await;
        for id in ids {
            let spec = self
                .core
                .spec(id)
                .unwrap_or(TransportSpec::Local {
                    shell: None,
                    cwd: None,
                });
            out.push(SessionInfo {
                id: id.to_string(),
                spec,
                alive: *alive.get(&id).unwrap_or(&false),
                opened_at_ms: *opened.get(&id).unwrap_or(&0),
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

    pub async fn is_alive(&self, id: Uuid) -> bool {
        *self.alive.lock().await.get(&id).unwrap_or(&false)
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
