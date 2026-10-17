# R6-07d — Linux desktop login, automatic Tools CD and clean ACPI shutdown

Date: 2026-10-17. Follow-up to R6-07c. Real WHPX VM with a LightDM/Xorg
session and updated production Linux Tools. GNOME/Wayland remains separate.

## Change and cause

The installed Python Tools previously searched only already mounted media.
In a real Xfce login the installed process started correctly as UID 1000,
but its private CD was not mounted and the control channel never connected.
The lab no longer mounts that CD from a privileged helper.

Tools now request a read-only mount through the
[UDisks2 Filesystem D-Bus API](https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Filesystem.html)
using `gdbus`. Only the fixed label `LIGHTVM_TOOLS` resolving to a block
device `/dev/srN` is eligible; hard disks, USB storage partitions and regular
files are excluded. The request sets `ro` and
[`auth.no_user_interaction`](https://storaged.org/doc/udisks2-api/latest/udisks-std-options.html).
It uses the existing desktop user's permission, no shell/sudo or authentication
dialog, with an 8-second timeout and at least 30 seconds between attempts.
An explicit `--config` retains the original behavior. Missing support or
denied policy leaves manual CD opening available.

The earlier Xvfb/runuser fixture did not qualify ACPI shutdown. The new
fixture uses a complete local desktop login with LightDM, Xorg and ACPI
services; no production QEMU shutdown behavior or deadline was changed.
This result does not establish which individual missing service caused the
old artificial session's fallback.

## Environment and exact components

Same Windows/WHPX host, approved ordinary QEMU and Ubuntu base as R6-07c/F6-03.
The preparation fixture is selected with `prepare_desktop_linux.py --session
lightdm`. It installs LightDM/Xorg, Xfce, Thunar/GVfs, UDisks2, polkit and
ACPI support in its own disposable VM. It uses the shipped installer to
prepare the Tools copy in the locked-password disposable desktop account;
normal XDG startup then launches it after LightDM auto-login.
There is no root Tools daemon, Xvfb or lab mount of `LIGHTVM_TOOLS`.
General Thunar automount is deliberately disabled in the final fixture so
that its success depends on the Tools request, not file-manager policy.

The real supervisor binary is unchanged from alpha.20261017.2, SHA-256
`ed017cd0ec6803a791643728edc043c8a6fe3a542328ce81fbab9b6af4d2c93d`.
A private runtime copy is staged at
`E:\OpenVM-tests\desktop-login-20261017\runtime`, with the updated Tools
script and its checksum. This is an updated component bundle, **not** an
unmodified alpha.20261017.2 ZIP. Python Tools SHA-256:
`c707fe2b330954894b6f7184a464c7c12829faf1e91e9007ffc384a8425fe2a7`.
The Windows Tools EXE is unchanged.

## Procedure

Prepare the separate fixture as in R6-07c, adding `--session lightdm`.
The preparation runner and all logs are retained at
`E:\OpenVM-tests\desktop-login-20261017`. Guest NAT is used for package
preparation only; qualification uses Offline plus production Tools channels.

```powershell
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_DESKTOP_LAB = 'C:\lvdesktop-20261017'
$env:LIGHTVM_DESKTOP_EXPECT_LOGIN = '1'
$env:LIGHTVM_E2E_QEMU_DIR = 'E:\OpenVM-tests\lab\qemu\qemu-11.1.0'
$env:LIGHTVM_INSTALLED_SERVICE_EXE = 'E:\OpenVM-tests\desktop-login-20261017\runtime\lightvm-service.exe'
& E:\OpenVM-tests\with-msvc.ps1 @('test', '-p', 'lightvm-service',
  '--features', 'supervisor', '--test', 'f4_real',
  'installed_xfce_autostart_gvfs_and_clipboard', '--locked',
  '--', '--ignored', '--nocapture', '--test-threads=1')
```

In addition to all R6-07c file/clipboard checks on two boots, strict mode
requires `Active=yes`, `Remote=no`, `Type=x11`, `Seat=seat0`, a mounted Tools
CD and both stops reporting `Powerdown/GuestShutdown`, with no Quit/Kill
fallback. The status records input devices, active services and logind
diagnostics. Source VMDK hashes must match before and after the test.

## Result — passed

**1 passed, 0 failed, 141.23 s**. Log:
`E:\OpenVM-tests\desktop-login-20261017\desktop-test.log`.
Evidence: `E:\OpenVM-tests\desktop-login-20261017\evidence\8b544ccc64a2`.
Both boots passed every strict login, CD mount, GVfs and clipboard check.
The kernel mount record identifies `/dev/sr1` at
`/media/desktop/LIGHTVM_TOOLS`, `ro`, UID/GID 1000. Both normal stops were
`phase Powerdown, cause GuestShutdown`: 931.2264 ms and 1.0617202 s.
There was no Quit/Kill fallback. The supervisor shut down and the source
VMDK hash remained unchanged. These shared-host timings are not benchmarks.

All 11 guest component tests passed in WSL Ubuntu, including optical-only,
read-only, noninteractive mounting, missing support and timeout handling.
Clippy with warnings denied, owned Rust formatting and Python syntax checks
passed. No native Windows Tools binary changed.

All 27 preserved evidence hashes were verified. The temporary C: root was
removed after all owned processes stopped, 12,445,048,858 logical file bytes.
Records: `evidence\SHA256-MANIFEST.json`, `evidence\CLEANUP.json`,
`fixture-manifest.json` and `cleanup.log` under the new E: lab root.
A separate 57 KB Python cache in the repository was left intact after
automatic approval review rejected its removal (`blocked by policy`);
it is excluded from the changes and does not contain VM disks.

## Earlier attempts and limits

The first LightDM attempt was stopped after diagnosis: the lab's Thunar
automount XML used flat booleans instead of nested `/.../enabled` properties.
After correcting the fixture, the CD was still not mounted at login; the
second attempt was stopped and retained as well. Both already had a real
active local graphical session and an unprivileged installed Tools process.
Neither is reported as passed. Their logs are `desktop-test.log.attempt1`
and `.attempt2`, with read-only guest diagnostics retained alongside them.
The production mounting change above addresses the bootstrap wait itself.

No host configuration, Windows/TPM VM or physical USB device was changed.
The test does not qualify GNOME/Wayland or a human following the installer
UI. Optional desktop services must already be available; denied mount
authorization is not bypassed. The previous ZIP remains historical evidence.

## Packaged follow-up

`F6-04-linux-mount-package.md` records the new clean-source experimental ZIP
alpha.20261017.3, containing these exact tested service and Tools bytes.
Actual install/service-process/uninstall checks passed on the development
host. It remains unsigned; Windows clean-machine qualification is still open.
