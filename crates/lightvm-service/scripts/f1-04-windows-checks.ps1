# F1-04: prove manuali su Windows per lightvm-service (non eseguite in CI).
#
# 1. Processo a integrita' bassa dello stesso utente verso la named pipe
#    (stesso approccio di F0-04: copia dell'eseguibile con
#    `icacls /setintegritylevel Low`). Esito atteso: connessione rifiutata dal
#    sistema operativo (ERROR_ACCESS_DENIED, os error 5).
# 2. Controllo a integrita' media: connessione e handshake accettati.
# 3. Credenziale errata a integrita' media: handshake rifiutato.
# 4. Kill reale del processo genitore (PowerShell intermedio): il servizio
#    rileva la chiusura della pipe di vita e termina.
#
# Uso: powershell -ExecutionPolicy Bypass -File crates/lightvm-service/scripts/f1-04-windows-checks.ps1 [-Bin target\debug]
# Le credenziali non vengono stampate.

param(
    [string]$Bin = "target\debug"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
$binDir = Join-Path $root $Bin
$svcExe = Join-Path $binDir "lightvm-service.exe"
$probeExe = Join-Path $binDir "lightvm-ipc-probe.exe"
foreach ($f in @($svcExe, $probeExe)) {
    if (-not (Test-Path $f)) { throw "manca $f (eseguire cargo build -p lightvm-service)" }
}

$work = Join-Path $env:TEMP ("lightvm-f1-04-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$catalog = Join-Path $work "catalog"
$lowDir = Join-Path $work "low"
$medDir = Join-Path $work "med"
New-Item -ItemType Directory -Force $catalog, $lowDir, $medDir | Out-Null

function Stamp { (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss.fffZ") }
Write-Output "=== $(Stamp) F1-04 prove Windows; lavoro in $work"
Write-Output ("utente: " + [System.Security.Principal.WindowsIdentity]::GetCurrent().Name + " sid=" + [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value)

# Copie a integrita' bassa e media.
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
Write-Output "--- livello di integrita' osservato dalla copia whoami-low.exe"
& $whoamiLow /groups | Select-String -Pattern "S-1-16-"
Write-Output "--- livello di integrita' di questa shell"
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
Write-Output "--- servizio pid=$($svc.Ready.pid) endpoint=$ep (credenziale non stampata, lunghezza $($cred.Length))"

$results = @()

Write-Output "--- 1. probe a integrita' bassa, credenziale valida"
$r = Run-Probe $probeLow $ep $cred
Write-Output "uscita=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ prova = "integrita' bassa"; atteso = 10; ottenuto = $r.Code }

Write-Output "--- 2. probe a integrita' media, credenziale valida"
$r = Run-Probe $probeMed $ep $cred
Write-Output "uscita=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ prova = "integrita' media"; atteso = 0; ottenuto = $r.Code }

Write-Output "--- 3. probe a integrita' media, credenziale errata"
$wrong = -join ((1..64) | ForEach-Object { "0123456789abcdef"[(Get-Random -Maximum 16)] })
$r = Run-Probe $probeMed $ep $wrong
Write-Output "uscita=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ prova = "credenziale errata"; atteso = 11; ottenuto = $r.Code }

Write-Output "--- 3b. probe a integrita' media, senza credenziale"
$r = Run-Probe $probeMed $ep ""
Write-Output "uscita=$($r.Code) $($r.Out)"
$results += [pscustomobject]@{ prova = "senza credenziale"; atteso = 11; ottenuto = $r.Code }

Write-Output "--- seconda istanza sullo stesso catalogo"
$second = cmd /c "`"$svcExe`" --catalog `"$catalog`" 2>&1"
Write-Output "uscita=$LASTEXITCODE $second"
$results += [pscustomobject]@{ prova = "seconda istanza"; atteso = 3; ottenuto = $LASTEXITCODE }

Write-Output "--- arresto controllato (riga shutdown sullo stdin)"
$svc.Process.StandardInput.WriteLine("shutdown")
$svc.Process.StandardInput.Flush()
if (-not $svc.Process.WaitForExit(10000)) { throw "servizio non terminato" }
Write-Output "uscita servizio=$($svc.Process.ExitCode) stderr: $($svc.Process.StandardError.ReadToEnd().Trim())"
$results += [pscustomobject]@{ prova = "shutdown via stdin"; atteso = 0; ottenuto = $svc.Process.ExitCode }

Write-Output "--- 4. kill reale del genitore (PowerShell intermedio)"
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
Write-Output "genitore pid=$($parent.Id) servizio pid=$svcPid"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Stop-Process -Id $parent.Id -Force
$exited = $child.WaitForExit(10000)
$sw.Stop()
Write-Output "servizio terminato dopo il kill del genitore: $exited in $($sw.ElapsedMilliseconds) ms"
$results += [pscustomobject]@{ prova = "kill del genitore"; atteso = "True"; ottenuto = "$exited" }
$orphans = Get-CimInstance Win32_Process -Filter "Name='lightvm-service.exe'" | Where-Object { $_.CommandLine -like "*$work*" }
Write-Output "processi lightvm-service residui per questa prova: $(@($orphans).Count)"
$results += [pscustomobject]@{ prova = "processi residui"; atteso = 0; ottenuto = @($orphans).Count }

Write-Output "=== $(Stamp) riepilogo"
$results | Format-Table -AutoSize | Out-String -Width 200
$fail = @($results | Where-Object { "$($_.atteso)" -ne "$($_.ottenuto)" })
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
if ($fail.Count -gt 0) { Write-Output "ESITO: FALLITO ($($fail.Count))"; exit 1 }
Write-Output "ESITO: tutte le prove come atteso"
