<#
.SYNOPSIS
    Checks the LightVM release artefacts for test hooks, debug settings,
    build-host paths and credentials (ROADMAP 10 F6).

.DESCRIPTION
    "Verificare che l'artefatto pubblico non includa endpoint di test, driver
    di automazione UI incorporato, credenziali o impostazioni di debug."

    Scans every .exe (and .dll) under -Path, or the files given, as bytes
    (UTF-8/ASCII and UTF-16LE at both alignments) for forbidden strings and
    patterns, and the folder for files that must not be shipped. Each
    finding names the file, the rule and the reason. Exit code 0 = clean,
    1 = findings, 2 = wrong usage.

    Rules (see docs/verification/F6-01-stabilization.md):
    - test-only environment hooks (LIGHTVM_UPDATE_SCHEMAS, LIGHTVM_E2E_*,
      LIGHTVM_F5_*, lab paths and variables of the real-QEMU tests);
    - UI automation driver (egui_kittest) and the service test fakes;
    - debug/testing command-line switches of the shipped programs;
    - debug build markers (overflow-check panic messages);
    - build-host paths (user profile, repository, cargo home), which a
      packaged build removes with --remap-path-prefix;
    - private keys and common API-token shapes; the guest credential file
      name of the lab;
    - test tools and debug files next to the binaries.

.PARAMETER Path
    Folders and/or files to scan. Default: dist\staging (package staging).

.PARAMETER AllowHostPaths
    Reports build-host paths as warnings instead of findings (a plain
    `cargo build --release` without the packaging flags keeps them).
#>
[CmdletBinding()]
param(
    [string[]]$Path,
    [switch]$AllowHostPaths
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Path) { $Path = @(Join-Path $Repo 'dist\staging') }
# `powershell -File` passes "a,b" as one string: split it when it is not a path.
$Path = @($Path | ForEach-Object { if (Test-Path -LiteralPath $_) { $_ } else { $_ -split ',' } })

# --- Rules -------------------------------------------------------------------
# Literal strings: name => reason.
$Literals = [ordered]@{
    'LIGHTVM_UPDATE_SCHEMAS'        = 'test hook: rewrites the schema snapshots'
    'LIGHTVM_E2E_'                  = 'test hook: real-QEMU end-to-end test variables'
    'LIGHTVM_F5_'                   = 'test hook: F5 real-client harness variables'
    'LIGHTVM_LAB_DIR'               = 'test hook: lab directory'
    'LIGHTVM_QEMU_DIR'              = 'test hook: lab QEMU directory variable'
    'LIGHTVM_SERVICE_EXE'           = 'test hook: service executable override of the tests'
    # Crate name (it would appear in source paths of panics). A bare
    # 'kittest' is not a rule: egui-wgpu's shader text mentions it.
    'egui_kittest'                  = 'UI automation driver embedded'
    'lightvm-service-tests'         = 'test support code (temporary catalogs of the tests)'
    'fake convert'                  = 'service test fakes (fake image tool)'
    'fake overlay'                  = 'service test fakes (fake image tool)'
    'lightvm-no-display.sock'       = 'supervisor unit-test fixture'
    '--simulated-console'           = 'debug switch: simulated console data in the product (ROADMAP F2: no simulated data)'
    '--smoke'                       = 'debug switch: smoke-test mode'
    '--no-supervisor'               = 'debug switch: UI without the supervisor'
    'attempt to add with overflow'  = 'debug build (overflow checks enabled)'
    'attempt to subtract with overflow' = 'debug build (overflow checks enabled)'
    'credenziali-guest'             = 'lab guest credential file name'
    'BEGIN RSA PRIVATE KEY'         = 'private key material'
    'BEGIN PRIVATE KEY'             = 'private key material'
    'BEGIN OPENSSH PRIVATE KEY'     = 'private key material'
    'BEGIN EC PRIVATE KEY'          = 'private key material'
}
# Regular expressions (ASCII view): name => reason.
$Patterns = [ordered]@{
    'sk-ant-[A-Za-z0-9_\-]{20,}'    = 'Anthropic API key shape'
    'ghp_[A-Za-z0-9]{36}'           = 'GitHub token shape'
    'AKIA[0-9A-Z]{16}'              = 'AWS access key shape'
    'xox[baprs]-[A-Za-z0-9\-]{10,}' = 'Slack token shape'
}
# Build-host paths.
$HostPaths = @()
if ($env:USERPROFILE) { $HostPaths += $env:USERPROFILE }
$HostPaths += $Repo
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$HostPaths += $CargoHome
# Files that must not be shipped next to the binaries.
$ForbiddenFiles = [ordered]@{
    'lightvm-ipc-probe.exe'  = 'IPC test client (not a product artifact)'
    'lightvm-peer-check.exe' = 'peer-check test tool'
    'lightvm-sbom.exe'       = 'release tool'
    'f0-*.exe'               = 'F0 feasibility spike'
    '*.pdb'                  = 'debug symbols (published separately if at all)'
}

# --- Scan ----------------------------------------------------------------------
$latin1 = [System.Text.Encoding]::GetEncoding(28591)
$utf16 = [System.Text.Encoding]::Unicode
$findings = New-Object System.Collections.Generic.List[string]
$warnings = New-Object System.Collections.Generic.List[string]
$scanned = 0

$files = @()
foreach ($p in $Path) {
    if (-not (Test-Path -LiteralPath $p)) { Write-Error "not found: $p"; exit 2 }
    $item = Get-Item -LiteralPath $p
    if ($item.PSIsContainer) {
        foreach ($rule in $ForbiddenFiles.Keys) {
            foreach ($f in @(Get-ChildItem -LiteralPath $item.FullName -Recurse -File -Filter $rule -ErrorAction SilentlyContinue)) {
                $findings.Add("$($f.FullName): file must not be shipped ($($ForbiddenFiles[$rule]))")
            }
        }
        $files += @(Get-ChildItem -LiteralPath $item.FullName -Recurse -File | Where-Object { $_.Extension -in '.exe', '.dll' })
    } else {
        $files += $item
    }
}
if ($files.Count -eq 0) { Write-Error "no executable found under $($Path -join ', ')"; exit 2 }

foreach ($f in $files) {
    $bytes = [System.IO.File]::ReadAllBytes($f.FullName)
    $scanned++
    $views = @(
        $latin1.GetString($bytes),
        $utf16.GetString($bytes, 0, $bytes.Length - ($bytes.Length % 2)),
        $utf16.GetString($bytes, 1, ($bytes.Length - 1) - (($bytes.Length - 1) % 2))
    )
    foreach ($k in $Literals.Keys) {
        foreach ($v in $views) {
            if ($v.IndexOf($k, [System.StringComparison]::Ordinal) -ge 0) {
                $findings.Add("$($f.Name): '$k' ($($Literals[$k]))")
                break
            }
        }
    }
    foreach ($k in $Patterns.Keys) {
        $m = [regex]::Match($views[0], $k)
        if ($m.Success) { $findings.Add("$($f.Name): pattern '$k' ($($Patterns[$k]))") }
    }
    foreach ($h in $HostPaths) {
        foreach ($form in @($h, $h.Replace('\', '/'), $h.Replace('\', '\\'))) {
            $hit = $false
            foreach ($v in $views) {
                if ($v.IndexOf($form, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) { $hit = $true; break }
            }
            if ($hit) {
                $msg = "$($f.Name): build-host path '$form' embedded (use the packaging --remap-path-prefix flags)"
                if ($AllowHostPaths) { $warnings.Add($msg) } else { $findings.Add($msg) }
                break
            }
        }
    }
}

Write-Host "Scanned $scanned files under $($Path -join ', ')"
foreach ($w in $warnings) { Write-Host "WARNING  $w" }
foreach ($x in $findings) { Write-Host "FINDING  $x" }
if ($findings.Count -gt 0) {
    Write-Host "$($findings.Count) findings"
    exit 1
}
Write-Host 'No findings'
exit 0
