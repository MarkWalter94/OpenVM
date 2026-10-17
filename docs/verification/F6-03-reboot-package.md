# F6-03 — Windows package containing the WHPX reboot fix

Date: 2026-10-17. Experimental unsigned ZIP, verified on the development
Windows/WHPX host. Clean-machine qualification remains open.

Host: Windows 11 Pro build 26300, Intel Core i9-14900K, 68,475,281,408
visible RAM bytes, PowerShell 7.6.5, WHPX. Shared development machine;
these timings are not release benchmarks. Environment and external QEMU
firmware hashes are retained in
`E:\OpenVM-tests\package-reboot-20261017\environment.json`.
Ordinary QEMU 11.1.0 executable SHA-256:
`47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`.
Ubuntu source image, synthetic NVMe/Q35 wrapper, guest Tools setup and its
scope limitations are identified in R6-07b.

## Build and identity

Built from a clean detached checkout of
`fa6d90f2734317f26f875f86a523150a7fefb64f`, including `bc15d03` (cold
relaunch of every WHPX guest reset) and Claude's Windows 11 TPM tests.
No shared-checkout working changes were copied into the package.

Artifact:
`E:\OpenVM-tests\package-reboot-20261017\dist\LightVM-0.0.0-alpha.20261017.2-windows-x64.zip`
(19,448,202 bytes).
SHA-256: `d05df3331d15f8f84801aa8b19f9825f823946c355250395ad5d63cabdc83ae3`.
`BUILDINFO.txt` records the clean commit and build settings.

Build procedure:

```powershell
& E:\OpenVM-tests\package-reboot-20261017\source\packaging\windows\build-package.ps1 `
  -Version '0.0.0-alpha.20261017.2' `
  -OutDir E:\OpenVM-tests\package-reboot-20261017\dist `
  -TargetDir E:\OpenVM-tests\package-reboot-20261017\target `
  -ToolsBundle E:\OpenVM-tests\target\debug\tools -NoSetupExe
```

The build uses the explicit MSVC environment, Rust/Cargo 1.99.0,
`x86_64-pc-windows-msvc`, static CRT, path remapping, default release features
and no dev-tools. TEMP/TMP and build output are on E:.
Log: `E:\OpenVM-tests\package-reboot-20261017\build.log`.
CycloneDX SBOM: 257 components; licence policy passed.
The artifact scanner examined all four EXEs, including Tools: no findings.
The two embedded-font notices are separately inventoried and hashed.

## Actual installed package process checks

Ran `E:\OpenVM-tests\package-reboot-20261017\verify-package.ps1`.
Log: `E:\OpenVM-tests\package-reboot-20261017\verify-package.log`.
Extracted files and evidence:
`E:\OpenVM-tests\package-reboot-20261017\verify-0167ed45f58b4543ba4739ffc730302d`.

Verified:

- ZIP SHA-256 and installer payload checksums.
- Installation to a fresh private prefix with `-NoShortcut`, without PATH
  changes; both installed font-notice hashes match the inventory.
- All three product executables successfully run `--help`.
- The actual installed release supervisor starts with a private catalog,
  audit and runtime at `E:\lpkg-94fb3804`, using the ordinary approved QEMU.
  Ready PID and event are checked; credentials stay in memory.
- A test IPC client over the real endpoint rejects an empty credential,
  accepts the issued credential and receives Pong. A second supervisor
  refuses the locked catalog with exit 3 while the first remains active.
- Orderly shutdown exits 0. A fresh process reopens the same catalog,
  accepts its new credential and rejects the previous process's credential;
  orderly shutdown again exits 0.
- Uninstallation removes all manifest-listed files while retaining an
  added user file and preserving the real user PATH.

The IPC probe is a test client outside the ZIP. No credential is passed on
its command line or written to retained logs. No real user catalog is used.

## Installed Linux qualification procedure

The optional `LIGHTVM_INSTALLED_SERVICE_EXE` in the ignored installed-Linux
test selects a real external supervisor instead of the in-process service.
It checks the ready PID, retains only PID/binary hash, uses the authenticated
IPC protocol and shuts down the owned child explicitly. The source fixture
and scenario are described in `R6-07b-installed-linux-tools.md`.

Reproduction after extracting this ZIP:

```powershell
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_LAB = 'C:\lvlinux-20261017'
$env:LIGHTVM_E2E_QEMU_DIR = 'E:\OpenVM-tests\lab\qemu\qemu-11.1.0'
$env:LIGHTVM_INSTALLED_LINUX_FIXTURE = 'E:\OpenVM-tests\lab\installed-linux\fixture-20261017'
$env:LIGHTVM_INSTALLED_SERVICE_EXE = 'E:\OpenVM-tests\package-reboot-20261017\verify-0167ed45f58b4543ba4739ffc730302d\LightVM-0.0.0-alpha.20261017.2-windows-x64\lightvm-service.exe'
& E:\OpenVM-tests\with-msvc.ps1 @('test', '-p', 'lightvm-service',
  '--features', 'supervisor', '--test', 'f4_real',
  'installed_linux_guest_warm_reboot', '--locked',
  '--', '--ignored', '--nocapture', '--test-threads=1')
```

The first attempt failed before starting the packaged service: dropping
the preparatory QemuHost (which owns a runtime) inside an async context
panicked. This was a test-harness defect, corrected by releasing it before
entering the async block; no product code changed. Failed log:
`E:\OpenVM-tests\package-reboot-20261017\guest-harness-failure.log`.
Its small evidence was preserved and disposable C: root removed before retry.
Clippy with warnings denied passed after the correction.

### Result — actual ZIP supervisor, passed

Retry: **1 passed, 0 failed, 363.18 s**.
Log: `E:\OpenVM-tests\package-reboot-20261017\guest-release-reboot.log`.
Permanent evidence:
`E:\OpenVM-tests\package-reboot-20261017\guest-evidence\e39a36d24fec`.
The supervisor executable hash recorded by the harness matches the extracted
ZIP binary:
`ed017cd0ec6803a791643728edc043c8a6fe3a542328ce81fbab9b6af4d2c93d`.

Verified source upload, installed Tools, guest compilation/execution,
artifact/log return and checksum, clean stop/start, offline snapshot restore,
baseline guard protecting existing snapshots, explicit test snapshot deletion
and baseline restore. The source VMDK hash remains unchanged.
Returned application A SHA-256:
`c667f8d2c3523868ca783f0d0e7df984049129568e117013e79777f255cecca8`.

Both guest-requested reboots retained run
`01a14a1b-200d-7c4e-8c61-1b124dd40644`, with no vCPU halt, reconnected Tools
and unchanged application state/hash. New boot IDs:

- `f778d98a-48cb-4e03-bfd8-f68fd642ca82`
- `ffd4b3c0-917e-4696-9cc4-c3288c8f2305`

The actual release service records both cold relaunches and final
`shutdown completed`, then exits successfully. Retained QEMU logs contain
no XSAVE/Unexpected-VP-exit fault. No owned service or QEMU remained running.

All 21 preserved evidence-file hashes were checked against
`guest-evidence\SHA256-MANIFEST.json`. The authorized disposable root
`C:\lvlinux-20261017` was removed: 1,917,298,372 logical file bytes.
Cleanup record: `guest-evidence\CLEANUP.json`; log: `guest-cleanup.log` under
the package root. This qualifies the actual release supervisor for this
headless fixture, extending the earlier in-process-only R6-07b evidence.

## Limits

This does not qualify another host, a clean machine, desktop interaction,
Linux desktop autostart/mapping, Windows Tools or USB. Windows 11 work
remains independently recorded by Claude. Inno Setup was skipped; the
PowerShell ZIP installer is the tested path. QEMU, swtpm, firmware and guest
images are external, not bundled. Runtime licence/source closure, signing
and independent review remain open. Earlier package F6-02 is preserved as
historical evidence and predates the WHPX reboot fix.

Subsequent qualification with the **same ZIP supervisor** is recorded in
`R6-07c-linux-desktop.md`: installed Xfce/X11 autostart, GVfs DAV file
operations and text/RGBA clipboard on two boots. Its specific scope and
shutdown limitation extend the evidence without changing this artifact.
