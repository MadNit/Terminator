//! End-to-end check that a real shell runs and both taps land on disk.

use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use terminator_core::{session::SessionManager, TransportSpec};

#[tokio::test]
async fn pty_session_streams_output_and_writes_both_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();
    let exited = Arc::new(Mutex::new(false));
    let exited2 = exited.clone();

    let id = mgr
        .open(
            TransportSpec::Local {
                shell: Some("/bin/bash".into()),
                cwd: None,
            },
            80,
            24,
            Arc::new(move |b: Bytes| seen2.lock().unwrap().extend_from_slice(&b)),
            Arc::new(move || *exited2.lock().unwrap() = true),
        )
        .await
        .expect("session should open");

    // Grab log paths before closing: the manager reaps on exit.
    let logs = mgr.logs(id).expect("logs available while running");

    tokio::time::sleep(Duration::from_millis(600)).await;
    mgr.write(id, Bytes::from("echo HELLO_FROM_TAP\n")).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let streamed = String::from_utf8_lossy(&seen.lock().unwrap().clone()).to_string();
    assert!(
        streamed.contains("HELLO_FROM_TAP"),
        "UI sink never received the output; got:\n{streamed}"
    );

    // Exit the shell so taps flush through the normal close path.
    mgr.write(id, Bytes::from("exit\n")).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert!(*exited.lock().unwrap(), "exit callback should have fired");

    // Raw .cast: replayable, escape codes intact, valid asciinema v2 header.
    let cast = std::fs::read_to_string(&logs.cast).expect("cast log written");
    let header: serde_json::Value = serde_json::from_str(cast.lines().next().unwrap()).unwrap();
    assert_eq!(header["version"], 2);
    assert_eq!(header["width"], 80);
    assert!(
        cast.contains("HELLO_FROM_TAP"),
        "cast log missing the command output"
    );

    // Plain text: clean and greppable.
    let plain = std::fs::read_to_string(&logs.plain).expect("plain log written");
    assert!(
        plain.contains("HELLO_FROM_TAP"),
        "plain log missing output; got:\n{plain}"
    );
    // Escape sequences must not survive into the plain log.
    assert!(
        !plain.contains('\u{1b}'),
        "plain log still contains escape sequences"
    );
}

#[tokio::test]
async fn idle_session_still_flushes_logs_to_disk() {
    // Regression: buffered writers previously only flushed on a *subsequent*
    // write, so a shell that printed its prompt and then went idle left an
    // empty log file until the session closed.
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    let id = mgr
        .open(
            TransportSpec::Local {
                shell: Some("/bin/bash".into()),
                cwd: None,
            },
            80,
            24,
            Arc::new(|_| {}),
            Arc::new(|| {}),
        )
        .await
        .unwrap();
    let logs = mgr.logs(id).unwrap();

    mgr.write(id, Bytes::from("echo IDLE_FLUSH_CHECK\n"))
        .unwrap();

    // Wait well past the flush tick, but never close the session.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let cast = std::fs::read_to_string(&logs.cast).unwrap();
    assert!(
        cast.contains("IDLE_FLUSH_CHECK"),
        "cast log not flushed while session still open:\n{cast}"
    );

    let plain = std::fs::read_to_string(&logs.plain).unwrap();
    assert!(
        plain.contains("IDLE_FLUSH_CHECK"),
        "plain log not flushed while session still open:\n{plain}"
    );

    mgr.close(id).unwrap();
}

#[tokio::test]
async fn resize_is_recorded_in_the_cast_log() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().to_path_buf());

    let id = mgr
        .open(
            TransportSpec::Local {
                shell: Some("/bin/bash".into()),
                cwd: None,
            },
            80,
            24,
            Arc::new(|_| {}),
            Arc::new(|| {}),
        )
        .await
        .unwrap();

    let logs = mgr.logs(id).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    mgr.resize(id, 120, 40).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    mgr.close(id).unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let cast = std::fs::read_to_string(&logs.cast).unwrap();
    assert!(
        cast.contains("120x40"),
        "resize event missing from cast log:\n{cast}"
    );
}

#[tokio::test]
async fn shell_integration_persists_command_history_to_the_store() {
    // The full loop: inject the OSC 133 snippet into a real bash, run real
    // commands, and assert the semantic markers survive all the way into
    // SQLite and the FTS index.
    let dir = tempfile::tempdir().unwrap();
    let store = terminator_core::store::Store::open(&dir.path().join("t.db")).unwrap();
    let mgr = SessionManager::new(dir.path().join("logs")).with_store(store.clone());

    let id = mgr
        .open(
            TransportSpec::Local {
                shell: Some("/bin/bash".into()),
                cwd: None,
            },
            80,
            24,
            Arc::new(|_| {}),
            Arc::new(|| {}),
        )
        .await
        .unwrap();

    // Enable shell integration, then run one succeeding and one failing command.
    // Sent verbatim -- the snippet is line-oriented shell source, and folding
    // it onto one line breaks `if ...; then` and `case ... esac`.
    mgr.write(
        id,
        Bytes::from(format!("{}\n", terminator_core::OSC133_BASH_ZSH)),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;

    mgr.write(id, Bytes::from("echo alpha_marker\n")).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    mgr.write(id, Bytes::from("false\n")).unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;

    let recorded = store.session_commands(&id.to_string()).unwrap();
    assert!(
        !recorded.is_empty(),
        "no commands captured -- shell integration did not reach the store"
    );

    let echo = recorded
        .iter()
        .find(|(c, _, _)| c.contains("alpha_marker"))
        .unwrap_or_else(|| panic!("echo not recorded; got {recorded:?}"));
    assert_eq!(echo.1, Some(0), "successful command should exit 0");

    let failed = recorded
        .iter()
        .find(|(c, _, _)| c.trim() == "false")
        .unwrap_or_else(|| panic!("false not recorded; got {recorded:?}"));
    assert_eq!(failed.1, Some(1), "failing command should exit 1");

    // And it must be findable through full-text search.
    let hits = store.search_commands("alpha_marker", 10).unwrap();
    assert!(!hits.is_empty(), "FTS index missed the recorded command");

    mgr.close(id).unwrap();
}

#[tokio::test]
async fn shell_integration_is_enabled_automatically_without_user_setup() {
    // The point of auto-integration: the user pastes nothing, yet history
    // still records. Note this test never writes the OSC 133 snippet.
    for shell in ["/bin/bash", "/bin/zsh"] {
        if !std::path::Path::new(shell).exists() {
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let store = terminator_core::store::Store::open(&dir.path().join("t.db")).unwrap();
        let mgr = SessionManager::new(dir.path().join("logs")).with_store(store.clone());

        let id = mgr
            .open(
                TransportSpec::Local {
                    shell: Some(shell.into()),
                    cwd: None,
                },
                80,
                24,
                Arc::new(|_| {}),
                Arc::new(|| {}),
            )
            .await
            .unwrap();

        // Let the shell finish loading rc files before typing.
        tokio::time::sleep(Duration::from_millis(900)).await;
        mgr.write(id, Bytes::from("echo auto_marker\n")).unwrap();
        tokio::time::sleep(Duration::from_millis(900)).await;

        let recorded = store.session_commands(&id.to_string()).unwrap();
        assert!(
            recorded.iter().any(|(c, _, _)| c.contains("auto_marker")),
            "{shell}: auto integration did not record the command; got {recorded:?}"
        );
        mgr.close(id).unwrap();
    }
}

#[tokio::test]
async fn user_shell_config_still_loads_under_integration() {
    // Integration must be transparent: replacing the rc file must not cost
    // the user their own configuration.
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bashrc"),
        "export USER_RC_LOADED=beacon_value\n",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::new(dir.path().join("logs"));
    let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let seen2 = seen.clone();

    // Point HOME at the fake profile for the duration of the spawn.
    let prev = std::env::var("HOME").ok();
    std::env::set_var("HOME", home.path());

    let id = mgr
        .open(
            TransportSpec::Local {
                shell: Some("/bin/bash".into()),
                cwd: None,
            },
            80,
            24,
            Arc::new(move |b: Bytes| seen2.lock().unwrap().extend_from_slice(&b)),
            Arc::new(|| {}),
        )
        .await
        .unwrap();

    if let Some(p) = prev {
        std::env::set_var("HOME", p);
    }

    tokio::time::sleep(Duration::from_millis(900)).await;
    mgr.write(id, Bytes::from("echo \"rc=$USER_RC_LOADED\"\n"))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;

    let out = String::from_utf8_lossy(&seen.lock().unwrap().clone()).to_string();
    assert!(
        out.contains("rc=beacon_value"),
        "user's .bashrc was not sourced under integration:\n{out}"
    );
    mgr.close(id).unwrap();
}
