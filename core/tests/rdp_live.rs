#![cfg(feature = "rdp")]
//! RDP tests that need a real server.
//!
//! Opt-in, like the SSH live tests: set `TERMINATOR_RDP_TEST` to the target,
//! plus credentials, or these skip.
//!
//!   TERMINATOR_RDP_TEST=10.0.0.5:3389 \
//!   TERMINATOR_RDP_USER=Administrator \
//!   TERMINATOR_RDP_PASS=... \
//!   [TERMINATOR_RDP_DOMAIN=CORP] \
//!   cargo test -p terminator-core --features rdp --test rdp_live -- --nocapture
//!
//! The negative tests below need no server and always run: they are the ones
//! that catch a connect path that hangs instead of failing, which is the worst
//! possible failure mode for a UI.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use terminator_core::rdp::{RdpConfig, RdpEvent, RdpInput, RdpManager};
use tokio::sync::mpsc;

fn base(host: &str, port: u16) -> RdpConfig {
    RdpConfig {
        host: host.to_owned(),
        port,
        user: "nobody".to_owned(),
        password: "nothing".to_owned(),
        domain: None,
        width: 1024,
        height: 768,
    }
}

/// A closed port must fail promptly with a message naming the target, not hang
/// until some OS-level timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_connection_fails_fast() {
    // Port 1 is reserved and never has a listener on a developer machine.
    let cfg = base("127.0.0.1", 1);
    let (tx, _rx) = mpsc::channel(8);
    let mgr = RdpManager::new();

    let res = tokio::time::timeout(Duration::from_secs(10), mgr.open(cfg, tx))
        .await
        .expect("open must not hang on a refused connection");

    let err = res.expect_err("connecting to a closed port must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("127.0.0.1:1"),
        "error should name the target, got: {msg}"
    );
}

/// A port that accepts TCP but speaks something else entirely -- or nothing at
/// all -- must fail rather than block forever waiting for a response that never
/// comes. This is the worst failure mode for a UI: a spinner with no error and
/// no way out. `CONNECT_TIMEOUT` in `core/src/rdp.rs` exists because of this.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_peer_times_out_instead_of_hanging() {
    // A listener that accepts and then says nothing at all -- the nastiest
    // case, because every byte we send is absorbed and no read ever resolves.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let held = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            // Hold the connection open, reply to nothing, until the client
            // gives up on its own.
            tokio::time::sleep(Duration::from_secs(120)).await;
            drop(stream);
        }
    });

    let (tx, _rx) = mpsc::channel(8);
    let mgr = RdpManager::new();

    // A short injected deadline: the point is that *a* deadline is enforced at
    // all, and proving that with the real 30s value would add half a minute to
    // every suite run.
    let res = tokio::time::timeout(
        Duration::from_secs(20),
        mgr.open_with_timeout(base("127.0.0.1", port), tx, Duration::from_secs(2)),
    )
    .await
    .expect("connect must impose its own deadline on a silent peer");

    let err = res.expect_err("a silent peer must not produce a working session");
    let msg = err.to_string();
    assert!(
        msg.contains("timed out"),
        "error should say it timed out, got: {msg}"
    );

    held.abort();
}

fn live_target() -> Option<(String, u16, String, String, Option<String>)> {
    let target = std::env::var("TERMINATOR_RDP_TEST").ok()?;
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) => (h.to_owned(), p.parse().ok()?),
        None => (target, 3389u16),
    };
    Some((
        host,
        port,
        std::env::var("TERMINATOR_RDP_USER").ok()?,
        std::env::var("TERMINATOR_RDP_PASS").ok()?,
        std::env::var("TERMINATOR_RDP_DOMAIN").ok(),
    ))
}

/// The real thing: connect, and prove pixels actually arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_connect_receives_frames() {
    let Some((host, port, user, password, domain)) = live_target() else {
        eprintln!("skipping: TERMINATOR_RDP_TEST not set");
        return;
    };

    let cfg = RdpConfig {
        host,
        port,
        user,
        password,
        domain,
        width: 1280,
        height: 800,
    };

    let (tx, mut rx) = mpsc::channel(8);
    let mgr = RdpManager::new();
    let (id, w, h, _action_tx) = mgr.open(cfg, tx).await.expect("connect");
    assert!(w > 0 && h > 0, "desktop size must be non-zero");

    // A freshly connected desktop always paints something (the logon screen at
    // minimum), so no frame within 30s means the pump is broken.
    let mut frames = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(RdpEvent::Frame { w, h, rgba, .. })) => {
                assert!(w > 0 && h > 0, "frame must have extent");
                // Tightly packed RGBA: base64 of exactly w*h*4 bytes.
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&rgba)
                    .expect("valid base64");
                assert_eq!(bytes.len(), usize::from(w) * usize::from(h) * 4);
                frames += 1;
                if frames >= 3 {
                    break;
                }
            }
            Ok(Some(RdpEvent::Disconnected { reason })) => panic!("disconnected: {reason}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("event channel closed"),
            Err(_) => break,
        }
    }

    assert!(frames >= 1, "no framebuffer updates arrived");
    mgr.close(id).expect("close");
}

/// Capture the live desktop to a raw RGBA dump so the pixels can be eyeballed.
///
/// This is the only way to confirm the `PixelFormat` choice is right: a channel
/// swap still produces perfectly valid frames of the correct size, so every
/// automated assertion passes while the user sees a blue-tinted desktop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_capture_to_raw() {
    let Some((host, port, user, password, domain)) = live_target() else {
        eprintln!("skipping: TERMINATOR_RDP_TEST not set");
        return;
    };
    if std::env::var("TERMINATOR_RDP_CAPTURE").is_err() {
        eprintln!("skipping: TERMINATOR_RDP_CAPTURE not set");
        return;
    }

    let (mut w, mut h) = (1280u16, 800u16);
    let cfg = RdpConfig {
        host,
        port,
        user,
        password,
        domain,
        width: w,
        height: h,
    };

    let (tx, mut rx) = mpsc::channel(8);
    let mgr = RdpManager::new();
    let (id, dw, dh, _action_tx) = mgr.open(cfg, tx).await.expect("connect");
    w = dw;
    h = dh;
    eprintln!("desktop = {w}x{h}");

    // Composite dirty rectangles into a full-screen buffer, exactly as the
    // canvas does, so the dump reflects what the user would actually see.
    let mut fb = vec![0u8; usize::from(w) * usize::from(h) * 4];
    let mut frames = 0usize;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(RdpEvent::Frame {
                x,
                y,
                w: fw,
                h: fh,
                rgba,
            })) => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(&rgba)
                    .expect("valid base64");
                assert_eq!(bytes.len(), usize::from(fw) * usize::from(fh) * 4);
                for row in 0..usize::from(fh) {
                    let dy = usize::from(y) + row;
                    if dy >= usize::from(h) {
                        break;
                    }
                    let dst = (dy * usize::from(w) + usize::from(x)) * 4;
                    let src = row * usize::from(fw) * 4;
                    let len = usize::from(fw) * 4;
                    if dst + len <= fb.len() {
                        fb[dst..dst + len].copy_from_slice(&bytes[src..src + len]);
                    }
                }
                frames += 1;
            }
            Ok(Some(RdpEvent::Resized { width, height })) => {
                eprintln!("resized -> {width}x{height}");
            }
            Ok(Some(RdpEvent::RemoteClipboard { .. })) => {}
            Ok(Some(RdpEvent::Disconnected { reason })) => panic!("disconnected: {reason}"),
            Ok(None) => break,
            Err(_) => break,
        }
    }

    eprintln!("composited {frames} frames");
    assert!(frames > 0, "no frames");
    std::fs::write("/tmp/rdp_capture.raw", &fb).expect("write raw");
    std::fs::write("/tmp/rdp_capture.dim", format!("{w} {h}")).expect("write dim");
    eprintln!("wrote /tmp/rdp_capture.raw");

    mgr.close(id).expect("close");
}

/// Prove keystrokes actually reach the server and the scancode mapping is right.
///
/// Deliberately uses only Ctrl+Esc (open Start menu) and Esc (close it): both
/// are handled by the shell itself, so nothing is ever typed into whatever
/// application happens to have focus on the test machine. A test that typed
/// characters could corrupt a real document.
///
/// The proof is visual: the Start menu covers the lower-left of the screen, so
/// that region must change substantially after Ctrl+Esc and change back after
/// Esc. A no-op input path leaves it identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_keyboard_opens_start_menu() {
    let Some((host, port, user, password, domain)) = live_target() else {
        eprintln!("skipping: TERMINATOR_RDP_TEST not set");
        return;
    };
    if std::env::var("TERMINATOR_RDP_INPUT").is_err() {
        eprintln!("skipping: TERMINATOR_RDP_INPUT not set");
        return;
    }

    const CTRL: u16 = 0x1D;
    const ESC: u16 = 0x01;

    let cfg = RdpConfig {
        host,
        port,
        user,
        password,
        domain,
        width: 1280,
        height: 800,
    };

    let (tx, rx) = mpsc::channel(8);
    let mgr = RdpManager::new();
    let (id, w, h, _action_tx) = mgr.open(cfg, tx).await.expect("connect");

    let fb = Arc::new(Mutex::new(vec![0u8; usize::from(w) * usize::from(h) * 4]));
    let pump = spawn_compositor(rx, w, h, Arc::clone(&fb));

    // Let the initial full-screen paint settle before sampling.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let before = snapshot(&fb);

    mgr.input(id, vec![RdpInput::KeyDown { scancode: CTRL }])
        .expect("ctrl down");
    mgr.input(id, vec![RdpInput::KeyDown { scancode: ESC }])
        .expect("esc down");
    mgr.input(id, vec![RdpInput::KeyUp { scancode: ESC }])
        .expect("esc up");
    mgr.input(id, vec![RdpInput::KeyUp { scancode: CTRL }])
        .expect("ctrl up");

    // The Start menu animates open; give it time to finish painting.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let opened = snapshot(&fb);

    // Sample the lower-left quadrant, which the Start menu covers.
    let changed = fraction_differing(&before, &opened, w, h, 0, h / 2, w / 3, h / 2);
    eprintln!("lower-left changed after Ctrl+Esc: {:.1}%", changed * 100.0);

    // Always try to close it again, even if the assertion below fails, so the
    // machine is not left with a Start menu hanging open.
    mgr.input(id, vec![RdpInput::KeyDown { scancode: ESC }])
        .expect("esc down");
    mgr.input(id, vec![RdpInput::KeyUp { scancode: ESC }])
        .expect("esc up");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let closed = snapshot(&fb);

    let reverted = fraction_differing(&opened, &closed, w, h, 0, h / 2, w / 3, h / 2);
    eprintln!(
        "lower-left changed after Esc:      {:.1}%",
        reverted * 100.0
    );

    dump_png(&opened, w, h, "/tmp/rdp_start_open.png");
    dump_png(&closed, w, h, "/tmp/rdp_start_closed.png");

    pump.abort();
    mgr.close(id).expect("close");

    assert!(
        changed > 0.05,
        "Ctrl+Esc changed only {:.2}% of the lower-left; keystrokes are not \
         reaching the server (or the scancodes are wrong)",
        changed * 100.0
    );
    assert!(
        reverted > 0.05,
        "Esc did not close the Start menu ({:.2}% changed); key-up handling is \
         suspect",
        reverted * 100.0
    );
}

/// Composite frames into a shared framebuffer in the background, mirroring what
/// the canvas does, so tests can sample "what the user would see" at any moment.
fn spawn_compositor(
    mut rx: mpsc::Receiver<RdpEvent>,
    w: u16,
    h: u16,
    fb: Arc<Mutex<Vec<u8>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use base64::Engine as _;
        while let Some(ev) = rx.recv().await {
            if let RdpEvent::Frame {
                x,
                y,
                w: fw,
                h: fh,
                rgba,
            } = ev
            {
                let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&rgba) else {
                    continue;
                };
                let mut fb = fb.lock().expect("fb");
                for row in 0..usize::from(fh) {
                    let dy = usize::from(y) + row;
                    if dy >= usize::from(h) {
                        break;
                    }
                    let dst = (dy * usize::from(w) + usize::from(x)) * 4;
                    let src = row * usize::from(fw) * 4;
                    let len = usize::from(fw) * 4;
                    if dst + len <= fb.len() && src + len <= bytes.len() {
                        fb[dst..dst + len].copy_from_slice(&bytes[src..src + len]);
                    }
                }
            }
        }
    })
}

fn snapshot(fb: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    fb.lock().expect("fb").clone()
}

/// Fraction of pixels differing inside a sub-rectangle. Compares channels with
/// a small tolerance so JPEG-ish codec noise does not read as a real change.
#[allow(clippy::too_many_arguments)]
fn fraction_differing(
    a: &[u8],
    b: &[u8],
    w: u16,
    _h: u16,
    rx: u16,
    ry: u16,
    rw: u16,
    rh: u16,
) -> f64 {
    let mut diff = 0usize;
    let mut total = 0usize;
    for y in ry..ry.saturating_add(rh) {
        for x in rx..rx.saturating_add(rw) {
            let i = (usize::from(y) * usize::from(w) + usize::from(x)) * 4;
            if i + 3 >= a.len() || i + 3 >= b.len() {
                continue;
            }
            total += 1;
            let d = (0..3)
                .map(|c| (i32::from(a[i + c]) - i32::from(b[i + c])).abs())
                .max()
                .unwrap_or(0);
            if d > 12 {
                diff += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        diff as f64 / total as f64
    }
}

fn dump_png(rgba: &[u8], w: u16, h: u16, path: &str) {
    // Minimal PNG writer so a failing input test leaves a viewable artifact.
    // PNG mandates zlib, but zlib permits *stored* (uncompressed) deflate
    // blocks, which need no compressor -- that keeps this dependency-free
    // rather than pulling flate2 in just for test output.
    let (wu, hu) = (usize::from(w), usize::from(h));
    let mut raw = Vec::with_capacity(hu * (1 + wu * 4));
    for y in 0..hu {
        raw.push(0); // filter type: none
        raw.extend_from_slice(&rgba[y * wu * 4..(y + 1) * wu * 4]);
    }

    let mut z = vec![0x78, 0x01]; // zlib header, no preset dict
    for (i, part) in raw.chunks(0xFFFF).enumerate() {
        let last = (i + 1) * 0xFFFF >= raw.len();
        z.push(u8::from(last));
        let len = part.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(part);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |ty: &[u8], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(ty);
        png.extend_from_slice(data);
        let mut crc = crc32_continue(0, ty);
        crc = crc32_continue(crc, data);
        png.extend_from_slice(&crc.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    let _ = std::fs::write(path, &png);
    eprintln!("wrote {path}");
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc32_continue(prev: u32, data: &[u8]) -> u32 {
    let mut crc = prev ^ 0xffff_ffff;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xffff_ffff
}

/// Prove mouse coordinates land where we say they do.
///
/// Only movement is sent -- never a button -- so nothing on the test machine
/// can be clicked. Parking the pointer over the Start button produces both a
/// hover highlight and the pointer bitmap itself (we render the cursor into the
/// framebuffer), so the bottom-left corner must change while a far-away control
/// region does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_mouse_move_lands_on_target() {
    let Some((host, port, user, password, domain)) = live_target() else {
        eprintln!("skipping: TERMINATOR_RDP_TEST not set");
        return;
    };
    if std::env::var("TERMINATOR_RDP_INPUT").is_err() {
        eprintln!("skipping: TERMINATOR_RDP_INPUT not set");
        return;
    }

    let cfg = RdpConfig {
        host,
        port,
        user,
        password,
        domain,
        width: 1280,
        height: 800,
    };
    let (tx, rx) = mpsc::channel(8);
    let mgr = RdpManager::new();
    let (id, w, h, _action_tx) = mgr.open(cfg, tx).await.expect("connect");

    let fb = Arc::new(Mutex::new(vec![0u8; usize::from(w) * usize::from(h) * 4]));
    let pump = spawn_compositor(rx, w, h, Arc::clone(&fb));

    // Park the pointer in the middle and let the desktop settle.
    mgr.input(id, vec![RdpInput::MouseMove { x: w / 2, y: h / 2 }])
        .expect("park");
    tokio::time::sleep(Duration::from_secs(6)).await;
    let before = snapshot(&fb);

    // Now hover the Start button, bottom-left.
    mgr.input(id, vec![RdpInput::MouseMove { x: 20, y: h - 20 }])
        .expect("hover start");
    tokio::time::sleep(Duration::from_secs(4)).await;
    let after = snapshot(&fb);

    let corner = fraction_differing(&before, &after, w, h, 0, h - 48, 48, 48);
    eprintln!("start-button corner changed: {:.1}%", corner * 100.0);
    dump_png(&after, w, h, "/tmp/rdp_mouse_hover.png");

    pump.abort();
    mgr.close(id).expect("close");

    assert!(
        corner > 0.02,
        "hovering the Start button changed only {:.2}% of that corner; mouse \
         coordinates are not reaching the server correctly",
        corner * 100.0
    );
}

/// Resize is the riskiest path in the whole client: it makes the server tear
/// down and rebuild the session (`DeactivateAll`), which we drive to completion
/// on the read half alone and then have to re-arm with the *new* share id.
/// Getting the share id wrong makes the server silently ignore everything we
/// send afterwards, so this asserts the session is still alive and painting
/// after the round trip.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_resize_reactivates_and_keeps_painting() {
    let Some((host, port, user, password, domain)) = live_target() else {
        eprintln!("skipping: TERMINATOR_RDP_TEST not set");
        return;
    };
    if std::env::var("TERMINATOR_RDP_INPUT").is_err() {
        eprintln!("skipping: TERMINATOR_RDP_INPUT not set");
        return;
    }

    let cfg = RdpConfig {
        host,
        port,
        user,
        password,
        domain,
        width: 1280,
        height: 800,
    };
    let (tx, mut rx) = mpsc::channel(8);
    let mgr = RdpManager::new();
    let (id, w0, h0, _action_tx) = mgr.open(cfg, tx).await.expect("connect");
    eprintln!("initial desktop {w0}x{h0}");

    // Drain the initial paint so the frames we count later are genuinely post-resize.
    let settle = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < settle {
        if tokio::time::timeout_at(settle, rx.recv()).await.is_err() {
            break;
        }
    }

    let (want_w, want_h) = (1024u16, 768u16);
    mgr.resize(id, want_w, want_h).expect("resize");

    let mut resized_to = None;
    let mut frames_after = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(RdpEvent::Resized { width, height })) => {
                eprintln!("server reactivated at {width}x{height}");
                resized_to = Some((width, height));
            }
            Ok(Some(RdpEvent::Frame { .. })) => {
                if resized_to.is_some() {
                    frames_after += 1;
                    if frames_after >= 5 {
                        break;
                    }
                }
            }
            Ok(Some(RdpEvent::Disconnected { reason })) => {
                panic!("session died during resize: {reason}")
            }
            Ok(Some(RdpEvent::RemoteClipboard { .. })) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }

    mgr.close(id).expect("close");

    let (rw, rh) = resized_to.expect("server never reactivated after a resize request");
    assert_eq!(
        (rw, rh),
        (want_w, want_h),
        "server reactivated at the wrong size"
    );
    assert!(
        frames_after > 0,
        "no frames after reactivation -- the share id is probably stale, so the \
         server is ignoring us"
    );
    eprintln!("{frames_after} frames after reactivation");
}
