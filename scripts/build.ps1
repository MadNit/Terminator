# Builds a release bundle (.msi and .exe installers).
#
#   .\scripts\build.ps1                  bundle for this machine
#   .\scripts\build.ps1 -NoBundle        binary only, skip installers
#
# Artifacts land in target\release\bundle\.

[CmdletBinding()]
param(
    [switch]$NoBundle,
    [Parameter(ValueFromRemainingArguments)] [string[]]$Rest = @()
)

. (Join-Path $PSScriptRoot 'lib.ps1')

Assert-Toolchain
Install-NodeModules
Set-Location $script:RepoRoot

Write-Info 'type-checking the frontend'
Invoke-Checked npx @('tsc', '--noEmit')
Write-Ok 'types clean'

$tauriArgs = @('run', 'tauri', 'build', '--')
if ($NoBundle) { $tauriArgs += '--no-bundle' }
$tauriArgs += $Rest

Write-Info 'building release bundle (this takes several minutes)'
Invoke-Checked npm $tauriArgs

Write-Host ''
Write-Ok 'build complete'

$bundleDir = Join-Path $script:RepoRoot 'target\release\bundle'
if (Test-Path $bundleDir) {
    Write-Info 'artifacts:'
    Get-ChildItem -Path $bundleDir -Recurse -Include '*.msi', '*.exe' |
        ForEach-Object { Write-Host "   $($_.FullName)" }
}

Write-Host @'

  Note: this build is unsigned. Windows SmartScreen will warn users with
  "Windows protected your PC" until the binary earns reputation or you sign
  it with a code-signing certificate. See docs/RELEASING.md.
'@
