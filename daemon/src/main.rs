//! `terminator-daemon` -- the long-lived PTY/SSH/RDP host. Owns every
//! session opened from the Tauri UI; survives the UI being closed so
//! tabs and scrollback are still there next time the user opens the
//! app. The Tauri app speaks plain HTTP to us.
//!

// `windows_subsystem = "windows"` strips the default console window
// that Rust binaries otherwise allocate on Windows. Without this,
// the Tauri host spawns a daemon and a separate `cmd.exe`-style
// window pops up next to the app -- and if the user closes that
// window (sensible thing to do, they think it's leftover noise),
// the daemon process dies with it. Every shell, local or SSH, then
// silently stops accepting input while still showing cached
// scrollback. Stderr is still routed to a log file via
// `daemon_client::spawn_daemon`, so the no-console build is not
// lossy for diagnostics.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Lifecycle: the daemon is spawned by the Tauri app on first launch
//! (if the port-file doesn't already point at a running instance).
//! It writes the listening port to `daemon.port` under the per-user
//! data dir so subsequent Tauri launches can connect without
//! scanning. When the Tauri app closes, the daemon keeps running;
//! closing the Tauri app does NOT terminate this process.
//!
//! Manual stop: send Ctrl-C in the terminal, or call
//! `POST /shutdown` (not yet implemented -- Session 1d).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use terminator_core::session::SessionManager;

mod manager;
mod persist;
mod rdp;
mod ringbuffer;
mod server;

use manager::DaemonSessionManager;
use persist::SessionStore;
use rdp::DaemonRdpManager;

#[tokio::main]
async fn main() -> Result<()> {
    // Default to info-level logs; the Tauri side can override via
    // `RUST_LOG=terminator_daemon=debug`.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_target(false)
        .init();

    let data_dir = resolve_data_dir()?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("create log dir {}", log_dir.display()))?;

    let core = Arc::new(SessionManager::new(log_dir.clone()));
    // The session store keeps `{id, spec, opened_at_ms,
    // closed_at_ms}` so the reattach prompt survives a daemon
    // restart. It is required for the "previously open" list,
    // which is now part of the GET /sessions response.
    let store = Arc::new(
        SessionStore::open(&data_dir.join("sessions.db"))
            .with_context(|| format!("open session store at {}", data_dir.display()))?,
    );
    let manager = Arc::new(DaemonSessionManager::new(core, Some(store)));
    // The daemon now owns the live RDP sessions too, so a
    // Tauri UI crash no longer takes the remote desktop with
    // it. Mirrors the PTY/SSH lifecycle that the Session 1
    // work moved out of the Tauri process.
    let rdp_manager = Arc::new(DaemonRdpManager::new());

    // Bind to 127.0.0.1:0 so the OS picks a free port. Writing that
    // port to disk is what the Tauri side reads to find us on
    // subsequent launches.
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind 127.0.0.1:0"))?;
    let bound_port = listener
        .local_addr()
        .with_context(|| "read local_addr from listener")?
        .port();

    let port_file = data_dir.join("daemon.port");
    std::fs::write(&port_file, bound_port.to_string())
        .with_context(|| format!("write port file {}", port_file.display()))?;
    tracing::info!(port = bound_port, data_dir = %data_dir.display(), "daemon ready");

    let app = server::router(manager, rdp_manager, log_dir);
    axum::serve(listener, app)
        .with_graceful_shutdown(server::shutdown_signal())
        .await
        .with_context(|| "axum::serve")?;

    // Clean up the port file on graceful shutdown so a future launch
    // doesn't read a stale value. A force-killed daemon leaves a
    // stale port file behind; the Tauri side handles that by
    // probing before relying on the file.
    let _ = std::fs::remove_file(&port_file);
    Ok(())
}

/// Per-user data dir. POSIX: `$XDG_DATA_HOME/terminator` (or
/// `~/.local/share/terminator`). Windows: `%LOCALAPPDATA%\terminator`.
/// macOS: `~/Library/Application Support/terminator`.
fn resolve_data_dir() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA is not set"))?;
        Ok(PathBuf::from(base).join("terminator"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
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
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".local").join("share").join("terminator"))
    }
}
