# Architecture

Last updated: 2026-10-10 (F0 review, F1 progress)

Document required by section 10 (F0) of `docs/ROADMAP.md`, derived from section 6. It describes the product's components, boundaries and rules; the existing code consists of F0 feasibility spikes and crates under construction. Items marked **[verified in F0: …]** refer to feasibility tests on the first host (Windows 11, `docs/verification/`); items marked **[pending]** depend on tests not yet run. Decisions are in the referenced ADRs.

## 1. Components and boundaries

| Component | Process | Responsibility | Does not | ADR |
| --- | --- | --- | --- | --- |
| Desktop UI (`apps/desktop`, egui/eframe) | UI process | Screens, wizards, console as a texture, input reading with winit, native file picker | Does not launch QEMU, does not open disks, does not run generic host commands, is not a trusted layer, does not know the QMP endpoint | 0001, 0014 |
| RFB client (`rfb-client`) | Library in the UI process | RFB 3.8 handshake, VeNCrypt X.509 with rustls, ZRLE/Hextile/CopyRect/Raw/DesktopSize decoding, shared framebuffer | Does not send input, clipboard or power control (client messages 0, 2, 3 only); does not depend on QMP or process code | 0003, 0024 |
| Supervisor (`lightvm-service`) | Supervisor process, separate from the UI | Sole authority over processes, sessions, policy and VM files; authenticated IPC; per-VM queue; Emergency Stop | Does not expose QMP or host paths to clients | 0002, 0009 |
| MCP adapter (`mcp-server`, binary `lightvm-mcp`, from F5) | Process launched by the MCP client via stdio | MCP ↔ IPC translation; pairing | Does not open disks, does not run QEMU, does not decide policy | 0006 |
| QEMU backend (`qemu-backend`) | Library in the supervisor | Probe, argument construction, spawn into the Job Object, single QMP client, server side of the console (private directory, `-vnc unix:`, display credential), screendump, input | Does not accept arbitrary QEMU options; does not contain the RFB client | 0007, 0010, 0012, 0013, 0024 |
| Storage (`vm-storage`) | Library in the supervisor | SQLite, versioned JSON manifests, media registry (ISOs used in place, re-verified before every use), baselines, overlays, journal, disk exclusion through share modes | Does not modify images in use; does not rely on QEMU's lock | 0005, 0008, 0025, 0029 |
| Core (`vm-core`) | Library | Models, states, use cases, policy, sessions | No dependency on UI or process I/O | — |
| Protocol (`lightvm-proto`) | Library | IPC/MCP types and JSON Schema generated from a single source; stable public errors | — | — |
| Guest detection (`guest-detect`) | Library in the supervisor | Reads an imported installation ISO (ISO 9660/Joliet, UDF, WIM XML metadata) and suggests the guest system, architecture, build and edition; bounded work and memory on untrusted bytes | Does not choose the hardware profile (the user confirms the system in the wizard); does not write, mount or execute anything from the image; no external dependencies | 0008, 0027 |
| QEMU | Child process of the supervisor, one per running VM | Runs the guest with an accelerator | — | 0007 |

## 2. Diagram

```text
 UI process (eframe)                       External MCP client process
 ┌─────────────────────────┐                ┌──────────────────────────┐
 │ screens, wizards         │                │ agent                    │
 │ RFB client → texture     │                └────────────┬─────────────┘
 │ winit input (scancode)   │                             │ stdio (MCP 2025-11-25)
 └──────┬───────────▲───────┘                ┌────────────▼─────────────┐
        │ IPC       │ framebuffer            │ lightvm-mcp adapter      │
        │ typed     │ (protected             └────────────┬─────────────┘
        │ authent.  │  display channel                    │ authenticated IPC
        │           │  or IPC in fallback)                │
 ┌──────▼───────────┴─────────────────────────────────────▼─────────────┐
 │ Supervisor process                                                    │
 │  IPC authentication → policy (intersection) → sessions/grants        │
 │  → per-VM operation queue ─┐      Emergency Stop priority path        │
 │  QEMU backend · storage · audit                                       │
 └────────┬───────────────────┴──────────────────────────────────────────┘
          │ spawn with structured arguments, Job Object / Linux mechanism
 ┌────────▼────────────────────┐
 │ QEMU process (per VM)        │◄── QMP on private channel (supervisor only)
 │  protected RFB display       │──► framebuffer to the UI
 └────────┬────────────────────┘
          │
        guest (offline, no NIC)
```

## 3. Process model

- The UI launches and monitors the supervisor; the supervisor launches the QEMU processes (ADR-0002).
- Single supervisor instance per catalog; processes identified by handle, creation time and `run_id`, never by PID alone. **[verified in F0: `F0-06` §3, reassigned PID recognized by creation time]**
- Windows: QEMU created with `CreateProcessW` and structured arguments, inside the supervisor's Job Object **from creation** (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` without breakaway, inheritance limited to the three standard handles (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), `DETACHED_PROCESS`, environment reduced to `SystemRoot`. **[verified in F0: `F0-06` §1]**
- Supervisor crash (Windows): the operating system closes the job and terminates QEMU (p50 118.9 / 110.4 ms, max 143.9 ms, 10 cycles, 0 leftovers). **[verified in F0: `F0-06` §2a]**
- External death of QEMU: detected by waiting on the handle and classified by exit code and job notifications. **[verified in F0: `F0-06` §2b]**
- Emergency Stop: `TerminateJobObject` with a dedicated code, completion after the observed exit and `ActiveProcesses = 0` (p95 640.2 ms over 30 samples at the desktop, 273.3 ms over 100 samples during boot). **[verified in F0: `F0-06` §2c]**
- Ordinary shutdown: `system_powerdown` with a configurable timeout, then `quit`, then `TerminateJobObject`. **[verified in F0 as a procedure: `F0-06` §2d]**
- Limits: CPU hard cap and job memory limit applied before spawn, configurable per VM (ADR-0017, partially accepted). **[verified in F0: `F0-08`; per VM in F1: `F1-10`]**
- Linux: proposed combination `PR_SET_PDEATHSIG` + pidfd + delegated cgroup v2 with `cgroup.kill` (ADR-0015, Proposed). **[verified in F0 only for the process lifecycle in WSL2: `F0-07` §4; QEMU on a physical host in R2a]**
- UI crash: agent sessions are revoked; the fate of manual VMs is a setting (default: terminate them immediately; ADR-0015 part B). **[verified on Windows in F1: `F1-09` (P-2)]**
- Recovery of the UI or application does not automatically reactivate agent sessions.
- The supervisor is not an always-on service in the MVP.

## 4. VM state machine

Observed state derived from the process and QMP, not from the database alone (section 6.3).

```text
            start                 QMP ready + process alive
 Stopped ─────────► Starting ───────────────────────────► Running
   ▲                   │                                    │
   │                   │ error / timeout                    │ shutdown, force-stop, Emergency Stop
   │                   ▼                                    ▼
   │                Failed ◄──── unexpected exit ───── Stopping
   │                   │                                    │
   │                   ▼                                    │ process terminated and files released
   │              Recovering ── reconciliation ───┐         │
   └───────────────────────────────────────────────┴────────┘
```

| State | Entry | Allowed operations | Notes |
| --- | --- | --- | --- |
| Stopped | Process terminated and files released | start, configuration, media, baseline, restore | Only state for operations on disks, NVRAM and media |
| Starting | Spawn started | force-stop | New `run_id` assigned |
| Running | Process alive and QMP ready | shutdown, force-stop, reset, screenshot, input, observe | Requests with a previous `run_id` rejected |
| Stopping | Shutdown requested | force-stop | Stop declared complete only once the process end is observed |
| Failed | Startup error or unexpected exit | recover, diagnostics | No automatic promotion to baseline |
| Recovering | Reconciliation after a crash | no external mutation | Applies the journal, checks files and processes |

**Guest stopped on a virtual CPU error.** The process can be alive while the guest no longer runs. On a guest triple fault, real hardware and KVM reset the machine. Under WHPX, QEMU 11.1 logs `WHPX: Unexpected VP exit code 4`, stops the vCPUs and leaves run state `internal-error` (QMP `STOP`). The VM stays `Running` in this table, because the process is alive and QMP is ready. Each run watches its QMP events (`qemu_backend::vcpu_fault`). On `STOP` it reads `query-status`. For `internal-error` it behaves as hardware: `system_reset`, then `cont` once QEMU has moved the guest to `prelaunch`. The limit is 3 automatic resets in 120 s. The next fault leaves the guest stopped and is reported in the VM summary (`vcpu_halt`). The UI shows it with reset and force stop. Input is refused with `state_conflict` (`reason=vcpu_halted`) instead of QEMU's `VM not running`. Each decision is logged and audited (`vm_auto_reset`, actor `system`). A user reset of such a guest sends `cont` after the reset. `guest-panicked` is reported, never reset automatically. **[verified with synthetic QMP: `R2c-01`, 2026-10-14; not yet against the real Vista fault]**

## 5. Per-VM operation queue

- One mutation queue per VM; incompatible operations blocked (`vm_busy`, `state_conflict`).
- Each operation has an `operation_id`; each execution has a `run_id`; revision checks on configuration and policy.
- Requests referring to a previous `run_id` are invalidated after a restart or restore.
- Automatic retries only for idempotent operations; GUI input never repeated after a timeout.
- Long-running commands return a state trackable by `operation_id`.
- The Emergency Stop path does not go through the queue (ADR-0009).
- Double authorization check: on receipt and immediately before forwarding to the backend (section 7.3).

## 6. Policy as intersection

```text
effective_policy = global_limits ∩ vm_policy ∩ session_grant ∩ available_host_capabilities
```

- No layer widens the previous ones; a missing host capability narrows, it is not ignored.
- The same policy applies to the UI and MCP; the UI is not an additional trusted layer.
- The revision of the applied policy is recorded in every session report (section 9).
- Public errors: `unauthenticated`, `permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict` (semantics of the authentication codes in ADR-0026).

## 7. Console

- Internal RFB client in the UI process, separate `rfb-client` crate (ADR-0024), restricted subset, read-only by construction; framebuffer drawn as an egui texture updated by rectangles (ADR-0003). **[verified in F0: `F0-04` §1–§3, app framebuffer identical to `screendump`]**
- Primary display channel: **VNC over a Unix socket** in the private per-execution directory created by the supervisor, user-only ACL; documented alternative VeNCrypt X.509 with a client certificate (rustls), not the default because shutting down the TLS session does not stop reading (ADR-0013). **[verified in F0: `F0-04` §1, §5, §6; second user pending]**
- Display credential issued by the supervisor per execution and VM, distinct from QMP and IPC, delivered to the UI and revoked with the session. **[pending: to be implemented and tested by F2, ADR-0013]**
- Fallback: framebuffer from QMP `screendump` at a limited rate, temporary file in a private directory chosen by the backend, delivered to the UI via IPC; latency declared in `docs/COMPATIBILITY.md`. **[feasibility verified in F0: capture p95 19.2 ms, `F0-04` §7; not needed on the first host]**
- Input: winit events with physical scancodes → QEMU qcodes for the declared layouts (US, IT) → IPC → authorized bridge → QMP. Keys released on focus loss, disconnection or owner change. **[verified in F0 from `egui::Event` onwards: `F0-04` §4; physical keyboard through winit pending]**
- No reading of the host clipboard: egui-winit is used in a patched copy that does not read it (`vendor/egui-winit`, F1-13); the UI filter remains as a second defense; labels are not selectable, so copying to the clipboard is not possible either.
- Screenshots produced by the backend even with the console closed, with `run_id` and display geometry.

### 7.1 Control channels

| Channel | Transport | Owner | Status |
| --- | --- | --- | --- |
| QMP | Unix socket `qmp.sock` in the private per-execution directory; a single client, the supervisor, connected right after spawn (QEMU serves one client at a time) | supervisor | verified in F0 (`F0-02` §4.3, `F0-04` §6, `F0-06`); named pipe created by QEMU discarded (ADR-0012) |
| Display | Unix socket `vnc.sock` in the private per-execution directory | UI (RFB client), with the endpoint received from the supervisor | verified in F0 (`F0-04`); per-execution credential pending (ADR-0013) |
| UI/MCP ↔ supervisor IPC | named pipe with ACL (Windows), Unix socket with restrictive permissions (Linux) | supervisor | from F1 |

## 8. Crate structure (section 6.2)

```text
apps/desktop/              egui/eframe UI
crates/vm-core/            models, states, use cases, policy and sessions
crates/qemu-backend/       processes, QMP, probe, server side of the console
crates/rfb-client/         read-only RFB client, used by the UI (ADR-0024)
crates/vm-storage/         SQLite, manifests, baselines, journal
crates/lightvm-service/    supervisor and local IPC
crates/lightvm-proto/      IPC/MCP types and generated JSON Schema
crates/guest-detect/       guest system detection from installation ISOs (ADR-0027)
crates/mcp-server/         added in F5
tests/{integration,security,e2e}/
docs/{adr,verification}/
```

Dependency direction (not fixed by the roadmap; recorded here and in ADR-0024):

```text
apps/desktop ──► lightvm-proto, rfb-client, lightvm-service (IPC client only, without `supervisor`)
lightvm-mcp ──► lightvm-proto, lightvm-service (IPC client only)
rfb-client ──► (no dependency on QMP, processes, storage or supervisor)
lightvm-service ──► vm-core, qemu-backend, vm-storage, lightvm-proto
qemu-backend ──► vm-core
vm-storage ──► vm-core
vm-core ──► (no dependency on UI, processes or I/O)
guest-detect ──► lightvm-proto
```

**Guest hardware profiles.** The QEMU command line is built from a closed set of profiles selected by the guest system and the firmware, never from free-form options. Linux guests use the ADR-0011 profile (`pc-q35`, UEFI, virtio devices). Windows XP, Vista, 7, 8.1 and 10 use the ADR-0027 profiles: emulated AHCI/IDE disk, standard `VGA`, `usb-tablet` on an xHCI or UHCI controller and the built-in PS/2 keyboard; SeaBIOS (`-bios`, pinned hash) or, for 8.1 and 10, UEFI; `pc-i440fx` only for XP. Every profile has no NIC (ADR-0004), uses the same nominal CPU model and is probed with its own machine type and firmware (ADR-0007). Disallowed system/firmware combinations are rejected in `vm-core` and in the backend. A VM without NVRAM (Legacy BIOS) has baselines with the disk only.

The UI depends on the protocol and the RFB client, never on the backend or storage; the MCP adapter depends only on the protocol. Section 6.2 places the console in `qemu-backend`, while section 6.4 runs the RFB client in the UI process: ADR-0024 separates the client into `crates/rfb-client` and leaves the server side of the console in `qemu-backend`. New crates (`vm-network`, `ai-control-bridge`, `vm-security`, `agent-runtime`, `test-runner`) only when they have stable responsibilities and dependencies; no plugin framework or second backend in the MVP. The F0 spikes (`spikes/`) are not product code and are not in the dependency graph of any shipped binary (since 2026-10-11 `f0-console` is no longer used by the UI; the console goes through the supervisor's display ticket, ADR-0013).

## 9. Review checklist (section 6.3)

To be applied to every change that touches processes, QEMU, files, IPC or MCP.

- [ ] Spawn with structured arguments, never strings for a shell; approved and verified QEMU path.
- [ ] No path concatenation in interpretable QEMU options (`-drive` and similar); explicit disk format; protocols on an allowlist.
- [ ] Media only from the approved registry, with identity/hash verified at use; opening protected against symlinks, junctions and substitutions between check and use.
- [ ] No mounting of guest filesystems on the host.
- [ ] Child environment, handles and descriptors cleaned up; no provider or MCP credentials passed to QEMU.
- [ ] QMP internal only, on a private channel; no unauthenticated TCP endpoint; no `qmp.execute`.
- [ ] Backend with a reduced API (probe, create, start, shutdown, force-stop, status, observe, input, baseline); unavailable capabilities produce explicit errors.
- [ ] VM state derived from the process and QMP, not from the database alone.
- [ ] Mutations through the per-VM queue, with `operation_id`, `run_id` and revision checks.
- [ ] Effective policy computed as an intersection; no widening layer.
- [ ] Retries only for idempotent operations; no GUI input repeated after a timeout.
- [ ] DB + filesystem operations with journal and reconciliation.
- [ ] Types and JSON Schema generated from the single source; stable public errors.
- [ ] Dependencies with pinned versions and updated lockfile; tested QEMU/firmware versions recorded.
- [ ] Text from the guest, logs and reports rendered as text; no link opened automatically.
- [ ] No secrets in process arguments, logs, reports or examples.

## 10. Outcome of the items to validate in F0

Summary of the review `docs/verification/F0-09-evidence-review.md`.

| Item | ADR | F0 outcome | Evidence |
| --- | --- | --- | --- |
| eframe build on Windows/Linux and rendering backend, including over RDP | 0014 | verified with limits (glow default, wgpu selectable, WARP degraded; Linux WSL2 only; RDP not run) | `F0-01`, `F0-04`, `F0-07` |
| QEMU build, WHPX accelerator, private QMP, stop | 0010, 0012, 0007 | verified with deviations (`-cpu qemu64`, expired signature) | `F0-02` |
| Guest and firmware | 0011 | verified (live boot in F0; disk installation and boot from disk later verified in F1-17, `F1-03`) | `F0-03`, `F0-02` §9 |
| RFB console on the protected channel, input, screenshots | 0003, 0013, 0024 | verified with pending items (physical keyboard, second user, display credential) | `F0-04` |
| Baseline/overlay/NVRAM and disk exclusion | 0005, 0025 | verified (5 cycles; overlay in use not protected from third parties of the same user) | `F0-05` |
| Process model and crashes | 0002, 0009, 0015 | verified with a pending item (UI crash) | `F0-06`, `F0-07` |
| Linux/KVM | 0016 | not run (lab unavailable); "verified on Windows only" proposed | `F0-07` |
| Resource limits and first measurements | 0017, 0018 | identified (thresholds and under-pressure tests pending) | `F0-08` |
