<#
.SYNOPSIS
    Uninstalls LightVM (experimental alpha) installed by install.ps1.

.DESCRIPTION
        powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\LightVM\uninstall.ps1"

    Removes only what install-manifest.json lists (files, Start Menu shortcut,
    user PATH entry if install.ps1 added it), then the prefix folder if it is
    empty. The VMs, the catalog, the settings and the audit log in
    %LOCALAPPDATA%\LightVM are NOT removed unless -RemoveUserData is given
    (which asks for confirmation; -Force skips it). A catalog moved to a
    custom folder in Settings is not under %LOCALAPPDATA%\LightVM and is
    never removed by this script.

.PARAMETER Prefix
    Installation folder. Default: the folder of this script if it contains
    install-manifest.json, otherwise %LOCALAPPDATA%\Programs\LightVM.

.PARAMETER RemoveUserData
    Also deletes %LOCALAPPDATA%\LightVM (ALL VMs, disks, imported media,
    settings and audit log). Irreversible.

.PARAMETER Force
    With -RemoveUserData: does not ask for confirmation.

.PARAMETER UserDataDir
    User data folder removed by -RemoveUserData. Default: %LOCALAPPDATA%\LightVM.

.PARAMETER EnvironmentKey
    Registry key, under HKEY_CURRENT_USER, that holds the user PATH.
    Default: Environment. For tests only (scratch key).
#>
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$Prefix,
    [switch]$RemoveUserData,
    [string]$UserDataDir = (Join-Path $env:LOCALAPPDATA 'LightVM'),
    [switch]$Force,
    [string]$EnvironmentKey = 'Environment'
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

# User PATH read without expansion and written back with its kind
# (REG_EXPAND_SZ stays REG_EXPAND_SZ): same functions as install.ps1.
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

if (-not $Prefix) {
    if (Test-Path -LiteralPath (Join-Path $PSScriptRoot 'install-manifest.json')) { $Prefix = $PSScriptRoot }
    else { $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\LightVM' }
}
$Prefix = [System.IO.Path]::GetFullPath($Prefix).TrimEnd('\')
$manifestPath = Join-Path $Prefix 'install-manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath)) {
    throw "No LightVM installation in $Prefix (install-manifest.json missing). Nothing removed."
}
$manifest = Get-Content -Raw -LiteralPath $manifestPath | ConvertFrom-Json
if ($manifest.product -ne 'LightVM') { throw "$manifestPath is not a LightVM manifest. Nothing removed." }

$running = @(Get-Process -Name 'lightvm-desktop', 'lightvm-service', 'lightvm-mcp' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($Prefix + '\', [StringComparison]::OrdinalIgnoreCase) })
if ($running.Count -gt 0) { throw 'LightVM is running from this installation: close it (stop the VMs first, and the AI clients using lightvm-mcp) and retry.' }

# The working directory must not keep the prefix busy.
if ((Get-Location).Path.StartsWith($Prefix, [StringComparison]::OrdinalIgnoreCase)) { Set-Location $env:TEMP }

# Only plain file names directly in the prefix: no separators, no "." or
# "..", and only files (never a folder).
foreach ($f in @($manifest.files)) {
    $knownTools = @('tools/TOOLS.EXE', 'tools/TOOLS.PY', 'tools/TOOLS.EXE.sha256', 'tools/TOOLS.PY.sha256', 'tools/README.txt', 'tools/LICENSE.txt')
    $toolEntry = $f -is [string] -and $knownTools -contains $f
    $toolsDir = Join-Path $Prefix 'tools'
    if ($toolEntry -and (Test-Path -LiteralPath $toolsDir) -and ((Get-Item -LiteralPath $toolsDir -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { Write-Warning 'Skipping reparse-point Tools directory'; continue }
    if ($f -isnot [string] -or (-not $toolEntry -and ($f -notmatch '^[A-Za-z0-9._-]+$' -or $f -match '^\.+$'))) {
        Write-Warning "Skipping unexpected manifest entry: $f"; continue
    }
    $p = Join-Path $Prefix $f
    if ((Test-Path -LiteralPath $p -PathType Leaf) -and $PSCmdlet.ShouldProcess($p, 'Remove file')) { Remove-Item -LiteralPath $p -Force }
}
$toolsDir = Join-Path $Prefix 'tools'
if ((Test-Path -LiteralPath $toolsDir -PathType Container) -and -not ((Get-Item -LiteralPath $toolsDir -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -and @(Get-ChildItem -LiteralPath $toolsDir -Force).Count -eq 0) {
    if ($PSCmdlet.ShouldProcess($toolsDir, 'Remove empty Tools directory')) { Remove-Item -LiteralPath $toolsDir -Force }
}
# The shortcut only if it is a .lnk file in the user's Start Menu "Programs"
# folder (a manifest cannot make the script delete files elsewhere).
if ($manifest.shortcut) {
    $programs = [System.IO.Path]::GetFullPath([Environment]::GetFolderPath('Programs')).TrimEnd('\') + '\'
    $lnk = [System.IO.Path]::GetFullPath([string]$manifest.shortcut)
    if ($lnk -like '*.lnk' -and $lnk.StartsWith($programs, [StringComparison]::OrdinalIgnoreCase) -and
        (Test-Path -LiteralPath $lnk -PathType Leaf)) {
        if ($PSCmdlet.ShouldProcess($lnk, 'Remove shortcut')) { Remove-Item -LiteralPath $lnk -Force }
    } elseif (Test-Path -LiteralPath $lnk) {
        Write-Warning "Shortcut $lnk is outside the Start Menu Programs folder: left in place."
    }
}
if ($manifest.path_added) {
    $current = Get-UserPathValue $EnvironmentKey
    if ($current.Exists) {
        $parts = @($current.Value.Split(';') | Where-Object { $_ })
        $kept = @($parts | Where-Object { $_.TrimEnd('\') -ine $Prefix })
        if ($kept.Count -ne $parts.Count -and $PSCmdlet.ShouldProcess('user PATH', "Remove $Prefix")) {
            Set-UserPathValue $EnvironmentKey ($kept -join ';') $current.Kind
            Send-EnvironmentChange
        }
    }
}
if ((Test-Path -LiteralPath $Prefix) -and @(Get-ChildItem -LiteralPath $Prefix -Force).Count -eq 0) {
    if ($PSCmdlet.ShouldProcess($Prefix, 'Remove empty folder')) { Remove-Item -LiteralPath $Prefix -Force }
} elseif (Test-Path -LiteralPath $Prefix) {
    Write-Warning "$Prefix contains files not installed by LightVM: left in place."
}
Write-Host "LightVM removed from $Prefix."

if ($RemoveUserData) {
    $data = [System.IO.Path]::GetFullPath($UserDataDir).TrimEnd('\')
    if (Test-Path -LiteralPath $data) {
        if ($Force -or $PSCmdlet.ShouldContinue("Delete $data with ALL virtual machines, disks, imported media, settings and audit log? This cannot be undone.", 'Remove LightVM user data')) {
            Remove-Item -LiteralPath $data -Recurse -Force
            Write-Host "User data removed: $data"
        } else {
            Write-Host "User data kept: $data"
        }
    }
} else {
    Write-Host "Your VMs and settings were kept in $UserDataDir."
}
