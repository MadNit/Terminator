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
    rdp::{RdpConfig, RdpEvent, RdpInput, RdpManager},
    secrets::{Backend, Secrets},
    session::Credentials,
    session::SessionManager,
    store::Store,
    TransportSpec,
};
use uuid::Uuid;

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
    sessions: SessionManager,
    rdp: RdpManager,
    store: Store,
    /// Arc so blocking keychain work can be moved onto a blocking thread.
    secrets: Arc<Secrets>,
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
    channel: Channel<SessionEvent>,
) -> Result<String, String> {
    let out_ch = channel.clone();
    let on_output = Arc::new(move |data: Bytes| {
        let _ = out_ch.send(SessionEvent::Output {
            data: B64.encode(&data),
        });
    });
    let on_exit = Arc::new(move || {
        let _ = channel.send(SessionEvent::Exit);
    });

    tracing::info!("open_session: {:?} {cols}x{rows}", spec);

    // Resolve the credential just in time. It lives only for this call and is
    // never written into the profile row.
    let creds = match (&spec, password, secret_ref) {
        (TransportSpec::Ssh { .. }, Some(pw), _) => Credentials {
            secret: Some(pw),
            key_passphrase: None,
        },
        // Propagate the error instead of swallowing it: a locked vault would
        // otherwise look exactly like "no password saved", and the connection
        // would fail later with a confusing auth error.
        (TransportSpec::Ssh { .. }, None, Some(r)) => Credentials {
            secret: blocking_secrets(&state.secrets, move |s| s.get(&r)).await?,
            key_passphrase: None,
        },
        _ => Credentials::default(),
    };

    let id = state
        .sessions
        .open_with(spec, cols, rows, creds, on_output, on_exit)
        .await
        .inspect_err(|err| tracing::error!("open_session failed: {err:#}"))
        .map_err(e)?;
    tracing::info!("session {id} open");
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

#[tauri::command]
async fn write_session(state: State<'_, AppState>, id: String, data: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let bytes = B64.decode(data.as_bytes()).map_err(e)?;
    state.sessions.write(id, Bytes::from(bytes)).map_err(e)
}

#[tauri::command]
async fn resize_session(
    state: State<'_, AppState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.sessions.resize(id, cols, rows).map_err(e)
}

#[tauri::command]
async fn close_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.sessions.close(id).map_err(e)
}

#[derive(serde::Serialize)]
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
    let log_dir = state.sessions.log_dir().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        let Ok(entries) = std::fs::read_dir(&log_dir) else {
            return Ok(items);
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
        Ok(items)
    })
    .await
    .map_err(e)?
}

#[tauri::command]
async fn read_log_file(path: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        if !p.exists() {
            return Err(format!("File not found: {path}"));
        }
        let meta = std::fs::metadata(p).map_err(|err| err.to_string())?;
        if meta.len() > 5 * 1024 * 1024 {
            use std::io::Read;
            let mut file = std::fs::File::open(p).map_err(|err| err.to_string())?;
            let mut buffer = vec![0u8; 5 * 1024 * 1024];
            let n = file.read(&mut buffer).map_err(|err| err.to_string())?;
            let mut s = String::from_utf8_lossy(&buffer[..n]).to_string();
            s.push_str("\n\n... [Log truncated at 5MB] ...");
            return Ok(s);
        }
        std::fs::read_to_string(p).map_err(|err| err.to_string())
    })
    .await
    .map_err(e)?
}

#[tauri::command]
async fn delete_session_log(
    state: State<'_, AppState>,
    dir_name: String,
) -> Result<(), String> {
    let log_dir = state.sessions.log_dir().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let safe_name = safe_file_name(&dir_name)?;
        let path = log_dir.join(safe_name);
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|err| err.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(e)?
}

#[tauri::command]
async fn session_logs(state: State<'_, AppState>, id: String) -> Result<serde_json::Value, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let p = state.sessions.logs(id).map_err(e)?;
    Ok(serde_json::json!({ "cast": p.cast, "plain": p.plain }))
}

#[tauri::command]
async fn log_dir(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state.sessions.log_dir().to_string_lossy().into_owned())
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

/// The OSC 133 snippet, so the UI can offer to install it into the user's
/// shell rc (or inject it over SSH later).
#[tauri::command]
async fn shell_integration_snippet() -> Result<String, String> {
    Ok(terminator_core::OSC133_BASH_ZSH.to_string())
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
#[tauri::command]
async fn open_rdp(
    state: State<'_, AppState>,
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

    // Bounded, and deliberately shallow. The engine coalesces damage while
    // this is full, so a slow renderer costs frame granularity rather than
    // unbounded memory.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<RdpEvent>(8);
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if channel.send(ev).is_err() {
                break;
            }
        }
    });

    let (id, width, height) = state
        .rdp
        .open(
            RdpConfig {
                host,
                port,
                user,
                password,
                domain,
                width,
                height,
            },
            tx,
        )
        .await
        .inspect_err(|err| tracing::error!("open_rdp failed: {err:#}"))
        .map_err(e)?;

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
    state.rdp.input(id, ops).map_err(e)
}

#[tauri::command]
async fn rdp_resize(
    state: State<'_, AppState>,
    id: String,
    width: u16,
    height: u16,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.rdp.resize(id, width, height).map_err(e)
}

#[tauri::command]
async fn close_rdp(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    state.rdp.close(id).map_err(e)
}

// ---------------------------------------------------------------------------
// File browser
// ---------------------------------------------------------------------------

/// Progress for one transfer, streamed to the webview.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
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
    let fs = state.sessions.files(id).await.map_err(e)?;
    fs.home().await.map_err(e)
}

#[tauri::command]
async fn list_remote_dir(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<terminator_core::files::Listing, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let fs = state.sessions.files(id).await.map_err(e)?;
    fs.list(&path).await.map_err(e)
}

#[tauri::command]
async fn remote_mkdir(state: State<'_, AppState>, id: String, path: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let fs = state.sessions.files(id).await.map_err(e)?;
    fs.mkdir(&path).await.map_err(e)
}

#[tauri::command]
async fn remote_remove(
    state: State<'_, AppState>,
    id: String,
    path: String,
    is_dir: bool,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let fs = state.sessions.files(id).await.map_err(e)?;
    fs.remove(&path, is_dir).await.map_err(e)
}

#[tauri::command]
async fn remote_rename(
    state: State<'_, AppState>,
    id: String,
    from: String,
    to: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let fs = state.sessions.files(id).await.map_err(e)?;
    fs.rename(&from, &to).await.map_err(e)
}

/// Turn a transfer channel into a progress sink for the core.
fn progress_sink(channel: Channel<TransferEvent>) -> terminator_core::files::ProgressSink {
    Arc::new(move |p: terminator_core::files::Progress| {
        let _ = channel.send(TransferEvent::Progress {
            transferred: p.transferred,
            total: p.total,
        });
    })
}

/// Local -> remote.
///
/// Errors are reported on the channel *as well as* returned, so a UI that is
/// only listening to the channel still learns the transfer failed.
#[tauri::command]
async fn upload_file(
    state: State<'_, AppState>,
    id: String,
    local: String,
    remote: String,
    channel: Channel<TransferEvent>,
) -> Result<u64, String> {
    let id = Uuid::parse_str(&id).map_err(e)?;
    let fs = state.sessions.files(id).await.map_err(e)?;
    let result = fs
        .upload(
            std::path::Path::new(&local),
            &remote,
            progress_sink(channel.clone()),
        )
        .await;

    match result {
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
    let fs = state.sessions.files(id).await.map_err(e)?;
    let result = fs
        .download(
            &remote,
            std::path::Path::new(&local),
            progress_sink(channel.clone()),
        )
        .await;

    match result {
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
            let state = AppState {
                sessions: SessionManager::new(data_dir.join("logs")).with_store(store.clone()),
                rdp: RdpManager::new(),
                store,
                secrets: Arc::new(Secrets::new(data_dir.join("secrets"))),
            };
            tracing::info!("data dir: {}", data_dir.display());
            warn_if_dev_server_down();
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_session,
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
            has_secret,
            vault_status,
            unlock_vault,
            lock_vault,
            change_vault_passphrase,
            shell_integration_snippet,
            open_rdp,
            rdp_input,
            rdp_resize,
            close_rdp,
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
