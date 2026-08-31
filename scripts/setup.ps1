# Installs everything needed to build Terminator on Windows.
#
#   powershell -ExecutionPolicy Bypass -File scripts\setup.ps1
#
# Safe to re-run: every step checks before it installs.

. (Join-Path $PSScriptRoot 'lib.ps1')

Write-Info 'setting up Terminator build environment (windows)'

$useWinget = Test-Have winget
if (-not $useWinget) {
    Write-Warn 'winget not found -- install the missing tools manually when prompted'
}

# --------------------------------------------------------------- MSVC toolchain

# Rust's default Windows toolchain (`stable-msvc`) links with the MSVC linker,
# which only ships with the Visual Studio Build Tools. Without it every build
# fails at the link step with "link.exe not found".
$hasMsvc = (Test-Path 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe') -or
           (Test-Path 'C:\Program Files\Microsoft Visual Studio')
if (-not $hasMsvc) {
    if ($useWinget) {
        Write-Info 'installing Visual Studio Build Tools (C++ workload, several GB)'
        Invoke-Checked winget @(
            'install', '--id', 'Microsoft.VisualStudio.2022.BuildTools',
            '-e', '--accept-package-agreements', '--accept-source-agreements',
            '--override', '--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
        )
    } else {
        Write-Die 'Install "Visual Studio Build Tools" with the "Desktop development with C++" workload: https://visualstudio.microsoft.com/downloads/'
    }
}
Write-Ok 'MSVC build tools'

# -------------------------------------------------------------------- WebView2

# Tauri renders in WebView2. Windows 11 and recent Windows 10 ship it, but a
# clean Windows 10 image may not have it.
$wv2Keys = @(
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
if (-not ($wv2Keys | Where-Object { Test-Path $_ })) {
    if ($useWinget) {
        Write-Info 'installing WebView2 runtime'
        & winget install --id Microsoft.EdgeWebView2Runtime -e `
            --accept-package-agreements --accept-source-agreements
    } else {
        Write-Warn 'WebView2 runtime not detected: https://developer.microsoft.com/microsoft-edge/webview2/'
    }
}
Write-Ok 'WebView2 runtime'

# ------------------------------------------------------------------------ Rust

Update-PathFromRegistry
if (-not (Test-Have cargo)) {
    Write-Info 'installing Rust via rustup'
    $exe = Join-Path $env:TEMP 'rustup-init.exe'
    Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $exe
    Invoke-Checked $exe @('-y', '--default-toolchain', 'stable')
    Update-PathFromRegistry
}
if (-not (Test-Have cargo)) { Write-Die 'Rust install failed; see https://rustup.rs' }
Write-Ok "Rust $((& rustc --version).Split(' ')[1])"

# ------------------------------------------------------------------------ Node

if (-not (Test-Have node)) {
    if ($useWinget) {
        Write-Info 'installing Node.js LTS'
        Invoke-Checked winget @(
            'install', '--id', 'OpenJS.NodeJS.LTS', '-e',
            '--accept-package-agreements', '--accept-source-agreements'
        )
        Update-PathFromRegistry
    } else {
        Write-Die 'Node.js 20+ required -- https://nodejs.org'
    }
}
if (-not (Test-Have node)) {
    Write-Die 'Node.js installed but not on PATH -- open a new terminal and re-run'
}
Write-Ok "Node.js $(& node -v)"

# ------------------------------------------------------------------ JavaScript

Install-NodeModules
Write-Ok 'npm dependencies'

# ------------------------------------------------------------- warm the build

Write-Info 'pre-fetching Rust dependencies (this is the slow part, once)'
Set-Location $script:RepoRoot
& cargo fetch --locked
if ($LASTEXITCODE -ne 0) { Invoke-Checked cargo @('fetch') }

Write-Host ''
Write-Ok 'setup complete'
Write-Host @'

  Next:
    .\scripts\dev.ps1      run the app with hot reload
    .\scripts\test.ps1     run the test suite
    .\scripts\build.ps1    produce a release bundle

'@
