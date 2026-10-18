# Licence texts and source inventory of the Secure Boot OVMF built by
# build.ps1 (ADR-0035, R6-05): edk2 at the pinned commit plus the submodules
# that build.ps1 initialises. The corresponding source is that edk2 commit,
# those submodule commits and 0001-*.patch of this directory.
# Runs on Windows PowerShell 5.1.
param(
  [Parameter(Mandatory)][string]$Edk2,   # the edk2 tree build.ps1 used
  [Parameter(Mandatory)][string]$Out     # new directory
)
$ErrorActionPreference = 'Stop'
# git with its exit code checked (a native command failure does not stop
# the script on 5.1): a failed query must not leave an empty commit, URL or
# submodule list in the inventory.
function Invoke-Git {
  $output = & git @args
  if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') failed (exit $LASTEXITCODE)" }
  $output
}
function Get-GitValue {
  $value = [string](Invoke-Git @args | Select-Object -First 1)
  if (-not $value.Trim()) { throw "git $($args -join ' ') returned nothing" }
  $value.Trim()
}
$Edk2 = (Resolve-Path -LiteralPath $Edk2).Path
if (Test-Path -LiteralPath $Out) { throw 'Output must be a new directory' }
# Every git query runs before the output directory is created.
$commit = Get-GitValue -C $Edk2 rev-parse --verify HEAD
if ($commit -notmatch '^[0-9a-f]{40}$') { throw "Unexpected edk2 commit '$commit'" }
$url = Get-GitValue -C $Edk2 config --get remote.origin.url
$submodules = @()
foreach ($row in @(Invoke-Git -C $Edk2 submodule status)) {
  # Leading '-': not initialised, hence not part of the build.
  if ($row -match '^-') { continue }
  if ($row -notmatch '^[ +U]?([0-9a-f]{40}) (\S+)') { throw "Unexpected submodule status line '$row'" }
  $sha = $Matches[1]
  $path = $Matches[2]
  $subUrl = Get-GitValue -C $Edk2 config --get "submodule.$path.url"
  $submodules += ,@($sha, $path, $subUrl)
}
New-Item -ItemType Directory -Path $Out | Out-Null
$lines = @("component`tpath`tcommit`turl`tlicence files")

function Copy-Licences([string]$Dir, [string]$Name) {
  $dest = Join-Path $Out $Name
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  $files = Get-ChildItem -LiteralPath $Dir -File |
    Where-Object { $_.Name -match '^(COPYING|LICEN[CS]E|License|BSD-2-Clause$|GPL$)' }
  foreach ($f in $files) { Copy-Item -LiteralPath $f.FullName -Destination $dest }
  ($files | ForEach-Object Name) -join ' '
}

$lic = Copy-Licences $Edk2 'edk2'
$lines += "edk2`t.`t$commit`t$url`t$lic"
$missing = @()
foreach ($sub in $submodules) {
  $sha, $path, $subUrl = $sub
  $dir = Join-Path $Edk2 $path
  $name = $path.Replace('/', '_')
  $lic = Copy-Licences $dir $name
  if (-not $lic) { $missing += "$path ($sha): no top-level licence file" }
  $lines += "$name`t$path`t$sha`t$subUrl`t$lic"
}
[IO.File]::WriteAllText((Join-Path $Out 'INVENTORY.tsv'), ($lines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText((Join-Path $Out 'MISSING.txt'), ($missing -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
$lines
'missing:'
$missing
