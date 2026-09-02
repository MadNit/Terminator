//! SSH transport (russh).
//!
//! Mirrors the local PTY transport: request a remote PTY, run the login shell,
//! and stream bytes. The channel is split into read and write halves owned by
//! separate tasks -- see the comment at the split for why sharing one task
//! between both directions deadlocks.
//!
//! The authenticated session handle is also kept so a second channel can be
//! opened for SFTP. Multiplexing onto the existing connection is what keeps the
//! file browser from triggering a second password prompt.

use super::{SshAuth, Transport, TransportSpec};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{known_hosts, load_secret_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::sync::mpsc::error::TrySendError;

/// What the host key check decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HostKeyPolicy {
    /// Accept and remember unknown hosts; still reject *changed* keys.
    TofuAccept,
    /// Reject anything not already in known_hosts.
    Strict,
}

pub struct Client {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
    known_hosts: PathBuf,
}

impl client::Handler for Client {
    type Error = anyhow::Error;

    /// The single most security-critical function in this file. A blanket
    /// `Ok(true)` here would silently accept any man-in-the-middle.
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match known_hosts::check_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                // Unknown host.
                if self.policy == HostKeyPolicy::Strict {
                    bail!(
                        "unknown host key for {}:{} and strict checking is on",
                        self.host,
                        self.port
                    );
                }
                known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    server_public_key,
                    &self.known_hosts,
                )
                .context("failed to record new host key")?;
                tracing::info!(
                    "learned new host key for {}:{} ({})",
                    self.host,
                    self.port,
                    server_public_key.fingerprint(Default::default())
                );
                Ok(true)
            }
            // A *changed* key is never auto-accepted: this is the actual
            // MITM signal, and TOFU explicitly does not cover it.
            Err(e) => Err(anyhow!(
                "host key verification failed for {}:{}: {e}. If this host was \
                 legitimately rebuilt, remove its entry from {}",
                self.host,
                self.port,
                self.known_hosts.display()
            )),
        }
    }
}

/// Commands sent to the task that owns the channel.
enum Cmd {
    Data(Bytes),
    Resize { cols: u16, rows: u16 },
    Shutdown(oneshot::Sender<()>),
}

/// TCP + SSH banner exchange. Long enough for a slow VPN, short enough that a
/// wrong address reports back while the user still remembers what they typed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Authentication, measured separately: it starts only after the transport is
/// up, and some servers are slow to run PAM.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SshTransport {
    cmd: mpsc::Sender<Cmd>,
    rx: Option<mpsc::Receiver<Bytes>>,
    /// Kept alive for opening further channels (SFTP). `channel_open_session`
    /// takes `&self`, so an `Arc` is enough -- no lock is needed even though
    /// the reader task holds a reference at the same time.
    session: Arc<Handle<Client>>,
    /// The SFTP session is opened on first use and reused thereafter: setting
    /// it up costs a channel open plus a protocol handshake, which is far too
    /// much to repeat on every directory listing.
    sftp: tokio::sync::OnceCell<Arc<russh_sftp::client::SftpSession>>,
}

/// Runtime credential, resolved by the caller (never persisted in a profile).
#[derive(Default)]
pub struct SshCredentials {
    pub secret: Option<String>,
    pub key_passphrase: Option<String>,
    /// Optional credentials for jump host / bastion
    pub jump_secret: Option<String>,
    pub jump_key_passphrase: Option<String>,
}

impl SshTransport {
    pub fn session_handle(&self) -> Arc<Handle<Client>> {
        self.session.clone()
    }

    /// Establishes an authenticated SSH session handle (either direct or via ProxyJump / jump host).
    pub async fn establish_session(
        spec: &TransportSpec,
        creds: &SshCredentials,
        known_hosts_path: &PathBuf,
    ) -> Result<Handle<Client>> {
        let (host, port, user, auth, jump_host) = match spec {
            TransportSpec::Ssh {
                host,
                port,
                user,
                auth,
                jump_host,
            } => (host.clone(), *port, user.clone(), auth.clone(), jump_host.clone()),
            _ => bail!("establish_session only handles TransportSpec::Ssh"),
        };

        if let Some(parent) = known_hosts_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });

        let handler = Client {
            host: host.clone(),
            port,
            policy: HostKeyPolicy::TofuAccept,
            known_hosts: known_hosts_path.clone(),
        };

        if let Some(jump_spec) = jump_host {
            tracing::info!("Connecting via jump host: {:?}", jump_spec.label());
            let jump_creds = SshCredentials {
                secret: creds.jump_secret.clone(),
                key_passphrase: creds.jump_key_passphrase.clone(),
                jump_secret: None,
                jump_key_passphrase: None,
            };
            // Establish session to the bastion / jump box first
            let jump_session = Box::pin(Self::establish_session(&jump_spec, &jump_creds, known_hosts_path)).await?;
            // Open direct-tcpip channel through the bastion to the target destination
            let channel = jump_session
                .channel_open_direct_tcpip(&host, port as u32, "127.0.0.1", 22)
                .await
                .with_context(|| format!("jump host refused tunnel to target {host}:{port}"))?;
            let stream = channel.into_stream();

            let mut session = tokio::time::timeout(
                CONNECT_TIMEOUT,
                client::connect_stream(config, stream, handler),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "timed out after {}s connecting to {host}:{port} via jump host",
                    CONNECT_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("failed to connect to {host}:{port} via jump host"))?;

            tokio::time::timeout(
                AUTH_TIMEOUT,
                authenticate(&mut session, &user, &auth, creds),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "timed out after {}s authenticating as {user}@{host}",
                    AUTH_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("authentication failed for {user}@{host}"))?;

            return Ok(session);
        }

        // Direct connection without jump host
        let mut session = tokio::time::timeout(
            CONNECT_TIMEOUT,
            client::connect(config, (host.as_str(), port), handler),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out after {}s connecting to {host}:{port}",
                CONNECT_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("failed to connect to {host}:{port}"))?;

        tokio::time::timeout(
            AUTH_TIMEOUT,
            authenticate(&mut session, &user, &auth, creds),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out after {}s authenticating as {user}@{host}",
                AUTH_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("authentication failed for {user}@{host}"))?;

        Ok(session)
    }

    pub async fn exec(&self, command: &str) -> Result<(i32, String, String)> {
        Self::exec_on_session(&self.session, command).await
    }

    pub async fn exec_command(
        spec: &TransportSpec,
        command: &str,
        creds: &SshCredentials,
        known_hosts_path: &PathBuf,
    ) -> Result<(i32, String, String)> {
        let session = Self::establish_session(spec, creds, known_hosts_path).await?;
        let session = Arc::new(session);
        Self::exec_on_session(&session, command).await
    }

    pub async fn exec_on_session(
        session: &Arc<Handle<Client>>,
        command: &str,
    ) -> Result<(i32, String, String)> {
        let mut channel = session.channel_open_session().await?;
        channel.exec(true, command).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_code = 0;

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    stdout.extend_from_slice(&data);
                }
                ChannelMsg::ExtendedData { data, .. } => {
                    stderr.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus { exit_status } => {
                    exit_code = exit_status as i32;
                }
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }

        let stdout_str = String::from_utf8_lossy(&stdout).to_string();
        let stderr_str = String::from_utf8_lossy(&stderr).to_string();
        Ok((exit_code, stdout_str, stderr_str))
    }

    pub async fn connect(
        spec: &TransportSpec,
        cols: u16,
        rows: u16,
        creds: SshCredentials,
        known_hosts_path: PathBuf,
    ) -> Result<Self> {
        let session = Self::establish_session(spec, &creds, &known_hosts_path).await?;
        let session = Arc::new(session);
        let channel = session.channel_open_session().await?;
        let channel_id = channel.id();
        channel
            .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await
            .context("remote refused a PTY")?;
        channel
            .request_shell(true)
            .await
            .context("remote refused a shell")?;

        // Output channel: sized for a few seconds of fast shell
        // output before the SSE consumer (Tauri side) needs to
        // keep up. 8192 is enough for ~1 MB of burst output at the
        // typical 64 KiB chunk size. If it ever fills the reader
        // drops new output rather than blocking -- a frozen
        // reader would cascade into a frozen input pipeline
        // (the SSH transport pauses, the writer can't make
        // progress, the cmd channel fills, `SshTransport::write`
        // blocks, the HTTP handler blocks, the UI freezes).
        let (out_tx, out_rx) = mpsc::channel::<Bytes>(8192);
        // Command channel: keystrokes queued for the writer task.
        // 1024 entries gives the writer enough headroom to
        // absorb normal typing bursts even when the network is
        // slow. Paired with `try_send` in `SshTransport::write`
        // so a full channel drops individual keystrokes with a
        // warning rather than blocking the entire input
        // pipeline (which is what froze the UI for ~5 s
        // before this fix: the channel was 64 and
        // `send().await` blocked indefinitely while the
        // writer was mid-`session.data()` on a slow network).
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(1024);

        // Reads and writes run in *separate* tasks, and that separation is
        // load-bearing -- a single task serving both directions deadlocks.
        //
        // Splitting the channel gives the read side its own task to drain `wait()`,
        // while the write side sends data directly through the session handle (`session.data`).
        // Direct `session.data` avoids russh's internal `ChannelTx` Tokio Notify race
        // condition where `WatchNotification` can lose window adjustment notifications.
        let (mut read_half, write_half) = channel.split();

        // Reader: drain the channel and forward output. Holds a reference to
        // the session handle because dropping the last one tears the connection
        // down, and the read side is what observes the channel closing.
        let reader_session = session.clone();
        tokio::spawn(async move {
            let _session = reader_session;
            while let Some(msg) = read_half.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        // `try_send` rather than `send().await`:
                        // a slow SSE consumer on the Tauri side
                        // must not back-pressure the reader task.
                        // If the consumer falls behind by the
                        // 8192-buffer worth, we drop the new
                        // output (with a one-shot warning per
                        // back-pressure event) rather than
                        // blocking, which would freeze the SSH
                        // transport and via the cascade
                        // described above freeze the input
                        // pipeline too.
                        if out_tx.try_send(Bytes::copy_from_slice(&data)).is_err() {
                            static WARNED: std::sync::atomic::AtomicBool =
                                std::sync::atomic::AtomicBool::new(false);
                            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                tracing::warn!(
                                    "ssh output buffer full; reader dropping bytes until consumer catches up"
                                );
                            }
                        }
                    }
                    // stderr on a shell channel; show it inline as a terminal
                    // would, rather than discarding it.
                    ChannelMsg::ExtendedData { data, .. } => {
                        if out_tx.try_send(Bytes::copy_from_slice(&data)).is_err() {
                            // Same back-pressure handling as above;
                            // we just rely on the static guard to
                            // keep the warning count down.
                        }
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            // Dropping out_tx closes the pump, which ends the session.
        });

        // Writer: keystrokes, resizes and shutdown.
        let writer_session = session.clone();
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Cmd::Data(b) => {
                        if b.is_empty() {
                            continue;
                        }
                        match tokio::time::timeout(Duration::from_secs(5), writer_session.data(channel_id, b)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(_undelivered)) => {
                                tracing::warn!("SSH writer session.data failed, channel closed");
                                break;
                            }
                            Err(_) => {
                                tracing::warn!("SSH writer session.data timed out after 5s");
                            }
                        }
                    }
                    Cmd::Resize { cols, rows } => {
                        let _ = write_half
                            .window_change(cols as u32, rows as u32, 0, 0)
                            .await;
                    }
                    Cmd::Shutdown(ack) => {
                        let _ = write_half.eof().await;
                        let _ = write_half.close().await;
                        let _ = ack.send(());
                        break;
                    }
                }
            }
        });

        Ok(Self {
            cmd: cmd_tx,
            rx: Some(out_rx),
            session,
            sftp: tokio::sync::OnceCell::new(),
        })
    }

    /// The SFTP session for this connection, opened on first use.
    ///
    /// Runs over a *second* channel on the same authenticated connection, so
    /// the user is never re-prompted and the shell channel is untouched by
    /// file transfers.
    ///
    /// `get_or_try_init` means a failure is not cached: if the server has SFTP
    /// disabled, or the subsystem request races a disconnect, a later attempt
    /// can still succeed rather than the drawer being permanently dead.
    pub async fn sftp(&self) -> Result<Arc<russh_sftp::client::SftpSession>> {
        self.sftp
            .get_or_try_init(|| async {
                let channel = self
                    .session
                    .channel_open_session()
                    .await
                    .context("failed to open a channel for SFTP")?;
                channel
                    .request_subsystem(true, "sftp")
                    .await
                    .context("remote refused the SFTP subsystem")?;
                let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
                    .await
                    .context("SFTP handshake failed")?;
                Ok::<_, anyhow::Error>(Arc::new(sftp))
            })
            .await
            .cloned()
    }
}

/// Try the requested method, and fall back to the agent where it makes sense.
async fn authenticate(
    session: &mut Handle<Client>,
    user: &str,
    auth: &SshAuth,
    creds: &SshCredentials,
) -> Result<()> {
    let result = match auth {
        SshAuth::Password => {
            let pw = creds
                .secret
                .as_deref()
                .ok_or_else(|| anyhow!("password auth selected but no password supplied"))?;
            session.authenticate_password(user, pw).await?
        }
        SshAuth::Key { path } => {
            let expanded = expand_tilde(path);
            let key = load_secret_key(&expanded, creds.key_passphrase.as_deref())
                .with_context(|| format!("failed to load key {expanded}"))?;
            session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))
                .await?
        }
        SshAuth::Agent | SshAuth::AgentSocket { .. } => {
            #[cfg(unix)]
            {
                let mut agent = match auth {
                    SshAuth::AgentSocket { socket_path } => {
                        let expanded = expand_tilde(socket_path);
                        let stream = tokio::net::UnixStream::connect(&expanded)
                            .await
                            .with_context(|| format!("failed to connect to SSH agent socket at {expanded}"))?;
                        russh::keys::agent::client::AgentClient::connect(stream)
                    }
                    _ => russh::keys::agent::client::AgentClient::connect_env()
                        .await
                        .context("no ssh-agent available (is SSH_AUTH_SOCK set?)")?,
                };
                let identities = agent.request_identities().await?;
                if identities.is_empty() {
                    bail!("ssh-agent has no identities loaded (try `ssh-add` or check if your security key / 1Password agent is unlocked)");
                }
                let mut last = None;
                for id in identities {
                    let key = id.public_key().into_owned();
                    let r = session
                        .authenticate_publickey_with(user, key, None, &mut agent)
                        .await?;
                    if matches!(r, AuthResult::Success) {
                        return Ok(());
                    }
                    last = Some(r);
                }
                last.ok_or_else(|| anyhow!("no agent identity was accepted by the remote server"))?
            }
            #[cfg(not(unix))]
            {
                bail!("ssh-agent authentication is only supported on Unix platforms");
            }
        }
    };

    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure {
            remaining_methods, ..
        } => bail!("server rejected credentials; it accepts: {remaining_methods:?}"),
    }
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    p.to_string()
}

#[async_trait]
impl Transport for SshTransport {
    fn output(&mut self) -> mpsc::Receiver<Bytes> {
        self.rx.take().expect("output() called more than once")
    }

    async fn write(&self, data: Bytes) -> Result<()> {
        // `try_send` (not `send().await`) is load-bearing for
        // the input pipeline. The Tauri side awaits the
        // HTTP response from `POST /sessions/{id}/input`; if
        // `cmd_tx.send().await` blocked on a full channel
        // here, that block propagates up through the
        // Tauri command to the UI and freezes input even
        // though output still flows. With `try_send` a
        // full channel returns immediately and the
        // keystroke is dropped (with a warning) instead of
        // stalling the entire input pipeline. The capacity
        // bump (64 -> 1024) makes this a rare event.
        match self.cmd.try_send(Cmd::Data(data)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                static WARNED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::warn!(
                        "ssh cmd channel full; input being dropped. \
                         network likely slow or UI not draining output fast enough"
                    );
                }
                Ok(())
            }
            Err(TrySendError::Closed(_)) => Err(anyhow!("ssh channel closed")),
        }
    }

    async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.cmd
            .send(Cmd::Resize { cols, rows })
            .await
            .map_err(|_| anyhow!("ssh channel closed"))
    }

    async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self.cmd.send(Cmd::Shutdown(tx)).await.is_ok() {
            // Bounded: never let a dead connection hang the UI on close.
            let _ = tokio::time::timeout(Duration::from_secs(3), rx).await;
        }
        Ok(())
    }

    async fn files(&self) -> Result<Arc<dyn crate::files::RemoteFs>> {
        let sftp = self.sftp().await?;
        Ok(Arc::new(crate::transport::sftp::SftpFs::new(sftp)) as Arc<dyn crate::files::RemoteFs>)
    }
}

/// Unused today but kept adjacent to the disconnect path for clarity.
#[allow(dead_code)]
const DISCONNECT_REASON: Disconnect = Disconnect::ByApplication;
