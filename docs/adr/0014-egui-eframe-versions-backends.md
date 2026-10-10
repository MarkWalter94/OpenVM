# ADR-0014 — egui/eframe versions and rendering backend

- Status: Accepted (Windows); Linux backend deferred to R2a with ADR-0016
- Date: 2026-10-09
- Owner: to be assigned

## Context

ADR-0001 settles on egui/eframe. The roadmap requires pinning the egui/eframe and backend versions in F0 (section 6.0) and verifying the minimal build on Windows and Linux, the backend (wgpu or glow) and the behaviour on hosts without graphics acceleration, for example RDP (section 10 F0). The "no rendering" risk calls for a verified software fallback and a declared limit (section 14). The manager's memory target is 250 MiB for UI plus supervisor (section 13); the metric proposed in `F0-09` §5 is private bytes.

## Decision

**Versions** (pinned with `=` in `[workspace.dependencies]`, `Cargo.lock` committed, no git dependencies):

| Crate | Version |
| --- | --- |
| egui, eframe, egui_extras, egui_kittest | 0.36.2 |
| Resolved transitive dependencies | egui-wgpu 0.36.2, egui_glow 0.36.2, egui-winit 0.36.2, wgpu 30.0.1, glow 0.17.0, glutin 0.32.3, winit 0.30.13, accesskit 0.24.1, accesskit_winit 0.32.2, accesskit_windows 0.32.1 |
| Toolchain | Rust 1.99.0 (`rust-toolchain.toml`), edition 2024 |

The four egui versions are upgraded together, repeating the `F0-01` verification. `egui_commonmark` is not yet in the workspace: it comes in with a version compatible with egui 0.36.2, pinned the same way, when the report view is needed.

**eframe features:** `default-features = false` with `accesskit`, `default_fonts`, `wayland`, `x11` and the `glow` and `wgpu` backends. Excluded: `links` (opening URLs in the browser, contrary to section 6.0), `persistence`, `web_screen_reader`.

**Backend on Windows:**

1. **Default: glow** (OpenGL 3.3). Measurements with the real console: working set 91.8–94.5 MiB, private bytes 88.4–97.6 MiB; first frame p50/p95 294/326 ms (spike, 20 warm starts); CPU with animation similar to wgpu (9–10% of one core). It is the only measured backend that meets the memory target with the private bytes metric, leaving headroom for the supervisor.
2. **Selectable: wgpu with DX12** (`WGPU_BACKEND=dx12` or an equivalent option). Private bytes 369–372 MiB with the UI alone: **declared deviation** from the section 13 memory target when the user chooses this backend.
3. **Declared degraded mode: wgpu on WARP** ("Microsoft Basic Render Driver"), the only verified software fallback on Windows: glow without acceleration fails (`egui_glow requires opengl 2.0+`, because without a WGL driver only GDI OpenGL 1.1 is available). WARP with 10 Hz animation uses about 7.9 logical processors and sustains about 5.5 updates per second; private bytes 376 MiB. In this mode the console frame rate is reduced and the interface signals the condition.
4. Proposed automatic selection: try glow; on initialization error, retry with wgpu DX12 on a hardware adapter and then on WARP, with a visible indication of the actual backend. The automatic fallback is **not yet implemented or tested** (F1/F2 backlog).
5. wgpu adapter: preference to be decided between `HighPerformance` (default, activates the discrete GPU on hybrid laptops) and `LowPower`; to be measured in F2.

**Rationale for the choice over the alternative (wgpu as default):** wgpu offers the WARP fallback and more APIs, but commits about 370 MiB with the UI alone. Choosing wgpu as default would mean permanently publishing a deviation from the memory target in the normal configuration, to serve a case (hosts without a GPU) that remains degraded even with wgpu (very high CPU on WARP). glow as default keeps the normal configuration within the target; glow's lack of a software fallback is compensated by the fallback to wgpu/WARP, with its deviation declared only where needed.

**Linux:** build verified in WSL2 (zig C driver) with wgpu on llvmpipe (Vulkan and GL); glow not tested on Linux; no physical host with a GPU. The default Linux backend is decided in R2a; both builds remain compilable in CI.

**Repaint pacing:** the display client schedules repaints taking into account egui's `predicted_dt` lead (grid-aligned deadlines with tolerance) and the verification counts actual frames; each `request_repaint()` from the RFB thread currently produces two UI passes, to be reduced in F2.

Verification of the criteria:

1. Real build on Windows and Linux: met (Windows with an explicit MSVC environment; Linux in WSL2 with a non-standard toolchain).
2. Startup in a local session and RDP: local met; **RDP not run**, limit declared in `docs/COMPATIBILITY.md`.
3. 1280×720 texture without continuous repaint: met (1280×720 texture in the spike and real 1280×800 framebuffer with per-rectangle updates).
4. AccessKit tree exposed: met via egui_kittest (screen reader in F2).
5. Versions compatible with each other and with the toolchain: met.

## Alternatives considered

- wgpu as default with DX12 (technical proposal in `F0-01` §10): rejected as default because of the memory budget, kept as selectable and as fallback.
- wgpu with Vulkan: slower first frame (p50 705 ms) and higher CPU than DX12 in the spike.
- glow only, without wgpu: excluded, it would leave hosts without a GPU with no fallback.

## Consequences

- The manager's memory and CPU measurements (section 13) are published per backend, with private bytes and working set kept separate.
- CI builds and tests both configurations (`--no-default-features --features glow` and wgpu).
- On hosts without a GPU or in RDP the actual mode must be tested before it is declared; until then it is a limit.
- A version upgrade requires repeating the minimal test.

## Required evidence

- Build, smoke and measurements per backend, WARP, glow without acceleration, egui_kittest: `docs/verification/F0-01-toolchain-eframe.md` §4–§7 (verified; RDP not run).
- Real console with wgpu DX12 and glow, CPU and memory measurements: `docs/verification/F0-04-console-rfb-input.md` Results §3, §7 (verified).
- Linux build and smoke in WSL2: `docs/verification/F0-07-linux-kvm.md` §2–§3 (verified **[build]**).
- RDP session and automatic glow → wgpu fallback: to be run (F1/F2 backlog).

## References

Roadmap, sections 2, 6.0, 10 (F0, F2), 13, 14, 17. Review: `docs/verification/F0-09-evidence-review.md`.
