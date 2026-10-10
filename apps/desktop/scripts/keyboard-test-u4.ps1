<#
.SYNOPSIS
  Test U-4 / P-4 (docs/STATUS.md): physical keyboard -> winit -> egui ->
  supervisor IPC (Input batches) -> QMP, on the F2 product path.

.DESCRIPTION
  Opens lightvm-desktop with a dedicated catalog and the approved QEMU
  distribution. The console goes through the supervisor: per-run display
  ticket for the screen, IPC input batches for keyboard and mouse (since F2
  the UI no longer opens QEMU's VNC or QMP sockets).

  Steps in the window:
    1. Dashboard > New VM: name "u4-keyboard", Ubuntu LTS, Import ISO... and
       choose the Ubuntu ISO (path printed below), defaults for the rest,
       Create. Needs free space for the ISO copy plus the 16 GiB reserve.
    2. Start the VM, then Console. Choose the guest layout (US/IT) in the
       console toolbar, the same as the one active in Ubuntu.
  What to try when the Ubuntu desktop appears (about 100 s):
    1. Open a terminal in the guest (Super key, type "terminal").
    2. Type letters, numbers and symbols.
    3. AltGr: @ # [ ] { } with the Italian layout active in the guest.
    4. Ctrl+C on "sleep 100".
    5. Ctrl+V: the key combination must reach the console, NEVER the text of
       the Windows clipboard.
    6. Numeric keypad (with and without Num Lock), arrows, Esc, F1-F12.
    7. Click outside the window with a key pressed: the key must end up
       released in the guest.
    8. Ctrl+Alt+Enter (full screen) and Ctrl+Alt+Esc (leave): neither must
       reach the guest.
  Write down what does not work: the outcome goes into docs/verification.
  Closing the window with the VM running asks to stop it first.
#>
param(
    [string]$Workspace = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path,
    [string]$QemuDir = '',
    [string]$Iso = '',
    [string]$Work = 'C:\lvmtest\u4'
)
$ErrorActionPreference = 'Stop'
if (-not $QemuDir) { $QemuDir = Join-Path $Workspace 'lab\qemu\qemu-11.1.0' }
if (-not $Iso) { $Iso = Join-Path $Workspace 'lab\media\ubuntu-26.04.1-desktop-amd64.iso' }
foreach ($f in @((Join-Path $QemuDir 'qemu-system-x86_64.exe'), $Iso)) {
    if (-not (Test-Path $f)) { throw "missing file: $f" }
}
# The most recent binary between release and debug.
$ui = @('target\release\lightvm-desktop.exe', 'target\debug\lightvm-desktop.exe') |
    ForEach-Object { Join-Path $Workspace $_ } | Where-Object { Test-Path $_ } |
    Sort-Object { (Get-Item $_).LastWriteTime } -Descending | Select-Object -First 1
if (-not $ui) { throw 'lightvm-desktop not built: cargo build --release -p lightvm-desktop lightvm-service' }

New-Item -ItemType Directory -Force $Work | Out-Null
Write-Host "ISO to import in the wizard: $Iso"
$uargs = @('--catalog', (Join-Path $Work 'catalog'), '--qemu-dir', $QemuDir,
    '--settings', (Join-Path $Work 'ui-settings.json'))
# Release builds use the GUI subsystem: Start-Process -Wait waits for the window.
Start-Process -FilePath $ui -ArgumentList $uargs -Wait -NoNewWindow
Write-Host "Done. Catalog in $Work\catalog (delete it to free the ISO copy)."
