# ADR-0035 — Windows 11 x64 guest on Windows hosts with an emulated TPM 2.0 and Secure Boot

- Status: Accepted (2026-10-15): direction set; project-built QEMU decided after the R6-05 spike
- Date: 2026-10-15
- Owner: project owner (decision); implementation recorded below

## Context

Windows 11 x64 requires UEFI with Secure Boot and TPM 2.0. ADR-0027 profiles stop at Windows 10 on Windows hosts; ADR-0028 covers Windows 11 ARM64 on macOS with `swtpm` and project-built Secure Boot firmware. QEMU attaches a TPM through `-tpmdev emulator` and a `swtpm` process over a socket; `swtpm` has no official Windows build. The project owner chose a real emulated TPM over bypassing the setup checks.

## Decision

- Profile **Windows 11 x64**: q35, OVMF with Secure Boot enabled and Microsoft keys enrolled in the per-VM variable store, `tpm-crb` backed by `swtpm` (TPM 2.0), AHCI disk, the profile's display and input devices.
- `swtpm` (and `libtpms`) built for Windows by the project from pinned sources, or an existing maintained Windows build if one is found and its provenance verifiable; licences inventoried (BSD-3-Clause); state stored per VM in the catalog, part of baselines and snapshots like the firmware variables; the process supervised in the VM's process tree like QEMU.
- The feasibility spike decides the transport available on Windows (QEMU's `-chardev socket` to `swtpm socket --ctrl type=tcp` on loopback with a per-run credential, or a Unix socket if both builds support `AF_UNIX` on Windows) and proves a full Windows 11 installation and boot.
- No setup bypass (`LabConfig`) in the product.

## Required evidence

- `swtpm` build reproducible, Windows 11 x64 installation completed and booted with Secure Boot on and TPM 2.0 reported by the guest; TPM state survives restart, baseline and snapshot revert. **Not verified.**

## References

Roadmap, sections 5, 12 (R2c, R6); ADR-0027, ADR-0028.

## Decision after the feasibility spike (2026-10-15, `R6-05`)

The spike found that no upstream QEMU build for Windows can have a TPM (`meson.build` requires a POSIX host for `CONFIG_TPM`; the emulator backend passes the data channel with `SCM_RIGHTS`, unavailable on Windows), and that the Secure Boot OVMF shipped with QEMU requires SMM, which WHPX does not provide. `swtpm`/`libtpms` 0.10.2 build unpatched with Cygwin and work over loopback TCP. The project owner chose:

- **A project-built QEMU for Windows** for the Windows 11 x64 profile: pinned upstream sources plus a minimal patch set that enables `CONFIG_TPM` on Windows and replaces the descriptor-passing data channel of the `emulator` backend with a second socket (`swtpm --server type=tcp` or a named pipe), with a per-run credential/port protection; reproducible build recipe in the repository, binaries hash-approved like the upstream build (this amends ADR-0010 for this profile only; the other profiles keep the upstream build).
- **Project-built OVMF with Secure Boot and without SMM** (`SECURE_BOOT_ENABLE=TRUE`, `SMM_REQUIRE=FALSE`), Microsoft keys enrolled in the per-VM variable store, as for ADR-0028. Declared limit: without SMM the variable store is writable by code running in the guest kernel, so Secure Boot protects against unsigned boot components, not against a guest already compromised at ring 0.
- `swtpm` built with Cygwin from pinned, signature-verified sources; the Cygwin runtime DLLs shipped with it (licences inventoried).
