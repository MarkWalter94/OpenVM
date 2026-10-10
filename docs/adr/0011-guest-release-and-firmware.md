# ADR-0011 — Guest release and firmware

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

The MVP has a single reference guest: Ubuntu LTS x86_64, with version and checksum pinned in F0 (roadmap, section 4). The baseline model includes the writable firmware state (UEFI NVRAM) (section 8). The demonstration test is graphical (section 9), so the guest must have a desktop environment.

## Decision

**Reference guest**

| Item | Value |
| --- | --- |
| Release | Ubuntu 26.04.1 LTS (Resolute Raccoon), desktop amd64 variant |
| File | `ubuntu-26.04.1-desktop-amd64.iso`, 6 482 409 472 bytes |
| SHA-256 | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` |
| Chain of trust | `SHA256SUMS` signed by key `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` (Ubuntu CD Image Automatic Signing Key 2012), fingerprint compared with the official ubuntu.com page; dedicated keyring |
| Identity | pinned by the hash, not by the URL or the file name |

**Firmware**

| Item | Value |
| --- | --- |
| Type | UEFI x86_64, OVMF/edk2 shipped with the QEMU build from ADR-0010 |
| Code | `share/edk2-x86_64-code.fd`, 3 653 632 bytes, SHA-256 `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a`, in `pflash0` with `read-only=on` |
| Variables template | `share/edk2-i386-vars.fd`, 540 672 bytes, SHA-256 `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |
| Variables for baseline and run | the baseline contains a copy of the template; each run uses a **copy of the baseline's vars**, in `pflash1`, recreated at every restore (ADR-0005) |
| Secure Boot | disabled in the reference profile: not needed for Ubuntu (shim/GRUB chain present); the `edk2-x86_64-secure-code.fd` variant with SMM has not been tested and is an optional test |
| Legacy BIOS | excluded: SeaBIOS with `-cpu host` under WHPX pauses with `WHPX: Unexpected VP exit code 4`, and the consistency model requires UEFI NVRAM |

**Reference VM profile**

- Machine type: `pc-q35-11.1` (the tests used the `q35` alias, which in this build resolves to `pc-q35-11.1`; the product uses the versioned name).
- CPU: **`-cpu qemu64`**, 2 vCPUs; RAM 4 GiB (section 13).
- Devices: `-nodefaults -no-user-config -nic none`, `virtio-vga`, `virtio-tablet-pci`, `virtio-keyboard-pci`, ISO on read-only `ide-cd`, disk on `virtio-blk-pci` via `-blockdev` with an explicit format.
- Observed guest resolution: 1280×800.

**Declared deviation (CPU):** `-cpu host` and `-cpu max` hang OVMF before display initialization on this host; the named models `Skylake-Client-v4`, `Haswell-v4` and `Nehalem` reach GRUB, `qemu64` reaches the desktop. The cause has not been determined. The profile uses `qemu64` until a model with more extensions passes a full boot; the choice must be repeated after the user action on the `HypervisorPlatform` feature (`F0-09`, U-1).

Criteria check:

1. Supported LTS release with verifiable checksum and signature: satisfied.
2. Installation and boot from disk under WHPX: **partly satisfied**. Booting from the media to the live desktop is verified; installation to disk and booting from disk have not been run (F1/F2 backlog). If they fail, this ADR reverts to Proposed.
3. Firmware with persistent state manageable as a separate file: satisfied (per-run vars modified by the firmware and restored from the baseline).
4. Graphical environment suitable for the fixture without networking: satisfied (ADR-0019).

## Alternatives considered

- Ubuntu 24.04.x LTS: available, but not the most recent LTS with a point release.
- Server variant with an added graphical environment: requires downloaded packages, incompatible with the offline profile.
- Legacy BIOS (SeaBIOS): excluded (see above).
- Firmware from a separate package: not needed; the firmware of the QEMU build has a recorded version and hash.
- `-cpu host`: requested in the first instance, failed on this host (deviation).

## Consequences

- The matrix in `docs/COMPATIBILITY.md` uses exactly these values in the guest, firmware and machine type columns.
- A change of guest release, firmware or CPU model requires new tests and a new baseline.
- A new point release of the same LTS is adopted only after checksum, signature and boot have been verified; a new LTS is evaluated after its first point release. A relevant security advisory mandates the update (section 17).
- Guest performance with `qemu64` (few CPU extensions, live desktop between 105 and 120 s) is not representative of a full CPU model.

## Required evidence

- Procurement and verification of the ISO: `docs/verification/F0-03-guest-media.md` (verified).
- Boot with firmware under WHPX and CPU model diagnostics: `docs/verification/F0-02-qemu-whpx-qmp.md` §9 (verified with `qemu64`; failed with `host`).
- Persistence and restore of the NVRAM in the baseline: `docs/verification/F0-05-baseline-overlay-firmware.md` §6 (verified, 5 cycles).
- Installation to disk and boot from disk: to be run (F1/F2 backlog in `docs/RELEASE_PLAN.md`).

## References

Roadmap, sections 4, 8, 9, 10 (F0), 13, 17. Review: `docs/verification/F0-09-evidence-review.md`.
