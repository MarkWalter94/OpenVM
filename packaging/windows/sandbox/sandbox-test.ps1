<#
    Clean-machine test of the LightVM package inside Windows Sandbox (F2).
    Started by LightVM-sandbox.wsb at logon. Steps:
      1. find dist\LightVM-*-windows-x64.zip and verify it against its .sha256;
      2. extract it to the sandbox desktop and run install.ps1 (default
         per-user prefix, Start Menu shortcut);
      3. start the installed lightvm-desktop.exe (no option: the package has
         no test switches since F6), check after 10 s that it is still running
         and that %LOCALAPPDATA%\LightVM\logs\lightvm-desktop.log records the
         rendering backend and the supervisor start, then close the window
         and record the exit code and the new log lines (desktop and
         supervisor);
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
    # Timed start and stop of the product binary (the --smoke switch exists
    # only in dev-tools builds, never in the package).
    $logs = Join-Path $env:LOCALAPPDATA 'LightVM\logs'
    $uiLog = Join-Path $logs 'lightvm-desktop.log'
    $svcLog = Join-Path $logs 'lightvm-service.log'
    $uiBefore = if (Test-Path $uiLog) { @(Get-Content $uiLog).Count } else { 0 }
    $svcBefore = if (Test-Path $svcLog) { @(Get-Content $svcLog).Count } else { 0 }
    $p = Start-Process -FilePath (Join-Path $prefix 'lightvm-desktop.exe') -PassThru
    Start-Sleep -Seconds 10
    $alive = -not $p.HasExited
    Log "start: running after 10 s: $alive"
    if ($alive) {
        [void]$p.CloseMainWindow()
        if (-not $p.WaitForExit(15000)) { Log 'start: window close not completed in 15 s, process terminated'; $p.Kill() }
    }
    $p.WaitForExit()
    Log "start: exit code $($p.ExitCode)"
    $uiNew = @(if (Test-Path $uiLog) { Get-Content $uiLog | Select-Object -Skip $uiBefore })
    $svcNew = @(if (Test-Path $svcLog) { Get-Content $svcLog | Select-Object -Skip $svcBefore })
    $uiNew | ForEach-Object { Log "desktop log: $_" }
    $svcNew | ForEach-Object { Log "service log: $_" }
    $rendering = @($uiNew | Where-Object { $_ -match 'rendering: ' }).Count -gt 0
    $service = @($uiNew | Where-Object { $_ -match 'lightvm-service started' }).Count -gt 0
    Log "start: rendering backend logged: $rendering; supervisor started: $service"
    if (-not ($alive -and $rendering)) { throw 'the installed app did not start (see the log lines above)' }
    Log 'RESULT: automated part completed; continue with the manual steps in USER_GUIDE.md / F2-03.'
} catch {
    Log "RESULT: FAILED - $($_.Exception.Message)"
} finally {
    [System.IO.File]::WriteAllLines($report, $log)
}
