#!/usr/bin/env bash
# Runs Terminator in development mode: Vite dev server + hot reload, with the
# Rust side rebuilt on change.
#
#   ./scripts/dev.sh                 normal run
#   ./scripts/dev.sh --vault         force the encrypted-vault secret backend
#   ./scripts/dev.sh --trace         verbose logging
#
# Any other arguments are passed through to `tauri dev`.

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

LOG_LEVEL="terminator=info,frontend=info"
PASSTHRU=()

for arg in "$@"; do
  case "$arg" in
    --vault)
      # Useful on macOS: unsigned dev builds get a new code signature on every
      # rebuild, so the keychain ACL never matches and the OS prompts for the
      # login password constantly. The vault sidesteps that entirely.
      export TERMINATOR_FORCE_FILE_SECRETS=1
      info "using the encrypted vault instead of the OS keychain"
      ;;
    --trace) LOG_LEVEL="terminator=trace,frontend=trace" ;;
    *)       PASSTHRU+=("$arg") ;;
  esac
done

require_toolchain
ensure_node_modules

cd "$REPO_ROOT"
export RUST_LOG="${RUST_LOG:-$LOG_LEVEL}"
# A panic inside a Tauri command otherwise unwinds into an opaque IPC error.
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

info "starting dev build (first compile takes a few minutes)"
# See the note in build.sh: this form is empty-array-safe on bash 3.2 (macOS).
exec npm run tauri dev -- ${PASSTHRU[@]+"${PASSTHRU[@]}"}
