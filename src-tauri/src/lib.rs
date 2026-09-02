//! Tauri adapter.
//!
//! Translates IPC into `terminator-core` calls. No session logic lives here by
//! design — see the note in Cargo.toml.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;
use tauri::{ipc::Channel, Manager, State};
use terminator_core::{
    known_hosts::{KnownHostEntry, KnownHostsManager},
    rdp::{RdpConfig, RdpEvent, RdpInput},
    secrets::{Backend, Secrets},
    session::Credentials,
    store::Store,
    transport::pty::{discover_shells, ShellOption},
    TransportSpec,
    TunnelConfig, TunnelManager, TunnelStatus,
};
use uuid::Uuid;

mod daemon_client;
use daemon_client::{DaemonClient, OutputEvent as DaemonOutputEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;

/// Events pushed to the webview for a single session.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum SessionEvent {
    /// Base64 because Tauri IPC serializes `Vec<u8>` as a JSON number array,
    /// which is far larger and slower to parse at terminal throughput.
    Output {
        data: String,
    },
    Exit,
}

struct AppState {
    /// Long-lived HTTP client to `terminator-daemon`. The daemon owns
    /// every PTY/SSH/RDP process and the on-disk log directory;
    /// the Tauri process is a thin proxy. Closing the Tauri app
    /// does NOT terminate the daemon, which is the entire point
    /// of having a daemon.
    daemon: Arc<DaemonClient>,
    store: Store,
    /// Arc so blocking keychain work can be moved onto a blocking thread.
    secrets: Arc<Secrets>,
    tunnels: TunnelManager,
    known_hosts_path: std::path::PathBuf,
}

/// Run a blocking secret-store operation off the async runtime.
///
/// Keychain reads can raise a modal OS authorization dialog and block for as
/// long as the user takes to answer it; doing that on a tokio worker stalls
/// every other session's I/O. Argon2 unlocking is likewise CPU-bound.
async fn blocking_secrets<T, F>(secrets: &Arc<Secrets>, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&Secrets) -> anyhow::Result<T> + Send + 'static,
{
    let s = secrets.clone();
    tokio::task::spawn_blocking(move || f(&s))
        .await
        .map_err(e)?
        .map_err(e)
}

fn e<T: std::fmt::Display>(err: T) -> String {
    err.to_string()
}

#[tauri::command]
async fn open_session(
    state: State<'_, AppState>,
    spec: TransportSpec,
    cols: u16,
    rows: u16,
    // Keychain entry holding the password, if this profile uses one.
    secret_ref: Option<String>,
    // One-shot password for "don't remember" connections. Takes precedence
    // over secret_ref and is never written to the keychain or vault.
    password: Option<String>,
    // Jump host credential refs / passwords if ProxyJump is configured
    jump_secret_ref: Option<String>,
    jump_password: Option<String>,
    channel: Channel<SessionEvent>,
) -> Result<String, String> {
    tracing::info!("open_session (via daemon): {:?} {cols}x{rows}", spec);

    // Resolve all four credential fields up front. Order of
    // preference is the same one `core::session` used: explicit
    // one-shot secret > keychain reference > None. We resolve
    // even for non-SSH specs because resolving is cheap and the
    // daemon will simply ignore fields it doesn't need.
    let mut creds = Credentials::default();
    if let Some(pw) = password {
        creds.secret = Some(pw);
    } else if let Some(r) = secret_ref {
        // `blocking_secrets` flattens to `Result<Option<String>, String>`
        // (the outer is the spawn-blocking dispatch; the inner
        // Option is the lookup outcome). Missing ref -> None;
        // lookup error -> propagate via `?`. The daemon will then
        // try public-key / agent auth if the SSH method allows it.
        creds.secret = blocking_secrets(&state.secrets, move |s| s.get(&r)).await?;
    }
    if let Some(pw) = jump_password {
        creds.jump_secret = Some(pw);
    } else if let Some(r) = jump_secret_ref {
        creds.jump_secret = blocking_secrets(&state.secrets, move |s| s.get(&r)).await?;
    }    let (id, mut sse) = state
        .daemon
        .open(spec, cols, rows, &creds)
        .await
        .inspect_err(|err| tracing::error!("daemon open_session failed: {err:#}"))
        .map_err(e)?;

    // Drain the SSE stream into the Tauri Channel. We don't await
    // this task; the channel keeps the receiver alive, and the
    // stream ends naturally when the daemon sends `Exit` or
    // the client disconnects.
    let out_ch = channel.clone();
    tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(item) = sse.next().await {
            match item {
                Ok(DaemonOutputEvent::Output { data }) => {
                    if out_ch.send(SessionEvent::Output { data }).is_err() {
                        break; // frontend dropped its subscription
                    }
                }
                Ok(DaemonOutputEvent::Exit) => {
                    let _ = out_ch.send(SessionEvent::Exit);
                    break;
                }
                Err(e) => {
                    tracing::warn!("SSE stream error: {e}");
                    break;
                }
            }
        }
    });

    tracing::info!("session {id} open (via daemon)");
    Ok(id.to_string())
}

/// Surfaces webview errors into the Rust log. Without this, a JS exception in
/// a release build is completely invisible.
#[tauri::command]
async fn log_frontend(level: String, message: String) {
    match level.as_str() {
        "error" => tracing::error!(target: "frontend", "{message}"),
        "warn" => tracing::warn!(target: "frontend", "{message}"),
        _ => tracing::info!(target: "frontend", "{message}"),
    }
}

/// Live sessions the daemon is still hosting, plus their
/// spec. Returned as a free-form JSON value because the
/// `TransportSpec` shape on the wire is what the frontend
/// already understands for `openSession`; reusing the type
/// here would force a stricter schema than the rest of the
/// app uses for the same concept.
#[tauri::command]
async fn list_sessions(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    state
        .daemon
        .list_sessions()
        .await
        .map_err(e)
}

/// Reattach to a session the daemon is already hosting. The
/// daemon replays the last ~1 MB of scrollback, then yields
/// live events through the same Tauri Channel shape `open_session`
/// uses, so the frontend can re-use its existing `OpenSession`
/// flow against the returned id.
#[tauri::command]
async fn attach_session(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
    channel: Channel<SessionEvent>,
) -> Result<String, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    tracing::info!("attach_session: {id} {cols}x{rows}");

    // Send the current terminal size so the reattached view
    // doesn't end up with the wrong dimensions the daemon has
    // been using while we were gone. If the session's process
    // has exited, the resize is a no-op and we proceed.
    state.daemon.resize(id, cols, rows).await.map_err(e)?;

    let mut sse = state
        .daemon
        .attach(id)
        .await
        .inspect_err(|err| tracing::error!("daemon attach_session failed: {err:#}"))
        .map_err(e)?;

    let out_ch = channel.clone();
    tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(item) = sse.next().await {
            match item {
                Ok(DaemonOutputEvent::Output { data }) => {
                    if out_ch.send(SessionEvent::Output { data }).is_err() {
                        break; // frontend dropped its subscription
                    }
                }
                Ok(DaemonOutputEvent::Exit) => {
                    let _ = out_ch.send(SessionEvent::Exit);
                    break;
                }
                Err(e) => {
                    tracing::warn!("SSE stream error: {e}");
                    break;
                }
            }
        }
    });

    Ok(id.to_string())
}

#[tauri::command]
async fn write_session(state: State<'_, AppState>, id: String, data: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let bytes = B64.decode(data.as_bytes()).map_err(e)?;
    state.daemon.write(id, Bytes::from(bytes)).await.map_err(e)
}

#[tauri::command]
async fn resize_session(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.resize(id, cols, rows).await.map_err(e)
}

#[tauri::command]
async fn close_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.close(id).await.map_err(e)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionLogItem {
    id: String,
    dir_name: String,
    timestamp: u64,
    cast_path: String,
    plain_path: String,
    plain_size: u64,
    cast_size: u64,
}

#[tauri::command]
async fn list_session_logs(state: State<'_, AppState>) -> Result<Vec<SessionLogItem>, String> {
    let value = state.daemon.list_session_logs().await.map_err(e)?;
    serde_json::from_value(value).map_err(e)
}

#[tauri::command]
async fn read_log_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<String, String> {
    // Routed through the daemon so the path stays validated:
    // the daemon rejects any path outside its log directory.
    state.daemon.read_log_file(&path).await.map_err(e)
}

#[tauri::command]
async fn delete_session_log(
    state: State<'_, AppState>,
    dir_name: String,
) -> Result<(), String> {
    state.daemon.delete_session_log(&dir_name).await.map_err(e)
}

#[tauri::command]
async fn session_logs(state: State<'_, AppState>, id: String) -> Result<serde_json::Value, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.session_log_paths(id).await.map_err(e)
}

#[tauri::command]
async fn log_dir(state: State<'_, AppState>) -> Result<String, String> {
    state.daemon.log_dir().await.map_err(e)
}

#[tauri::command]
async fn list_profiles(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let p = state.store.list_profiles().map_err(e)?;
    serde_json::to_value(p).map_err(e)
}

#[tauri::command]
async fn save_profile(
    state: State<'_, AppState>,
    name: String,
    group: Option<String>,
    spec: serde_json::Value,
) -> Result<String, String> {
    state
        .store
        .save_profile(&name, group.as_deref(), &spec)
        .map_err(e)
}

#[tauri::command]
async fn update_profile(
    state: State<'_, AppState>,
    id: String,
    name: String,
    group: Option<String>,
    spec: serde_json::Value,
) -> Result<(), String> {
    state
        .store
        .update_profile(&id, &name, group.as_deref(), &spec)
        .map_err(e)
}

#[tauri::command]
async fn delete_profile(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete_profile(&id).map_err(e)
}

#[tauri::command]
async fn set_secret(state: State<'_, AppState>, key: String, value: String) -> Result<(), String> {
    blocking_secrets(&state.secrets, move |s| s.set(&key, &value)).await
}

/// Removes a stored credential. Deleting a profile without this would leave
/// its password behind in the keychain or vault forever, with nothing in the
/// UI still referencing it.
#[tauri::command]
async fn delete_secret(state: State<'_, AppState>, key: String) -> Result<(), String> {
    blocking_secrets(&state.secrets, move |s| s.delete(&key)).await
}

/// Moves a credential to a new key, for when editing a profile changes the
/// user@host:port the key is derived from.
///
/// Done in the backend rather than as a frontend get/set pair so the plaintext
/// password never has to cross into the webview.
#[tauri::command]
async fn rename_secret(state: State<'_, AppState>, from: String, to: String) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    blocking_secrets(&state.secrets, move |s| {
        if let Some(v) = s.get(&from)? {
            s.set(&to, &v)?;
            s.delete(&from)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
async fn session_commands(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, String> {
    let rows = state.store.session_commands(&id).map_err(e)?;
    Ok(serde_json::json!(rows
        .into_iter()
        .map(|(c, x, d)| serde_json::json!({
            "command": c,
            "exitCode": x,
            "durationMs": d,
        }))
        .collect::<Vec<_>>()))
}

#[tauri::command]
async fn search_commands(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    let rows = state
        .store
        .search_commands(&query, limit.unwrap_or(50))
        .map_err(e)?;
    Ok(serde_json::json!(rows
        .into_iter()
        .map(|(c, x)| serde_json::json!({ "command": c, "exitCode": x }))
        .collect::<Vec<_>>()))
}

/// Whether a credential is already stored, so the UI can prompt for it up
/// front instead of letting the connection fail with an auth error.
#[tauri::command]
async fn has_secret(key: String, state: State<'_, AppState>) -> Result<bool, String> {
    blocking_secrets(&state.secrets, move |s| Ok(s.get(&key)?.is_some())).await
}

/// Whether the UI must collect a passphrase before secrets can be used, and
/// whether a vault already exists (i.e. unlock vs. first-time setup).
#[derive(serde::Serialize)]
struct VaultStatus {
    backend: String,
    locked: bool,
    initialized: bool,
}

#[tauri::command]
async fn vault_status(state: State<'_, AppState>) -> Result<VaultStatus, String> {
    Ok(VaultStatus {
        backend: match state.secrets.backend() {
            Backend::Keychain => "keychain".into(),
            Backend::File => "file".into(),
        },
        locked: state.secrets.is_locked(),
        initialized: state.secrets.vault_exists(),
    })
}

#[tauri::command]
async fn unlock_vault(passphrase: String, state: State<'_, AppState>) -> Result<(), String> {
    // Never log the passphrase, nor the reason -- "wrong passphrase" in a log
    // is a hint worth denying an attacker with log access.
    blocking_secrets(&state.secrets, move |s| s.unlock(&passphrase)).await
}

#[tauri::command]
async fn lock_vault(state: State<'_, AppState>) -> Result<(), String> {
    state.secrets.lock();
    Ok(())
}

#[tauri::command]
async fn change_vault_passphrase(
    passphrase: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    blocking_secrets(&state.secrets, move |s| s.change_passphrase(&passphrase)).await
}

/// Surfaced so the UI can warn when secrets fall back to file storage.
#[tauri::command]
async fn secrets_backend(state: State<'_, AppState>) -> Result<String, String> {
    Ok(match state.secrets.backend() {
        Backend::Keychain => "keychain".into(),
        Backend::File => "file".into(),
    })
}

/// Scan the well-known install locations and `$PATH` for usable shells so the
/// New Connection dialog can offer them as a dropdown (default = first hit,
/// i.e. PowerShell on Windows, $SHELL elsewhere). Pure read -- no state.
#[tauri::command]
async fn list_local_shells() -> Result<Vec<ShellOption>, String> {
    Ok(discover_shells())
}

/// The OSC 133 snippet, so the UI can offer to install it into the user's
/// shell rc (or inject it over SSH later).
#[tauri::command]
async fn shell_integration_snippet() -> Result<String, String> {
    Ok(terminator_core::OSC133_BASH_ZSH.to_string())
}

// ---------------------------------------------------------------------------
// SSH Port Forwarding & Tunnels
// ---------------------------------------------------------------------------

#[tauri::command]
async fn list_tunnels(state: State<'_, AppState>) -> Result<Vec<TunnelConfig>, String> {
    state.store.list_tunnels().map_err(e)
}

#[tauri::command]
async fn save_tunnel(state: State<'_, AppState>, config: TunnelConfig) -> Result<(), String> {
    state.store.save_tunnel(&config).map_err(e)
}

#[tauri::command]
async fn delete_tunnel(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.tunnels.stop_tunnel(&id).await.ok();
    state.store.delete_tunnel(&id).map_err(e)
}

#[tauri::command]
async fn active_tunnels(state: State<'_, AppState>) -> Result<Vec<TunnelStatus>, String> {
    Ok(state.tunnels.list_active().await)
}

// ---------------------------------------------------------------------------
// Snippets Library
// ---------------------------------------------------------------------------


#[tauri::command]
async fn list_known_hosts(state: State<'_, AppState>) -> Result<Vec<KnownHostEntry>, String> {
    KnownHostsManager::list_from_path(&state.known_hosts_path).map_err(e)
}

#[tauri::command]
async fn delete_known_host(
    state: State<'_, AppState>,
    line_number: usize,
    host_pattern: String,
) -> Result<(), String> {
    KnownHostsManager::delete_entry(&state.known_hosts_path, line_number, &host_pattern).map_err(e)
}

#[tauri::command]
async fn add_known_host(
    state: State<'_, AppState>,
    host_pattern: String,
    key_type: String,
    public_key: String,
    comment: Option<String>,
) -> Result<KnownHostEntry, String> {
    KnownHostsManager::add_entry(
        &state.known_hosts_path,
        &host_pattern,
        &key_type,
        &public_key,
        comment.as_deref(),
    )
    .map_err(e)
}

#[tauri::command]
async fn list_snippets(state: State<'_, AppState>) -> Result<Vec<terminator_core::Snippet>, String> {
    state.store.list_snippets().map_err(e)
}

#[tauri::command]
async fn save_snippet(state: State<'_, AppState>, snippet: terminator_core::Snippet) -> Result<(), String> {
    state.store.save_snippet(&snippet).map_err(e)
}

#[tauri::command]
async fn delete_snippet(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete_snippet(&id).map_err(e)
}

#[tauri::command]
async fn start_tunnel(
    state: State<'_, AppState>,
    config: TunnelConfig,
    secret_ref: Option<String>,
    password: Option<String>,
) -> Result<TunnelStatus, String> {
    let creds = match (&config.ssh_spec, password, secret_ref) {
        (TransportSpec::Ssh { .. }, Some(pw), _) => terminator_core::transport::ssh::SshCredentials {
            secret: Some(pw),
            key_passphrase: None,
            jump_secret: None,
            jump_key_passphrase: None,
        },
        (TransportSpec::Ssh { .. }, None, Some(key)) => {
            let secret = blocking_secrets(&state.secrets, move |s| s.get(&key)).await?;
            terminator_core::transport::ssh::SshCredentials {
                secret,
                key_passphrase: None,
                jump_secret: None,
                jump_key_passphrase: None,
            }
        }
        _ => terminator_core::transport::ssh::SshCredentials::default(),
    };

    state.tunnels.start_tunnel(config, creds).await.map_err(e)
}

#[tauri::command]
async fn stop_tunnel(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.tunnels.stop_tunnel(&id).await.map_err(e)
}

// ---------------------------------------------------------------------------
// RDP
//
// A parallel path to the byte-stream commands above, because RDP is a
// framebuffer protocol rather than a stream -- see core/src/rdp.rs.
// ---------------------------------------------------------------------------

/// Open an RDP session and start streaming framebuffer updates.
///
/// Returns the desktop size the server actually granted, which is often not
/// the size we asked for.
///
/// Clipboard: at open time we read the current local OS clipboard
/// and seed the daemon's CLIPRDR backend with it. The drain task
/// writes `RdpEvent::RemoteClipboard` back to the OS clipboard
/// when the remote desktop's clipboard changes. The webview is
/// responsible for forwarding subsequent local clipboard
/// changes via `rdp_local_clipboard` while the pane has focus.
#[tauri::command]
async fn open_rdp(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    spec: TransportSpec,
    width: u16,
    height: u16,
    secret_ref: Option<String>,
    password: Option<String>,
    channel: Channel<RdpEvent>,
) -> Result<RdpOpened, String> {
    let (host, port, user, domain) = match &spec {
        TransportSpec::Rdp {
            host,
            port,
            user,
            domain,
        } => (host.clone(), *port, user.clone(), domain.clone()),
        other => return Err(format!("not an RDP profile: {}", other.label())),
    };

    // RDP has no agent and no key auth -- CredSSP needs the actual password,
    // so a missing one is a hard error rather than something to attempt.
    let password = match (password, secret_ref) {
        (Some(pw), _) => pw,
        (None, Some(r)) => blocking_secrets(&state.secrets, move |s| s.get(&r))
            .await?
            .ok_or("no password saved for this connection")?,
        (None, None) => return Err("RDP requires a password".into()),
    };

    tracing::info!("open_rdp: {user}@{host}:{port} {width}x{height}");

    // Open on the daemon. The cleartext password crosses
    // 127.0.0.1 only -- same loopback trust model the SSH
    // `open_session` already uses.
    let (id, width, height, mut sse) = state
        .daemon
        .rdp_open(&RdpConfig {
            host,
            port,
            user,
            password,
            domain,
            width,
            height,
        })
        .await
        .inspect_err(|err| tracing::error!("open_rdp failed: {err:#}"))
        .map_err(e)?;

    // Seed the daemon with the current local clipboard so the
    // CLIPRDR backend can advertise it the next time the server
    // asks for a format list. A failure here is non-fatal: the
    // session is open, we just won't have any local clipboard
    // data ready for the server until the webview pushes an
    // update through `rdp_local_clipboard`.
    if let Ok(initial) = app.clipboard().read_text() {
        if let Err(err) = state.daemon.rdp_local_clipboard(id, &initial).await {
            tracing::debug!("seed rdp clipboard: {err:#}");
        }
    }

    // Drain the daemon's SSE stream of `RdpEvent`s into the
    // Tauri `Channel` the webview is already listening on. Same
    // pattern `open_session` uses for PTY/SSH `OutputEvent`s.
    //
    // `RdpEvent::RemoteClipboard` is also written to the OS
    // clipboard here so the user can paste it into any local
    // app. Text only for v1.
    let channel = channel.clone();
    // `app` is `AppHandle` (Arc internally, cheap to clone,
    // `'static`). Move a clone into the spawn so the drain task
    // owns a handle that outlives the function frame; we then
    // call `app.clipboard().write_text(...)` inside the loop.
    let app = app.clone();
    tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(item) = sse.next().await {
            match item {
                Ok(RdpEvent::RemoteClipboard { text }) => {
                    if let Err(err) = app.clipboard().write_text(text.clone()) {
                        tracing::warn!("write local clipboard: {err:#}");
                    }
                    if channel.send(RdpEvent::RemoteClipboard { text }).is_err() {
                        break;
                    }
                }
                Ok(ev) => {
                    if channel.send(ev).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!("rdp SSE stream error: {err:#}");
                    break;
                }
            }
        }
    });

    tracing::info!("rdp session {id} open at {width}x{height}");
    Ok(RdpOpened {
        id: id.to_string(),
        width,
        height,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RdpOpened {
    id: String,
    width: u16,
    height: u16,
}

/// Input is batched by the frontend: a single mouse drag would otherwise be
/// hundreds of separate IPC round trips.
#[tauri::command]
async fn rdp_input(
    state: State<'_, AppState>,
    id: String,
    ops: Vec<RdpInput>,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.rdp_input(id, ops).await.map_err(e)
}

#[tauri::command]
async fn rdp_resize(
    state: State<'_, AppState>,
    id: String,
    width: u16,
    height: u16,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.rdp_resize(id, width, height).await.map_err(e)
}

#[tauri::command]
async fn close_rdp(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.rdp_close(id).await.map_err(e)
}

/// Push a local clipboard update for a live RDP session. The
/// webview calls this whenever the OS clipboard changes while
/// the RDP pane has focus, so the daemon's CLIPRDR backend can
/// re-advertise the new text. Text only for v1.
#[tauri::command]
async fn rdp_local_clipboard(
    state: State<'_, AppState>,
    id: String,
    text: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.rdp_local_clipboard(id, &text).await.map_err(e)
}

// ---------------------------------------------------------------------------
// File browser
// ---------------------------------------------------------------------------

/// Progress for one transfer, streamed to the webview.
///
/// The webview side (TypeScript) expects all three variants because
/// the upload/download commands used to emit them; now that those
/// commands are stubbed while the daemon grows matching routes,
/// only `Failed` is sent. The other two are kept on the wire so the
/// frontend does not need a matching change when the routes land.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
#[allow(dead_code)]
enum TransferEvent {
    #[serde(rename_all = "camelCase")]
    Progress { transferred: u64, total: u64 },
    #[serde(rename_all = "camelCase")]
    Done { bytes: u64 },
    #[serde(rename_all = "camelCase")]
    Failed { message: String },
}

/// Where a local pane should start.
#[tauri::command]
async fn local_home() -> Result<String, String> {
    Ok(terminator_core::files::local_home()
        .to_string_lossy()
        .to_string())
}

/// Scratch directory for files being shuttled between the OS and a remote host.
///
/// Dragging a remote file to the Finder and pasting a file into the drawer both
/// need a real path on this machine: a drag payload is a file URL, and the
/// SFTP upload path takes a local path rather than bytes. Both are staged here.
fn staging_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(e)?
        .join("transfer-staging");
    std::fs::create_dir_all(&dir).map_err(e)?;
    Ok(dir)
}

/// A collision-free path inside the staging directory.
///
/// Two files with the same name from different remote directories must not
/// overwrite each other mid-drag, so each gets its own subdirectory. Keeping
/// the original file name matters: it becomes the name in the Finder once the
/// drop lands.
fn staging_path(app: &tauri::AppHandle, name: &str) -> Result<std::path::PathBuf, String> {
    let name = safe_file_name(name)?;
    let dir = staging_dir(app)?.join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).map_err(e)?;
    Ok(dir.join(name))
}

/// Reduce an untrusted name to a single path component, or reject it.
///
/// The name comes from a remote listing or from a pasted file, so a hostile or
/// merely broken source could otherwise write anywhere the app can reach by
/// returning something like `../../../.ssh/authorized_keys`.
fn safe_file_name(name: &str) -> Result<&str, String> {
    std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or_else(|| format!("unsafe file name: {name:?}"))
}

/// Reserve a local path for a file about to be downloaded for a drag-out.
#[tauri::command]
async fn stage_path(app: tauri::AppHandle, name: String) -> Result<String, String> {
    Ok(staging_path(&app, &name)?.to_string_lossy().to_string())
}

/// Write bytes from the webview (a pasted or dropped file) to a real path so
/// the normal SFTP upload can take it from there.
#[tauri::command]
async fn stage_bytes(
    app: tauri::AppHandle,
    name: String,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let path = staging_path(&app, &name)?;
    tokio::task::spawn_blocking(move || {
        std::fs::write(&path, bytes)?;
        Ok::<_, std::io::Error>(path)
    })
    .await
    .map_err(e)?
    .map_err(e)
    .map(|p| p.to_string_lossy().to_string())
}

/// Drop everything staged in previous runs.
///
/// Deliberately done at startup rather than on exit: a crash or a force-quit
/// would otherwise leak copies of remote files onto disk indefinitely, and at
/// startup nothing can still be holding a drag in progress.
fn clear_staging(app: &tauri::AppHandle) {
    if let Ok(dir) = staging_dir(app) {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// List a local directory.
///
/// Runs on the blocking pool: `std::fs` is synchronous, and a directory on a
/// stalled network mount would otherwise block a tokio worker and freeze
/// unrelated sessions.
#[tauri::command]
async fn list_local_dir(path: String) -> Result<terminator_core::files::Listing, String> {
    tokio::task::spawn_blocking(move || {
        terminator_core::files::list_local(std::path::Path::new(&path))
    })
    .await
    .map_err(e)?
    .map_err(e)
}

#[tauri::command]
async fn remote_home(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_home(id).await.map_err(e)
}

#[tauri::command]
async fn list_remote_dir(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<terminator_core::files::Listing, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_list(id, &path).await.map_err(e)
}

#[tauri::command]
async fn remote_mkdir(state: State<'_, AppState>, id: String, path: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_mkdir(id, &path).await.map_err(e)
}

#[tauri::command]
async fn remote_remove(
    state: State<'_, AppState>,
    id: String,
    path: String,
    is_dir: bool,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_remove(id, &path, is_dir).await.map_err(e)
}

#[tauri::command]
async fn remote_rename(
    state: State<'_, AppState>,
    id: String,
    from: String,
    to: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_rename(id, &from, &to).await.map_err(e)
}

/// Local -> remote.
///
/// Errors are reported on the channel *as well as* returned, so a UI that is
/// only listening to the channel still learns the transfer failed.
///
/// The pre-daemon path ran `RemoteFs::upload` on the Tauri side, which only
/// worked for local PTY sessions (the SFTP impl needs the SSH connection the
/// daemon now owns). Routing the call through the daemon means a single
/// round-trip HTTP POST: the daemon reads the local file (same machine) and
/// streams it to the remote via SFTP. Per-byte progress events are not
/// streamed yet -- the channel only sees the final `Done` or `Failed` event.
#[tauri::command]
async fn upload_file(
    state: State<'_, AppState>,
    id: String,
    local: String,
    remote: String,
    channel: Channel<TransferEvent>,
) -> Result<u64, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    match state.daemon.files_upload(id, &local, &remote).await {
        Ok(bytes) => {
            let _ = channel.send(TransferEvent::Done { bytes });
            Ok(bytes)
        }
        Err(err) => {
            let message = format!("{err:#}");
            tracing::error!("upload failed: {message}");
            let _ = channel.send(TransferEvent::Failed {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

#[tauri::command]
async fn read_remote_text_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<String, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_read(id, &path, 10 * 1024 * 1024).await.map_err(e)
}

#[tauri::command]
async fn write_remote_text_file(
    state: State<'_, AppState>,
    id: String,
    path: String,
    content: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_write(id, &path, &content).await.map_err(e)
}

#[tauri::command]
async fn read_local_text_file(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        terminator_core::files::read_local_text(std::path::Path::new(&path), 10 * 1024 * 1024)
    })
    .await
    .map_err(e)?
    .map_err(e)
}

#[tauri::command]
async fn write_local_text_file(path: String, content: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        terminator_core::files::write_local_text(std::path::Path::new(&path), &content)
    })
    .await
    .map_err(e)?
    .map_err(e)
}

#[tauri::command]
async fn search_remote_dir(
    state: State<'_, AppState>,
    id: String,
    path: String,
    options: terminator_core::files::SearchOptions,
) -> Result<Vec<terminator_core::files::FileSearchResult>, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.daemon.files_search(id, &path, &options).await.map_err(e)
}

#[tauri::command]
async fn search_local_dir(
    path: String,
    options: terminator_core::files::SearchOptions,
) -> Result<Vec<terminator_core::files::FileSearchResult>, String> {
    tokio::task::spawn_blocking(move || {
        terminator_core::files::search_local(std::path::Path::new(&path), &options)
    })
    .await
    .map_err(e)?
    .map_err(e)
}

/// Cross-session search. Walks every live session's
/// scrollback ring buffer and returns matching lines. The
/// webview groups by session id and shows them in the
/// command palette / find panel.
#[tauri::command]
async fn search_sessions(
    state: State<'_, AppState>,
    query: String,
    case_sensitive: Option<bool>,
    max_per_session: Option<usize>,
) -> Result<serde_json::Value, String> {
    if query.is_empty() {
        return Ok(serde_json::json!({ "results": [] }));
    }
    let case_sensitive = case_sensitive.unwrap_or(false);
    let max_per_session = max_per_session.unwrap_or(50);
    let results = state
        .daemon
        .search_sessions(&query, case_sensitive, max_per_session)
        .await
        .map_err(e)?;
    Ok(serde_json::json!({ "results": results }))
}

/// Remote -> local.
#[tauri::command]
async fn download_file(
    state: State<'_, AppState>,
    id: String,
    remote: String,
    local: String,
    channel: Channel<TransferEvent>,
) -> Result<u64, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    match state.daemon.files_download(id, &remote, &local).await {
        Ok(bytes) => {
            let _ = channel.send(TransferEvent::Done { bytes });
            Ok(bytes)
        }
        Err(err) => {
            let message = format!("{err:#}");
            tracing::error!("download failed: {message}");
            let _ = channel.send(TransferEvent::Failed {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

// ---------------------------------------------------------------------------
// Command Execution & Batch Runner
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecRequest {
    id: String,
    label: String,
    spec: TransportSpec,
    command: String,
    secret_ref: Option<String>,
    password: Option<String>,
    jump_secret_ref: Option<String>,
    jump_password: Option<String>,
    cwd: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchExecResult {
    id: String,
    label: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
    error: Option<String>,
    duration_ms: u64,
}

#[tauri::command]
async fn exec_command(
    state: State<'_, AppState>,
    spec: TransportSpec,
    command: String,
    secret_ref: Option<String>,
    password: Option<String>,
    jump_secret_ref: Option<String>,
    jump_password: Option<String>,
    cwd: Option<String>,
) -> Result<terminator_core::session::ExecResult, String> {
    let mut creds = Credentials::default();
    if let Some(p) = password {
        creds.secret = Some(p);
    } else if let Some(r) = secret_ref {
        let sec = state.secrets.clone();
        creds.secret = blocking_secrets(&sec, move |s| s.get(&r)).await?;
    }
    if let Some(jp) = jump_password {
        creds.jump_secret = Some(jp);
    } else if let Some(jr) = jump_secret_ref {
        let sec = state.secrets.clone();
        creds.jump_secret = blocking_secrets(&sec, move |s| s.get(&jr)).await?;
    }

    state
        .daemon
        .exec_command(&spec, &command, &creds, cwd.as_deref())
        .await
        .map_err(e)
}

#[tauri::command]
async fn batch_exec(
    state: State<'_, AppState>,
    requests: Vec<BatchExecRequest>,
) -> Result<Vec<BatchExecResult>, String> {
    let mut handles = Vec::new();

    for req in requests {
        let state_daemon = state.daemon.clone();
        let secrets = state.secrets.clone();

        handles.push(tokio::spawn(async move {
            let start = std::time::Instant::now();
            let mut creds = Credentials::default();
            if let Some(p) = req.password {
                creds.secret = Some(p);
            } else if let Some(r) = req.secret_ref {
                let sec = secrets.clone();
                if let Ok(Ok(Some(s))) = tokio::task::spawn_blocking(move || sec.get(&r)).await {
                    creds.secret = Some(s);
                }
            }
            if let Some(jp) = req.jump_password {
                creds.jump_secret = Some(jp);
            } else if let Some(jr) = req.jump_secret_ref {
                let sec = secrets.clone();
                if let Ok(Ok(Some(s))) = tokio::task::spawn_blocking(move || sec.get(&jr)).await {
                    creds.jump_secret = Some(s);
                }
            }

            match tokio::time::timeout(
                std::time::Duration::from_secs(60),
                state_daemon.exec_command(&req.spec, &req.command, &creds, req.cwd.as_deref()),
            )
            .await
            {
                Ok(Ok(res)) => BatchExecResult {
                    id: req.id,
                    label: req.label,
                    exit_code: res.exit_code,
                    stdout: res.stdout,
                    stderr: res.stderr,
                    error: None,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
                Ok(Err(err)) => BatchExecResult {
                    id: req.id,
                    label: req.label,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some(err.to_string()),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
                Err(_) => BatchExecResult {
                    id: req.id,
                    label: req.label,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some("Command timed out after 60 seconds".into()),
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            }
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        if let Ok(res) = handle.await {
            results.push(res);
        }
    }
    Ok(results)
}

#[cfg(target_os = "macos")]
fn disable_press_and_hold() {
    use std::ffi::c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFPreferencesSetAppValue(
            key: *const c_void,
            value: *const c_void,
            application_id: *const c_void,
        );
        fn CFPreferencesAppSynchronize(application_id: *const c_void) -> u8;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const std::os::raw::c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(cf: *const c_void);
        static kCFBooleanFalse: *const c_void;
    }

    #[link(name = "objc", kind = "dylib")]
    extern "C" {
        fn objc_getClass(name: *const std::os::raw::c_char) -> *mut c_void;
        fn sel_registerName(name: *const std::os::raw::c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    // On macOS, WKWebView by default enables "Press and Hold" (accent character picker)
    // for alphanumeric keys. This suppresses continuous key-repeat events when holding
    // down character keys ('j', 'k', 'l', 'h' in vi/vim), while non-alphanumeric keys
    // like arrow keys continue repeating. Disabling ApplePressAndHoldEnabled restores
    // normal terminal key repeat behavior.
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    unsafe {
        // 1. Set via CoreFoundation preferences for both bundle ID and current app domain
        let key_c = std::ffi::CString::new("ApplePressAndHoldEnabled").unwrap();
        let app_c = std::ffi::CString::new("com.terminator.app").unwrap();
        let key =
            CFStringCreateWithCString(std::ptr::null(), key_c.as_ptr(), K_CF_STRING_ENCODING_UTF8);
        let app =
            CFStringCreateWithCString(std::ptr::null(), app_c.as_ptr(), K_CF_STRING_ENCODING_UTF8);

        if !key.is_null() {
            if !app.is_null() {
                CFPreferencesSetAppValue(key, kCFBooleanFalse, app);
                CFPreferencesAppSynchronize(app);
                CFRelease(app);
            }
            CFRelease(key);
        }

        // 2. Set in NSUserDefaults in-memory for the running process
        let ns_user_defaults_class = objc_getClass(c"NSUserDefaults".as_ptr());
        let ns_string_class = objc_getClass(c"NSString".as_ptr());

        if !ns_user_defaults_class.is_null() && !ns_string_class.is_null() {
            let standard_user_defaults_sel = sel_registerName(c"standardUserDefaults".as_ptr());
            let string_with_utf8_sel = sel_registerName(c"stringWithUTF8String:".as_ptr());
            let set_bool_for_key_sel = sel_registerName(c"setBool:forKey:".as_ptr());

            let msg_send_fn: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let defaults = msg_send_fn(ns_user_defaults_class, standard_user_defaults_sel);

            let msg_send_str_fn: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *const std::os::raw::c_char,
            ) -> *mut c_void = std::mem::transmute(objc_msgSend as *const ());
            let key_str = msg_send_str_fn(
                ns_string_class,
                string_with_utf8_sel,
                c"ApplePressAndHoldEnabled".as_ptr(),
            );

            if !defaults.is_null() && !key_str.is_null() {
                let msg_send_set_bool_fn: unsafe extern "C" fn(
                    *mut c_void,
                    *mut c_void,
                    bool,
                    *mut c_void,
                ) = std::mem::transmute(objc_msgSend as *const ());
                msg_send_set_bool_fn(defaults, set_bool_for_key_sel, false, key_str);
            }
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    disable_press_and_hold();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "terminator=info,terminator_core=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_drag::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            clear_staging(app.handle());

            let store = Store::open(&data_dir.join("terminator.db"))?;
            let known_hosts = data_dir.join("known_hosts");

            // The Tauri runtime is single-threaded for setup, but the
            // daemon spawn/connect call is async. We block here on
            // purpose: the app cannot meaningfully run without a
            // daemon, so we'd rather fail fast and visibly than
            // come up half-configured.
            //
            // `tauri::async_runtime::block_on` runs the future on
            // Tauri's own tokio runtime (the same one that
            // services IPC). `futures::executor::block_on` would
            // build a fresh runtime with no reactor, and any
            // `tokio::time::sleep` / `tokio::spawn` inside
            // `spawn_or_connect` would panic with "there is no
            // reactor running".
            let daemon = tauri::async_runtime::block_on(async {
                daemon_client::spawn_or_connect().await
            })
            .map_err(|e| -> Box<dyn std::error::Error> {
                format!("failed to start terminator-daemon: {e}").into()
            })?;
            tracing::info!("daemon client ready at {}", "<redacted>"); // URL printed in debug only

            let state = AppState {
                daemon: Arc::new(daemon),
                store,
                secrets: Arc::new(Secrets::new(data_dir.join("secrets"))),
                tunnels: TunnelManager::new(known_hosts.clone()),
                known_hosts_path: known_hosts,
            };
            tracing::info!("data dir: {}", data_dir.display());
            warn_if_dev_server_down();
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_session,
            attach_session,
            list_sessions,
            write_session,
            resize_session,
            close_session,
            session_logs,
            list_session_logs,
            read_log_file,
            delete_session_log,
            log_dir,
            list_profiles,
            save_profile,
            update_profile,
            delete_profile,
            search_commands,
            session_commands,
            set_secret,
            delete_secret,
            rename_secret,
            secrets_backend,
            list_local_shells,
            has_secret,
            vault_status,
            unlock_vault,
            lock_vault,
            change_vault_passphrase,
            shell_integration_snippet,
            list_tunnels,
            save_tunnel,
            delete_tunnel,
            active_tunnels,
            start_tunnel,
            stop_tunnel,
            list_known_hosts,
            delete_known_host,
            add_known_host,
            list_snippets,
            save_snippet,
            delete_snippet,
            open_rdp,
            rdp_input,
            rdp_resize,
            close_rdp,
            rdp_local_clipboard,
            local_home,
            list_local_dir,
            stage_path,
            stage_bytes,
            remote_home,
            list_remote_dir,
            remote_mkdir,
            remote_remove,
            remote_rename,
            upload_file,
            download_file,
            read_remote_text_file,
            write_remote_text_file,
            read_local_text_file,
            write_local_text_file,
            search_remote_dir,
            search_local_dir,
            search_sessions,
            exec_command,
            batch_exec,
            log_frontend,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Debug builds load `build.devUrl`, not the bundled `frontendDist`. If the
/// Vite dev server is not up, the window renders blank with no error anywhere
/// -- there is no frontend left to report one. Say so explicitly instead of
/// leaving a silent empty window.
#[cfg(debug_assertions)]
fn warn_if_dev_server_down() {
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    // Resolve by name and try *every* address: Vite binds ::1 by default on
    // some setups, so probing 127.0.0.1 alone reports a false failure.
    const DEV_HOST: &str = "localhost:1420";

    let mut reachable = false;
    for _ in 0..6 {
        reachable = DEV_HOST
            .to_socket_addrs()
            .map(|addrs| {
                addrs
                    .into_iter()
                    .any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
            })
            .unwrap_or(false);
        if reachable {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    if !reachable {
        tracing::error!(
            "dev server NOT reachable at http://{DEV_HOST} -- this debug build loads \
             devUrl, so the window will be BLANK. Start it first: \
             npx vite --port 1420 --strictPort"
        );
    }
}

#[cfg(not(debug_assertions))]
fn warn_if_dev_server_down() {}

#[cfg(test)]
mod tests {
    use super::safe_file_name;

    #[test]
    fn plain_names_pass_through() {
        assert_eq!(safe_file_name("report.csv").unwrap(), "report.csv");
        assert_eq!(safe_file_name("a b.tar.gz").unwrap(), "a b.tar.gz");
    }

    #[test]
    fn traversal_is_stripped_or_rejected() {
        // A staged file must never land outside the staging directory, however
        // creative the name a remote host returns.
        assert_eq!(safe_file_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(safe_file_name("/etc/passwd").unwrap(), "passwd");
        assert!(safe_file_name("..").is_err());
        assert!(safe_file_name(".").is_err());
        assert!(safe_file_name("").is_err());
        assert!(safe_file_name("/").is_err());
    }
}
