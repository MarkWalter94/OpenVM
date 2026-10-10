<#
.SYNOPSIS
    Checks of install.ps1 / uninstall.ps1 that need no sandbox: user PATH
    kept as REG_EXPAND_SZ with its unexpanded entries, and the uninstaller
    manifest filter.

.DESCRIPTION
        powershell -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\install-uninstall-test.ps1

    The real user PATH is never touched: the scripts are pointed with
    -EnvironmentKey to a scratch key under HKCU\Software, removed at the end
    (the script also checks that HKCU\Environment\Path is unchanged). The
    package is a fake one (dummy files with a valid SHA256SUMS.txt) in a
    temporary folder; no shortcut is created. Exit code 0 when every check
    passes.
#>
[CmdletBinding()]
param()

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$scripts = Split-Path -Parent $here
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("lightvm-install-test-" + [guid]::NewGuid().ToString('N'))
$scratchKey = 'Software\LightVM-install-test-' + [guid]::NewGuid().ToString('N')
$failures = 0

function Check([bool]$ok, [string]$what) {
    if ($ok) { Write-Host "PASS $what" } else { Write-Host "FAIL $what"; $script:failures++ }
}

function Read-RawPath([string]$key) {
    $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($key)
    if (-not $k) { return $null }
    try {
        if ($null -eq $k.GetValue('Path')) { return $null }
        return @{
            Value = [string]$k.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            Kind  = $k.GetValueKind('Path')
        }
    } finally { $k.Close() }
}

function Invoke-Script([string]$file, [string[]]$arguments) {
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $file @arguments 2>&1
    $code = $LASTEXITCODE
    $out | ForEach-Object { Write-Host "    | $_" }
    return $code
}

$realBefore = Read-RawPath 'Environment'
try {
    # Fake package.
    $pkg = Join-Path $work 'package'
    New-Item -ItemType Directory -Force -Path $pkg | Out-Null
    $payload = @('lightvm-desktop.exe', 'lightvm-service.exe', 'README.md', 'USER_GUIDE.md',
        'LICENSE.txt', 'THIRD-PARTY-LICENSES.txt', 'BUILDINFO.txt')
    foreach ($f in $payload) { Set-Content -LiteralPath (Join-Path $pkg $f) -Value "dummy $f" -Encoding ascii }
    Copy-Item -LiteralPath (Join-Path $scripts 'install.ps1') -Destination $pkg
    Copy-Item -LiteralPath (Join-Path $scripts 'uninstall.ps1') -Destination $pkg
    $sums = foreach ($f in $payload + @('install.ps1', 'uninstall.ps1')) {
        '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $pkg $f)).Hash.ToLowerInvariant(), $f
    }
    [System.IO.File]::WriteAllText((Join-Path $pkg 'SHA256SUMS.txt'), (($sums -join "`n") + "`n"))

    # 1. -AddToPath keeps REG_EXPAND_SZ and the unexpanded entries.
    $scratch = "HKCU:\$scratchKey"
    New-Item -Path $scratch -Force | Out-Null
    $original = '%USERPROFILE%\bin;C:\Tools'
    New-ItemProperty -LiteralPath $scratch -Name 'Path' -Value $original -PropertyType ExpandString | Out-Null
    $prefix = Join-Path $work 'prefix'
    $code = Invoke-Script (Join-Path $pkg 'install.ps1') @('-Prefix', $prefix, '-NoShortcut', '-AddToPath', '-EnvironmentKey', $scratchKey)
    Check ($code -eq 0) 'install.ps1 -AddToPath exit code 0'
    $after = Read-RawPath $scratchKey
    Check ($after.Kind -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) "PATH kind after install is ExpandString (found $($after.Kind))"
    Check ($after.Value -eq "$original;$prefix") "PATH after install keeps %USERPROFILE% unexpanded (found '$($after.Value)')"

    # 2. uninstall removes only the prefix entry, kind kept.
    $code = Invoke-Script (Join-Path $prefix 'uninstall.ps1') @('-Prefix', $prefix, '-EnvironmentKey', $scratchKey)
    Check ($code -eq 0) 'uninstall.ps1 exit code 0'
    $after = Read-RawPath $scratchKey
    Check ($after.Kind -eq [Microsoft.Win32.RegistryValueKind]::ExpandString) "PATH kind after uninstall is ExpandString (found $($after.Kind))"
    Check ($after.Value -eq $original) "PATH after uninstall is the original (found '$($after.Value)')"
    Check (-not (Test-Path -LiteralPath $prefix)) 'prefix removed'

    # 3. Manifest filter: ".", "..", a folder and a shortcut outside the
    #    Start Menu are not removed; a listed file is.
    $prefix2 = Join-Path $work 'prefix2'
    New-Item -ItemType Directory -Force -Path (Join-Path $prefix2 'sub') | Out-Null
    Set-Content -LiteralPath (Join-Path $prefix2 'sub\keep.txt') -Value 'keep' -Encoding ascii
    Set-Content -LiteralPath (Join-Path $prefix2 'listed.txt') -Value 'x' -Encoding ascii
    Set-Content -LiteralPath (Join-Path $work 'sentinel.txt') -Value 'parent' -Encoding ascii
    $outsideLnk = Join-Path $work 'outside.lnk'
    Set-Content -LiteralPath $outsideLnk -Value 'not a shortcut' -Encoding ascii
    Copy-Item -LiteralPath (Join-Path $scripts 'uninstall.ps1') -Destination $prefix2
    $manifest = [ordered]@{
        product    = 'LightVM'
        prefix     = $prefix2
        files      = @('.', '..', '...', 'sub', 'listed.txt', 'uninstall.ps1', 'install-manifest.json')
        shortcut   = $outsideLnk
        path_added = $false
    }
    [System.IO.File]::WriteAllText((Join-Path $prefix2 'install-manifest.json'), ($manifest | ConvertTo-Json))
    $code = Invoke-Script (Join-Path $prefix2 'uninstall.ps1') @('-Prefix', $prefix2, '-EnvironmentKey', $scratchKey)
    Check ($code -eq 0) 'uninstall.ps1 with a hostile manifest exit code 0'
    Check (-not (Test-Path -LiteralPath (Join-Path $prefix2 'listed.txt'))) 'listed file removed'
    Check (Test-Path -LiteralPath (Join-Path $prefix2 'sub\keep.txt')) 'folder entry "sub" not removed'
    Check (Test-Path -LiteralPath (Join-Path $work 'sentinel.txt')) 'parent folder ("..") untouched'
    Check (Test-Path -LiteralPath $outsideLnk) 'shortcut outside the Start Menu Programs folder kept'
    Check ((Read-RawPath $scratchKey).Value -eq $original) 'PATH untouched when path_added is false'
} finally {
    Remove-Item -LiteralPath "HKCU:\$scratchKey" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

$realAfter = Read-RawPath 'Environment'
Check (($null -eq $realBefore -and $null -eq $realAfter) -or
    ($realBefore.Value -eq $realAfter.Value -and $realBefore.Kind -eq $realAfter.Kind)) 'real user PATH unchanged'

if ($failures -gt 0) { Write-Host "$failures check(s) failed"; exit 1 }
Write-Host 'All checks passed'
exit 0
