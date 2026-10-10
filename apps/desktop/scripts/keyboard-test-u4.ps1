<#
.SYNOPSIS
  Test U-4 / P-4 (docs/STATUS.md): physical keyboard -> winit -> egui -> QMP.

.DESCRIPTION
  Starts QEMU (WHPX, CPU Skylake-Client-v4, Ubuntu live ISO) with VNC console
  and QMP on Unix sockets in a private directory (ACL current user only), then
  opens lightvm-desktop on the real console (F0-04 spike path: the UI talks
  directly to QMP; in the product the console will go through the supervisor
  in F2). When the window is closed QEMU is terminated.

  What to try when the Ubuntu desktop appears (about 100 s):
    1. Open a terminal in the guest (Super key, type "terminal").
    2. Type letters, numbers and symbols: the guest has the US keyboard,
       unless the layout is changed in Ubuntu; with -Layout it the UI maps the
       IT keys.
    3. AltGr: @ # [ ] { } with the Italian layout active in the guest.
    4. Ctrl+C on a running command (e.g. "ping 1.1.1.1" fails immediately
       without network: use "sleep 100" and then Ctrl+C).
    5. Ctrl+V: the key combination must reach the console, NEVER the text of
       the Windows clipboard.
    6. Numeric keypad (with and without Num Lock), arrows, Esc, F1-F12.
    7. Click outside the window with a key pressed: the key must end up
       released in the guest.
  Write down what does not work: the outcome goes into docs/verification.

.PARAMETER Smoke
  If > 0, closes the UI after N seconds and saves the framebuffer (check
  without a person at the keyboard).
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
foreach ($f in @($qemu, $Iso)) { if (-not (Test-Path $f)) { throw "missing file: $f" } }
# The most recent binary between release and debug.
$ui = @('target\release\lightvm-desktop.exe', 'target\debug\lightvm-desktop.exe') |
    ForEach-Object { Join-Path $Workspace $_ } | Where-Object { Test-Path $_ } |
    Sort-Object { (Get-Item $_).LastWriteTime } -Descending | Select-Object -First 1
if (-not $ui) { throw 'lightvm-desktop not built: cargo build --release -p lightvm-desktop' }

# Private directory with ACL for the current user only (VNC and QMP sockets).
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
Write-Host "QEMU started (PID $($p.Id)); waiting for the sockets..."
$t0 = Get-Date
while (-not ((Test-Path (Join-Path $priv 'vnc.sock')) -and (Test-Path (Join-Path $priv 'qmp.sock')))) {
    if ($p.HasExited) { throw "QEMU exited (code $($p.ExitCode)): see $log" }
    if (((Get-Date) - $t0).TotalSeconds -gt 30) { Stop-Process -Id $p.Id -Force; throw 'sockets not created within 30 s' }
    Start-Sleep -Milliseconds 200
}
try {
    $uargs = @('--no-supervisor', '--vnc-socket', (Join-Path $priv 'vnc.sock'),
        '--qmp-socket', (Join-Path $priv 'qmp.sock'), '--capture-dir', $Work,
        '--tmp-dir', $priv, '--layout', $Layout)
    if ($Smoke -gt 0) {
        $uargs += @('--smoke', "$Smoke", '--dump-fb', (Join-Path $Work 'framebuffer.png'))
    } else {
        Write-Host 'Console open: the Ubuntu desktop appears in about 100 s. Close the window when done.'
    }
    & $ui @uargs
} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Write-Host "QEMU terminated. Files in $Work"
}
