# F0-08 — OS limits for CPU, memory and storage; first consumption measurement (Windows)

- Execution date (UTC): 2026-10-09
- Executed by: development agent (spike `spikes/f0-supervisor`, subcommands `limits-probe`, `cpu-cap`, `mem-limit`)
- Roadmap step: section 10 F0 ("identify the OS limits available for CPU, memory and storage and record a first consumption measurement"); section 7.2 point 8 and following paragraphs; section 13
- Related ADRs: ADR-0017, ADR-0002, ADR-0009
- Overall outcome: **partial** — Job Object CPU and memory limits verified with real effect on QEMU under WHPX; hard storage quota not available without administrator rights; measurements on a host shared with other agents; Linux part in `F0-07`

## Purpose

1. Establish which Job Object limits can be applied to the QEMU tree **without elevation** and which cannot.
2. Verify the real effect (not just the metric) of `JOB_OBJECT_LIMIT_PROCESS_MEMORY`/`JOB_OBJECT_LIMIT_JOB_MEMORY` and of the CPU hard cap (`JOBOBJECT_CPU_RATE_CONTROL_INFORMATION`) on QEMU with an active guest, including the attribution of WHPX vCPU time to the QEMU process.
3. Record the real capacity of the lab and a first measurement of CPU, memory and space of a session.

It does not demonstrate: limits on Linux (cgroup v2, `F0-07`), I/O and network rate control (set but effect not measured), tests under volume-filling pressure or screenshot/log floods, UI/supervisor manager metrics (the UI does not yet exist as a measurable process).

## Environment

| Item | Value |
| --- | --- |
| Host | see the "Lab capacity" table |
| OS and build | Windows 11 Pro 10.0.26200; non-administrator user |
| Accelerator | WHPX; Hyper-V and VBS/HVCI active (`F0-02`, 1.2) |
| Toolchain | Rust 1.99.0 MSVC; `windows-sys` 0.61.2 |
| QEMU | 11.1.0 portable (`F0-02`, 2.2) |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso` live, `-cpu qemu64 -smp 2 -m 4096`, OVMF firmware in pflash |
| Other conditions | Shared host: in parallel the QEMU instances of the F0-04 agent and (for part of the day) WSL were active (`vmmemWSL` with about 6 GiB of working set at the first reading). Free RAM detected was 8.1 GiB out of 31.6. CPU measurements are per process/job and do not depend on the other loads, but boot times do |

### Lab capacity

Source: `lab/logs/f0-08-host-capacity.txt` (CIM, `Get-PhysicalDisk`, without elevation).

| Item | Value |
| --- | --- |
| OS | Microsoft Windows 11 Pro 10.0.26200, build 26200 |
| CPU | 12th Gen Intel Core i7-12800HX, 16 cores / 24 logical processors |
| RAM | 31.6 GiB physical; commit limit 63.6 GiB (paging file `C:\pagefile.sys` 32 768 MiB) |
| Lab volume | E: NTFS, 1 863.0 GiB total, 744.2 GiB free, on Samsung SSD 970 EVO Plus 2 TB (NVMe) |
| Other volumes | C: NTFS 952.8 GiB (308.8 free); D: NTFS 931.5 GiB (515.0 free); all on NVMe SSD |
| Comparison with the section 13 reference | exceeds 4 cores/8 threads, 16 GiB and 100 GiB free |

## Procedure

```text
target\release\f0-supervisor.exe limits-probe                                        → lab/logs/f0-08-limits-probe.log
target\release\f0-supervisor.exe cpu-cap --rate 0    --seconds 240                   → f0-08-cpu-rate0.log
target\release\f0-supervisor.exe cpu-cap --rate 2500 --seconds 150                   → f0-08-cpu-rate2500.log
target\release\f0-supervisor.exe cpu-cap --rate 500  --seconds 150                   → f0-08-cpu-rate500.log
target\release\f0-supervisor.exe cpu-cap --rate 200  --seconds 150                   → f0-08-cpu-rate200.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 1024 --seconds 60   → f0-08-mem-process-1024.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 4300 --seconds 150  → f0-08-mem-process-4300.log
target\release\f0-supervisor.exe mem-limit --kind job     --limit-mb 4300 --seconds 150  → f0-08-mem-job-4300.log
target\release\f0-supervisor.exe mem-limit --kind process --limit-mb 6144 --seconds 150  → f0-08-mem-process-6144.log
```

- `--rate` is the `CpuRate` value of `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` with `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`, in hundredths of a percentage point **of the cycles of the whole system** (all 24 logical processors): 2500 = 25 % of the system = 6 logical processors; 500 = 5 % = 1.2 processors; 200 = 2 % = 0.48 processors. Applied to the job before the spawn (QEMU is born already in the job, `F0-06`).
- CPU is sampled every second from `JobObjectBasicAccountingInformation` (`TotalUserTime + TotalKernelTime` of the job, i.e. QEMU plus its `conhost.exe`) and compared with `GetProcessTimes` of QEMU alone; memory with `K32GetProcessMemoryInfo` (working set, peak, `PrivateUsage`) and with the job's `PeakProcessMemoryUsed`/`PeakJobMemoryUsed`.
- Each test starts the boot of the ISO (real guest CPU load: firmware, kernel, decompression, desktop startup) and, in the 240 s test without cap, continues on the idle live desktop.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, without breakaway | verified | `limit_flags=0x2000`; see `F0-06` |
| `JOB_OBJECT_LIMIT_PROCESS_MEMORY` and `JOB_OBJECT_LIMIT_JOB_MEMORY` without elevation | verified | set (`limit_flags=8960` = 0x2300), read back 1 GiB / 2 GiB |
| CPU hard cap without elevation | verified | `SetInformationJobObject(JobObjectCpuRateControlInformation)` succeeded |
| Effect of a memory limit below the guest RAM | verified | QEMU exits immediately: `cannot set up guest memory 'pc.ram'`, exit 1, `PROCESS_MEMORY_LIMIT` notification |
| Effect of a memory limit between startup and steady state | verified (unfavourable behaviour) | QEMU terminates after about 34 s with **0xC0000005** (access violation), notifications `*_MEMORY_LIMIT` + `ABNORMAL_EXIT_PROCESS` |
| WHPX vCPU time attributed to the QEMU process | verified | up to 3.09 logical processors consumed by the job during boot; cumulative job = cumulative QEMU |
| CPU hard cap 25 % with active guest | verified, but **not binding** on this host | maximum observed 11.6 % of the system: QEMU with 2 vCPUs does not reach 25 % of 24 logical processors |
| CPU hard cap that actually binds (5 %, 2 %) | verified | p95 5.5 % and 2.4 % of the system; boot visibly slowed |
| Working set limit (`JOB_OBJECT_LIMIT_WORKINGSET`) | **not applicable without elevation** | error 1314 "Il privilegio richiesto non appartiene al client" |
| Realtime priority class on the job | **not applicable without elevation** | error 1314 |
| Job I/O rate control | set, effect not measured | `SetIoRateControlInformationJobObject` returns success (10 MB/s) |
| Job network rate control | set, effect not measured (the guest has no NIC) | `JobObjectNetRateControlInformation` succeeded |
| Hard NTFS storage quota | **not verifiable without elevation** | `fsutil quota query E:` → "Errore 5: Accesso negato." |
| First consumption measurement | verified (shared host) | following tables |

### Memory limits

| Test | Private bytes at startup | Observed outcome | Job notifications |
| --- | --- | --- | --- |
| `PROCESS_MEMORY` 1024 MiB, guest 4096 MiB | — | QEMU exits after 0.2 s, exit 1, stderr `cannot set up guest memory 'pc.ram'` | `NEW_PROCESS` ×2, `PROCESS_MEMORY_LIMIT`, `EXIT_PROCESS` ×2, `ACTIVE_PROCESS_ZERO` |
| `PROCESS_MEMORY` 4300 MiB | 4 215 MiB | QEMU starts, the boot proceeds, then QEMU terminates after 34.3 s with code 3221225477 (0xC0000005) without a message on stderr | `PROCESS_MEMORY_LIMIT`, `ABNORMAL_EXIT_PROCESS`, `EXIT_PROCESS` (conhost), `ACTIVE_PROCESS_ZERO` |
| `JOB_MEMORY` 4300 MiB | 4 215 MiB | identical: exit after 34.5 s, 0xC0000005 | `JOB_MEMORY_LIMIT`, `ABNORMAL_EXIT_PROCESS`, … |
| `PROCESS_MEMORY` 6144 MiB | 4 214 MiB | no effect for 150 s; private 4 536 MiB, job peak 4 537 MiB; final Emergency Stop in 602 ms | only `NEW_PROCESS` ×2 |

Observations:

- With `-m 4096` QEMU immediately commits about 4 215 MiB of private bytes (the guest RAM is allocated with commit at startup) and rises to about 4 536 MiB during boot. The limit must therefore be sized on guest RAM plus a measured margin (here about 440 MiB).
- A limit too low at startup produces a clean, diagnosable error; a limit exceeded **at steady state** produces a QEMU crash (access violation) instead of a handled error. The supervisor can nevertheless recognize the cause from the `PROCESS_MEMORY_LIMIT`/`JOB_MEMORY_LIMIT` notification received on the job's completion port before `ABNORMAL_EXIT_PROCESS`.
- Working set and private bytes do not coincide: once the desktop is reached the working set is about 4 100 MiB (the guest RAM becomes resident), at the start of the boot about 118 MiB with private bytes already over 4 200 MiB.

### CPU hard cap and vCPU attribution

Percentages per 1 s sample. "% system" = job CPU time / (real time × 24 logical processors); "% of one core" = CPU time / real time.

| Cap | Duration | % system p50 / p95 / max | % of one core p50 / p95 / max | Screen at the end of the test (non-black pixels) |
| --- | --- | --- | --- | --- |
| none | 240 s (boot + desktop) | 4.6 / 8.9 / 12.9 | 109.3 / 212.4 / 309.3 | desktop (99.9 %) |
| 25 % (2500) | 150 s | 4.9 / 9.1 / 11.6 | 118.7 / 218.7 / 277.9 | 36.6 % (desktop loading) |
| 5 % (500) | 149 s | 4.0 / 5.5 / 8.8 | 96.9 / 131.2 / 210.9 | 1.7 % (boot not finished) |
| 2 % (200) | 150 s | 1.8 / 2.4 / 5.9 | 43.7 / 57.8 / 140.6 | 3.6 % (initial phase/GRUB) |

- **Attribution:** the cumulative CPU time of the job coincides with that of QEMU alone (for example 198.5 s versus 198.5 s at t = 150 s without cap), so `conhost.exe` is negligible; the job reaches 3.09 logical processors in one second, beyond the 2 vCPUs, so it includes vCPUs and I/O/emulation threads. On this host (Hyper-V with VBS) guest execution time under WHPX **is** accounted to the vCPU threads of the QEMU process and subject to the job cap; in the survey of `vmmem*` processes done with a WHPX VM at the desktop (`f0-08-host-capacity.txt`) no `vmmem` process appears for the WHPX VM; the `vmmemWSL` observed earlier belongs to WSL.
- **Real effect:** with a 2 % cap the working set stays at about 115 MiB for all 150 s (the guest gets no further than the first phases), with a 5 % cap the boot is not finished at 150 s, while without a cap the desktop is present at 240 s and the CPU drops below 10 % of one core from about t = 200 s. The cap slows the guest down, it does not terminate it.
- **Overshoots in the 1 s sample:** the maximum exceeds the cap (8.8 % with a 5 % cap, 5.9 % with a 2 % cap) while the p95 stays close; the system's regulation happens over scheduling intervals and does not guarantee compliance in every second. It must be declared as an "average" control, not an instantaneous one.
- **25 % cap:** the requirement "CPU measured below 25 %" is met (maximum 11.6 %), but it is also met without a cap (maximum 12.9 %): on a host with 24 logical processors a cap at 25 % of the system does not limit a VM with 2 vCPUs. A meaningful limit must be expressed in logical processors and converted: for "at most N equivalent vCPUs" `CpuRate = N × 10000 / processori_logici`.

### First consumption measurement

Sources: `f0-08-cpu-rate0.log`, `f0-08-mem-process-6144.log`, `f0-06-scenario-c-desktop.log`, `f0-05-*`.

| Measurement | Value | Notes |
| --- | --- | --- |
| QEMU CPU during boot (0–180 s, without cap) | p50 1.09 logical processors, peaks 2–3.1 | 4.6 % p50 of the system |
| QEMU CPU with idle live desktop (t = 200–240 s) | 0–6.2 % of one core (0.00–0.26 % of the system) | samples at 5 s; the desktop shows the "Welcome to Ubuntu" window |
| Cumulative CPU to reach the desktop | about 290 s of CPU for about 200 s of boot | varies with the host load |
| QEMU private bytes | 4 215 MiB at startup, 4 536 MiB at steady state | guest 4 096 MiB |
| QEMU working set | 118 MiB at startup, about 4 100 MiB at the desktop | |
| Overlay after a live session without writes | 197 120 bytes (unchanged since creation) | `F0-05`, 5 cycles |
| Overlay after a session with an interrupted guest `dd` | 327 483 392 and 1 132 920 832 bytes on disk | `F0-05`, 8.1 |
| OVMF vars per run | 540 672 bytes | |
| Log of a QEMU session | stderr about 389 bytes, stdout 0 bytes | plus the spike's log (a few KiB) |
| Screendump | 3 072 016 bytes per 1280×800 PPM (about 360 KiB converted to PNG) | fixed size per resolution |
| QMP socket | 0-byte file, orphaned if QEMU was terminated forcibly | removed by the spike |

## Limits

- Host shared with other agents: boot times and latencies depend on the load; per-job CPU percentages do not.
- 1 s sampling: does not observe the behaviour of the cap over shorter intervals.
- A single guest configuration (2 vCPUs, 4 GiB) and a single load (boot and idle desktop); no sustained synthetic load in the guest.
- I/O rate and net rate only set; no measurement of the effect.
- NTFS quota not queryable without administrator rights; no tests under volume-filling pressure, nor of screenshot/log floods (required by ADR-0017, criterion 4): **not executed**.
- The QEMU crash due to a memory limit at steady state is observed in 2 tests; the internal cause was not studied (unhandled allocation failure in QEMU or in WHPX).

## Required actions

For the user (as administrator, optional, none was performed):

1. To evaluate a hard storage quota: `fsutil quota query E:` and, to test it, enable NTFS quotas on a test volume (`fsutil quota track E:` / `fsutil quota enforce E:`), then repeat the filling tests. NTFS quotas are per user and per volume, not per directory: they do not distinguish LightVM from other programs of the same user.
2. Testing `JOB_OBJECT_LIMIT_WORKINGSET` requires the privilege named by error 1314 (grant through local security policy or elevated execution): to be evaluated only if the working set became a required control; it is not proposed for the MVP.

Backlog (`docs/RELEASE_PLAN.md`): tests under storage pressure and artifact floods; measurement of the effect of the I/O rate; CPU/memory measurements of UI and supervisor when they exist as processes.

## Proposed decisions

For **ADR-0017**:

1. **CPU (Windows):** Job Object hard cap (`JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`) applied to the job before the spawn, expressed by the policy in equivalent logical processors and converted with `CpuRate = N × 10000 / processori_logici` (rounded down, minimum 1). Declare it as an average limit over scheduling intervals, not instantaneous; binding also for the WHPX vCPUs on this host (to be re-verified for every enabled host).
2. **Memory (Windows):** `JOB_OBJECT_LIMIT_JOB_MEMORY` on the whole job (covers QEMU and any descendants), sized as guest RAM + measured margin (initial proposal: guest RAM + 512 MiB for this build and configuration), with a check at startup that the initial commit is below the limit. Exceeding it at steady state causes a QEMU crash: the supervisor classifies it as `resource_limit` using the `JOB_MEMORY_LIMIT` notification of the completion port.
3. **Working set and priority:** not used (they require privileges).
4. **Storage:** no hard quota available without administrator rights, and NTFS quotas do not isolate LightVM: base profile with an application budget, monitoring and stop, declared risk of overshoot; a policy that requires a hard quota does not start on this host. Per-session accounting: overlay (from 192 KiB up to the virtual size), vars 540 672 bytes, logs, screendumps (3 MiB per 1280×800 PPM before conversion), journal `.partial`/`trash/` files (`F0-05`, 9).
5. Thresholds, check frequency and reserve remain to be set after the pressure tests (not executed).
