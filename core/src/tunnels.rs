//! SSH port forwarding tunnels (Local -L, Remote -R, Dynamic SOCKS5 -D).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Mutex, RwLock};

use crate::transport::ssh::{SshCredentials, SshTransport};
use crate::transport::TransportSpec;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    /// Local port forwarding (-L local_port:remote_host:remote_port)
    Local,
    /// Remote port forwarding (-R remote_port:local_host:local_port)
    Remote,
    /// Dynamic SOCKS5 proxy (-D local_port)
    Dynamic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub id: String,
    pub name: String,
    pub kind: TunnelKind,
    /// Linked SSH profile or explicit SSH spec
    pub ssh_spec: TransportSpec,
    /// Local bind address (e.g. "127.0.0.1" or "0.0.0.0")
    #[serde(default = "default_bind_addr")]
    pub local_addr: String,
    /// Local port to listen on or forward to
    pub local_port: u16,
    /// Remote host (for Local: target destination host reached from remote SSH server;
    /// for Remote: local destination target host reached from local machine)
    #[serde(default = "default_target_host")]
    pub target_host: String,
    /// Remote/target port
    #[serde(default)]
    pub target_port: u16,
}

fn default_bind_addr() -> String {
    "127.0.0.1".to_string()
}

fn default_target_host() -> String {
    "127.0.0.1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelStatus {
    pub id: String,
    pub active: bool,
    pub error: Option<String>,
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub active_connections: usize,
}

struct RunningTunnel {
    shutdown_tx: Option<oneshot::Sender<()>>,
    status: Arc<RwLock<TunnelStatus>>,
}

#[derive(Clone)]
pub struct TunnelManager {
    running: Arc<Mutex<HashMap<String, RunningTunnel>>>,
    known_hosts_path: PathBuf,
}

impl TunnelManager {
    pub fn new(known_hosts_path: PathBuf) -> Self {
        Self {
            running: Arc::new(Mutex::new(HashMap::new())),
            known_hosts_path,
        }
    }

    pub async fn list_active(&self) -> Vec<TunnelStatus> {
        let running = self.running.lock().await;
        let mut statuses = Vec::new();
        for (_, rt) in running.iter() {
            let s = rt.status.read().await;
            statuses.push(s.clone());
        }
        statuses
    }

    pub async fn stop_tunnel(&self, id: &str) -> Result<()> {
        let mut running = self.running.lock().await;
        if let Some(mut rt) = running.remove(id) {
            if let Some(tx) = rt.shutdown_tx.take() {
                let _ = tx.send(());
            }
            let mut s = rt.status.write().await;
            s.active = false;
        }
        Ok(())
    }

    pub async fn start_tunnel(&self, config: TunnelConfig, creds: SshCredentials) -> Result<TunnelStatus> {
        self.stop_tunnel(&config.id).await.ok();

        let status = Arc::new(RwLock::new(TunnelStatus {
            id: config.id.clone(),
            active: true,
            error: None,
            bytes_rx: 0,
            bytes_tx: 0,
            active_connections: 0,
        }));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let rt = RunningTunnel {
            shutdown_tx: Some(shutdown_tx),
            status: status.clone(),
        };

        // Connect SSH transport
        let ssh_client = match SshTransport::connect(
            &config.ssh_spec,
            80,
            24,
            creds,
            self.known_hosts_path.clone(),
        )
        .await
        {
            Ok(client) => client,
            Err(e) => {
                let err_msg = format!("SSH connection failed: {e}");
                let mut s = status.write().await;
                s.active = false;
                s.error = Some(err_msg.clone());
                bail!(err_msg);
            }
        };

        let session = ssh_client.session_handle();
        let status_clone = status.clone();
        let config_clone = config.clone();

        tokio::spawn(async move {
            let res = match config_clone.kind {
                TunnelKind::Local => {
                    run_local_tunnel(config_clone, session, status_clone.clone(), shutdown_rx).await
                }
                TunnelKind::Dynamic => {
                    run_dynamic_socks5_tunnel(config_clone, session, status_clone.clone(), shutdown_rx).await
                }
                TunnelKind::Remote => {
                    run_remote_tunnel(config_clone, session, status_clone.clone(), shutdown_rx).await
                }
            };

            let mut s = status_clone.write().await;
            s.active = false;
            if let Err(e) = res {
                tracing::warn!("Tunnel stopped with error: {e}");
                s.error = Some(e.to_string());
            }
        });

        {
            let mut running = self.running.lock().await;
            running.insert(config.id.clone(), rt);
        }

        let s = status.read().await;
        Ok(s.clone())
    }
}

async fn run_local_tunnel(
    config: TunnelConfig,
    session: Arc<russh::client::Handle<crate::transport::ssh::Client>>,
    status: Arc<RwLock<TunnelStatus>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let bind_addr = format!("{}:{}", config.local_addr, config.local_port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind local port {bind_addr}"))?;

    tracing::info!("Local tunnel listening on {bind_addr} -> {}:{}", config.target_host, config.target_port);

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("Local tunnel {} shut down", config.id);
                break;
            }
            accept_res = listener.accept() => {
                let (tcp_stream, peer_addr) = match accept_res {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!("Failed accepting connection on {bind_addr}: {e}");
                        continue;
                    }
                };

                let session = session.clone();
                let status = status.clone();
                let target_host = config.target_host.clone();
                let target_port = config.target_port as u32;

                tokio::spawn(async move {
                    {
                        let mut s = status.write().await;
                        s.active_connections += 1;
                    }

                    if let Err(e) = handle_direct_tcpip(tcp_stream, session, &target_host, target_port, peer_addr, status.clone()).await {
                        tracing::debug!("Direct tcpip connection error: {e}");
                    }

                    {
                        let mut s = status.write().await;
                        if s.active_connections > 0 {
                            s.active_connections -= 1;
                        }
                    }
                });
            }
        }
    }

    Ok(())
}

async fn handle_direct_tcpip(
    mut tcp_stream: TcpStream,
    session: Arc<russh::client::Handle<crate::transport::ssh::Client>>,
    target_host: &str,
    target_port: u32,
    peer_addr: SocketAddr,
    status: Arc<RwLock<TunnelStatus>>,
) -> Result<()> {
    let channel = session
        .channel_open_direct_tcpip(
            target_host,
            target_port,
            &peer_addr.ip().to_string(),
            peer_addr.port() as u32,
        )
        .await
        .context("SSH server refused channel_open_direct_tcpip")?;

    let mut stream = channel.into_stream();
    let (mut rx_bytes, mut tx_bytes) = (0u64, 0u64);

    let (mut local_read, mut local_write) = tcp_stream.split();
    let (mut ssh_read, mut ssh_write) = tokio::io::split(&mut stream);

    let client_to_server = async {
        let mut buf = [0u8; 16384];
        loop {
            let n = local_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ssh_write.write_all(&buf[..n]).await?;
            ssh_write.flush().await?;
            tx_bytes += n as u64;
            let mut s = status.write().await;
            s.bytes_tx += n as u64;
        }
        Ok::<(), anyhow::Error>(())
    };

    let server_to_client = async {
        let mut buf = [0u8; 16384];
        loop {
            let n = ssh_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            local_write.write_all(&buf[..n]).await?;
            local_write.flush().await?;
            rx_bytes += n as u64;
            let mut s = status.write().await;
            s.bytes_rx += n as u64;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        res1 = client_to_server => res1?,
        res2 = server_to_client => res2?,
    }

    Ok(())
}

async fn run_dynamic_socks5_tunnel(
    config: TunnelConfig,
    session: Arc<russh::client::Handle<crate::transport::ssh::Client>>,
    status: Arc<RwLock<TunnelStatus>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let bind_addr = format!("{}:{}", config.local_addr, config.local_port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind SOCKS5 proxy on {bind_addr}"))?;

    tracing::info!("Dynamic SOCKS5 proxy listening on {bind_addr}");

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                tracing::info!("Dynamic SOCKS5 tunnel {} shut down", config.id);
                break;
            }
            accept_res = listener.accept() => {
                let (tcp_stream, peer_addr) = match accept_res {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!("Failed accepting connection on {bind_addr}: {e}");
                        continue;
                    }
                };

                let session = session.clone();
                let status = status.clone();

                tokio::spawn(async move {
                    {
                        let mut s = status.write().await;
                        s.active_connections += 1;
                    }

                    if let Err(e) = handle_socks5_client(tcp_stream, session, peer_addr, status.clone()).await {
                        tracing::debug!("SOCKS5 client error: {e}");
                    }

                    {
                        let mut s = status.write().await;
                        if s.active_connections > 0 {
                            s.active_connections -= 1;
                        }
                    }
                });
            }
        }
    }

    Ok(())
}

async fn handle_socks5_client(
    mut stream: TcpStream,
    session: Arc<russh::client::Handle<crate::transport::ssh::Client>>,
    peer_addr: SocketAddr,
    status: Arc<RwLock<TunnelStatus>>,
) -> Result<()> {
    // SOCKS5 greeting handshake: [VER, NMETHODS, METHODS...]
    let mut ver_methods = [0u8; 2];
    stream.read_exact(&mut ver_methods).await?;
    if ver_methods[0] != 0x05 {
        bail!("unsupported SOCKS version: {}", ver_methods[0]);
    }

    let num_methods = ver_methods[1] as usize;
    let mut methods = vec![0u8; num_methods];
    stream.read_exact(&mut methods).await?;

    // Respond NO AUTH REQUIRED (0x00)
    stream.write_all(&[0x05, 0x00]).await?;
    stream.flush().await?;

    // SOCKS5 request: [VER, CMD, RSV, ATYP, DST.ADDR, DST.PORT]
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    if header[0] != 0x05 {
        bail!("invalid SOCKS version in request");
    }
    let cmd = header[1];
    if cmd != 0x01 {
        // Only CONNECT (0x01) supported
        stream.write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.ok();
        bail!("unsupported SOCKS5 command {cmd}");
    }

    let atyp = header[3];
    let (target_host, target_port) = match atyp {
        0x01 => {
            // IPv4
            let mut ip_bytes = [0u8; 4];
            stream.read_exact(&mut ip_bytes).await?;
            let mut port_bytes = [0u8; 2];
            stream.read_exact(&mut port_bytes).await?;
            let port = u16::from_be_bytes(port_bytes);
            let ip = std::net::Ipv4Addr::from(ip_bytes);
            (ip.to_string(), port)
        }
        0x03 => {
            // Domain name: [LEN, DOMAIN...]
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            let mut port_bytes = [0u8; 2];
            stream.read_exact(&mut port_bytes).await?;
            let port = u16::from_be_bytes(port_bytes);
            let host_str = String::from_utf8_lossy(&domain).to_string();
            (host_str, port)
        }
        0x04 => {
            // IPv6
            let mut ip_bytes = [0u8; 16];
            stream.read_exact(&mut ip_bytes).await?;
            let mut port_bytes = [0u8; 2];
            stream.read_exact(&mut port_bytes).await?;
            let port = u16::from_be_bytes(port_bytes);
            let ip = std::net::Ipv6Addr::from(ip_bytes);
            (ip.to_string(), port)
        }
        _ => {
            stream.write_all(&[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.ok();
            bail!("unsupported address type {atyp}");
        }
    };

    // Open direct-tcpip to remote target
    let channel = match session
        .channel_open_direct_tcpip(
            &target_host,
            target_port as u32,
            &peer_addr.ip().to_string(),
            peer_addr.port() as u32,
        )
        .await
    {
        Ok(ch) => {
            // SOCKS5 reply SUCCESS (0x00)
            stream.write_all(&[0x05, 0x00, 0x00, 0x01, 127, 0, 0, 1, 0, 0]).await?;
            stream.flush().await?;
            ch
        }
        Err(e) => {
            // SOCKS5 reply GENERAL FAILURE (0x01)
            stream.write_all(&[0x05, 0x01, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await.ok();
            return Err(e.into());
        }
    };

    let mut ssh_stream = channel.into_stream();
    let (mut local_read, mut local_write) = stream.split();
    let (mut ssh_read, mut ssh_write) = tokio::io::split(&mut ssh_stream);

    let client_to_server = async {
        let mut buf = [0u8; 16384];
        loop {
            let n = local_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ssh_write.write_all(&buf[..n]).await?;
            ssh_write.flush().await?;
            let mut s = status.write().await;
            s.bytes_tx += n as u64;
        }
        Ok::<(), anyhow::Error>(())
    };

    let server_to_client = async {
        let mut buf = [0u8; 16384];
        loop {
            let n = ssh_read.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            local_write.write_all(&buf[..n]).await?;
            local_write.flush().await?;
            let mut s = status.write().await;
            s.bytes_rx += n as u64;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        res1 = client_to_server => res1?,
        res2 = server_to_client => res2?,
    }

    Ok(())
}

async fn run_remote_tunnel(
    config: TunnelConfig,
    session: Arc<russh::client::Handle<crate::transport::ssh::Client>>,
    _status: Arc<RwLock<TunnelStatus>>,
    mut shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    // Request remote forward on remote server
    let remote_port = session
        .tcpip_forward(&config.target_host, config.target_port as u32)
        .await
        .context("SSH server refused tcpip_forward request")?;

    tracing::info!(
        "Remote tunnel active: remote port {} forwarded to local {}:{}",
        remote_port,
        config.local_addr,
        config.local_port
    );

    tokio::select! {
        _ = &mut shutdown_rx => {
            session.cancel_tcpip_forward(&config.target_host, config.target_port as u32).await.ok();
            tracing::info!("Remote tunnel {} shut down", config.id);
        }
    }

    Ok(())
}
