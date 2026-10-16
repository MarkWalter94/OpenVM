# R6-03 — User snapshots (ADR-0032): storage, service, UI and the disk operations on real QEMU

- Execution date (UTC): 2026-10-15
- Executed by: development agent on the project host
- Roadmap step: section 12, R6 (user snapshots in manual use), in parallel by owner decision
- Related ADRs: ADR-0032 (user snapshots, disk state), ADR-0005 (baselines), ADR-0017 (storage budget), ADR-0031 (manual-use features)
- Overall outcome: verified in simulation/test (storage, core, service, IPC, UI) and verified on hardware for the disk operations (real QEMU 11.1.0, WHPX); real guests reverted to a snapshot not executed

## Purpose

ADR-0032 lists the required evidence for the user snapshots: take/revert/delete with the VM off and running, tree consistency, firmware state restored, interruption and crash recovery, budget accounting, refusal during an agent session, and real guests (Windows and Linux) reverted to an earlier snapshot booting normally. This file records what was run for every part except the real guests, which are not executed and stay declared as such in the ADR.

What this file does NOT demonstrate: that a real Windows or Linux guest, reverted to an earlier snapshot, boots normally; that a running snapshot is more than crash-consistent (the guest file systems are not flushed without LightVM Tools, ADR-0031, which is not implemented); anything about a second host or a KVM host (R2a).

## Environment

| Item | Value |
| --- | --- |
| Host (model, CPU, RAM, storage) | Intel Core i7-12800HX, 31.6 GiB RAM; worktree on volume E:, scratch work under `E:\OpenVM\lab` |
| OS and build | Windows 11 Pro 10.0.26200 |
| Accelerator and state | WHPX (for the running-VM test of §Results 9 only) |
| Toolchain / relevant versions | Rust toolchain of `rust-toolchain.toml` (1.99.0), MSVC; `cargo test --locked` with a private target directory |
| QEMU binary (version, origin, SHA-256) | QEMU 11.1.0, `lab/qemu/qemu-11.1.0` (ADR-0010): `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534`, `qemu-io.exe` SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a`, `qemu-system-x86_64.exe` as in `R2c-01` |
| Firmware (file, version, SHA-256) | OVMF `share/edk2-x86_64-code.fd` of the same build with a copy of `share/edk2-i386-vars.fd` as writable variables (the build ships no `edk2-x86_64-vars.fd`; same substitution as `F1-03`), only for §Results 9 |
| Guest / media (name, version, SHA-256) | none: a 32 MiB empty QCOW2 scratch disk (`qemu-img create`), filled and read back with `qemu-io` pattern writes and pattern-verified reads; no guest OS boots in §Results 9 — the purpose of the running VM is that QEMU opens the disk while the QMP path executes |
| Graphics session (local, RDP, headless) | local; kittest snapshots with the wgpu renderer |
| Other conditions (antivirus, group policies, etc.) | as in `F0-02` |

## Procedure

Only the real tests (`real_snapshots`, `#[ignore]`d like the other lab tests) need the lab: they read the QEMU folder from `LIGHTVM_QEMU_DIR` and the lab root from `LIGHTVM_LAB_DIR` (the worktree is not where the lab lives), and print a message and skip when they are not set. Both were set:

```powershell
$env:LIGHTVM_QEMU_DIR = 'E:\OpenVM\lab\qemu\qemu-11.1.0'
$env:LIGHTVM_LAB_DIR = 'E:\OpenVM\lab'
cargo test --locked --package lightvm-proto
cargo test --locked --package vm-storage --test snapshots
cargo test --locked --package vm-core --test user_snapshots
cargo test --locked --package qemu-backend --lib
cargo test --locked --package lightvm-service --test user_snapshots
cargo test --locked --package lightvm-desktop --test snapshots_ui --test visual_snapshots
cargo test --locked --package qemu-backend --test real_snapshots -- --ignored --test-threads=1
```

The full blocking regression (`scripts/regression.ps1`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`, clippy of the UI with the glow backend only, clippy of the UI with the wgpu backend only, clippy of the service client side `--no-default-features`, `cargo test --workspace --locked --no-fail-fast`, the lightvm-proto schema snapshot, licence check and SBOM) was run on the same commit; logs in `E:\OpenVM\lab\logs\r6-03-regression` (summary in `summary.txt`).

## Results

| # | Item | Outcome | Evidence |
| --- | --- | --- | --- |
| 1 | Proto: snapshot types (`SnapshotSummary`, `SnapshotTree` with parent and "you are here"), description validation (limit, control characters), the three storage operations, `HostCapacity` unchanged (the Snapshots entry reuses the disk-image tool check) | verified in simulation/test | `lightvm-proto` unit tests; `--test schema_snapshot` (the published schema is unchanged) |
| 2 | Backend: the `qemu-img snapshot -c/-a/-d` command through the product's image command (timeout, private log folder, cancellation), `blockdev-snapshot-internal-sync` and `blockdev-snapshot-delete-internal-sync` over QMP on the `disk0` node, tag validation (lowercase hex and `-`, at most 64) before it reaches the disk | verified in simulation/test | `qemu-backend` library tests (`image`, `run`) |
| 3 | Storage: journaled snapshot take/revert/delete; take-revert-delete builds and keeps the tree (parent of each snapshot, "you are here"); rename and a running snapshot with its thumbnail; a failed or cancelled disk operation changes nothing; revert refused with an overlay, a disk missing, or the VM not stopped, and with an altered snapshot; a crash at every critical step settled from the disk on reopen; an unreadable disk leaves the intent for review; deleting the VM removes its snapshots | verified in simulation/test | `vm-storage` `tests/snapshots.rs` (8 tests) |
| 4 | Core: user only (agents refused and audited, the user refused while an agent session targets the VM, reads and renames stay available), take with the VM off or running, delete running, revert only with the VM off (the refusal names the offered flow), one operation at a time (busy refused), the reverted configuration validated and made the core's record (name kept, missing media detached, out-of-limit refused, a failed finish leaves the record alone), every step audited | verified in simulation/test | `vm-core` `tests/user_snapshots.rs` (3 tests) |
| 5 | Through the IPC with a real catalog: take off and running, delete while running, revert off only (the running refusal names the offered flow), rename; the tree updates after every operation; a failed disk tool and a cancellation leave a consistent state; promote-to-baseline and restore refused while snapshots exist; a VM without a disk has no snapshots yet | verified in simulation/test | `lightvm-service` `tests/user_snapshots.rs` (4 tests) |
| 6 | Refusal during an agent session: the snapshot requests are refused for agents and agent sessions, audited | verified in simulation/test | `vm-core` `tests/user_snapshots.rs::agents_and_agent_sessions_are_refused` |
| 7 | UI: the page with the tree, "you are here" and the badges (While running / Clean / Firmware saved), Memory "never saved"; take with name and description; the running take explained as a power cut (crash-consistent); revert with the VM off (*Discard and revert*); the running-VM revert flow (the VM must be shut down normally first, with the offered snapshot of the current state); delete, rename; the refusals in plain words; the progress with the elapsed time and the disabled changes; one operation at a time; empty and unread lists; the missing disk-image tool disables the entry | verified in simulation/test (kittest) | `lightvm-desktop` `tests/snapshots_ui.rs` (15 tests); goldens `tests/snapshots/snapshots_page.png`, `tests/snapshots/snapshots_take_dialog.png` |
| 8 | VM off, real QEMU: create, apply and delete through the product's exact `qemu-img snapshot -c/-a/-d` command on a real QCOW2 disk. An 8-byte marker written before the take (STATE-A), one written after (STATE-B): the apply restores the STATE-A marker (`qemu-io` pattern verification). A second snapshot of the kept state, a third marker (STATE-C), the deletion of the first: the apply of the second still restores STATE-A. Deleting a tag that is not there fails and changes nothing | verified on hardware (QEMU 11.1.0) | `qemu-backend` `tests/real_snapshots.rs::real_snapshot_off_qemu_img`; log `E:\OpenVM\lab\logs\r6-03\s1792060260758-snapshot-off.log` |
| 9 | VM running, real QEMU under WHPX: the disk is opened by QEMU (32 MiB scratch disk, OVMF, no guest OS), the take and the delete go through QMP `blockdev-snapshot-internal-sync` / `-delete-internal-sync` (15 ms / 12 ms), an invalid tag is refused before it reaches QEMU; after the stop the apply with `qemu-img` restores the marker the disk held while the VM ran (STATE-X, over a STATE-Y written after) | verified on hardware (QEMU 11.1.0, WHPX) | `tests/real_snapshots.rs::real_snapshot_running_qmp`; log `E:\OpenVM\lab\logs\r6-03\s1792060261197-snapshot-qmp.log` |
| 10 | Real guests: a Windows and a Linux guest reverted to an earlier snapshot boot normally | not executed (no guest installed for this feature) | ADR-0032 |
| 11 | A running take with the guest file systems flushed first (LightVM Tools, ADR-0031) | not executed (not implemented) | ADR-0031 |
| 12 | Full blocking regression on the final commit: fmt, clippy (workspace, UI glow, UI wgpu, service client), the whole workspace test suite, the schema snapshot, licence check, SBOM | PASS (all steps) | `E:\OpenVM\lab\logs\r6-03-regression\summary.txt` |

Notes on §Results 9: the stop used in the test is the product's Emergency Stop escalation (the scratch disk has no guest that answers an orderly shutdown); the normal path — the UI offering the orderly shutdown first and waiting for the VM stopped before the revert — is the one verified in §Results 7. The firmware choice (i386 variables) is irrelevant to the disk evidence: no guest boots.

Note on the goldens: the new **Snapshots…** item makes the Dashboard's More menu taller, which moved the click point (and the rendered mouse cursor) of two existing goldens (`dialog_delete.png`, `baselines_empty.png`); they were regenerated with `UPDATE_SNAPSHOTS=1` and otherwise unchanged.

## Limits

- No real guest: the scratch disk proves the product's disk operations (create/apply/delete through `qemu-img` and through QMP, the markers restored), not that a guest OS boots after a revert (item 10).
- The "crash-consistent" semantics of a running snapshot is QEMU's for internal snapshots; on a real guest the file systems may need a check at the next boot, and LightVM says so in the UI.
- WHPX on the one Windows host only; a KVM host (R2a) is not covered.
- The snapshot space is accounted by the product from the `qemu-img info` files of the snapshot, against the VM's storage budget (ADR-0017); it is not cross-checked against an OS quota.
- The QMP path is exercised on the first disk node (`disk0`), which is the only disk a manual VM has today.

## Required actions

- Revert real guests (Windows and Linux) to an earlier snapshot and check the normal boot, recording each guest, version and outcome (ADR-0032, item 10).
- ADR-0031 LightVM Tools: flush the guest file systems before a running take, when the tools are installed (item 11).

## Proposed decisions

None: ADR-0032 records the implementation and the evidence still missing.
