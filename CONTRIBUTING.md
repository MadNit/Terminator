# Contributing

Thanks for taking a look.

## Getting set up

```sh
./scripts/setup.sh                                            # macOS / Linux
powershell -ExecutionPolicy Bypass -File scripts\setup.ps1    # Windows
```

Then `./scripts/dev.sh` (or `scripts\dev.ps1`) to run the app with hot reload.

## Before opening a PR

```sh
./scripts/test.sh
```

This runs exactly what CI runs: `cargo test`, `tsc --noEmit`, `cargo fmt
--check` and `cargo clippy -D warnings`. Running it locally saves a round trip.

## Project layout

- **`core/`** — the headless engine. No UI dependencies, on purpose: it can be
  driven from tests, a CLI, or a different frontend. New protocol and logging
  work belongs here.
- **`src-tauri/`** — a thin adapter exposing `core` to the webview. Keep logic
  out of it; if a command is doing real work, that work probably belongs in
  `core`.
- **`src/`** — React + xterm.js.

## Dependency pins

`russh` and `ironrdp` are pinned to exact versions in the root `Cargo.toml`.
They both ride the RustCrypto pre-release chain and **do not co-resolve** at
their latest versions. The comment above `[workspace.dependencies]` explains
the working combination — read it before bumping either, and bump them
together.

## Testing notes

- The live SSH tests in `core/tests/ssh_live.rs` need a real host and are
  skipped unless `TERMINATOR_SSH_TEST` is set. CI does not set it.
- `TERMINATOR_FORCE_FILE_SECRETS=1` forces the encrypted vault instead of the
  OS keychain, which makes the fallback path testable on a machine whose
  keychain works fine.

## A note on the UI

Terminal panes own real OS resources — a PTY or an SSH connection. React
StrictMode double-invokes effects in development, so any effect that creates a
session must survive being cleaned up and re-run without leaking or killing the
session. `TerminalPane` handles this with a deferred teardown; please don't
"simplify" it away without reading the comment there.

## Style

- Comment the non-obvious: why a workaround exists, not what a line does.
- Prefer fixing the cause over adding a guard around the symptom.
