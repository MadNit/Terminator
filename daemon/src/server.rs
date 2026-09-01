//! HTTP + SSE server. The route surface mirrors the Tauri commands
//! the UI used to call directly, so the frontend can be ported one
//! command at a time.
//!
//! Wire format:
//!   - All request/response bodies are JSON.
//!   - Output is delivered as Server-Sent Events on
//!     `GET /sessions/{id}/output`. Each event is a JSON-encoded
//!     [`OutputEvent`] from the manager. The SSE stream stays open
//!     until the client disconnects or the session exits.
//!
//! Auth: the daemon listens on `127.0.0.1` only, with an OS-assigned
//! port stored in a per-user file. No auth header is required
//! because no network listener is exposed. When the daemon ships,
//! it will refuse to start if `127.0.0.1` is unavailable (an
//! indicator the port-bind is being shadowed by something malicious).

use std::convert::Infallible;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use bytes::Bytes;
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::{info, warn};
use uuid::Uuid;

use terminator_core::session::Credentials;
use terminator_core::transport::TransportSpec;

use crate::manager::{DaemonSessionManager, OutputEvent, SessionInfo};
use crate::rdp::{DaemonRdpManager, RdpInfo};

#[derive(Clone)]
struct AppState {
    manager: Arc<DaemonSessionManager>,
    /// Live RDP sessions + per-session broadcast channels. A
    /// Tauri UI crash no longer takes the remote desktop with
    /// it; this is the same survival guarantee the PTY/SSH
    /// sessions have had since Session 1.
    rdp: Arc<DaemonRdpManager>,
    /// Wall-clock time the daemon started. Used by the health check
    /// and any future `/stats` endpoint.
    started_at_ms: i64,
    /// On-disk log directory. Owned by the daemon (the same
    /// `SessionManager` writes `.cast` / `.log` files here for
    /// every open session); the Tauri side just reads the path
    /// back when it needs to surface log file management to the
    /// user.
    log_dir: Arc<std::path::PathBuf>,
}

#[derive(Debug, Deserialize)]
struct OpenRequest {
    spec: TransportSpec,
    cols: u16,
    rows: u16,
    /// SSH password. Used when `spec.kind == "ssh"` and
    /// `spec.auth.method == "password"`. Local sessions ignore it.
    #[serde(default)]
    password: Option<String>,
    /// Passphrase for the SSH key file. Used when
    /// `spec.auth.method == "key"`. The key file path itself is
    /// already in the spec, so we only carry the secret across
    /// the wire.
    #[serde(default)]
    key_passphrase: Option<String>,
    /// Same two fields, but for the jump host. A `None` jump host
    /// means these are ignored.
    #[serde(default)]
    jump_password: Option<String>,
    #[serde(default)]
    jump_key_passphrase: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpenResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct WriteRequest {
    /// Base64 because SSE / JSON for binary would need re-framing
    /// anyway; doing the encode here means the body shape matches
    /// what the Tauri side already sent.
    data_b64: String,
}

#[derive(Debug, Deserialize)]
struct ResizeRequest {
    cols: u16,
    rows: u16,
}

pub fn router(
    manager: Arc<DaemonSessionManager>,
    rdp: Arc<DaemonRdpManager>,
    log_dir: std::path::PathBuf,
) -> Router {
    let state = AppState {
        manager,
        rdp,
        started_at_ms: now_ms(),
        log_dir: Arc::new(log_dir),
    };
    Router::new()
        .route("/health", get(health))
        .route("/sessions", get(list_sessions).post(open_session))
        .route("/sessions/:id", get(get_session).delete(close_session))
        .route("/sessions/:id/output", get(session_output_sse))
        .route("/sessions/:id/scrollback", get(session_scrollback))
        .route("/sessions/:id/input", post(write_session))
        .route("/sessions/:id/resize", post(resize_session))
        // File browser + one-shot exec. These complete the
        // migration that started in commit 258e4cf: every
        // session-related operation now goes through the
        // daemon, so the Tauri side is a pure proxy.
        .route("/sessions/:id/files/home", get(files_home))
        .route("/sessions/:id/files/list", get(files_list))
        .route("/sessions/:id/files/mkdir", post(files_mkdir))
        .route("/sessions/:id/files/remove", post(files_remove))
        .route("/sessions/:id/files/rename", post(files_rename))
        .route("/sessions/:id/files/read", get(files_read))
        .route("/sessions/:id/files/write", post(files_write))
        .route("/sessions/:id/files/upload", post(files_upload))
        .route("/sessions/:id/files/download", post(files_download))
        .route("/sessions/:id/files/search", post(files_search))
        .route("/sessions/:id/exec", post(exec_command))
        // Log file management. Lets the Tauri side delete
        // archived sessions and read the on-disk `.cast` /
        // `.log` files without ever knowing where they live
        // on disk.
        .route("/log_dir", get(log_dir_route))
        .route("/session_logs", get(session_logs_route))
        .route("/session_logs/:dir_name", delete(delete_session_log_route))
        .route("/log_file", get(log_file_route))
        .route("/sessions/:id/logs", get(session_log_paths_route))
        // RDP. Same "daemon owns the live session" pattern
        // as the PTY/SSH routes above; a Tauri UI crash no
        // longer takes the remote desktop with it. The
        // `Output` SSE stream is a stream of `RdpEvent`s
        // (`Frame { rgba }` / `Resized` / `Disconnected`)
        // rather than raw bytes.
        .route("/rdp", get(rdp_list).post(rdp_open))
        .route("/rdp/:id", get(rdp_get).delete(rdp_close))
        .route("/rdp/:id/output", get(rdp_output_sse))
        .route("/rdp/:id/input", post(rdp_input))
        .route("/rdp/:id/resize", post(rdp_resize))
        // Local clipboard updates. The Tauri side calls this
        // when the user copies something locally and the RDP
        // pane has focus -- the daemon's CLIPRDR backend
        // re-advertises the new text the next time the server
        // asks for our format list.
        .route("/rdp/:id/clipboard", post(rdp_local_clipboard))
        .with_state(state)
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            let _ = s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => info!("ctrl-c received, shutting down"),
        _ = terminate => info!("SIGTERM received, shutting down"),
    }
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "started_at_ms": state.started_at_ms,
        "now_ms": now_ms(),
    }))
}

async fn list_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let list = state.manager.list().await;
    Json(list)
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionInfo>, StatusCode> {
    let id = parse_id(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    match state.manager.core().spec(id) {
        Some(spec) => Ok(Json(SessionInfo {
            id: id.to_string(),
            spec,
            alive: state.manager.is_alive(id).await,
            opened_at_ms: 0, // see TODO below
        })),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn open_session(
    State(state): State<AppState>,
    Json(req): Json<OpenRequest>,
) -> Result<(StatusCode, Json<OpenResponse>), (StatusCode, String)> {
    let creds = Credentials {
        secret: req.password,
        key_passphrase: req.key_passphrase,
        jump_secret: req.jump_password,
        jump_key_passphrase: req.jump_key_passphrase,
    };
    let (id, _rx) = state
        .manager
        .open(req.spec, req.cols, req.rows, creds)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("open failed: {e}")))?;
    Ok((
        StatusCode::CREATED,
        Json(OpenResponse {
            id: id.to_string(),
        }),
    ))
}

async fn write_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<WriteRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let bytes = base64_decode(&req.data_b64)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("bad base64: {e}")))?;
    state
        .manager
        .write(id, bytes)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("write failed: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn resize_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ResizeRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .manager
        .resize(id, req.cols, req.rows)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("resize failed: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn close_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .manager
        .close(id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("close failed: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn session_output_sse(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    let id = parse_id(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let rx = state.manager.subscribe(id).await.map_err(|_| {
        // Either unknown id, or the per-session channel has been
        // torn down. Both look like "no such session" to the client.
        warn!(%id, "subscribe to unknown session");
        StatusCode::NOT_FOUND
    })?;
    // Snapshot the scrollback buffer BEFORE the stream starts so
    // the first thing the client sees is the most recent ~1 MB
    // of output, not a blank terminal. The buffer only grows
    // while the live broadcast is also running, so anything that
    // arrives between the snapshot and the first recv() below
    // is still in the broadcast (no gap).
    let replay: Vec<OutputEvent> = state
        .manager
        .scrollback(id)
        .await
        .into_iter()
        .map(|chunk| OutputEvent::Output {
            data: base64_encode(&chunk),
        })
        .collect();
    let stream = broadcast_to_sse(rx, replay);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))))
}

async fn session_scrollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let id = parse_id(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    // Same encoding the SSE replay uses: an array of base64
    // chunks, in arrival order. Empty array for unknown sessions
    // so a UI reattach can treat "no scrollback" and "session
    // gone" as two distinct conditions.
    let chunks: Vec<String> = state
        .manager
        .scrollback(id)
        .await
        .iter()
        .map(|c| base64_encode(c))
        .collect();
    Ok(Json(serde_json::json!({ "chunks": chunks })))
}

fn broadcast_to_sse(
    mut rx: tokio::sync::broadcast::Receiver<OutputEvent>,
    replay: Vec<OutputEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        // Drain the scrollback buffer first. These are the
        // events the client "missed" while the UI was gone;
        // yielding them in order gives the terminal a sensible
        // view of "what was on screen" before live events take
        // over.
        for ev in replay {
            let payload = serde_json::to_string(&ev).unwrap_or_default();
            yield Ok(Event::default().data(payload));
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_default();
                    yield Ok(Event::default().data(payload));
                    if matches!(ev, OutputEvent::Exit) {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // A slow HTTP client missed some bytes. We
                    // can't replay them; just keep going. The
                    // client can reattach from the start if it
                    // needs every byte (the ring-buffer Session
                    // 2 adds makes that cheap).
                    warn!("SSE client lagged; events dropped");
                }
                Err(RecvError::Closed) => {
                    // Sender went away -- the session is gone.
                    break;
                }
            }
        }
    }
}

// ============================================================
// File browser + exec endpoints. These exist so the Tauri
// side doesn't need a second hop through a stale helper
// SessionManager: every file operation runs in the daemon
// process that owns the SSH connection, which means SFTP
// reads/writes share a single multiplexed channel with the
// live terminal session, and a one-shot `exec_command` for
// RemoteEditorModal doesn't have to reconnect to the host.
// ============================================================

async fn files_home(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<String, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| {
        warn!(%id, "files_home: {e}");
        file_status_for(&e)
    })?;
    fs.home().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

#[derive(serde::Deserialize)]
struct FilesListQuery {
    path: String,
}

async fn files_list(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<FilesListQuery>,
) -> Result<Json<terminator_core::files::Listing>, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.list(&q.path)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

#[derive(serde::Deserialize)]
struct FilesMkdirBody {
    path: String,
}

async fn files_mkdir(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesMkdirBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.mkdir(&body.path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct FilesRemoveBody {
    path: String,
    is_dir: bool,
}

async fn files_remove(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesRemoveBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.remove(&body.path, body.is_dir)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct FilesRenameBody {
    from: String,
    to: String,
}

async fn files_rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesRenameBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.rename(&body.from, &body.to)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct FilesReadQuery {
    path: String,
    /// Cap on how many bytes we'll read. Defaults to 10 MiB,
    /// which matches what the Tauri side passed before the
    /// daemon existed. The Tauri side passes an explicit
    /// value when reading larger files (e.g. the Mini-IDE
    /// editor for remote files).
    #[serde(default = "default_files_max")]
    max: usize,
}

fn default_files_max() -> usize {
    10 * 1024 * 1024
}

async fn files_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<FilesReadQuery>,
) -> Result<String, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.read_text(&q.path, q.max)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

#[derive(serde::Deserialize)]
struct FilesWriteBody {
    path: String,
    content: String,
}

async fn files_write(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesWriteBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.write_text(&body.path, &body.content)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(StatusCode::NO_CONTENT)
}

// -- file transfer (local <-> remote) --------------------------------
//
// `local_path` is on the SAME machine as the daemon: the Tauri app
// is the only client on 127.0.0.1, and both processes resolve the
// user data dir identically. The Tauri side is responsible for any
// staging/validation (e.g. the `safe_file_name` helper on the drag
// drop path); the daemon trusts the path it is given.
//
// Progress events are NOT streamed: the transfer is one HTTP
// round-trip, so the channel-based progress UI the Tauri side had
// before will only see the final `Done` (or `Failed`) event. The
// `RemoteFs` impls still report progress to the core's `ProgressSink`
// if one is supplied; we just don't surface it over HTTP yet.

#[derive(serde::Deserialize)]
struct FilesUploadBody {
    local_path: String,
    remote: String,
}

#[derive(serde::Serialize)]
struct FilesUploadResponse {
    bytes: u64,
}

async fn files_upload(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesUploadBody>,
) -> Result<Json<FilesUploadResponse>, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    let local = std::path::PathBuf::from(&body.local_path);
    // `RemoteFs::upload` does its own chunked read; for v1 we
    // pass a no-op progress sink. Per-byte progress over HTTP
    // is a separate change.
    let sink: terminator_core::files::ProgressSink = std::sync::Arc::new(|_| {});
    let bytes = fs
        .upload(&local, &body.remote, sink)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(FilesUploadResponse { bytes }))
}

#[derive(serde::Deserialize)]
struct FilesDownloadBody {
    remote: String,
    local_path: String,
}

async fn files_download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesDownloadBody>,
) -> Result<Json<FilesUploadResponse>, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    let local = std::path::PathBuf::from(&body.local_path);
    let sink: terminator_core::files::ProgressSink = std::sync::Arc::new(|_| {});
    let bytes = fs
        .download(&body.remote, &local, sink)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(FilesUploadResponse { bytes }))
}

#[derive(serde::Deserialize)]
struct FilesSearchBody {
    path: String,
    options: terminator_core::files::SearchOptions,
}

async fn files_search(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<FilesSearchBody>,
) -> Result<
    Json<Vec<terminator_core::files::FileSearchResult>>,
    (StatusCode, String),
> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let fs = state.manager.files(id).await.map_err(|e| file_status_for(&e))?;
    fs.search(&body.path, &body.options)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

#[derive(serde::Deserialize)]
struct ExecBody {
    spec: TransportSpec,
    command: String,
    password: Option<String>,
    key_passphrase: Option<String>,
    jump_password: Option<String>,
    jump_key_passphrase: Option<String>,
    cwd: Option<String>,
}

async fn exec_command(
    State(state): State<AppState>,
    Json(body): Json<ExecBody>,
) -> Result<Json<terminator_core::session::ExecResult>, (StatusCode, String)> {
    // Same credential-resolution rules as open_session:
    // the daemon knows nothing about the user's keychain, so
    // the Tauri side is expected to send resolved secrets
    // across. We never persist any of these fields.
    let creds = terminator_core::session::Credentials {
        secret: body.password,
        key_passphrase: body.key_passphrase,
        jump_secret: body.jump_password,
        jump_key_passphrase: body.jump_key_passphrase,
    };
    let cwd = body.cwd.as_deref();
    state
        .manager
        .exec_command(&body.spec, &body.command, creds, cwd)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))
}

/// Pick a sensible HTTP status from a `core::SessionManager`
/// error. The most common case is "no such session", which
/// surfaces as `anyhow!("unknown session {id}")`; everything
/// else is a 500. Centralizing this here keeps the per-route
/// handlers from each having to re-derive the mapping.
fn file_status_for(e: &anyhow::Error) -> (StatusCode, String) {
    let s = format!("{e:#}");
    if s.contains("unknown session") {
        (StatusCode::NOT_FOUND, s)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, s)
    }
}

fn parse_id(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| anyhow::anyhow!("bad uuid {s}: {e}"))
}

fn base64_decode(s: &str) -> Result<Bytes> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map(Bytes::from)
        .map_err(|e| anyhow::anyhow!("base64 decode: {e}"))
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// RDP
// ---------------------------------------------------------------------------
//
// `RdpEvent` already has the same `#[serde(tag = "type", rename_all =
// "camelCase")]` shape the PTY `OutputEvent` uses, so the SSE wire
// format is identical: `data: {"type":"frame",...}\n\n`. The Tauri
// side deserializes the same way, just into `RdpEvent` instead of
// `OutputEvent`.

#[derive(Debug, serde::Serialize)]
struct RdpOpened {
    id: String,
    width: u16,
    height: u16,
}

async fn rdp_open(
    State(state): State<AppState>,
    Json(cfg): Json<terminator_core::rdp::RdpConfig>,
) -> Result<Json<RdpOpened>, (StatusCode, String)> {
    let user = cfg.user.clone();
    let host = cfg.host.clone();
    let port = cfg.port;
    let (id, width, height, _rx) = state
        .rdp
        .open(cfg)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    tracing::info!(%id, user, host, port, "rdp session open");
    Ok(Json(RdpOpened {
        id: id.to_string(),
        width,
        height,
    }))
}

async fn rdp_list(State(state): State<AppState>) -> Json<Vec<RdpInfo>> {
    Json(state.rdp.list().await)
}

async fn rdp_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RdpInfo>, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .rdp
        .get(id)
        .await
        .map(Json)
        .map_err(|e| rdp_status_for(&e))
}

async fn rdp_close(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .rdp
        .close(id)
        .await
        .map_err(|e| rdp_status_for(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rdp_input(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(ops): Json<Vec<terminator_core::rdp::RdpInput>>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .rdp
        .input(id, ops)
        .await
        .map_err(|e| rdp_status_for(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct RdpResizeBody {
    width: u16,
    height: u16,
}

async fn rdp_resize(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RdpResizeBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .rdp
        .resize(id, body.width, body.height)
        .await
        .map_err(|e| rdp_status_for(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct RdpLocalClipboardBody {
    text: String,
}

async fn rdp_local_clipboard(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RdpLocalClipboardBody>,
) -> Result<StatusCode, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    state
        .rdp
        .set_local_clipboard(id, body.text)
        .await
        .map_err(|e| rdp_status_for(&e))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rdp_output_sse(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, Infallible>>>,
    (StatusCode, String),
> {
    let id = parse_id(&id).map_err(|_| (StatusCode::BAD_REQUEST, "bad uuid".into()))?;
    let rx = state
        .rdp
        .subscribe(id)
        .await
        .map_err(|e| rdp_status_for(&e))?;
    Ok(Sse::new(rdp_broadcast_to_sse(rx))
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))))
}

/// RDP has no scrollback buffer (the server resends frames on
/// reattach), so the SSE stream is just live events.
fn rdp_broadcast_to_sse(
    mut rx: tokio::sync::broadcast::Receiver<terminator_core::rdp::RdpEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_default();
                    yield Ok(Event::default().data(payload));
                }
                Err(RecvError::Lagged(_)) => {
                    // A slow webview dropped a frame. The next
                    // `Frame` we receive (typically a few ms
                    // later) repaints the whole dirty region
                    // and catches the renderer up, so we just
                    // log and continue.
                    warn!("rdp SSE client lagged; frame dropped");
                }
                Err(RecvError::Closed) => break,
            }
        }
    }
}

/// Pick a sensible HTTP status from an `RdpManager` error.
/// Currently the only one we look for is "no such rdp
/// session" -- everything else is a 500.
fn rdp_status_for(e: &anyhow::Error) -> (StatusCode, String) {
    let s = format!("{e:#}");
    if s.contains("no such rdp session") {
        (StatusCode::NOT_FOUND, s)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, s)
    }
}

// ---------------------------------------------------------------------------
// Log file management
// ---------------------------------------------------------------------------
//
// These routes existed as direct filesystem reads in the Tauri side
// (against the stale `state.helpers` SessionManager) and moved to
// the daemon so the on-disk log directory is owned by exactly one
// process. The Tauri side is now a pure proxy.

/// One entry in the `GET /session_logs` response. Mirrors
/// `SessionLogItem` on the Tauri side.
#[derive(Debug, Serialize)]
struct SessionLogItem {
    id: String,
    dir_name: String,
    timestamp: u64,
    cast_path: String,
    plain_path: String,
    plain_size: u64,
    cast_size: u64,
}

async fn log_dir_route(State(state): State<AppState>) -> String {
    state.log_dir.to_string_lossy().into_owned()
}

async fn session_logs_route(State(state): State<AppState>) -> Json<Vec<SessionLogItem>> {
    let log_dir = state.log_dir.clone();
    // Spawn-block: directory walk can block on a stalled
    // network mount and we don't want to freeze unrelated
    // sessions.
    let items = tokio::task::spawn_blocking(move || walk_log_dir(&log_dir))
        .await
        .unwrap_or_default();
    Json(items)
}

fn walk_log_dir(log_dir: &std::path::Path) -> Vec<SessionLogItem> {
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let cast_path = path.join("session.cast");
        let plain_path = path.join("session.log");
        let cast_size = std::fs::metadata(&cast_path).map(|m| m.len()).unwrap_or(0);
        let plain_size = std::fs::metadata(&plain_path).map(|m| m.len()).unwrap_or(0);
        let parts: Vec<&str> = dir_name.splitn(2, '-').collect();
        let timestamp = parts
            .first()
            .and_then(|ts| ts.parse::<u64>().ok())
            .unwrap_or(0);
        items.push(SessionLogItem {
            id: dir_name.clone(),
            dir_name,
            timestamp,
            cast_path: cast_path.to_string_lossy().to_string(),
            plain_path: plain_path.to_string_lossy().to_string(),
            plain_size,
            cast_size,
        });
    }
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    items
}

async fn delete_session_log_route(
    State(state): State<AppState>,
    Path(dir_name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let log_dir = state.log_dir.clone();
    // The Tauri side used to call `safe_file_name` on the
    // directory name before deleting; the same regex (only
    // `[A-Za-z0-9_-]`) is applied here so the path can never
    // escape the log directory via `..` traversal.
    let safe = safe_log_dir_name(&dir_name)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    tokio::task::spawn_blocking(move || {
        let path = log_dir.join(&safe);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join error: {e}")))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

/// Allow only `[A-Za-z0-9_-]`, no leading dot, no path
/// separator. Mirrors the Tauri side's `safe_file_name` helper.
fn safe_log_dir_name(name: &str) -> std::result::Result<String, String> {
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("invalid directory name: {name}"));
    }
    Ok(name.to_string())
}

#[derive(Debug, Deserialize)]
struct LogFileQuery {
    path: String,
}

async fn log_file_route(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LogFileQuery>,
) -> Result<String, (StatusCode, String)> {
    let log_dir = state.log_dir.clone();
    let path = q.path;
    tokio::task::spawn_blocking(move || read_log_file(&log_dir, &path))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("join error: {e}")))?
}

fn read_log_file(
    log_dir: &std::path::Path,
    path: &str,
) -> std::result::Result<String, (StatusCode, String)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return Err((StatusCode::NOT_FOUND, format!("File not found: {path}")));
    }
    // Reject any path that is not inside the daemon's log
    // directory. The Tauri side already passes paths the
    // daemon returned, but a hostile or buggy caller could
    // still try to read arbitrary files.
    let canonical_log_dir = match std::fs::canonicalize(log_dir) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("canonicalize log dir: {e}"),
            ))
        }
    };
    let canonical_path = match std::fs::canonicalize(p) {
        Ok(d) => d,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("canonicalize path: {e}"),
            ))
        }
    };
    if !canonical_path.starts_with(&canonical_log_dir) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("path is outside the log directory: {path}"),
        ));
    }
    // Helper: convert a `String` error from `map_err(|e| e.to_string())`
    // into the `(StatusCode, String)` the handler returns.
    let s500 = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let meta = std::fs::metadata(&canonical_path).map_err(|e| e.to_string()).map_err(s500)?;
    if meta.len() > 5 * 1024 * 1024 {
        use std::io::Read;
        let mut file = std::fs::File::open(&canonical_path)
            .map_err(|e| e.to_string())
            .map_err(s500)?;
        let mut buffer = vec![0u8; 5 * 1024 * 1024];
        let n = file.read(&mut buffer).map_err(|e| e.to_string()).map_err(s500)?;
        let mut s = String::from_utf8_lossy(&buffer[..n]).to_string();
        s.push_str("\n\n... [Log truncated at 5MB] ...");
        return Ok(s);
    }
    std::fs::read_to_string(&canonical_path)
        .map_err(|e| e.to_string())
        .map_err(s500)
}

async fn session_log_paths_route(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = parse_id(&id).map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let p = state
        .manager
        .session_logs(id)
        .await
        .map_err(|e| file_status_for(&e))?;
    Ok(Json(serde_json::json!({ "cast": p.cast, "plain": p.plain })))
}
