# F0-09 — Review of the F0 evidence: go/no-go decision and F1 backlog

- Execution date (UTC): 2026-10-09
- Executed by: architect responsible for the review (Claude Code agent)
- Roadmap step: section 15, step 6 ("Review of the evidence: go/no-go decision and F1 backlog"); section 10, F0 (exit criterion, decision to stop or change target, no Linux lab); sections 5, 13, 17
- Related ADRs: all; closed or updated in this step: ADR-0010, 0011, 0012, 0013, 0014, 0016, 0018, 0019, 0024, 0025
- Overall outcome: **partial**. F0 exit criterion met with published deviations for boot, console/input, channels, restore, supervision, resource limits and fixture; F0 **is not yet closed** because the pending items listed in section 3 remain, including the user's confirmation of the outcome "verified on Windows only".

## Purpose

Review the verification files F0-01…F0-08, assess each element of the F0 exit criterion (section 10), publish deviations and pending items, compare the measurements with the section 13 targets and propose the go/no-go decision with the F1 backlog.

This file does **not** run new tests: every outcome reported refers to a verification file and its section. It does not declare anything "supported" (section 5) and does not modify the section 13 targets.

## Sources reviewed

| File | Step | Overall outcome declared in the file | File status at review |
| --- | --- | --- | --- |
| `F0-01-toolchain-eframe.md` | 1 | (per item) Windows build verified with explicit MSVC environment; glow without acceleration failed; RDP and Linux not executed | complete |
| `F0-02-qemu-whpx-qmp.md` | 2 (+ part of 4) | (per item) WHPX, QMP over Unix socket, no NIC, forced stop verified; image locking and `-cpu host` failed | complete |
| `F0-03-guest-media.md` | 1–2 | verified (integrity and signature of the media); boot not executed in that file | complete |
| `F0-04-console-rfb-input.md` | 3 | partial | complete |
| `F0-05-baseline-overlay-firmware.md` | 4 | partial | complete |
| `F0-06-process-supervisor.md` | 4 | partial | complete (the series of 100 Emergency Stop samples during boot was completed during the review and integrated here) |
| `F0-07-linux-kvm.md` | 5 | partial; Linux/KVM smoke on a physical host not executed | complete |
| `F0-08-resource-limits-measurements.md` | 5 | partial | complete |

Host of all Windows tests: HP ZBook Fury 16 G9, Intel Core i7-12800HX (16 cores / 24 logical processors), 31.6 GiB RAM, Windows 11 Pro 10.0.26200, NTFS volume E: on NVMe SSD (ADR-0018). No result extends to other hosts (section 5).

## 1. F0 exit criterion: outcome per element

Criterion (section 10, F0): accelerated boot and console/input demonstrated on the first host; protected control channels; restore path and supervision feasible; resource limits identified and fixture defined; Windows and Linux tests, or an explicit block of the failed target, or "verified on Windows only".

| # | Element | Outcome | Evidence (file, section) | Limits |
| --- | --- | --- | --- | --- |
| E1 | Accelerated boot on the first host | **verified with deviation** | `F0-02` §4.1 (`query-accelerators` → `enabled: whpx`, no TCG), §9.2 (Ubuntu 26.04.1 live desktop between 105 and 120 s); `F0-04` Environment; `F0-05` §6; `F0-06` §2c | Only with `-cpu qemu64`: `-cpu host` and `-cpu max` stall OVMF before the display (`F0-02` §9.1, cause not determined). Only live session from ISO: installation to disk not executed. A single host |
| E2 | Console on the first host | **verified** | `F0-04` Results §1–3: internal RFB 3.8 client in the UI process, Unix socket in a private directory, `DIFF … pixel_diversi=0` between the app framebuffer and `screendump` with wgpu DX12, glow and TLS transport | Hextile, CopyRect, Raw and DesktopSize only with tests on constructed data (QEMU always used ZRLE); no real resolution change; tests with the Windows session mostly locked |
| E3 | Input on the first host | **verified with deviation** | `F0-04` §4: keyboard via QMP (GRUB, Ctrl+Alt+T, typing with US and IT layouts), absolute mouse (`virtio-tablet`, language change to "Deutsch"), UI → key table → QMP → guest path from `egui::Event` (test `console_reale`) | Physical keyboard → winit → egui → QMP **not executed** (session locked, no synthetic `SendInput`); AltGr not verified; egui-winit reads the host clipboard on Ctrl+V (text discarded by the UI) |
| E4 | Screenshots | **verified** | `F0-04` §7: end-to-end `screendump` p95 19.2 ms (120 samples) and 19.3 ms (100 samples with animation), 0 errors | Measured at 1280×800 (guest resolution), not 1280×720 |
| E5 | Protected QMP channel | **verified with limits** | `F0-02` §4.3 (AF_UNIX with user-only ACL in `priv/`), §4.2 (named pipe discarded); `F0-04` §6 (low-integrity process of the same user rejected with `os error 10013`); `F0-06` §1 (about 200 starts with QMP over Unix socket) | Test with a second local user **not executed**; negative test from a foreign process of `F0-06` not executed; QEMU serves only one QMP client at a time (`F0-04` §6) |
| E6 | Protected display channel | **verified with limits** | `F0-04` §1 and §6 (VNC over private Unix socket, low-integrity rejection); §5 (VeNCrypt X.509 with rustls: rejection of a foreign CA, wrong name, non-loopback address) | Per-run display credential issued by the supervisor not implemented (the product supervisor does not exist): the protection tested is the ACL of the private directory; over TLS the session stop fails (`F0-04` §5); second user not tested |
| E7 | Feasible restore path | **verified** (feasibility) | `F0-05` §6: 5 cycles baseline → overlay + per-run vars → boot → stop → restore; hashes of base and baseline vars unchanged; run vars modified by the firmware and recreated; §8 crashes during writes and during `qemu-img convert` with the base unchanged | 5 cycles, not the 30 of F3; live guest; overlay in use writable by third parties of the same user (`F0-05` §7.3); an interrupted `.partial` file passes `qemu-img check` (`F0-05` §8.2) |
| E8 | Feasible supervision | **verified with a pending item** | `F0-06` §1 (QEMU in the Job Object from creation, `KILL_ON_JOB_CLOSE`, no breakaway, environment with `SystemRoot` only), §2a (supervisor death → end of QEMU, p50 118.9 / 110.4 ms, max 143.9 ms, 10 cycles, 0 leftovers), §2b, §2c (Emergency Stop: p95 640.2 ms over 30 samples at the desktop; p95 273.3 ms over 100 samples during boot), §3 (handle + creation time identity) | UI crash on Windows **not executed** (the UI does not yet start the supervisor); series with desktop reached of 30 samples, not 100; ACPI graceful shutdown never completed by the live session (`F0-06` §2d, `F0-02` §9.2) |
| E9 | Resource limits identified | **verified** (identification) | `F0-08` Results: CPU hard cap and Job Object memory limits without elevation with real effect on QEMU/WHPX; working set and priority not applicable without elevation; no hard storage quota without administrator rights; `F0-07` §4 (cgroup v2 delegated in WSL2, **[processes]** only) | Thresholds and frequencies not set; tests under storage pressure and artifact floods not executed; cgroup limits on QEMU with KVM not tested (no physical host) |
| E10 | Fixture defined | **verified** (definition) | ADR-0019 (Accepted in this step), based on `F0-04` §4 and `F0-05` §8.1 | Implementation within F3 (section 9); calibration of the control pixels of the mouse step to be set at implementation |
| E11 | Windows and Linux tests, explicit block or "verified on Windows only" | **"verified on Windows only" proposed**, awaiting the user's confirmation | `F0-07` (no physical Linux host; WSL2 used only for builds and process mechanisms, as allowed by section 10); ADR-0016 | Without the user's confirmation the outcome is not recorded; the Ubuntu cell remains "not verified" and the Linux tests are deferred to R2a |

### Blocking F0 activities (section 10) and coverage

| Activity | Outcome | Evidence | Note |
| --- | --- | --- | --- |
| Rust toolchain, minimal eframe build on Windows and Linux | verified with limits | `F0-01` §4; `F0-07` §2 | Windows requires the Build Tools 2022 environment loaded explicitly (`LNK1104` otherwise); Linux in WSL2 with the zig C driver, not with `build-essential` |
| Rendering backend (wgpu or glow) | verified | `F0-01` §5; `F0-04` §3, §7 | decision in ADR-0014 |
| Behaviour without graphics acceleration, for example RDP | partly verified | `F0-01` §6 (WARP verified with CPU about 7.9 logical processors; glow without acceleration fails); `F0-07` §3 (llvmpipe in WSLg) | real RDP session **not executed** |
| QEMU with verified provenance; binary, version, firmware, guest with checksums | verified with a limit | `F0-02` §2; `F0-03` | Authenticode signature of the QEMU binaries not valid (expired certificate) |
| Probe without user credentials in the guest | verified | `F0-02` §4.1; `F0-06` §1 (QEMU environment reduced to `SystemRoot`) | |
| Guest boot on Windows/WHPX with documented configuration | verified with deviation | `F0-02` §9 | `-cpu qemu64` |
| Boot on Linux/KVM if the lab is available | not executed (no lab) | `F0-07` | |
| Private QMP, native console, input, screenshots, forced stop | verified with limits | `F0-02` §4, §7; `F0-04` | E3, E5, E6 |
| Immutable base / overlay / restore cycle with persistent firmware | verified | `F0-05` §6 | |
| Process model: QEMU child of the supervisor, OS termination, UI crash, supervisor crash | verified with a pending item | `F0-06` | UI crash not executed on Windows |
| OS limits for CPU, memory, storage; first consumption measurement | verified with limits | `F0-08` | |
| Threat model, MVP restrictions, initial matrix, fixture | verified (documents) | `docs/SECURITY.md`, `docs/COMPATIBILITY.md`, ADR-0019 | |
| First version of `ARCHITECTURE.md`, `SECURITY.md` and critical ADRs | verified (documents) | `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, `docs/adr/` | updated in this step |

## 2. Published deviations

Items verified with an outcome different from the one required or expected. No deviation modifies a roadmap requirement; each has a cause (known or declared undetermined) and a planned intervention.

| ID | Deviation | Cause | Planned intervention | Evidence |
| --- | --- | --- | --- | --- |
| D-1 | WHPX boot succeeded with `-cpu qemu64`, not with `-cpu host` (nor `-cpu max`) | Not determined; hypothesis: CPUID features exposed by `host`/`max` not handled by WHPX with VBS/HVCI active | Reference profile with `qemu64` recorded in ADR-0011 and in `COMPATIBILITY.md`; in F1 full boot with `Skylake-Client-v4` or `Haswell-v4` (they reach GRUB) and repetition of `-cpu host` after checking the `HypervisorPlatform` feature (user action U-1) | `F0-02` §9.1–9.2 |
| D-2 | QEMU image locking ineffective on Windows | QEMU 11.1.0 opens base and overlay with `FILE_SHARE_WRITE` | Exclusion guaranteed by the supervisor (ADR-0025): baseline opened with `FILE_SHARE_READ`, exclusive open as a detector, no reliance on `qemu-img` refusing | `F0-02` §8.3; `F0-05` §7 |
| D-3 | Overlay in use cannot be protected from writes by another process of the same user | The share mode does not distinguish processes; QEMU itself grants `FILE_SHARE_WRITE` | Limit declared in `SECURITY.md` (consistent with section 7.1, out of scope); in F1 test of a per-run directory with an explicit ACL and of a distinct process identity for QEMU | `F0-05` §7.3, §7.5 |
| D-4 | Authenticode signature of the QEMU binaries not valid (certificate expired on 12/10/2023) | Certificate of the signer of the Windows build expired | Provenance based on HTTPS, published SHA-512 and SHA-256 matching the independent winget manifest; risk declared in ADR-0010; additional verification (reproducible rebuild or alternative source) before F6 | `F0-02` §2.2 |
| D-5 | RFB session stop over VeNCrypt/TLS failed; connections in `CloseWait` on the QEMU side | Non-interruptible blocking read over TLS; QEMU does not immediately close terminated TLS connections | VeNCrypt only as a documented alternative (ADR-0013), not the default; in F2 test with `connections=` and supervisor timeouts | `F0-04` §5 |
| D-6 | `HypervisorPlatform` feature reported *Disabled* via CIM, but WHPX working | Not determined; DISM not queryable without elevation | User action U-1; no inference for other hosts; F1 diagnostics must not rely on the CIM state of the feature alone | `F0-02` §1.1, §1.3 |
| D-7 | ACPI graceful shutdown (`system_powerdown`) never completed by the Ubuntu live session | Behaviour of the live session (`POWERDOWN` event emitted, no `SHUTDOWN`) | Escalation `system_powerdown` → `quit` → `TerminateJobObject` always present, configurable timeout; repetition with an installed guest | `F0-02` §9.2; `F0-06` §2d |
| D-8 | QMP over a named pipe created by QEMU unsuitable (blocking at startup, single client without reconnection, stall in `prelaunch`, taken by the first process) | Behaviour of the `pipe` chardev of QEMU 11.1.0 on Windows | Discarded in ADR-0012 in favour of the Unix socket | `F0-02` §4.2 |
| D-9 | QEMU serves only one QMP client at a time | Behaviour of the QMP monitor | A single owner of the QMP connection in the supervisor, multiplexing input, screenshots and events (F1 backlog) | `F0-04` §6 |
| D-10 | MSVC build fails without an explicit environment (`LNK1104 msvcrt.lib`) | rustc picks a VS installation lacking the CRT libraries | Explicit "Developer" environment in development and in CI; user decision on fixing the local installation (U-9) | `F0-01` §4.1 |
| D-11 | glow has no software fallback on Windows; WARP (via wgpu) consumes about 7.9 logical processors and sustains about 5.5 updates/s at 10 Hz | OpenGL without a driver offers only GDI 1.1; software rasterization | glow by default with wgpu/WARP fallback as a declared degraded mode (ADR-0014); reduced console frequency in that mode | `F0-01` §5–6 |
| D-12 | UI memory with wgpu DX12: private bytes 369–372 MiB, above the 250 MiB target already with the UI alone | Allocations of the wgpu backend/DX12 driver (internal cause not isolated) | glow by default (88–98 MiB); wgpu selectable with a declared deviation; analysis of wgpu allocations in F2 | `F0-01` §5; `F0-04` §7 |
| D-13 | egui-winit reads the host clipboard when Ctrl+V is pressed | Behaviour of egui-winit 0.36.2 (`on_keyboard_input`) | The text is already discarded by the UI; blocking the host clipboard read in the F1 backlog, before F2 | `F0-04` §4 |
| D-14 | Job memory limit exceeded at steady state: QEMU terminates with an access violation instead of a handled error | Unhandled allocation failure (internal cause not studied) | Limit sized on guest RAM + measured margin; classification `resource_limit` from the `JOB_MEMORY_LIMIT` notification (ADR-0017, Proposed) | `F0-08` Memory limits |
| D-15 | Job CPU hard cap respected on average, not in every second | Regulation over scheduling intervals | Declare it as an average limit; expression in equivalent logical processors | `F0-08` CPU hard cap |
| D-16 | No hard storage quota without administrator rights; NTFS quotas per user and volume | Privileges; nature of NTFS quotas | Application budget with monitoring and stop, declared risk of overshoot (ADR-0017, Proposed) | `F0-08` Results |
| D-17 | On Linux (WSLg) the X11 path panics because `libxkbcommon-x11.so` is missing | Library loaded with `dlopen` absent | Dependency in the Linux packaging; panic turned into a usable error (F1 backlog) | `F0-07` §3 |
| D-18 | Emergency Stop with desktop reached measured on 30 samples, not the 100 of section 13 (the series during boot has 100 samples) | Each sample requires the full boot to the desktop on a shared host | 100-sample series with desktop or installed guest in the formal measurements (F1/F4) | `F0-06` §2c |

## 3. Pending items

### 3.1 Pending items for the formal closure of F0

| ID | Pending item | Element | Who | Expected evidence |
| --- | --- | --- | --- | --- |
| P-1 | Confirmation that no physical Linux host with KVM is available in F0 (outcome "verified on Windows only") | E11 | user (U-5) | record in `docs/STATUS.md`; ADR-0016 becomes effective |
| P-2 | UI crash on Windows with a UI that starts the supervisor as a separate process; decision on ADR-0015 part B | E8 | development | section added to `F0-06` or a new F0 file |
| P-3 | Series of 100 Emergency Stop samples during boot | E8 | development | **closed**: `F0-06` §2c, p50 142.7 ms, p95 273.3 ms, max 354.5 ms, 0 errors, 0 leftovers |
| P-4 | Physical keyboard → winit → egui → QMP with the session unlocked, including AltGr (IT layout), Ctrl+C/V, keypad | E3 | user (U-4) with development | section added to `F0-04` or a new F0 file |
| P-5 | Negative test with a second local user towards `qmp.sock` and `vnc.sock`, or the user's decision to record it as a limit | E5, E6 | user (U-3) | section added to `F0-04`/`F0-06` or limit recorded in `SECURITY.md` |
| P-6 | Recording the availability of code signing and independent review (decision to close in F0, section 17) | — | user (U-7) | `docs/STATUS.md`; ADR-0020 |
| P-7 | Details of the approved data medium (tool, format, limits, device, read-only verification) — decision to close in F0 (section 17) | — | development | additional section of ADR-0008 |

### 3.2 Pending items declared as limits, with references

| ID | Pending item | Reference |
| --- | --- | --- |
| L-1 | Real RDP session and host without a GPU (eframe behaviour) | Limit declared in `COMPATIBILITY.md`; optional user action U-8; test within F2 |
| L-2 | Installation of Ubuntu to disk and boot from the disk under WHPX | F1/F2 backlog; ADR-0011 criterion 2 |
| L-3 | Cold UI starts (20 samples), idle manager CPU over 5 minutes, UI + supervisor memory | F1/F2 backlog (measurements with product processes) |
| L-4 | Storage thresholds, pressure tests and screenshot/log floods | ADR-0017 (Proposed); F1/F3 backlog |
| L-5 | All Linux tests on a physical host (KVM, real QEMU, cgroup on QEMU, eframe with GPU, packaging) | R2a (ADR-0016) |
| L-6 | Hextile, CopyRect, Raw and DesktopSize sent by a real QEMU | F2 backlog |

## 4. Comparison with the section 13 targets

The targets are not modified. The F0 measurements are first measurements on spikes, not the formal release measurements.

| Measurement | Target (sec. 13) | F0 measurement | Samples | Outcome against the target | Evidence |
| --- | --- | --- | --- | --- | --- |
| Guest screenshot | p95 ≤ 500 ms at 1280×720 | p50 17.6 ms, p95 19.2 ms, max 20.5 ms (idle guest); p50 16.4, p95 19.3, max 24.3 ms (animation); 0 errors, 0 timeouts | 120 + 100 | within the target, at 1280×800 | `F0-04` §7 |
| UI memory — glow | ≤ 250 MiB (UI + supervisor) | working set 91.8–94.5 MiB; private bytes 88.4–97.6 MiB (console active) | 1 series of 60 s per condition | UI only: within the target with both metrics; supervisor not measured | `F0-04` §7; `F0-01` §5 |
| UI memory — wgpu DX12 | ≤ 250 MiB (UI + supervisor) | working set 190.6–198.3 MiB; private bytes 369.2–372.0 MiB | 1 series of 60 s per condition | **deviation D-12** with the proposed metric (private bytes); working set within the target | `F0-04` §7 |
| UI memory — wgpu WARP | ≤ 250 MiB | working set 124.8 MiB; private bytes 376.3 MiB (spike without console) | 1 | **deviation** with the proposed metric; degraded mode | `F0-01` §5 |
| UI start (warm) | p95 ≤ 3 s | first frame p50/p95: glow 294/326 ms, wgpu Vulkan 705/919 ms (20 starts), wgpu DX12 501/538 ms (10 starts); full process with `--smoke 0.5` p95 glow 653 ms, wgpu 1333 ms | 20 (10 for DX12) | within the target on the warm sample | `F0-01` §5 |
| UI start (cold) | p95 ≤ 3 s | not measured | — | **no measurement** (pending L-3) | `F0-01` §5 |
| Orphan processes | zero | 0 leftovers: 5 kill cycles (`F0-02` §7), 5 baseline cycles and storage crash tests (`F0-05`), 10 supervisor deaths, 6 external kills of QEMU, 130 Emergency Stops, 10 stops with escalation (`F0-06`) | see previous column | zero on the cycles run; "app crash" scenario not executed (P-2) | `F0-02`, `F0-05`, `F0-06` |
| Emergency Stop | p95 ≤ 2 s for the end of the QEMU process | desktop: p50 394.3 ms, p95 640.2 ms, max 652.0 ms; boot: p50 142.7 ms, p95 273.3 ms, max 354.5 ms; 0 errors, 0 timeouts, 0 leftovers | 30 (desktop) + 100 (boot) | within the target; desktop series with fewer than 100 samples (D-18) | `F0-06` §2c |
| Idle manager CPU | average < 1% of one core | indications over 10–60 s: wgpu `--static` 0.16%, glow `--static` 0.47% (`F0-01`); console with idle guest: wgpu 0.05%, glow 0.18% (`F0-04`) | windows of 10 and 60 s | **formal measurement missing** (5 minutes, UI + supervisor) | `F0-01` §5; `F0-04` §7 |
| Revocation | p95 ≤ 250 ms | not measurable (sessions from F4) | — | not executed | — |
| Temporary space | no unaccounted growth | per-session accounting recorded (overlay, vars 540 672 bytes, logs, screendump 3 072 016 bytes per PPM); QMP socket orphaned after a forced stop, removed by the spike | — | first accounting; series of sessions and cleanup after restart not executed | `F0-08` First measurement; `F0-05` §9 |

Console latency of the screendump fallback (section 6.4, no numerical target): the fallback is not needed on the first host; its measured latency (p95 19.2 ms for the capture, plus delivery via IPC not yet existing) is reported in `COMPATIBILITY.md`.

## 5. Proposed memory metric

Proposal: for the target "Manager memory ≤ 250 MiB" use **private bytes** on Windows (`PrivateMemorySize64`, i.e. `PrivateUsage`), summed over UI and supervisor; on Linux the proposed equivalent metric is private anonymous memory `RssAnon + VmSwap` from `/proc/<pid>/status`, reported under its own label. The working set (Windows) and `VmRSS` (Linux) are published alongside, without merging them under the same label (section 13).

Rationale:

1. **Private bytes are the memory the process commits in the system** (commit), which the operating system cannot reduce by trimming the working set; the working set depends on the state of the host (memory pressure, minimized window) and is therefore not reproducible across runs.
2. **The working set hides real allocations of the backend**: with wgpu DX12 the working set (about 191–198 MiB) would fall within the target while the actual commitment is about 370 MiB (`F0-04` §7). Using the working set would make the target appear met on a favourable metric, against the rule of section 13.
3. **Additivity**: the sum of the private bytes of UI and supervisor does not double-count shared pages (DLLs, mappings), which the working set includes in each process.
4. With glow the two metrics almost coincide (88–98 MiB), so the choice does not penalize the proposed backend; it correctly penalizes configurations that commit more memory.

Consequence: ADR-0014 chooses glow as the default backend on Windows; wgpu DX12 and WARP remain available with the declared deviation D-12.

## 6. Proposed go/no-go decision

**Proposal: Windows 11 x86_64 remains the first target (and the only MVP target with the outcome "verified on Windows only").** No check failed without an allowed alternative: the observed failures (`-cpu host`, image locking, TLS stop, glow without acceleration) have a verified alternative within the roadmap options (named CPU model, exclusion in the supervisor, Unix socket, wgpu/WARP fallback). Neither the target change to Linux (section 10: Linux has no lab) nor the block applies. No result uses TCG.

Verified with deviation on the first host (section 2): D-1 `-cpu qemu64` instead of `host`; D-2 QEMU image locking ineffective, replaced by the supervisor's exclusion; D-4 expired QEMU Authenticode signature; D-5 TLS stop failed; D-6 `HypervisorPlatform` "Disabled" via CIM with WHPX working; D-7 ACPI shutdown not completed by the live session; D-8/D-9 QMP limits (named pipe discarded, single client); D-10 explicit MSVC environment; D-11/D-12 rendering (glow without software fallback, wgpu memory); D-13 host clipboard read by egui-winit; D-14–D-16 resource limits; D-18 Emergency Stop samples.

Pending: P-1, P-2, P-4…P-7 for the formal closure (P-3 closed during the review); L-1…L-6 declared as limits with references.

**F0 closure condition:** F0 closes when P-1, P-2 and P-4…P-7 have a recorded outcome (verified, failed with the consequence applied, or a limit decided by the user where table 3.1 allows it). Until then `docs/STATUS.md` reports F0 as "under review". Section 10 states that each phase starts after the exit criterion of the previous one: F1 work started in parallel cannot be used to declare an F1 exit criterion before F0 is closed, and its status must be reconciled with this review.

If a pending item fails:

| Pending item | Consequence in case of failure |
| --- | --- |
| P-1 (Linux host available) | The Linux/KVM smoke test must be run in F0; the outcome becomes "Windows and Linux verified" or an explicit block for Linux |
| P-2 (UI crash) | If the supervisor does not survive or does not stop the VMs in a controlled way, ADR-0015 part B remains open and F0 does not close on element E8 |
| P-4 (physical keyboard) | The winit → qcode mapping must be fixed before stabilizing the UI (section 6.4) |
| P-5 (second user) | If another user connects to the sockets, ADR-0012/0013 go back to Proposed and the socket created by the supervisor and passed by descriptor, or VeNCrypt with a per-run certificate, is tested |

## 7. ADRs: outcome of the review

| ADR | Status after the review | Decision or reason |
| --- | --- | --- |
| 0010 | Accepted | QEMU 11.1.0 from qemu.weilnetz.de, portable extraction, checksums; expired signature as a risk; winget as an alternative with elevation |
| 0011 | Accepted | Ubuntu 26.04.1 LTS desktop amd64; OVMF edk2 in pflash, vars copied per run; `pc-q35-11.1`; `-cpu qemu64`; Secure Boot off |
| 0012 | Accepted | QMP over Unix socket in a private directory; named pipe discarded; a single QMP client owned by the supervisor |
| 0013 | Accepted | VNC over Unix socket primary; VeNCrypt documented alternative with the stop problem; screendump fallback feasible |
| 0014 | Accepted | egui/eframe 0.36.2; glow default on Windows, wgpu DX12 selectable, WARP degraded mode |
| 0016 | Accepted (conditional on the user's confirmation) | "Verified on Windows only"; Ubuntu cell "not verified"; Linux tests in R2a |
| 0018 | Accepted | Development host as the first Windows measurement host |
| 0019 | Accepted | Fixture defined (section 9: implementation within F3) |
| 0024 (new) | Accepted | Separate `rfb-client` crate used by the UI; `qemu-backend` remains in the supervisor |
| 0025 (new) | Accepted | Disk exclusion guaranteed by the supervisor, not by QEMU locking |
| 0015 | Proposed | Part A Linux with **[processes]** tests in WSL2; part B awaiting the UI crash on Windows (P-2) |
| 0017 | Proposed | Windows mechanisms identified; thresholds, pressure tests and Linux part on a physical host missing |
| 0020 | Proposed | Requires the user's declaration (P-6) |
| 0021, 0022, 0023 | Proposed | Closable within F1 (section 17); 0022 with evidence on egui_kittest |
| 0001, 0002, 0003, 0004, 0005, 0007, 0008, 0009 | Accepted (unchanged) | "Evidence" sections updated with the F0 references; 0002: UI crash pending |

## 8. Actions requiring the user

Collected from all F0 files. None was performed by this review.

| ID | Action | Origin | Effect |
| --- | --- | --- | --- |
| U-1 | In PowerShell **as administrator**: `Get-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform`; if disabled, consider `Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform` (reboot), then repeat `F0-02` §4.1 and §9.1 | `F0-02` Required actions | Clarifies D-6; may change the outcome of `-cpu host` (D-1). Not blocking |
| U-2 | Provide a physical Ubuntu LTS x86_64 Linux host with KVM (unprivileged user with access to `/dev/kvm`, local graphics session with a GPU) | `F0-07` Actions 1 | If available in F0, the Linux smoke becomes mandatory in F0; otherwise R2a |
| U-3 | Create a second local non-administrator account and run with its credentials (`runas /user:…`) `f0-console probe-unix` towards `priv\vnc.sock` and `priv\qmp.sock`; alternatively decide to record the test as a limit | `F0-02` Actions 3; `F0-04` Actions 1; `F0-05` Limits; `F0-06` Actions | Closes P-5 |
| U-4 | With the Windows session unlocked, use the physical keyboard in `lightvm-desktop --vnc-socket … --qmp-socket …` typing in the guest terminal, including AltGr with the IT layout, Ctrl+C/V and the keypad | `F0-04` Actions 2 | Closes P-4 |
| U-5 | Confirm that no physical Linux host with KVM is available in F0 and accept the outcome "verified on Windows only" | `F0-07` Proposed decisions, ADR-0016 | Closes P-1 |
| U-6 | In the WSL distro: `sudo apt install build-essential pkg-config` (possibly `libxkbcommon-x11-0`), then repeat `lab/wsl/build-all.sh` without the zig driver | `F0-07` Actions 2 | Linux build with the standard toolchain; not blocking for F0 |
| U-7 | Declare who holds the Windows signing credentials and who performs the independent review, or record their unavailability | ADR-0020; section 14 | Closes P-6 |
| U-8 | Optional: open an RDP session to the host and run the `lightvm-desktop` smoke (automatic adapter and `--wgpu-adapter cpu`) | `F0-01` §6 | Reduces limit L-1 |
| U-9 | Decide how to fix the local MSVC environment (component with libraries in VS 2026, or systematic use of the Build Tools 2022 Developer Prompt) | `F0-01` §4.1 | Removes D-10 in development |
| U-10 | Optional, as administrator: `fsutil quota query E:` and a possible test of NTFS quotas on a test volume | `F0-08` Actions 1 | Information for ADR-0017; NTFS quotas do not isolate LightVM |
| U-11 | Optional and **not valid for the matrix**: `sudo usermod -aG kvm mortali` and QEMU in the WSL distro for a nested KVM probe | `F0-07` Actions 3 | Integration only; no effect on the Ubuntu cell |

## 9. F1 backlog

The F1 backlog derived from section 10 F1 and from the actions of the F0 files is in `docs/RELEASE_PLAN.md`, section "F1 backlog".

## Limits of the review

- The review is based on the verification files present at the time of reading; `F0-06` was completed during the review (series of 100 samples) and its final values are integrated.
- The tests were run on a host shared with other agents (parallel QEMU instances, WSL): boot times and latencies are affected (`F0-06`, `F0-08`).
- No measurement in this file is new: every value is copied from the file indicated.

## Proposed decisions

Recorded in the ADRs listed in section 7 and in `docs/STATUS.md`. The go/no-go decision of section 6 becomes effective with the user's confirmation (U-5) and the closure of pending items P-2 and P-4…P-7.
