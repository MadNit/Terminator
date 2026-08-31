//! OSC 133 semantic command capture.
//!
//! These drive the parser with synthetic byte streams so the assertions are
//! deterministic -- no shell, no timing, no prompt-theme variance.

use std::sync::{Arc, Mutex};
use terminator_core::tap::plain::{CommandRecord, CommandSink, PlainTap};
use terminator_core::tap::{Direction, Tap};

/// Feed `bytes` through a PlainTap and return everything the sink captured,
/// plus the resulting plain-text log.
fn run(bytes: &[u8]) -> (Vec<CommandRecord>, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.log");

    let got: Arc<Mutex<Vec<CommandRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_out = got.clone();
    let sink: CommandSink = Box::new(move |rec| sink_out.lock().unwrap().push(rec));

    let tap = PlainTap::with_sink(&path, Some(sink)).unwrap();
    tap.on_data(Direction::Output, bytes);
    tap.on_close();

    let log = std::fs::read_to_string(&path).unwrap();
    let recs = got.lock().unwrap().clone();
    (recs, log)
}

#[test]
fn captures_command_exit_code_and_output() {
    let stream = concat!(
        "\x1b]133;A\x07",         // prompt start
        "user@host ~ $ ",         // the prompt itself
        "\x1b]133;E;echo hi\x07", // command line, reported verbatim
        "echo hi\r\n",            // shell echoes what was typed
        "\x1b]133;C\x07",         // output begins
        "hi\r\n",
        "\x1b]133;D;0\x07", // finished, exit 0
    );
    let (recs, log) = run(stream.as_bytes());

    assert_eq!(recs.len(), 1, "expected exactly one command, got {recs:?}");
    assert_eq!(recs[0].command, "echo hi");
    assert_eq!(recs[0].exit_code, Some(0));

    // The plain log stays human-readable and free of escape codes.
    assert!(log.contains("hi"), "output missing from log:\n{log}");
    assert!(!log.contains('\u{1b}'), "escape codes leaked:\n{log}");
}

#[test]
fn captures_nonzero_exit_code() {
    let stream = concat!(
        "\x1b]133;A\x07",
        "\x1b]133;E;false\x07",
        "\x1b]133;C\x07",
        "\x1b]133;D;1\x07",
    );
    let (recs, _) = run(stream.as_bytes());
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].command, "false");
    assert_eq!(recs[0].exit_code, Some(1));
}

#[test]
fn command_containing_semicolons_survives_osc_param_splitting() {
    // OSC parameters are split on ';', so this is the case that silently
    // truncates history if the parser does not rejoin the tail.
    let stream = concat!(
        "\x1b]133;A\x07",
        "\x1b]133;E;cd /tmp; ls -la; echo done\x07",
        "\x1b]133;C\x07",
        "\x1b]133;D;0\x07",
    );
    let (recs, _) = run(stream.as_bytes());
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].command, "cd /tmp; ls -la; echo done");
}

#[test]
fn falls_back_to_echoed_text_when_no_explicit_command_marker() {
    // Third-party integrations emit only A/B/C/D. We should still recover the
    // command from what the shell echoed between B and C.
    let stream = concat!(
        "\x1b]133;A\x07",
        "$ ",
        "\x1b]133;B\x07",
        "uptime",
        "\r\n\x1b]133;C\x07",
        "load average: 1.0\r\n",
        "\x1b]133;D;0\x07",
    );
    let (recs, _) = run(stream.as_bytes());
    assert_eq!(recs.len(), 1, "expected one command, got {recs:?}");
    assert_eq!(recs[0].command, "uptime");
}

#[test]
fn explicit_marker_wins_over_echoed_text() {
    // With syntax highlighting the shell rewrites the line after typing, so
    // the echo is unreliable. `E` must take precedence.
    let stream = concat!(
        "\x1b]133;A\x07",
        "\x1b]133;E;git status\x07",
        "\x1b]133;B\x07",
        "git \x1b[32mstatus\x1b[0m", // redrawn with colour
        "\x1b]133;C\x07",
        "\x1b]133;D;0\x07",
    );
    let (recs, _) = run(stream.as_bytes());
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].command, "git status");
}

#[test]
fn bare_prompt_with_no_command_records_nothing() {
    // Shells emit D on the very first prompt, before anything has run.
    let stream = "\x1b]133;D;0\x07\x1b]133;A\x07user@host ~ $ ";
    let (recs, _) = run(stream.as_bytes());
    assert!(recs.is_empty(), "recorded a phantom command: {recs:?}");
}

#[test]
fn state_resets_between_commands() {
    let stream = concat!(
        "\x1b]133;A\x07\x1b]133;E;first\x07\x1b]133;C\x07\x1b]133;D;0\x07",
        "\x1b]133;A\x07\x1b]133;E;second\x07\x1b]133;C\x07\x1b]133;D;2\x07",
    );
    let (recs, _) = run(stream.as_bytes());
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].command, "first");
    assert_eq!(recs[0].exit_code, Some(0));
    assert_eq!(recs[1].command, "second");
    assert_eq!(recs[1].exit_code, Some(2));
}
