<#
.SYNOPSIS
    Builds LightVM with the MSVC environment loaded.

.DESCRIPTION
    Loads the Visual Studio Build Tools environment (vcvars64.bat, found with
    vswhere) into this process and runs cargo with live output:

      default      cargo build --workspace --locked (default members)
      -Release     cargo build --release --locked of the shipped binaries
                   (lightvm-desktop, lightvm-service, lightvm-mcp)
      -DevTools    adds the lightvm-desktop `dev-tools` feature (--smoke,
                   --static, --simulated-console, --no-supervisor)
      -Run         starts lightvm-desktop.exe after a successful build;
                   arguments after -RunArgs are passed to it
      -Package     delegates to packaging/windows/build-package.ps1
                   (zip, SBOM, artefact check, setup.exe if Inno Setup is
                   installed); the other switches are ignored

    Exit code: the cargo (or packaging) exit code.

.EXAMPLE
    .\scripts\build.ps1
.EXAMPLE
    .\scripts\build.ps1 -Release -Run
.EXAMPLE
    .\scripts\build.ps1 -DevTools -Run -RunArgs '--simulated-console'
.EXAMPLE
    .\scripts\build.ps1 -Package
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$DevTools,
    [switch]$Run,
    [string[]]$RunArgs,
    [switch]$Package,
    [string]$TargetDir
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if ($Package) {
    $pkgArgs = @{}
    if ($TargetDir) { $pkgArgs.TargetDir = $TargetDir }
    & (Join-Path $Repo 'packaging\windows\build-package.ps1') @pkgArgs
    exit $LASTEXITCODE
}

# MSVC environment (same lookup as scripts/regression.ps1), skipped when the
# shell already has it (e.g. a Developer PowerShell).
if (-not $env:VCToolsInstallDir) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vcvars = $null
    if (Test-Path $vswhere) {
        $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) { $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat' }
    }
    if (-not $vcvars -or -not (Test-Path $vcvars)) {
        throw 'MSVC build tools not found (vswhere/vcvars64.bat).'
    }
    Write-Host "==> MSVC environment: $vcvars"
    & cmd.exe /d /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process') }
    }
    if (-not $env:VCToolsInstallDir) { throw "vcvars64.bat did not load the MSVC environment." }
}
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) { $env:PATH = "$cargoBin;$env:PATH" }
if ($TargetDir) { $env:CARGO_TARGET_DIR = $TargetDir }

$cargoArgs = @('build', '--locked')
if ($Release) {
    $cargoArgs += @('--release', '-p', 'lightvm-desktop', '-p', 'lightvm-service', '-p', 'lightvm-mcp')
} else {
    $cargoArgs += '--workspace'
}
if ($DevTools) { $cargoArgs += @('--features', 'lightvm-desktop/dev-tools') }

Write-Host "==> cargo $($cargoArgs -join ' ')"
Push-Location $Repo
try {
    & cargo @cargoArgs
    $code = $LASTEXITCODE
} finally { Pop-Location }
if ($code -ne 0) {
    Write-Host "==> build FAILED (exit $code)" -ForegroundColor Red
    exit $code
}

$outDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Repo 'target' }
$outDir = Join-Path $outDir $(if ($Release) { 'release' } else { 'debug' })
Write-Host "==> build OK: $outDir" -ForegroundColor Green

if ($Run) {
    $exe = Join-Path $outDir 'lightvm-desktop.exe'
    Write-Host "==> $exe $($RunArgs -join ' ')"
    & $exe @RunArgs
    exit $LASTEXITCODE
}
exit 0
