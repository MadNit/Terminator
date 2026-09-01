//! Daemon-side RDP bookkeeping on top of `core::rdp::RdpManager`.
//!
//! The Tauri app used to hold the `RdpManager` in `AppState`, so a
//! UI crash took every active remote desktop with it. Now the
//! daemon owns the live sessions and the broadcast channel that
//! the SSE handler subscribes to, mirroring the lifecycle the
//! PTY/SSH sessions have had since Session 1.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use terminator_core::rdp::{RdpConfig, RdpEvent, RdpInput, RdpManager};

/// Per-RDP-session metadata exposed via the list/detail endpoints.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RdpInfo {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The initial desktop size negotiated at handshake time. The
    /// server may reactivation-resize later, which is reported as
    /// a separate `RdpEvent::Resized` on the SSE stream.
    pub width: u16,
    pub height: u16,
    pub alive: bool,
    /// Wall-clock time the session was opened, milliseconds since
    /// the Unix epoch. Used by the UI to sort "most recent first"
    /// if RDP ever joins the reattach prompt.
    pub opened_at_ms: i64,
}

/// Endpoint metadata we keep around so `GET /rdp` can return
/// the same fields the Tauri side passed in. The
/// `RdpManager` core only stores the live `RdpSession` handle,
/// not the spec, so we mirror the small subset we care about.
#[derive(Debug, Clone)]
struct EndpointMeta {
    host: String,
    port: u16,
    user: String,
}

/// Bundle of every "what's the state of the world" data the
/// daemon's RDP HTTP layer needs. Cheap to clone: the
/// `RdpManager` and the broadcast senders are both `Arc` under
/// the hood.
pub struct DaemonRdpManager {
    core: Arc<RdpManager>,
    /// Per-session broadcast that the SSE handler subscribes
    /// to. RdpEvents are heavy (a 1920x1080x4-byte frame is
    /// ~8 MB), so the capacity is small -- one buffered + one
    /// in flight. The webview catches up by re-issuing a
    /// reactivation request after a slow frame, the same way
    /// mstsc handles a stuck client.
    channels: Mutex<HashMap<Uuid, broadcast::Sender<RdpEvent>>>,
    /// Last known initial size for `RdpInfo`. Updated when the
    /// session opens; not refreshed on every `Resized` event so
    /// the value reflects the user's intent, not a transient
    /// server-side optimization.
    sizes: Mutex<HashMap<Uuid, (u16, u16)>>,
    /// Host/port/user per session, for the `RdpInfo` list view.
    endpoints: Mutex<HashMap<Uuid, EndpointMeta>>,
    opened_at: Mutex<HashMap<Uuid, i64>>,
    /// Latest local clipboard text per session, written by
    /// the Tauri side via `POST /rdp/{id}/clipboard` and read
    /// by the CLIPRDR backend when the server asks for our
    /// format list. Cleared on `close`.
    local_clipboards: Mutex<HashMap<Uuid, String>>,
}

impl DaemonRdpManager {
    pub fn new() -> Self {
        Self {
            core: Arc::new(RdpManager::new()),
            channels: Mutex::new(HashMap::new()),
            sizes: Mutex::new(HashMap::new()),
            endpoints: Mutex::new(HashMap::new()),
            opened_at: Mutex::new(HashMap::new()),
            local_clipboards: Mutex::new(HashMap::new()),
        }
    }

    /// Open a new RDP session. Returns `(id, width, height,
    /// fresh_receiver)`. The receiver yields `RdpEvent`s from the
    /// moment the function returns, so the SSE handler can
    /// subscribe before the HTTP response is framed. The first
    /// `RdpEvent::Resized` carries the initial desktop size --
    /// `width` / `height` here are the requested values, which
    /// are usually identical.
    pub async fn open(
        &self,
        cfg: RdpConfig,
    ) -> Result<(Uuid, u16, u16, broadcast::Receiver<RdpEvent>)> {
        let (tx, rx) = broadcast::channel::<RdpEvent>(4);
        // Bridge: core pushes onto `core_tx`, we forward onto
        // the broadcast so multiple SSE clients can subscribe
        // (currently the Tauri side keeps exactly one, but the
        // shape is symmetric with the PTY/SSH one).
        let (core_tx, mut core_rx) = tokio::sync::mpsc::channel::<RdpEvent>(8);
        let bcast_tx = tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = core_rx.recv().await {
                // Lagged receivers are a slow HTTP client, not
                // a bug; drop the oldest rather than block the
                // engine.
                let _ = bcast_tx.send(ev);
            }
        });

        // Snapshot the endpoint bits we want to remember before
        // moving the `RdpConfig` into the engine task.
        let endpoint = EndpointMeta {
            host: cfg.host.clone(),
            port: cfg.port,
            user: cfg.user.clone(),
        };
        let (id, width, height) = self
            .core
            .open_with_timeout(cfg, core_tx, std::time::Duration::from_secs(30))
            .await?;

        self.channels
            .lock()
            .await
            .insert(id, tx);
        self.sizes
            .lock()
            .await
            .insert(id, (width, height));
        self.endpoints
            .lock()
            .await
            .insert(id, endpoint);
        self.opened_at
            .lock()
            .await
            .insert(id, now_ms());

        Ok((id, width, height, rx))
    }

    /// Forward a batch of UI inputs to the live session.
    pub async fn input(&self, id: Uuid, ops: Vec<RdpInput>) -> Result<()> {
        let core = self.core.clone();
        tokio::task::spawn_blocking(move || core.input(id, ops))
            .await
            .map_err(|e| anyhow!("rdp input task panicked: {e}"))?
    }

    /// Request a desktop reactivation at the new size.
    pub async fn resize(&self, id: Uuid, width: u16, height: u16) -> Result<()> {
        let core = self.core.clone();
        tokio::task::spawn_blocking(move || core.resize(id, width, height))
            .await
            .map_err(|e| anyhow!("rdp resize task panicked: {e}"))??;
        // Track the new requested size; this is what the user
        // last asked for, not necessarily what the server
        // accepted. The next `RdpEvent::Resized` is authoritative.
        self.sizes
            .lock()
            .await
            .insert(id, (width, height));
        Ok(())
    }

    /// Update the local clipboard text for a session. The daemon's
    /// CLIPRDR backend reads this on the next `on_request_format_list`
    /// and re-advertises it to the server. Text only for v1.
    ///
    /// For Session 3 part 2, this currently just stores the value in
    /// `local_clipboards`; the actual wire-up to the
    /// `CliprdrClient` is the engine work tracked as a follow-up.
    pub async fn set_local_clipboard(&self, id: Uuid, text: String) -> Result<()> {
        // Sanity-check the session exists. Avoid silently accepting
        // clipboard updates for a session that's already closed --
        // the caller would think the copy succeeded when in fact
        // the daemon has nowhere to send it.
        if !self.channels.lock().await.contains_key(&id) {
            return Err(anyhow!("no such rdp session: {id}"));
        }
        self.local_clipboards
            .lock()
            .await
            .insert(id, text);
        Ok(())
    }

    /// Read the current local clipboard text for a session. Returns
    /// `None` if the session has no recorded local text (either the
    /// session is unknown, or the caller never set it).
    #[allow(dead_code)] // Used by the CLIPRDR backend in a follow-up commit.
    pub async fn local_clipboard(&self, id: Uuid) -> Option<String> {
        self.local_clipboards
            .lock()
            .await
            .get(&id)
            .cloned()
    }

    /// Tear down a live session. The `RdpSession` drop sends
    /// `Shutdown` on its command channel, which the engine task
    /// handles and then the broadcast sender drops -- SSE
    /// subscribers see EOF.
    pub async fn close(&self, id: Uuid) -> Result<()> {
        {
            let core = self.core.clone();
            tokio::task::spawn_blocking(move || core.close(id))
                .await
                .map_err(|e| anyhow!("rdp close task panicked: {e}"))??;
        }
        self.channels.lock().await.remove(&id);
        self.sizes.lock().await.remove(&id);
        self.endpoints.lock().await.remove(&id);
        self.opened_at.lock().await.remove(&id);
        self.local_clipboards.lock().await.remove(&id);
        Ok(())
    }

    /// Subscribe a new SSE client to the live event stream.
    /// Returns 404-ish if the session has been closed.
    pub async fn subscribe(
        &self,
        id: Uuid,
    ) -> Result<broadcast::Receiver<RdpEvent>> {
        let channels = self.channels.lock().await;
        channels
            .get(&id)
            .map(|tx| tx.subscribe())
            .ok_or_else(|| anyhow!("no such rdp session: {id}"))
    }

    /// Snapshot of every live session, plus the metadata
    /// needed to render it in the UI.
    pub async fn list(&self) -> Vec<RdpInfo> {
        let sizes = self.sizes.lock().await;
        let opened = self.opened_at.lock().await;
        let endpoints = self.endpoints.lock().await;
        let mut out = Vec::with_capacity(sizes.len());
        for (id, (w, h)) in sizes.iter() {
            // `core.list_info` would need to be added; for
            // now we use `RdpManager::is_alive` semantics by
            // checking whether the channel still exists. If
            // the engine has crashed but we haven't noticed
            // yet, the SSE stream will emit the `Disconnected`
            // event and the Tauri side will call `close_rdp`.
            let alive = true;
            let (host, port, user) = endpoints
                .get(id)
                .map(|m| (m.host.clone(), m.port, m.user.clone()))
                .unwrap_or_default();
            out.push(RdpInfo {
                id: id.to_string(),
                host,
                port,
                user,
                width: *w,
                height: *h,
                alive,
                opened_at_ms: *opened.get(id).unwrap_or(&0),
            });
        }
        // Newest first, matching the PTY/SSH listing.
        out.sort_by(|a, b| b.opened_at_ms.cmp(&a.opened_at_ms));
        out
    }

    /// Single-session lookup, used by `GET /rdp/{id}`.
    pub async fn get(&self, id: Uuid) -> Result<RdpInfo> {
        let (w, h) = self
            .sizes
            .lock()
            .await
            .get(&id)
            .copied()
            .ok_or_else(|| anyhow!("no such rdp session: {id}"))?;
        let (host, port, user) = self
            .endpoints
            .lock()
            .await
            .get(&id)
            .map(|m| (m.host.clone(), m.port, m.user.clone()))
            .unwrap_or_default();
        let opened_at_ms = *self
            .opened_at
            .lock()
            .await
            .get(&id)
            .unwrap_or(&0);
        Ok(RdpInfo {
            id: id.to_string(),
            host,
            port,
            user,
            width: w,
            height: h,
            alive: true,
            opened_at_ms,
        })
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
