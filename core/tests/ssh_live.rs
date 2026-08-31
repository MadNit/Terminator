//! Real SSH round-trip against a live server.
//!
//! Skipped automatically unless TERMINATOR_SSH_TEST is set, so CI and other
//! machines are unaffected. Point it at any reachable host:
//!
//!   TERMINATOR_SSH_TEST=1 \
//!   TERMINATOR_SSH_HOST=localhost \
//!   TERMINATOR_SSH_USER=$USER \
//!   TERMINATOR_SSH_KEY=~/.ssh/id_ed25519 \
//!   cargo test -p terminator-core --features ssh --test ssh_live -- --nocapture

#![cfg(feature = "ssh")]

use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use terminator_core::session::SessionManager;
use terminator_core::transport::{SshAuth, TransportSpec};

struct Env {
    host: String,
    port: u16,
    user: String,
    key: String,
}

fn env() -> Option<Env> {
    if std::env::var("TERMINATOR_SSH_TEST").is_err() {
        eprintln!("skipping: TERMINATOR_SSH_TEST not set");
        return None;
    }
    Some(Env {
        host: std::env::var("TERMINATOR_SSH_HOST").unwrap_or_else(|_| "localhost".into()),
        port: std::env::var("TERMINATOR_SSH_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22),
        user: std::env::var("TERMINATOR_SSH_USER").expect("TERMINATOR_SSH_USER required"),
        key: std::env::var("TERMINATOR_SSH_KEY").expect("TERMINATOR_SSH_KEY required"),
    })
}

fn spec(e: &Env) -> TransportSpec {
    TransportSpec::Ssh {
        host: e.host.clone(),
        port: e.port,
        user: e.user.clone(),
        auth: SshAuth::Key {
            path: e.key.clone(),
        },
    }
}

/// Collect output until `needle` appears or the deadline passes.
async fn wait_for(seen: &Arc<Mutex<Vec<u8>>>, needle: &str, secs: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let text = String::from_utf8_lossy(&seen.lock().unwrap().clone()).to_string();
        if text.contains(needle) || std::time::Instant::now() > deadline {
            return text;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn ssh_key_auth_runs_a_remote_shell_and_logs_it() {
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let store = terminator_core::store::Store::open(&dir.path().join("t.db")).unwrap();
    let mgr = SessionManager::new(dir.path().join("logs")).with_store(store.clone());

    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();

    let id = mgr
        .open(
            spec(&e),
            80,
            24,
            Arc::new(move |b: Bytes| seen2.lock().unwrap().extend_from_slice(&b)),
            Arc::new(|| {}),
        )
        .await
        .expect("ssh session should open");

    // A real remote PTY must produce a prompt unprompted.
    let banner = wait_for(&seen, "$", 10).await;
    assert!(
        !banner.is_empty(),
        "no output from remote shell -- PTY was probably not granted"
    );

    mgr.write(id, Bytes::from("echo REMOTE_OK_MARKER\n"))
        .unwrap();
    let out = wait_for(&seen, "REMOTE_OK_MARKER", 10).await;
    assert!(
        out.contains("REMOTE_OK_MARKER"),
        "remote command produced no output:\n{out}"
    );

    // Logging must work identically for remote sessions.
    let logs = mgr.logs(id).unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let plain = std::fs::read_to_string(&logs.plain).unwrap();
    assert!(
        plain.contains("REMOTE_OK_MARKER"),
        "remote output missing from plain log:\n{plain}"
    );
    assert!(
        !plain.contains('\u{1b}'),
        "escape codes leaked into the plain log"
    );

    mgr.close(id).unwrap();
}

#[tokio::test]
async fn ssh_resize_reaches_the_remote_pty() {
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();

    let id = mgr
        .open(
            spec(&e),
            80,
            24,
            Arc::new(move |b: Bytes| seen2.lock().unwrap().extend_from_slice(&b)),
            Arc::new(|| {}),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(1200)).await;
    mgr.resize(id, 120, 40).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;

    seen.lock().unwrap().clear();
    mgr.write(id, Bytes::from("tput cols\n")).unwrap();
    let out = wait_for(&seen, "120", 10).await;
    assert!(
        out.contains("120"),
        "remote PTY did not resize; `tput cols` said:\n{out}"
    );

    mgr.close(id).unwrap();
}

#[tokio::test]
async fn ssh_rejects_a_changed_host_key() {
    // TOFU's whole value is refusing a key that changed. Seed known_hosts with
    // a wrong key for this host and require the connection to fail.
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    std::fs::create_dir_all(&data).unwrap();

    // A syntactically valid ed25519 key that is definitely not the server's.
    let bogus = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBl5ZQ0Qkr5aQ0Zt3z9m1lYQKzXQ0Lx3Vb0Xj5aQ0Zt0";
    let entry = if e.port == 22 {
        format!("{} {}\n", e.host, bogus)
    } else {
        format!("[{}]:{} {}\n", e.host, e.port, bogus)
    };
    std::fs::write(data.join("known_hosts"), entry).unwrap();

    // SessionManager derives known_hosts from log_dir's parent.
    let mgr = SessionManager::new(data.join("logs"));

    let result = mgr
        .open(spec(&e), 80, 24, Arc::new(|_| {}), Arc::new(|| {}))
        .await;

    let err = match result {
        Ok(_) => {
            panic!("connected despite a mismatched host key -- TOFU is not protecting anything")
        }
        Err(e) => format!("{e:#}"),
    };
    eprintln!("host-key rejection error: {err}");
    // Guard against passing for the wrong reason (e.g. a parse failure or a
    // refused connection would also be an Err).
    assert!(
        err.contains("host key verification failed") || err.contains("KeyChanged"),
        "rejected, but not because the host key changed: {err}"
    );
}

#[tokio::test]
async fn ssh_agent_auth_works() {
    // Agent auth is the default in the UI, so it needs its own coverage.
    // Requires the key to be loaded: `ssh-add <key>`.
    let Some(e) = env() else { return };
    if std::env::var("SSH_AUTH_SOCK").is_err() {
        eprintln!("skipping: no SSH_AUTH_SOCK");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();

    let agent_spec = TransportSpec::Ssh {
        host: e.host.clone(),
        port: e.port,
        user: e.user.clone(),
        auth: SshAuth::Agent,
    };

    let id = mgr
        .open(
            agent_spec,
            80,
            24,
            Arc::new(move |b: Bytes| seen2.lock().unwrap().extend_from_slice(&b)),
            Arc::new(|| {}),
        )
        .await
        .expect("agent auth should succeed");

    mgr.write(id, Bytes::from("echo AGENT_OK\n")).unwrap();
    let out = wait_for(&seen, "AGENT_OK", 10).await;
    assert!(out.contains("AGENT_OK"), "no output via agent auth:\n{out}");
    mgr.close(id).unwrap();
}

/// Regression: a stalled consumer plus a stdin-ignoring remote command must
/// not wedge the input path.
///
/// The transport used to serve both directions from one task via `select!`.
/// Two things then conspire:
///
///   1. Our own output channel is bounded (256). A slow consumer -- the real
///      app pushes every byte through taps, logging and IPC to the webview --
///      makes `out_tx.send().await` block, and because that happens *inside* a
///      select arm the task stops servicing the other arm entirely.
///   2. Writing blocks once the SSH send window is exhausted, which happens
///      whenever the remote process is not reading stdin. The window is only
///      replenished by a WindowAdjust, which russh can only process while its
///      session loop runs -- and that loop blocks as soon as the bounded
///      per-channel buffer (100 messages) fills because nobody is draining
///      `wait()`.
///
/// The single task then cannot make progress on the direction that would
/// unblock it. Output keeps arriving, typing is dead forever, and because
/// `SshTransport::write` awaits a bounded(64) command channel every later
/// keystroke is swallowed too.
///
/// Needs a multi-thread runtime: the stalled consumer blocks a worker thread on
/// purpose, which would wedge a current-thread runtime all by itself and prove
/// nothing. The stall is deadline-based for the same reason -- it must release
/// itself without needing another task to get scheduled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_stays_writable_when_consumer_stalls_and_window_fills() {
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();

    // Epoch-millis deadline the consumer parks until. 0 means "run freely".
    let stall_until = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stall_until2 = stall_until.clone();
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    };

    let id = mgr
        .open(
            spec(&e),
            80,
            24,
            Arc::new(move |b: Bytes| {
                while now_ms() < stall_until2.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(20));
                }
                seen2.lock().unwrap().extend_from_slice(&b);
            }),
            Arc::new(|| {}),
        )
        .await
        .expect("ssh session should open");

    wait_for(&seen, "$", 10).await;

    // Noisy command that never reads stdin, so the remote receive window is
    // never drained and the server stops replenishing our send window.
    mgr.write(id, Bytes::from("seq 1 20000000\n")).unwrap();
    tokio::time::sleep(Duration::from_millis(750)).await;

    // Freeze the consumer so our bounded output channel backs up behind it.
    stall_until.store(now_ms() + 8_000, std::sync::atomic::Ordering::SeqCst);

    // Shove at stdin to exhaust the SSH send window while the reader is stuck.
    let blob = Bytes::from(vec![b'x'; 32 * 1024]);
    for _ in 0..512 {
        let _ = mgr.write(id, blob.clone());
    }

    // Well past the stall deadline: a healthy transport has fully recovered.
    tokio::time::sleep(Duration::from_secs(14)).await;

    // Interrupt the noisy command and discard the stdin garbage.
    for _ in 0..3 {
        mgr.write(id, Bytes::from("\x03")).unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    seen.lock().unwrap().clear();
    mgr.write(id, Bytes::from("echo STILL_ALIVE_MARKER\n"))
        .unwrap();
    let out = wait_for(&seen, "STILL_ALIVE_MARKER", 25).await;
    let tail: String = out
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    assert!(
        out.contains("STILL_ALIVE_MARKER"),
        "input path wedged after consumer stall + window exhaustion -- the \
         read/write split regressed. Last output seen:\n{tail}"
    );

    mgr.close(id).unwrap();
}
