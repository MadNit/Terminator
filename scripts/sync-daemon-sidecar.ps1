# Builds the daemon as a separate binary and copies it into
# `src-tauri/binaries/` with the target-triple suffix Tauri
# requires for `externalBin`. Run automatically by Tauri's
# `beforeBuildCommand` before the bundle is assembled.
#
# Without this, the bundled installer ships only
# `terminator.exe` and the first thing the main exe does on
# launch -- `daemon_client::spawn_or_connect` -- fails to
# find the daemon binary and refuses to start.

$ErrorActionPreference = 'Stop'

$targetTriple = 'x86_64-pc-windows-msvc'
$sidecarDir = Join-Path $PSScriptRoot '..\src-tauri\binaries'
$sidecarExe = Join-Path $sidecarDir "terminator-daemon-$targetTriple.exe"

New-Item -ItemType Directory -Path $sidecarDir -Force | Out-Null

Write-Host "Building terminator-daemon (release)..."
cargo build -p terminator-daemon --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$sourceExe = Join-Path $PSScriptRoot '..\target\release\terminator-daemon.exe'
if (-not (Test-Path $sourceExe)) {
    throw "expected $sourceExe but it was not produced"
}

# `Copy-Item -Force` is fine here: both files are owned by the
# build, and the Tauri sidecar convention is that the source
# carries the target triple in its filename.
Copy-Item $sourceExe $sidecarExe -Force
Write-Host "Copied -> $sidecarExe"
