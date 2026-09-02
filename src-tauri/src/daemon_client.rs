//! HTTP client for `terminator-daemon`. Tauri commands call into this
//! instead of touching `core::SessionManager` directly, so the daemon
//! owns every PTY/SSH/RDP process and they survive a UI restart.
//!
//! Lifecycle is handled by `spawn_or_connect` in lib.rs; the resulting
//! `DaemonClient` is what the Tauri commands hold in `AppState`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tracing::{info, warn};
use uuid::Uuid;

use terminator_core::files::Listing;
use terminator_core::session::Credentials;
use terminator_core::transport::TransportSpec;

/// Per-byte progress for a file transfer, streamed from the
/// daemon's `/files/upload` and `/files/download` SSE
/// responses. Mirrors `core::TransferEvent` and the
/// `Channel<TransferEvent>` on the lib.rs side; the SSE
/// parser just deserialises each `data:` payload into
/// this enum and the Tauri command forwards it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TransferEvent {
    /// Per-chunk update. `transferred` is the running total
    /// in bytes; `total` is the expected final size (0 if
    /// the source size wasn't known up front, e.g. some
    /// remote endpoints don't report it before the body
    /// starts).
    Progress {
        transferred: u64,
        total: u64,
    },
    /// Transfer finished; `bytes` is the total written.
    Done {
        bytes: u64,
    },
    /// Transfer failed; `message` is the human-readable
    /// error suitable for surfacing in the UI.
    Failed {
        message: String,
    },
}

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
        creds: &Credentials,
    ) -> Result<(Uuid, SseStream<OutputEvent>)> {
        let body = serde_json::json!({
            "spec": spec,
            "cols": cols,
            "rows": rows,
            "password": creds.secret,
            "key_passphrase": creds.key_passphrase,
            "jump_password": creds.jump_secret,
            "jump_key_passphrase": creds.jump_key_passphrase,
        });
        let body = serde_json::to_string(&body).context("serialize open body")?;
        // SSH `open` blocks until the daemon finishes the TCP
        // connect + handshake + auth + PTY-open round trip
        // (worst case ~50s on a slow link: the daemon's own
        // CONNECT_TIMEOUT is 20s and AUTH_TIMEOUT is 30s). The
        // default reqwest client below has a 5s timeout, which
        // would make every SSH connect abort before the daemon
        // even responded -- the front-end then shows
        // "[connection failed]". Override per call so the other
        // endpoints keep the snappy 5s limit.
        let resp = self
            .http
            .post(format!("{}/sessions", self.base_url))
            .header("content-type", "application/json")
            .body(body)
            .timeout(Duration::from_secs(90))
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
        // The SSE stream is long-lived; the reqwest client's
        // 5s default would cut it off the moment the first
        // silence arrived. Drop the per-request timeout so
        // the connection stays up until the daemon closes
        // it on session exit.
        let sse_resp = self
            .http
            .get(&sse_url)
            .timeout(Duration::from_secs(24 * 60 * 60))
            .send()
            .await
            .with_context(|| format!("GET {sse_url}"))?;
        if !sse_resp.status().is_success() {
            let status = sse_resp.status();
            return Err(anyhow!("GET /sessions/{id}/output returned {status}"));
        }
        let stream = SseStream::<OutputEvent>::new(sse_resp);
        Ok((id, stream))
    }

    pub async fn write(&self, id: Uuid, data: Bytes) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let url = format!("{}/sessions/{}/input", self.base_url, id);
        let encoded = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&data)
        };
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "data_b64": encoded }))
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

    /// Subscribe to a session that already exists on the daemon.
    /// This is the reattach path: the user has the daemon from
    /// a previous Tauri run still alive in the background, and
    /// wants to keep watching a session that was opened in
    /// that previous run. The returned stream replays the
    /// scrollback first and then yields live events, same as
    /// the open path -- the Tauri Channel the consumer drains
    /// this into looks identical to the one for a fresh open.
    pub async fn attach(&self, id: Uuid) -> Result<SseStream<OutputEvent>> {
        let sse_url = format!("{}/sessions/{}/output", self.base_url, id);
        let sse_resp = self
            .http
            .get(&sse_url)
            .timeout(Duration::from_secs(24 * 60 * 60))
            .send()
            .await
            .with_context(|| format!("GET {sse_url}"))?;
        if !sse_resp.status().is_success() {
            let status = sse_resp.status();
            return Err(anyhow!("GET /sessions/{id}/output returned {status}"));
        }
        Ok(SseStream::<OutputEvent>::new(sse_resp))
    }

    // ---- File browser + exec through the daemon ----
    //
    // Each of these was a `state.helpers.files(id).await` call
    // before commit 258e4cf moved sessions to the daemon. Now
    // they hit the matching daemon route, which runs the
    // RemoteFs method in the process that owns the SSH
    // connection.

    pub async fn files_home(&self, id: Uuid) -> Result<String> {
        let url = format!("{}/sessions/{}/files/home", self.base_url, id);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(resp.text().await.context("read files/home body")?)
    }

    pub async fn files_list(&self, id: Uuid, path: &str) -> Result<Listing> {
        let url = format!("{}/sessions/{}/files/list", self.base_url, id);
        let resp = self.http.get(&url).query(&[("path", path)]).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse files/list body")
    }

    pub async fn files_mkdir(&self, id: Uuid, path: &str) -> Result<()> {
        self.files_write_like("mkdir", id, path).await
    }

    pub async fn files_remove(&self, id: Uuid, path: &str, is_dir: bool) -> Result<()> {
        let url = format!("{}/sessions/{}/files/remove", self.base_url, id);
        let resp = self.http.post(&url)
            .json(&serde_json::json!({ "path": path, "is_dir": is_dir }))
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn files_rename(&self, id: Uuid, from: &str, to: &str) -> Result<()> {
        let url = format!("{}/sessions/{}/files/rename", self.base_url, id);
        let resp = self.http.post(&url)
            .json(&serde_json::json!({ "from": from, "to": to }))
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn files_read(&self, id: Uuid, path: &str, max_bytes: usize) -> Result<String> {
        let url = format!("{}/sessions/{}/files/read", self.base_url, id);
        let max_str = max_bytes.to_string();
        let resp = self.http.get(&url)
            .query(&[("path", path), ("max", max_str.as_str())])
            .send().await.with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.text().await.context("read files/read body")
    }

    pub async fn files_write(&self, id: Uuid, path: &str, content: &str) -> Result<()> {
        let url = format!("{}/sessions/{}/files/write", self.base_url, id);
        let resp = self.http.post(&url)
            .json(&serde_json::json!({ "path": path, "content": content }))
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn exec_command(
        &self,
        spec: &TransportSpec,
        command: &str,
        creds: &terminator_core::session::Credentials,
        cwd: Option<&str>,
    ) -> Result<terminator_core::session::ExecResult> {
        // The daemon's POST /sessions/{id}/exec route uses the
        // spec from the request body, not from the URL, so we
        // can pass any UUID in the path. Nil is the obvious
        // "this slot is meaningless" choice; the daemon
        // handler does not read it.
        let url = format!("{}/sessions/{}/exec", self.base_url, Uuid::nil());
        let body = serde_json::json!({
            "spec": spec,
            "command": command,
            "password": creds.secret,
            "key_passphrase": creds.key_passphrase,
            "jump_password": creds.jump_secret,
            "jump_key_passphrase": creds.jump_key_passphrase,
            "cwd": cwd,
        });
        let resp = self.http.post(&url).json(&body).send().await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse exec body")
    }

    async fn files_write_like(&self, op: &str, id: Uuid, path: &str) -> Result<()> {
        let url = format!("{}/sessions/{}/files/{}", self.base_url, id, op);
        let resp = self.http.post(&url)
            .json(&serde_json::json!({ "path": path }))
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    // -- file transfer (local <-> remote) ----------------------------
    //
    // The daemon owns the SSH connection, so the local file on the
    // Tauri side is read by the daemon (they are on the same
    // machine, sharing the user data dir).
    //
    // Both upload and download now stream progress over
    // SSE (the daemon's response body is a
    // `text/event-stream`). The Tauri command consumes
    // the stream and pushes `Progress` / `Done` /
    // `Failed` events to its `Channel<TransferEvent>`.

    pub async fn files_upload(
        &self,
        id: Uuid,
        local_path: &str,
        remote: &str,
    ) -> Result<SseStream<TransferEvent>> {
        let url = format!("{}/sessions/{}/files/upload", self.base_url, id);
        let body = serde_json::json!({
            "local_path": local_path,
            "remote": remote,
        });
        let resp = self.http.post(&url)
            .json(&body)
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(SseStream::<TransferEvent>::new(resp))
    }

    pub async fn files_download(
        &self,
        id: Uuid,
        remote: &str,
        local_path: &str,
    ) -> Result<SseStream<TransferEvent>> {
        let url = format!("{}/sessions/{}/files/download", self.base_url, id);
        let body = serde_json::json!({
            "remote": remote,
            "local_path": local_path,
        });
        let resp = self.http.post(&url)
            .json(&body)
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(SseStream::<TransferEvent>::new(resp))
    }

    pub async fn files_search(
        &self,
        id: Uuid,
        path: &str,
        options: &terminator_core::files::SearchOptions,
    ) -> Result<Vec<terminator_core::files::FileSearchResult>> {
        let url = format!("{}/sessions/{}/files/search", self.base_url, id);
        let body = serde_json::json!({
            "path": path,
            "options": options,
        });
        let resp = self.http.post(&url)
            .json(&body)
            .send().await.with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse files_search body")
    }

    // -- log file management -----------------------------------------
    //
    // These proxy the Tauri commands that used to read from
    // `state.helpers.log_dir()` / `state.helpers.logs()`. Now that
    // the daemon owns the on-disk log directory, every read or
    // delete goes through HTTP so the path resolution stays in one
    // process.

    pub async fn log_dir(&self) -> Result<String> {
        let url = format!("{}/log_dir", self.base_url);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.text().await.context("read log_dir body")
    }

    pub async fn list_session_logs(&self) -> Result<serde_json::Value> {
        let url = format!("{}/session_logs", self.base_url);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse session_logs body")
    }

    pub async fn delete_session_log(&self, dir_name: &str) -> Result<()> {
        let url = format!("{}/session_logs/{}", self.base_url, dir_name);
        let resp = self.http.delete(&url).send().await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn read_log_file(&self, path: &str) -> Result<String> {
        let url = format!("{}/log_file", self.base_url);
        let resp = self.http.get(&url)
            .query(&[("path", path)])
            .send().await.with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.text().await.context("read log_file body")
    }

    pub async fn session_log_paths(&self, id: Uuid) -> Result<serde_json::Value> {
        let url = format!("{}/sessions/{}/logs", self.base_url, id);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse session_log_paths body")
    }

    /// Cross-session search. Walks every live session's
    /// scrollback ring buffer and returns the lines that
    /// contain `needle`. The webview groups the results by
    /// session id and shows them in the command palette.
    pub async fn search_sessions(
        &self,
        needle: &str,
        case_sensitive: bool,
        max_per_session: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}/search", self.base_url);
        let case_str = if case_sensitive { "true" } else { "false" };
        let max_str = max_per_session.to_string();
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("q", needle),
                ("case_sensitive", case_str),
                ("max_per_session", max_str.as_str()),
            ])
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        resp.json().await.context("parse search body")
    }

    // -- RDP ---------------------------------------------------------
    //
    // The daemon owns every RDP session, so the Tauri side is
    // a pure HTTP client here. Open returns the SSE stream of
    // `RdpEvent`s alongside the id + initial size; the caller
    // is expected to drain the stream into a Tauri Channel
    // (mirroring what `open` does for PTY/SSH).
    //
    // The cleartext password travels over 127.0.0.1 only --
    // same loopback trust model the SSH `open` already uses.

    pub async fn rdp_open(
        &self,
        cfg: &terminator_core::rdp::RdpConfig,
    ) -> Result<(Uuid, u16, u16, SseStream<terminator_core::rdp::RdpEvent>)> {
        let url = format!("{}/rdp", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(cfg)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        let opened: RdpOpenResponse = resp.json().await
            .context("parse rdp_open body")?;
        let id = Uuid::parse_str(&opened.id)
            .map_err(|e| anyhow!("daemon returned bad uuid {}: {e}", opened.id))?;

        // Subscribe to the SSE stream AFTER the open call so
        // the daemon has had a chance to register the channel
        // (it does this in `open` before returning, so the race
        // is closed). The Tauri side drains this into the
        // existing `Channel<RdpEvent>` the webview listens to.
        let sse_url = format!("{}/rdp/{}/output", self.base_url, id);
        let sse_resp = self
            .http
            .get(&sse_url)
            .send()
            .await
            .with_context(|| format!("GET {sse_url}"))?;
        if !sse_resp.status().is_success() {
            let status = sse_resp.status();
            return Err(anyhow!("GET /rdp/{id}/output returned {status}"));
        }
        let stream = SseStream::<terminator_core::rdp::RdpEvent>::new(sse_resp);
        Ok((id, opened.width, opened.height, stream))
    }

    pub async fn rdp_input(
        &self,
        id: Uuid,
        ops: Vec<terminator_core::rdp::RdpInput>,
    ) -> Result<()> {
        let url = format!("{}/rdp/{}/input", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .json(&ops)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn rdp_resize(
        &self,
        id: Uuid,
        width: u16,
        height: u16,
    ) -> Result<()> {
        let url = format!("{}/rdp/{}/resize", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "width": width, "height": height }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    pub async fn rdp_close(&self, id: Uuid) -> Result<()> {
        let url = format!("{}/rdp/{}", self.base_url, id);
        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }

    /// Push a local clipboard update to the daemon. The daemon's
    /// CLIPRDR backend uses this to (re-)advertise the local
    /// clipboard to the RDP server the next time it asks for a
    /// format list. Text only for v1.
    pub async fn rdp_local_clipboard(&self, id: Uuid, text: &str) -> Result<()> {
        let url = format!("{}/rdp/{}/clipboard", self.base_url, id);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(http_err(status, body, &url));
        }
        Ok(())
    }
}

fn http_err(status: reqwest::StatusCode, body: String, url: &str) -> anyhow::Error {
    anyhow!("{url} returned {status}: {body}")
}


#[derive(Debug, Deserialize)]
struct OpenResponse {
    id: String,
}

/// Wire shape for `POST /rdp`. Matches the daemon's
/// `RdpOpened` struct.
#[derive(Debug, Deserialize)]
struct RdpOpenResponse {
    id: String,
    width: u16,
    height: u16,
}

/// `SseStream<T>` is a thin wrapper around a reqwest `Bytes`
/// stream that turns `data: ...\n\n` chunks into `T`s. Generic
/// over the event type because the daemon emits two different
/// shapes: `OutputEvent` for PTY/SSH byte streams and `RdpEvent`
/// for the RDP desktop frame stream. A small hand-rolled parser
/// is plenty here; the daemon only emits single-line `data:`
/// fields and we never expect multi-line events.
pub struct SseStream<T> {
    inner: std::pin::Pin<Box<dyn futures::stream::Stream<Item = reqwest::Result<Bytes>> + Send>>,
    buffer: Vec<u8>,
    done: bool,
    _marker: std::marker::PhantomData<T>,
}

impl<T> SseStream<T> {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: Box::pin(resp.bytes_stream()),
            buffer: Vec::with_capacity(4096),
            done: false,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> futures::stream::Stream for SseStream<T>
where
    T: serde::de::DeserializeOwned + Send + Unpin + 'static,
{
    type Item = Result<T>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            // Try to parse a complete event from the buffer first.
            match take_event::<T>(&mut self.buffer) {
                Some(Ok(event)) => return std::task::Poll::Ready(Some(Ok(event))),
                Some(Err(e)) => return std::task::Poll::Ready(Some(Err(e))),
                None => {}
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
fn take_event<T: serde::de::DeserializeOwned>(buf: &mut Vec<u8>) -> Option<Result<T>> {
    // SSE events are delimited by a blank line. Find the first
    // double-newline pair.
    let sep = find_double_newline(buf)?;
    // Copy the event bytes out before draining, since the drain
    // needs a mutable borrow that conflicts with the slice.
    let event_bytes = buf[..sep].to_vec();
    // Advance past the separator (the "\n\n" itself).
    let sep_len = if buf[sep..].starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    };
    let _ = buf.drain(..sep + sep_len);

    let event_str = match std::str::from_utf8(&event_bytes) {
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
    Some(serde_json::from_str::<T>(&data)
        .map_err(|e| anyhow!("malformed SSE event: {e}; data={data}")))
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

/// Path the daemon's stderr is teed to so an early-exit / panic
/// during startup is visible to whoever launched it. Mirrors
/// `daemon::resolve_data_dir` on the daemon side, but never
/// fails -- if we cannot resolve the per-user dir (e.g. on a
/// server with no `LOCALAPPDATA`), we fall back to the system
/// temp dir.
fn daemon_stderr_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p)
            .join("terminator")
            .join("logs")
            .join("daemon.stderr.log");
    }
    if let Some(p) = std::env::var_os("TEMP") {
        return PathBuf::from(p).join("terminator-daemon.stderr.log");
    }
    PathBuf::from("terminator-daemon.stderr.log")
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
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    // Capture stderr to a per-user log file. The previous version
    // piped stderr to /dev/null, which meant a panic during
    // startup (or any tracing output) was invisible: the spawn
    // would succeed, the port file would never appear, and the
    // 10s poll below would time out with no diagnostic. Teeing
    // stderr to a known location lets us read it from outside
    // the daemon when this happens.
    let stderr_path = daemon_stderr_path();
    if let Some(parent) = stderr_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&stderr_path)
    {
        Ok(file) => {
            info!(?stderr_path, "daemon stderr -> file");
            cmd.stderr(Stdio::from(file));
        }
        Err(e) => {
            warn!(?stderr_path, ?e, "could not open daemon stderr log; falling back to /dev/null");
            cmd.stderr(Stdio::null());
        }
    }
    // Detach the daemon from this process. On Unix, the child
    // becomes a new session leader so it survives the parent's
    // exit. On Windows, stdio is fully detached; the daemon has
    // no controlling console.
    #[cfg(unix)]
    {
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

// =========================================================================
// Resilient wrapper
// =========================================================================
//
// `DaemonClient` holds a `base_url` baked in at startup. If the
// `terminator-daemon` process dies mid-session (SIGKILL, OOM, the
// console window being closed before the `windows_subsystem = "windows"`
// fix landed, ...) every Tauri command that talks to it gets a TCP
// `connection refused` and the user's terminal stops accepting input.
//
// `ResilientDaemon` wraps a `DaemonClient` and treats that category of
// error as a signal to respawn. On the first request failure the
// `inner` `Arc<DaemonClient>` is replaced with one pointing at the
// new daemon's port and the same call is retried once. Application
// errors (HTTP 4xx/5xx with a real response body, JSON parse failures,
// ...) are NOT retried -- they would either still fail against a fresh
// daemon (e.g. `attach` for a session id the new daemon never knew
// about) or be a real bug.
//
// The wrapper keeps the same method signatures as `DaemonClient`, so
// `AppState` only has to switch the type -- the call sites in
// `lib.rs` do not change.

// Heuristic: is this error almost certainly a dead-daemon (TCP-level)
// error rather than an application error? We retry on connection
// refused, connection reset, DNS failures, and read/write timeouts --
// i.e. everything `reqwest` itself flags as having failed before
// getting a response. HTTP status codes (4xx/5xx) come back through
// a different path that does not include `reqwest::Error::is_*`,
// so a 404 from `attach` (session gone) is correctly *not* retried.
fn is_conn_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(e) = cause.downcast_ref::<reqwest::Error>() {
            return e.is_connect() || e.is_timeout() || e.is_request();
        }
    }
    false
}

/// Sugar over `is_conn_error` for the respawn wrapper -- the
/// handlers hold the call result in a local `let result = ...;`
/// before deciding whether to respawn, so this one-liner avoids
/// `if let Err(e) = &result { is_conn_error(e) } else { false }`
/// at every call site.
fn result_is_conn<T>(result: &Result<T, anyhow::Error>) -> bool {
    result.as_ref().err().is_some_and(is_conn_error)
}

pub struct ResilientDaemon {
    inner: Arc<tokio::sync::RwLock<Arc<DaemonClient>>>,
}

impl ResilientDaemon {
    pub fn new(client: Arc<DaemonClient>) -> Self {
        Self {
            inner: Arc::new(tokio::sync::RwLock::new(client)),
        }
    }

    /// Force a respawn right now. Used by the Tauri command that
    /// wants to recover from a stale state, e.g. after a manual
    /// "the daemon looks wedged" hotkey.
    pub async fn respawn(&self) -> Result<()> {
        let new = spawn_or_connect().await?;
        *self.inner.write().await = Arc::new(new);
        Ok(())
    }

    /// Cheap probe: returns true if the daemon currently responds to
    /// `/health`. Used by the Tauri command exposed to the UI.
    pub async fn is_alive(&self) -> bool {
        let client = self.inner.read().await.clone();
        let url = format!("{}/health", client.base_url);
        matches!(
            client.http.get(&url).send().await,
            Ok(r) if r.status().is_success()
        )
    }
}

// Hand-written respawn wrappers, one per public method. A macro
// was tempting but ran into a wall: methods take a mix of
// `&Credentials`-style references and owned `Bytes`/`Vec<...>`,
// and the retry path needs both. References can be used twice
// for free (`Copy`); owned values have to be cloned for the
// first call so the original is still in hand for the retry.
// Picking either `$arg` or `$arg.clone()` in the macro body
// broke one side or the other, so the wrappers are spelled out
// explicitly here. The shape is identical for every method:
//   1. Snapshot the current `DaemonClient`.
//   2. Call the underlying method.
//   3. On a connection-level error, respawn and retry once.
//   4. On any other error (HTTP 4xx/5xx, JSON parse, ...), return
//      the original error -- a fresh daemon would just see the
//      same application-level failure.
//
// SSE-returning methods (`open`, `attach`, `rdp_open`,
// `files_upload`, `files_download`) are wrapped in full. On
// retry, the first HTTP call repeats (so a slow SSH handshake
// gets another chance) and the SSE subscribe hits the *new*
// daemon with the new session id. A respawned daemon has no
// memory of the old session, so `attach` against a vanished
// session id returns 404 -- which is the correct outcome.
impl ResilientDaemon {
    // -- session lifecycle --
    pub async fn open(
        &self,
        spec: TransportSpec,
        cols: u16,
        rows: u16,
        creds: &terminator_core::session::Credentials,
    ) -> Result<(uuid::Uuid, SseStream<OutputEvent>)> {
        let result = {
            let client = self.inner.read().await.clone();
            client.open(spec.clone(), cols, rows, creds).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("open: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.open(spec, cols, rows, creds).await;
        }
        result
    }

    pub async fn write(&self, id: uuid::Uuid, data: Bytes) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.write(id, data.clone()).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("write: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.write(id, data).await;
        }
        result
    }

    pub async fn resize(&self, id: uuid::Uuid, cols: u16, rows: u16) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.resize(id, cols, rows).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("resize: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.resize(id, cols, rows).await;
        }
        result
    }

    pub async fn close(&self, id: uuid::Uuid) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.close(id).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("close: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.close(id).await;
        }
        result
    }

    pub async fn list_sessions(&self) -> Result<Value> {
        let result = {
            let client = self.inner.read().await.clone();
            client.list_sessions().await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("list_sessions: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.list_sessions().await;
        }
        result
    }

    pub async fn attach(&self, id: uuid::Uuid) -> Result<SseStream<OutputEvent>> {
        let result = {
            let client = self.inner.read().await.clone();
            client.attach(id).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        warn!("attach: conn error; respawning daemon");
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.attach(id).await;
        }
        result
    }

    // -- file browser + exec --
    pub async fn files_home(&self, id: uuid::Uuid) -> Result<String> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_home(id).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_home(id).await;
        }
        result
    }

    pub async fn files_list(
        &self,
        id: uuid::Uuid,
        path: &str,
    ) -> Result<terminator_core::files::Listing> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_list(id, path).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_list(id, path).await;
        }
        result
    }

    pub async fn files_mkdir(&self, id: uuid::Uuid, path: &str) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_mkdir(id, path).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_mkdir(id, path).await;
        }
        result
    }

    pub async fn files_remove(&self, id: uuid::Uuid, path: &str, is_dir: bool) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_remove(id, path, is_dir).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_remove(id, path, is_dir).await;
        }
        result
    }

    pub async fn files_rename(&self, id: uuid::Uuid, from: &str, to: &str) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_rename(id, from, to).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_rename(id, from, to).await;
        }
        result
    }

    pub async fn files_read(&self, id: uuid::Uuid, path: &str, max_bytes: usize) -> Result<String> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_read(id, path, max_bytes).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_read(id, path, max_bytes).await;
        }
        result
    }

    pub async fn files_write(&self, id: uuid::Uuid, path: &str, content: &str) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_write(id, path, content).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_write(id, path, content).await;
        }
        result
    }

    pub async fn files_search(
        &self,
        id: uuid::Uuid,
        path: &str,
        options: &terminator_core::files::SearchOptions,
    ) -> Result<Vec<terminator_core::files::FileSearchResult>> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_search(id, path, options).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_search(id, path, options).await;
        }
        result
    }

    pub async fn exec_command(
        &self,
        spec: &TransportSpec,
        command: &str,
        creds: &terminator_core::session::Credentials,
        cwd: Option<&str>,
    ) -> Result<terminator_core::session::ExecResult> {
        let result = {
            let client = self.inner.read().await.clone();
            client.exec_command(spec, command, creds, cwd).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.exec_command(spec, command, creds, cwd).await;
        }
        result
    }

    pub async fn files_upload(
        &self,
        id: uuid::Uuid,
        local_path: &str,
        remote: &str,
    ) -> Result<SseStream<TransferEvent>> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_upload(id, local_path, remote).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_upload(id, local_path, remote).await;
        }
        result
    }

    pub async fn files_download(
        &self,
        id: uuid::Uuid,
        remote: &str,
        local_path: &str,
    ) -> Result<SseStream<TransferEvent>> {
        let result = {
            let client = self.inner.read().await.clone();
            client.files_download(id, remote, local_path).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.files_download(id, remote, local_path).await;
        }
        result
    }

    // -- log file management --
    pub async fn log_dir(&self) -> Result<String> {
        let result = {
            let client = self.inner.read().await.clone();
            client.log_dir().await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.log_dir().await;
        }
        result
    }

    pub async fn list_session_logs(&self) -> Result<Value> {
        let result = {
            let client = self.inner.read().await.clone();
            client.list_session_logs().await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.list_session_logs().await;
        }
        result
    }

    pub async fn delete_session_log(&self, dir_name: &str) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.delete_session_log(dir_name).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.delete_session_log(dir_name).await;
        }
        result
    }

    pub async fn read_log_file(&self, path: &str) -> Result<String> {
        let result = {
            let client = self.inner.read().await.clone();
            client.read_log_file(path).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.read_log_file(path).await;
        }
        result
    }

    pub async fn session_log_paths(&self, id: uuid::Uuid) -> Result<Value> {
        let result = {
            let client = self.inner.read().await.clone();
            client.session_log_paths(id).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.session_log_paths(id).await;
        }
        result
    }

    pub async fn search_sessions(
        &self,
        needle: &str,
        case_sensitive: bool,
        max_per_session: usize,
    ) -> Result<Vec<Value>> {
        let result = {
            let client = self.inner.read().await.clone();
            client.search_sessions(needle, case_sensitive, max_per_session).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.search_sessions(needle, case_sensitive, max_per_session).await;
        }
        result
    }

    // -- RDP --
    pub async fn rdp_open(
        &self,
        cfg: &terminator_core::rdp::RdpConfig,
    ) -> Result<(uuid::Uuid, u16, u16, SseStream<terminator_core::rdp::RdpEvent>)> {
        let result = {
            let client = self.inner.read().await.clone();
            client.rdp_open(cfg).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.rdp_open(cfg).await;
        }
        result
    }

    pub async fn rdp_input(&self, id: uuid::Uuid, ops: Vec<terminator_core::rdp::RdpInput>) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.rdp_input(id, ops.clone()).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.rdp_input(id, ops).await;
        }
        result
    }

    pub async fn rdp_resize(&self, id: uuid::Uuid, width: u16, height: u16) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.rdp_resize(id, width, height).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.rdp_resize(id, width, height).await;
        }
        result
    }

    pub async fn rdp_close(&self, id: uuid::Uuid) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.rdp_close(id).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.rdp_close(id).await;
        }
        result
    }

    pub async fn rdp_local_clipboard(&self, id: uuid::Uuid, text: &str) -> Result<()> {
        let result = {
            let client = self.inner.read().await.clone();
            client.rdp_local_clipboard(id, text).await
        };
        if !result_is_conn(&result) {
            return result;
        }
        if let Ok(new) = spawn_or_connect().await {
            *self.inner.write().await = Arc::new(new);
            let client = self.inner.read().await.clone();
            return client.rdp_local_clipboard(id, text).await;
        }
        result
    }
}


