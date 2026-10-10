<#
.SYNOPSIS
  Prova U-4 / P-4 (docs/STATUS.md): tastiera fisica -> winit -> egui -> QMP.

.DESCRIPTION
  Avvia QEMU (WHPX, CPU Skylake-Client-v4, ISO Ubuntu live) con console VNC e
  QMP su socket Unix in una directory privata (ACL solo utente corrente), poi
  apre lightvm-desktop sulla console reale (percorso dello spike F0-04: la UI
  parla direttamente con QMP; nel prodotto la console passera' dal supervisor
  in F2). Alla chiusura della finestra QEMU viene terminato.

  Cosa provare quando compare il desktop Ubuntu (circa 100 s):
    1. Aprire un terminale nel guest (tasto Super, scrivere "terminal").
    2. Digitare lettere, numeri e simboli: e' la tastiera US nel guest, salvo
       cambio di layout in Ubuntu; con -Layout it la UI mappa i tasti IT.
    3. AltGr: @ # [ ] { } con layout italiano attivo nel guest.
    4. Ctrl+C in un comando in corso (es. "ping 1.1.1.1" fallisce subito senza
       rete: usare "sleep 100" e poi Ctrl+C).
    5. Ctrl+V: nella console deve arrivare la combinazione di tasti, MAI il
       testo degli appunti di Windows.
    6. Tastierino numerico (con e senza Bloc Num), frecce, Esc, F1-F12.
    7. Cliccare fuori dalla finestra con un tasto premuto: il tasto deve
       risultare rilasciato nel guest.
  Annotare cosa non funziona: l'esito va registrato in docs/verification.

.PARAMETER Smoke
  Se > 0, chiude la UI dopo N secondi e salva il framebuffer (verifica senza
  persona alla tastiera).
#>
param(
    [string]$Workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path,
    [string]$QemuDir = '',
    [string]$Iso = '',
    [string]$Work = 'C:\lvmtest\u4',
    [ValidateSet('us', 'it')][string]$Layout = 'it',
    [int]$Smoke = 0
)
$ErrorActionPreference = 'Stop'
if (-not $QemuDir) { $QemuDir = Join-Path $Workspace 'lab\qemu\qemu-11.1.0' }
if (-not $Iso) { $Iso = Join-Path $Workspace 'lab\media\ubuntu-26.04.1-desktop-amd64.iso' }
$qemu = Join-Path $QemuDir 'qemu-system-x86_64.exe'
foreach ($f in @($qemu, $Iso)) { if (-not (Test-Path $f)) { throw "file mancante: $f" } }
# Il binario piu' recente fra release e debug.
$ui = @('target\release\lightvm-desktop.exe', 'target\debug\lightvm-desktop.exe') |
    ForEach-Object { Join-Path $Workspace $_ } | Where-Object { Test-Path $_ } |
    Sort-Object { (Get-Item $_).LastWriteTime } -Descending | Select-Object -First 1
if (-not $ui) { throw 'lightvm-desktop non compilato: cargo build --release -p lightvm-desktop' }

# Directory privata con ACL solo per l'utente corrente (socket VNC e QMP).
$priv = Join-Path $Work 'priv'
New-Item -ItemType Directory -Force $priv | Out-Null
& icacls $priv /inheritance:r /grant:r "$($env:USERNAME):(OI)(CI)F" | Out-Null
Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $priv 'vnc.sock'), (Join-Path $priv 'qmp.sock')
$vars = Join-Path $Work 'vars.fd'
Copy-Item -Force (Join-Path $QemuDir 'share\edk2-i386-vars.fd') $vars

$fw = (Join-Path $QemuDir 'share\edk2-x86_64-code.fd') -replace '\\', '/'
$isoQ = $Iso -replace '\\', '/'
$varsQ = $vars -replace '\\', '/'
$privQ = $priv -replace '\\', '/'
$qargs = @(
    '-name', 'guest=lightvm-u4', '-no-user-config', '-nodefaults',
    '-machine', 'q35,pflash0=fwcode,pflash1=fwvars', '-accel', 'whpx',
    '-cpu', 'Skylake-Client-v4', '-smp', '2', '-m', '4096', '-nic', 'none',
    '-blockdev', "driver=file,node-name=fwcode,filename=$fw,read-only=on",
    '-blockdev', "driver=file,node-name=fwvars,filename=$varsQ",
    '-blockdev', "driver=file,node-name=isofile,filename=$isoQ,read-only=on",
    '-blockdev', 'driver=raw,node-name=iso,file=isofile,read-only=on',
    '-device', 'ide-cd,drive=iso,bus=ide.0,bootindex=0',
    '-device', 'virtio-vga', '-device', 'virtio-tablet-pci', '-device', 'virtio-keyboard-pci',
    '-display', 'none',
    '-vnc', "unix:$privQ/vnc.sock",
    '-qmp', "unix:$privQ/qmp.sock,server=on,wait=off"
)
$log = Join-Path $Work 'qemu.stderr.txt'
$p = Start-Process -FilePath $qemu -ArgumentList $qargs -PassThru -WindowStyle Hidden -RedirectStandardError $log
Write-Host "QEMU avviato (PID $($p.Id)); attendo i socket..."
$t0 = Get-Date
while (-not ((Test-Path (Join-Path $priv 'vnc.sock')) -and (Test-Path (Join-Path $priv 'qmp.sock')))) {
    if ($p.HasExited) { throw "QEMU uscito (codice $($p.ExitCode)): vedi $log" }
    if (((Get-Date) - $t0).TotalSeconds -gt 30) { Stop-Process -Id $p.Id -Force; throw 'socket non creati entro 30 s' }
    Start-Sleep -Milliseconds 200
}
try {
    $uargs = @('--senza-supervisor', '--vnc-socket', (Join-Path $priv 'vnc.sock'),
        '--qmp-socket', (Join-Path $priv 'qmp.sock'), '--capture-dir', $Work,
        '--tmp-dir', $priv, '--layout', $Layout)
    if ($Smoke -gt 0) {
        $uargs += @('--smoke', "$Smoke", '--dump-fb', (Join-Path $Work 'framebuffer.png'))
    } else {
        Write-Host 'Console aperta: il desktop Ubuntu compare in circa 100 s. Chiudere la finestra al termine.'
    }
    & $ui @uargs
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Write-Host "QEMU terminato. File in $Work"
}
