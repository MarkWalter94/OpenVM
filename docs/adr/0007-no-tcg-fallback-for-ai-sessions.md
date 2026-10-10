# ADR-0007 — No TCG fallback for AI sessions

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

The QEMU documentation distinguishes virtualization from TCG emulation and attributes no isolation guarantees to the latter (roadmap, section 3, constraint 1). A silent fallback would run an AI session in an unqualified profile and would skew the measurements.

## Decision

- AI sessions require hardware acceleration (WHPX on Windows, KVM on Linux) and an approved QEMU configuration.
- No silent fallback to TCG: if the accelerator is missing or fails, startup is refused with an explicit error.
- The probe checks the accelerator with the machine and configuration actually used, not just the presence of the binary.
- TCG is not used to make F0 appear complete (section 10, stop decision).
- Any experimental manual use of TCG stays outside the AI profile and the MVP plan (section 5).

## Alternatives considered

- Automatic fallback to TCG with a warning: excluded (sections 3 and 7.2, point 1).
- TCG permitted for AI sessions on hosts without an accelerator: excluded.

## Consequences

- Diagnostics must distinguish "accelerator absent", "accelerator present but rejected" and "incompatible configuration".
- The backend must pass the accelerator explicitly and verify the active one via QMP.

## Required evidence

- Real WHPX probe: `docs/verification/F0-02-qemu-whpx-qmp.md` §4.1 (verified, `query-accelerators` → `enabled: whpx`; no F0 test uses TCG, even where `-cpu host` failed).
- F1 exit criterion: clear error when the accelerator is absent.

## References

Roadmap, sections 3, 5, 7.2 (point 1), 10 (F0, F1).
