<#
.SYNOPSIS
    Builds the experimental per-user Windows package of LightVM (F2).

.DESCRIPTION
    1. Builds lightvm-desktop.exe, lightvm-service.exe and lightvm-mcp.exe
       (MCP adapter, F5) in release mode with the MSVC toolchain pinned in
       rust-toolchain.toml (`--locked`), with the C runtime linked statically
       (`+crt-static`) so that the package does not depend on the Visual C++
       Redistributable on a clean machine. The desktop is built with its
       default features only: the `dev-tools` test/measurement switches
       (`--smoke`, `--simulated-console`, `--static`, `--no-supervisor`) are
       never in the package.
    2. Generates THIRD-PARTY-LICENSES.txt from the lockfile (crates linked into
       the three binaries for x86_64-pc-windows-msvc, licence texts copied from
       the crate sources in the cargo registry).
    3. Stages the binaries, README, USER_GUIDE, LICENSE, install.ps1 and
       uninstall.ps1; generates SBOM.cdx.json (CycloneDX 1.5, offline, with
       crates/lightvm-sbom; SOURCE_DATE_EPOCH = commit time) and checks the
       licence policy of deny.toml; runs scripts/check-release-artifacts.ps1
       on the staging folder and stops on any finding (test hooks, debug
       switches, host paths, credentials, files not to ship); writes
       SHA256SUMS.txt and BUILDINFO.txt.
    4. Produces dist\LightVM-<version>-windows-x64.zip and its .sha256 file.
    5. Optionally (if Inno Setup 6 is installed and -NoSetupExe is not given)
       compiles a per-user setup.exe from lightvm.iss, with its .sha256 file.
    6. Writes dist\LightVM-sandbox.wsb and dist\sandbox-test.ps1 for the
       clean-machine test in Windows Sandbox (procedure in USER_GUIDE.md and
       docs/verification/F2-03-packaging-guide-ci.md).

    The package is NOT code-signed (ADR-0020: no signing credentials).
    QEMU is NOT bundled: the user installs it separately (ADR-0010).

.PARAMETER Version
    Version in the file names. Default: version of the lightvm-desktop crate.

.PARAMETER OutDir
    Output folder. Default: <repo>\dist (git-ignored).

.PARAMETER TargetDir
    Cargo target directory. Default: <repo>\target\packaging.

.PARAMETER SkipBuild
    Reuses the binaries already in <TargetDir>\release.

.PARAMETER NoSetupExe
    Does not compile the Inno Setup installer even if ISCC.exe is available.
#>
[CmdletBinding()]
param(
    [string]$Version,
    [string]$OutDir,
    [string]$TargetDir,
    [string]$ToolsBundle,
    [switch]$SkipBuild,
    [switch]$NoSetupExe
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $OutDir) { $OutDir = Join-Path $Repo 'dist' }
if (-not $TargetDir) { $TargetDir = Join-Path $Repo 'target\packaging' }
$Triple = 'x86_64-pc-windows-msvc'

function Write-Step([string]$m) { Write-Host "==> $m" }

# Runs cargo inside the MSVC environment (same approach as the development
# wrapper: vcvars64.bat located with vswhere, then cargo in the repo root).
function Invoke-Cargo {
    param([string[]]$CargoArgs, [switch]$Capture)
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vcvars = $null
    if (Test-Path $vswhere) {
        $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if ($vs) { $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat' }
    }
    if (-not $vcvars -or -not (Test-Path $vcvars)) {
        throw 'MSVC build tools not found (vswhere/vcvars64.bat). Install Visual Studio 2022 Build Tools with the x64 C++ tools.'
    }
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    foreach ($a in $CargoArgs) { if ($a -match '["%!]') { throw "unsupported character in cargo argument: $a" } }
    $quoted = ($CargoArgs | ForEach-Object { if ($_ -match '[\s|&<>^()]') { '"' + $_ + '"' } else { $_ } }) -join ' '
    # A temporary batch file avoids the PowerShell 5.1 / cmd.exe quoting pitfalls.
    $bat = Join-Path ([System.IO.Path]::GetTempPath()) ("lightvm-cargo-" + [guid]::NewGuid().ToString('N') + '.cmd')
    $lines = @(
        '@echo off',
        "call `"$vcvars`" >nul 2>&1",
        "set `"PATH=$cargoBin;%PATH%`"",
        "cd /d `"$Repo`"",
        "cargo $quoted"
    )
    [System.IO.File]::WriteAllText($bat, (($lines -join "`r`n") + "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
    try {
        if ($Capture) {
            $out = & cmd.exe /d /c $bat
            if ($LASTEXITCODE -ne 0) { throw "cargo $quoted failed (exit $LASTEXITCODE)" }
            return $out
        }
        & cmd.exe /d /c $bat
        if ($LASTEXITCODE -ne 0) { throw "cargo $quoted failed (exit $LASTEXITCODE)" }
    } finally { Remove-Item -Force -ErrorAction SilentlyContinue $bat }
}

function Get-Sha256([string]$Path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant() }

# --- Metadata --------------------------------------------------------------
$env:CARGO_TARGET_DIR = $TargetDir
Write-Step 'Reading cargo metadata'
$metaJson = Invoke-Cargo -Capture -CargoArgs @('metadata', '--format-version', '1', '--locked', '--filter-platform', $Triple)
$meta = ($metaJson -join "`n") | ConvertFrom-Json
$desktopPkg = $meta.packages | Where-Object { $_.name -eq 'lightvm-desktop' } | Select-Object -First 1
if (-not $Version) { $Version = $desktopPkg.version }
if ($Version -notmatch '^[0-9A-Za-z.\-+]+$') { throw "invalid version: $Version" }
$Name = "LightVM-$Version-windows-x64"
Write-Step "Package $Name"

# --- Build ------------------------------------------------------------------
# Static CRT (no Visual C++ Redistributable needed); build-host paths (user
# profile, repository, target dir; rustc applies the last matching rule) remapped so they do not end up in the binaries; PDB
# referenced by file name only.
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$RustFlags = @(
    '-Ctarget-feature=+crt-static',
    "--remap-path-prefix=$CargoHome=/cargo",
    "--remap-path-prefix=$Repo=/lightvm",
    "--remap-path-prefix=$TargetDir=/target",
    '-Clink-arg=/PDBALTPATH:%_PDB%'
)
$Release = Join-Path $TargetDir 'release'
if (-not $SkipBuild) {
    Write-Step 'Building release binaries (static CRT)'
    $oldFlags = $env:RUSTFLAGS; $oldEncoded = $env:CARGO_ENCODED_RUSTFLAGS
    $env:RUSTFLAGS = $null
    $env:CARGO_ENCODED_RUSTFLAGS = ($RustFlags -join [char]0x1f)
    try {
        # Default features only (never `dev-tools`): checked below by
        # check-release-artifacts.ps1 on the staged binaries.
        Invoke-Cargo -CargoArgs @('build', '--release', '--locked', '-p', 'lightvm-desktop', '-p', 'lightvm-service',
            '-p', 'lightvm-mcp', '--bin', 'lightvm-desktop', '--bin', 'lightvm-service', '--bin', 'lightvm-mcp')
    } finally { $env:RUSTFLAGS = $oldFlags; $env:CARGO_ENCODED_RUSTFLAGS = $oldEncoded }
}
foreach ($b in 'lightvm-desktop.exe', 'lightvm-service.exe', 'lightvm-mcp.exe') {
    if (-not (Test-Path (Join-Path $Release $b))) { throw "missing $b in $Release" }
}

# --- Staging ----------------------------------------------------------------
$Staging = Join-Path $OutDir "staging\$Name"
if (Test-Path (Join-Path $OutDir 'staging')) { Remove-Item -Recurse -Force (Join-Path $OutDir 'staging') }
New-Item -ItemType Directory -Force $Staging | Out-Null

Copy-Item (Join-Path $Release 'lightvm-desktop.exe') $Staging
Copy-Item (Join-Path $Release 'lightvm-service.exe') $Staging
Copy-Item (Join-Path $Release 'lightvm-mcp.exe') $Staging
Copy-Item (Join-Path $Repo 'README.md') $Staging
Copy-Item (Join-Path $Repo 'docs\USER_GUIDE.md') $Staging
Copy-Item (Join-Path $Repo 'LICENSE') (Join-Path $Staging 'LICENSE.txt')
Copy-Item (Join-Path $PSScriptRoot 'install.ps1') $Staging
Copy-Item (Join-Path $PSScriptRoot 'uninstall.ps1') $Staging

# Guest Tools are built separately with the XP-targeting mingw-w64 recipe.
# Their checksum files are shipped beside the supervisor and checked at run start.
if (-not $ToolsBundle) { $ToolsBundle = Join-Path $Release 'tools' }
$ToolsDest = Join-Path $Staging 'tools'
New-Item -ItemType Directory -Force -Path $ToolsDest | Out-Null
foreach ($n in @('TOOLS.EXE', 'TOOLS.PY', 'TOOLS.EXE.sha256', 'TOOLS.PY.sha256', 'README.txt', 'LICENSE.txt')) {
    $source = Join-Path $ToolsBundle $n
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Guest Tools bundle missing: $source. Build packaging/tools/build.py --output <bundle> and pass -ToolsBundle <bundle>."
    }
    Copy-Item -LiteralPath $source -Destination (Join-Path $ToolsDest $n)
}
foreach ($n in @('TOOLS.EXE', 'TOOLS.PY')) {
    $expectedTools = (Get-Content -LiteralPath (Join-Path $ToolsDest "$n.sha256") -Raw).Trim()
    if ($expectedTools -notmatch '^[0-9a-f]{64}$' -or (Get-Sha256 (Join-Path $ToolsDest $n)) -ne $expectedTools) { throw "Guest Tools checksum mismatch: $n" }
}

# --- Third-party licences ---------------------------------------------------
Write-Step 'Generating THIRD-PARTY-LICENSES.txt'
$tree = Invoke-Cargo -Capture -CargoArgs @('tree', '--locked', '-p', 'lightvm-desktop', '-p', 'lightvm-service',
    '-p', 'lightvm-mcp', '-e', 'normal', '--target', $Triple, '--prefix', 'none', '--format', '{p}|{l}')
$crates = @{}
foreach ($line in $tree) {
    if ($line -notmatch '^(\S+) v(\S+)(.*)\|(.*)$') { continue }
    $n = $Matches[1]; $v = $Matches[2]; $rest = $Matches[3]; $lic = ($Matches[4] -replace '\s*\(\*\)\s*$', '').Trim()
    if ($rest -match '\((?!\*)[A-Za-z]:\\' -or $rest -match '\(/' ) { continue } # workspace path crates
    $crates["$n@$v"] = [pscustomobject]@{ Name = $n; Version = $v; License = $lic }
}
$byId = @{}
foreach ($p in $meta.packages) { $byId["$($p.name)@$($p.version)"] = $p }
$licPattern = '^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE|AUTHORS|OFL|UFL)'
$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine('LightVM - third-party licences')
[void]$sb.AppendLine('==============================')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Generated by packaging/windows/build-package.ps1 for $Name from Cargo.lock:")
[void]$sb.AppendLine("crates linked into lightvm-desktop.exe, lightvm-service.exe and lightvm-mcp.exe for $Triple")
[void]$sb.AppendLine('(cargo tree -e normal). Licence texts are copied from the crate sources;')
[void]$sb.AppendLine('policy and review in docs/verification/F1-07-licences-packaging.md and')
[void]$sb.AppendLine('deny.toml (ADR-0023). This file is not a legal review; the SBOM is SBOM.cdx.json.')
[void]$sb.AppendLine('Also embedded: SQLite (public domain, via libsqlite3-sys "bundled"),')
[void]$sb.AppendLine('C/assembly code from BoringSSL in ring (see the ring licence below),')
[void]$sb.AppendLine('egui default fonts (OFL-1.1 and Ubuntu Font Licence, epaint_default_fonts).')
[void]$sb.AppendLine('QEMU, its firmware and guest images are NOT distributed with LightVM.')
[void]$sb.AppendLine('')
$missing = @()
$standard = [ordered]@{
    'Apache-2.0' = @{ Signature = 'Apache License\s+Version 2\.0, January 2004'; Text = $null; From = $null }
    'MIT'        = @{ Signature = 'Permission is hereby granted, free of charge'; Text = $null; From = $null }
    'BSL-1.0'    = @{ Signature = 'Boost Software License - Version 1\.0'; Text = $null; From = $null }
}
$sorted = $crates.Values | Sort-Object Name, Version
[void]$sb.AppendLine("Summary ($(@($sorted).Count) crates)")
[void]$sb.AppendLine('-----------------------------')
foreach ($c in $sorted) { [void]$sb.AppendLine(("{0} {1}: {2}" -f $c.Name, $c.Version, $c.License)) }
[void]$sb.AppendLine('')
foreach ($c in $sorted) {
    [void]$sb.AppendLine('=' * 78)
    [void]$sb.AppendLine(("{0} {1}  -  {2}" -f $c.Name, $c.Version, $c.License))
    $p = $byId["$($c.Name)@$($c.Version)"]
    $files = @()
    if ($p) {
        $dir = Split-Path -Parent $p.manifest_path
        $files += @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match $licPattern })
        foreach ($sub in 'fonts', 'licenses', 'LICENSES', 'third_party') {
            $sd = Join-Path $dir $sub
            if (Test-Path $sd) { $files += @(Get-ChildItem -LiteralPath $sd -File -Recurse | Where-Object { $_.Name -match $licPattern -or ($sub -eq 'fonts' -and $_.Extension -eq '.txt') }) }
        }
    }
    if ($files.Count -eq 0) {
        $missing += "$($c.Name) $($c.Version)"
        $authors = if ($p -and $p.authors) { ($p.authors -join ', ') } else { 'the crate authors' }
        $crateRepo = if ($p -and $p.repository) { $p.repository } else { 'n/a' }
        [void]$sb.AppendLine("The crate source ships no licence file. The standard text of the licence(s)")
        [void]$sb.AppendLine("declared above applies (see 'Standard licence texts' at the end).")
        [void]$sb.AppendLine("Copyright holders: $authors. Repository: $crateRepo")
        continue
    }
    foreach ($f in ($files | Sort-Object FullName -Unique)) {
        $text = (Get-Content -LiteralPath $f.FullName -Raw -Encoding UTF8).TrimEnd()
        foreach ($k in @($standard.Keys)) {
            if (-not $standard[$k].Text -and $text -match $standard[$k].Signature) { $standard[$k].Text = $text; $standard[$k].From = "$($c.Name) $($c.Version)/$($f.Name)" }
        }
        [void]$sb.AppendLine('-' * 78)
        [void]$sb.AppendLine("File: $($f.Name)")
        [void]$sb.AppendLine('')
        [void]$sb.AppendLine($text)
        [void]$sb.AppendLine('')
    }
}
[void]$sb.AppendLine('=' * 78)
[void]$sb.AppendLine('Standard licence texts (for the crates above that ship no licence file)')
foreach ($k in @($standard.Keys)) {
    [void]$sb.AppendLine('=' * 78)
    if ($standard[$k].Text) {
        [void]$sb.AppendLine("$k (text as shipped by $($standard[$k].From); the copyright line of that crate does not apply to other crates)")
        [void]$sb.AppendLine('')
        [void]$sb.AppendLine($standard[$k].Text)
    } else {
        [void]$sb.AppendLine("$k`: text not found in the crate sources; see https://spdx.org/licenses/$k.html")
    }
    [void]$sb.AppendLine('')
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $Staging 'THIRD-PARTY-LICENSES.txt'), ($sb.ToString() -replace "`r?`n", "`r`n"), $utf8)
Write-Host "    crates: $(@($sorted).Count); without licence file: $($missing.Count)"
if ($missing.Count -gt 0) { Write-Host "    without licence file: $($missing -join ', ')" }

# --- SBOM (F6, ADR-0023) ------------------------------------------------------
# Offline (cargo metadata --offline, Cargo.lock hashes); reproducible: the
# timestamp is the commit time. Also checks the licences against deny.toml.
Write-Step 'Generating SBOM.cdx.json (CycloneDX 1.5) and checking the licence policy'
$oldSde = $env:SOURCE_DATE_EPOCH
$env:SOURCE_DATE_EPOCH = [string][DateTimeOffset]::Parse((& git -C $Repo log -1 --format=%cI).Trim()).ToUnixTimeSeconds()
try {
    Invoke-Cargo -CargoArgs @('run', '--release', '--locked', '--offline', '-q', '-p', 'lightvm-sbom', '--',
        '--workspace', $Repo, '--target', $Triple, '--product-version', $Version,
        '--binary', (Join-Path $Staging 'lightvm-desktop.exe'),
        '--binary', (Join-Path $Staging 'lightvm-service.exe'),
        '--binary', (Join-Path $Staging 'lightvm-mcp.exe'),
        '--out', (Join-Path $Staging 'SBOM.cdx.json'),
        '--check-licences', (Join-Path $Repo 'deny.toml'))
} finally { $env:SOURCE_DATE_EPOCH = $oldSde }
if (-not (Test-Path (Join-Path $Staging 'SBOM.cdx.json'))) { throw 'SBOM.cdx.json not produced' }

# --- Release artefact check (F6) ------------------------------------------------
Write-Step 'Checking the staged artefacts (test hooks, debug settings, host paths, credentials)'
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Repo 'scripts\check-release-artifacts.ps1') -Path $Staging
if ($LASTEXITCODE -ne 0) { throw 'release artefact check failed (see the FINDING lines)' }

# --- Build info and checksums ----------------------------------------------
$commit = (& git -C $Repo rev-parse HEAD).Trim()
$dirty = [bool](& git -C $Repo status --porcelain --untracked-files=no)
$commitTime = [DateTimeOffset]::Parse((& git -C $Repo log -1 --format=%cI).Trim())
$rustc = (Invoke-Cargo -Capture -CargoArgs @('--version')) -join ' '
$info = @(
    "Package:      $Name",
    "Commit:       $commit$(if ($dirty) { ' (working tree with uncommitted changes)' })",
    "Commit time:  $($commitTime.ToString('o'))",
    "Toolchain:    $rustc (rust-toolchain.toml), target $Triple, flags: +crt-static, --remap-path-prefix (cargo home, target dir, repository), /PDBALTPATH:%_PDB%",
    "Build:        cargo build --release --locked -p lightvm-desktop -p lightvm-service -p lightvm-mcp (default features, no dev-tools)",
    'SBOM:         SBOM.cdx.json (CycloneDX 1.5, crates/lightvm-sbom)',
    'Signature:    NOT code-signed (ADR-0020: no signing credentials). Windows SmartScreen may warn.',
    'QEMU:         NOT included. Install QEMU 11.1.0 separately (USER_GUIDE.md, ADR-0010).',
    'Status:       experimental alpha (ROADMAP F2), not a supported release.'
)
[System.IO.File]::WriteAllText((Join-Path $Staging 'BUILDINFO.txt'), (($info -join "`r`n") + "`r`n"), $utf8)

$sums = Get-ChildItem -LiteralPath $Staging -Recurse -File | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object Name |
    ForEach-Object { $relative = $_.FullName.Substring($Staging.Length + 1).Replace('\', '/'); "$(Get-Sha256 $_.FullName)  $relative" }
[System.IO.File]::WriteAllText((Join-Path $Staging 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"), $utf8)

# --- Zip (forward-slash entries, timestamps = commit time) -------------------
Write-Step 'Creating zip'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$Zip = Join-Path $OutDir "$Name.zip"
if (Test-Path $Zip) { Remove-Item -Force $Zip }
$fs = [System.IO.File]::Open($Zip, [System.IO.FileMode]::CreateNew)
try {
    $za = New-Object System.IO.Compression.ZipArchive($fs, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($f in (Get-ChildItem -LiteralPath $Staging -Recurse -File | Sort-Object FullName)) {
            $relative = $f.FullName.Substring($Staging.Length + 1).Replace('\', '/')
            $e = $za.CreateEntry("$Name/$relative", [System.IO.Compression.CompressionLevel]::Optimal)
            $e.LastWriteTime = $commitTime
            $es = $e.Open()
            try { $in = [System.IO.File]::OpenRead($f.FullName); try { $in.CopyTo($es) } finally { $in.Dispose() } } finally { $es.Dispose() }
        }
    } finally { $za.Dispose() }
} finally { $fs.Dispose() }
$zipHash = Get-Sha256 $Zip
[System.IO.File]::WriteAllText("$Zip.sha256", "$zipHash  $Name.zip`n", $utf8)
Write-Host "    $Zip"
Write-Host "    SHA-256 $zipHash"

# --- Optional Inno Setup per-user installer -----------------------------------
$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | ForEach-Object Source),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if ($NoSetupExe) {
    Write-Step 'Inno Setup installer skipped (-NoSetupExe)'
} elseif (-not $iscc) {
    Write-Step 'Inno Setup 6 not found: setup.exe not produced (the zip is the reference package)'
} else {
    Write-Step "Compiling per-user setup.exe with $iscc"
    $numeric = ($Version -replace '[^0-9.].*$', '')
    if ($numeric -notmatch '^\d+(\.\d+){0,3}$') { $numeric = '0.0.0' }
    & $iscc /Qp "/DAppVersion=$Version" "/DAppNumericVersion=$numeric" "/DStagingDir=$Staging" "/DOutputDir=$OutDir" "/DOutputBaseName=$Name-setup" (Join-Path $PSScriptRoot 'lightvm.iss')
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit $LASTEXITCODE)" }
    $Setup = Join-Path $OutDir "$Name-setup.exe"
    $setupHash = Get-Sha256 $Setup
    [System.IO.File]::WriteAllText("$Setup.sha256", "$setupHash  $Name-setup.exe`n", $utf8)
    Write-Host "    $Setup"
    Write-Host "    SHA-256 $setupHash"
}

# --- Windows Sandbox files ----------------------------------------------------
Write-Step 'Writing Windows Sandbox configuration'
$wsb = (Get-Content -Raw (Join-Path $PSScriptRoot 'sandbox\LightVM-sandbox.wsb.template')).Replace('@DIST_DIR@', [System.Security.SecurityElement]::Escape((Resolve-Path $OutDir).Path))
[System.IO.File]::WriteAllText((Join-Path $OutDir 'LightVM-sandbox.wsb'), $wsb, $utf8)
Copy-Item (Join-Path $PSScriptRoot 'sandbox\sandbox-test.ps1') (Join-Path $OutDir 'sandbox-test.ps1') -Force

Write-Step 'Done'
