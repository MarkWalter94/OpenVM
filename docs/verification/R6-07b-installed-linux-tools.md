# R6-07b — Installed Linux Tools and offline checkpoints

Date: 2026-10-17. **Passed: installed Tools, application transfer/build,
cold restart, snapshot/baseline restore and two consecutive guest-requested
reboots. The WHPX reboot fault is resolved by the service's automatic cold
relaunch of the same run (`bc15d03`).**

Prepared a separate installed Ubuntu Server 24.04 disk fixture and an ignored
test through production service IPC. Windows 11 remains owned by Claude;
none of its disks, processes or files were used or stopped by this work.
The Codex files accidentally included in Claude's commit `6f94db7` were
compared with the current intended contents; no differences were found in
the filtered-network test or font-packaging changes.

## Prepared fixture

Downloaded the Canonical Ubuntu cloud image, release 20260926, from its
[official directory](https://cloud-images.ubuntu.com/releases/noble/release-20260926/).
SHA-256 agrees with the
[HTTPS checksum list](https://cloud-images.ubuntu.com/releases/noble/release-20260926/SHA256SUMS):
`6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2`.
This check did not independently validate the detached GPG signature.

The preparer `tests/common/prepare_installed_linux.py` creates a disposable
VMDK/VMX wrapper (NVMe/Q35, 2 vCPUs, 3072 MiB) and NoCloud ISO in a new
directory on E:. This is a
preinstalled Ubuntu disk with a synthetic VMware wrapper, not a real VMware
installation. `qemu-img compare` reports identical disk content after
conversion; log `E:\OpenVM-tests\installed-linux-disk-compare.log`.

Fixture: `E:\OpenVM-tests\lab\installed-linux\fixture-20261017`.
Original download: `E:\OpenVM-tests\lab\media\ubuntu-cloud-20260926`.
The host-only ISO preparer uses pycdlib 1.22.0 installed under
`E:\OpenVM-tests\python-tools`; the guest requires no package download.
Python syntax and ISO read-back checks verified that the NoCloud files and
embedded guest probe match their sources.

The seed uses the shipped Python Tools `--install` once, then a lab systemd
unit launches the installed copy on subsequent boots with the current
run's configuration mounted from the Tools ISO. A separate guest fixture
accepts a small set of commands through the Tools inbox. This headless
service wrapper is test setup, not a shipped Linux service installer.
It does not qualify desktop-session autostart, clipboard or UI inspection.

## Real test

`installed_linux::installed_linux_tools_build_restart_snapshot_and_baseline`
is included by `tests/f4_real.rs`. The passed run checks:

1. Import through service IPC, offline networking with only the Tools
   channel, and installation of the shipped Python Tools.
2. Upload application source through Tools, compile/build/execute inside
   the guest and return artifact/log with SHA-256 verification on the host.
3. Clean guest poweroff, an offline snapshot and a verified baseline of A.
4. Normal boot preserving A, mutation to B, then snapshot restore to A.
5. Refusal to restore a baseline while a snapshot exists, then explicit
   deletion of only the already verified test snapshot.
6. Baseline restore to A, including the original application hash, followed
   by clean shutdown. The source VMDK hash remains unchanged. Each normal
   boot has a different Linux boot ID and uses the persistent installed Tools.

Fresh nonce replies distinguish a new guest boot from old output files
restored by a checkpoint. Returned guest applications are never executed
on the host. Hashing the source disk uses a bounded buffer.

## Results and space management

- Clippy for `lightvm-service`, `supervisor`, `--test f4_real`, with warnings
  denied: passed (`E:\OpenVM-tests\installed-linux-clippy-c-final.log`).
- Rust formatting, Python syntax, NoCloud ISO read-back and disk-content
  comparison: passed.
- Initial real test: **failed before VM import/boot**. The production
  service returned `resource_limit`: free E: storage was below its 16 GiB
  reserve, giving a zero maximum disk size. Log:
  `E:\OpenVM-tests\installed-linux-real.log`.
- Initial harness preflight: **failed before source hashing/import/boot**,
  reporting 10 GiB free. The test requires at least 26 GiB free (16 GiB
  production reserve plus 10 GiB for its disposable disk/checkpoints).
  Log: `E:\OpenVM-tests\installed-linux-final-preflight.log`.
- No reserve or product admission check was reduced. The failed attempt
  left no installed-Linux test VM running. Windows processes were untouched.
- After the owner's explicit temporary-C: authorization, the complete
  test **passed: 1 passed, 290.77 seconds**. Log:
  `E:\OpenVM-tests\installed-linux-c-final2.log`.
- Final evidence: `E:\OpenVM-tests\installed-linux-evidence-20261017\0c7604022702`.
  Includes the audit, four fresh boot/status replies, mutation B, returned
  application, build log, results and QEMU logs. Artifact A SHA-256:
  `a7da48e82674b06809d75aa28c31068bfbde346eadf6c658eaab27606854a91f`.
- All Codex Linux catalogs, disks and private sockets under
  `C:\lvlinux-20261017`, including diagnostic attempts, were removed after
  evidence preservation. The directory no longer exists. Removed logical
  file bytes: 15,202,951,232 (about 14.16 GiB). Cleanup record and evidence
  checksum manifest are in `E:\OpenVM-tests\installed-linux-evidence-20261017`;
  cleanup log: `E:\OpenVM-tests\installed-linux-c-cleanup.log`.

Automatic approval review rejected removal of Codex's disposable package
build cache at `E:\OpenVM-tests\package-20261017\target` with the generic
reason `blocked by policy`. The cache was left intact. No alternative
deletion or removal of Claude's artifacts was attempted.

## Guest-requested reboot: fix and real verification

Claude's commit `bc15d03` extends the WHPX reset policy to every WHPX run.
QEMU receives `-action reboot=shutdown`; the service observes the reset exit
and starts fresh QEMU processes on the same disk/firmware, retaining the
run, network policy and Tools. This avoids the failing in-process WHPX
reset; it does not repair WHPX's internal warm-reset implementation.

`installed_linux_guest_warm_reboot` now performs the entire application,
restart and checkpoint scenario followed by **two consecutive guest-requested
reboots** on the restored baseline overlay. Each reboot requires a new Linux
boot ID, the same service RunId, running state with no vCPU halt, Tools
reconnection and the original application state/hash. Final clean shutdown
and an unchanged source VMDK are also required.

Actual run at `bc15d03`: **1 passed, 359.20 seconds**.
Log: `E:\OpenVM-tests\installed-linux-reboot-final.log`.
Evidence: `E:\OpenVM-tests\installed-linux-reboot-evidence-20261017\62134a946ec4`.
Run: `01a14914-85e2-7a43-b28e-77f09c200ef1` throughout both reboots.
Boot IDs after them:

- `62da9ad3-1ab6-45e3-90f8-dd9c2c30a6ff`
- `33b2c339-498a-4449-a751-9b1e2124ed9b`

Retained QEMU stderr logs contain none of the prior XSAVE/VP-exit faults.
Clippy with warnings denied passed (`installed-linux-reboot-clippy.log`).
Evidence was preserved and verified; the temporary C: root was removed
again (1,916,165,998 logical file bytes). Cleanup log:
`E:\OpenVM-tests\installed-linux-reboot-cleanup.log`; record and checksum
manifest: `E:\OpenVM-tests\installed-linux-reboot-evidence-20261017`.
The test remains ignored by default because it requires real WHPX and
the explicitly prepared guest image, not because of a known failing reboot.

No additional production runtime edits were made by Codex over Claude's
fix. The older experimental ZIP in F6-02 predates this fix; these results
qualify the current source checkout.

## Earlier diagnostics and scope limits

The original synthetic IDE/i440FX fixture did not expose the secondary
CD to this Linux guest, so cloud-init did not configure it. Moving the CD
from slave to master did not fix detection; that attempted builder change
was fully withdrawn. The passed fixture uses NVMe/Q35. No generic QEMU
command-line change is retained.

An earlier complete scenario reached installed Tools, normal restart and
snapshot restore, then a guest-requested warm reboot caused `failed to get
xsave state` and `WHPX: Unexpected VP exit code 4`. The watcher attempted
three resets and left the vCPUs halted. Diagnostic log:
`E:\OpenVM-tests\installed-linux-c-run4.log`; preserved QEMU stderr and audit:
`E:\OpenVM-tests\installed-linux-evidence-20261017\25f8f8c36390`.
That halted diagnostic test was terminated explicitly; it is not a pass.

That earlier failure predates `bc15d03` and is superseded by the real reboot
verification above, using the same ordinary QEMU binary and Linux fixture.
Claude's Windows/QEMU files and development bundle were not modified.

The runs above qualify a preinstalled headless Ubuntu 24.04.5 lab image through an
in-process production service harness. It does not qualify desktop login
autostart, clipboard, UI inspection, installation from a desktop ISO or a
packaged supervisor process, and does not expand supported guest versions.

## Reproduce with an authorized disposable catalog volume

```powershell
$env:TEMP = 'E:\OpenVM-tests\tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'E:\OpenVM-tests\target'
$env:LIGHTVM_LAB = 'C:\lvlinux-20261017'
$env:LIGHTVM_E2E_QEMU_DIR = 'E:\OpenVM-tests\lab\qemu\qemu-11.1.0'
$env:LIGHTVM_INSTALLED_LINUX_FIXTURE = 'E:\OpenVM-tests\lab\installed-linux\fixture-20261017'
& E:\OpenVM-tests\with-msvc.ps1 @('test', '-p', 'lightvm-service',
  '--features', 'supervisor', '--test', 'f4_real',
  'installed_linux_tools_build_restart_snapshot_and_baseline', '--locked',
  '--', '--ignored', '--nocapture', '--test-threads=1')
```

Packaged Tools must be staged beside the test executable on its physical
E: path, as in R1-02. The seed was refreshed with the final guest fixture;
it may only be refreshed while its VM is stopped. The catalog volume must
have at least 26 GiB free. Preserve the small evidence files and remove the
disposable C: root again after the test; the shipped source fixture stays on E:.
Use filter `installed_linux_guest_warm_reboot` instead to include the two
guest-requested reboot checks as well as the full checkpoint scenario.

## Follow-up — actual packaged supervisor (2026-10-17)

`F6-03-reboot-package.md` records a further complete run against the actual
release supervisor extracted from the clean-source experimental ZIP
`0.0.0-alpha.20261017.2` (`fa6d90f`, includes `bc15d03`). **Passed in 363.18 s**:
installed Tools, guest application build/log/checksum, stop/start, offline
snapshot/baseline restore and two guest-requested reboots in the same run.
The release binary hash, run and boot IDs, first test-harness failure, fixed
retry and evidence hashes are recorded there. This supersedes the packaged-
supervisor exclusion above for this headless fixture. Desktop/autostart,
mapping, Windows Tools and clean-machine qualification remain open.

Select an extracted release binary with `LIGHTVM_INSTALLED_SERVICE_EXE`;
otherwise the test retains its original in-process mode. The ready credential
stays in memory. The owned child is shut down explicitly and guarded on failure.
Temporary C: test data was removed again after preserving and verifying the
evidence under `E:\OpenVM-tests\package-reboot-20261017\guest-evidence`.
