# ADR-0032 — User snapshots in manual use (VMware-style snapshot manager, disk state)

- Status: Accepted (2026-10-14): direction set; evidence pending
- Date: 2026-10-14
- Owner: project owner (decision); implementation recorded below

## Context

The project owner requires snapshots as in VMware: several named snapshots per VM, taken whenever the user wants, a tree, revert to any of them and delete without losing the others. LightVM today has only **baselines with the VM off** (ADR-0005), which serve agent sessions and are not a snapshot manager. VMware VMs imported by ADR-0030 can carry a snapshot tree.

Facts on the first host (QEMU 11.1.0, WHPX), probed on 2026-10-14 with a scratch QCOW2 disk:

- `savevm` (snapshot **with RAM and device state**) is refused: *"State blocked due to missing dirty memory tracking support, and some system register/state save-restore"* (WHPX migration blocker). RAM snapshots are therefore not possible on Windows hosts with this accelerator.
- `snapshot_blkdev_internal` / QMP `blockdev-snapshot-internal-sync` (**disk-only internal snapshot of a running VM**) works: two snapshots taken while running, listed by `qemu-img snapshot -l`.

## Decision

**What a snapshot is.** A named point (name, optional description, date, a screenshot thumbnail when the VM was running) holding the **disk state** as a QCOW2 internal snapshot of the VM's managed disk, plus a copy of the writable firmware state (UEFI variables) and of the VM configuration at that moment. Never presented as saving memory: the UI says that reverting restarts the guest from the saved disk state.

- **VM off:** consistent snapshot (`qemu-img snapshot -c` with the files released by the supervisor).
- **VM running:** disk-only snapshot through QMP `blockdev-snapshot-internal-sync`, shown as *taken while running (like a power cut at that instant)*. When LightVM Tools are installed (ADR-0031) the guest file systems are flushed first; without them the state is crash-consistent. The firmware state copied is the run's current variables file.
- **Revert:** only with the VM off (the UI offers to shut it down first, never a hidden forced stop): `qemu-img snapshot -a`, firmware state and configuration restored; the guest starts with a normal boot.
- **Delete:** any snapshot, the others keep their state (`qemu-img snapshot -d` with the VM off, `blockdev-snapshot-delete-internal-sync` while running).
- **Tree:** the parent of each snapshot is the snapshot the disk state descended from (the last one taken or reverted to); the UI shows the tree and "you are here".

**Where.** One file per disk (internal snapshots), so no backing-file chains are added (roadmap section 8). Operations are journaled and serialized per VM like other destructive storage operations, reconciled after a crash, and counted by the storage budget (ADR-0017: snapshot size reported by `qemu-img info`).

**Relation with baselines and agent sessions.** Snapshots are a manual-use feature (as ADR-0031): not exposed to agents, not taken or reverted during an agent session (sessions run on their own overlay of a baseline). A snapshot of a VM that is off can be promoted to a baseline (copy materialized as a standalone image, ADR-0005 rules).

**VMware import.** ADR-0030 imports the current state; as an option the import also converts each VMware snapshot's disk state into a LightVM snapshot, keeping names, descriptions and tree. VMware memory states (`.vmem`, `.vmsn`) are not imported.

## Alternatives considered

- RAM snapshots (`savevm`, `snapshot-save`): blocked by WHPX on this host (fact above). To revisit only if a future accelerator/QEMU combination supports them, with its own evidence.
- External overlays per snapshot (backing-file chains): more files, chain management and growing chains that section 8 avoids; internal snapshots give the same tree in one file.

## Required evidence

- Take/revert/delete with the VM off and running, tree consistency, firmware state restored, interruption and crash recovery, budget accounting, refusal during an agent session.
- Real guests: a Windows and a Linux guest reverted to an earlier snapshot boot normally. **Not verified.**

## References

Roadmap, sections 8, 12 (R6); ADR-0005 (baselines), ADR-0017 (storage budget), ADR-0030 (VMware import), ADR-0031 (manual-use features).
