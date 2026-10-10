# ADR-0017 — OS limits and storage budget

- Status: Partially accepted (2026-10-10): **per-VM limits** set by the user and applied by the supervisor on Windows; host thresholds, complete storage accounting, stress tests and Linux still open (F3, R2a)
- Date: 2026-10-09; partial decision: 2026-10-10
- Owner: project owner

## Context

The AI profile requires CPU/memory limits applied to the process tree, in addition to the guest's virtual configuration, and an overall storage budget with a host reserve and a declared nature of the control (roadmap, section 7.2, point 8). Guest vCPUs and RAM are not limits on the QEMU process; the virtual size of the disk does not limit overlays, logs and screenshots. In F0 the effective limits must be tested and the thresholds, maximum check frequency, reserve and artifact limits must be set.

## Decision

To be taken. To be settled, for Windows and Linux:

- CPU and memory mechanism on the QEMU tree, with the effects of the accelerator observed;
- nature of the storage control: hard OS quota (if available) or application budget with monitoring and stop, with the risk of overrun declared;
- thresholds, maximum check frequency, reserve for the manifest and minimal outcome, limits for screenshots and logs;
- level of additional confinement available (section 7.2, penultimate paragraphs), distinguishing guest configuration, API control and host process sandbox.

Criteria:

1. The limit is observed on the QEMU process and its descendants, not inferred from a metric.
2. A missing mandatory control blocks AI Mode; a missing additional control is declared as a limit.
3. A policy that requires a hard quota does not start without that capability.
4. Stress tests (filling up, screenshot/log flood).

### State of the evidence at the F0 review

Mechanisms **identified** on Windows (`F0-08`), proposed and not yet decided because thresholds and stress tests are missing (criterion 4):

1. CPU: Job Object hard cap (`JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`) applied before spawn, expressed in equivalent logical processors (`CpuRate = N × 10000 / logical_cpus`). Binding also for WHPX vCPUs on this host (vCPU time accounted to the QEMU process); honoured on average, not in every second (5% cap: p95 5.5%, max 8.8% on 1 s samples).
2. Memory: `JOB_OBJECT_LIMIT_JOB_MEMORY` on the whole job, sized as guest RAM plus a measured margin (QEMU with `-m 4096`: 4 215 MiB of private bytes at startup, 4 536 MiB at steady state). A limit exceeded at steady state makes QEMU terminate with an access violation (0xC0000005): the supervisor classifies `resource_limit` from the `JOB_MEMORY_LIMIT` notification of the completion port.
3. Working set and realtime priority of the job: not applicable without elevation (error 1314); not used.
4. Storage: no hard quota available without administrator rights (`fsutil quota query` → access denied) and NTFS quotas are per user and volume, not per LightVM: profile with application budget, monitoring and stop, risk of overrun declared; a policy that requires a hard quota does not start on this host. Per-session accounting: overlay, vars 540 672 bytes, logs, screendumps (3 072 016 bytes per 1280×800 PPM), `.partial` files and the journal's `trash/`.
5. Job I/O rate and network rate controls: can be set, effect not measured.

Linux: delegated cgroup v2 (`memory.max`, `cpu.max`, `pids.max`, `cgroup.kill`) verified in WSL2 only **[processes]** with test children (`F0-07` §4); on QEMU with KVM deferred to R2a.

Missing to close the ADR: thresholds, maximum check frequency, host reserve, limits for screenshots and logs, stress tests (volume fill-up, screenshot and log flood), measurement of the effect of the I/O rate.

### Decision of 2026-10-10 (project owner)

**Limits are set in the VM settings**, can be given **on the command line when opening a VM** and, in F5, **via MCP**. Implementation and evidence in `docs/verification/F1-10-vm-limits-settings.md`:

- `VmConfig.limits`: `cpu_max_percent` (5–100, default 100), `memory_margin_mib` (256–16384, default 1024), `storage_budget_gib` (optional). Validated by the core; never applied by the client.
- Windows: per-run Job Object with CPU hard cap (`vcpus × percent / logical CPUs`) and job memory (guest RAM + margin), applied before spawn and read back; job memory below the RAM → startup refused.
- Storage budget: checked at every startup on the VM's files excluding the main disk; over budget the startup is refused (`resource_limit`). Complete accounting and host thresholds in F3.
- Changes: only with the VM powered off, expected revision, disk only growing, audit `vm_configure`; agent sessions do not modify VMs in F1. In F5 the MCP tools may propose limits that are **only more restrictive** than the user's (the user's policy remains the ceiling, section 7.2).
- Command line: `lightvm-desktop --open-vm NOME [--create] [--vcpu N] [--ram MIB] [--disk GIB] [--cpu-max %] [--memory-margin MIB] [--disk-budget GIB|none]`, on the same authenticated channel as the UI.

Still open to close the ADR: host reserve, check frequency, screenshot and log limits, stress tests (volume fill-up, flood), measured effect of the CPU cap with a loaded guest; Linux part (cgroup v2) in R2a.

## Alternatives considered

Candidates to be verified:

- Windows: Job Object CPU and memory limits (ADR-0002).
- Linux: cgroup v2 controllers (`cpu.max`, `memory.max`), consistent with ADR-0015.
- Storage: filesystem project quotas (where available) versus application budget with monitoring and stop.

## Consequences

- Storage accounting includes baseline, retained overlays, QCOW2 overhead, firmware, media registry, logs, screenshots and metadata (section 8).
- No promise of protection against the volume filling up because of other host processes.

## Required evidence

- Job Object limits, real effect on QEMU/WHPX and first consumption measurement: `docs/verification/F0-08-resource-limits-measurements.md` (partially verified; stress tests not run).
- Storage accounting and journal: `docs/verification/F0-05-baseline-overlay-firmware.md` §9.
- cgroup v2 in a non-accelerated environment: `docs/verification/F0-07-linux-kvm.md` §4 (**[processes]**).
- Stress tests and thresholds: F1/F3 backlog in `docs/RELEASE_PLAN.md`.

## References

Roadmap, sections 7.2, 8, 10 (F0), 13, 14, 15 (step 5), 17.
