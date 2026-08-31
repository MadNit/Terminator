//! Transport abstraction.
//!
//! A transport is anything that produces and consumes a byte stream: a local
//! PTY, an SSH channel, or (later) an RDP virtual channel. Sessions are written
//! against this trait only, so adding SSH is a new impl rather than a rewrite.

pub mod pty;
#[cfg(feature = "ssh")]
pub mod sftp;
#[cfg(feature = "ssh")]
pub mod ssh;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

/// How the user proves who they are. The secret itself is deliberately absent,
/// so a profile can be persisted without ever holding a credential.
///
/// Defined here rather than in `ssh.rs` because profiles and the UI reference
/// it even in builds without the `ssh` feature compiled in.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum SshAuth {
    /// Use the running ssh-agent. Safest default: no secret ever reaches us.
    #[default]
    Agent,
    Password,
    Key {
        path: String,
    },
    /// Custom SSH agent socket (e.g. 1Password SSH agent, YubiKey/GPG agent, or specific path)
    #[serde(rename = "agent_socket")]
    AgentSocket {
        socket_path: String,
    },
}

/// Where a session's bytes come from.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TransportSpec {
    Local {
        /// None = platform default shell.
        shell: Option<String>,
        cwd: Option<String>,
    },
    Ssh {
        host: String,
        port: u16,
        user: String,
        #[serde(default)]
        auth: SshAuth,
        /// Optional intermediate jump host / bastion for ProxyJump (ssh -J).
        #[serde(default)]
        jump_host: Option<Box<TransportSpec>>,
    },
    Rdp {
        host: String,
        port: u16,
        user: String,
        /// Windows domain. `None` means a local account on the target.
        ///
        /// Defaulted so RDP profiles saved before this field existed still
        /// deserialize instead of vanishing from the sidebar.
        #[serde(default)]
        domain: Option<String>,
    },
}

impl TransportSpec {
    pub fn label(&self) -> String {
        match self {
            TransportSpec::Local { shell, .. } => shell
                .as_deref()
                .and_then(|s| s.rsplit(['/', '\\']).next())
                .unwrap_or("shell")
                .to_string(),
            TransportSpec::Ssh { host, user, .. } => format!("{user}@{host}"),
            TransportSpec::Rdp { host, user, .. } => format!("rdp {user}@{host}"),
        }
    }
}

/// A live, bidirectional byte stream.
///
/// `Sync` is required because sessions are shared across tasks: the UI writes
/// keystrokes while the pump task reads output concurrently.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Bytes coming from the remote end. Closed when the session ends.
    fn output(&mut self) -> mpsc::Receiver<Bytes>;

    /// Send bytes to the remote end (keystrokes, paste, etc).
    async fn write(&self, data: Bytes) -> anyhow::Result<()>;

    /// Window size changed.
    async fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;

    /// Terminate the underlying process/channel.
    async fn shutdown(&self) -> anyhow::Result<()>;

    /// A file browser for the far end of this transport, if it has one.
    ///
    /// Defaults to unsupported so local PTY and RDP transports need no stub.
    /// Async because opening it is a network round trip for SSH.
    async fn files(&self) -> anyhow::Result<std::sync::Arc<dyn crate::files::RemoteFs>> {
        anyhow::bail!("this session does not support file transfer")
    }
}
