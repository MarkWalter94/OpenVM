# F6-04 — Experimental ZIP with automatic Linux Tools CD mounting

Date: 2026-10-17. Supersedes alpha.20261017.2 for the Linux startup fix;
the earlier package and its verification remain available.

## Artifact and build

`E:\OpenVM-tests\package-login-20261017\dist\LightVM-0.0.0-alpha.20261017.3-windows-x64.zip`
(19,448,994 bytes), SHA-256:
`4fee43a4a56b603cc4bacbaaa8ff96af3db55e3791fb64fb24797b40c6825f84`.

Clean source commit `14eb7033f09efbeeecbe9ca9e3e2bf84c83c74cd`.
The isolated checkout at `E:\OpenVM-tests\package-reboot-20261017\source`
was checked clean and moved to this commit. Reused its release target cache,
without modifying the shared working tree or another agent's files.

```powershell
& E:\OpenVM-tests\package-reboot-20261017\source\packaging\windows\build-package.ps1 `
  -Version '0.0.0-alpha.20261017.3' `
  -OutDir E:\OpenVM-tests\package-login-20261017\dist `
  -TargetDir E:\OpenVM-tests\package-reboot-20261017\target `
  -ToolsBundle E:\OpenVM-tests\desktop-login-20261017\runtime\tools -NoSetupExe
```

Default release features, static CRT, path remapping, no dev-tools.
Build log: `E:\OpenVM-tests\package-login-20261017\build.log`.
SBOM: 257 components, licence policy passed. Artifact scanner: four EXEs,
no findings. `BUILDINFO.txt` identifies the clean source and unsigned status.
Both manual and full-automatic Windows Sandbox configurations were generated.

## Verification

`E:\OpenVM-tests\package-login-20261017\verify-package.ps1` passed;
log: `verify-package.log` in that root. Extracted evidence:
`verify-231ce45bcf8e4a3d84f7a8e239c4fe42`.

Verified ZIP and installed-payload checksums, embedded-font notices, all
three product CLI launches, real installed supervisor authentication/Ping,
catalog lock refusal, orderly shutdown/restart and rejection of the old
credential. Uninstallation removed all manifest payload files and retained
the added user file; the real user PATH remained unchanged. Private service
scratch was `E:\lpkg-a9641b8a`; no real user catalog was used.

The verifier checks that the extracted components match the exact bytes
qualified by the real Linux test in `R6-07d-desktop-login.md`:

| Component | SHA-256 |
| --- | --- |
| Supervisor | `ed017cd0ec6803a791643728edc043c8a6fe3a542328ce81fbab9b6af4d2c93d` |
| Python Tools | `c707fe2b330954894b6f7184a464c7c12829faf1e91e9007ffc384a8425fe2a7` |
| Windows Tools EXE (unchanged) | `b754a90539a1989ebda0bf0e7c7886812c347036015154240be8f0d9524753bf` |

That test passed complete LightDM/Xorg login, read-only Tools CD mounting
as UID 1000 without a root mount helper, GVfs/clipboard on two boots and
two clean ACPI shutdowns in 141.23 s. Its exact tested components were
assembled into this ZIP; the full VM scenario was not repeated from a
second extraction. Guest/host evidence hashes and C: cleanup are in R6-07d.

## Limits

Unsigned experimental ZIP, not a supported release; Inno Setup was skipped.
QEMU/swtpm/firmware and guest disks remain external. Windows Sandbox is not
installed, so the generated clean-machine procedure was not executed.
GNOME/Wayland, installed Windows Tools, native drag, WPF, USB and independent
review/signing remain separate work. The Linux change neither enables host
Windows features nor modifies physical USB devices or another VM.
