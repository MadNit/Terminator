#!/usr/bin/env bash
# Installs everything needed to build Terminator on macOS or Linux.
#
# Safe to re-run: every step checks before it installs.

source "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"

OS="$(os_name)"
[[ "$OS" == unsupported ]] && die "unsupported OS: $(uname -s). Use scripts/setup.ps1 on Windows."

info "setting up Terminator build environment ($OS)"

# ---------------------------------------------------------------- system deps

if [[ "$OS" == macos ]]; then
  # The Rust toolchain needs a linker, which on macOS ships with the Xcode
  # Command Line Tools. `rusqlite`'s bundled SQLite also needs a C compiler.
  if ! xcode-select -p >/dev/null 2>&1; then
    info "installing Xcode Command Line Tools (a GUI dialog will appear)"
    xcode-select --install || true
    die "re-run this script once the Command Line Tools finish installing"
  fi
  ok "Xcode Command Line Tools"

elif [[ "$OS" == linux ]]; then
  # Tauri v2 renders through WebKitGTK 4.1. The 4.0 packages are for Tauri v1
  # and will fail to link.
  PKGS=(
    build-essential curl wget file pkg-config
    libwebkit2gtk-4.1-dev
    libssl-dev
    libayatana-appindicator3-dev
    librsvg2-dev
    libxdo-dev
  )
  if have apt-get; then
    info "installing system packages via apt (sudo required)"
    sudo apt-get update
    sudo apt-get install -y "${PKGS[@]}"
  elif have dnf; then
    info "installing system packages via dnf (sudo required)"
    sudo dnf install -y \
      webkit2gtk4.1-devel openssl-devel curl wget file \
      libappindicator-gtk3-devel librsvg2-devel libxdo-devel \
      gcc gcc-c++ make
  elif have pacman; then
    info "installing system packages via pacman (sudo required)"
    sudo pacman -Syu --needed --noconfirm \
      webkit2gtk-4.1 base-devel curl wget file openssl \
      libappindicator-gtk3 librsvg xdotool
  else
    warn "unrecognised distro -- install these manually:"
    printf '  %s\n' "${PKGS[@]}" >&2
  fi
  ok "system packages"
fi

# ---------------------------------------------------------------------- rust

load_toolchain_paths
if ! have cargo; then
  info "installing Rust via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  load_toolchain_paths
fi
have cargo || die "Rust install failed; see https://rustup.rs"
ok "Rust $(rustc --version | cut -d' ' -f2)"

# ---------------------------------------------------------------------- node

if ! have node; then
  if [[ "$OS" == macos ]] && have brew; then
    info "installing Node.js via Homebrew"
    brew install node
  elif have apt-get; then
    warn "Node.js not found. Distro packages are usually too old for Vite 7."
    warn "Install Node 20+ from https://nodejs.org or via nvm, then re-run."
    die "Node.js 20+ required"
  else
    die "Node.js 20+ required -- https://nodejs.org"
  fi
fi
ok "Node.js $(node -v)"

# --------------------------------------------------------------- javascript

ensure_node_modules
ok "npm dependencies"

# ----------------------------------------------------------- warm the build

info "pre-fetching Rust dependencies (this is the slow part, once)"
cd "$REPO_ROOT"
cargo fetch --locked || cargo fetch

printf '\n'
ok "setup complete"
cat <<'EOF'

  Next:
    ./scripts/dev.sh      run the app with hot reload
    ./scripts/test.sh     run the test suite
    ./scripts/build.sh    produce a release bundle

EOF
