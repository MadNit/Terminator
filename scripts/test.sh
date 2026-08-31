#!/usr/bin/env bash
# Runs the full check suite: Rust tests, clippy, formatting and TypeScript.
#
#   ./scripts/test.sh              everything
#   ./scripts/test.sh --fast       tests only, skip lint/format
#   ./scripts/test.sh --ssh        also run the live SSH tests (see below)
#
# The live SSH tests need a real host and are skipped unless TERMINATOR_SSH_TEST
# is set, because they would otherwise fail on every contributor's machine.

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

FAST=0
for arg in "$@"; do
  case "$arg" in
    --fast) FAST=1 ;;
    --ssh)  export TERMINATOR_SSH_TEST="${TERMINATOR_SSH_TEST:-1}" ;;
  esac
done

require_toolchain
ensure_node_modules
cd "$REPO_ROOT"

info "cargo test"
cargo test --workspace
ok "Rust tests"

info "tsc --noEmit"
npx tsc --noEmit
ok "TypeScript"

if (( FAST == 0 )); then
  if cargo fmt --version >/dev/null 2>&1; then
    info "cargo fmt --check"
    cargo fmt --all -- --check || die "formatting issues; run: cargo fmt --all"
    ok "formatting"
  else
    warn "rustfmt not installed; skipping (rustup component add rustfmt)"
  fi

  if cargo clippy --version >/dev/null 2>&1; then
    info "cargo clippy"
    cargo clippy --workspace --all-targets -- -D warnings
    ok "clippy"
  else
    warn "clippy not installed; skipping (rustup component add clippy)"
  fi
fi

printf '\n'
ok "all checks passed"
