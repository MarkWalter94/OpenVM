# Assemble a DEVELOPMENT bundle only. Release signing, licences/source closure
# and independent qualification remain required (ADR-0020/0035).
param(
  [Parameter(Mandatory)][string]$Qemu,
  [Parameter(Mandatory)][string]$Swtpm,
  [Parameter(Mandatory)][string]$Firmware,
  [Parameter(Mandatory)][string]$Out,
  # Optional outputs of the collect-licenses scripts (qemu, swtpm, firmware),
  # copied to licenses/<name> without their sources/ directory (the
  # corresponding sources are distributed beside the bundle, not in it).
  [string[]]$Licenses = @()
)
$ErrorActionPreference = 'Stop'
# [IO.Path]::GetRelativePath needs PowerShell 7; this also runs on 5.1.
function Get-Relative([string]$Root, [string]$Path) {
  $prefix = $Root.TrimEnd('\') + '\'
  if (-not $Path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Outside $Root" }
  $Path.Substring($prefix.Length)
}
# Every file under $Root, walked without following links: Get-ChildItem
# -Recurse of 5.1 descends into junctions and directory symlinks, and the
# files reached through them are not reparse points themselves. Any
# reparse point (the root, a directory or a file) is refused.
function Get-InputFiles([string]$Root) {
  $rootItem = Get-Item -LiteralPath $Root -Force
  if (-not $rootItem.PSIsContainer) { throw "$Root is not a directory" }
  $pending = New-Object System.Collections.Generic.Stack[System.IO.FileSystemInfo]
  $pending.Push($rootItem)
  while ($pending.Count -gt 0) {
    $dir = $pending.Pop()
    if ($dir.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Link in input: $($dir.FullName)" }
    foreach ($entry in Get-ChildItem -LiteralPath $dir.FullName -Force) {
      if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Link in input: $($entry.FullName)" }
      if ($entry.PSIsContainer) { $pending.Push($entry) } else { $entry }
    }
  }
}
# Relative paths against the PowerShell location, resolved once.
$destination = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
if (Test-Path -LiteralPath $destination) { throw 'Output must be a new directory' }
# All inputs are walked and checked before anything is written.
$plan = @()
foreach ($component in @(@('qemu', $Qemu), @('swtpm', $Swtpm), @('firmware', $Firmware))) {
  $source = (Resolve-Path -LiteralPath $component[1]).ProviderPath
  $plan += ,@($source, (Join-Path $destination $component[0]), @(Get-InputFiles $source), $false)
}
foreach ($dir in $Licenses) {
  $source = (Resolve-Path -LiteralPath $dir).ProviderPath
  $files = @(Get-InputFiles $source)
  if (Test-Path -LiteralPath (Join-Path $source 'MISSING.txt')) {
    $missing = @(Get-Content -LiteralPath (Join-Path $source 'MISSING.txt') | Where-Object { $_.Trim() }).Count
    if ($missing -gt 0) { throw "$source lists $missing component(s) without a licence" }
  }
  $plan += ,@($source, (Join-Path (Join-Path $destination 'licenses') (Split-Path -Leaf $source)), $files, $true)
}
New-Item -ItemType Directory -Path $destination | Out-Null
foreach ($step in $plan) {
  $source, $target, $files, $isLicence = $step
  New-Item -ItemType Directory -Force -Path $target | Out-Null
  foreach ($entry in $files) {
    $relative = Get-Relative $source $entry.FullName
    if ($isLicence -and ($relative -like 'sources\*' -or $relative -like 'MISSING*')) { continue }
    $file = Join-Path $target $relative
    New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($file)) | Out-Null
    Copy-Item -LiteralPath $entry.FullName -Destination $file
  }
}
[IO.File]::WriteAllText((Join-Path $destination 'DEVELOPMENT.txt'), 'Development only. Not release-qualified. See docs/verification/R6-05-windows11-x64-swtpm.md.' + "`n", [Text.UTF8Encoding]::new($false))
$lines = Get-ChildItem -LiteralPath $destination -Recurse -File | Sort-Object FullName | ForEach-Object {
  $relative = (Get-Relative $destination $_.FullName).Replace('\','/')
  '{0} *{1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
}
$manifest = Join-Path $destination 'SHA256SUMS'
[IO.File]::WriteAllText($manifest, ($lines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
'{0} win11-bundle development' -f (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant()
