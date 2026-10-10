# ADR-0015 — Child termination on Linux and fate of manual VMs without a UI

- Status: Proposed for part A (Linux, R2a); **part B Accepted on 2026-10-10** (decision of the project owner)
- Date: 2026-10-09; part B: 2026-10-10
- Owner: to be assigned
- To be closed by: F0

## Context

ADR-0002 settles the process model and the Job Object on Windows. On Linux the equivalent mechanism for terminating children when the supervisor dies is chosen in F0. The fate of manual VMs left without an interface after a UI crash must also be decided, between controlled shutdown and UI reconnection; the decision and the evidence for the two crash scenarios (UI and supervisor) are recorded in an ADR (roadmap, section 6.1).

## Decision

To be taken, in two parts.

**A. Linux mechanism.** Criteria:

1. When the supervisor dies, for any cause including `SIGKILL`, the whole QEMU tree terminates; no orphan process observed.
2. Processes are identified with a stable handle (not just a reusable PID).
3. The mechanism works without elevated privileges.
4. It is compatible with the resource limits chosen in ADR-0017.

**B. Manual VMs after a UI crash.** Criteria:

1. Agent sessions are revoked in any case (section 7.3); the choice only concerns VMs without an AI session.
2. No automatic reactivation of agent sessions.
3. Consistency with saving the user's work (section 7.3, last sentence).
4. If reconnection is chosen: the supervisor must outlive the UI for a limited and verifiable time, and the new UI authenticates on the IPC like the previous one.

### State of the evidence at the F0 review

**A. Linux mechanism — proposal ready, [processes] evidence in WSL2 (`F0-07` §4).** No single mechanism meets the four criteria; the proposed combination meets them with a `sleep` test child in WSL2, an environment admitted by this ADR for the process lifecycle only:

1. `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` with a `getppid()` check on every QEMU, spawned by a thread that lives as long as the supervisor (the signal is tied to the creating thread): SIGKILL to the supervisor → end of the direct child, p95 1.142 ms, 30 samples, 0 orphans.
2. Identity via pidfd (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`): 30/30 `ESRCH` after reaping; kernel ≥ 5.4.
3. One cgroup v2 per VM inside a systemd user scope with `Delegate=yes`, with `memory.max`, `cpu.max`, `pids.max` and `cgroup.kill` (kernel ≥ 5.14): the only tested mechanism that also terminates descendants that left with `setsid` (p95 0.420 ms, 30 samples, 0 orphans); it is the same tool used for the limits of ADR-0017.
4. Process group as fallback where cgroup delegation is missing, with the declared limit (it does not contain descendants that change group); in that case CPU/memory limits on the tree are missing and AI Mode stays disabled on that host (section 7.2).

Still to be decided: creation of the delegated scope (`systemd-run --user --scope -p Delegate=yes` at startup, or `StartTransientUnit` via D-Bus from the supervisor) and a scope per VM or a sub-cgroup of the supervisor's scope (the latter was tested). Follow-up test in WSL2 (`docs/verification/F1-08-ci.md` §3, valid for processes only): the delegated scope is created with `StartTransientUnit` via D-Bus without `systemd-run`; if the scope contains only the VM, systemd removes it when the VM exits, so supervisor and VM leaves must be in the same scope; without a systemd user session the bus is unreachable, the process group fallback is used and agent mode stays disabled. Part A remains Proposed until the explicit choice is made. Confirmation with real QEMU and KVM on a physical host is deferred to R2a (ADR-0016).

**B. Manual VMs after a UI crash — decided on 2026-10-10** (section "Part B decision" at the end). State before the decision:

- Windows: the death of the supervisor always terminates QEMU via the Job Object (p50 118.9 ms with `abort()`, 110.4 ms with `TerminateProcess`, max 143.9 ms, 10 cycles, `F0-06` §2a): reconnection is possible only if the supervisor outlives the UI; if the supervisor dies the manual VM is lost and its overlay is not a clean state. The **UI crash has not been run** on Windows because the UI does not yet start the supervisor as a separate process (pending P-2 in `F0-09`).
- Linux, **[processes]** only: the supervisor detects the death of the UI with a pidfd and stops the VM (SIGTERM, 2 s, SIGKILL): p95 1.769 ms until the end of VM and supervisor, 30 samples (`F0-07` §4).
- Proposal to be confirmed with the Windows test: controlled shutdown as the initial behaviour (`system_powerdown` with timeout, then `quit`, then termination with OS primitives), because it is verifiable without IPC; reconnection remains an alternative to be tested once authenticated IPC exists.

Test on Windows run on 2026-10-10: `docs/verification/F1-09-ui-supervisor-ui-crash.md`.

### Part B decision (2026-10-10)

Choice of the project owner: the fate of VMs on loss of the UI is **a user setting**, with the default being **terminate the VMs immediately**.

| Policy (`ParentLossPolicy`) | Behaviour | Availability |
| --- | --- | --- |
| `kill_vms` (default) | revocation of agent sessions, Emergency Stop of all VMs with OS primitives (recorded in the audit and in the core), service shutdown | UI and service |
| `shutdown_vms { powerdown_s }` | revocation of agent sessions, `system_powerdown` waiting `powerdown_s` (1–300, in the UI 5/10/30/60/120/300 s), then `quit` 5 s and termination 10 s; the service stop timeout covers at least `powerdown_s + 20` s | UI and service |
| `keep_vms` | VMs and service stay active without a UI | service only (`--parent-loss-policy keep-vms`, tests); **disabled in the UI** until reconnection of a new UI exists (criterion 4) |

Rationale and reading of the roadmap:

- Section 6.1 admits "controlled shutdown" or "UI reconnection". `kill_vms` is read as a controlled shutdown **by the supervisor** with zero wait: the termination is requested, observed and recorded by the supervisor through the same path as the Emergency Stop (section 7.3). It may lose the state of the VM overlay, never the baseline. It is an explicit choice of the project owner, not a technical fallback.
- `shutdown_vms` is the controlled shutdown "graceful with timeout, then forced" foreseen among the alternatives.
- `keep_vms` without reconnection would leave VMs that no UI can command any more: it is not among the alternatives until criterion 4 is met.
- Criteria 1–2: under every policy agent sessions are revoked first and are not reactivated. Criterion 3: manually closing the app remains a separate controlled shutdown; the policy applies only to abnormal loss of the UI.
- Regardless of the policy, if the supervisor does not complete the shutdown within its own timeout the remaining VMs are terminated with the Emergency Stop before exiting (no longer left to the closing of the Job Object alone).

Evidence (Windows, real QEMU, `F1-09` section of 2026-10-10): `kill_vms` → QEMU terminated 105 ms and service 208 ms after killing the UI, audit `emergency_stop succeeded`; `shutdown_vms` with 3 s → QEMU 3.21 s, service 3.29 s, audit `vm_stop succeeded`. Linux: deferred to R2a together with part A.

## Alternatives considered

A. Linux (candidates, to be verified):
- process group plus `PR_SET_PDEATHSIG` on direct children;
- dedicated cgroup v2 (for example a systemd user scope) with termination of the whole group;
- a combination of the two, with pidfd for process identity.

B. Manual VMs:
- controlled shutdown on loss of the UI (graceful with timeout, then forced);
- reconnection of a new UI instance to the still-running supervisor.

## Consequences

- A affects ADR-0009 (Emergency Stop on Linux) and ADR-0017 (limits).
- B determines the behaviour described to the user and the "Lifecycle" test of section 7.4.

## Required evidence

- Supervisor crash on Windows: `docs/verification/F0-06-process-supervisor.md` §2a (verified). UI crash on Windows: not run (pending P-2 in `docs/verification/F0-09-evidence-review.md`).
- Scenarios on Linux in a non-accelerated environment, process lifecycle only: `docs/verification/F0-07-linux-kvm.md` §4 (verified **[processes]**).
- The same scenarios with real QEMU on a physical Linux host: R2a (ADR-0016).

## References

Roadmap, sections 6.1, 7.3, 7.4, 10 (F0), 13, 17.
