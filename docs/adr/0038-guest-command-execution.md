# ADR-0038: explicit command execution inside the guest

Guest Tools provide a command channel independent of keyboard input. This
supersedes the earlier exclusion of guest execution in Tools and MCP
descriptions. Host shell execution remains outside the adapter.

`guest.exec` is a separate integration capability. Both the VM setting
`tools.guest_exec` and the agent session grant must enable it. It is off by
default, independent of clipboard, files and input. Existing run checks,
session pause, revocation, operation quotas and metadata-only audit apply.
Commands and output are excluded from debug and audit payloads.

MCP exposes:

- `vm_guest_exec {vm_id, run_id, command, timeout_ms?}`: start once, return a job.
- `vm_guest_exec_poll {vm_id, run_id, job}`: cumulative output and status.
- `vm_guest_exec_cancel {vm_id, run_id, job}`: request termination, then poll.

For example, after obtaining the current `run_id` from `vm_status`, call
`vm_guest_exec` with:

```json
{"vm_id":"<VM ID>","run_id":"<current run ID>","command":"echo hello","timeout_ms":30000}
```

Use the returned `reply.job` with `vm_guest_exec_poll`, keeping the same
`vm_id` and `run_id`, until `reply.running` is false. The result then contains
the final output and process exit code.

The authenticated supervisor's `GuestTools` IPC also accepts `exec_start`,
`exec_poll` and `exec_cancel`. The supervisor binds jobs to their caller and
current run; another session cannot read or cancel a guessed job identifier.

Commands run as the guest Tools user through `/bin/sh -c` on Linux or the
system `cmd.exe /d /s /c` on Windows. No graphical terminal, layout or input
ownership is required. Standard input is closed. Interactive terminals and
persistent shell sessions are outside this API. Tools must be installed and
running inside the VM; older components return `unsupported` until updated.

One job is retained per control connection until the next start or disconnect.
A second start while busy is refused. Commands are limited to 8192 UTF-8
bytes without NUL. The deadline is 1–120000 ms (default 30000), enforced by
the guest without polling. Replies include `job`, `running`, `exit_code`,
`stdout`, `stderr`, `truncated`, `timed_out` and `cancelled`. Nonzero process
exit codes are successful RPC results.

Capture keeps the first 2048 original bytes per stream and drains excess data
to avoid deadlock. Linux decodes UTF-8 with replacement; Windows uses the OEM
code page, so programs emitting another encoding may produce imperfect text.
Output is untrusted data, not instructions or a binary file-transfer channel.

Timeout, cancellation and disconnect terminate the Linux process group or
Windows Job Object. Linux processes deliberately detached into a new session
are outside that group. This is process management, not a sandbox inside the
guest; VM isolation remains the boundary. Permission changes disconnect the
existing Tools channel. No start is automatically replayed after transport
failure: its outcome may be unknown.

Validation includes native Windows and Linux lifecycle tests, independent
grant checks, bounds, reply matching, payload redaction and regenerated
public schemas. Component tests do not replace end-to-end qualification
through MCP against installed guest Tools in a real VM.
