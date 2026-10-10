# F1-04: manual checks on Windows for lightvm-service (not run in CI).
#
# 1. Low-integrity process of the same user towards the named pipe
#    (same approach as F0-04: copy of the executable with
#    `icacls /setintegritylevel Low`). Expected outcome: connection refused by
#    the operating system (ERROR_ACCESS_DENIED, os error 5).
# 2. Medium-integrity control: connection and handshake accepted.
# 3. Wrong credential at medium integrity: handshake rejected.
# 4. Real kill of the parent process (intermediate PowerShell): the service
#    detects the closing of the lifeline pipe and terminates.
#
# Usage: powershell -ExecutionPolicy Bypass -File crates/lightvm-service/scripts/f1-04-windows-checks.ps1 [-Bin target\debug]
# Credentials are not printed.

param(
    [string]$Bin = "target\debug"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$binDir = Join-Path $root $Bin
$svcExe = Join-Path $binDir "lightvm-service.exe"
$probeExe = Join-Path $binDir "lightvm-ipc-probe.exe"
foreach ($f in @($svcExe, $probeExe)) {
    if (-not (Test-Path $f)) { throw "missing $f (run cargo build -p lightvm-service)" }
}

$work = Join-Path $env:TEMP ("lightvm-f1-04-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$catalog = Join-Path $work "catalog"
$lowDir = Join-Path $work "low"
$medDir = Join-Path $work "med"
New-Item -ItemType Directory -Force $catalog, $lowDir, $medDir | Out-Null

function Stamp { (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ") }
Write-Output "=== $(Stamp) F1-04 Windows checks; working in $work"
Write-Output ("user: " + [System.Security.Principal.WindowsIdentity]::GetCurrent().Name + " sid=" + [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value)

# Low- and medium-integrity copies.
$probeLow = Join-Path $lowDir "probe-low.exe"
$whoamiLow = Join-Path $lowDir "whoami-low.exe"
$probeMed = Join-Path $medDir "probe-med.exe"
Copy-Item $probeExe $probeLow
Copy-Item $probeExe $probeMed
Copy-Item (Join-Path $env:SystemRoot "System32\whoami.exe") $whoamiLow
& icacls $probeLow /setintegritylevel Low | Out-Null
& icacls $whoamiLow /setintegritylevel Low | Out-Null
Write-Output "--- icacls probe-low.exe"
& icacls $probeLow
Write-Output "--- integrity level observed by the whoami-low.exe copy"
& $whoamiLow /groups | Select-String -Pattern "S-1-16-"
Write-Output "--- integrity level of this shell"
& (Join-Path $env:SystemRoot "System32\whoami.exe") /groups | Select-String -Pattern "S-1-16-"

function Start-Service-Child([string]$catalogDir) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $svcExe
    $psi.Arguments = "--catalog `"$catalogDir`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $line = $p.StandardOutput.ReadLine()
    $ready = $line | ConvertFrom-Json
    return @{ Process = $p; Ready = $ready }
}

function Run-Probe([string]$exe, [string]$endpoint, [string]$credential) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.Arguments = "--endpoint `"$endpoint`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardInput.WriteLine($credential)
    $p.StandardInput.Close()
    $out = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    return @{ Code = $p.ExitCode; Out = $out.Trim() }
}

$svc = Start-Service-Child $catalog
$ep = $svc.Ready.endpoint
$cred = $svc.Ready.credential
Write-Output "--- service pid=$($svc.Ready.pid) endpoint=$ep (credential not printed, length $($cred.Length))"

$results = @()

Write-Output "--- 1. low-integrity probe, valid credential"
$r = Run-Probe $probeLow $ep $cred
Write-Output "exit=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ check = "low integrity"; expected = 10; actual = $r.Code }

Write-Output "--- 2. medium-integrity probe, valid credential"
$r = Run-Probe $probeMed $ep $cred
Write-Output "exit=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ check = "medium integrity"; expected = 0; actual = $r.Code }

Write-Output "--- 3. medium-integrity probe, wrong credential"
$wrong = -join ((1..64) | ForEach-Object { "0123456789abcdef"[(Get-Random -Maximum 16)] })
$r = Run-Probe $probeMed $ep $wrong
Write-Output "exit=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ check = "wrong credential"; expected = 11; actual = $r.Code }

Write-Output "--- 3b. medium-integrity probe, no credential"
$r = Run-Probe $probeMed $ep ""
Write-Output "exit=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ check = "no credential"; expected = 11; actual = $r.Code }

Write-Output "--- second instance on the same catalog"
$second = cmd /c "`"$svcExe`" --catalog `"$catalog`" 2>&1"
Write-Output "exit=$LASTEXITCODE $second"
$results += [pscustomobject]@{ check = "second instance"; expected = 3; actual = $LASTEXITCODE }

Write-Output "--- controlled shutdown (shutdown line on stdin)"
$svc.Process.StandardInput.WriteLine("shutdown")
$svc.Process.StandardInput.Flush()
if (-not $svc.Process.WaitForExit(10000)) { throw "service did not terminate" }
Write-Output "service exit=$($svc.Process.ExitCode) stderr: $($svc.Process.StandardError.ReadToEnd().Trim())"
$results += [pscustomobject]@{ check = "shutdown via stdin"; expected = 0; actual = $svc.Process.ExitCode }

Write-Output "--- 4. real kill of the parent (intermediate PowerShell)"
$catalog2 = Join-Path $work "catalog2"
New-Item -ItemType Directory -Force $catalog2 | Out-Null
$readyFile = Join-Path $work "ready2.json"
$inner = @"
`$psi = New-Object System.Diagnostics.ProcessStartInfo
`$psi.FileName = '$svcExe'
`$psi.Arguments = '--catalog "$catalog2"'
`$psi.UseShellExecute = `$false
`$psi.RedirectStandardInput = `$true
`$psi.RedirectStandardOutput = `$true
`$psi.RedirectStandardError = `$true
`$p = [System.Diagnostics.Process]::Start(`$psi)
`$line = `$p.StandardOutput.ReadLine()
`$o = `$line | ConvertFrom-Json
Set-Content -Encoding ascii -Path '$readyFile' -Value (`$o.pid)
Start-Sleep -Seconds 600
"@
$enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($inner))
$parent = Start-Process -FilePath "powershell.exe" -ArgumentList "-NoProfile", "-EncodedCommand", $enc -PassThru -WindowStyle Hidden
$deadline = (Get-Date).AddSeconds(20)
while (-not (Test-Path $readyFile) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
$svcPid = [int](Get-Content $readyFile)
$child = Get-Process -Id $svcPid
Write-Output "parent pid=$($parent.Id) service pid=$svcPid"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Stop-Process -Id $parent.Id -Force
$exited = $child.WaitForExit(10000)
$sw.Stop()
Write-Output "service terminated after the parent kill: $exited in $($sw.ElapsedMilliseconds) ms"
$results += [pscustomobject]@{ check = "parent kill"; expected = "True"; actual = "$exited" }
$orphans = Get-CimInstance Win32_Process -Filter "Name='lightvm-service.exe'" | Where-Object { $_.CommandLine -like "*$work*" }
Write-Output "leftover lightvm-service processes for this check: $(@($orphans).Count)"
$results += [pscustomobject]@{ check = "leftover processes"; expected = 0; actual = @($orphans).Count }

Write-Output "=== $(Stamp) summary"
$results | Format-Table -AutoSize | Out-String -Width 200
$fail = @($results | Where-Object { "$($_.expected)" -ne "$($_.actual)" })
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
if ($fail.Count -gt 0) { Write-Output "RESULT: FAILED ($($fail.Count))"; exit 1 }
Write-Output "RESULT: all checks as expected"
