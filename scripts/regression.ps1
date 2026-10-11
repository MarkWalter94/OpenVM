<#
.SYNOPSIS
    Runs the blocking LightVM suites and summarises the results (F6).

.DESCRIPTION
    Steps (each with its own log in -OutDir):
      fmt              cargo fmt --all --check
      clippy           cargo clippy --workspace --all-targets --locked -D warnings
      clippy-ui-glow   lightvm-desktop with the glow backend only
      clippy-ui-wgpu   lightvm-desktop with the wgpu backend only
      clippy-client    lightvm-service --no-default-features (UI/adapter client side)
      test             cargo test --workspace --locked --no-fail-fast
                       (includes the schema snapshot, the headless UI tests,
                       the protocol tests over stdio, F2-F6 service tests)
      schema           lightvm-proto schema snapshot (explicit, quick)
      licences         cargo deny --locked check when cargo-deny is installed;
                       otherwise the offline check of lightvm-sbom against the
                       deny.toml allow list (declared SPDX expressions of the
                       shipped closure for each declared target)
      sbom             CycloneDX SBOM of the shipped binaries (lightvm-sbom)
      release-check    (-Release) release build of the shipped binaries and
                       scripts/check-release-artifacts.ps1 on them

    The real-QEMU and real-client tests (#[ignore]) are not blocking suites
    here: they need the lab (QEMU, ISO) and are run by hand (docs/TESTING.md).

    Exit code: 0 if every executed step passed, 1 otherwise.

.PARAMETER OutDir
    Logs and summary. Default: target\regression\<UTC timestamp>.

.PARAMETER TargetDir
    CARGO_TARGET_DIR for the run. Default: target\regression-build.

.PARAMETER Steps
    Subset of steps to run (names above). Default: all but release-check.

.PARAMETER Release
    Adds the release-check step.

.PARAMETER NoDebugInfo
    Builds the dev/test profile without debug information
    (CARGO_PROFILE_DEV_DEBUG=0): about a third of the disk space, same
    checks; use it on hosts with little free space.
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    [string]$TargetDir,
    [string[]]$Steps,
    [switch]$Release,
    [switch]$NoDebugInfo
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stamp = [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ')
if (-not $OutDir) { $OutDir = Join-Path $Repo "target\regression\$stamp" }
if (-not $TargetDir) { $TargetDir = Join-Path $Repo 'target\regression-build' }
New-Item -ItemType Directory -Force $OutDir | Out-Null

# MSVC environment (same approach as packaging/windows/build-package.ps1).
$vcvars = $null
if ($env:OS -eq 'Windows_NT') {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) { $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat' }
    }
    if (-not $vcvars -or -not (Test-Path $vcvars)) {
        throw 'MSVC build tools not found (vswhere/vcvars64.bat).'
    }
}
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

function Invoke-Logged {
    param([string]$Name, [string]$CommandLine, [string]$Log)
    $bat = Join-Path ([System.IO.Path]::GetTempPath()) ("lightvm-reg-" + [guid]::NewGuid().ToString('N') + '.cmd')
    $lines = @('@echo off')
    if ($vcvars) { $lines += "call `"$vcvars`" >nul 2>&1" }
    $lines += @(
        "set `"PATH=$cargoBin;%PATH%`"",
        "set `"CARGO_TARGET_DIR=$TargetDir`"",
        'set "CARGO_INCREMENTAL=0"',
        $(if ($NoDebugInfo) { 'set "CARGO_PROFILE_DEV_DEBUG=0"' } else { 'rem debug info: profile default' }),
        "cd /d `"$Repo`"",
        "($CommandLine) > `"$Log`" 2>&1"
    )
    [System.IO.File]::WriteAllText($bat, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
    try {
        & cmd.exe /d /c $bat | Out-Null
        return $LASTEXITCODE
    } finally { Remove-Item -Force -ErrorAction SilentlyContinue $bat }
}

$haveDeny = $false
$probe = Join-Path $OutDir 'cargo-deny-version.log'
if ((Invoke-Logged 'probe' 'cargo deny --version' $probe) -eq 0) { $haveDeny = $true }

$sbomOut = Join-Path $OutDir 'lightvm.cdx.json'
$all = [ordered]@{
    'fmt'            = 'cargo fmt --all --check'
    'clippy'         = 'cargo clippy --workspace --all-targets --locked -- -D warnings'
    'clippy-ui-glow' = 'cargo clippy -p lightvm-desktop --no-default-features --features glow --all-targets --locked -- -D warnings'
    'clippy-ui-wgpu' = 'cargo clippy -p lightvm-desktop --no-default-features --features wgpu --all-targets --locked -- -D warnings'
    'clippy-client'  = 'cargo clippy -p lightvm-service --no-default-features --all-targets --locked -- -D warnings'
    'test'           = 'cargo test --workspace --locked --no-fail-fast'
    'schema'         = 'cargo test --locked -p lightvm-proto --test schema_snapshot'
    'licences'       = if ($haveDeny) { 'cargo deny --locked check' } else {
        # Offline fallback: the declared licences of the shipped closure for
        # every target of deny.toml.
        (@('x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-apple-darwin') | ForEach-Object {
            "cargo run --locked --offline -q -p lightvm-sbom -- --workspace . --target $_ --out `"$OutDir\licences-$_.cdx.json`" --check-licences deny.toml"
        }) -join ' && '
    }
    'sbom'           = "cargo run --locked --offline -q -p lightvm-sbom -- --workspace . --out `"$sbomOut`" --notices `"$OutDir\THIRD-PARTY-NOTICES.txt`""
    'release-check'  = "cargo build --release --locked -p lightvm-desktop -p lightvm-service -p lightvm-mcp --bin lightvm-desktop --bin lightvm-service --bin lightvm-mcp && powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSScriptRoot\check-release-artifacts.ps1`" -Path `"$TargetDir\release\lightvm-desktop.exe`",`"$TargetDir\release\lightvm-service.exe`",`"$TargetDir\release\lightvm-mcp.exe`" -AllowHostPaths"
}
# `powershell -File` passes "a,b" as one string.
if ($Steps) { $Steps = @($Steps | ForEach-Object { $_ -split "," } | Where-Object { $_ }) }
if (-not $Steps) { $Steps = @($all.Keys | Where-Object { $_ -ne 'release-check' }) }
if ($Release -and $Steps -notcontains 'release-check') { $Steps += 'release-check' }
foreach ($s in $Steps) { if (-not $all.Contains($s)) { throw "unknown step $s (known: $($all.Keys -join ', '))" } }

$results = @()
foreach ($s in $Steps) {
    $log = Join-Path $OutDir "$s.log"
    Write-Host "==> $s"
    $t0 = [DateTime]::UtcNow
    $code = Invoke-Logged $s $all[$s] $log
    $secs = [int]([DateTime]::UtcNow - $t0).TotalSeconds
    $detail = ''
    $text = if (Test-Path $log) { Get-Content -Raw -LiteralPath $log } else { '' }
    if ($s -in 'test', 'schema') {
        $p = 0; $f = 0; $i = 0
        foreach ($m in [regex]::Matches($text, 'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored')) {
            $p += [int]$m.Groups[1].Value; $f += [int]$m.Groups[2].Value; $i += [int]$m.Groups[3].Value
        }
        $detail = "$p passed, $f failed, $i ignored"
        $failedTests = @([regex]::Matches($text, '(?m)^test (\S+) \.\.\. FAILED') | ForEach-Object { $_.Groups[1].Value })
        if ($failedTests.Count -gt 0) { $detail += '; failed: ' + ($failedTests -join ', ') }
    } elseif ($s -like 'clippy*') {
        $w = ([regex]::Matches($text, '(?m)^(warning|error)(\[\S+\])?: ')).Count
        $detail = "$w diagnostics"
    } elseif ($s -eq 'licences') {
        $detail = if ($haveDeny) { 'cargo-deny' } else { 'offline lightvm-sbom check (cargo-deny not installed)' }
    } elseif ($s -eq 'release-check') {
        $m = [regex]::Match($text, '(\d+) findings|No findings')
        $detail = if ($m.Success) { $m.Value } else { '' }
    }
    $results += [pscustomobject]@{
        Step = $s
        Result = if ($code -eq 0) { 'PASS' } else { "FAIL ($code)" }
        Seconds = $secs
        Detail = $detail
        Log = $log
    }
    Write-Host ("    {0} in {1} s {2}" -f $results[-1].Result, $secs, $detail)
}

$summary = Join-Path $OutDir 'summary.txt'
$lines = @(
    "LightVM regression $stamp",
    "Commit: $((& git -C $Repo rev-parse HEAD).Trim())$(if (& git -C $Repo status --porcelain --untracked-files=no) { ' (uncommitted changes)' })",
    "Target dir: $TargetDir",
    ''
)
$lines += ($results | Format-Table Step, Result, Seconds, Detail -AutoSize | Out-String -Width 200).TrimEnd()
[System.IO.File]::WriteAllText($summary, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host ''
$lines | ForEach-Object { Write-Host $_ }
Write-Host "Logs: $OutDir"
if ($results | Where-Object { $_.Result -ne 'PASS' }) { exit 1 }
exit 0
