# Asset notices not covered by cargo metadata / the Rust dependency SBOM.
# Dot-source from build-package.ps1. No downloads or changes to source assets.
function Copy-EmbeddedFontNotices {
    param(
        [Parameter(Mandatory)][string]$Repo,
        [Parameter(Mandatory)][string]$Staging
    )
    $fontSpecs = @(
        @{
            Name = 'Departure Mono'; Version = '1.500'
            File = 'DepartureMono-Regular.otf'; Notice = 'DepartureMono-OFL.txt'
            Sha256 = '4d53f663155cf8bf7ffc8e688776e719625f7bbb80a8d90073438b249261a2e0'
            Source = 'https://github.com/rektdeckard/departure-mono/releases/tag/v1.500'
        },
        @{
            Name = 'JetBrains Mono'; Version = '2.304'
            File = 'JetBrainsMono-Regular.ttf'; Notice = 'JetBrainsMono-OFL.txt'
            Sha256 = 'a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f'
            Source = 'https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304'
        }
    )
    $records = @()
    $fontRoot = Join-Path $Repo 'apps\desktop\assets\fonts'
    $noticeRoot = Join-Path $Staging 'licenses\fonts'
    foreach ($spec in $fontSpecs) {
        $font = Join-Path $fontRoot $spec.File
        $notice = Join-Path $fontRoot $spec.Notice
        if (-not (Test-Path -LiteralPath $font -PathType Leaf) -or
            -not (Test-Path -LiteralPath $notice -PathType Leaf)) {
            throw "Embedded font or licence text missing: $($spec.Name)"
        }
        if ((Get-FileHash -LiteralPath $font -Algorithm SHA256).Hash.ToLowerInvariant() -ne $spec.Sha256) {
            throw "Embedded font changed without updating its reviewed inventory: $($spec.Name)"
        }
        $text = Get-Content -LiteralPath $notice -Raw -Encoding UTF8
        if ($text -notmatch 'SIL OPEN FONT LICENSE Version 1\.1' -or $text -notmatch 'Copyright') {
            throw "Incomplete embedded font notice: $($spec.Name)"
        }
        New-Item -ItemType Directory -Path $noticeRoot -Force | Out-Null
        Copy-Item -LiteralPath $notice -Destination (Join-Path $noticeRoot $spec.Notice)
        $records += [pscustomobject][ordered]@{
            name = $spec.Name; version = $spec.Version
            embedded_in = 'lightvm-desktop.exe'; source_file = $spec.File
            sha256 = $spec.Sha256; source = $spec.Source
            licence = 'OFL-1.1'; notice = "licenses/fonts/$($spec.Notice)"
            notice_sha256 = (Get-FileHash -LiteralPath $notice -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $json = ConvertTo-Json -InputObject @($records) -Depth 4
    [System.IO.File]::WriteAllText((Join-Path $Staging 'BUNDLED-ASSETS.json'), $json + "`n", $utf8)
    return $records
}
