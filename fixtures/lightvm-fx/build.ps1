# Builds the LightVM fixture data ISO (ADR-0019) reproducibly:
#   fixtures/lightvm-fx/fx.sh  ->  <Out> (ISO 9660, volume label LIGHTVM_FX)
# No external ISO tool: the image is written by the `lightvm-fx-iso` binary of
# the `lightvm-fixture` crate (fixed timestamps and layout, same bytes on every
# build). Import the result in LightVM as a data medium (`import_media`).
#
# Usage (from a shell with the Rust toolchain and, on Windows, the MSVC
# environment):
#   pwsh fixtures/lightvm-fx/build.ps1 -Out C:\path\lightvm-fx.iso
param(
    [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Push-Location $root
try {
    cargo run --quiet -p lightvm-fixture --bin lightvm-fx-iso -- $Out
    if ($LASTEXITCODE -ne 0) { throw "lightvm-fx-iso failed ($LASTEXITCODE)" }
    (Get-FileHash -Algorithm SHA256 $Out).Hash.ToLower()
}
finally {
    Pop-Location
}
