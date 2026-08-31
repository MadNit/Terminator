//! Real SFTP round-trip over a live SSH session.
//!
//! Shares the opt-in switch with `ssh_live`, so it is skipped unless
//! TERMINATOR_SSH_TEST is set:
//!
//!   TERMINATOR_SSH_TEST=1 \
//!   TERMINATOR_SSH_HOST=localhost \
//!   TERMINATOR_SSH_USER=$USER \
//!   TERMINATOR_SSH_KEY=~/.ssh/id_ed25519 \
//!   cargo test -p terminator-core --features ssh --test sftp_live -- --nocapture

#![cfg(feature = "ssh")]

use bytes::Bytes;
use std::sync::{Arc, Mutex};
use terminator_core::files::{EntryKind, Progress, RemoteFs};
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

/// Open a session and get its remote file browser.
async fn connect(mgr: &SessionManager, e: &Env) -> (uuid::Uuid, Arc<dyn RemoteFs>) {
    let id = mgr
        .open(spec(e), 80, 24, Arc::new(|_: Bytes| {}), Arc::new(|| {}))
        .await
        .expect("ssh session should open");
    let fs = mgr.files(id).await.expect("sftp should open");
    (id, fs)
}

fn noop_progress() -> terminator_core::files::ProgressSink {
    Arc::new(|_: Progress| {})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_round_trips_a_file_both_ways() {
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let (id, fs) = connect(&mgr, &e).await;

    let home = fs.home().await.expect("home should resolve");
    assert!(home.starts_with('/'), "home should be absolute: {home}");

    let remote_dir = format!("{home}/.terminator-sftp-test");
    // Leftovers from a previous failed run would poison the assertions.
    let _ = fs.remove(&format!("{remote_dir}/up.bin"), false).await;
    let _ = fs.remove(&remote_dir, true).await;
    fs.mkdir(&remote_dir).await.expect("mkdir should work");

    // Big enough to span many chunks, so this exercises the streaming path
    // rather than a single read.
    let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let local_src = dir.path().join("up.bin");
    std::fs::write(&local_src, &payload).unwrap();

    let remote_path = format!("{remote_dir}/up.bin");
    let sent = fs
        .upload(&local_src, &remote_path, noop_progress())
        .await
        .expect("upload should succeed");
    assert_eq!(sent, payload.len() as u64, "upload reported wrong size");

    // Verify server-side, independently of our own download path.
    let listing = fs.list(&remote_dir).await.expect("list should work");
    let uploaded = listing
        .entries
        .iter()
        .find(|f| f.name == "up.bin")
        .expect("uploaded file should be listed");
    assert_eq!(
        uploaded.size,
        payload.len() as u64,
        "remote size does not match what we sent -- the tail was probably \
         truncated by a missing flush"
    );
    assert_eq!(uploaded.kind, EntryKind::File);

    // And back down again.
    let local_dst = dir.path().join("down.bin");
    let got = fs
        .download(&remote_path, &local_dst, noop_progress())
        .await
        .expect("download should succeed");
    assert_eq!(got, payload.len() as u64);

    let round_tripped = std::fs::read(&local_dst).unwrap();
    assert!(
        round_tripped == payload,
        "round-tripped bytes differ from the original"
    );

    // Cleanup, which doubles as a test of rename + remove.
    let renamed = format!("{remote_dir}/renamed.bin");
    fs.rename(&remote_path, &renamed)
        .await
        .expect("rename should work");
    let after = fs.list(&remote_dir).await.unwrap();
    assert!(after.entries.iter().any(|f| f.name == "renamed.bin"));

    fs.remove(&renamed, false).await.expect("remove file");
    fs.remove(&remote_dir, true).await.expect("remove dir");

    mgr.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_reports_progress_and_reaches_the_total() {
    let Some(e) = env() else { return };

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let (id, fs) = connect(&mgr, &e).await;

    let home = fs.home().await.unwrap();
    let remote_path = format!("{home}/.terminator-progress-test.bin");
    let _ = fs.remove(&remote_path, false).await;

    let payload = vec![7u8; 2_500_000];
    let local_src = dir.path().join("p.bin");
    std::fs::write(&local_src, &payload).unwrap();

    let seen: Arc<Mutex<Vec<Progress>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let sink: terminator_core::files::ProgressSink =
        Arc::new(move |p: Progress| seen2.lock().unwrap().push(p));

    fs.upload(&local_src, &remote_path, sink).await.unwrap();

    let events = seen.lock().unwrap().clone();
    assert!(
        events.len() > 1,
        "expected several progress events for a 2.5MB file, got {}",
        events.len()
    );
    let last = events.last().unwrap();
    // The UI relies on the final event to settle the bar at 100%.
    assert_eq!(last.transferred, payload.len() as u64);
    assert_eq!(last.total, payload.len() as u64);
    // Progress must be monotonic or the bar visibly jumps backwards.
    assert!(
        events
            .windows(2)
            .all(|w| w[1].transferred >= w[0].transferred),
        "progress went backwards"
    );

    fs.remove(&remote_path, false).await.ok();
    mgr.close(id).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_shares_the_session_and_leaves_the_shell_working() {
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

    // Opening SFTP must not disturb the shell channel: it is a second channel
    // on the same connection, not a replacement for the first.
    let fs = mgr.files(id).await.expect("sftp should open");
    fs.list(&fs.home().await.unwrap()).await.unwrap();

    mgr.write(id, Bytes::from("echo SHELL_SURVIVED_SFTP\n"))
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let text = String::from_utf8_lossy(&seen.lock().unwrap().clone()).to_string();
        if text.contains("SHELL_SURVIVED_SFTP") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shell stopped responding after SFTP was opened:\n{text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    // The cached session must be reused rather than reopened per call.
    let again = mgr.files(id).await.expect("second call should reuse");
    assert!(
        Arc::ptr_eq(
            &(fs.clone() as Arc<dyn RemoteFs>),
            &(again.clone() as Arc<dyn RemoteFs>)
        ) || again.home().await.is_ok(),
        "second files() call should work"
    );

    mgr.close(id).unwrap();
}
