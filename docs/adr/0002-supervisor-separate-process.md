# ADR-0002 — Supervisor as a process separate from the UI; QEMU as a child of the supervisor

- Status: Accepted
- Date: 2026-10-09
- Owner: to be assigned

## Context

QEMU processes, sessions, policies and VM files require a single authority. If the UI owned QEMU directly, an interface crash would leave orphaned processes or active sessions, and the UI would become a trusted layer (roadmap, section 6.1).

## Decision

- The supervisor is a process distinct from the eframe process, started and watched by the UI, reachable only through authenticated local IPC, the same one used by the MCP adapter.
- The supervisor is the sole authority over processes, sessions, policies and VM files; the logic lives in Rust libraries independent of the UI.
- QEMU processes are children of the supervisor, not of the UI.
- On Windows, QEMU processes belong to a Job Object of the supervisor with automatic termination when the handle is closed.
- On Linux an equivalent mechanism is used, chosen in F0 (ADR-0015).
- The supervisor starts with LightVM and exits after the controlled shutdown of the VMs; in the MVP it is not an always-on installed service.
- A single supervisor instance per catalog; processes recognized through handle/identity and `run_id`, never through the PID alone.
- Loss of the supervisor interrupts sessions; recovery of the UI or the application does not automatically reactivate an agent session.
- A UI crash revokes agent sessions (section 7.3). The fate of manual VMs left without a UI is decided in ADR-0015.

## Alternatives considered

- QEMU as a child of the UI: excluded (section 6.1).
- Supervisor as an always-on system service: excluded for the MVP (section 6.1).
- Logic in the UI with a thin supervisor: excluded; the UI is not a trusted layer (section 6.1).

## Consequences

- A typed, authenticated IPC is needed from F1, with a named pipe with ACLs on Windows or a local socket with restrictive permissions on Unix (section 9).
- Three crash scenarios to test: UI, supervisor, QEMU.
- The UI receives only the data needed by its screens.

## Required evidence

- QEMU as a child of the supervisor in a Job Object, termination of children on supervisor death, detection of external QEMU death, handle + creation-time identity, no orphaned processes: `docs/verification/F0-06-process-supervisor.md` §1, §2a, §2b, §3 (verified on Windows). UI crash with session revocation: not run (the UI does not yet start the supervisor; pending P-2 in `docs/verification/F0-09-evidence-review.md`).
- Linux mechanisms in a non-accelerated environment: `docs/verification/F0-07-linux-kvm.md` §4 (**[processi]**; ADR-0015).

## Implementation details proven in F0 (Windows)

To be carried over into `qemu-backend` and `lightvm-service` (`F0-06`, Proposed decisions):

- QEMU created with `CreateProcessW`, absolute path, structured arguments with MSVC quoting, no shell.
- `PROC_THREAD_ATTRIBUTE_JOB_LIST`: QEMU in the job from creation, without the window of a later assignment; anonymous job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, without `BREAKAWAY_OK` or `SILENT_BREAKAWAY_OK`, with a completion port for notifications.
- `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limited to stdin (`NUL`), stdout and stderr to files.
- `DETACHED_PROCESS` (no `conhost.exe`, single-process tree); both variants verified in the job.
- QEMU environment reduced to `SystemRoot`, the only variable found to be necessary in the tested configuration.
- Identity: handle kept for the whole run, PID, creation time and `run_id`; the "alive" state is read by waiting on the handle, not from the existence of the PID.
- IPC rejects processes without a credential (section 7.4, "IPC/console" row): from F1.

## References

Roadmap, sections 6.1, 6.2, 7.3, 7.4, 9, 10 (F0), 13.
