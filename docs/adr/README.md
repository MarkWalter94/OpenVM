# Architecture Decision Records

This directory records the technical choices that the roadmap leaves open or sets as a direction to be applied, with rationale, alternatives and required evidence. Reference: `docs/ROADMAP.md`, section 17.

## Binding rule

An ADR **may not override** the mandatory checks, exclusions, exit criteria or conditions of the alternative branches set by the roadmap (section 17). An ADR chooses *among* the permitted alternatives; if no alternative passes the tests, the ADR moves to **Blocked** and the affected milestone remains blocked. A technical limitation does not authorize deleting, simulating or making optional a requirement.

An ADR does not declare anything as verified: the evidence lives in `docs/verification/` and the ADR points to it.

## Statuses

| Status | Meaning |
| --- | --- |
| Proposed | Open decision, to be closed by the indicated phase (section 17). Lists alternatives, criteria and required evidence. |
| Accepted | Decision taken. For ADRs that adopt the roadmap, the direction is set; the required evidence remains to be produced. |
| Superseded | Replaced by a later ADR, named in the Status field. The text remains unchanged for traceability. |
| Blocked | No permitted alternative has passed the tests; the milestone that depends on it is blocked. |

## Format

File name: `NNNN-short-title.md`, four-digit numbering, sequential, never reused.

```markdown
# ADR-NNNN — <Title>

- Status: Proposed | Accepted | Superseded by ADR-NNNN | Blocked
- Date: YYYY-MM-DD
- Owner: <name or "to be assigned">
- To be closed by: F0 | F1 | ... (Proposed only)

## Context
Problem, constraints and references to the roadmap.

## Decision
The choice (or, for a Proposed ADR, the criteria by which it will be made).

## Alternatives considered
Options permitted by the roadmap, with the reason for choosing or excluding them.

## Consequences
Effects on architecture, security, tests and compatibility; what happens if the tests fail.

## Required evidence
Evidence needed, with references to the files in `docs/verification/` (existing or with a proposed name).

## References
Sections of `docs/ROADMAP.md` and external documents.
```

When a Proposed ADR is closed, the following are updated: the ADR's status and Decision section, `docs/STATUS.md`, `docs/RELEASE_PLAN.md` ("Open decisions" section) and, where relevant, `docs/COMPATIBILITY.md` and `docs/SECURITY.md`.

## Index

| ADR | Title | Status | Roadmap sections |
| --- | --- | --- | --- |
| [0001](0001-native-ui-egui-eframe.md) | Native Rust UI with egui/eframe and the console as a texture | Accepted | 6.0, 6.4 |
| [0002](0002-supervisor-separate-process.md) | Supervisor as a separate process; QEMU as a child of the supervisor | Accepted | 6.1 |
| [0003](0003-internal-rfb-console-and-fallback.md) | Console with an internal RFB client on the protected channel and screendump fallback | Accepted | 6.4 |
| [0004](0004-offline-mvp-profile.md) | Offline MVP profile with no NIC and no host shares | Accepted | 4, 7.2 |
| [0005](0005-baseline-with-vm-off.md) | Baseline with the VM off and per-session overlay | Accepted | 8 |
| [0006](0006-mcp-stdio-pairing-grant.md) | MCP over stdio with pairing and a limited grant, no generic QMP | Accepted | 9 |
| [0007](0007-no-tcg-fallback-for-ai-sessions.md) | No TCG fallback for AI sessions | Accepted | 3, 7.2 |
| [0008](0008-media-registry-data-disc.md) | Approved media registry and read-only data medium | Accepted; copy rule amended by ADR-0029 | 4, 6.3 |
| [0009](0009-emergency-stop-os-primitives.md) | Emergency Stop with OS primitives, independent of QMP | Accepted | 7.3 |
| [0010](0010-qemu-build-and-source.md) | QEMU build and source | Accepted (Windows; Linux in R2a) | 6.3, 10 F0, 17 |
| [0011](0011-guest-release-and-firmware.md) | Guest release and firmware | Accepted | 4, 8, 17 |
| [0012](0012-private-qmp-channel.md) | Private QMP channel | Accepted (Windows; Linux in R2a) | 6.3, 7.2, 17 |
| [0013](0013-protected-console-transport.md) | Protected console transport and fallback | Accepted (Windows; Linux in R2a) | 6.4, 17 |
| [0014](0014-egui-eframe-versions-backends.md) | egui/eframe versions and rendering backend | Accepted (Windows; Linux backend in R2a) | 6.0, 14, 17 |
| [0015](0015-linux-child-termination-manual-vms.md) | Child termination on Linux and fate of manual VMs left without a UI | Part B Accepted (2026-10-10); part A Proposed (Linux, R2a) | 6.1, 7.3, 17 |
| [0016](0016-linux-lab-outcome.md) | Linux lab outcome | Accepted (2026-10-10): only Windows verified; physical Linux in R2a | 5, 10 F0, 17 |
| [0017](0017-os-limits-storage-budget.md) | OS limits and storage budget | Partly Accepted (2026-10-10): per-VM limits; host thresholds and full storage in F3 | 7.2, 8, 17 |
| [0018](0018-reference-hardware.md) | Reference hardware | Accepted | 13, 17 |
| [0019](0019-deterministic-test-fixture.md) | Deterministic test fixture | Accepted (definition; implementation by F3) | 9, 17 |
| [0020](0020-signing-and-independent-review.md) | Signing responsibility and independent review | Accepted (2026-10-10): not available; F6 publication blocked | 10 F6, 14, 17 |
| [0021](0021-mcp-sdk-and-clients.md) | MCP SDK and clients | Accepted for the SDK (`rmcp` 3.5.1); clients open until F5 | 9, 17 |
| [0022](0022-ui-test-framework.md) | UI test framework | Accepted (2026-10-10): egui_kittest | 13, 17 |
| [0023](0023-dependencies-and-licences.md) | Dependencies and licences | Accepted for policy and checks; SBOM and egui fonts open until F6 | 6.0, 6.3, 17 |
| [0024](0024-separate-rfb-client-crate.md) | Separate `rfb-client` crate used by the UI; `qemu-backend` stays in the supervisor | Accepted | 6.1, 6.2, 6.4 |
| [0025](0025-disk-exclusion-in-supervisor.md) | Disk exclusion guaranteed by the supervisor, not by the QEMU lock | Accepted (Windows; Linux in R2a) | 3, 6.3, 7.1, 7.4, 8, 10 |
| [0026](0026-authentication-authorization-codes.md) | Public codes for authentication and authorization | Accepted (2026-10-10) | 6.3, 7.3, 7.4, 9 |
| [0027](0027-windows-guest-hardware-profiles.md) | Windows guest hardware profiles (XP, Vista, 7, 8.1, 10; SeaBIOS; offline) | Accepted (2026-10-12): direction set; evidence pending | 4, 5, 12 (R2c) |
| [0028](0028-macos-hvf-windows11-arm64.md) | macOS Apple Silicon host and Windows 11 ARM64 guest (HVF, swtpm, project-built Secure Boot firmware, offline) | Accepted (2026-10-12): direction set; evidence partial | 5, 7.2, 12 (R2b) |
| [0029](0029-media-used-in-place.md) | Installation and data ISOs used in place, without a managed copy (owner decision; amends ADR-0008) | Accepted (2026-10-14) | 6.3, 7.4 |
