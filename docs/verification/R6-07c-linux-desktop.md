# R6-07c — Installed Xfce Tools autostart, GVfs and X11 clipboard

Date: 2026-10-17. Real Ubuntu VM through the actual release supervisor.
This qualifies the specific Xfce/X11 lab session below, not every Linux
desktop, a display-manager login or Windows Tools.

## Environment and fixture

Host and supervisor are those of `F6-03-reboot-package.md`: Windows 11 Pro
build 26300, Intel Core i9-14900K, WHPX, ordinary approved QEMU 11.1.0.
The actual extracted alpha.20261017.2 supervisor has SHA-256
`ed017cd0ec6803a791643728edc043c8a6fe3a542328ce81fbab9b6af4d2c93d`.
No Tools or additional software was installed on the host, and no optional
Windows feature was enabled.
Claude's Windows/TPM disks, processes and source files were untouched.

`tests/common/prepare_desktop_linux.py` creates only
`C:\lvdesktop-20261017\fixture`. It checks the official Ubuntu cloud image
hash recorded in R6-07b, creates a new 12 GiB virtual overlay and a NoCloud
preparation ISO. A separate ordinary-QEMU VM with NAT downloads Ubuntu
packages, then shuts down. `qemu-img convert` produces a self-contained VMDK
and synthetic NVMe/Q35 VMware wrapper, 2 vCPUs and 4096 MiB. The original
cloud disk is a read-only backing file, never booted for writes.

Installed package versions:

| Package | Version |
| --- | --- |
| xfce4-session | 4.18.3-1build2 |
| gvfs-backends | 1.54.4-0ubuntu1~24.04.4 |
| xclip | 0.13-3 |
| python3-pil | 10.2.0-1ubuntu1.3 |
| xvfb | 2:21.1.12-1ubuntu1.8 |

The preparation command and exact fixed-argument QEMU runner/logs are
retained under `E:\OpenVM-tests\desktop-20261017`.

```powershell
$env:PYTHONPATH = 'E:\OpenVM-tests\python-tools'
& C:\Python312\python.exe crates\lightvm-service\tests\common\prepare_desktop_linux.py `
  --root C:\lvdesktop-20261017\fixture `
  --image E:\OpenVM-tests\lab\media\ubuntu-cloud-20260926\ubuntu-24.04-server-cloudimg-amd64.img `
  --qemu E:\OpenVM-tests\lab\qemu\qemu-11.1.0
& E:\OpenVM-tests\desktop-20261017\prepare-vm.ps1
```

Qualification uses **Offline** networking plus production Tools channels;
guest Internet is used only for preparing the isolated fixture.
The lab mounts the Tools CD read-only, starts Xvfb with TCP disabled, then
runs a real D-Bus/Xfce session as UID 1000. The shipped `INSTALL.SH` installs
Tools in that account; its initial process is stopped before Xfce starts.
Only the installed XDG `.desktop` entry launches Tools thereafter, with no
explicit `--config` argument. A separate lab XDG-autostart probe observes
the installed process and exercises desktop APIs. No root Tools service
or direct test launcher substitutes for Tools autostart.

## Procedure and checks

`installed_linux::desktop_linux::installed_xfce_autostart_gvfs_and_clipboard`
is ignored by default. It selects the actual extracted service via
`LIGHTVM_INSTALLED_SERVICE_EXE`, starts a private catalog and imports the
prepared fixture. Only a private test folder is shared, under two names:
`Builds` writable and `ReadOnly` read-only.

```powershell
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_DESKTOP_LAB = 'C:\lvdesktop-20261017'
$env:LIGHTVM_E2E_QEMU_DIR = 'E:\OpenVM-tests\lab\qemu\qemu-11.1.0'
$env:LIGHTVM_INSTALLED_SERVICE_EXE = 'E:\OpenVM-tests\package-reboot-20261017\verify-0167ed45f58b4543ba4739ffc730302d\LightVM-0.0.0-alpha.20261017.2-windows-x64\lightvm-service.exe'
& E:\OpenVM-tests\with-msvc.ps1 @('test', '-p', 'lightvm-service',
  '--features', 'supervisor', '--test', 'f4_real',
  'installed_xfce_autostart_gvfs_and_clipboard', '--locked',
  '--', '--ignored', '--nocapture', '--test-threads=1')
```

Checks on each of two cold boots:

1. Fresh nonce/status over authenticated Tools; a single installed Tools
   process, same unprivileged UID as the Xfce session, no bootstrap argument.
2. Native GIO/GVfs DAV mount, create directory, upload/read exact UTF-8 file
   bytes, accented filename containing a space, rename, replacement, read
   through the read-only alias, refused writes/deletion through that alias,
   removal of test content and unmount. The host confirms no file remains.
3. Actual X11 clipboard text and Pillow/RGBA image set/get inside the guest;
   image returned over production clipboard RPC and checked by SHA-256.
   A new host-supplied Unicode/multiline text value is written/read through
   the service clipboard API. This is not a Windows clipboard test.
4. Ordinary VM stop, fresh boot and all checks again. Final owned service
   shutdown and source VMDK hash equality.

GIO exercises the desktop virtual filesystem, rather than a direct HTTP
probe. See the [GIO overview](https://docs.gtk.org/gio/overview.html) for its
relationship with GVfs and application file APIs.

## Result — passed

Final run: **1 passed, 0 failed, 353.43 s**. Log:
`E:\OpenVM-tests\desktop-20261017\desktop-test.log`.
Evidence: `E:\OpenVM-tests\desktop-20261017\evidence\4efa16d48f3e`.
Both boots passed every autostart, GVfs and clipboard check listed above.
Boot IDs:

- `52fe2925-c70c-4345-9620-6b818a48afda`
- `e977a401-2a54-47f2-aa49-2113d98f72d4`

Each had exactly one installed `python3 tools.py` process as UID 1000.
Both returned RGBA clipboard SHA-256
`fe3623f8e135251ca64eda9a2e0772c22d7bf0866da0dd5abd199b5614c05db9`.
The source VMDK hash was unchanged; the supervisor completed shutdown and
no owned QEMU/service remained. Clippy with warnings denied, formatting
of the owned Rust modules and Python syntax checks passed.

All 22 preserved evidence-file hashes were verified against
`evidence\SHA256-MANIFEST.json`; fixture/source hashes are in
`fixture-manifest.json`. The entire disposable C: root was removed,
14,436,240,076 logical file bytes. Record: `evidence\CLEANUP.json`;
log: `cleanup.log` under the desktop lab root. No VM disk is retained on C:.

## Earlier failure

The first run passed actual Xfce Tools autostart but the lab probe tried
to mount both share URLs separately. GVfs reuses the DAV authority mount
and returned `Location is already mounted` for the second URL. The probe
was corrected to mount/unmount once and operate on both share paths. A
nonexistent `gio copy --overwrite` option was also removed before retry;
GIO copy overwrites by default unless interactive mode is selected.
The [GLib copy implementation](https://github.com/GNOME/glib/blob/2.80.0/gio/gio-tool-copy.c)
defines that default; this is not a WebDAV server workaround.
The stopped preparation fixture was refreshed, not a running VM's media.
Log: `E:\OpenVM-tests\desktop-20261017\desktop-test.log.attempt1`.
No product change was needed for these probe corrections.

## Limits

Xfce runs on Xvfb in a lab-started user session; the test does not drive a
display-manager login, physical display, Thunar GUI or host keyboard.
The Tools CD is already mounted by lab setup; automatic desktop mounting
of the CD after login remains unqualified. No GNOME/Wayland claim follows
from X11/xclip, and no Windows installed-Tools claim follows from this VM.
The guest installs packages from Ubuntu repositories during preparation;
this is separate from the offline qualification. USB remains deferred.
Windows clean-machine qualification remains open because Windows Sandbox
is absent; the strengthened ready-to-run procedure is recorded in F2-03.

This Xfce fixture did not power off in response to the ordinary ACPI stop
within the service's 60-second graceful-stop deadline. The service used
QMP Quit (`phase Quit, cause QmpQuit`) and the next boot recovered normally.
The two-boot Tools checks do not qualify clean guest shutdown. No production
shutdown behavior was changed to hide or shorten this fallback.

## Follow-up — complete desktop session

`R6-07d-desktop-login.md` records a later LightDM/Xorg session with active
local login, automatic read-only CD mounting by the updated production
Tools and two clean ACPI shutdowns. That extends these earlier Xvfb results;
the fallback above remains the recorded outcome for the older lab session.
