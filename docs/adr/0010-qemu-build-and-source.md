# ADR-0010 — QEMU build and source

- Status: Accepted (Windows); Linux part deferred to R2a with ADR-0016
- Date: 2026-10-09
- Owner: to be assigned

## Context

QEMU is installed separately by the user, with guidance and diagnostics; the LightVM installer is not self-contained (roadmap, section 4). In F0, QEMU must be installed with verified provenance and the binary, version and firmware must be pinned (section 10 F0), without depending on `master` branches (section 6.3). At the initial survey, `qemu-system-x86_64` and `qemu-img` were not on the PATH (section 2).

The choice of build affects other decisions: WHPX support, private channels for QMP (ADR-0012) and for the display (ADR-0013), bundled UEFI firmware (ADR-0011).

## Decision

On Windows 11 x86_64 the reference build is **QEMU 11.1.0 for Windows published by qemu.weilnetz.de** (the Windows builds page indicated by the QEMU project), installed **portably** by extracting the NSIS installer without running it.

| Item | Value |
| --- | --- |
| Version | 11.1.0 (`v11.1.0-12130-ge470268ff4`), stable release of 2026-08-11 |
| Installer | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe`, 206 615 928 bytes |
| Installer SHA-512 | `5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543` (matches the `.sha512` file published by the same source) |
| Installer SHA-256 | `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f` (matches the winget manifest `SoftwareFreedomConservancy.QEMU`, a repository independent of the site) |
| `qemu-system-x86_64.exe` | SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` |
| `qemu-img.exe` | SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| `qemu-io.exe` | SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` |
| Firmware | `share/edk2-*` from the same build (ADR-0011) |
| Verified installation method | extraction with 7-Zip into a user directory, no elevation |

Criteria check:

1. Identified stable release: satisfied (11.1.0, neither a release candidate nor `master`).
2. Verifiable provenance: satisfied with a limitation. Documented source, published SHA-512 and SHA-256 matching the independent winget manifest. **Declared risk:** the Authenticode signature of the binaries is not valid (certificate "Universität Mannheim", GEANT Code Signing CA 4, valid from 12/09/2022 to 12/10/2023, expired). Provenance therefore does not rely on a valid code signature.
3. Accelerator available: satisfied (`-accel help` → `tcg`, `whpx`; real probe `query-accelerators` → `enabled: whpx`).
4. Private QMP channel and protected display: satisfied with Unix sockets (`-qmp unix:…`, `-vnc unix:…`) and with VeNCrypt (`tls-creds-x509`, build with GnuTLS); see ADR-0012 and ADR-0013.
5. UEFI firmware with version and hash: satisfied (ADR-0011).
6. License: QEMU is not redistributed by LightVM in the MVP; the installation guide points to the source and to the recorded checksums.

**Documented alternative for the user:** `winget install --id SoftwareFreedomConservancy.QEMU --exact` installs the same installer (same SHA-256) per machine in `Program Files` and **requires elevation** (UAC). It was not run in F0; the guide presents it as an alternative, with verification of the binaries against the hashes in this ADR.

**Linux:** no build tested (no physical host, ADR-0016). The choice between the distribution package and compiling from a signed tarball is deferred to R2a with the same criteria.

## Alternatives considered

- MSYS2 package and compiling from a signed tarball on Windows: not tested; they remain candidates for an additional provenance check (reproducible rebuild) before F6.
- NSIS installer run directly or via winget: same content, requires elevation; documented alternative.
- Excluded: builds from `master`, binaries without documented provenance, TCG as a substitute for the accelerator (ADR-0007).

## Consequences

- QEMU version, binary hashes and firmware are recorded in `docs/COMPATIBILITY.md`; any change requires repeating the probes (section 10 F6).
- The build includes remote protocol drivers in `qemu-img` (`http`, `https`, `ftp`, `ftps`, `nfs`, `ssh`, `nbd`): the backend must use structured representations with allowlisted format and protocol (section 6.3).
- F1 diagnostics verify the hashes of the approved binaries before use.
- Before F6: additional provenance check (reproducible rebuild, alternative source with a valid signature, or manifest signature), or the risk is kept and declared in the installation documentation.
- If a new version fixes vulnerabilities, the build is updated with a new verification (section 17).

## Required evidence

- Installation, provenance, capabilities, WHPX probe, QMP and shutdown: `docs/verification/F0-02-qemu-whpx-qmp.md` §2–§7 (verified, with the signature limitation).
- Use of the same build for console, storage, supervision and limits: `F0-04`, `F0-05`, `F0-06`, `F0-08`.
- Linux/KVM: `docs/verification/F0-07-linux-kvm.md` (not run, no lab); deferred to R2a.

## References

Roadmap, sections 2, 3, 4, 6.3, 6.4, 10 (F0, F6), 14, 17. Review: `docs/verification/F0-09-evidence-review.md`.
