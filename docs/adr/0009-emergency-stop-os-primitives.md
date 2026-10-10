# ADR-0009 — Emergency Stop with OS primitives, independent of QMP

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

A stuck QMP, an unresponsive guest or a client that keeps sending requests must not prevent shutdown. No command may depend on the LLM's consent (roadmap, section 7.3).

## Decision

- Three distinct actions: **Pause agent** (blocks new mutations, the VM keeps running), **Revoke** (invalidates the session, discards the unforwarded queue, closes the channels), **Emergency Stop** (revoke plus termination of the QEMU tree with OS primitives).
- Emergency Stop does not wait for QMP or for guest shutdown; it follows a priority path in the supervisor, independent of the VM's operation queue.
- On Windows, termination uses the supervisor's Job Object (ADR-0002); on Linux, the mechanism chosen in ADR-0015.
- Completion is declared only after the end of the processes has been observed.
- Emergency Stop may lose the overlay state but must not alter the baseline.
- A local emergency command usable if the UI is frozen is provided.
- The report distinguishes discarded queue, requests already forwarded and observed effects; no retroactive undo is promised.

## Alternatives considered

- Stop via QMP `quit` or `system_powerdown`: permitted for ordinary shutdown, not as the emergency path.
- Kill by PID: excluded as the sole identifier (section 6.1).

## Consequences

- The supervisor keeps process handles and `run_id`.
- Measurement target: p95 ≤ 2 s for the end of the QEMU process (section 13), to be measured, not guaranteed.

## Required evidence

- Termination of the QEMU tree without QMP in F0: `docs/verification/F0-06-process-supervisor.md` §2c (verified on Windows: `TerminateJobObject` with a dedicated code, completion declared after waiting on the QEMU handle and `ActiveProcesses = 0`; with the guest at the desktop p50 394.3 ms, p95 640.2 ms, max 652.0 ms over 30 samples; during boot p50 142.7 ms, p95 273.3 ms, max 354.5 ms over 100 samples; 0 errors, 0 timeouts, 0 leftovers). Proposed wait timeout: 10 s with a visible error. Ordinary shutdown with escalation `system_powerdown` → `quit` → termination: `F0-06` §2d (verified as a procedure; the live guest does not complete the ACPI shutdown).
- Linux: `cgroup.kill` in a non-accelerated environment, `docs/verification/F0-07-linux-kvm.md` §4 (**[processi]**); with real QEMU in R2a.
- "Revocation" negative test (input flood during Emergency Stop) and stop latency measurements: F4.

## References

Roadmap, sections 6.1, 7.3, 7.4, 10 (F0, F4), 13.
