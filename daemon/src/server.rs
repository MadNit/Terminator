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
    routing::{get, post},
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

#[derive(Clone)]
struct AppState {
    manager: Arc<DaemonSessionManager>,
    /// Wall-clock time the daemon started. Used by the health check
    /// and any future `/stats` endpoint.
    started_at_ms: i64,
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

pub fn router(manager: Arc<DaemonSessionManager>) -> Router {
    let state = AppState {
        manager,
        started_at_ms: now_ms(),
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
        .route("/sessions/:id/exec", post(exec_command))
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
