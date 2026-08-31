<div align="center">

# Terminator

**A cross-platform, multi-tab terminal with saved sessions, SSH, RDP, SFTP, and logging you actually own.**

[![CI](https://github.com/MadNit/Terminator/actions/workflows/ci.yml/badge.svg)](https://github.com/MadNit/Terminator/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/MadNit/Terminator?include_prereleases)](https://github.com/MadNit/Terminator/releases)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

Terminator is a modern, cross-platform terminal emulator and remote access suite (MobaXterm/PuTTY style) built with **Tauri v2**, **Rust**, **React 19**, **TypeScript**, and **xterm.js**.

Unlike traditional terminal emulators, Terminator is designed around a unified **Tap stream architecture**: every byte traveling across a session passes through a central headless tap pipeline. A single live session stream simultaneously paints the interactive WebGL terminal canvas, parses VT sequences headlessly for state tracking, writes searchable plain-text logs to disk, and records timestamps in asciinema `.cast` v2 format for instant session playback.

---

## Table of Contents

- [Features](#features)
  - [Terminal & Session Management](#terminal--session-management)
  - [SSH Remote Access](#ssh-remote-access)
  - [RDP Remote Desktop](#rdp-remote-desktop)
  - [SFTP Remote File Browser Drawer](#sftp-remote-file-browser-drawer)
  - [Security & Credential Management](#security--credential-management)
  - [Session Logging & asciinema Recording](#session-logging--asciinema-recording)
  - [Shell Integration (OSC 133)](#shell-integration-osc-133)
- [Keyboard Shortcuts](#keyboard-shortcuts)
- [Pre-Built Installation](#pre-built-installation)
  - [macOS](#macos)
  - [Windows](#windows)
  - [Linux](#linux)
- [Building from Source](#building-from-source)
  - [Prerequisites Overview](#prerequisites-overview)
  - [macOS Build Guide](#macos-build-guide)
  - [Linux Build Guide](#linux-build-guide)
  - [Windows Build Guide](#windows-build-guide)
  - [Development Workflow](#development-workflow)
  - [Production Bundle Builds](#production-bundle-builds)
- [Testing & Quality Assurance](#testing--quality-assurance)
  - [Running Unit & Integration Tests](#running-unit--integration-tests)
  - [Live SSH & RDP Testing](#live-ssh--rdp-testing)
  - [Linting & Type Checking](#linting--type-checking)
- [Architecture](#architecture)
- [Configuration & Environment Variables](#configuration--environment-variables)
- [Contributing](#contributing)
- [License](#license)

---

## Features

### Terminal & Session Management
- **True Multi-Tab PTY**: Native pseudo-terminal allocation across macOS, Linux, and Windows using `portable-pty`.
- **Multi-Pane Split Views (1x1, 1x2, 2x1, 2x2 Grid)**: Split your workspace into side-by-side vertical splits, horizontal stacked panes, or a 2x2 multi-terminal grid (MobaXterm style).
- **Multi-Exec / Keystroke Broadcasting (⚡)**: One-click toggle in top navigation to broadcast typed input simultaneously across all active terminal sessions and split panes.
- **GPU-Accelerated Rendering**: Powered by `xterm.js` and `@xterm/addon-webgl` for high-throughput, low-latency terminal rendering.
- **Saved Connection Profiles**: Persist SSH, RDP, and local terminal profiles with custom arguments, working directories, and host configurations in a local SQLite database (`rusqlite`).
- **Auto-Reconnection & Keep-Alive Resilience**:
  - **SSH Keep-Alive Heartbeats**: Periodic keepalive probe pings keep firewalls and NAT routers from dropping idle sessions.
  - **Intelligent Exponential Backoff**: Automatically attempts reconnection on dropped sessions with countdown timer (1s, 2s, 4s, 8s, 16s), manual "Reconnect Now" override, and in-place tab restoration.
- **Live Reconnect & Resilient Tabs**: Reconnect disconnected sessions in place without losing tab layout or credential mappings.

### SSH Remote Access, Jump Hosts & Tunneling
- **Full-featured SSH Client**: Built directly on asynchronous Rust (`russh` and Tokio).
- **Jump Hosts & SSH ProxyJump (`ssh -J`)**: Connect transparently to target servers behind firewalls or private VPCs via intermediate bastion / jump hosts with end-to-end encryption.
- **Flexible Authentication**: Supports password authentication, private keys (RSA, Ed25519, ECDSA), and native OpenSSH `ssh-agent` forwarding/querying.
- **SSH Port Forwarding & Tunnels Manager**:
  - **Local Port Forwarding (`-L`)**: Forward local client ports to remote server ports through secure SSH channels.
  - **Dynamic SOCKS5 Proxy (`-D`)**: Run a local SOCKS5 proxy server routed directly through remote SSH sessions for web browsers and application proxying.
  - **Remote Port Forwarding (`-R`)**: Expose local services and web servers through remote SSH server ports.
  - **Live Monitoring & Metrics**: View active connections, live upload/download transfer rates (`bytes_tx`, `bytes_rx`), and one-click start/stop controls.
- **Non-blocking Concurrency**: High-throughput multiplexed I/O channels for terminal interactive sessions and SFTP subsystems concurrently.

### RDP Remote Desktop
- **Secure NLA / CredSSP**: Native Remote Desktop Protocol client built on `ironrdp` with Network Level Authentication (NTLM/CredSSP).
- **Responsive Viewport & Dynamic Resize**: Viewport dynamically adapts to client window dimensions and reports resolution changes to the remote desktop server.
- **Accurate Scancode Input**: Translates modern web keyboard and mouse events directly into native Windows scancodes and mouse motion packets.

### Direct Terminal File Transfer & SFTP Drawer
- **Direct Terminal Drag & Drop Upload**: Drag any file directly from Finder / File Explorer onto an active SSH terminal pane to trigger an immediate, high-speed SFTP streaming upload to the shell's **active working directory** (automatically resolved via OSC 7, OSC 133 semantic CWD markers, shell window title updates, and prompt path inspection), with live progress toast and terminal notification.
- **Remote Host File Drawer (⌘J / Ctrl+J)**: Integrated slide-out file browser docked to the active SSH session tab.
- **Desktop Drag & Drop**: Drop files directly from Finder / File Explorer onto the remote directory drawer to trigger streaming SFTP uploads.
- **Clipboard Integration**: Copy files in your OS file manager and paste (⌘V / Ctrl+V) directly into the remote drawer.
- **Direct Drag Out & File Dialogs**: Drag remote files out to your desktop or use native OS Save/Upload file dialogs for large files.
- **Path Copying**: Instant ⌘C / Ctrl+C copying of selected remote paths.

### Snippets Library & Command Palette (⌘K)
- **Parameterized Snippets Library**: Create, categorize, and organize reusable commands and multi-line scripts.
- **Dynamic Parameter Prompts (`{{variable}}`)**: Define placeholders inside commands (e.g. `docker logs -f {{container}} --tail {{lines}}`) and receive interactive prompts to supply values before execution.
- **Unified Command Palette (⌘K / Ctrl+K)**: Quick launcher to search across all saved connections, snippets, recordings, layout actions, and multi-exec toggles with instant keyboard execution.

### Security, Known Hosts & Credential Management
- **SSH Known Hosts & Host Key Manager GUI**:
  - **Host Key Verification & TOFU**: Strict verification against known public keys, automatic Trust-On-First-Use (TOFU) recording, and explicit MITM warnings for altered keys.
  - **Known Hosts Inspection**: Search and inspect trusted host keys, key algorithms (Ed25519, ECDSA, RSA), and cryptographic SHA256 fingerprints.
  - **Host Key Revocation & Manual Trust**: One-click revocation of stale/rebuilt host keys and manual addition of trusted server public keys.
- **OS Native Keychain Storage**: Passwords and credentials securely stored using platform APIs (macOS Keychain via Security framework, Windows Credential Manager, Linux Secret Service / DBus).
- **Encrypted Fallback Vault**: For headless, containerized, or non-keychain environments, Terminator includes a zero-trust encrypted vault using **Argon2id** key derivation and **XChaCha20-Poly1305** AEAD encryption.
- **Memory Zeroization**: Sensitive cryptographic material and cleartext passwords are scrubbed on drop (`zeroize`).

### Session Logging & asciinema Recording
- **The "Tap" Multiplexer**: Every session passes through a multi-consumer fan-out stream (`core/src/tap/`).
- **Plain-Text Greppable Logs**: Headless VT escape code stripping via `vte` yields clean, searchable text logs in your local application directory.
- **asciinema v2 `.cast` Recording**: Full terminal timing and terminal dimensions captured out of the box for instant playback and audit trails.

### Shell Integration (OSC 133)
- **Automatic OSC 133 Hooks**: Injects semantic shell markers into Bash, Zsh, and Fish sessions.
- **Command & Exit Tracking**: Accurately capture command start, prompt start, output boundaries, and command exit status codes.

---

## Keyboard Shortcuts

| Shortcut (macOS) | Shortcut (Windows/Linux) | Action |
|---|---|---|
| <kbd>⌘</kbd> + <kbd>K</kbd> | <kbd>Ctrl</kbd> + <kbd>K</kbd> | Open Command Palette (search profiles, snippets, & actions) |
| <kbd>⇧</kbd> + <kbd>⌘</kbd> + <kbd>P</kbd> | <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>P</kbd> | Open Snippets & Command Library Modal |
| <kbd>⌘</kbd> + <kbd>B</kbd> | <kbd>Ctrl</kbd> + <kbd>B</kbd> | Toggle Connections Sidebar |
| <kbd>⌘</kbd> + <kbd>J</kbd> | <kbd>Ctrl</kbd> + <kbd>J</kbd> | Toggle SFTP Remote File Browser Drawer |
| <kbd>⌘</kbd> + <kbd>C</kbd> | <kbd>Ctrl</kbd> + <kbd>C</kbd> | Copy selected terminal text / remote file path |
| <kbd>⌘</kbd> + <kbd>V</kbd> | <kbd>Ctrl</kbd> + <kbd>V</kbd> | Paste text into terminal / upload copied file into SFTP drawer |

---

## Pre-Built Installation

Download ready-to-run packages from the [latest GitHub Releases](https://github.com/MadNit/Terminator/releases/latest).

| Platform | Architecture | Installer / Package |
|---|---|---|
| **macOS** | Apple Silicon (M1/M2/M3/M4) | `Terminator_*_aarch64.dmg` |
| **macOS** | Intel (x86_64) | `Terminator_*_x64.dmg` |
| **Windows** | 64-bit (x64) | `Terminator_*_x64-setup.exe` / `.msi` |
| **Linux (Debian / Ubuntu)** | x86_64 | `terminator_*_amd64.deb` |
| **Linux (Fedora / RHEL)** | x86_64 | `terminator-*.x86_64.rpm` |
| **Linux (Universal)** | x86_64 | `terminator_*_amd64.AppImage` |

### Platform Verification Notes

#### macOS
Pre-built binaries are distributed unsigned by default. When downloading via a browser, Gatekeeper may flag the file as quarantined:
```sh
# Remove the quarantine flag after moving Terminator to /Applications:
xattr -cr /Applications/Terminator.app
```

#### Windows
Windows SmartScreen may display an unrecognized application warning. Click **More info → Run anyway** to proceed.

#### Linux
```sh
# Debian / Ubuntu
sudo dpkg -i terminator_*_amd64.deb

# Fedora / RHEL / CentOS
sudo rpm -i terminator-*.x86_64.rpm

# AppImage (any Linux distribution)
chmod +x terminator_*.AppImage && ./terminator_*.AppImage
```

---

## Building from Source

Building locally produces binaries tailored to your system and completely avoids OS quarantine and signing prompts.

### Prerequisites Overview

| Dependency | Minimum Version | Notes |
|---|---|---|
| **Rust** | Stable (1.80+) | Installed via [rustup](https://rustup.rs) |
| **Node.js** | 20.x LTS+ | Needed for Vite frontend compilation & Tauri CLI |
| **npm** | 10.x+ | Ships with Node.js |
| **C/C++ Compiler & Linker** | Platform-dependent | Clang / GCC / MSVC (for building native dependencies like SQLite) |

---

### macOS Build Guide

#### 1. System Requirements
- macOS 12 Monterey or newer.
- Xcode Command Line Tools (provides `clang`, `make`, and Apple SDK linkers).

#### 2. Install Dependencies
```bash
# Install Xcode Command Line Tools
xcode-select --install

# Install Node.js (via Homebrew if not already present)
brew install node

# Install Rust stable toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

#### 3. Automated Setup
Run the setup script from the repository root to verify your toolchains and install Node dependencies:
```bash
./scripts/setup.sh
```

#### 4. Run Development Server
```bash
./scripts/dev.sh
```
> **macOS Development Tip:** Debug builds get re-signed on each compilation, which causes macOS to re-prompt for Keychain access on every run. Use the `--vault` flag during development to use the internal encrypted vault instead:
> ```bash
> ./scripts/dev.sh --vault
> ```

#### 5. Build Production DMG & .app
```bash
./scripts/build.sh
```
Compiled artifacts will be located in `target/release/bundle/macos/` (`Terminator.app` and `Terminator_*.dmg`).

---

### Linux Build Guide

Terminator uses WebKitGTK 4.1 for its Tauri v2 desktop frontend.

#### 1. Install System Dependencies

##### Debian / Ubuntu (22.04 LTS, 24.04 LTS, etc.):
```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  curl \
  wget \
  file \
  pkg-config \
  libwebkit2gtk-4.1-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libxdo-dev
```
> ⚠️ **Important:** Ensure you install `libwebkit2gtk-4.1-dev`. The legacy `4.0` package is for Tauri v1 and will fail to link.

##### Fedora / RHEL / AlmaLinux:
```bash
sudo dnf install -y \
  gcc \
  gcc-c++ \
  make \
  curl \
  wget \
  file \
  pkgconf-pkg-config \
  openssl-devel \
  webkit2gtk4.1-devel \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  libxdo-devel
```

##### Arch Linux / Manjaro:
```bash
sudo pacman -Syu --needed \
  base-devel \
  curl \
  wget \
  file \
  openssl \
  webkit2gtk-4.1 \
  libappindicator-gtk3 \
  librsvg \
  xdotool
```

#### 2. Install Rust and Node.js
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Install Node.js 20+ (using NodeSource or nvm if distro package is older)
# Example using NodeSource for Debian/Ubuntu:
curl -fsSL https://deb.nodesource.com/setup_20.x | sudo -E bash -
sudo apt-get install -y nodejs
```

#### 3. Automated Setup
```bash
./scripts/setup.sh
```

#### 4. Run Development Server
```bash
./scripts/dev.sh
```

#### 5. Build Production Packages (.deb, .rpm, .AppImage)
```bash
./scripts/build.sh
```
Bundles will be generated in `target/release/bundle/deb/`, `target/release/bundle/rpm/`, and `target/release/bundle/appimage/`.

---

### Windows Build Guide

#### 1. System Requirements
- Windows 10 (64-bit) or Windows 11.
- Visual Studio Build Tools with C++ workload.
- Microsoft Edge WebView2 Runtime (installed by default on Windows 11).

#### 2. Install Dependencies via Winget or Manual Install
Open **PowerShell as Administrator**:

```powershell
# 1. Install Visual Studio 2022 Build Tools (Desktop development with C++)
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# 2. Install WebView2 Runtime (if on older Windows 10)
winget install --id Microsoft.EdgeWebView2Runtime -e

# 3. Install Node.js LTS
winget install --id OpenJS.NodeJS.LTS -e

# 4. Install Rust (MSVC toolchain)
winget install --id Rustlang.Rustup -e
```
*Note: Restart your PowerShell terminal after installing to refresh environment variables.*

#### 3. Automated Setup
Run the setup script with PowerShell:
```powershell
powershell -ExecutionPolicy Bypass -File scripts\setup.ps1
```

#### 4. Run Development Server
```powershell
powershell -ExecutionPolicy Bypass -File scripts\dev.ps1
```

#### 5. Build Production Installer (.msi and .exe)
```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
```
Installers will land in `target\release\bundle\msi\` and `target\release\bundle\nsis\`.

---

### Development Workflow

| Action | macOS / Linux | Windows (PowerShell) | Description |
|---|---|---|---|
| **Setup Environment** | `./scripts/setup.sh` | `.\scripts\setup.ps1` | Validates dependencies & runs `npm install` + `cargo fetch` |
| **Start Dev App** | `./scripts/dev.sh` | `.\scripts\dev.ps1` | Launches Vite frontend + Tauri backend with hot reload |
| **Run Full Test Suite** | `./scripts/test.sh` | `.\scripts\test.ps1` | Runs frontend typechecks, clippy, fmt, and Rust unit tests |
| **Create Release Bundles** | `./scripts/build.sh` | `.\scripts\build.ps1` | Compiles optimized release binary and native installers |

---

## Testing & Quality Assurance

Terminator includes comprehensive testing across the headless core, transport abstractions, encryption vaults, and React components.

### Running Unit & Integration Tests

```bash
# Run full suite (typecheck, rustfmt, clippy, unit tests)
./scripts/test.sh

# Run tests only (skip linter checks for fast iteration)
./scripts/test.sh --fast

# On Windows:
powershell -ExecutionPolicy Bypass -File scripts\test.ps1 -Fast
```

### Live SSH & RDP Testing

By default, the test suite skips tests requiring live network hosts. You can opt into real server testing using environment variables:

#### SSH Live Testing
```bash
# Run with local SSH server test enabled
./scripts/test.sh --ssh
# Or set explicitly:
TERMINATOR_SSH_TEST=1 cargo test -p terminator-core --test ssh_live
```

#### RDP Live Testing
Tests end-to-end NLA/CredSSP authentication, packet negotiation, and pixel buffer rendering against a live Windows host:
```bash
TERMINATOR_RDP_TEST=192.168.1.100:3389 \
TERMINATOR_RDP_USER=Administrator \
TERMINATOR_RDP_PASS=SecretPassword123 \
TERMINATOR_RDP_DOMAIN=CORP \
  cargo test -p terminator-core --features rdp --test rdp_live -- --nocapture
```

Additional RDP validation variables:
- `TERMINATOR_RDP_CAPTURE=1`: Dumps the raw frame buffer to `/tmp/rdp_capture.raw` to verify color channels and pixel format.
- `TERMINATOR_RDP_INPUT=1`: Exercises keyboard scancodes (Ctrl+Esc) and mouse motion events safely without modifying remote state.

### Linting & Type Checking

```bash
# Frontend type check
npx tsc --noEmit

# Rust formatting check
cargo fmt --all -- --check

# Rust linter check
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Architecture

Terminator is organized into clean, modular layers separating business logic from the UI:

```
Terminator/
├── core/                  # Headless Rust engine (zero GUI dependencies)
│   ├── src/
│   │   ├── transport/     # PTY, SSH, SFTP, and RDP stream drivers
│   │   ├── tap/           # Multiplexing tap pipeline (plain text, asciinema v2 .cast)
│   │   ├── vault.rs       # Argon2id + XChaCha20-Poly1305 encrypted secret store
│   │   ├── secrets.rs     # Platform OS Keychain & fallback resolution
│   │   ├── store.rs       # SQLite connection profiles database
│   │   ├── session.rs     # Session lifecycle management
│   │   └── shell_init.rs  # Automatic OSC 133 shell integration script injection
│   └── tests/             # End-to-end live transport & vault test suites
├── src-tauri/             # Tauri v2 desktop application harness
│   ├── src/
│   │   ├── lib.rs         # Tauri IPC command definitions & state management
│   │   └── main.rs        # Application entrypoint
│   └── tauri.conf.json    # Window geometry, capabilities, and bundle configuration
├── src/                   # React 19 + TypeScript frontend
│   ├── components/        # TerminalPane, RdpPane, FileDrawer, Sidebar, etc.
│   ├── lib/               # Tauri IPC API bindings, clipboard, and scancode translators
│   ├── App.tsx            # Multi-tab container & keyboard event routing
│   └── main.tsx           # React DOM root
├── scripts/               # Cross-platform automation scripts (Bash & PowerShell)
└── docs/                  # Release engineering & signing documentation
```

### The Stream Tap Architecture

```
                  ┌──────────────────────────────────────────────┐
                  │          Transport Stream (PTY/SSH)          │
                  └──────────────────────┬───────────────────────┘
                                         │
                                   Raw Byte Flow
                                         │
                                         ▼
                     ┌────────────────────────────────────────┐
                     │            core::tap::Tap              │
                     └─┬─────────────────┬──────────────────┬─┘
                       │                 │                  │
                       ▼                 ▼                  ▼
              ┌─────────────────┐ ┌───────────────┐ ┌───────────────┐
              │  xterm.js / UI  │ │  Plain Text   │ │ asciinema v2  │
              │ WebGL Viewport  │ │ Log Generator │ │ .cast Storage │
              └─────────────────┘ └───────────────┘ └───────────────┘
```

---

## Configuration & Environment Variables

| Variable / Flag | Scope | Description |
|---|---|---|
| `--vault` | `./scripts/dev.sh` | Forces use of the encrypted software vault instead of the OS keychain (avoids macOS dev prompts). |
| `--trace` | `./scripts/dev.sh` | Enables verbose `tracing` debug logs across all Rust subsystems. |
| `--no-bundle` | `./scripts/build.sh` | Builds release binaries directly in `target/release/` without generating OS installers. |
| `--target <triple>` | `./scripts/build.sh` | Cross-compiles for a specific Rust target triple. |
| `TERMINATOR_SSH_TEST` | `cargo test` | Opts into live SSH integration test execution. |
| `TERMINATOR_RDP_TEST` | `cargo test` | Host/port (`host:3389`) for live RDP integration testing. |
| `TERMINATOR_RDP_USER` | `cargo test` | Username for live RDP authentication. |
| `TERMINATOR_RDP_PASS` | `cargo test` | Password for live RDP authentication. |
| `TERMINATOR_RDP_DOMAIN`| `cargo test` | Active Directory domain for live RDP authentication. |

---

## Contributing

Contributions are welcome! Please review [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a pull request.

Always ensure tests pass and code is formatted cleanly:
```bash
./scripts/test.sh
```

---

## License

This project is licensed under the [MIT License](LICENSE).

