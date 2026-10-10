<#
    Clean-machine test of the LightVM package inside Windows Sandbox (F2).
    Started by LightVM-sandbox.wsb at logon. Steps:
      1. find dist\LightVM-*-windows-x64.zip and verify it against its .sha256;
      2. extract it to the sandbox desktop and run install.ps1 (default
         per-user prefix, Start Menu shortcut);
      3. run "lightvm-desktop.exe --smoke 5 --no-supervisor" from the installed
         location and record exit code and output;
      4. write the report to Desktop\LightVM-sandbox-report.txt.
    The installation is left in place so the tester can open LightVM from the
    Start Menu (expected: QEMU "not set" in Settings, no VM can start: the
    sandbox has neither QEMU nor WHPX), then run uninstall.ps1 and check that
    %LOCALAPPDATA%\Programs\LightVM and the shortcut are gone.
#>
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$desk = [Environment]::GetFolderPath('Desktop')
$dist = Join-Path $desk 'dist'
$report = Join-Path $desk 'LightVM-sandbox-report.txt'
$log = New-Object System.Collections.Generic.List[string]
function Log([string]$m) { $log.Add($m); Write-Host $m }
try {
    Log "LightVM sandbox test, $((Get-Date).ToUniversalTime().ToString('o'))"
    Log "OS: $([Environment]::OSVersion.VersionString)"
    $zip = Get-ChildItem -LiteralPath $dist -Filter 'LightVM-*-windows-x64.zip' | Sort-Object LastWriteTime | Select-Object -Last 1
    if (-not $zip) { throw "no LightVM-*-windows-x64.zip in $dist" }
    $expected = ((Get-Content -LiteralPath "$($zip.FullName).sha256" -Raw).Trim() -split '\s+')[0]
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip.FullName).Hash.ToLowerInvariant()
    Log "zip: $($zip.Name) sha256 $actual"
    if ($actual -ne $expected) { throw "SHA-256 mismatch (expected $expected)" }
    Log 'zip checksum: OK'
    $x = Join-Path $desk 'LightVM-package'
    Expand-Archive -LiteralPath $zip.FullName -DestinationPath $x -Force
    $pkg = Get-ChildItem -LiteralPath $x -Directory | Select-Object -First 1
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $pkg.FullName 'install.ps1') 2>&1 | ForEach-Object { Log "install: $_" }
    if ($LASTEXITCODE -ne 0) { throw "install.ps1 exit $LASTEXITCODE" }
    $prefix = Join-Path $env:LOCALAPPDATA 'Programs\LightVM'
    $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'LightVM.lnk'
    Log "installed exe: $(Test-Path (Join-Path $prefix 'lightvm-desktop.exe')); shortcut: $(Test-Path $lnk)"
    $out = Join-Path $env:TEMP 'lightvm-smoke-out.txt'
    $err = Join-Path $env:TEMP 'lightvm-smoke-err.txt'
    $p = Start-Process -FilePath (Join-Path $prefix 'lightvm-desktop.exe') -ArgumentList '--smoke', '5', '--no-supervisor' `
        -RedirectStandardOutput $out -RedirectStandardError $err -PassThru -Wait
    Log "smoke exit code: $($p.ExitCode)"
    Get-Content $out -ErrorAction SilentlyContinue | ForEach-Object { Log "smoke stdout: $_" }
    Get-Content $err -ErrorAction SilentlyContinue | ForEach-Object { Log "smoke stderr: $_" }
    Log 'RESULT: automated part completed; continue with the manual steps in USER_GUIDE.md / F2-03.'
} catch {
    Log "RESULT: FAILED - $($_.Exception.Message)"
} finally {
    [System.IO.File]::WriteAllLines($report, $log)
}
