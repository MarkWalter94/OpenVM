# F0-01 — Rust toolchain, workspace and eframe spike

Phase: F0, step 1 (ROADMAP section 15: "verify toolchain; minimal build").
Execution date: 9 October 2026. Host: Windows 11 (the only host tested in this verification).

Outcome legend: **verified** = command executed with the reported outcome; **failed** = command executed with a negative outcome; **not executed** = test not performed, with reason.

## 1. Summary

| Test | Outcome |
| --- | --- |
| Cargo workspace (section 6.2) created and compiled, edition 2024, resolver 3 | verified |
| `cargo build --release -p lightvm-desktop` without warnings | verified, but only with the MSVC environment of Build Tools 2022 (see 4.1) |
| `cargo build` from the shell without the MSVC environment set | **failed**: `LNK1104 msvcrt.lib` (see 4.1) |
| Build with GNU toolchain (`stable-x86_64-pc-windows-gnu`) | verified |
| `cargo clippy --workspace --all-targets --features lightvm-desktop/glow -- -D warnings` | verified (no warnings) |
| `cargo fmt --all --check` | verified |
| wgpu smoke (Vulkan, NVIDIA) | verified |
| wgpu smoke with `WGPU_BACKEND=dx12` and `WGPU_BACKEND=gl` | verified |
| glow smoke (OpenGL 3.3, NVIDIA) | verified |
| wgpu on software rasterizer (WARP, "Microsoft Basic Render Driver") | verified, with very high CPU during the animation |
| glow without hardware acceleration (`--glow-hw off`) | **failed**: `egui_glow requires opengl 2.0+` |
| RDP session / host without GPU | not executed |
| Headless `egui_kittest` tests with AccessKit tree | verified (3 tests passed) |
| AccessKit with a real screen reader | not executed (planned in F2) |
| Path canonicalization warnings (ROADMAP section 2) | not reproduced |
| Build and smoke on Linux | not executed (no Linux host used in this step) |

## 2. Environment

Commands: `Get-CimInstance Win32_OperatingSystem`, `Win32_Processor`, `Win32_VideoController`, `Win32_ComputerSystem`, `query session`, `rustc -Vv`, `cargo -V`, `rustup show`, `vswhere.exe -products *`.

| Item | Detected value |
| --- | --- |
| OS | Microsoft Windows 11 Pro 10.0.26200, build 26200 |
| Machine | HP ZBook Fury 16 G9 Mobile Workstation PC |
| CPU | 12th Gen Intel Core i7-12800HX, 16 cores, 24 logical processors |
| RAM | 31.6 GiB visible |
| GPU 1 | Intel UHD Graphics, driver 32.0.101.7077 |
| GPU 2 | NVIDIA RTX A2000 8GB Laptop GPU, driver 32.0.15.9658 (WDDM), 596.58 (Vulkan/GL) |
| Display | 1920×1080 on both adapters, scale 1.0 (`pixels_per_point=1.00`) |
| Session | local console (`query session`: `>console MOrtali Attivo`), not RDP |
| rustup | 1.29.1 |
| rustc / cargo | 1.99.0 (b940084d7 2026-09-28) / 1.99.0 (5f94df478 2026-08-27) |
| User default toolchain | `stable-x86_64-pc-windows-gnu` (`~/.rustup/settings.toml`), while the "default host" is `x86_64-pc-windows-msvc` |
| Toolchain active in the workspace | `1.99.0-x86_64-pc-windows-msvc (overridden by 'E:\OpenVM\rust-toolchain.toml')` |
| GNU linker | WinLibs GCC 16.1.0 (MinGW-W64 x86_64-ucrt-posix-seh) in PATH |
| Visual Studio installations detected | Build Tools 2022 17.14.41; VS Enterprise 2026 18.10.0; VS Enterprise 2022 17.14.40; VS Enterprise 2019 16.11.60 |
| Windows SDK | 10.0.26100.0 |

Note: on the first cargo invocation in the workspace, rustup **automatically installed** into the user profile the toolchain `1.99.0-x86_64-pc-windows-msvc` required by `rust-toolchain.toml` (message `warn: the missing active toolchain ... has been auto-installed`, with a warning that auto-installation is deprecated). It is a user installation in `~/.rustup`, not a system one. Same compiler commit (b940084d7) as the GNU toolchain already present.

## 3. Workspace created

| Path | Content |
| --- | --- |
| `Cargo.toml` | workspace, `resolver = "3"`, edition 2024, `rust-version = "1.99"`, MIT licence, `[workspace.lints]` (rust: `unsafe_code = deny`, `missing_debug_implementations`, `unreachable_pub`, `rust_2018_idioms`, `unused_qualifications`; clippy: `all`, `dbg_macro`, `todo`, `unimplemented`, `undocumented_unsafe_blocks = deny`) |
| `Cargo.lock` | 394 packages, no git sources |
| `rust-toolchain.toml` | `channel = "1.99.0"`, components `rustfmt`, `clippy`, profile `minimal` |
| `apps/desktop/` | crate `lightvm-desktop` (lib + bin), eframe spike |
| `crates/vm-core`, `qemu-backend`, `vm-storage`, `lightvm-service`, `lightvm-proto` | skeletons: only `lib.rs` with a responsibility comment, no API |
| `tests/{integration,security,e2e}/README.md` | placeholders |

`crates/mcp-server` was not created: the roadmap assigns it to F5. No `.cargo/config.toml` was created.

### Versions of the chosen crates

Verified with `cargo search` and `cargo info <crate>@<version>` on 9 October 2026; pinned with `=` in `[workspace.dependencies]`.

| Crate | Version | Notes |
| --- | --- | --- |
| egui | =0.36.2 | latest on crates.io at the time of the verification |
| eframe | =0.36.2 | `rust-version` 1.95; features used: `accesskit`, `default_fonts`, `wayland`, `x11`, plus `wgpu` (default of the desktop crate) or `glow` |
| egui_extras | =0.36.2 | without default features (not yet used in the code) |
| egui_kittest | =0.36.2 | dev-dependency; pulls in `kittest 0.4.0` (the latest on crates.io is 0.5.0, but egui_kittest 0.36.2 requires 0.4) |

Transitive versions resolved in the lockfile: egui-wgpu 0.36.2, egui_glow 0.36.2, egui-winit 0.36.2, wgpu 30.0.1, glow 0.17.0, glutin 0.32.3, winit 0.30.13, accesskit 0.24.1, accesskit_winit 0.32.2, accesskit_windows 0.32.1.

eframe features deliberately excluded: `links` (opens URLs in the browser, contrary to ROADMAP 6.0 "never automatically openable links"), `persistence`, `web_screen_reader`.

### Desktop app (spike)

- "LightVM" window 1460×860 points, dark theme by default, switch to light in Settings.
- Side panel with Dashboard, Console, Settings; central panel with a 1280×720 texture generated in memory and updated at most 10 times per second via `ctx.request_repaint_after` (no continuous repaint).
- Options: `--smoke SECONDS`, `--renderer wgpu|glow` (if both features are enabled), `--static` (still framebuffer, to measure idle), `--wgpu-adapter cpu|auto`, `--glow-hw preferred|required|off`.
- The smoke test prints `SMOKE ...` lines with outcome, backend and adapter, egui/eframe/backend versions read from `Cargo.lock` in `build.rs`, window size, frames drawn, framebuffer updates, time to first frame, histogram of the intervals between frames and egui repaint causes.

The framebuffer is simulated: it is not a real console and does not satisfy any console criterion (ROADMAP 6.4).

## 4. Build

### 4.1 MSVC linker: failure without the environment set

Command (Git Bash, no MSVC variable set):

```text
cargo build --release -p lightvm-desktop
```

Outcome: **failed** for all build scripts.

```text
= note: "C:\\Program Files\\Microsoft Visual Studio\\18\\Enterprise\\VC\\Tools\\MSVC\\14.51.36231\\bin\\HostX64\\x64\\link.exe" ...
= note: LINK : fatal error LNK1104: impossibile aprire il file 'msvcrt.lib'
```

Identified cause: rustc automatically picks the most recent installation (VS Enterprise 2026, MSVC 14.51.36231), which contains `link.exe` but not the CRT libraries. Checked with `ls .../VC/Tools/MSVC/<ver>/lib/x64/msvcrt.lib`:

| Installation | `link.exe` | `lib/x64/msvcrt.lib` |
| --- | --- | --- |
| VS Enterprise 2026, MSVC 14.51.36231 | present | absent |
| VS Enterprise 2022, MSVC 14.44.35207 | present | absent |
| Build Tools 2022, MSVC 14.44.35207 | present | present |
| VS Enterprise 2019, MSVC 14.29.30133 | absent | absent |

Workaround used, without changes to the system: every cargo command with an MSVC target was run in a `cmd` that first calls `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat` (temporary wrapper in the session scratchpad, not in the repository). `vcvars64.bat` prints `"vswhere.exe" non è riconosciuto...`, a harmless warning. With this environment:

```text
cargo build --release -p lightvm-desktop
    Finished `release` profile [optimized + debuginfo] target(s)
```

Outcome: **verified**, no warnings (recompilation of the desktop crate after the last change, output without `warning` lines).

Proposed fixes (user decision, not applied):
1. install in VS 2026 the "MSVC Build Tools x64/x86" component with libraries (or remove the partial installation), so that automatic detection finds a complete toolset; or
2. always work from a "Developer PowerShell/Command Prompt" of Build Tools 2022 and repeat the same setup in CI; or
3. adopt the GNU toolchain (see 4.2), which however changes ABI, debug symbol format and signing/diagnostic tools compared with the standard Windows ecosystem.

### 4.2 GNU toolchain

```text
cargo +stable-x86_64-pc-windows-gnu build --release -p lightvm-desktop --target-dir target/gnu
    Finished `release` profile [optimized + debuginfo] target(s) in 2m 21s
```

Outcome: **verified**; the wgpu smoke (3 s) of the GNU binary ends with `SMOKE esito=ok`, Vulkan NVIDIA backend, `primo_frame_ms=1028`. Executable of 102.5 MiB versus 13.3 MiB for MSVC: the difference comes from the embedded DWARF debug information (release profile with `debug = "line-tables-only"`); with MSVC it ends up in the separate PDB.

### 4.3 Lint and formatting

```text
cargo fmt --all --check                                                           -> exit 0
cargo clippy --workspace --all-targets --features lightvm-desktop/glow -- -D warnings  -> exit 0
cargo clippy -p lightvm-desktop --all-targets --no-default-features --features glow    -> no warnings
```

Outcome: **verified** (MSVC environment of 4.1).

## 5. Smoke and measurements per backend

### Method

- Temporary PowerShell script (session scratchpad). Startup: N consecutive runs of `lightvm-desktop.exe --smoke 0.5` measured with `Measure-Command`, plus the `primo_frame_ms` value printed by the app (from entering `main` to the first UI pass). The `Measure-Command` total includes the 0.5 s wait of the smoke test and the shutdown: it is not the startup time.
- Memory: process started with `--smoke 16`; `WorkingSet64` and `PrivateMemorySize64` from `System.Diagnostics.Process` (the same as `Get-Process`) read after 3 s.
- CPU: difference in `TotalProcessorTime` between 3 s and 13 s (10 s window), as a percentage of one logical core.
- Warm starts only (binary and driver already cached). Cold starts not measured: they would require a reboot or flushing the cache.
- Window in the foreground, no mouse or keyboard interaction during the measurements.
- Build: `target\release` (feature `wgpu`) and `target\glow\release` (`--no-default-features --features glow`), both MSVC.

The sample is small and on a single host: it indicates orders of magnitude, it is not the formal measurement of section 13 (which requires, among other things, 5 minutes of idle and summing UI + supervisor).

### Results

| Configuration | Adapter reported by the app | First frame p50 / p95 / max (ms) | Working set at 3 s | Private bytes at 3 s | CPU 3–13 s, 10 Hz animation | CPU 3–13 s, `--static` |
| --- | --- | --- | --- | --- | --- | --- |
| default wgpu (20 starts) | `backend=Vulkan adapter="NVIDIA RTX A2000 8GB Laptop GPU" tipo=DiscreteGpu driver_info="596.58"` | 705 / 919 / 1005 | 177.6 MiB | 396.3 MiB | 7.18% | 0.16% |
| wgpu, `WGPU_BACKEND=dx12` (10 starts) | `backend=Dx12 adapter="NVIDIA RTX A2000 8GB Laptop GPU" tipo=DiscreteGpu driver="32.0.15.9658"` | 501 / 538 / 538 | 179.2 MiB | 357.8 MiB | 3.90% | not measured |
| wgpu, DX12 + `--wgpu-adapter cpu` (WARP, 10 starts) | `backend=Dx12 adapter="Microsoft Basic Render Driver" tipo=Cpu driver="10.0.26100.9278"` | 448 / 482 / 482 | 124.8 MiB | 376.3 MiB | **785.73%** (about 7.9 logical cores) | 0.16% |
| glow (20 starts) | `vendor="NVIDIA Corporation" renderer="NVIDIA RTX A2000 8GB Laptop GPU/PCIe/SSE2" versione_gl="3.3.0 NVIDIA 596.58"` | 294 / 326 / 347 | 82.9 MiB | 81.4 MiB | 7.49% | 0.47% |

`Measure-Command` total (includes 0.5 s of smoke and shutdown), p50/p95: wgpu 1069/1333 ms; glow 638/653 ms; wgpu DX12 784/820 ms; WARP 1246/1307 ms.

For all configurations with a GPU the smoke test reports `finestra_punti=1460x860 pixels_per_point=1.00 finestra_px=1460x860`. Over 16 s of animated run: wgpu Vulkan `frame=159 aggiornamenti_fb=152`, glow `frame=165 aggiornamenti_fb=157`, wgpu DX12 `frame=164 aggiornamenti_fb=156`; with WARP `frame=89 aggiornamenti_fb=88`, i.e. about 5.5 updates per second instead of 10: the software rasterizer cannot keep up with the requested rate. With `--static`: 7–9 frames in 16 s.

Other smoke tests run (4–5 s each, outcome `ok`): `WGPU_BACKEND=gl` → `backend=Gl adapter="NVIDIA RTX A2000 8GB Laptop GPU/PCIe/SSE2" driver_info="4.6.0 NVIDIA 596.58"`; glow `--glow-hw required` → same NVIDIA adapter.

Observations:
- The process uses the discrete NVIDIA GPU in all cases with default wgpu/glow (`PowerPreference::HighPerformance` is the default in egui-wgpu). On a hybrid laptop this keeps the dGPU powered on; `WGPU_POWER_PREF=low` would allow choosing the integrated one, not tested.
- CPU with animation (4–7.5% of one core) is dominated by the generation of the 1280×720 pattern and by copying it into a new `ColorImage` at every update, not by rendering. It is not representative of the real display client.
- With wgpu the private bytes (358–396 MiB) greatly exceed the working set; with glow the two measurements are close (about 80 MiB). The section 13 target (manager memory ≤250 MiB, UI + supervisor) must be evaluated by stating which metric is used: with wgpu the working set of the UI alone fits, the private bytes do not.
- First frame slower with wgpu/Vulkan (p50 705 ms) than with glow (294 ms) or wgpu/DX12 (501 ms).

### Problem found and fixed during the spike

The first version requested the repaint 100 ms after the last update and checked `elapsed >= 100 ms`. Measurement: 244 frames for 43 updates in 5 s, with 106 intervals between frames under 2 ms. Cause, read in the egui 0.36.2 source (`context.rs`, `request_repaint_after`): egui brings every delayed repaint forward by the expected duration of a frame (`predicted_dt`), so the wake-up arrives before the deadline and the app requested immediate repaints in bursts. Fix: deadlines on a fixed 100 ms grid with a tolerance equal to `predicted_dt` + 3 ms. After the fix: 52 frames for 43 updates (wgpu) and 61 for 48 (glow) in 5 s. This is a useful indication for the real display client too: the repaint rate must be checked with a measurement, not deduced from the code.

## 6. Software rendering and hosts without acceleration

| Test | Command | Outcome |
| --- | --- | --- |
| wgpu adapter list | `lightvm-desktop.exe --smoke 4 --wgpu-adapter cpu` (prints the adapters on stderr) | Vulkan: NVIDIA, Intel UHD 770; DX12: NVIDIA, Intel UHD, **Microsoft Basic Render Driver (Cpu)**; GL: NVIDIA |
| wgpu on WARP | `--wgpu-adapter cpu` (with and without `WGPU_BACKEND=dx12`) | verified: `adapter="Microsoft Basic Render Driver" tipo=Cpu`; high CPU with animation (section 5) |
| wgpu, CPU adapter Vulkan only | `WGPU_BACKEND=vulkan` + `--wgpu-adapter cpu` | failed as expected: `Adapter selection failed: nessun adattatore wgpu di tipo CPU disponibile` (no software Vulkan driver installed) |
| glow without acceleration | `--glow-hw off` | failed: `egui_glow: OpenGL: egui_glow requires opengl 2.0+.` Without acceleration WGL only offers the Microsoft GDI OpenGL 1.1 implementation |
| Alternative software OpenGL | search for `C:\Windows\System32\opengl32sw.dll` | absent; no Mesa/llvmpipe installed and, per the task rules, no installation performed |

Fallback options available in eframe/egui-wgpu 0.36.2 (read in the source):
- environment variables read by the default `WgpuSetup::CreateNew`: `WGPU_BACKEND` (`vulkan`, `dx12`, `metal`, `opengl`, `webgpu`), `WGPU_POWER_PREF` (`low`, `high`, `none`), plus debug/validation flags;
- `NativeOptions::wgpu_options.wgpu_setup` → `WgpuSetupCreateNew::native_adapter_selector` to choose the adapter (used by `--wgpu-adapter cpu`);
- `force_fallback_adapter` is fixed to `false` in egui-wgpu and is not exposed;
- `NativeOptions::glow_options.hardware_acceleration` (`Required`, `Preferred`, `Off`) for glow.

Not verified on this host: RDP session, machine without a GPU or with a basic driver, VM. In an RDP session Windows normally exposes a remote adapter (or WARP); the outcome must be tested, not deduced. Proposal for a later test: same binary, smoke in an RDP session to this host and on a VM without a GPU, recording the adapter chosen automatically and the one forced with `--wgpu-adapter cpu`.

## 7. Headless UI tests with egui_kittest

Command (MSVC environment of 4.1): `cargo test -p lightvm-desktop`

```text
running 3 tests
test albero_accesskit_contiene_le_voci_di_navigazione ... ok
test click_su_dashboard_cambia_sezione ... ok
test tema_scuro_predefinito_e_passaggio_al_chiaro ... ok
test result: ok. 3 passed; 0 failed; 0 ignored
```

Outcome: **verified**. File: `apps/desktop/tests/kittest_ui.rs`. The tests build the UI with `Harness::builder().with_size(1280×800).build_ui_state(...)`, look up the AccessKit nodes with `get_by_role_and_label(Role::Button, "Dashboard")` (and Console, Settings), click an item and change the theme.

Problem encountered: `get_by_label("Console")` fails because it finds two nodes (the navigation button and the section title). The test therefore uses role plus label. Original message: `If you were expecting multiple nodes, use query_all instead of query.`

Limits: the test uses neither a GPU nor a native window, and verifies neither rendering nor a real UIA client (Narrator, NVDA). Graphical snapshots (feature `snapshot` + `wgpu` of egui_kittest) were not tested.

## 8. Path canonicalization warnings

The reconnaissance in section 2 of the roadmap reports canonicalization warnings emitted by Rust/Cargo. In this verification they **were not reproduced**. Tests run, all without warnings:

| Condition | Commands |
| --- | --- |
| Git Bash, cwd `E:\OpenVM` | `rustc -Vv`, `cargo -V`, `rustup show`, full builds |
| PowerShell, cwd `E:\OpenVM\docs` and `E:\` | `rustc -V`, `cargo -V` |
| `cmd /c`, cwd `E:\OpenVM` | `rustc -V`, `cargo -V` |
| UNC cwd `\\localhost\e$\OpenVM` (Git Bash) | `rustc -V`, `cargo -V`, `rustup show active-toolchain` |
| cwd `E:\CPro\packages\rtvc` (directory with a rustup override) | `rustc -V`, `cargo -V` |

Elements detected that may explain the original warning, without direct proof:
- `E:` is a local NTFS volume (`Win32_LogicalDisk DriveType=3`), no `subst` nor network drive; `~/.cargo` and `~/.rustup` are not links or junctions; no `CARGO_*`/`RUSTUP_*` variable set.
- `~/.rustup/settings.toml` contains an override with a verbatim path: `'\\?\E:\CPro\packages\rtvc' = "stable-x86_64-pc-windows-gnu"`. `rustfmt` also prints `\\?\E:\OpenVM\...` paths. It is the format returned by `std::fs::canonicalize` on Windows; on its own it did not produce warnings in the tests.
- Most likely hypothesis: the reconnaissance was run with a cwd or in an environment (for example a sandbox with limited access to parent directories, or a cwd that no longer existed) in which canonicalization failed. It was not possible to recreate that condition.

Proposed fix: no change to the system until the warning reappears. If it reappears, record the exact text, the cwd and the shell, and run `rustup show` and `cargo -vV` in the same condition. If the warning mentions the verbatim override, remove and recreate it with `rustup override unset --path E:\CPro\packages\rtvc` and `rustup override set ...` (user action, not executed).

## 9. Limits of the verification

- A single physical host, local console session, NVIDIA and Intel GPUs with drivers installed. No tests on Linux, RDP, VM or machine without a GPU.
- Measurements on small samples, warm only, 10 s windows (section 13 requires 5 minutes of idle and 20 cold and warm starts).
- The framebuffer is a generated pattern: CPU measurements with animation do not apply to the real RFB console.
- AccessKit verified only through the tree in egui_kittest, not with a screen reader.
- The MSVC build works only with the Build Tools 2022 environment loaded manually (section 4.1).
- The numbers depend on the driver versions listed in section 2.

## 10. Proposed decisions for the ADR (egui/eframe and backend)

To be evaluated and recorded in the ADR by the responsible agent or person; this document does not decide them.

1. **Versions:** pin egui, eframe, egui_extras and egui_kittest to `=0.36.2`, with a versioned `Cargo.lock` and no git dependencies. Update all four together, repeating this verification.
2. **eframe features:** `default-features = false` with `accesskit`, `default_fonts`, `wayland`, `x11` and one backend. Exclude `links` (ROADMAP 6.0) and `persistence` until needed.
3. **Default backend on Windows (user decision):** two measured options:
   - *glow*: faster first frame (p50 294 ms), much lower memory (about 83 MiB of working set, 81 MiB private), smaller binary (6.8 MiB versus 13.3), but no software fallback on Windows (`--glow-hw off` fails) and OpenGL is a less well maintained API on Windows drivers;
   - *wgpu*: software fallback verified (WARP via DX12), multiple backends (Vulkan, DX12, GL), but private bytes around 360–400 MiB and slower startup with Vulkan.
   Technical proposal: wgpu as the product backend to have the WARP fallback, with DX12 preferred on Windows (first frame and CPU better than Vulkan in this measurement), and glow kept compilable as a diagnostic alternative. The choice must be confirmed by the user in light of the section 13 memory budget.
4. **Software fallback:** use WARP only when there is no hardware adapter, because with the console animated at 10 Hz it consumes about 7.9 logical cores and does not sustain 10 updates per second. If adopted, declare it as a degraded mode in `docs/COMPATIBILITY.md`, plan a reduced console frequency and verify it in RDP and on a VM before declaring it.
5. **Adapter selection:** decide between `HighPerformance` (default, activates the dGPU on hybrid laptops) and `LowPower` for an app that often stays idle.
6. **Windows toolchain (user decision):** the MSVC target (proposed, consistent with PDB, code signing and Windows tools) requires a complete, automatically detectable MSVC toolset or an explicit "Developer" environment in development and in CI; alternatively the GNU target, already working on this host.
7. **UI test framework:** egui_kittest 0.36.2 is usable for headless tests via AccessKit; graphical snapshots and tests on Linux remain to be tried within F1.
8. **Repaint rate:** the real display client must schedule repaints on deadlines that take into account egui's `predicted_dt` advance, and the verification must measure the actual frames (the smoke test counts frames and intervals).

## 11. Files produced by this step

- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`
- `apps/desktop/Cargo.toml`, `apps/desktop/build.rs`, `apps/desktop/src/lib.rs`, `apps/desktop/src/main.rs`, `apps/desktop/tests/kittest_ui.rs`
- `crates/{vm-core,qemu-backend,vm-storage,lightvm-service,lightvm-proto}/{Cargo.toml,src/lib.rs}`
- `tests/{integration,security,e2e}/README.md`
- this document
