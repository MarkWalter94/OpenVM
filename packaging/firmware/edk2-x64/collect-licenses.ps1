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
if (Test-Path -LiteralPath $Out) { throw 'Output must be a new directory' }
New-Item -ItemType Directory -Path $Out | Out-Null
$Edk2 = (Resolve-Path -LiteralPath $Edk2).Path
$lines = @("component`tpath`tcommit`turl`tlicence files")

function Copy-Licences([string]$Dir, [string]$Name) {
  $dest = Join-Path $Out $Name
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  $files = Get-ChildItem -LiteralPath $Dir -File |
    Where-Object { $_.Name -match '^(COPYING|LICEN[CS]E|License|BSD-2-Clause$|GPL$)' }
  foreach ($f in $files) { Copy-Item -LiteralPath $f.FullName -Destination $dest }
  ($files | ForEach-Object Name) -join ' '
}

$commit = git -C $Edk2 rev-parse HEAD
$url = git -C $Edk2 config --get remote.origin.url
$lic = Copy-Licences $Edk2 'edk2'
$lines += "edk2`t.`t$commit`t$url`t$lic"
$missing = @()
foreach ($row in (git -C $Edk2 submodule status)) {
  # Leading '-': not initialised, hence not part of the build.
  if ($row -match '^-') { continue }
  $parts = $row.Trim().Split(' ')
  $sha = $parts[0].TrimStart('+', 'U')
  $path = $parts[1]
  $dir = Join-Path $Edk2 $path
  $subUrl = git -C $Edk2 config --get "submodule.$path.url"
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
