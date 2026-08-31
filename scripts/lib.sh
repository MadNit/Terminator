#!/usr/bin/env bash
# Shared helpers for the macOS/Linux scripts.
#
# Sourced, never executed directly.

# Fail loudly: an unset variable or a failed command in a pipeline should stop
# the build rather than silently produce a broken bundle.
set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'; C_BLUE=$'\033[34m'; C_GREEN=$'\033[32m'
  C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'
else
  C_RESET=""; C_BLUE=""; C_GREEN=""; C_YELLOW=""; C_RED=""
fi

info()  { printf '%s==>%s %s\n'  "$C_BLUE"   "$C_RESET" "$*"; }
ok()    { printf '%s  ok%s %s\n' "$C_GREEN"  "$C_RESET" "$*"; }
warn()  { printf '%swarn%s %s\n' "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()   { printf '%serror%s %s\n' "$C_RED"   "$C_RESET" "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

os_name() {
  case "$(uname -s)" in
    Darwin) echo macos ;;
    Linux)  echo linux ;;
    *)      echo unsupported ;;
  esac
}

# Rust and Node are commonly installed somewhere a non-interactive shell won't
# see (rustup, nvm, asdf). Pick them up so these scripts also work from GUI
# launchers, cron and CI.
load_toolchain_paths() {
  # shellcheck disable=SC1091
  [[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
  local d
  for d in "$HOME/.cargo/bin" "$HOME/.local/opt/node/bin"; do
    if [[ -d "$d" && ":$PATH:" != *":$d:"* ]]; then PATH="$d:$PATH"; fi
  done
  export PATH
}

require_toolchain() {
  load_toolchain_paths
  local missing=0
  have cargo || { warn "cargo not found -- install Rust: https://rustup.rs"; missing=1; }
  have node  || { warn "node not found -- install Node.js 20+: https://nodejs.org"; missing=1; }
  have npm   || { warn "npm not found (ships with Node.js)"; missing=1; }
  (( missing == 0 )) || die "missing prerequisites; run scripts/setup.sh first"

  # Vite 7 and the Tauri CLI both need a modern Node. Checking here gives a
  # clear message instead of a cryptic syntax error deep inside a build.
  local major
  major="$(node -p 'process.versions.node.split(".")[0]')"
  (( major >= 20 )) || die "Node.js 20+ required, found $(node -v)"
}

ensure_node_modules() {
  cd "$REPO_ROOT"
  # Compare against the lockfile so a dependency bump can't silently build
  # against stale packages.
  if [[ ! -d node_modules ]] || [[ package-lock.json -nt node_modules ]]; then
    info "installing npm dependencies"
    npm ci 2>/dev/null || npm install
    touch node_modules
  fi
}
