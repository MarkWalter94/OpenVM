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
    -Uninstall also verifies payload/shortcut removal and user-file/PATH
    preservation. LightVM-sandbox-auto.wsb selects this complete sequence.
    The installation is left in place so the tester can open LightVM from the
    Start Menu (expected: QEMU "not set" in Settings, no VM can start: the
    sandbox has neither QEMU nor WHPX), then run uninstall.ps1 and check that
    %LOCALAPPDATA%\Programs\LightVM and the shortcut are gone.
#>
param([switch]$Uninstall)
Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$desk = [Environment]::GetFolderPath('Desktop')
$dist = Join-Path $desk 'dist'
$report = Join-Path $desk 'LightVM-sandbox-report.txt'
$log = New-Object System.Collections.Generic.List[string]
$result = 0
function Log([string]$m) { $log.Add($m); Write-Host $m }
try {
    Log "LightVM sandbox test, $((Get-Date).ToUniversalTime().ToString('o'))"
    if ($env:USERNAME -ne 'WDAGUtilityAccount') { throw 'Run this script inside Windows Sandbox, not on the host.' }
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
    if (-not (Test-Path $lnk)) { throw 'Start Menu shortcut missing' }
    foreach ($exe in @('lightvm-desktop.exe', 'lightvm-service.exe', 'lightvm-mcp.exe')) {
        $help = (& (Join-Path $prefix $exe) --help 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0 -or $help -notmatch 'usage: lightvm-') { throw "CLI launch failed: $exe" }
        Log "CLI help: $exe OK"
    }
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
    $forced = $false
    Log "start: running after 10 s: $alive"
    if ($alive) {
        [void]$p.CloseMainWindow()
        if (-not $p.WaitForExit(15000)) { Log 'start: window close not completed in 15 s, process terminated'; $forced = $true; $p.Kill() }
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
    if (-not ($alive -and $rendering -and $service)) { throw 'The app must render and start its supervisor' }
    if ($forced -or $p.ExitCode -ne 0) { throw 'The desktop did not close cleanly' }
    $leftover = @(Get-Process -Name lightvm-service -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $prefix 'lightvm-service.exe') })
    if ($leftover.Count) { throw 'Packaged supervisor remained after desktop shutdown' }
    if ($Uninstall) {
        $manifest = Get-Content (Join-Path $prefix 'install-manifest.json') -Raw | ConvertFrom-Json
        Set-Content (Join-Path $prefix 'user-test-file.txt') 'Preserve this user file'
        $pathBefore = [Environment]::GetEnvironmentVariable('Path', 'User')
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $prefix 'uninstall.ps1') -Prefix $prefix 2>&1 | ForEach-Object { Log "uninstall: $_" }
        if ($LASTEXITCODE -ne 0) { throw 'Uninstaller failed' }
        foreach ($file in $manifest.files) {
            if (Test-Path -LiteralPath (Join-Path $prefix $file)) { throw "Payload remained: $file" }
        }
        if ((Test-Path $lnk) -or -not (Test-Path (Join-Path $prefix 'user-test-file.txt'))) { throw 'Shortcut removal or user-file preservation failed' }
        if ([Environment]::GetEnvironmentVariable('Path', 'User') -cne $pathBefore) { throw 'User PATH changed during removal' }
        Log 'RESULT: PASS - install, CLI, desktop rendering, supervisor, clean shutdown, uninstall and user-file preservation.'
    } else {
        Log 'RESULT: automated part completed; continue with the manual steps in USER_GUIDE.md / F2-03.'
    }
} catch {
    $result = 1
    Log "RESULT: FAILED - $($_.Exception.Message)"
} finally {
    [System.IO.File]::WriteAllLines($report, $log)
}
exit $result
