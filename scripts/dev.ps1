# Runs Terminator in development mode: Vite dev server + hot reload.
#
#   .\scripts\dev.ps1                normal run
#   .\scripts\dev.ps1 -Vault         force the encrypted-vault secret backend
#   .\scripts\dev.ps1 -Trace         verbose logging

[CmdletBinding()]
param(
    [switch]$Vault,
    [switch]$Trace,
    [Parameter(ValueFromRemainingArguments)] [string[]]$Rest = @()
)

. (Join-Path $PSScriptRoot 'lib.ps1')

if ($Vault) {
    # Forces the encrypted vault instead of the Windows Credential Manager.
    # Handy when testing the fallback path other platforms may hit.
    $env:TERMINATOR_FORCE_FILE_SECRETS = '1'
    Write-Info 'using the encrypted vault instead of the OS credential store'
}

$level = if ($Trace) { 'terminator=trace,frontend=trace' } else { 'terminator=info,frontend=info' }

Assert-Toolchain
Install-NodeModules

Set-Location $script:RepoRoot
if (-not $env:RUST_LOG)       { $env:RUST_LOG = $level }
# A panic inside a Tauri command otherwise unwinds into an opaque IPC error.
if (-not $env:RUST_BACKTRACE) { $env:RUST_BACKTRACE = '1' }

Write-Info 'starting dev build (first compile takes a few minutes)'
Invoke-Checked npm (@('run', 'tauri', 'dev', '--') + $Rest)
