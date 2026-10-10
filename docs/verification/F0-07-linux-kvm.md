# F0-07 — Linux/KVM: inventory, build portability and process mechanisms

- Execution date (UTC): 2026-10-09
- Executed by: development agent (role: Linux/Rust systems engineer), session on a Windows host
- Roadmap step: section 15, step 5 ("Linux/KVM smoke test and resource controls"); section 10, F0 (paragraph "No Linux lab")
- Related ADRs: ADR-0015, ADR-0016, ADR-0014, ADR-0017, ADR-0009
- Overall outcome: **partial**. Linux/KVM smoke test on a physical host: **not executed** (no physical Linux host available). Build portability and process mechanisms in WSL2: executed, with the limited validity declared below.

## Purpose

Establish which Linux environment is available on this machine and use it **only** within the limit set by the roadmap (section 10, F0): "WSL2, nested virtual machines or runners without verifiable acceleration do not replace the physical host for this test; they serve only for builds and accelerator-independent tests".

Each test in this file has one of the following three validities, indicated on every result:

| Label | Meaning |
| --- | --- |
| **[build]** | Portability of the build and of the headless tests on Linux. It does not certify the Linux host. |
| **[processes]** | Linux kernel mechanisms for the process lifecycle (ADR-0015, allowed by ADR-0015 "in a non-accelerated Linux environment, provided the file declares that the test concerns only the process lifecycle"). The test child is `sleep`, not QEMU. |
| **[nested]** | Anything concerning KVM inside WSL2: nested virtualization, **NOT valid for the matrix** (roadmap, "No Linux lab"). |

What this file does **not** demonstrate: KVM-accelerated boot, QEMU working on Linux, behaviour of cgroup limits on a QEMU process with KVM, eframe rendering with a GPU on Linux, support for Ubuntu LTS as a host. The Ubuntu cell of the matrix (section 5) remains **not verified**.

## Environment

| Item | Value |
| --- | --- |
| Physical host (model, CPU, RAM, storage) | HP ZBook Fury 16 G9, Intel Core i7-12800HX (16 cores, 24 logical processors), 31.6 GiB RAM (data from F0-01); local NTFS volume `E:` |
| Host OS and build | Windows 11 Pro 10.0.26200 (build 26200.9457 according to `wsl --version`) |
| Host hypervisor | `HypervisorPresent=True` (`Win32_ComputerSystem`); Docker Desktop 4.92.0 installed (distribution `docker-desktop`, stopped, not used) |
| WSL | WSL 2.7.8.0, kernel `6.18.33.1-microsoft-standard-WSL2`, WSLg 1.0.73.2, MSRDC 1.2.6676, Direct3D 1.611.1, DXCore 10.0.26100.1 |
| User `.wslconfig` | **absent** (`Test-Path $env:USERPROFILE\.wslconfig` → `False`): default settings, hence no explicit `nestedVirtualization` setting |
| WSL distributions | `Ubuntu` (default, version 2), `docker-desktop` (version 2) |
| Distribution used | Ubuntu 26.04 LTS "Resolute Raccoon", glibc 2.43, systemd 259 active (`/etc/wsl.conf`: `[boot] systemd=true`) |
| Resources visible in the WSL VM | 24 CPUs, 15 791 MiB RAM, 4 GiB swap; `/` 1007 GiB (955 GiB free) |
| User in the distro | `mortali` (uid 1000), groups `adm cdrom sudo dip plugdev users`; **not** in the `kvm` group; `sudo -n true` → "interactive authentication is required" (no passwordless sudo) |
| Accelerator and state | **[nested]** `/dev/kvm` present (`crw-rw---- root kvm 10,232`), modules `kvm_intel` and `kvm` loaded, `kvm_intel.nested=Y`, CPU flags `vmx` and `hypervisor` visible. `/dev/kvm` **not accessible** to the user (not in the `kvm` group). Nested virtualization: not valid for the matrix |
| cgroup | unified cgroup v2 (`nsdelegate`); root controllers `cpuset cpu io memory hugetlb pids rdma`; `user@1000.service` delegated with `cpu memory pids` |
| Rust toolchain | rustup 1.29.1, rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0 (5f94df478 2026-08-27), host `x86_64-unknown-linux-gnu`, selected by `rust-toolchain.toml` |
| C driver / linker | **non-standard**: zig 0.14.1 (`zig cc -target x86_64-linux-gnu.2.39`) via the wrapper `lab/wsl/bin/cc`; see "Installations performed" |
| QEMU binary | **absent** in the distro (`command -v qemu-system-x86_64` empty; `apt-cache policy qemu-system-x86`: Installed `(none)`, Candidate `1:10.2.1+ds-1ubuntu3`). Not installed, per the task rules |
| Firmware / guest | not applicable (no boot performed) |
| Graphics session | WSLg: `WAYLAND_DISPLAY=wayland-0`, `DISPLAY=:0`; `/dev/dxg` present; Mesa 26.0.3, no "dozen" Vulkan ICD installed |
| Other conditions | sources built directly from `/mnt/e/OpenVM` (9p), `CARGO_TARGET_DIR=/mnt/e/OpenVM/lab/wsl/target` separate from the Windows `target/`; other agents were modifying `apps/desktop`, `crates/rfb-client` and `spikes/*` in parallel |

### Installations performed (WSL user profile only, no sudo)

| Component | Where | Provenance and verification | Reason |
| --- | --- | --- | --- |
| rustup 1.29.1 | `~/.rustup` (617 MiB), `~/.cargo` (278 MiB, includes the crate registry) | `https://sh.rustup.rs`, SHA-256 of the downloaded script `7d0ea0f8eba7fa1ebfe998091cd7ec4501e33ec5ca6b884eb4d894d7da5170af`; options `-y --default-toolchain none --profile minimal --no-modify-path` | no Rust in the distro |
| toolchain 1.99.0-x86_64-unknown-linux-gnu | `~/.rustup/toolchains` | installed automatically by rustup on the first invocation in `/mnt/e/OpenVM` (`rust-toolchain.toml`) | version pinned by the workspace |
| zig 0.14.1 | `~/.local/opt/zig-x86_64-linux-0.14.1` (340 MiB) + cache `~/.cache/zig` (50 MiB) | `https://ziglang.org/download/0.14.1/zig-x86_64-linux-0.14.1.tar.xz`, SHA-256 `24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c`, equal to the value published in `https://ziglang.org/download/index.json` | the distro has no `gcc`/`cc`, `libc6-dev` (`crt1.o`, `libc.so`, headers) nor `pkg-config`; without sudo `build-essential` and the like cannot be installed. `zig cc` provides a C driver and glibc stubs without privileges |

No apt package, no WSL distribution, no Windows feature installed. To remove: `rm -rf ~/.rustup ~/.cargo ~/.local/opt/zig-x86_64-linux-0.14.1 ~/.cache/zig` in the distro.

## Procedure

All scripts are in `lab/wsl/` (directory ignored by git); raw logs in `lab/logs/f0-07-*`. Commands launched from Git Bash with `MSYS_NO_PATHCONV=1 wsl -d Ubuntu -- ...`.

1. Windows inventory: `wsl --status`, `wsl -l -v`, `wsl --version`, `Test-Path $env:USERPROFILE\.wslconfig`, `(Get-CimInstance Win32_ComputerSystem).HypervisorPresent`.
2. Distro inventory: `lab/wsl/inventory.sh`, `lab/wsl/inventory2.sh` (kernel, `/dev/kvm`, groups, sudo, cgroup, systemd, toolchain, libraries, WSLg, `systemd-run --user --scope`). Logs: `lab/logs/f0-07-inventory.txt`, `f0-07-inventory2.txt`, `f0-07-kvm-check.txt`.
3. User installations: `lab/wsl/install-user-tools.sh` (log `f0-07-install-user-tools.txt`).
4. Build environment: `lab/wsl/env.sh`:
   ```sh
   export PATH="$HOME/.cargo/bin:/mnt/e/OpenVM/lab/wsl/bin:$PATH"
   export CC=/mnt/e/OpenVM/lab/wsl/bin/cc CXX=/mnt/e/OpenVM/lab/wsl/bin/c++ AR=ar
   export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/mnt/e/OpenVM/lab/wsl/bin/cc
   export CARGO_TARGET_DIR=/mnt/e/OpenVM/lab/wsl/target
   export RUSTUP_AUTO_INSTALL=0
   ```
   The wrapper `lab/wsl/bin/cc` runs `zig cc -target x86_64-linux-gnu.2.39` discarding `--target=*`, `-Wl,--no-undefined-version` and `-Wl,-O1` (the latter produced the linker warning "ignoring deprecated linker optimization setting '1'").
5. Build and test **[build]**: `lab/wsl/build-desktop.sh` (`cargo build -p lightvm-desktop`), `lab/wsl/build-all.sh` (`cargo build --release -p lightvm-desktop`, `cargo build --release -p f0-linux-procs`, `cargo clippy -p f0-linux-procs --all-targets -- -D warnings`, `cargo test --workspace`), then `lab/wsl/run.sh cargo test --workspace` repeated.
6. eframe smoke under WSLg **[build]**: `lab/wsl/smoke.sh <binary> [options]` starts `lightvm-desktop --smoke 16`, reads `VmRSS`/`RssAnon`/`RssFile` from `/proc/<pid>/status` at 3 s and the CPU (`utime+stime` from `/proc/<pid>/stat`) between 3 and 13 s; `lab/wsl/primo-frame.sh` runs 20 warm starts with `--smoke 0.5`.
7. Spike `spikes/f0-linux-procs` **[processes]**:
   ```sh
   f0-linux-procs processi --samples 30
   f0-linux-procs cgroup --samples 30                                   # from the startup cgroup of `wsl --`
   systemd-run --user --scope -p Delegate=yes --quiet -- f0-linux-procs cgroup --samples 30
   ```
8. Check on Windows that the spike does not break the workspace: `cargo check -p f0-linux-procs` and `cargo clippy -p f0-linux-procs -- -D warnings` (temporary target dir in `lab/wsl/`, then removed); `cargo +stable-x86_64-pc-windows-gnu run -p f0-linux-procs` prints `f0-linux-procs: non applicabile su questo sistema operativo (spike solo Linux)` and exits normally.

## Results

### 1. Inventory

| Item | Outcome | Evidence |
| --- | --- | --- |
| WSL distribution present | verified | `Ubuntu` (WSL2, default), `docker-desktop` |
| Physical Linux host with KVM | **not executed** (not available) | no physical Linux host in this session |
| `/dev/kvm` in the distro | verified **[nested]** | present, `root:kvm 0660`, `kvm_intel.nested=Y`; user not in the `kvm` group → cannot be opened |
| QEMU in the distro | not present | `qemu-system-x86_64` absent |
| Probe `-accel kvm -S` + `query-accelerators` | **not executed** | QEMU absent (not installed per the rules) and `/dev/kvm` not accessible to the user. Even if run, it would have been "nested WSL2, NOT valid for the matrix" |
| systemd and user cgroup delegation | verified | systemd 259 PID 1; `systemd-run --user --scope -p MemoryMax=64M -p CPUQuota=20% true` → `SCOPE_OK` |

### 2. Build and tests on Linux **[build]**

| Item | Outcome | Evidence |
| --- | --- | --- |
| `cargo build -p lightvm-desktop` (debug) | verified, with zig linker | `Finished dev profile ... in 2m 50s`, `EXIT=0 durata_s=171` (log `f0-07-build-desktop.txt`) |
| `cargo build --release -p lightvm-desktop` | verified, with zig linker | `EXIT=0 durata_s=231`; binary 134 019 080 bytes, SHA-256 `f0368ddd3b6bfdcd44a9b02590bccb0155a2e3b3ffd71c5ebdd2a92484e90ca2`, 2026-10-09 11:12:40 UTC |
| Libraries statically linked into the binary | observed | `ldd`: only `libc.so.6`, `libm.so.6`, `ld-linux-x86-64.so.2`; X11, Wayland, xkbcommon, EGL and Vulkan are loaded with `dlopen` at runtime, which is why the build does not require `-dev` packages |
| `cargo test --workspace`, first attempt (11:12 UTC) | **failed**, external cause | `apps/desktop/src/console.rs`: `unresolved import rfb_client`, `cannot find module or crate f0_console`: change in progress by another agent on `apps/desktop`, not a portability problem |
| `cargo test --workspace --exclude lightvm-desktop` | verified | all tests passed (f0_console 6, rfb_client 10, `sola_lettura` 3, others 0 tests) |
| `cargo test --workspace`, second attempt (completed 11:21 UTC) | verified | `EXIT=0`, no warnings; including `lightvm_desktop` lib 5 tests and `kittest_ui` 4 tests (headless egui_kittest with AccessKit) |
| `cargo clippy -p f0-linux-procs --all-targets -- -D warnings` (Linux) | verified | `EXIT=0` |
| `cargo check`/`clippy -p f0-linux-procs` (Windows) | verified | no warnings; run on Windows: "not applicable" message |

Validity: the build succeeded with a non-standard C driver (zig). The same build with Ubuntu's `build-essential` **was not run**; see Required actions.

### 3. eframe smoke under WSLg **[build]**

Release binary from point 2. Linux metrics, **not comparable** with Windows working set/private bytes (section 13): `VmRSS` = total resident memory; `RssAnon` = anonymous resident memory (the closest to private bytes); `RssFile` = resident mapped file pages.

| Configuration | Adapter reported | First frame (ms) | VmRSS / RssAnon / RssFile at 3 s | CPU 3–13 s (% of one core) | Frames / fb updates in 16 s |
| --- | --- | --- | --- | --- | --- |
| default wgpu, Wayland | `backend=Vulkan adapter="llvmpipe (LLVM 21.1.8, 256 bits)" tipo=Cpu driver_info="Mesa 26.0.3-1ubuntu1 (LLVM 21.1.8)"` | 366 | 173.8 / 46.2 / 96.2 MiB | 56.8% | 126 / 125 |
| wgpu, `--static` | same | 149 | 150.9 / 37.9 / 93.4 MiB | 0.80% | 6 / 1 |
| wgpu, `WGPU_BACKEND=gl` | `backend=Gl adapter="llvmpipe (LLVM 21.1.8, 256 bits)" tipo=Cpu driver_info="4.5 (Core Profile) Mesa 26.0.3-1ubuntu1"` | 265 | 165.0 / 65.4 / 89.8 MiB | 52.6% | 139 / 136 |
| wgpu, X11 (`env -u WAYLAND_DISPLAY`) | — | — | — | — | **failed**: panic `xkbcommon-dl-0.4.2/src/x11.rs:59:28: Library libxkbcommon-x11.so could not be loaded.`, exit 101 |
| Debug build, default wgpu | llvmpipe Vulkan | 509 | 201.0 / 53.9 / 115.7 MiB | 50.7% | 156 / 155 |

First frame, 20 warm starts (default wgpu, release, `--smoke 0.5`): p50 251 ms, p95 424 ms, max 499 ms (log `f0-07-primo-frame-20.txt`).

Observations:
- Available adapters (`--wgpu-adapter cpu` lists them): only `Vulkan "llvmpipe"` and `Gl "llvmpipe"`, both of type Cpu. WSLg GPU acceleration (Mesa d3d12 on `/dev/dxg`) was not reached: stderr `libEGL warning: failed to get driver name for fd -1`, `MESA: error: ZINK: failed to choose pdev`, `egl: failed to create dri2 screen`. Cause not investigated further. In practice it is a test of **software rendering on Linux**, useful for the F0 item "host without graphics acceleration", not for performance with a GPU.
- With 10 Hz animation llvmpipe does not keep up: 125 updates in 16 s (about 7.8/s) versus 152 on Windows/NVIDIA (F0-01). CPU about 53–57% of one core, versus 7% on Windows with a GPU and 786% with WARP (F0-01). The cost of WSLg/RDP composition on the Windows side is not included.
- On X11 `libxkbcommon-x11.so` is missing (Ubuntu package `libxkbcommon-x11-0`): runtime requirement to declare in the Linux packaging. The app panics instead of giving a usable error: to be fixed within F1 ("errors usable by the user").

### 4. Spike `spikes/f0-linux-procs` **[processes]**

Kernel `6.18.33.1-microsoft-standard-WSL2`. Workspace release binary, SHA-256 `6e19036c4ad76c3177ecefe846f7474816ed64f531b389ef4f4e8f0486867eb0`, run 2026-10-09 11:21 UTC (log `lab/logs/f0-07-procs-run2.txt`). 30 samples per series, 5 s timeout per sample. Time measured with `Instant` (CLOCK_MONOTONIC) from sending the signal until the pidfds of **all** observed processes become readable (process exit). Test child: `sleep 1000` (QEMU absent).

| Scenario | Mechanism | Outcome | p50 / p95 / max (ms) | Timeouts / errors / orphans |
| --- | --- | --- | --- | --- |
| a. SIGKILL to the supervisor, end of the direct child | `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` + `getppid()` check | verified | 0.700 / 1.142 / 3.820 | 0 / 0 / 0 |
| a2. Same, child that creates a grandchild (`sh -c 'sleep 1000 & ...; exec sleep 1000'`) | PDEATHSIG not inherited after `fork` | **limit** verified | direct child terminated = yes; **grandchild survived = yes** (then killed by the spike) | — |
| b. Signal and reaping via pidfd | `pidfd_open`, `pidfd_send_signal(SIGKILL)`, `waitid(P_PIDFD, WEXITED)` | verified | 0.080 / 0.161 / 0.191 | 0 / 0 / 0 |
| b. Identity without PID reuse | `si_code=CLD_KILLED`, `si_status=SIGKILL`; `pidfd_send_signal(fd, 0)` after reaping; `pidfd_open` on an already reaped PID | verified | 30/30 `CLD_KILLED`+`SIGKILL`; 30/30 `ESRCH` on the old pidfd; `pidfd_open` on reaped PID → `ESRCH` | — |
| c. Group stop | group-leader child (`process_group(0)`) with 2 grandchildren, `kill(-pgid, SIGKILL)` | verified | 0.215 / 0.313 / 0.315 | 0 / 0 / 0 |
| c2. Grandchild that runs `setsid` | leaves the group and the session | **limit** verified | members `pgid=8294,sid=8078`, `pgid=8295,sid=8295` (escaped), `pgid=8294,sid=8078`; **1 survivor** of `kill(-pgid)` (then killed by the spike) | — |
| UI crash | the supervisor watches the UI with a pidfd; on its death (SIGKILL) it sends SIGTERM to the VM, waits 2 s, then SIGKILL, and exits | verified | 0.629 / 1.769 / 1.916 (until the end of VM **and** supervisor) | 0 / 0 / 0 |
| Final check | user `sleep 1000` processes left | verified | 0 | — |

cgroup v2 (same run):

| Scenario | Outcome | Evidence |
| --- | --- | --- |
| d. Sub-cgroup from the startup cgroup of `wsl --` (`/init.scope`) | **not available** | `mkdir /sys/fs/cgroup/init.scope/supervisor: Permission denied (os error 13)` |
| d. Sub-cgroup in a delegated user scope (`systemd-run --user --scope -p Delegate=yes`) | verified | base `/user.slice/user-1000.slice/user@1000.service/app.slice/run-p8389-i24777.scope`, controllers `cpu memory pids`; the supervisor moves into a leaf `supervisor/` ("no internal processes" rule) and enables `+cpu +memory +pids` |
| d. `memory.max=128 MiB`, `memory.swap.max=0`, process allocating 512 MiB | verified | process terminated with signal 9, `memory.events oom_kill=1`, `memory.peak=134217728` |
| d. `cpu.max="20000 100000"` (0.2 CPU), 4 busy threads for 3 s | verified | `core_medi=0.206`, `cpu_usata_s=0.623` over 3.031 s, `nr_throttled=31`, `throttled_usec=11424258` |
| d. Control without limit (`cpu.max="max 100000"`) | verified | `core_medi=3.965`, `nr_throttled=0` |
| d. `pids.max=4` | verified | 3 `sleep` out of 8 started (the shell counts as 1), `sh: 0: Cannot fork`, `pids.events max=1` |
| d. `cgroup.kill` on a tree with a `setsid` grandchild (3 members) — end of all members (pidfd) | verified | 30 samples: p50 0.203 / p95 0.420 / max 0.511 ms; timeouts 0, errors 0, orphans 0 |
| d. `cgroup.kill` — `populated 0` in `cgroup.events` | verified | 30 samples: p50 0.213 / p95 0.434 / max 0.526 ms; timeouts 0 |

A first complete run (2026-10-09 11:08 UTC, same source built outside the workspace in `lab/wsl/standalone/` while another `spikes/*` member was not yet loadable; logs `f0-07-procs-processi.txt`, `f0-07-procs-cgroup.txt`) gave values of the same order: a 0.409/0.809/3.317; b 0.072/0.165/0.187; c 0.215/0.457/0.563; UI crash 0.557/2.870/3.047; `cgroup.kill` 0.239/0.549/0.641 ms; no timeouts, errors or orphans.

Note on the method: in an intermediate version reading `cgroup.events` waited for `POLLPRI` with a 10 ms timeout and returned about 10.6 ms per sample; with a 1 ms timeout and the separate measurement on the pidfds the value drops below 1 ms. The reference measurement is the one on the pidfds.

## Limits

- **No physical Linux host.** Everything was run in WSL2 (Hyper-V VM on Windows). The Linux/KVM smoke test of section 10 F0 and of the Ubuntu cell (section 5) was not executed.
- **Nested KVM not tested and in any case not valid.** `/dev/kvm` exists but is not accessible to the user; QEMU is not installed. No KVM numbers in this file.
- **Non-standard toolchain.** The Linux build uses zig 0.14.1 as C driver/linker (glibc 2.39 stubs) instead of Ubuntu's gcc/binutils. The resulting binary does not link `libgcc_s`. A build with `build-essential` may differ (linker, unwinding, flags) and must be repeated.
- **Software rendering only.** Under WSLg the app used llvmpipe; WSLg goes through a Weston compositor and RDP to Windows: latencies and CPU are representative neither of a Linux desktop with a GPU nor of a native X11/Wayland session. The X11 path failed because of a missing library.
- **Test child `sleep`, not QEMU.** QEMU is multi-threaded, allocates the guest RAM, uses KVM and may have helper processes. In particular the following are not verified: accounting of guest memory and KVM allocations in the cgroup, effect of `cpu.max` on KVM vCPU threads, termination times of a QEMU with a running guest (Emergency Stop p95 ≤ 2 s of section 13), behaviour of SIGTERM on QEMU.
- **Simulated controlled shutdown.** In the "UI crash" scenario the controlled shutdown is SIGTERM → 2 s wait → SIGKILL; with QEMU the controlled shutdown would go through QMP (`system_powerdown`) and is not tested. Reattaching a new UI to the supervisor (ADR-0015, part B, second alternative) **was not tested**: it requires authenticated IPC.
- **PDEATHSIG is tied to the thread.** Per `prctl(2)` the signal fires on the death of the *thread* that created the child, not of the process. The spike creates children from the main thread; a supervisor that launched QEMU from a pool thread (for example an async runtime) could kill the VM when that thread terminates. Documented behaviour, not reproduced here.
- **cgroup delegation dependent on systemd.** The sub-cgroup was created only inside a user scope with `Delegate=yes` obtained with `systemd-run --user`. Without a systemd user session (distributions without systemd, containers, some CI runners) the mechanism is not available. Not tested: creation of the scope via D-Bus (`StartTransientUnit`) by the supervisor itself.
- **PID reuse not forced.** It is verified that the pidfd returns `ESRCH` after the process is reaped; a real reuse of the same PID number was not provoked (with a high `pid_max` it would require a very large number of processes). The guarantee comes from the documented semantics of pidfd.
- **Samples.** 30 samples per series (the task asked for at least 20); section 13 requires 100 samples for stop and revocation latencies on declared hardware: these measurements on `sleep` in WSL2 do not replace that measurement.
- **Concurrency with other agents.** `apps/desktop` changed during the test: the smoke release binary (11:12 UTC) predates the changes that made the first `cargo test` fail; the second `cargo test --workspace` (11:21 UTC) concerns the subsequent state. Adding the member `spikes/f0-linux-procs` records the package in `Cargo.lock` (dependency `libc 0.2.190` already present in the lockfile).

## Required actions

User actions (none performed by this test):

1. **Physical Linux host with KVM** (Ubuntu LTS x86_64, section 5) to close the Ubuntu cell of the matrix and, later, R2a. Minimum requirements of the test: access to `/dev/kvm` for the unprivileged user, QEMU with verified provenance, guest and firmware with checksums as for Windows (F0-02/F0-03), local graphics session with a GPU. If not available in F0, record the outcome "verified on Windows only" (see Proposed decisions).
2. In the WSL distro, for a Linux build with the standard toolchain: `sudo apt install build-essential pkg-config` (possibly `libxkbcommon-x11-0` to test the X11 path). After installing, repeat `lab/wsl/build-all.sh` without `CC`/zig linker (remove the relevant lines from `lab/wsl/env.sh`).
3. Optional and **not valid for the matrix**: for a nested KVM probe in WSL2, `sudo usermod -aG kvm mortali` and `sudo apt install qemu-system-x86` (or use the QEMU binary with verified provenance chosen in ADR-0010), then `-accel kvm -S` + `query-accelerators` via QMP over Unix socket. It only serves to uncover integration problems, not to qualify the host.
4. Update the index of `docs/verification/README.md` with this file and the status in `docs/STATUS.md`/`docs/COMPATIBILITY.md` (files outside the scope of this test).

Technical activities to carry over into the backlog (`docs/RELEASE_PLAN.md`):

5. Declare in the Linux packaging the runtime dependencies loaded with `dlopen` (at least `libxkbcommon0`, `libxkbcommon-x11-0` for X11, `libwayland-client0`, `libEGL`/`libGL` or `libvulkan1` with Mesa) and turn the `xkbcommon-dl` panic into a usable error (F1, "diagnostics with usable errors").
6. Repeat the `f0-linux-procs` spike on the physical host with real QEMU as the child (ADR-0015), measuring 100 Emergency Stop samples with a running guest and the cgroup limits on the QEMU process with KVM (ADR-0017).
7. Verify the creation of the delegated scope without an external `systemd-run` (D-Bus) and the behaviour without a systemd user session.

## Proposed decisions

Proposals for the related ADRs; they are not decisions until the ADRs are updated.

### ADR-0015, part A (Linux mechanism)

The **[processes]** tests indicate that no single mechanism satisfies the four criteria of the ADR; the following combination covers them all in WSL2 with `sleep`:

1. **Every QEMU with `PR_SET_PDEATHSIG(SIGKILL)`** set in `pre_exec` with a `getppid()` check against the "parent already dead" race. Covers criterion 1 for the direct child even with `SIGKILL` to the supervisor (p95 1.1 ms, no orphans out of 30). Constraint: QEMU processes must be created from a thread that lives as long as the supervisor (thread dedicated to spawning), not from pool threads.
2. **Identity via pidfd** (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll` for the end of the process): criterion 2 verified (30/30 `ESRCH` after reaping). Kernel requirements: `pidfd_open` ≥ 5.3, `P_PIDFD` ≥ 5.4.
3. **One cgroup v2 per VM** inside a systemd user scope with `Delegate=yes`, with `memory.max`, `cpu.max`, `pids.max` and **`cgroup.kill`** (kernel ≥ 5.14) for the Emergency Stop and for recovery after a supervisor crash: it is the only tested mechanism that also terminates descendants that escaped with `setsid` (cases a2 and c2 fail with PDEATHSIG and `kill(-pgid)`). It works without privileges (criterion 3) and is the same tool as the limits of ADR-0017 (criterion 4).
4. **Process group** per VM as a fallback when cgroup delegation is not available: useful for `kill(-pgid)`, but it does not contain descendants that change group.

Consequence for section 7.2 point 8: where cgroup delegation is not available, the CPU/memory limits on the QEMU tree are missing; under the rule "a missing mandatory control blocks AI Mode", AI Mode on that host stays disabled, while manual management can use PDEATHSIG + process group, declaring its limit. To be confirmed in ADR-0017 and ADR-0009.

Still to decide: how the supervisor obtains the delegated scope (started by the UI with `systemd-run --user --scope -p Delegate=yes`, or a D-Bus `StartTransientUnit` call from the supervisor) and whether each VM has its own systemd scope or a sub-cgroup of the supervisor's scope (the latter is what was tested).

### ADR-0015, part B (manual VMs after a UI crash)

Only the **controlled shutdown** was tested: the supervisor detects the death of the UI with a pidfd (independent of the PID, no polling) and stops the VM; p95 1.8 ms until the end of VM and supervisor with `sleep` and SIGTERM. Proposal: controlled shutdown as the initial behaviour (with QMP `system_powerdown` and a timeout before SIGTERM/SIGKILL, to be tested with QEMU), because it is verifiable without IPC; reattaching remains an alternative to be tested when authenticated IPC exists. The final choice is up to the ADR owner, consistently with F0-06 on Windows.

### ADR-0016 (outcome of the Linux lab)

- On this machine **there is no physical Linux host with KVM**: there is only WSL2 with nested KVM, which the roadmap explicitly excludes as a test.
- **Assessment:** F0 **can** close with the outcome "verified on Windows only" according to the paragraph "No Linux lab", **provided that**: (a) the user confirms that no physical Linux host with KVM is available in F0 (otherwise the test must be done and the outcome would be "Windows and Linux verified" or an explicit blocker); (b) Windows on its own passes all the checks of the F0 exit criterion (F0-02…F0-06, outside this file and not assessed here); (c) the outcome is recorded in `docs/STATUS.md` and `docs/COMPATIBILITY.md`, with Windows as the only MVP target and the Ubuntu cell "not verified". This file does not close F0: it provides the allowed Linux part (accelerator-independent builds and tests), as required by the consequences of ADR-0016.
- **Tests that remain pending** with that outcome (deferred to R2a, which does not close without them):
  1. real smoke test on a physical Linux host with KVM: QEMU with verified provenance, accelerated guest boot, private QMP over Unix socket, console and input, forced stop;
  2. ADR-0015 on the physical host with real QEMU: UI crash, supervisor crash, Emergency Stop with 100 samples, no orphans;
  3. cgroup limits applied to QEMU with KVM (guest memory and kernel allocations, vCPUs, `cgroup.kill`);
  4. eframe build with the standard Ubuntu toolchain and smoke with a real GPU, native Wayland and X11;
  5. Linux packages and host policy (section 5, Ubuntu and Debian/Fedora rows).

## Proposal: minimal CI for F1

Roadmap section 10, F1: "Windows/Linux CI; macOS build on a suitable runner within F2". Section 13: on every change unit tests, schema validation, lint and build; accelerated boot, QMP, console and restore on controlled physical runners. The provider is not chosen here.

| Job | Runner | Content | Notes |
| --- | --- | --- | --- |
| `lint` | any Linux x86_64 | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings` | without accelerator |
| `build-test-linux` | Ubuntu LTS x86_64, without accelerator | `apt install build-essential pkg-config`; `cargo build --workspace --locked`; `cargo test --workspace --locked` (including headless egui_kittest tests) | standard toolchain, not zig |
| `build-test-windows` | Windows 11 x86_64, without accelerator | explicit MSVC environment (Build Tools, see F0-01 §4.1); `cargo build --workspace --locked`; `cargo test --workspace --locked` | the `LNK1104 msvcrt.lib` problem of F0-01 must be reproduced or ruled out on the runner |
| `clippy-features` | Linux and Windows | clippy with `--no-default-features --features glow` for `lightvm-desktop` | keeps the alternative backend compilable |
| `processi-linux` | Ubuntu LTS, without accelerator | `f0-linux-procs processi` (then the real supervisor tests): PDEATHSIG, pidfd, groups; cgroup part only if the runner offers a systemd user session with delegation, otherwise recorded as "not executed" | accelerator-independent |
| `processi-windows` | Windows, without accelerator | supervisor Job Object tests (F0-06) | accelerator-independent |
| `smoke-render-software` | Linux with a virtual display (for example headless Weston or Xvfb + Mesa llvmpipe) and Windows with WARP (`--wgpu-adapter cpu`) | `lightvm-desktop --smoke 5`, including Wayland and X11 on Linux | verifies the software fallback and the runtime dependencies (`libxkbcommon-x11-0`); does not measure performance |
| `acceleratore-assente` | Linux without `/dev/kvm`, Windows without WHPX | capability detection must produce the clear error required by the F1 exit criterion, without TCG fallback | relies precisely on the absence of an accelerator |
| `licenze-dipendenze` | Linux | licence inventory and dependency advisories (tool to be chosen in ADR-0023) | completion of a non-blocking F0 activity within F1 |
| `build-macos` | macOS Apple Silicon | `cargo build --workspace --locked` | within F2; portability assumptions only, no declared support; if missing, risk in `STATUS.md` |
| `lab-fisico-windows` / `lab-fisico-linux` | controlled physical runners (Windows/WHPX; Linux/KVM when available) | accelerator probe, accelerated boot, QMP, console, 20 start/stop cycles without orphans, Emergency Stop | without production credentials; not triggered automatically by untrusted PRs; the only jobs that can produce boot evidence |

The following cannot run on runners without verifiable acceleration, nor be replaced with TCG: guest boot, Emergency Stop measurements on real QEMU, resource limits on the QEMU tree with an accelerator, real console. QMP/RFB protocol tests against simulated servers are allowed (section 10 F1) and must be recorded as mocks.

## Files produced

- `docs/verification/F0-07-linux-kvm.md` (this file)
- `spikes/f0-linux-procs/Cargo.toml`, `spikes/f0-linux-procs/src/main.rs`, `spikes/f0-linux-procs/src/linux.rs`
- `lab/wsl/` (ignored by git): `env.sh`, `bin/cc`, `bin/c++`, `inventory.sh`, `inventory2.sh`, `install-user-tools.sh`, `build-desktop.sh`, `build-all.sh`, `run.sh`, `smoke.sh`, `primo-frame.sh`, `standalone/Cargo.toml`, `target/`
- `lab/logs/f0-07-*` (ignored by git): inventories, installations, builds, tests, smoke, spike measurements
