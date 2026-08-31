# Shared helpers for the Windows scripts. Dot-sourced, never run directly.

$ErrorActionPreference = 'Stop'

$script:RepoRoot = Split-Path -Parent $PSScriptRoot

function Write-Info { param([string]$Message) Write-Host "==> $Message" -ForegroundColor Blue }
function Write-Ok   { param([string]$Message) Write-Host "  ok $Message" -ForegroundColor Green }
function Write-Warn { param([string]$Message) Write-Host "warn $Message" -ForegroundColor Yellow }
function Write-Die  {
    param([string]$Message)
    Write-Host "error $Message" -ForegroundColor Red
    exit 1
}

function Test-Have { param([string]$Name) [bool](Get-Command $Name -ErrorAction SilentlyContinue) }

# Installers (rustup, winget, nvm) update the machine/user PATH but not the
# already-running shell. Re-read it so a fresh install is visible immediately.
function Update-PathFromRegistry {
    $sep = [IO.Path]::PathSeparator
    # Use the .NET accessors rather than $env:Path: PowerShell environment
    # variables are case-sensitive on Unix, so $env:Path is null there while
    # $env:PATH is the real one. Going through [Environment] works on every
    # platform, which also makes this function testable off Windows.
    $current = [Environment]::GetEnvironmentVariable('PATH')

    $parts = @()
    # Keep the current process PATH first: a caller (or a CI step) may have
    # added entries present in neither the Machine nor the User scope.
    $parts += $current
    $parts += [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $parts += [Environment]::GetEnvironmentVariable('Path', 'User')

    # $HOME is set on every platform; USERPROFILE only on Windows.
    $home_ = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    if ($home_) { $parts += (Join-Path $home_ '.cargo/bin') }

    $seen = [System.Collections.Generic.HashSet[string]]::new()
    $clean = @($parts |
        Where-Object { $_ } |
        ForEach-Object { $_ -split [regex]::Escape($sep) } |
        Where-Object { $_ -and $seen.Add($_) })

    if ($clean.Count -gt 0) {
        [Environment]::SetEnvironmentVariable('PATH', ($clean -join $sep))
    }
}

function Assert-Toolchain {
    Update-PathFromRegistry
    $missing = @()
    if (-not (Test-Have cargo)) { $missing += 'Rust (https://rustup.rs)' }
    if (-not (Test-Have node))  { $missing += 'Node.js 20+ (https://nodejs.org)' }
    if (-not (Test-Have npm))   { $missing += 'npm (ships with Node.js)' }
    if ($missing.Count -gt 0) {
        $missing | ForEach-Object { Write-Warn "missing: $_" }
        Write-Die 'missing prerequisites; run scripts\setup.ps1 first'
    }

    # Vite 7 and the Tauri CLI both need a modern Node. Fail clearly here rather
    # than with a cryptic syntax error deep inside a build.
    $major = [int](& node -p 'process.versions.node.split(".")[0]')
    if ($major -lt 20) { Write-Die "Node.js 20+ required, found $(& node -v)" }
}

function Install-NodeModules {
    Set-Location $script:RepoRoot
    $stamp = Join-Path $script:RepoRoot 'node_modules\.install-stamp'
    $lock  = Join-Path $script:RepoRoot 'package-lock.json'
    $stale = -not (Test-Path $stamp) -or
             ((Test-Path $lock) -and (Get-Item $lock).LastWriteTime -gt (Get-Item $stamp).LastWriteTime)

    if ($stale) {
        Write-Info 'installing npm dependencies'
        & npm ci
        if ($LASTEXITCODE -ne 0) {
            & npm install
            if ($LASTEXITCODE -ne 0) { Write-Die 'npm install failed' }
        }
        New-Item -ItemType File -Path $stamp -Force | Out-Null
    }
}

function Invoke-Checked {
    param([Parameter(Mandatory)][string]$Exe, [string[]]$Arguments = @())
    & $Exe @Arguments
    # PowerShell does not stop on a non-zero exit from a native process, so a
    # failed compile would otherwise look like a successful build.
    if ($LASTEXITCODE -ne 0) { Write-Die "$Exe exited with code $LASTEXITCODE" }
}
