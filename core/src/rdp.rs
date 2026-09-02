//! RDP client.
//!
//! # Why this is not a [`Transport`](crate::transport::Transport)
//!
//! Every other session in this app is a byte stream: bytes in from the
//! keyboard, bytes out to a terminal emulator, and taps in the middle that log
//! and parse them. RDP is not that. It is a *framebuffer* protocol -- the
//! server sends compressed rectangle updates, the client sends structured
//! input events (scancodes, mouse coordinates, wheel deltas). There is no
//! meaningful byte stream to tap, nothing for the VT parser to read, and
//! "resize" means renegotiating a virtual channel rather than an ioctl.
//!
//! Forcing it through `Transport` would mean inventing a fake byte encoding on
//! both sides of the IPC boundary and then immediately decoding it again. So
//! RDP gets its own parallel path: [`RdpManager`] here, [`SessionManager`]
//! there. They share the app's plumbing (profiles, secrets, tabs) and nothing
//! else.
//!
//! # Task layout
//!
//! ```text
//!   engine task            writer task
//!   ───────────            ───────────
//!   owns ActiveStage       owns the socket write half
//!   owns DecodedImage      does nothing but drain `out_rx`
//!   owns input Database
//!        │  bytes to send
//!        └──────────────►  out_tx ──►
//! ```
//!
//! The split is deliberate and load-bearing, for exactly the reason the SSH
//! transport needs one (see `transport/ssh.rs`): if the task that reads from
//! the server were also the task that writes to it, a full socket send buffer
//! would stop us reading, TCP backpressure would stop the server sending, and
//! the session would wedge with no way out. `ActiveStage` and `DecodedImage`
//! both need `&mut` and must stay in one task, so that task must never await
//! the socket directly -- it hands bytes to the writer and moves on.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use uuid::Uuid;

use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::{
    self, ClientConnector, ConnectionResult, Credentials as RdpCredentials, DesktopSize, ServerName,
};
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::cliprdr::backend::CliprdrBackend;
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataResponse, LockDataId,
};
use ironrdp::cliprdr::CliprdrClient;
use ironrdp::core::{AsAny, IntoOwned};
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp::pdu::Action;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{ActiveStage, ActiveStageOutput};
use ironrdp::svc::SvcMessage;
use ironrdp_tokio::{split_tokio_framed, TokioFramed};

/// What to connect to.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RdpConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub domain: Option<String>,
    pub width: u16,
    pub height: u16,
}

/// What the UI sends us.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RdpInput {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseDown {
        button: u8,
    },
    MouseUp {
        button: u8,
    },
    Wheel {
        delta: i16,
        horizontal: bool,
    },
    KeyDown {
        scancode: u16,
    },
    KeyUp {
        scancode: u16,
    },
    UnicodeChar {
        ch: char,
    },
    /// Sent when the pane loses focus. Without it a modifier held at the
    /// moment focus left stays latched on the server forever, and every
    /// subsequent keystroke arrives shifted or control-modified.
    ReleaseAll,
    /// The local clipboard text changed. The daemon's CLIPRDR
    /// backend uses this to (re-)advertise the local clipboard to
    /// the RDP server the next time the channel asks for a format
    /// list. Text only for v1; the wire format leaves room for
    /// `format` later if we add HTML or image support.
    LocalClipboard {
        text: String,
    },
}

/// What we send the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RdpEvent {
    /// A dirty rectangle, tightly packed RGBA, row-major, `w * h * 4` bytes.
    Frame {
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        /// Base64 because this crosses a JSON IPC boundary.
        rgba: String,
    },
    /// The desktop changed size (initial handshake, or a server-driven
    /// reactivation after our resize request).
    Resized {
        width: u16,
        height: u16,
    },
    Disconnected {
        reason: String,
    },
    /// The remote desktop's clipboard changed and the daemon
    /// pulled the text back over CLIPRDR. The Tauri side
    /// writes this into the OS clipboard. Text only for v1.
    RemoteClipboard {
        text: String,
    },
}

/// Commands into the engine task.
enum Cmd {
    Input(Vec<RdpInput>),
    Resize { width: u16, height: u16 },
    Shutdown,
}

/// A live RDP session handle.
pub struct RdpSession {
    cmd: mpsc::Sender<Cmd>,
}

impl RdpSession {
    pub fn input(&self, ops: Vec<RdpInput>) -> Result<()> {
        self.cmd
            .try_send(Cmd::Input(ops))
            .map_err(|_| anyhow!("rdp session is not accepting input"))
    }

    pub fn resize(&self, width: u16, height: u16) -> Result<()> {
        self.cmd
            .try_send(Cmd::Resize { width, height })
            .map_err(|_| anyhow!("rdp session is not accepting commands"))
    }

    pub fn shutdown(&self) {
        let _ = self.cmd.try_send(Cmd::Shutdown);
    }
}

/// Registry of live RDP sessions, mirroring `SessionManager` for byte streams.
#[derive(Clone, Default)]
pub struct RdpManager {
    inner: Arc<Mutex<HashMap<Uuid, Arc<RdpSession>>>>,
}

impl RdpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connect, then start pumping. Returns once the desktop is active, so a
    /// failure here is a real connection failure the user can act on.
    pub async fn open(
        &self,
        cfg: RdpConfig,
        events: mpsc::Sender<RdpEvent>,
    ) -> Result<(Uuid, u16, u16, mpsc::UnboundedSender<BackendAction>)> {
        self.open_with_timeout(cfg, events, CONNECT_TIMEOUT).await
    }

    /// As [`open`](Self::open), but with an explicit handshake deadline.
    ///
    /// Exists so tests can prove the deadline is enforced without sitting
    /// through the full production timeout on every run.
    pub async fn open_with_timeout(
        &self,
        cfg: RdpConfig,
        events: mpsc::Sender<RdpEvent>,
        timeout: Duration,
    ) -> Result<(Uuid, u16, u16, mpsc::UnboundedSender<BackendAction>)> {
        let (session, width, height, cliprdr_actions) =
            connect(cfg, events, timeout).await?;
        let id = Uuid::new_v4();
        self.inner
            .lock()
            .map_err(|_| anyhow!("rdp session map poisoned"))?
            .insert(id, Arc::new(session));
        Ok((id, width, height, cliprdr_actions))
    }

    pub fn input(&self, id: Uuid, ops: Vec<RdpInput>) -> Result<()> {
        self.get(id)?.input(ops)
    }

    pub fn resize(&self, id: Uuid, width: u16, height: u16) -> Result<()> {
        self.get(id)?.resize(width, height)
    }

    pub fn close(&self, id: Uuid) -> Result<()> {
        let removed = self.inner.lock().ok().and_then(|mut m| m.remove(&id));
        if let Some(s) = removed {
            s.shutdown();
        }
        Ok(())
    }

    fn get(&self, id: Uuid) -> Result<Arc<RdpSession>> {
        self.inner
            .lock()
            .map_err(|_| anyhow!("rdp session map poisoned"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("no such rdp session: {id}"))
    }
}

/// CredSSP can ask us to relay Kerberos traffic to a KDC. We only support
/// NTLM, which never takes this path, so reaching it is a configuration
/// problem worth reporting rather than something to silently retry.
struct NoNetworkClient;

impl ironrdp_tokio::NetworkClient for NoNetworkClient {
    fn send(
        &mut self,
        _req: &connector::sspi::generator::NetworkRequest,
    ) -> impl std::future::Future<Output = connector::ConnectorResult<Vec<u8>>> {
        std::future::ready(Err(connector::custom_err!(
            "kerberos",
            std::io::Error::other("Kerberos is not supported; use NTLM credentials")
        )))
    }
}

// ---------------------------------------------------------------------------
// Clipboard (CLIPRDR static virtual channel, MS-RDPECLIP)
// ---------------------------------------------------------------------------
//
// The CliprdrBackend trait is synchronous but the work it does
// is async (read/write local clipboard, call back into the
// `Cliprdr` state machine). We bridge the two halves through a
// `BackendAction` channel:
//
//   - The trait methods are sync; they push `BackendAction`s to
//     a channel and return immediately.
//   - The engine loop reads the channel, calls the relevant
//     `Cliprdr` method (`initiate_copy`, `initiate_paste`,
//     `submit_format_data`), takes the resulting `SvcMessage`s,
//     encodes them via `stage.process_svc_processor_messages`,
//     and writes the bytes to the socket.
//
// Text only for v1; the wire format leaves room to add a
// `format` discriminator later if we support HTML, images, or
// file copies.

/// Actions the CLI backend wants the engine loop to perform on
/// the `CliprdrClient` state machine. Drained between
/// processing incoming PDUs and writing the next outgoing
/// frame.
#[derive(Debug)]
pub enum BackendAction {
    /// Re-advertise the local clipboard to the server. The
    /// engine calls `cliprdr.initiate_copy(&[CF_UNICODETEXT])`
    /// (plus any future formats) and writes the resulting
    /// `FormatList` PDU.
    InitiateCopy,
    /// Replace the local clipboard text and re-advertise
    /// to the server. Used by the daemon's
    /// `set_local_clipboard` (which the Tauri side hits via
    /// `POST /rdp/{id}/clipboard`). The two steps are
    /// bundled so a "set new text" can't race with a
    /// concurrent `initiate_copy` from the server's
    /// `on_request_format_list` and advertise the wrong
    /// value.
    SetLocalClipboard {
        text: String,
    },
    /// Ask the server for the data of a specific format it
    /// just advertised. The engine calls
    /// `cliprdr.initiate_paste(format_id)`.
    InitiatePaste {
        format_id: ClipboardFormatId,
    },
    /// The server asked for the data of a specific format;
    /// respond with the current local text. The engine calls
    /// `cliprdr.submit_format_data(response)`.
    SubmitFormatData {
        format_id: ClipboardFormatId,
    },
}

/// The OS-specific half of the CLIPRDR channel: implements the
/// `CliprdrBackend` trait and bridges to the engine loop via
/// `BackendAction`. The local clipboard text lives with the
/// engine, not here -- the backend only forwards requests and
/// decodes responses.
struct TerminatorCliprdrBackend {
    /// Channel the backend trait methods push actions onto
    /// for the engine loop to process on the cliprdr.
    actions: mpsc::UnboundedSender<BackendAction>,
    /// `RdpEvent::RemoteClipboard { text }` is sent down this
    /// channel when the server's clipboard data arrives. The
    /// Tauri side writes it to the OS clipboard.
    events: mpsc::Sender<RdpEvent>,
}

impl std::fmt::Debug for TerminatorCliprdrBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminatorCliprdrBackend").finish()
    }
}

impl AsAny for TerminatorCliprdrBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl TerminatorCliprdrBackend {
    fn new(
        actions: mpsc::UnboundedSender<BackendAction>,
        events: mpsc::Sender<RdpEvent>,
    ) -> Self {
        Self { actions, events }
    }
}

impl CliprdrBackend for TerminatorCliprdrBackend {
    fn temporary_directory(&self) -> &str {
        // File transfers (CF_HDROP) are not supported in v1;
        // the backend trait requires this stub. Return an
        // empty path so any accidental file-list traffic fails
        // fast rather than silently writing to a real
        // directory.
        ""
    }

    fn client_capabilities(
        &self,
    ) -> ironrdp::cliprdr::pdu::ClipboardGeneralCapabilityFlags {
        // USE_LONG_FORMAT_NAMES is the only one we care about
        // for text. No file / no lock capabilities for v1.
        ironrdp::cliprdr::pdu::ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        // Nothing extra to do. The server will send a
        // FormatList for its own clipboard shortly; the
        // initial `initiate_copy` happens from
        // `on_request_format_list` below, which is the
        // right hook per [MS-RDPECLIP] 1.3.2.1.
    }

    fn on_request_format_list(&mut self) {
        // Server is asking what formats we can provide. Queue
        // an InitiateCopy; the engine will pull the current
        // local text and advertise CF_UNICODETEXT.
        let _ = self.actions.send(BackendAction::InitiateCopy);
    }

    fn on_remote_copy(
        &mut self,
        available_formats: &[ClipboardFormat],
    ) {
        // Server has new clipboard data. Pick the first
        // text format we understand and queue a paste.
        // Order: CF_UNICODETEXT (13) > CF_TEXT (1) > anything
        // text-shaped; for v1 we only support the unicode
        // variant.
        let unicode = ClipboardFormatId::new(13);
        for f in available_formats {
            if f.id == unicode {
                let _ = self
                    .actions
                    .send(BackendAction::InitiatePaste { format_id: unicode });
                return;
            }
        }
    }

    fn on_format_data_request(
        &mut self,
        request: ironrdp::cliprdr::pdu::FormatDataRequest,
    ) {
        // Server wants the local text in a specific format.
        // Queue the response; the engine will read the local
        // text and call `cliprdr.submit_format_data`.
        let _ = self
            .actions
            .send(BackendAction::SubmitFormatData {
                format_id: request.format,
            });
    }

    fn on_format_data_response(
        &mut self,
        response: ironrdp::cliprdr::pdu::FormatDataResponse<'_>,
    ) {
        // Server sent us the data for a paste we requested.
        // Decode the text and emit it to the Tauri side. Text
        // is UTF-16LE for CF_UNICODETEXT; the wire format
        // is little-endian u16 code units.
        if response.is_error() {
            // Server couldn't satisfy the paste. Nothing to
            // do; the local clipboard is unchanged.
            return;
        }
        // We only requested CF_UNICODETEXT, so any non-empty
        // response is treated as UTF-16LE text. (A more
        // complete impl would inspect the format list and
        // pick a decoder; for v1 the cliprdr's
        // `initiate_paste` is only ever called for
        // CF_UNICODETEXT.)
        let text = decode_utf16le_text(response.data());
        // Send to Tauri side. Best-effort: if the consumer
        // is gone we just drop the event.
        let _ = self.events.try_send(RdpEvent::RemoteClipboard { text });
    }

    // -- The remaining callbacks are not exercised in v1 (we
    // only support text, no file transfer, no lock
    // negotiation). Provide empty defaults so the trait is
    // satisfied. `on_process_negotiated_capabilities` is not
    // optional, so it has to be a no-op body here.

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        // No-op: we don't negotiate any capabilities beyond
        // USE_LONG_FORMAT_NAMES, and the cliprdr handles the
        // intersect internally.
    }

    fn on_file_contents_request(
        &mut self,
        _request: FileContentsRequest,
    ) {
        // File transfer is not supported in v1. The cliprdr
        // wouldn't have sent a FormatList with FileGroupDescriptorW
        // because we never advertise it, so this callback
        // should never fire.
    }

    fn on_file_contents_response(
        &mut self,
        _response: FileContentsResponse<'_>,
    ) {
        // Same as above; not reachable in v1.
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        // No-op: lock negotiation is not enabled in v1.
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        // No-op.
    }
}

/// Decode a CF_UNICODETEXT payload (UTF-16LE) into a `String`.
/// Trims a trailing NUL terminator if present (Windows
/// convention for clipboard text).
fn decode_utf16le_text(bytes: &[u8]) -> String {
    // Round to whole code units; trailing odd byte (if any) is
    // dropped.
    let units = bytes.len() / 2;
    let codes: Vec<u16> = (0..units)
        .map(|i| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]))
        .collect();
    let mut text = String::from_utf16_lossy(&codes);
    while text.ends_with('\0') {
        text.pop();
    }
    text
}

/// Build the format list we advertise when the server asks for
/// our local clipboard. v1: just CF_UNICODETEXT.
fn local_text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat {
        id: ClipboardFormatId::new(13),
        name: None,
    }]
}

/// Helper for the engine loop: drain pending backend actions,
/// apply each one to the `CliprdrClient`, and return the
/// resulting outgoing SVC messages (as a flat `Vec<SvcMessage>`;
/// the caller wraps in `SvcProcessorMessages` for
/// `process_svc_processor_messages`).
fn drain_backend_actions(
    actions: &mut mpsc::UnboundedReceiver<BackendAction>,
    cliprdr: &mut CliprdrClient,
    local_text: &Arc<Mutex<Option<String>>>,
) -> Vec<SvcMessage> {
    let mut out: Vec<SvcMessage> = Vec::new();
    while let Ok(action) = actions.try_recv() {
        match action {
            BackendAction::InitiateCopy => {
                let formats = local_text_formats();
                match cliprdr.initiate_copy(&formats) {
                    Ok(msgs) => out.extend(Vec::from(msgs)),
                    Err(e) => tracing::debug!("cliprdr.initiate_copy: {e}"),
                }
            }
            BackendAction::SetLocalClipboard { text } => {
                // Replace the local text and re-advertise in
                // one atomic step. Doing them as two separate
                // actions would race with a concurrent
                // `on_request_format_list` -> `InitiateCopy`
                // and could advertise a stale value.
                if let Ok(mut g) = local_text.lock() {
                    *g = Some(text);
                }
                let formats = local_text_formats();
                match cliprdr.initiate_copy(&formats) {
                    Ok(msgs) => out.extend(Vec::from(msgs)),
                    Err(e) => tracing::debug!("cliprdr.initiate_copy: {e}"),
                }
            }
            BackendAction::InitiatePaste { format_id } => match cliprdr.initiate_paste(format_id) {
                Ok(msgs) => out.extend(Vec::from(msgs)),
                Err(e) => tracing::debug!("cliprdr.initiate_paste: {e}"),
            },
            BackendAction::SubmitFormatData { format_id } => {
                // Only CF_UNICODETEXT for v1; anything else is
                // a no-op (the server shouldn't have asked).
                if format_id != ClipboardFormatId::new(13) {
                    continue;
                }
                let text = local_text
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_default();
                let response = FormatDataResponse::new_unicode_string(&text);
                match cliprdr.submit_format_data(response.into_owned()) {
                    Ok(msgs) => out.extend(Vec::from(msgs)),
                    Err(e) => tracing::debug!("cliprdr.submit_format_data: {e}"),
                }
            }
        }
    }
    out
}

/// Encode a Rust `String` as UTF-16LE for CF_UNICODETEXT.
/// The trailing NUL is added by the caller (drain_backend_actions).
// (intentionally removed: we use FormatDataResponse::new_unicode_string
//  which handles the trailing NUL and the unicode-string serialization
//  per MS-RDPECLIP 2.2.5.1.)

fn build_config(cfg: &RdpConfig) -> connector::Config {
    connector::Config {
        desktop_size: DesktopSize {
            width: cfg.width,
            height: cfg.height,
        },
        desktop_scale_factor: 0,
        // NLA only. The legacy TLS-with-graphical-login path hands anyone who
        // can reach the port a fully joined session before authentication --
        // IronRDP's own docs enumerate the MITM and takeover consequences.
        enable_tls: false,
        enable_credssp: true,
        credentials: RdpCredentials::UsernamePassword {
            username: cfg.user.clone(),
            password: cfg.password.clone(),
        },
        domain: cfg.domain.clone(),
        client_build: 0,
        client_name: hostname(),
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0,
        ime_file_name: String::new(),
        bitmap: None,
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        platform: ironrdp::pdu::rdp::capability_sets::MajorPlatformType::UNSPECIFIED,
        hardware_id: None,
        request_data: None,
        autologon: false,
        enable_audio_playback: false,
        performance_flags: Default::default(),
        license_cache: None,
        timezone_info: Default::default(),
        compression_type: None,
        // Draw the cursor ourselves into the framebuffer. The alternative is
        // plumbing cursor bitmaps through to a CSS cursor, which buys nothing
        // for a windowed client.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "terminator".to_owned())
}

/// Default deadline for the entire handshake: TCP, X.224 negotiation, TLS,
/// CredSSP and the capability exchange.
///
/// IronRDP imposes no timeout of its own, and neither does a TCP read. A peer
/// that completes the TCP handshake and then says nothing -- a firewall
/// tarpit, the wrong port, a half-dead host, a hung Terminal Services stack --
/// leaves every read pending forever, so without this the UI spins on a
/// connecting spinner with no error and no way out. Verified by
/// `silent_peer_times_out_instead_of_hanging` in `tests/rdp_live.rs`.
///
/// 30s is generous on purpose: CredSSP against a distant or busy domain
/// controller is genuinely slow, and a false timeout is worse than a slow
/// connect.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

async fn connect(
    cfg: RdpConfig,
    events: mpsc::Sender<RdpEvent>,
    deadline: Duration,
) -> Result<(RdpSession, u16, u16, mpsc::UnboundedSender<BackendAction>)> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    tokio::time::timeout(deadline, connect_inner(cfg, events))
        .await
        .map_err(|_| {
            anyhow!(
                "timed out after {}s connecting to {addr}; the host accepted the \
                 connection but did not complete the RDP handshake",
                deadline.as_secs()
            )
        })?
}

async fn connect_inner(
    cfg: RdpConfig,
    events: mpsc::Sender<RdpEvent>,
) -> Result<(RdpSession, u16, u16, mpsc::UnboundedSender<BackendAction>)> {
    let addr = format!("{}:{}", cfg.host, cfg.port);
    let stream = TcpStream::connect(&addr)
        .await
        .with_context(|| format!("cannot reach {addr}"))?;
    // Nagle batches our small input PDUs, which shows up directly as cursor
    // and keystroke lag.
    let _ = stream.set_nodelay(true);

    let local: SocketAddr = stream.local_addr().context("local address")?;
    let mut framed = TokioFramed::new(stream);

    // CLIPRDR (text-only clipboard) shares its state with
    // the daemon's `set_local_clipboard` and with the
    // engine loop. The `BackendAction` channel is the
    // bridge: the backend's sync trait callbacks push
    // actions on its end, the daemon's
    // `set_local_clipboard` pushes a `SetLocalClipboard`
    // action on its end (via the sender we return), and
    // the engine drains the receiver and calls the right
    // `initiate_*` / `submit_*` method.
    let local_text: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    // `actions_tx` is cloned three ways: one to the
    // backend (so its trait callbacks can push), one
    // returned to the caller as the "external" sender
    // (the daemon uses it from `set_local_clipboard`),
    // and the original is dropped. `actions_rx` is moved
    // into the engine task.
    let (actions_tx, actions_rx) = mpsc::unbounded_channel::<BackendAction>();
    let backend_tx = actions_tx.clone();
    let external_tx = actions_tx;

    let mut connector = ClientConnector::new(build_config(&cfg), local)
        // DisplayControl is what makes live resizing possible; without this
        // channel the desktop is stuck at its handshake size for the whole
        // session.
        .with_static_channel(
            DrdynvcClient::new()
                .with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new()))),
        )
        // Clipboard (CLIPRDR). Text only for v1.
        .with_static_channel(CliprdrClient::new(Box::new(
            TerminatorCliprdrBackend::new(backend_tx, events.clone()),
        )));

    let upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .map_err(|e| anyhow!("RDP negotiation failed: {e}"))?;

    // TLS has to happen on the raw stream, so unwrap the framing, upgrade, and
    // re-frame. Any bytes already buffered would be pre-TLS handshake bytes,
    // so dropping them here is correct.
    let (stream, _leftover) = framed.into_inner();
    let (upgraded_stream, server_public_key) = ironrdp_tls::upgrade(stream, &cfg.host)
        .await
        .map_err(|e| anyhow!("TLS upgrade failed: {e}"))?;
    let server_public_key = ironrdp_tls::extract_tls_server_public_key(&server_public_key)
        .ok_or_else(|| anyhow!("server certificate has no usable public key"))?
        .to_vec();

    let upgraded = ironrdp_tokio::mark_as_upgraded(upgrade, &mut connector);
    let mut framed = TokioFramed::new(upgraded_stream);

    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut NoNetworkClient,
        ServerName::new(cfg.host.clone()),
        server_public_key,
        None,
    )
    .await
    .map_err(|e| anyhow!("{}", describe_connect_error(&e)))?;

    let width = result.desktop_size.width;
    let height = result.desktop_size.height;

    let (reader, mut writer) = split_tokio_framed(framed);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(256);

    // Writer task: the only thing allowed to touch the socket's write half.
    tokio::spawn(async move {
        use ironrdp_tokio::FramedWrite;
        while let Some(bytes) = out_rx.recv().await {
            if let Err(e) = writer.write_all(&bytes).await {
                tracing::debug!("rdp write failed: {e}");
                break;
            }
        }
    });

    tokio::spawn(engine(
        reader,
        result,
        out_tx,
        cmd_rx,
        events,
        width,
        height,
        local_text,
        actions_rx,
    ));

    Ok((RdpSession { cmd: cmd_tx }, width, height, external_tx))
}

/// Turn IronRDP's connector errors into something a user can act on.
///
/// The raw errors are protocol-accurate but opaque -- "HYBRID_REQUIRED_BY_SERVER"
/// tells an administrator plenty and a normal user nothing.
fn describe_connect_error(e: &connector::ConnectorError) -> String {
    let raw = e.to_string();
    let lower = raw.to_lowercase();
    if lower.contains("logon") || lower.contains("credssp") || lower.contains("authenticate") {
        format!("{raw} (check the username, password, and domain)")
    } else if lower.contains("ssl_not_allowed") || lower.contains("standard rdp security") {
        format!("{raw} (this server does not support TLS; Terminator requires it)")
    } else {
        raw
    }
}

type Reader = TokioFramed<tokio::io::ReadHalf<ironrdp_tls::TlsStream<TcpStream>>>;

/// The engine task. Owns everything that needs `&mut` and never awaits the
/// socket's write half -- see the module docs for why that matters.
#[allow(clippy::too_many_arguments)]
async fn engine(
    mut reader: Reader,
    result: ConnectionResult,
    out: mpsc::Sender<Vec<u8>>,
    mut cmd: mpsc::Receiver<Cmd>,
    events: mpsc::Sender<RdpEvent>,
    width: u16,
    height: u16,
    // Latest local clipboard text. Shared with the
    // `TerminatorCliprdrBackend` (so its `on_format_data_request`
    // callback can read the current text) and with the
    // daemon's `set_local_clipboard` (so the Tauri side can
    // push updates).
    local_text: Arc<Mutex<Option<String>>>,
    // Backend actions the cliprdr trait methods pushed
    // since the last loop iteration. Drained every iteration
    // and applied to the cliprdr state machine, with the
    // resulting outgoing PDUs written to the socket.
    mut actions_rx: mpsc::UnboundedReceiver<BackendAction>,
) {
    let mut image = DecodedImage::new(PixelFormat::RgbA32, width, height);
    let mut stage = ActiveStage::new(result);
    let mut keyboard = Database::new();
    let mut damage: Option<Rect> = None;

    // Drive the cliprdr's lock-cleanup state machine on a
    // timer. The 5s interval matches the ironrdp-cliprdr
    // README's recommendation; we don't do file transfers so
    // there are no file-contents timeouts to worry about.
    let mut cliprdr_tick = tokio::time::interval(std::time::Duration::from_secs(5));
    cliprdr_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let _ = events.send(RdpEvent::Resized { width, height }).await;

    // Helper: drain any pending backend actions and pump
    // their results (and the periodic drive_timeouts) to the
    // socket. Pulled out so both the regular loop and the
    // close path can call it.
    let pump_cliprdr = |stage: &mut ActiveStage,
                        actions_rx: &mut mpsc::UnboundedReceiver<BackendAction>,
                        local_text: &Arc<Mutex<Option<String>>>,
                        out: &mpsc::Sender<Vec<u8>>| {
        let cliprdr = match stage.get_svc_processor_mut::<CliprdrClient>() {
            Some(c) => c,
            None => return Ok(()),
        };
        // 1. Drain backend actions -> call methods -> collect messages.
        let mut pending = drain_backend_actions(actions_rx, cliprdr, local_text);
        // 2. Drive timeouts (lock cleanup) -> more messages.
        if let Ok(msgs) = cliprdr.drive_timeouts() {
            pending.extend(Vec::from(msgs));
        }
        if pending.is_empty() {
            return Ok(());
        }
        // 3. Encode via the stage's SVC processor and write to socket.
        let messages = ironrdp::svc::SvcProcessorMessages::<CliprdrClient>::new(pending);
        match stage.process_svc_processor_messages::<CliprdrClient>(messages) {
            Ok(encoded) => {
                if !encoded.is_empty() {
                    let _ = out.try_send(encoded);
                }
                Ok(())
            }
            Err(e) => Err(anyhow!("cliprdr encode: {e}")),
        }
    };

    let reason = loop {
        // Flush pending damage whenever the UI has capacity. Coalescing here
        // rather than sending every rectangle is what keeps a full-screen
        // redraw from turning into thousands of IPC messages.
        if damage.is_some() && events.capacity() > 0 {
            if let Some(r) = damage.take() {
                if let Some(ev) = encode_frame(&image, r) {
                    let _ = events.try_send(ev);
                }
            }
        }

        // Drain any pending cliprdr actions before blocking on the
        // select, so a fast `set_local_clipboard` doesn't have to
        // wait for a frame to arrive.
        if let Err(e) = pump_cliprdr(&mut stage, &mut actions_rx, &local_text, &out) {
            break format!("cliprdr: {e}");
        }

        let outputs = tokio::select! {
            biased;

            c = cmd.recv() => match c {
                None | Some(Cmd::Shutdown) => break "closed by user".to_owned(),
                Some(Cmd::Input(ops)) => {
                    let fastpath = keyboard.apply(ops.into_iter().filter_map(to_operation));
                    if fastpath.is_empty() {
                        continue;
                    }
                    match stage.process_fastpath_input(&mut image, &fastpath) {
                        Ok(o) => o,
                        Err(e) => break format!("input error: {e}"),
                    }
                }
                Some(Cmd::Resize { width, height }) => {
                    match stage.encode_resize(u32::from(width), u32::from(height), None, None) {
                        Some(Ok(frame)) => vec![ActiveStageOutput::ResponseFrame(frame)],
                        // The server has not brought DisplayControl up yet.
                        // Dropping the request is right: the pane keeps its
                        // own size and the next resize will carry the latest
                        // dimensions anyway.
                        None => continue,
                        Some(Err(e)) => break format!("resize failed: {e}"),
                    }
                }
            },

            frame = reader_next(&mut reader) => match frame {
                Ok((action, payload)) => match stage.process(&mut image, action, &payload) {
                    Ok(o) => o,
                    Err(e) => break format!("session error: {e}"),
                },
                Err(e) => break e,
            },

            // Periodic cliprdr timer. Drives lock cleanup
            // and any time-bounded state in the cliprdr
            // state machine. Cheap when nothing to do.
            _ = cliprdr_tick.tick() => {
                if let Err(e) = pump_cliprdr(&mut stage, &mut actions_rx, &local_text, &out) {
                    break format!("cliprdr: {e}");
                }
                continue;
            }
        };

        // After handling a frame, the cliprdr's `process` may
        // have queued backend actions (e.g. on remote copy ->
        // backend's on_remote_copy -> BackendAction::InitiatePaste).
        // Drain them now so the new state is sent before we
        // block on the next select.
        if let Err(e) = pump_cliprdr(&mut stage, &mut actions_rx, &local_text, &out) {
            break format!("cliprdr: {e}");
        }

        match handle_outputs(
            outputs,
            &mut stage,
            &mut image,
            &mut reader,
            &out,
            &events,
            &mut damage,
        )
        .await
        {
            Ok(()) => {}
            Err(reason) => break reason,
        }
    };

    // Best effort: the peer may already be gone.
    if let Ok(frames) = stage.graceful_shutdown() {
        for f in frames {
            if let ActiveStageOutput::ResponseFrame(bytes) = f {
                let _ = out.try_send(bytes);
            }
        }
    }
    let _ = events.send(RdpEvent::Disconnected { reason }).await;
}

async fn reader_next(
    reader: &mut Reader,
) -> std::result::Result<(Action, bytes::BytesMut), String> {
    reader
        .read_pdu()
        .await
        .map_err(|e| format!("connection lost: {e}"))
}

#[allow(clippy::too_many_arguments)]
async fn handle_outputs(
    outputs: Vec<ActiveStageOutput>,
    stage: &mut ActiveStage,
    image: &mut DecodedImage,
    reader: &mut Reader,
    out: &mpsc::Sender<Vec<u8>>,
    events: &mpsc::Sender<RdpEvent>,
    damage: &mut Option<Rect>,
) -> std::result::Result<(), String> {
    for output in outputs {
        match output {
            ActiveStageOutput::ResponseFrame(bytes) => {
                if out.send(bytes).await.is_err() {
                    return Err("connection closed".to_owned());
                }
            }
            ActiveStageOutput::GraphicsUpdate(rect) => {
                let r = Rect {
                    left: rect.left,
                    top: rect.top,
                    // InclusiveRectangle really is inclusive on both edges, so
                    // a 1x1 update arrives as left==right. Getting this wrong
                    // clips a pixel row and column off every single update.
                    right: rect.right,
                    bottom: rect.bottom,
                };
                *damage = Some(match damage.take() {
                    Some(prev) => prev.union(r),
                    None => r,
                });
            }
            ActiveStageOutput::Terminate(reason) => return Err(reason.description()),
            ActiveStageOutput::DeactivateAll(mut activation) => {
                // The server is renegotiating -- typically because we asked to
                // resize. Drive the sequence to completion, then rebuild the
                // framebuffer at whatever size we actually got.
                match reactivate(&mut activation, reader, out).await {
                    Ok(finalized) => {
                        let (w, h) = (finalized.width, finalized.height);
                        *image = DecodedImage::new(PixelFormat::RgbA32, w, h);
                        *damage = None;
                        // The share id changes across a reactivation; keeping
                        // the old one makes the server ignore everything we
                        // send afterwards.
                        stage.set_share_id(finalized.share_id);
                        stage.set_enable_server_pointer(finalized.enable_server_pointer);
                        let _ = events
                            .send(RdpEvent::Resized {
                                width: w,
                                height: h,
                            })
                            .await;
                    }
                    Err(e) => return Err(e),
                }
            }
            // Pointer shape is rendered into the framebuffer for us
            // (pointer_software_rendering), and multitransport/autodetect are
            // optimisations we decline.
            _ => {}
        }
    }
    Ok(())
}

/// Run a reactivation sequence to completion on the read half.
///
/// `single_sequence_step_read` only needs `FramedRead`, which is what lets the
/// writer stay in its own task instead of being folded back in here.
async fn reactivate(
    activation: &mut ironrdp::connector::connection_activation::ConnectionActivationSequence,
    reader: &mut Reader,
    out: &mpsc::Sender<Vec<u8>>,
) -> std::result::Result<Finalized, String> {
    use ironrdp::core::WriteBuf;
    let mut buf = WriteBuf::new();

    loop {
        let written = ironrdp_tokio::single_sequence_step_read(reader, activation, &mut buf)
            .await
            .map_err(|e| format!("reactivation failed: {e}"))?;

        if written.size().is_some() && out.send(buf.filled().to_vec()).await.is_err() {
            return Err("connection closed during reactivation".to_owned());
        }

        if let ConnectionActivationState::Finalized {
            desktop_size,
            share_id,
            enable_server_pointer,
            ..
        } = activation.connection_activation_state()
        {
            return Ok(Finalized {
                width: desktop_size.width,
                height: desktop_size.height,
                share_id,
                enable_server_pointer,
            });
        }
    }
}

/// What a completed reactivation tells us about the new desktop.
struct Finalized {
    width: u16,
    height: u16,
    share_id: u32,
    enable_server_pointer: bool,
}

/// An inclusive rectangle, kept separate from IronRDP's so the union logic
/// stays readable.
#[derive(Debug, Clone, Copy)]
struct Rect {
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
}

impl Rect {
    fn union(self, other: Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// Repack a dirty rectangle out of the framebuffer.
///
/// `DecodedImage` rows are `stride` bytes apart and the rect is a window into
/// them, so this has to copy row by row -- a single slice would carry the
/// pixels either side of the rectangle along with it.
fn encode_frame(image: &DecodedImage, rect: Rect) -> Option<RdpEvent> {
    use base64::Engine as _;

    let img_w = image.width();
    let img_h = image.height();
    if img_w == 0 || img_h == 0 {
        return None;
    }

    // A resize can shrink the framebuffer between damage being recorded and
    // this running; clamp rather than panic on the slice.
    let left = rect.left.min(img_w.saturating_sub(1));
    let top = rect.top.min(img_h.saturating_sub(1));
    let right = rect.right.min(img_w.saturating_sub(1));
    let bottom = rect.bottom.min(img_h.saturating_sub(1));
    if right < left || bottom < top {
        return None;
    }

    let w = right - left + 1;
    let h = bottom - top + 1;
    let bpp = image.bytes_per_pixel();
    let stride = image.stride();
    let data = image.data();

    let row_len = usize::from(w) * bpp;
    let mut packed = Vec::with_capacity(row_len * usize::from(h));
    for row in 0..usize::from(h) {
        let start = (usize::from(top) + row) * stride + usize::from(left) * bpp;
        let end = start + row_len;
        if end > data.len() {
            return None;
        }
        packed.extend_from_slice(&data[start..end]);
    }

    Some(RdpEvent::Frame {
        x: left,
        y: top,
        w,
        h,
        rgba: base64::engine::general_purpose::STANDARD.encode(&packed),
    })
}

fn to_operation(input: RdpInput) -> Option<Operation> {
    Some(match input {
        RdpInput::MouseMove { x, y } => Operation::MouseMove(MousePosition { x, y }),
        RdpInput::MouseDown { button } => {
            Operation::MouseButtonPressed(MouseButton::from_web_button(button)?)
        }
        RdpInput::MouseUp { button } => {
            Operation::MouseButtonReleased(MouseButton::from_web_button(button)?)
        }
        RdpInput::Wheel { delta, horizontal } => Operation::WheelRotations(WheelRotations {
            is_vertical: !horizontal,
            rotation_units: delta,
        }),
        RdpInput::KeyDown { scancode } => Operation::KeyPressed(Scancode::from_u16(scancode)),
        RdpInput::KeyUp { scancode } => Operation::KeyReleased(Scancode::from_u16(scancode)),
        RdpInput::UnicodeChar { ch } => Operation::UnicodeKeyPressed(ch),
        // Handled before this point: it expands into a variable number of
        // release operations rather than mapping to a single one.
        RdpInput::ReleaseAll => return None,
        // Clipboard is handled at a higher layer (the CLI stream
        // routes it to the CLIPRDR backend, not the keyboard/mouse
        // pump). Mapping to None is the right escape.
        RdpInput::LocalClipboard { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_union_covers_both() {
        let a = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        let b = Rect {
            left: 5,
            top: 15,
            right: 15,
            bottom: 30,
        };
        let u = a.union(b);
        assert_eq!((u.left, u.top, u.right, u.bottom), (5, 10, 20, 30));
    }

    #[test]
    fn rect_union_is_idempotent() {
        let a = Rect {
            left: 1,
            top: 2,
            right: 3,
            bottom: 4,
        };
        let u = a.union(a);
        assert_eq!((u.left, u.top, u.right, u.bottom), (1, 2, 3, 4));
    }

    /// A 1x1 update arrives with left == right, so the inclusive arithmetic
    /// has to yield 1, not 0.
    #[test]
    fn single_pixel_rect_has_extent_one() {
        let img = DecodedImage::new(PixelFormat::RgbA32, 4, 4);
        let ev = encode_frame(
            &img,
            Rect {
                left: 2,
                top: 3,
                right: 2,
                bottom: 3,
            },
        )
        .expect("in bounds");
        match ev {
            RdpEvent::Frame { x, y, w, h, .. } => {
                assert_eq!((x, y, w, h), (2, 3, 1, 1));
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn encode_frame_packs_rows_tightly() {
        let img = DecodedImage::new(PixelFormat::RgbA32, 8, 8);
        let ev = encode_frame(
            &img,
            Rect {
                left: 1,
                top: 1,
                right: 4,
                bottom: 2,
            },
        )
        .expect("in bounds");
        match ev {
            RdpEvent::Frame { w, h, rgba, .. } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(rgba)
                    .expect("valid base64");
                assert_eq!((w, h), (4, 2));
                // Tightly packed: exactly w*h*4, not the strided span.
                assert_eq!(bytes.len(), 4 * 2 * 4);
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// Damage recorded before a shrink must not slice out of bounds.
    #[test]
    fn out_of_bounds_rect_is_clamped() {
        let img = DecodedImage::new(PixelFormat::RgbA32, 4, 4);
        let ev = encode_frame(
            &img,
            Rect {
                left: 0,
                top: 0,
                right: 99,
                bottom: 99,
            },
        )
        .expect("clamped");
        match ev {
            RdpEvent::Frame { w, h, .. } => assert_eq!((w, h), (4, 4)),
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn web_mouse_buttons_map_to_rdp() {
        assert!(matches!(
            to_operation(RdpInput::MouseDown { button: 0 }),
            Some(Operation::MouseButtonPressed(MouseButton::Left))
        ));
        assert!(matches!(
            to_operation(RdpInput::MouseDown { button: 1 }),
            Some(Operation::MouseButtonPressed(MouseButton::Middle))
        ));
        assert!(matches!(
            to_operation(RdpInput::MouseDown { button: 2 }),
            Some(Operation::MouseButtonPressed(MouseButton::Right))
        ));
        // Nothing sensible to send for an unknown button.
        assert!(to_operation(RdpInput::MouseDown { button: 77 }).is_none());
    }

    /// Extended scancodes carry the 0xE0 prefix; losing it turns the right
    /// Alt key into the left one and breaks the arrow keys.
    #[test]
    fn extended_scancodes_keep_their_prefix() {
        match to_operation(RdpInput::KeyDown { scancode: 0xE04B }) {
            Some(Operation::KeyPressed(s)) => assert_eq!(s.as_u16(), 0xE04B),
            other => panic!("expected a key press, got {other:?}"),
        }
        match to_operation(RdpInput::KeyDown { scancode: 0x1E }) {
            Some(Operation::KeyPressed(s)) => assert_eq!(s.as_u16(), 0x1E),
            other => panic!("expected a key press, got {other:?}"),
        }
    }

    #[test]
    fn wheel_axis_is_inverted_for_horizontal() {
        match to_operation(RdpInput::Wheel {
            delta: 120,
            horizontal: false,
        }) {
            Some(Operation::WheelRotations(w)) => assert!(w.is_vertical),
            other => panic!("expected wheel, got {other:?}"),
        }
        match to_operation(RdpInput::Wheel {
            delta: 120,
            horizontal: true,
        }) {
            Some(Operation::WheelRotations(w)) => assert!(!w.is_vertical),
            other => panic!("expected wheel, got {other:?}"),
        }
    }
}
