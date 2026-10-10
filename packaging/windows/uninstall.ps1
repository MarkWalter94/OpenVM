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
#>
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$Prefix,
    [switch]$RemoveUserData,
    [string]$UserDataDir = (Join-Path $env:LOCALAPPDATA 'LightVM'),
    [switch]$Force
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'

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

$running = @(Get-Process -Name 'lightvm-desktop', 'lightvm-service' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and $_.Path.StartsWith($Prefix + '\', [StringComparison]::OrdinalIgnoreCase) })
if ($running.Count -gt 0) { throw 'LightVM is running from this installation: close it (stop the VMs first) and retry.' }

# The working directory must not keep the prefix busy.
if ((Get-Location).Path.StartsWith($Prefix, [StringComparison]::OrdinalIgnoreCase)) { Set-Location $env:TEMP }

foreach ($f in @($manifest.files)) {
    if ($f -notmatch '^[A-Za-z0-9._-]+$') { Write-Warning "Skipping unexpected manifest entry: $f"; continue }
    $p = Join-Path $Prefix $f
    if ((Test-Path -LiteralPath $p) -and $PSCmdlet.ShouldProcess($p, 'Remove file')) { Remove-Item -LiteralPath $p -Force }
}
if ($manifest.shortcut -and $manifest.shortcut -like '*.lnk' -and (Test-Path -LiteralPath $manifest.shortcut)) {
    if ($PSCmdlet.ShouldProcess($manifest.shortcut, 'Remove shortcut')) { Remove-Item -LiteralPath $manifest.shortcut -Force }
}
if ($manifest.path_added) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ($userPath) {
        $kept = @($userPath.Split(';') | Where-Object { $_ -and ($_.TrimEnd('\') -ine $Prefix) })
        if ($PSCmdlet.ShouldProcess('user PATH', "Remove $Prefix")) {
            [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
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
