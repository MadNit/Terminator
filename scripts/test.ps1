# Runs the full check suite: Rust tests, clippy, formatting and TypeScript.
#
#   .\scripts\test.ps1              everything
#   .\scripts\test.ps1 -Fast        tests only, skip lint/format
#   .\scripts\test.ps1 -Ssh         also run the live SSH tests

[CmdletBinding()]
param([switch]$Fast, [switch]$Ssh)

. (Join-Path $PSScriptRoot 'lib.ps1')

# The live SSH tests need a real host, so they stay opt-in; otherwise they
# would fail on every contributor's machine.
if ($Ssh -and -not $env:TERMINATOR_SSH_TEST) { $env:TERMINATOR_SSH_TEST = '1' }

Assert-Toolchain
Install-NodeModules
Set-Location $script:RepoRoot

Write-Info 'cargo test'
Invoke-Checked cargo @('test', '--workspace')
Write-Ok 'Rust tests'

Write-Info 'tsc --noEmit'
Invoke-Checked npx @('tsc', '--noEmit')
Write-Ok 'TypeScript'

if (-not $Fast) {
    & cargo fmt --version *> $null
    if ($LASTEXITCODE -eq 0) {
        Write-Info 'cargo fmt --check'
        & cargo fmt --all -- --check
        if ($LASTEXITCODE -ne 0) { Write-Die 'formatting issues; run: cargo fmt --all' }
        Write-Ok 'formatting'
    } else {
        Write-Warn 'rustfmt not installed; skipping (rustup component add rustfmt)'
    }

    & cargo clippy --version *> $null
    if ($LASTEXITCODE -eq 0) {
        Write-Info 'cargo clippy'
        Invoke-Checked cargo @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings')
        Write-Ok 'clippy'
    } else {
        Write-Warn 'clippy not installed; skipping (rustup component add clippy)'
    }
}

Write-Host ''
Write-Ok 'all checks passed'
