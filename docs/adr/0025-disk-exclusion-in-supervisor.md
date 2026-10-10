# ADR-0025 — Disk exclusion guaranteed by the supervisor, not by the QEMU lock

- Status: Accepted (Windows); Linux verification deferred to R2a with ADR-0016
- Date: 2026-10-09
- Owner: to be assigned

## Context

`qemu-img` must not modify images in use (roadmap, sections 3 and 8) and the baseline is immutable (ADR-0005). In `F0-02` §8.3, with QEMU 11.1.0 on Windows, `qemu-io` and `qemu-img` wrote to and resized both the overlay and the base while QEMU had them open: the image lock does not protect. `F0-05` §7 found the cause (QEMU opens base and overlay with `FILE_SHARE_WRITE`) and tested an exclusion independent of QEMU.

## Decision

1. **The QEMU image lock is not used as a guarantee.** No product control depends on `qemu-img` or `qemu-io` refusing.
2. **Baseline:** the supervisor opens `base.qcow2` and the baseline vars with read-only access and `FILE_SHARE_READ` (without `FILE_SHARE_WRITE` or `FILE_SHARE_DELETE`) for every run and every storage operation of the VM. With the handle open QEMU reads the base; writing, `resize`, `snapshot`, `commit` into the base, renaming and deletion by other processes fail (error 32). This holds whether it is opened before or after QEMU.
3. **Writer detector:** if the exclusive opening of the baseline fails, someone holds it open for writing: the startup or operation is refused (`vm_busy` or `state_conflict`).
4. **Run overlay and vars:** the supervisor holds a handle without `FILE_SHARE_DELETE` (it prevents renaming and deletion) and places them in a per-run directory with a restricted ACL. **Declared limit:** another process of the same user can write the overlay in use, because QEMU must write it and opens it with `FILE_SHARE_WRITE`; the share mode does not distinguish between processes. The limit is consistent with section 7.1 (absolute separation from malicious processes of the same user is out of scope) and is recorded in `docs/SECURITY.md`. **Still open** is the choice of additional protection for the overlay in use among a per-run ACL granted only to QEMU's identity, a distinct process identity for QEMU (restricted token or AppContainer), or the declared limit made permanent; the choice is closed in F1 with evidence (backlog F1-09). Until then the declared limit applies.
5. **Between runs:** read-only attribute or ACL on the baseline and verification of the manifest hash before every use, because the handle protection ends when the supervisor closes.
6. **Operations that produce images** (baseline creation and materialization) write to `.partial` files created with `CREATE_NEW` and promote them with an atomic rename only after `qemu-img check` **and** comparison (`qemu-img compare`) or hash: an interrupted `convert` produces a file that passes `check` (`F0-05` §8.2).
7. All disk mutations go through the per-VM queue and the journal (ADR-0005; schema proposed in `F0-05` §9).

## Alternatives considered

- Relying on the QEMU lock: excluded, it failed on Windows.
- A share mode that reserves the overlay for QEMU alone: not feasible (`F0-05` §7.3: share READ before QEMU prevents startup; after QEMU it is refused; READ|WRITE lets anyone write).
- Only a read-only attribute on the base (`F0-02` §8.4): useful against mistakes, removable by the same user and with no effect on the overlay; adopted as a complementary measure.
- Distinct process identity for QEMU (restricted token, AppContainer) with the overlay ACL granted only to that identity: not tested; candidate in F1 to reduce the limit of point 4.

## Consequences

- `vm-storage` and the supervisor implement the exclusive opening of the baseline, the writer detector, `.partial` names with atomic rename and hash verification before every startup (F1/F3 backlog).
- The negative "Storage" test of section 7.4 (restore concurrent with boot, deletion of a referenced base) uses these mechanisms.
- The protection holds as long as the supervisor is alive; when it dies the Job Object also terminates QEMU (`F0-06`), so no active QEMU is left on an unprotected base.
- It does not protect against an administrator, volume-level access or memory mappings (not tested).

## Required evidence

- Failure of the QEMU lock: `docs/verification/F0-02-qemu-whpx-qmp.md` §8.3 (failed, as expected by the decision).
- QEMU share mode, base protection, writer detector, overlay limit, baseline vars: `docs/verification/F0-05-baseline-overlay-firmware.md` §7 (verified, two runs with identical outcomes).
- Partial file not detectable by `check`: `F0-05` §8.2 (verified).
- Test of per-run ACL and distinct identity for QEMU: F1.
- Linux: R2a.

## References

Roadmap, sections 3, 6.3, 7.1, 7.4, 8, 10 (F0, F3). Review: `docs/verification/F0-09-evidence-review.md`.
