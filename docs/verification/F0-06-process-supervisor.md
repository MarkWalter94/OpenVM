# F0-06 — Process supervisor: Job Object, crashes, Emergency Stop, identity

- Execution date (UTC): 2026-10-09
- Executed by: development agent (spike `spikes/f0-supervisor`)
- Roadmap step: section 15, step 4 of F0 ("process supervision"); section 10 F0 ("supervisor process model… supervisor crash"); sections 6.1, 6.3, 7.3, 13
- Related ADRs: ADR-0002, ADR-0009, ADR-0012, ADR-0015, ADR-0017
- Overall outcome: **partial** (supervisor, QEMU and Emergency Stop verified; UI crash and QMP channel test from a foreign process not executed)

## Purpose

Demonstrate on the first host that a Rust supervisor:

1. starts QEMU without a shell, with a cleaned environment and limited handle inheritance, inside a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and without breakaway;
2. in case of its own abnormal death has QEMU terminated by the operating system (scenario a);
3. detects and qualifies the death of QEMU caused from outside (b);
4. performs the Emergency Stop with `TerminateJobObject`, measuring the latency up to the observed exit (c), and the graceful shutdown with escalation (d);
5. recognizes QEMU by handle and creation time, not only by PID.

It does not demonstrate: the separate UI process and its crash (the UI does not yet exist as a process that starts the supervisor), authenticated IPC, rejection of a foreign process on the QMP socket, behaviour on Linux (`F0-07`), operating-system timing guarantees.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i7-12800HX (16 cores / 24 threads), 31.6 GiB RAM, NVMe SSD |
| OS and build | Windows 11 Pro 10.0.26200; non-administrator user, no elevation |
| Accelerator | WHPX (`query-accelerators` → `whpx`) |
| Toolchain | Rust 1.99.0 MSVC (Build Tools 2022 environment, `F0-01`); `windows-sys` 0.61.2, `uds_windows` 1.2.1, `serde_json` 1.0.151 |
| QEMU | 11.1.0 portable (`lab/qemu/qemu-11.1.0/`, SHA-256 in `F0-02`) |
| Firmware | `edk2-x86_64-code.fd` read-only + per-run copy of `edk2-i386-vars.fd` |
| Guest | `ubuntu-26.04.1-desktop-amd64.iso` (SHA-256 `601e30fb…1bda1f`), live, `-cpu qemu64 -smp 2 -m 4096` |
| Session | local; QEMU headless (`-display none`), QMP over Unix socket in `lab/vms/f0-06/priv/` |
| Other conditions | The spike process was not already in a job (`IsProcessInJob` → `false`). Other agents were running QEMU and WSL in parallel on the same host: latencies are measured on a shared host; leftovers are counted both on the host total and on the QEMU instances of this spike alone |

## Procedure

### Spawn

`spikes/f0-supervisor/src/win.rs`, function `spawn`: `CreateProcessW` with

- `lpApplicationName` = absolute path of `qemu-system-x86_64.exe`; command line built from a vector of arguments with MSVC quoting (unit test `quoting_msvc`); no shell;
- `STARTUPINFOEXW` with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` = only stdin (`NUL`), stdout and stderr (files in `lab/logs/<tag>.stdout.txt`/`.stderr.txt`, duplicated as inheritable only for the call); `bInheritHandles=TRUE` but restricted to the list;
- `PROC_THREAD_ATTRIBUTE_JOB_LIST` = anonymous job created by the supervisor: the process is in the job **from creation**, without the time window of a subsequent `AssignProcessToJobObject`;
- `CREATE_UNICODE_ENVIRONMENT` with an explicit environment block (no inheritance from the supervisor's environment); `CREATE_NO_WINDOW` (`DETACHED_PROCESS` variant tested);
- working directory = QEMU directory.

Job: `CreateJobObjectW(NULL, NULL)` (anonymous, non-inheritable handle), `JOBOBJECT_EXTENDED_LIMIT_INFORMATION.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (`BREAKAWAY_OK` and `SILENT_BREAKAWAY_OK` absent), completion port associated for notifications (`NEW_PROCESS`, `EXIT_PROCESS`, `ACTIVE_PROCESS_ZERO`, memory limits).

The `unsafe` code is confined to `win.rs` with a crate-level `#![allow(unsafe_code)]` justified in `lib.rs` and a `SAFETY:` comment for each block.

### Commands

```text
cargo build --release -p f0-supervisor          (Build Tools 2022 environment)
target\release\f0-supervisor.exe env-probe                                  → lab/logs/f0-06-env-probe.log
target\release\f0-supervisor.exe pid-reuse                                  → lab/logs/f0-06-pid-reuse.log
target\release\f0-supervisor.exe scenario-a --cycles 5 --mode abort --delay-ms 15000 → f0-06-scenario-a-abort.log
target\release\f0-supervisor.exe scenario-a --cycles 5 --mode kill  --delay-ms 15000 → f0-06-scenario-a-kill.log
target\release\f0-supervisor.exe scenario-b --cycles 6                      → f0-06-scenario-b.log
target\release\f0-supervisor.exe scenario-c --samples 30 --wait-min-s 125 --wait-max-s 140 --label desktop → f0-06-scenario-c-desktop.log
target\release\f0-supervisor.exe scenario-c --samples 100 --wait-min-s 5 --wait-max-s 60 --label boot     → f0-06-scenario-c-boot.log
target\release\f0-supervisor.exe scenario-d --cycles 5 --boot-wait-s 130 --powerdown-timeout-s 45 --quit-timeout-s 5 → f0-06-scenario-d-boot130.log
target\release\f0-supervisor.exe scenario-d --cycles 5 --boot-wait-s 180 --powerdown-timeout-s 60 --quit-timeout-s 5 → f0-06-scenario-d.log
```

After each cycle the spike counts the `qemu-system-x86_64.exe` processes with Toolhelp (equivalent to `Get-Process -Name`) and, to distinguish those of other agents, with `Get-CimInstance Win32_Process` filtering the command line on `lightvm-f0-0[568]`.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| 1a. Spawn without shell, structured arguments, explicit path | verified | `CreateProcessW` with absolute `lpApplicationName`; MSVC quoting with unit test |
| 1b. Minimal environment needed by QEMU | verified | empty environment: QEMU exits with `-qmp unix:…: Failed to create Unix socket: Input/output error`; with `SystemRoot` alone QMP and WHPX work |
| 1c. Inheritance limited to the three standard handles | verified by construction, not observed from outside | `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`; no handle enumeration tool available without installations (see Limits) |
| 1d. QEMU in the Job Object from creation, `KILL_ON_JOB_CLOSE`, no breakaway | verified | `IsProcessInJob=true`, `limit_flags=0x2000`, `NEW_PROCESS` notification for QEMU |
| 1e. QEMU's `conhost.exe` in the job | verified | with `CREATE_NO_WINDOW`: child `conhost.exe` present, `IsProcessInJob=true`, PID in the job list; with `DETACHED_PROCESS`: no `conhost.exe` |
| 2a. Supervisor terminated with `abort()` → QEMU terminates (5 cycles) | verified | latency p50 118.9 ms, max 136.0 ms; conhost terminated with QEMU; 0 leftovers |
| 2a'. Supervisor killed from outside (`TerminateProcess`, 5 cycles) | verified | latency p50 110.4 ms, max 143.9 ms; 0 leftovers |
| 2b. QEMU killed from outside → detected by the supervisor (6 cycles) | verified | wait on the handle completed with code 1 (`taskkill /F`) or 0xFFFFFFFF (`Stop-Process -Force`); `EXIT_PROCESS` notifications for QEMU and conhost, `ACTIVE_PROCESS_ZERO`; 0 leftovers |
| 2c. Emergency Stop (`TerminateJobObject`) with guest running: 30 samples at 125–140 s of boot | verified, within the target | p50 394.3 ms, p95 640.2 ms, max 652.0 ms; 0 errors; 0 leftovers (see note on one sample) |
| 2c'. Emergency Stop: 100 samples during boot (5–60 s) | verified, within the target | p50 142.7 ms, p95 273.3 ms, max 354.5 ms; 0 errors, 0 timeouts; 0 cycles with leftovers |
| 2d. Graceful shutdown: `system_powerdown` → `quit` → termination | verified as a procedure; **ACPI shutdown not obtained** | 5 + 5 cycles: no guest `SHUTDOWN` within 45 s (boot) and 60 s (desktop); `quit` completes in 410–788 ms; job termination never needed; 0 leftovers |
| 3. Identity: reused PID not mistaken for QEMU | verified | PID 35072 reassigned to a new process after 1 678 creations; different creation time → `Reused`; with the handle open the PID was not reassigned in 2 000 creations |
| UI crash | not executed | the UI does not yet start the supervisor as a separate process |
| Rejection of a foreign process on the QMP socket (ADR-0012) | not executed | requires a second account or a process with a different identity |

### 1. Environment, console and job membership (`env-probe`)

```text
supervisor (questo processo) già in un job: Ok(false)
[f0-06-env-empty-nowindow] stderr: …qemu-system-x86_64.exe: -qmp unix:E:\OpenVM\lab\vms\f0-06\priv\…sock,server=on,wait=off: Failed to create Unix socket: Input/output error
[f0-06-env-empty-nowindow] messaggi_job=[("NEW_PROCESS", 40820), ("NEW_PROCESS", 44960), ("EXIT_PROCESS", 40820), ("EXIT_PROCESS", 44960), ("ACTIVE_PROCESS_ZERO", 0)]
[f0-06-env-systemrootonly-nowindow] env=[("SystemRoot", "C:\\WINDOWS")]
[f0-06-env-systemrootonly-nowindow] QMP ok accel={"enabled": "whpx", …} status={"running": false, "status": "prelaunch"}
[f0-06-env-systemrootonly-nowindow] qemu_nel_job=true pid_nel_job=[8160, 26184] figli=[(26184, "conhost.exe", Some(true))]
[f0-06-env-systemrootonly-nowindow] limit_flags=0x2000 (KILL_ON_JOB_CLOSE=0x2000, BREAKAWAY_OK=0x800, SILENT_BREAKAWAY_OK=0x1000)
[f0-06-env-systemrootonly-nowindow] stop ms_processo=56.4 ms_job_vuoto=56.5
[f0-06-env-systemrootonly-detached] qemu_nel_job=true pid_nel_job=[15908] figli=[]
[f0-06-env-systemrootonly-detached] stop ms_processo=50.5 ms_job_vuoto=50.5
```

- **Environment variables:** the only variable found to be necessary is `SystemRoot` (without it Winsock initialization fails and QEMU does not create the QMP socket). `PATH` (the DLLs are in the executable's directory), `TEMP`/`TMP`, `USERPROFILE`, `APPDATA` are not needed for the configuration used (firmware with absolute paths, `-no-user-config`). Features not covered (for example temporary snapshot files or graphical displays) might require others.
- **Console:** with `CREATE_NO_WINDOW` Windows creates a `conhost.exe` child of QEMU, which is in the job (inherited at creation) and terminates with it; with `DETACHED_PROCESS` there is no `conhost.exe` at all and QEMU works equally well with stdout/stderr on files. `DETACHED_PROCESS` reduces the tree to a single process.
- With the VM paused (`-S`, 512 MiB) `TerminateJobObject` leads to the exit in about 50–56 ms.

### 2a. Supervisor death

Orchestrator (`scenario-a`) → child supervisor process (`supervise`) → QEMU in the supervisor's job, ISO booting for 15 s. The orchestrator, outside the job, opens its own handles on supervisor, QEMU and conhost (verifying QEMU's creation time) and measures from the observed exit of the supervisor to the observed exit of QEMU.

```text
[f0-06-a-abort-1] supervisor pid=38560 READY pid=5216 creation=134360179801608497 conhost=40604 conhost_in_job=true identita_qemu_coincide=true
[f0-06-a-abort-1] supervisor_uscita=Some(3221226505) qemu_uscita=Some(0) ms_da_morte_supervisor_a_uscita_qemu=… conhost_uscita=Some(Ok(Some(0)))
RIEPILOGO scenario a (abort): n=5 min=105.0 p50=118.9 p95=136.0 max=136.0 media=120.5 fallimenti=0
[f0-06-a-kill-1] TerminateProcess(supervisor) dall'esterno
[f0-06-a-kill-1] supervisor_uscita=Some(1) qemu_uscita=Some(0) ms_da_morte_supervisor_a_uscita_qemu=128.63 conhost_uscita=Some(Ok(Some(0)))
RIEPILOGO scenario a (kill): n=5 min=96.2 p50=110.4 p95=143.9 max=143.9 media=115.1 fallimenti=0
```

`abort()` terminates the supervisor with 0xC0000409; QEMU closed by the job exits with code 0. After each cycle: `residui_spike=0`. In a first series (discarded from the statistics, same termination outcomes) the conhost had not yet been created when the children were read: the spike now waits up to 3 s for it to appear.

### 2b. QEMU killed from outside

```text
[f0-06-b-1] killer="taskkill.exe" … rilevato_dal_supervisor=Some(1) (0x1) messaggi_job=[("NEW_PROCESS", 1284), ("NEW_PROCESS", 25132), ("EXIT_PROCESS", 1284), ("EXIT_PROCESS", 25132), ("ACTIVE_PROCESS_ZERO", 0)] identita=Same
[f0-06-b-2] killer="powershell.exe" … rilevato_dal_supervisor=Some(4294967295) (0xFFFFFFFF) …
[f0-06-b-1] job attivi=0 totali=2 terminati=0
```

The supervisor waits on the process handle and obtains the exit code; the cause is classified as follows: `0xE5E5` = supervisor Emergency Stop (code chosen by the spike), 0 = orderly exit (`quit`), any other value without supervisor action = external termination or crash (with `ABNORMAL_EXIT_PROCESS` for exception codes, observed in `F0-08` for the memory limit). The time from the start of the external command to the observation is 309–1 035 ms, dominated by the startup of `taskkill`/`powershell`: it is not a measurement of the detection latency, which was not isolated. `identita=Same` after the death indicates that, as long as the supervisor holds the handle, the PID stays associated with the terminated process object (see 3).

### 2c. Emergency Stop

Measurement: from just before `TerminateJobObject` to the return of `WaitForSingleObject` on QEMU's handle ("QEMU exit") and up to `ActiveProcesses = 0` in the job accounting ("job empty", includes conhost). The guest is running (`query-status` → `running`) and the screen state is recorded with `screendump` just before.

"Desktop" series (`f0-06-scenario-c-desktop.log`, random wait 125–140 s):

| Subset | n | min | p50 | p95 | max |
| --- | --- | --- | --- | --- | --- |
| all samples | 30 | 296.5 ms | 394.3 ms | 640.2 ms | 652.0 ms |
| desktop reached (≥ 99.9 % non-black pixels) | 23 | 326.5 ms | 423.7 ms | 640.2 ms | 652.0 ms |
| boot not yet at the desktop (≤ 1.8 %) | 7 | 296.5 ms | 311.6 ms | — | 394.3 ms |

"Job empty" matches "QEMU exit" to a tenth of a millisecond in all samples. No errors or timeouts. QEMU exit code = 58853 (0xE5E5), i.e. the one passed to `TerminateJobObject`. Comparison with section 13 (target p95 ≤ 2 s, not modified): p95 640.2 ms, **within the target** on this sample. The number of samples of the desktop series (30) is lower than the 100 required by section 13 for latencies; the boot series below has 100 samples.

The latency grows with the guest memory actually resident: about 50 ms with the VM paused (512 MiB), 90–355 ms during boot (p50 143 ms), 300–650 ms with the desktop (working set about 4 GiB): destroying the WHPX partition and releasing the memory dominate the time.

"Boot" series (`f0-06-scenario-c-boot.log`, 100 samples, random wait 5–60 s from start, guest `running` in all samples, screen from the firmware to the boot logo):

| n | min | p50 | p95 | max | mean | errors/timeouts | cycles with leftovers |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 100 | 89.8 ms | 142.7 ms | 273.3 ms | 354.5 ms | 157.7 ms | 0 | 0 |

It meets the minimum number of samples of section 13 for this load condition (boot) and stays within the target p95 ≤ 2 s.

Note on sample 1 of the desktop series: the count `residui_spike=1` recorded at 11:19:25 coincides with the start of the exclusion test of `F0-05` (QEMU `lightvm-f0-05-excl-t0`, started at 11:19:24), run in parallel and matched by the same filter `lightvm-f0-0[568]`; the sample's QEMU had exited (wait on the handle completed, job empty). It is not a leftover of scenario c, but the measurement of that sample took place with a second QEMU running.

### 2d. Graceful shutdown with escalation

Procedure: `system_powerdown` via QMP, wait for the exit until the timeout; then `quit` with a 5 s wait; then `TerminateJobObject`.

| Series | State before the command | Powerdown timeout | `SHUTDOWN` from the guest | Final phase | `quit` time | Total p50 / max |
| --- | --- | --- | --- | --- | --- | --- |
| `f0-06-scenario-d-boot130.log` (5 cycles) | 130 s, screen still booting (1.5–1.7 %) | 45 s | no | `quit` | 410–523 ms | 45 843 / 45 993 ms |
| `f0-06-scenario-d.log` (5 cycles) | 180 s, desktop (≥ 99.9 %) | 60 s | no | `quit` | 465–788 ms | 60 910 / 61 048 ms |

The `POWERDOWN` event is emitted immediately; the only `SHUTDOWN` observed is the one caused by `quit` (at the timeout). The Ubuntu live session does not power off via ACPI within 60 s, consistent with `F0-02` (9.2). The escalation works and leaves no leftovers; the real graceful shutdown will have to be retested with an installed guest.

### 3. Process identity (`pid-reuse`)

```text
QEMU pid=35072 creation=134360177668174621
dopo la morte, handle ancora aperto: check_pid=Same
fase 1 (handle QEMU aperto): 2000 processi creati, riuso del PID 35072 = false
handle chiuso: check_pid=Absent
fase 2: PID 35072 riusato dopo 1678 processi; check_pid=Reused { creation: 134360178683131401 } → scambio evitato=true
```

Mechanism: the supervisor keeps the handle returned by `CreateProcessW` (primary identity) and the PID + creation time pair (`GetProcessTimes`) as a serializable identity. As long as the handle is open, Windows does not reassign the PID (phase 1); a check based on the PID alone after the handle is closed would find a foreign process (phase 2), recognized as different by the creation time. Caution: with the handle open `OpenProcess(pid)` succeeds even when the process has terminated (`Same`); the "alive" state must therefore be read from the wait on the handle, not from the existence of the PID.

## Limits

- Host shared with other agents (QEMU of the F0-04 console, WSL): latencies and boot times are affected by the load; in particular the desktop was not reached in all samples within the planned waits.
- Handle inheritance is guaranteed by the explicit list passed to `CreateProcessW`, but it was not verified by enumerating the handles of the QEMU process (that would require Sysinternals `handle.exe` or code based on undocumented APIs).
- Supervisor death is tested with `abort()` and `TerminateProcess`; not with a crash from an unhandled exception nor with a stack overflow (same handle-closing mechanism, not tested).
- The detection latency of scenario b was not isolated.
- The UI and its crash, authenticated IPC, the negative test on the QMP socket and host suspend/resume are not covered.
- Emergency Stop measured without a concurrent QMP input flood (negative test "Revocation" of section 7.4, planned in F4).

## Required actions

- No administrator action needed for these tests.
- Optional: a second local non-administrator account for the negative test on the QMP socket (ADR-0012) and on the ACLs of the private directory.
- F1 backlog: bring into `crates/qemu-backend` the spawn with `HANDLE_LIST` + `JOB_LIST`, the classification of exit codes, the shutdown escalation and the handle + creation time identity; real start/stop cycles (F1 criterion: at least 20) with this code.

## Proposed decisions

For **ADR-0002**:

1. QEMU created by the supervisor with `CreateProcessW` + `PROC_THREAD_ATTRIBUTE_JOB_LIST` (no window outside the job) and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` (only stdin `NUL`, stdout/stderr on files); anonymous job with `KILL_ON_JOB_CLOSE`, without `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK`, with a completion port for notifications.
2. `DETACHED_PROCESS` instead of `CREATE_NO_WINDOW`: no `conhost.exe`, tree of a single process (both variants verified in the job).
3. QEMU environment = `SystemRoot` only (plus any variables added for an explicit need, documented); no inheritance from the supervisor's environment.
4. Identity: process handle kept for the whole lifetime of the run + PID + creation time + `run_id`; never decisions based on the PID alone.

For **ADR-0009**:

5. Emergency Stop = `TerminateJobObject(job, codice_dedicato)` followed by a wait on QEMU's handle and on `ActiveProcesses = 0`; completion declared only after both. Measured p95 640 ms with the live desktop (30 samples) and p95 273 ms during boot (100 samples); proposed wait timeout 10 s with a visible error.
6. Ordinary shutdown: `system_powerdown` with a timeout, then `quit`, then `TerminateJobObject`; with the Ubuntu live session ACPI never completed the power-off, so the powerdown timeout must be made configurable and the escalation must always be present.

For **ADR-0015** (part B, manual VMs after a UI crash): on Windows the death of the supervisor always terminates QEMU within about 150 ms (10 cycles), so "reattaching" is possible only if the supervisor survives the UI; if the supervisor dies, the manual VM is lost (overlay state not clean). The choice remains to be made with the UI crash test.

For **ADR-0012**: the QMP channel over Unix socket in a private directory worked in all runs (about 200 starts); the socket file stays orphaned after the Emergency Stop and must be removed by the supervisor (the spike does so). The negative test from a foreign process remains to be run.

For **ADR-0017**: the job's CPU/memory limits are compatible with this process model (see `F0-08-resource-limits-measurements.md`).
