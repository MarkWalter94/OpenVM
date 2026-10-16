# Assemble a DEVELOPMENT bundle only. Release signing, licences/source closure
# and independent qualification remain required (ADR-0020/0035).
param(
  [Parameter(Mandatory)][string]$Qemu,
  [Parameter(Mandatory)][string]$Swtpm,
  [Parameter(Mandatory)][string]$Firmware,
  [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Out) { throw 'Output must be a new directory' }
$destination = [IO.Path]::GetFullPath($Out)
New-Item -ItemType Directory -Path $destination | Out-Null
foreach ($component in @(@('qemu', $Qemu), @('swtpm', $Swtpm), @('firmware', $Firmware))) {
  $source = (Resolve-Path -LiteralPath $component[1]).Path
  $target = Join-Path $destination $component[0]
  New-Item -ItemType Directory -Path $target | Out-Null
  foreach ($entry in Get-ChildItem -LiteralPath $source -Recurse -File) {
    if ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Link in build output' }
    $relative = [IO.Path]::GetRelativePath($source, $entry.FullName)
    $file = Join-Path $target $relative
    New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($file)) | Out-Null
    Copy-Item -LiteralPath $entry.FullName -Destination $file
  }
}
[IO.File]::WriteAllText((Join-Path $destination 'DEVELOPMENT.txt'), 'Development only. Not release-qualified. See docs/verification/R6-05-windows11-x64-swtpm.md.' + "`n", [Text.UTF8Encoding]::new($false))
$lines = Get-ChildItem -LiteralPath $destination -Recurse -File | Sort-Object FullName | ForEach-Object {
  $relative = [IO.Path]::GetRelativePath($destination, $_.FullName).Replace('\','/')
  '{0} *{1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative
}
$manifest = Join-Path $destination 'SHA256SUMS'
[IO.File]::WriteAllText($manifest, ($lines -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
'{0} win11-bundle development' -f (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash.ToLowerInvariant()
