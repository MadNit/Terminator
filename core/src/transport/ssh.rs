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

/// What the host key check decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HostKeyPolicy {
    /// Accept and remember unknown hosts; still reject *changed* keys.
    TofuAccept,
    /// Reject anything not already in known_hosts.
    Strict,
}

struct Client {
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
}

impl SshTransport {
    pub async fn connect(
        spec: &TransportSpec,
        cols: u16,
        rows: u16,
        creds: SshCredentials,
        known_hosts_path: PathBuf,
    ) -> Result<Self> {
        let (host, port, user, auth) = match spec {
            TransportSpec::Ssh {
                host,
                port,
                user,
                auth,
            } => (host.clone(), *port, user.clone(), auth.clone()),
            _ => bail!("SshTransport only handles TransportSpec::Ssh"),
        };

        if let Some(parent) = known_hosts_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let config = Arc::new(client::Config {
            // Without keepalives, NAT/firewall idle timeouts silently kill
            // long-lived sessions -- the classic "my SSH froze" complaint.
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });

        let handler = Client {
            host: host.clone(),
            port,
            policy: HostKeyPolicy::TofuAccept,
            known_hosts: known_hosts_path,
        };

        // An unreachable host would otherwise sit in the OS TCP backoff for
        // ~75s with the UI stuck on "connecting", and a server that completes
        // the TCP handshake but never sends a banner hangs indefinitely.
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

        // Keyboard-interactive servers can stall here just as easily.
        tokio::time::timeout(
            AUTH_TIMEOUT,
            authenticate(&mut session, &user, &auth, &creds),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "timed out after {}s authenticating as {user}@{host}",
                AUTH_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("authentication failed for {user}@{host}"))?;

        let session = Arc::new(session);
        let channel = session.channel_open_session().await?;
        channel
            .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
            .await
            .context("remote refused a PTY")?;
        channel
            .request_shell(true)
            .await
            .context("remote refused a shell")?;

        let (out_tx, out_rx) = mpsc::channel::<Bytes>(256);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(64);

        // Reads and writes run in *separate* tasks, and that separation is
        // load-bearing -- a single task serving both directions deadlocks.
        //
        // The chain: russh delivers channel messages over a bounded mpsc
        // (channel_buffer_size, 100 by default). Writing blocks once the SSH
        // send window is exhausted, and the window is only replenished when
        // russh's session loop processes a WindowAdjust from the server. If one
        // task owns both directions, blocking on a write stops it draining
        // `wait()`; the 100-slot buffer fills; russh's session loop blocks
        // delivering into it; and a blocked session loop can never process the
        // WindowAdjust that would unblock the write. Typing dies permanently
        // while output keeps flowing.
        //
        // Splitting the channel gives each direction its own task, so the read
        // side keeps draining no matter what the write side is doing.
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
                        if out_tx.send(Bytes::copy_from_slice(&data)).await.is_err() {
                            break;
                        }
                    }
                    // stderr on a shell channel; show it inline as a terminal
                    // would, rather than discarding it.
                    ChannelMsg::ExtendedData { data, .. } => {
                        if out_tx.send(Bytes::copy_from_slice(&data)).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            // Dropping out_tx closes the pump, which ends the session.
        });

        // Writer: keystrokes, resizes and shutdown.
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    Cmd::Data(b) => {
                        if write_half.data(&b[..]).await.is_err() {
                            break;
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
        SshAuth::Agent => {
            let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                .await
                .context("no ssh-agent available (is SSH_AUTH_SOCK set?)")?;
            let identities = agent.request_identities().await?;
            if identities.is_empty() {
                bail!("ssh-agent has no identities loaded (try `ssh-add`)");
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
            last.ok_or_else(|| anyhow!("no agent identity was accepted"))?
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
        self.cmd
            .send(Cmd::Data(data))
            .await
            .map_err(|_| anyhow!("ssh channel closed"))
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
