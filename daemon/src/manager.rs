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
}

impl DaemonSessionManager {
    pub fn new(core: Arc<SessionManager>) -> Self {
        Self {
            core,
            channels: Mutex::new(std::collections::HashMap::new()),
            buffers: Mutex::new(std::collections::HashMap::new()),
            opened_at: Mutex::new(std::collections::HashMap::new()),
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
        self.opened_at.lock().await.insert(id, now_ms());

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
                alive: true, // core still has the id, so it's alive
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
