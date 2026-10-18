<#
.SYNOPSIS
    Installs LightVM (experimental alpha) for the current user, without
    administrator rights.

.DESCRIPTION
    Run from the extracted package folder:

        powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1

    - Verifies the package files against SHA256SUMS.txt.
    - Copies them into the prefix (default %LOCALAPPDATA%\Programs\LightVM).
    - Creates a Start Menu shortcut for the current user ("LightVM").
    - Changes the user PATH only with -AddToPath.
    - Writes install-manifest.json in the prefix (first, before copying, and
      again after each step), used by uninstall.ps1 to remove exactly what
      was installed. An interrupted install can be repaired with -Force or
      removed with uninstall.ps1.
    It never touches the VM catalog and settings (%LOCALAPPDATA%\LightVM).
    The binaries are NOT code-signed (ADR-0020). QEMU is not included.

.PARAMETER Prefix
    Installation folder. Default: %LOCALAPPDATA%\Programs\LightVM.

.PARAMETER StartMenuDir
    Folder of the shortcut. Default: the user's Start Menu "Programs" folder.

.PARAMETER NoShortcut
    Does not create the Start Menu shortcut.

.PARAMETER AddToPath
    Adds the prefix to the user PATH (HKCU\Environment). Off by default.

.PARAMETER Force
    Replaces an existing installation in the prefix.

.PARAMETER EnvironmentKey
    Registry key, under HKEY_CURRENT_USER, that holds the user PATH changed
    by -AddToPath. Default: Environment. For tests only: point it to a
    scratch key so that the real user PATH is not touched.
#>
[CmdletBinding()]
param(
    [string]$Prefix = (Join-Path $env:LOCALAPPDATA 'Programs\LightVM'),
    [string]$StartMenuDir = [Environment]::GetFolderPath('Programs'),
    [switch]$NoShortcut,
    [switch]$AddToPath,
    [switch]$Force,
    [string]$EnvironmentKey = 'Environment'
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# The user PATH is a REG_EXPAND_SZ value: entries such as %USERPROFILE%\bin
# must stay unexpanded, so it is read without expansion and written back
# with its kind ([Environment]::SetEnvironmentVariable would expand it and
# store a REG_SZ). The same functions are in uninstall.ps1.
function Get-UserPathValue([string]$Key) {
    $expand = [Microsoft.Win32.RegistryValueKind]::ExpandString
    $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($Key)
    if (-not $k) { return @{ Value = ''; Kind = $expand; Exists = $false } }
    try {
        $v = $k.GetValue('Path', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $v) { return @{ Value = ''; Kind = $expand; Exists = $false } }
        $kind = $k.GetValueKind('Path')
        if ($kind -ne [Microsoft.Win32.RegistryValueKind]::String) { $kind = $expand }
        return @{ Value = [string]$v; Kind = $kind; Exists = $true }
    } finally { $k.Close() }
}

function Set-UserPathValue([string]$Key, [string]$Value, [Microsoft.Win32.RegistryValueKind]$Kind) {
    $type = if ($Kind -eq [Microsoft.Win32.RegistryValueKind]::String) { 'String' } else { 'ExpandString' }
    $regPath = "HKCU:\$Key"
    if (-not (Test-Path -LiteralPath $regPath)) { New-Item -Path $regPath -Force | Out-Null }
    Set-ItemProperty -LiteralPath $regPath -Name 'Path' -Value $Value -Type $type
}

# Tells running programs (Explorer, new terminals) that the environment changed.
function Send-EnvironmentChange {
    try {
        if (-not ('LightVM.NativeEnv' -as [type])) {
            Add-Type -Namespace LightVM -Name NativeEnv -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out System.UIntPtr lpdwResult);
'@
        }
        $r = [UIntPtr]::Zero
        # HWND_BROADCAST, WM_SETTINGCHANGE "Environment", SMTO_ABORTIFHUNG, 5 s.
        [void][LightVM.NativeEnv]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$r)
    } catch {
        Write-Warning "Could not announce the PATH change ($_): it applies after signing out and in again."
    }
}

$Source = $PSScriptRoot
$Payload = @(
    'lightvm-desktop.exe', 'lightvm-service.exe', 'lightvm-mcp.exe', 'README.md', 'USER_GUIDE.md',
    'LICENSE.txt', 'THIRD-PARTY-LICENSES.txt', 'SBOM.cdx.json', 'BUNDLED-ASSETS.json', 'BUILDINFO.txt', 'SHA256SUMS.txt',
    'licenses/fonts/DepartureMono-OFL.txt', 'licenses/fonts/JetBrainsMono-OFL.txt',
    'install.ps1', 'uninstall.ps1',
    'tools/TOOLS.EXE', 'tools/TOOLS.PY', 'tools/TOOLS.EXE.sha256', 'tools/TOOLS.PY.sha256', 'tools/README.txt', 'tools/LICENSE.txt'
)
$ShortcutName = 'LightVM.lnk'

if (-not [Environment]::Is64BitOperatingSystem) { throw 'LightVM requires 64-bit Windows (x64).' }
$build = [Environment]::OSVersion.Version
if ($build.Major -lt 10 -or $build.Build -lt 22000) {
    Write-Warning "Windows build ${build}: LightVM targets Windows 11 (build 22000 or later)."
}

# 1. Package integrity (protects against a truncated or modified extraction;
#    it does not replace checking the zip's SHA-256 before extracting it).
$sumsFile = Join-Path $Source 'SHA256SUMS.txt'
if (-not (Test-Path -LiteralPath $sumsFile)) { throw "SHA256SUMS.txt not found next to install.ps1 ($Source)." }
$expected = @{}
foreach ($l in Get-Content -LiteralPath $sumsFile) {
    if ($l -match '^([0-9a-f]{64})  (\S+)$') { $expected[$Matches[2]] = $Matches[1] }
}
foreach ($f in $Payload) {
    $p = Join-Path $Source $f
    if (-not (Test-Path -LiteralPath $p)) { throw "Package incomplete: $f missing." }
    if ($f -eq 'SHA256SUMS.txt') { continue }
    if (-not $expected.ContainsKey($f)) { throw "Package inconsistent: $f not listed in SHA256SUMS.txt." }
    $h = (Get-FileHash -Algorithm SHA256 -LiteralPath $p).Hash.ToLowerInvariant()
    if ($h -ne $expected[$f]) { throw "Checksum mismatch for $f (expected $($expected[$f]), found $h)." }
}
Write-Host 'Package checksums: OK'

# 2. Prefix (normalised without a trailing '\', as in uninstall.ps1).
$Prefix = [System.IO.Path]::GetFullPath($Prefix).TrimEnd('\')
if ($Prefix -notmatch '\\') { throw "The prefix $Prefix is a drive root: choose a folder." }
$srcFull = [System.IO.Path]::GetFullPath($Source).TrimEnd('\')
if ($Prefix -ieq $srcFull) { throw 'The prefix is the package folder itself: extract the package elsewhere.' }
$manifestPath = Join-Path $Prefix 'install-manifest.json'
$previous = $null
if (Test-Path -LiteralPath $Prefix) {
    $existing = @(Get-ChildItem -LiteralPath $Prefix -Force)
    if ($existing.Count -gt 0 -and -not $Force) {
        throw "The prefix $Prefix is not empty. Use -Force to replace an existing LightVM installation."
    }
    if ($existing.Count -gt 0 -and -not (Test-Path -LiteralPath $manifestPath)) {
        throw "The prefix $Prefix is not empty and contains no LightVM installation (install-manifest.json missing): refusing to overwrite."
    }
    if (Test-Path -LiteralPath $manifestPath) {
        $previous = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
        if ($previous.product -ne 'LightVM') { throw "$manifestPath is not a LightVM manifest: refusing to overwrite." }
    }
}
$running = @(Get-Process -Name 'lightvm-desktop', 'lightvm-service', 'lightvm-mcp' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($Prefix + '\', [StringComparison]::OrdinalIgnoreCase) })
if ($running.Count -gt 0) { throw 'LightVM is running from the prefix: close it (stop the VMs first, and the AI clients using lightvm-mcp) and retry.' }

# The manifest is written BEFORE anything else and rewritten after every
# step, so that an install interrupted half-way (state "installing") can be
# repaired with install.ps1 -Force or removed with uninstall.ps1. It lists
# only what this script writes (the payload names, the shortcut once
# created, the PATH entry once added).
$version = $null
$bi = Join-Path $Source 'BUILDINFO.txt'
$m = Select-String -LiteralPath $bi -Pattern '^Package:\s+(\S+)' | Select-Object -First 1
if ($m) { $version = $m.Matches[0].Groups[1].Value }
$shortcutPath = $null
# A shortcut created by an earlier install of this prefix stays listed (it
# is replaced below unless -NoShortcut).
if ($previous -and ($previous.PSObject.Properties.Name -contains 'shortcut') -and $previous.shortcut -and
    (Test-Path -LiteralPath ([string]$previous.shortcut) -PathType Leaf)) { $shortcutPath = [string]$previous.shortcut }
$pathAdded = $false
# A PATH entry added by an earlier (possibly interrupted) install of this
# prefix stays owned by LightVM while it is still there.
if ($previous -and ($previous.PSObject.Properties.Name -contains 'path_added') -and $previous.path_added) {
    $current = Get-UserPathValue $EnvironmentKey
    if (@($current.Value.Split(';') | Where-Object { $_ -and $_.TrimEnd('\') -ieq $Prefix }).Count -gt 0) { $pathAdded = $true }
}
$utf8 = New-Object System.Text.UTF8Encoding($false)
function Write-InstallManifest([string]$State) {
    $manifest = [ordered]@{
        product      = 'LightVM'
        package      = $version
        state        = $State
        installed_at = (Get-Date).ToUniversalTime().ToString('o')
        prefix       = $Prefix
        files        = $Payload + @('install-manifest.json')
        shortcut     = $shortcutPath
        path_added   = $pathAdded
    }
    [System.IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json), $utf8)
}

New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
Write-InstallManifest 'installing'
foreach ($f in $Payload) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent (Join-Path $Prefix $f)) | Out-Null
    Copy-Item -LiteralPath (Join-Path $Source $f) -Destination (Join-Path $Prefix $f) -Force
}
Write-Host "Files copied to $Prefix"

# 3. Shortcut.
if (-not $NoShortcut) {
    New-Item -ItemType Directory -Force -Path $StartMenuDir | Out-Null
    $newShortcut = Join-Path ([System.IO.Path]::GetFullPath($StartMenuDir)) $ShortcutName
    $shell = New-Object -ComObject WScript.Shell
    $lnk = $shell.CreateShortcut($newShortcut)
    $lnk.TargetPath = Join-Path $Prefix 'lightvm-desktop.exe'
    $lnk.WorkingDirectory = $Prefix
    $lnk.Description = 'LightVM (experimental alpha)'
    $lnk.IconLocation = (Join-Path $Prefix 'lightvm-desktop.exe') + ',0'
    $lnk.Save()
    [void][System.Runtime.InteropServices.Marshal]::ReleaseComObject($shell)
    $shortcutPath = $newShortcut
    Write-InstallManifest 'installing'
    Write-Host "Start Menu shortcut: $shortcutPath"
}

# 4. Optional user PATH.
if ($AddToPath) {
    $current = Get-UserPathValue $EnvironmentKey
    $parts = @(if ($current.Value) { $current.Value.Split(';') | Where-Object { $_ } })
    if (-not ($parts | Where-Object { $_.TrimEnd('\') -ieq $Prefix })) {
        Set-UserPathValue $EnvironmentKey (($parts + $Prefix) -join ';') $current.Kind
        $pathAdded = $true
        Write-InstallManifest 'installing'
        Send-EnvironmentChange
        Write-Host "Added to the user PATH: $Prefix (new terminals only)"
    }
}

# 5. Manifest for the uninstaller: installation complete.
Write-InstallManifest 'installed'

Write-Host ''
Write-Host "LightVM installed in $Prefix."
Write-Host 'Not code-signed: Windows SmartScreen may show a warning on first start.'
Write-Host 'QEMU is not included: see USER_GUIDE.md, section "Install QEMU".'
Write-Host "Uninstall: powershell -NoProfile -ExecutionPolicy Bypass -File `"$Prefix\uninstall.ps1`""
