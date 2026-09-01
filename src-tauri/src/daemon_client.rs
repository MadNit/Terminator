//! HTTP client for `terminator-daemon`. Tauri commands call into this
//! instead of touching `core::SessionManager` directly, so the daemon
//! owns every PTY/SSH/RDP process and they survive a UI restart.
//!
//! Lifecycle is handled by `spawn_or_connect` in lib.rs; the resulting
//! `DaemonClient` is what the Tauri commands hold in `AppState`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{info, warn};
use uuid::Uuid;

use terminator_core::transport::TransportSpec;

/// Wire shape of the daemon's `OutputEvent` enum. Kept identical to
/// the daemon's `OutputEvent` in `daemon/src/manager.rs` -- the two
/// crates can't share the type (one is a lib, one a bin), so this
/// is the contract. The serde tag matches the daemon's
/// `#[serde(tag = "type", rename_all = "camelCase")]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutputEvent {
    Output { data: String },
    Exit,
}

#[derive(Clone)]
pub struct DaemonClient {
    base_url: String,
    http: reqwest::Client,
}

impl DaemonClient {
    /// Open a new session on the daemon. Returns the session id and
    /// an `SseStream` that yields `OutputEvent`s as the daemon
    /// produces them. The stream is single-use; spawn a task that
    /// drains it into a Tauri Channel.
    pub async fn open(
        &self,
        spec: TransportSpec,
        cols: u16,
        rows: u16,
        password: Option<&str>,
    ) -> Result<(Uuid, SseStream)> {
        let body = serde_json::json!({
            "spec": spec,
            "cols": cols,
            "rows": rows,
            "password": password,
        });
        let resp = self
            .http
            .post(format!("{}/sessions", self.base_url))
            .json(&body)
            .send()
            .await
            .context("POST /sessions")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("POST /sessions returned {status}: {body}"));
        }
        let open: OpenResponse = resp.json().await.context("parse OpenResponse")?;
        let id: Uuid = open
            .id
            .parse()
            .with_context(|| format!("daemon returned a non-UUID id: {}", open.id))?;

        // Open the SSE stream separately. The Tauri command will
        // drain it and forward events into the Tauri Channel.
        let sse_url = format!("{}/sessions/{}/output", self.base_url, id);
        let sse_resp = self
            .http
            .get(&sse_url)
            .send()
            .await
            .with_context(|| format!("GET {sse_url}"))?;
        if !sse_resp.status().is_success() {
            let status = sse_resp.status();
            return Err(anyhow!("GET /sessions/{id}/output returned {status}"));
        }
        let stream = SseStream::new(sse_resp);
        Ok((id, stream))
    }

    pub async fn write(&self, id: Uuid, data: Bytes) -> Result<()> {
        let url = format!("{}/sessions/{}/input", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({
                "data_b64": base64::engine::general_purpose::STANDARD.encode(&data),
            }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("POST {url} returned {status}: {body}"));
        }
        Ok(())
    }

    pub async fn resize(&self, id: Uuid, cols: u16, rows: u16) -> Result<()> {
        let url = format!("{}/sessions/{}/resize", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "cols": cols, "rows": rows }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("POST {url} returned {status}: {body}"));
        }
        Ok(())
    }

    pub async fn close(&self, id: Uuid) -> Result<()> {
        let url = format!("{}/sessions/{}", self.base_url, id);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("DELETE {url} returned {status}: {body}"));
        }
        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Value> {
        let url = format!("{}/sessions", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(anyhow!("GET {url} returned {status}"));
        }
        Ok(resp.json().await?)
    }
}

#[derive(Debug, Deserialize)]
struct OpenResponse {
    id: String,
}

/// `SseStream` is a thin wrapper around a reqwest `Bytes` stream
/// that turns `data: ...\n\n` chunks into [`OutputEvent`]s. A small
/// hand-rolled parser is plenty here; the daemon only emits
/// `Output` and `Exit` and we never expect multi-line `data:`
/// fields.
pub struct SseStream {
    inner: std::pin::Pin<Box<dyn futures::stream::Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buffer: Vec<u8>,
    done: bool,
}

impl SseStream {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: Box::pin(resp.bytes_stream()),
            buffer: Vec::with_capacity(4096),
            done: false,
        }
    }
}

impl futures::stream::Stream for SseStream {
    type Item = Result<OutputEvent>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            // Try to parse a complete event from the buffer first.
            if let Some(event) = take_event(&mut self.buffer) {
                return std::task::Poll::Ready(Some(Ok(event)));
            }
            if self.done {
                return std::task::Poll::Ready(None);
            }
            // Pull more bytes from the underlying transport.
            match self.inner.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    self.buffer.extend_from_slice(&chunk);
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(anyhow!(e))));
                }
                std::task::Poll::Ready(None) => {
                    self.done = true;
                    // Loop once more to flush any trailing event.
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

/// Pull one complete SSE event from `buf`. Returns `None` if the
/// buffer doesn't yet end with a blank line. The `data:` field is
/// the only one we care about; everything else is ignored.
fn take_event(buf: &mut Vec<u8>) -> Option<OutputEvent> {
    // SSE events are delimited by a blank line. Find the first
    // double-newline pair.
    let sep = find_double_newline(buf)?;
    let event_bytes = &buf[..sep];
    // Advance past the separator (the "\n\n" itself).
    let sep_len = if buf.starts_with(b"\r\n\r\n", sep) {
        4
    } else {
        2
    };
    let _ = buf.drain(..sep + sep_len);

    let event_str = match std::str::from_utf8(event_bytes) {
        Ok(s) => s,
        Err(_) => return None,
    };

    // Concatenate every `data:` line. SSE allows multiple data
    // lines per event; the daemon never emits them, but be safe.
    let mut data = String::new();
    for line in event_str.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
    }
    if data.is_empty() {
        return None;
    }
    match serde_json::from_str::<OutputEvent>(&data) {
        Ok(ev) => Some(ev),
        Err(e) => Some(Err(anyhow!("malformed SSE event: {e}; data={data}"))),
    }
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    // Use a small window: search for "\n\n" or "\r\n\r\n".
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
        if i + 3 < buf.len()
            && &buf[i..i + 4] == b"\r\n\r\n"
        {
            return Some(i);
        }
    }
    None
}

/// Locate the per-user data dir we wrote `daemon.port` to.
fn data_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| anyhow!("LOCALAPPDATA is not set"))?;
        Ok(PathBuf::from(base).join("terminator"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("terminator"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(xdg).join("terminator"));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".local").join("share").join("terminator"))
    }
}

fn port_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("daemon.port"))
}

/// Probe whether a daemon is already running on the port recorded in
/// `daemon.port`. Returns `Some(client)` on success, `None` if no
/// port file or the daemon is dead.
pub async fn try_connect() -> Result<Option<DaemonClient>> {
    let path = port_file()?;
    let port = match std::fs::read_to_string(&path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return Ok(None),
    };
    let port: u16 = match port.parse() {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let client = DaemonClient {
        base_url: format!("http://127.0.0.1:{port}"),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("build reqwest client")?,
    };
    match client.http.get(format!("{}/health", client.base_url)).send().await {
        Ok(r) if r.status().is_success() => Ok(Some(client)),
        _ => {
            // Stale port file; clear it so we don't probe again.
            let _ = std::fs::remove_file(&path);
            Ok(None)
        }
    }
}

/// Spawn a fresh daemon as a detached child process, then wait for
/// it to write a `daemon.port` we can connect to.
pub async fn spawn_daemon() -> Result<DaemonClient> {
    let exe = find_daemon_exe().context("locate terminator-daemon binary")?;
    info!(?exe, "spawning terminator-daemon");
    let mut cmd = Command::new(&exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach the daemon from this process. On Windows,
    // `CREATE_NO_WINDOW` is implied by detaching stdio; the
    // daemon doesn't open a console. On Unix, the child becomes a
    // new session leader so it survives the parent's exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().with_context(|| format!("spawn {exe:?}"))?;

    // Poll for daemon.port. The daemon writes it almost
    // immediately, so 2 seconds is plenty in practice; we allow
    // more to be safe on cold cache.
    let path = port_file()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(p) = s.trim().parse::<u16>() {
                let client = DaemonClient {
                    base_url: format!("http://127.0.0.1:{p}"),
                    http: reqwest::Client::builder()
                        .timeout(Duration::from_secs(5))
                        .build()
                        .context("build reqwest client")?,
                };
                if client
                    .http
                    .get(format!("{}/health", client.base_url))
                    .send()
                    .await
                    .is_ok()
                {
                    return Ok(client);
                }
            }
        }
        if std::time::Instant::now() > deadline {
            return Err(anyhow!(
                "spawned daemon did not write a usable port file within 10s"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Resolve the path to the daemon binary. In dev, it's in the same
/// `target/{profile}` directory as `terminator.exe`. In production
/// (Tauri bundle), it ships next to the main exe.
fn find_daemon_exe() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "terminator-daemon.exe"
    } else {
        "terminator-daemon"
    };

    // First, look in the current process's directory. `cargo run`
    // places both binaries in `target/debug/`, and Tauri's MSI/NSIS
    // bundles the daemon next to the main exe.
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let candidate = dir.join(exe_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // Fall back: walk up from CARGO_MANIFEST_DIR looking for the
    // `target/{profile}/terminator-daemon` that `cargo run -p
    // terminator-daemon` produces. Only relevant in dev.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    for ancestor in Path::new(manifest_dir).ancestors() {
        let candidate = ancestor.join("target").join(profile).join(exe_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// One-shot helper: try to connect to an existing daemon, or spawn
/// one if none is running. Returns the live client.
pub async fn spawn_or_connect() -> Result<DaemonClient> {
    if let Some(client) = try_connect().await? {
        info!("connected to existing terminator-daemon");
        return Ok(client);
    }
    warn!("no terminator-daemon found; spawning a new one");
    spawn_daemon().await
}
