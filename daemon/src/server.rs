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
use std::net::SocketAddr;
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
        .route("/sessions/:id/input", post(write_session))
        .route("/sessions/:id/resize", post(resize_session))
        .with_state(state)
}

pub async fn bind_and_serve(
    manager: Arc<DaemonSessionManager>,
    addr: SocketAddr,
) -> Result<()> {
    let app = router(manager);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("daemon listening on {}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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
    let stream = broadcast_to_sse(rx);
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))))
}

fn broadcast_to_sse(
    mut rx: tokio::sync::broadcast::Receiver<OutputEvent>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
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

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
