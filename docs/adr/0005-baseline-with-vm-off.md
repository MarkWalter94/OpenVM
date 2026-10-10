# ADR-0005 — Baseline with the VM off and per-session overlay

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

Live snapshots involve RAM, device state and application consistency; `qemu-img` must not modify images in use (roadmap, sections 3 and 8). The MVP needs a repeatable return to the initial state.

## Decision

- Visible term: **baseline with the VM off**; never presented as saving the RAM.
- Immutable baseline of disk and configuration, including the relevant writable firmware state (UEFI NVRAM), recorded only after shutdown and release of the files.
- Each test uses a QCOW2 overlay with a pinned internal backing file and a copy of the baseline's firmware state.
- Restore creates a new overlay and restores the other managed persistent states; the guest restarts with a normal boot; requests from the old run are invalidated.
- New checkpoints materialized into standalone images with the VM off, without growing chains; a session's initial baseline cannot be replaced by the agent.
- Excluded in the MVP: RAM snapshots, live snapshots, migration, arbitrary backing-file chains, automatic commit of the overlay into the base, import of external QCOW2 images.
- A forced shutdown does not automatically produce a verified baseline.

## Alternatives considered

- Internal QCOW2 snapshots with RAM state (`savevm`): excluded (section 8).
- Live snapshots with blockdev: excluded from the MVP (sections 4 and 8).
- Full disk copy for every test: not provided for; the model is an overlay on an immutable base.

## Consequences

- Locks, journal, reference checks and post-crash reconciliation (F3).
- The storage budget must account for retained overlays, QCOW2 overhead and metadata (ADR-0017).
- `vm.snapshot` and `vm.restore` reject a running VM and perform no hidden shutdowns (section 9).

## Required evidence

- Base/overlay/restore cycle with persistent firmware and interruption tests: `docs/verification/F0-05-baseline-overlay-firmware.md` §6, §8 (verified, 5 cycles; the run's vars file is modified by the firmware and recreated from the baseline; base unchanged after a kill during guest writes and during `qemu-img convert`). Journal and reconciliation proposal in `F0-05` §9. Disk exclusion in ADR-0025.
- F3 exit criterion: 30 cycles, base hash unchanged, crash tests.

## References

Roadmap, sections 3, 4, 8, 9, 10 (F0, F3).
