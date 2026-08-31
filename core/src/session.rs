//! Session lifecycle: binds a transport to a set of taps.
//!
//! The UI receives output through a plain callback, so the core has no idea
//! whether it is talking to Tauri, an Electron sidecar, or a test harness.

use crate::store::Store;
use crate::tap::{
    cast::CastTap,
    plain::{CommandRecord, CommandSink, PlainTap},
    Direction, Tap, TapSet,
};
use crate::transport::{pty::PtyTransport, Transport, TransportSpec};
use anyhow::{anyhow, Result};
use bytes::{Bytes, BytesMut};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

/// How the core hands output to whatever UI is attached.
pub type OutputSink = Arc<dyn Fn(Bytes) + Send + Sync>;
/// Called once when the session's process exits.
pub type ExitSink = Arc<dyn Fn() + Send + Sync>;

/// Credentials resolved at connect time. Never persisted with a profile --
/// the profile only records *which* method to use.
#[derive(Default)]
pub struct Credentials {
    pub secret: Option<String>,
    pub key_passphrase: Option<String>,
    pub jump_secret: Option<String>,
    pub jump_key_passphrase: Option<String>,
}

/// Output is coalesced over this window before reaching the UI. Without it a
/// fast producer generates thousands of tiny IPC messages per second and the
/// renderer falls behind.
const COALESCE_WINDOW: Duration = Duration::from_millis(8);
/// Upper bound on a single batch, so a flood still yields to the event loop.
const MAX_BATCH: usize = 256 * 1024;
/// How often buffered taps are pushed to disk while a session is idle.
const FLUSH_TICK: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct Session {
    pub id: Uuid,
    pub title: String,
    transport: Arc<dyn Transport>,
    taps: TapSet,
    pub log_paths: LogPaths,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogPaths {
    pub cast: PathBuf,
    pub plain: PathBuf,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Mutex<HashMap<Uuid, Arc<Session>>>>,
    log_dir: PathBuf,
    /// Optional so tests and headless embedders can run without a database.
    store: Option<Store>,
}

impl SessionManager {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            log_dir,
            store: None,
        }
    }

    /// Persist OSC 133 command records into the given store.
    pub fn with_store(mut self, store: Store) -> Self {
        self.store = Some(store);
        self
    }

    pub fn log_dir(&self) -> &PathBuf {
        &self.log_dir
    }

    /// Open a session, wire up logging taps, and start pumping output.
    ///
    /// Async because connecting a remote transport is a network operation;
    /// local sessions simply never await.
    pub async fn open(
        &self,
        spec: TransportSpec,
        cols: u16,
        rows: u16,
        on_output: OutputSink,
        on_exit: ExitSink,
    ) -> Result<Uuid> {
        self.open_with(spec, cols, rows, Default::default(), on_output, on_exit)
            .await
    }

    /// As [`open`], with runtime credentials for transports that need them.
    pub async fn open_with(
        &self,
        spec: TransportSpec,
        cols: u16,
        rows: u16,
        #[allow(unused_variables)] creds: Credentials,
        on_output: OutputSink,
        on_exit: ExitSink,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let short = &id.to_string()[..8];
        let dir = self.log_dir.join(format!("{stamp}-{short}"));

        let log_paths = LogPaths {
            cast: dir.join("session.cast"),
            plain: dir.join("session.log"),
        };

        let mut transport: Box<dyn Transport> = match &spec {
            TransportSpec::Local { .. } => Box::new(PtyTransport::spawn(&spec, cols, rows)?),
            #[cfg(feature = "ssh")]
            TransportSpec::Ssh { .. } => {
                let known_hosts = self
                    .log_dir
                    .parent()
                    .unwrap_or(&self.log_dir)
                    .join("known_hosts");
                Box::new(
                    crate::transport::ssh::SshTransport::connect(
                        &spec,
                        cols,
                        rows,
                        crate::transport::ssh::SshCredentials {
                            secret: creds.secret,
                            key_passphrase: creds.key_passphrase,
                            jump_secret: creds.jump_secret,
                            jump_key_passphrase: creds.jump_key_passphrase,
                        },
                        known_hosts,
                    )
                    .await?,
                )
            }
            other => return Err(anyhow!("transport not implemented yet: {}", other.label())),
        };
        let rx = transport.output();
        let transport: Arc<dyn Transport> = Arc::from(transport);

        let mut taps = TapSet::new();
        // Raw, replayable. Source of truth.
        match CastTap::create(&log_paths.cast, cols, rows) {
            Ok(t) => taps.push(Arc::new(t) as Arc<dyn Tap>),
            Err(e) => tracing::warn!("cast log disabled: {e}"),
        }
        // Clean, greppable, plus OSC 133 command records.
        //
        // The sink is what turns the semantic markers into durable history:
        // every command the shell reports is written straight through to the
        // FTS index, keyed by session, as it completes.
        let sink: Option<CommandSink> = self.store.clone().map(|store| {
            let sid = id.to_string();
            Box::new(move |rec: CommandRecord| {
                if let Err(e) =
                    store.record_command(&sid, &rec.command, rec.exit_code, rec.duration_ms)
                {
                    tracing::warn!("failed to record command: {e}");
                }
            }) as CommandSink
        });
        match PlainTap::with_sink(&log_paths.plain, sink) {
            Ok(t) => taps.push(Arc::new(t) as Arc<dyn Tap>),
            Err(e) => tracing::warn!("plain log disabled: {e}"),
        }

        let session = Arc::new(Session {
            id,
            title: spec.label(),
            transport: transport.clone(),
            taps: taps.clone(),
            log_paths,
        });

        self.inner
            .lock()
            .map_err(|_| anyhow!("session map poisoned"))?
            .insert(id, session.clone());

        // Pump: transport -> taps -> UI.
        let manager = self.clone();
        tokio::spawn(async move {
            pump(rx, taps, on_output).await;
            manager.reap(id);
            on_exit();
        });

        Ok(id)
    }

    pub fn write(&self, id: Uuid, data: Bytes) -> Result<()> {
        let s = self.get(id)?;
        s.taps.on_data(Direction::Input, &data);
        let t = s.transport.clone();
        tokio::spawn(async move {
            if let Err(e) = t.write(data).await {
                tracing::warn!("write failed: {e}");
            }
        });
        Ok(())
    }

    pub fn resize(&self, id: Uuid, cols: u16, rows: u16) -> Result<()> {
        let s = self.get(id)?;
        s.taps.on_resize(cols, rows);
        let t = s.transport.clone();
        tokio::spawn(async move {
            if let Err(e) = t.resize(cols, rows).await {
                tracing::warn!("resize failed: {e}");
            }
        });
        Ok(())
    }

    pub fn close(&self, id: Uuid) -> Result<()> {
        if let Ok(s) = self.get(id) {
            let t = s.transport.clone();
            tokio::spawn(async move {
                let _ = t.shutdown().await;
            });
        }
        self.reap(id);
        Ok(())
    }

    pub fn logs(&self, id: Uuid) -> Result<LogPaths> {
        Ok(self.get(id)?.log_paths.clone())
    }

    /// File browser for a session's remote end.
    ///
    /// Delegates to the transport, which caches the underlying connection, so
    /// calling this per directory listing is cheap after the first time.
    pub async fn files(&self, id: Uuid) -> Result<Arc<dyn crate::files::RemoteFs>> {
        self.get(id)?.transport.files().await
    }

    /// Execute a one-shot command against a local or remote target.
    pub async fn exec_command(
        &self,
        spec: &TransportSpec,
        command: &str,
        #[allow(unused_variables)] creds: Credentials,
        #[allow(unused_variables)] cwd: Option<&str>,
    ) -> Result<ExecResult> {
        match spec {
            TransportSpec::Local { .. } => {
                let (exit_code, stdout, stderr) =
                    crate::transport::pty::PtyTransport::exec_local(command, cwd).await?;
                Ok(ExecResult {
                    exit_code,
                    stdout,
                    stderr,
                })
            }
            #[cfg(feature = "ssh")]
            TransportSpec::Ssh { .. } => {
                let known_hosts = self
                    .log_dir
                    .parent()
                    .unwrap_or(&self.log_dir)
                    .join("known_hosts");
                let ssh_creds = crate::transport::ssh::SshCredentials {
                    secret: creds.secret,
                    key_passphrase: creds.key_passphrase,
                    jump_secret: creds.jump_secret,
                    jump_key_passphrase: creds.jump_key_passphrase,
                };
                let (exit_code, stdout, stderr) =
                    crate::transport::ssh::SshTransport::exec_command(
                        spec,
                        command,
                        &ssh_creds,
                        &known_hosts,
                    )
                    .await?;
                Ok(ExecResult {
                    exit_code,
                    stdout,
                    stderr,
                })
            }
            other => Err(anyhow!("command execution not supported on {}", other.label())),
        }
    }

    fn get(&self, id: Uuid) -> Result<Arc<Session>> {
        self.inner
            .lock()
            .map_err(|_| anyhow!("session map poisoned"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("no such session: {id}"))
    }

    /// Remove from the registry and flush taps exactly once.
    fn reap(&self, id: Uuid) {
        let removed = self.inner.lock().ok().and_then(|mut m| m.remove(&id));
        if let Some(s) = removed {
            s.taps.on_close();
        }
    }
}

/// Read from the transport, feed the taps, and hand coalesced batches to the UI.
///
/// The select loop also drives a flush tick: a session that emits a burst and
/// then goes idle must still land on disk promptly.
async fn pump(mut rx: mpsc::Receiver<Bytes>, taps: TapSet, on_output: OutputSink) {
    let mut ticker = tokio::time::interval(FLUSH_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let first = tokio::select! {
            biased;
            chunk = rx.recv() => match chunk {
                Some(c) => c,
                None => break,
            },
            _ = ticker.tick() => {
                taps.flush();
                continue;
            }
        };

        let mut batch = BytesMut::from(&first[..]);

        // Greedily absorb whatever else arrives inside the coalesce window.
        let mut ended = false;
        loop {
            if batch.len() >= MAX_BATCH {
                break;
            }
            match tokio::time::timeout(COALESCE_WINDOW, rx.recv()).await {
                Ok(Some(more)) => batch.extend_from_slice(&more),
                Ok(None) => {
                    ended = true;
                    break;
                }
                Err(_) => break, // window elapsed
            }
        }

        let out = batch.freeze();
        taps.on_data(Direction::Output, &out);
        on_output(out);

        if ended {
            break;
        }
    }
    taps.flush();
}
