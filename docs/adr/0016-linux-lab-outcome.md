# ADR-0016 — Linux lab outcome

- Status: Accepted (2026-10-10): **verified on Windows only** for the MVP; physical Linux host available later for R2a
- Date: 2026-10-09; confirmation: 2026-10-10
- Owner: to be assigned

## Context

The F0 exit criterion requires Windows and Linux evidence, or an explicit blocking result for the failed target, or the outcome "verified on Windows only" (roadmap, section 10 F0). The availability of a physical Linux host with KVM is not confirmed at the start of F0.

## Decision

Outcomes admitted by section 10:

| Outcome | Condition | Effect |
| --- | --- | --- |
| Windows and Linux verified | Real smoke test on a physical Linux host with KVM passed | Windows first target; Ubuntu cell with recorded outcome |
| Verified on Windows only | No physical Linux host with KVM available in F0 | Windows the only MVP target; Ubuntu cell "not verified"; Linux qualification deferred to R2a, which does not close without that evidence |
| Linux first target | Windows fails the checks; Linux passes them with a lab available | Change of target without relaxing the criteria |
| Blocked | No host passes the checks | Project halted on that criterion |

**Proposed outcome: "verified on Windows only".**

Facts (`F0-07`): on the available machine there is no physical Linux host with KVM; there is only WSL2 (kernel `6.18.33.1-microsoft-standard-WSL2`) with nested `/dev/kvm` not accessible to the user, which section 10 excludes as evidence. WSL2 was used only for builds and accelerator-independent tests and for process mechanisms (labels **[build]** and **[processes]**). Windows passes the exit criterion checks with the deviations and pending items published in `docs/verification/F0-09-evidence-review.md`.

Conditions for this to take effect:

1. **User confirmation** that no physical Linux host with KVM is available in F0 (`F0-09`, action U-5). If the host is available, the Linux/KVM smoke test must be run in F0 and this ADR records "Windows and Linux verified" or an explicit block for Linux.
2. Windows passes the exit criterion checks (pending items P-2…P-7 of `F0-09` closed).
3. Recording in `docs/STATUS.md` and `docs/COMPATIBILITY.md`, with Windows as the only MVP target and the Ubuntu cell "not verified".

**Evidence deferred to R2a** (R2a does not close without it):

1. Real smoke test on a physical Linux host with KVM: QEMU with verified provenance, accelerated guest boot, private QMP on a Unix socket, console and input, forced stop.
2. ADR-0015 on the physical host with real QEMU: UI crash, supervisor crash, Emergency Stop with 100 samples, no orphans.
3. cgroup limits applied to QEMU with KVM (guest memory, vCPUs, `cgroup.kill`).
4. eframe build with the standard Ubuntu toolchain and smoke test with a real GPU, native Wayland and X11.
5. Linux packages and host policy (section 5, Ubuntu and Debian/Fedora rows).

### Confirmation of 2026-10-10

The project owner confirms (action U-5) that no physical Linux host with KVM is available in F0; a Linux server for the tests will be obtained **later**, for R2a. Recorded outcome: **verified on Windows only**; Ubuntu cell of `docs/COMPATIBILITY.md` "not verified".

Guidance from the owner: test Linux in the next step, including by running LightVM inside a Linux VM on Windows. Per section 10 (paragraph "Linux lab missing") a nested virtual machine **does not replace** the physical host: the VM on Windows is used for builds, accelerator-independent tests and process mechanisms (labels **[build]** and **[processes]**, like WSL2 in `F0-07`), not to qualify the Ubuntu cell nor to close R2a. The deferred evidence remains as listed above, on the physical server.

## Alternatives considered

WSL2, nested VMs or runners without verifiable acceleration do **not** replace the physical host for this evidence; they serve only for builds and accelerator-independent tests (section 10 F0). The nested KVM probe in WSL2 (`F0-07`, action 3) remains optional and not valid for the matrix.

## Consequences

- The Linux eframe build (ADR-0014) and the accelerator-independent tests remain required and are planned in the F1 CI (Linux job without accelerator, backlog in `docs/RELEASE_PLAN.md`).
- Missing hardware means pending verification, not passed verification.
- ADR-0010, ADR-0012, ADR-0013, ADR-0014 and ADR-0015 have their Linux part deferred to R2a.

## Required evidence

- `docs/verification/F0-07-linux-kvm.md` (Linux/KVM smoke test on a physical host not run; builds and processes in WSL2 run with limited validity).
- Review: `docs/verification/F0-09-evidence-review.md` §1 (E11), §6.

## References

Roadmap, sections 5, 10 (F0), 12 (R2a), 14, 15 (step 5), 17.
