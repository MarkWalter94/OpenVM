# F1-05 — Desktop UI: host clipboard, rendering backend selection, snapshots and first measurements with the supervisor

- Execution date (UTC): 2026-10-10, from about 09:35 to 10:10
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 6.0 (native UI), 6.4 (console), 13 (measurements); backlog `docs/RELEASE_PLAN.md` F1-13, F1-16, F1-18, F1-19 (snapshots)
- Related ADRs: ADR-0001, ADR-0003, ADR-0014, ADR-0018, ADR-0022 (not modified)
- Overall outcome: **partial** — automated UI tests green; backend selection with fallback verified on hardware; first UI + supervisor measurements recorded (memory with wgpu above the target, as D-12); host clipboard read inside egui-winit **not blocked**; manual clipboard test, RDP session and cold starts **not executed**

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `f1-18/` (`riepilogo.txt`, `avvii-caldo-glow.csv`, `avvii-caldo-wgpu.csv`, `idle-*.csv`, `idle-*-stdout.txt`, `ambiente.txt`), `f1-18-idle300/`, `f1-05/smoke*.txt`.

This file closes the UI stream interrupted in commit `aa04ccd` (code of `clipboard.rs`, `render.rs`, `libraries.rs`, snapshots): the code was present, the tests were missing.

## Purpose

1. F1-13: a `Paste` produced by egui-winit carries no host clipboard text either into the console or into other widgets, and the combination reaches the guest as keys.
2. F1-16: automatic selection glow → wgpu (DX12) → WARP with a visible indication, and fallback when a backend fails.
3. F1-19 / ADR-0022: `egui_kittest` graphical snapshots compared with files in the repository.
4. F1-18: memory (private bytes and working set) and idle CPU of UI + supervisor over 5 minutes; warm UI starts.

It does not demonstrate: blocking of the host clipboard read upstream of egui-winit, behaviour in RDP, cold start, accessibility with a real screen reader, Linux.

## Environment

| Item | Value |
| --- | --- |
| Host | Intel Core i9-14900K (32 logical processors), 64 GB RAM, NVIDIA GPU (OpenGL driver "NVIDIA 577.00") |
| OS and build | Microsoft Windows 11 Pro 10.0.26200 |
| Power profile | "Prestazioni eccellenti" (`powercfg /getactivescheme`) |
| Toolchain | Rust 1.99.0 MSVC (see `F1-03` §Environment); `target/release` binaries of 2026-10-10 |
| UI | egui/eframe 0.36.2, wgpu 30.0.1, glow 0.17.0 |
| Graphics session | local, 125 % scale (`pixels_per_point=1.25`), window 1460×860 points |
| Supervisor | `lightvm-service` started by the UI with `--catalogo` on an empty catalog, without `--qemu-dir` (backend "not configured", no VMs) |
| Other conditions | non-dedicated host; during the second glow idle series `vm-storage` tests and rendering smoke tests were running (see Limits) |

## Procedure

```text
cargo test --workspace --locked          (UI tests included)
cargo build --release --locked -p lightvm-desktop -p lightvm-service

lightvm-desktop --renderer auto --smoke 1 --static
lightvm-desktop --glow-hw off --smoke 1 --static
lightvm-desktop --renderer warp --smoke 1 --static
lightvm-desktop --renderer glow,warp --glow-hw off --smoke 1 --static
```

Measurements with a PowerShell script (in the session scratchpad, not in the repository):

- **warm start**: 20 consecutive runs per backend of `lightvm-desktop --renderer <glow|wgpu> --smoke 0.5 --static`; first frame from entering `main` (`SMOKE primo_frame_ms`) and duration of the whole process (external stopwatch);
- **idle UI + supervisor**: `lightvm-desktop --renderer <glow|wgpu> --smoke S --static --catalogo … --runtime-dir … --audit-log …`; after 10 s, samples every 5 s of `PrivateMemorySize64` and `WorkingSet64` of the UI and of the child `lightvm-service.exe`; CPU = CPU time consumed / elapsed time, as a percentage of one logical core. Two series: `S = 300` (effective window 271 s, shorter than the 5 minutes required) and `S = 340` (window 311 s).

## Results

### Host clipboard (F1-13)

| Item | Outcome | Evidence |
| --- | --- | --- |
| `Paste` with the console focused → `Ctrl+V` to the guest as keys, no text | verified (headless) | `tests/clipboard_ui.rs::paste_con_console_a_fuoco_diventa_ctrl_v_via_qmp`: events sent to the QMP channel replaced by a local `Receiver` |
| `Shift+Insert` → `Shift+Insert` to the guest | verified (headless) | `shift_insert_diventa_shift_insert_via_qmp` |
| No text in other widgets | verified (headless) | `paste_non_produce_testo_in_altri_widget` (text field outside the app in the same pass); control: without the app the paste inserts text (`controllo_senza_app_il_paste_inserisce_testo`) |
| `Paste` buffer overwritten with zeros, physical key replaced | verified (unit) | `appunti::tests::incolla_diventa_tasto_v_senza_testo`, `shift_insert_diventa_insert`, `senza_incolla_nessuna_modifica` |
| **Host clipboard read** | **not blocked** | egui-winit 0.36.2 calls `Clipboard::get()` on Ctrl+V before the app receives the input; the `clipboard` feature cannot be disabled from eframe and the only earlier hook requires `unsafe` (rationale in `apps/desktop/src/clipboard.rs`). The text read is discarded and zeroed, but the UI process reads it |
| Manual test with a physical keyboard and the real clipboard | not executed | requires the real console with QEMU and a person at the keyboard (P-4) |

### Rendering backend (F1-16)

| Item | Outcome | Evidence |
| --- | --- | --- |
| `auto` on hardware → glow | verified | `SMOKE backend=glow … indicatore="Rendering: glow (OpenGL) · automatico"` |
| glow without acceleration (`--glow-hw off`) → fallback to wgpu DX12 | verified | `tentativo_fallito glow:egui_glow … requires opengl 2.0+`; `backend=wgpu … "Rendering: wgpu DX12 · ripiego automatico"` |
| Forced WARP | verified | `backend=warp degradato=true forzato=true … "Rendering: wgpu WARP (software) · forzato"` |
| List `glow,warp` with glow failed → WARP, degraded mode | verified | `backend=warp degradato=true … "· ripiego automatico"` |
| Status bar with degraded mode and failed attempts | verified (snapshot) | `istantanee::istantanea_barra_di_stato_degradata`, `istantanea_barra_di_stato_glow`; `render::tests` (4 tests) |
| RDP session | not executed | limit L-1 unchanged (U-8) |

### `egui_kittest` snapshots (F1-19, ADR-0022)

| Item | Outcome | Evidence |
| --- | --- | --- |
| 6 snapshots compared with the PNGs in `apps/desktop/tests/snapshots/` | verified | `tests/visual_snapshots.rs`: VM list, empty list, degraded and glow status bar, console with a synthetic frame, dashboard with list; `test result: ok. 6 passed` (wgpu renderer of the test, tolerance in `kittest.toml`) |
| Same snapshots on another host or GPU | not executed | stability across different GPUs is not tested (risk for CI) |

### Measurements (F1-18; non-dedicated host, not release measurements)

| Measurement | Target (sec. 13) | Series | p50 | p95 | Maximum | Errors | Outcome |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Warm start, first frame — glow | p95 ≤ 3 s | 20 | 117 ms | 139 ms | 140 ms | 0 | within the target |
| Warm start, whole process (`--smoke 0.5`) — glow | — | 20 | 633 ms | 642 ms | 645 ms | 0 | recorded |
| Warm start, first frame — wgpu DX12 | p95 ≤ 3 s | 20 | 239 ms | 248 ms | 414 ms | 0 | within the target |
| Warm start, whole process — wgpu DX12 | — | 20 | 700 ms | 732 ms | 739 ms | 0 | recorded |
| Cold start | p95 ≤ 3 s | — | — | — | — | — | not executed (flushing the file cache requires a reboot or privileges) |

| Idle UI + supervisor | Window | UI CPU (% core) | Supervisor CPU | Private bytes UI / supervisor (last sample) | Total private bytes (max) | Working set UI / supervisor | Outcome |
| --- | --- | --- | --- | --- | --- | --- | --- |
| glow, series 1 | 271 s, 54 samples | 0.006 % | 0 % | 98.0 / 2.4 MiB | 100.8 MiB | 67.1 / 14.1 MiB | within the targets; window < 5 min |
| glow, series 2 | 311 s, 62 samples | 0.141 % | 0 % | 103.0 / 2.4 MiB | 105.5 MiB | 73.3 / 11.0 MiB | within the targets (CPU < 1 %, memory ≤ 250 MiB) |
| wgpu DX12, series 1 | 271 s, 54 samples | 0.069 % | 0 % | 489.1 / 2.4 MiB | 491.9 MiB | 183.1 / 10.9 MiB | CPU within the target; **memory above 250 MiB** |
| wgpu DX12, series 2 | 311 s, 62 samples | 0.025 % | 0 % | 488.0 / 2.4 MiB | 491.8 MiB | 185.3 / 11.0 MiB | as above (deviation D-12 confirmed with the supervisor) |

In all four runs, at the end of the smoke the UI stopped the supervisor with the `shutdown` line: `SMOKE servizio_arresto codice=Some(0) forzato=false`; no leftover `lightvm-service.exe`.

## Limits

- A single host; no repeated measurement on a dedicated host (ADR-0018). The glow CPU in series 2 (0.141 %) is higher than in series 1 (0.006 %): during series 2 tests and smoke windows were running on the same desktop; cause not isolated.
- The measured supervisor is idle, without VMs or QEMU: QEMU's memory is not included (section 13: the manager is measured).
- The clipboard tests are headless: they go through neither winit nor the operating system.
- The snapshots are generated and compared on the same host.

## Required actions

1. F1-13: decide how to prevent the host clipboard read inside egui-winit before F2 (fork or patch of eframe/egui-winit without `clipboard`, or a Win32 filter in a separate crate with reviewed `unsafe`) — `RELEASE_PLAN.md` F1-13.
2. D-12: reduce private bytes with wgpu (device limits, allocator, giving up wgpu as the default on Windows) or declare glow as the only default within the target.
3. Cold start after a host reboot (20 samples) and RDP session (U-8).
4. Manual clipboard test with the real console together with P-4.

## Update of 2026-10-10: preferred backend as a setting

At the request of the project owner the rendering backend is a setting in Settings (saved in `impostazioni-ui.json`, effective from the next start; `--renderer` takes precedence): automatic (default: glow → wgpu → WARP), glow first, wgpu first (higher memory, D-12), WARP only. Every choice except WARP keeps the fallback to the other backends. Verification: `impostazioni::tests::ogni_preferenza_produce_un_piano_valido`, `campi_assenti_valgono_come_predefiniti`, `kittest_ui::settings_salva_politica_e_rendering` (headless). Host clipboard read: **no setting** — the roadmap excludes the clipboard from the MVP (sections 4, 6.4, 7.2) and an ADR cannot override it; blocking the read inside egui-winit remains action 1.

## Proposed decisions

- **ADR-0022:** accept `egui_kittest` with snapshots as the UI test framework for F1–F2, with the declared risk of stability across different GPUs to be verified in CI.


## Update of 2026-10-10 (about 12:00 UTC): host clipboard read blocked in egui-winit (F1-13)

Section appended at the end (rule 8). The outcome "host clipboard read not blocked" in the Results section is superseded by this test.

### Change

`egui-winit` 0.36.2 is replaced, through `[patch.crates-io]`, by a copy in `vendor/egui-winit` with two changes (described in `vendor/README.md`, marked `LightVM` in the sources): `Clipboard::get()` always returns `None` (this also covers `State::clipboard_text()`, used by eframe for `RequestPaste`) and the paste combination is no longer turned into `Event::Paste`: it continues as `Event::Key`, which the console forwards to the guest. No `unsafe`. The app's filter (`apps/desktop/src/clipboard.rs`) remains as a second line of defence.

### Procedure

Script in the session scratchpad: sets a recognizable text in the Windows clipboard, starts `apps/desktop/scripts/keyboard-test-u4.ps1 -Smoke 45` (real console on the Ubuntu ISO), brings the "LightVM" window to the foreground and sends with `WScript.Shell.SendKeys` three Ctrl+V and one Shift+Insert (real Windows keyboard events → winit → egui-winit). Same test with the patch disabled (`[patch.crates-io]` line commented out), then restored.

### Results

| Item | Outcome | Evidence (`lab/logs/f1-13-ctrl-v*.txt`) |
| --- | --- | --- |
| **Without the patch**: egui-winit reads the clipboard and generates `Paste` | verified (reference) | `appunti_scartati=2` (`Paste` events intercepted and zeroed by the app's filter) |
| **With the patch**: no clipboard read, no `Paste` | verified | `appunti_scartati=0` with the same combinations |
| Workspace with the patch | verified | `cargo test --workspace`: 356 passed, 0 failed; clippy `-D warnings` clean (workspace, glow and wgpu variants) |
| Combination forwarded to the guest as keys | not executed with the system keyboard | `eventi_input=0` in both tests: the console did not have keyboard focus (a click on the image is needed); covered by P-4 (manual test) and by the headless tests |

Limit: the patch must be reapplied at every egui/eframe update (ADR-0014, ADR-0023). Note: the test overwrote the content of the host's Windows clipboard.


## Update of 2026-10-10 (about 12:15 UTC): memory with wgpu (D-12)

Section appended at the end (rule 8). Cause identified: egui-wgpu creates the device with `wgpu::MemoryHints::Performance` (wgpu's default), which preallocates large memory blocks. `apps/desktop/src/main.rs` now wraps egui-wgpu's descriptor setting `memory_hints: MemoryHints::MemoryUsage` (limits and features unchanged).

Measurement: release binary, `lightvm-desktop --renderer <wgpu|warp> --smoke 45 --static --senza-supervisor`, `PrivateMemorySize64` every 5 s for 30 s (6 samples, constant values), same host and session, before and after the change a few minutes apart.

| Configuration | UI private bytes | Outcome |
| --- | --- | --- |
| wgpu DX12, `Performance` (before) | 488.6 MiB | above the 250 MiB target (D-12, as in the F1-18 measurements above) |
| **wgpu DX12, `MemoryUsage`** | **187.6 / 189.4 MiB** (two runs) | **within the target** |
| **wgpu WARP, `MemoryUsage`** | **188.2 MiB** | within the target (in F0: 376.3 MiB) |

UI tests green (53), smoke `esito=ok`. Not measured in this section: idle CPU over 5 minutes and console frame rate under load with the new hint.


## Update of 2026-10-10 (evening): new visual system for the UI

Section appended at the end (rule 8). The UI moves from egui's default style to its own visual system (`apps/desktop/src/theme.rs`): dark (default) and light palette with a single accent colour, type scale, spacing, cards, settings rows with a description, status indicators (dot + text), pills, notices, primary/danger/subtle buttons. Rules kept: no text fields, state never conveyed by colour alone, no clipboard functions.

Visible changes: sidebar with brand and supervisor status; Dashboard with a supervisor summary (status, backend, acceleration, policy on close) and VM rows with a configuration summary, state in Italian and a single primary action per row (Start when the VM is off, Stop otherwise) plus "Modifica"; the VM's "Risorse e limiti" panel grouped into Resources and Limits; Settings page in cards (Appearance, Supervisor, abnormal shutdown, Rendering, Diagnostics); the two "Sfoglia…" have distinct accessible names; sliders have an accessible name even without text next to them.

Snapshots (`apps/desktop/tests/visual_snapshots.rs`): the 6 existing ones regenerated with the new theme, 4 new ones (`dashboard_supervisor`, `impostazioni_vm`, `settings`, `settings_chiaro`); the paths shown in the snapshots are fixed and do not depend on the user's profile.

UI fixes from the general code review (same day, before the commit):

| Problem | Fix | Test |
| --- | --- | --- |
| egui's slider corrected the values (range, step) when the VM panel opened: phantom changes that could be saved | `SliderClamping::Edits` | `vm_settings_ui.rs`: `aprire_il_pannello_non_crea_modifiche` |
| Selectable labels: Ctrl+C copied text to the host clipboard | `selectable_labels = false` in both themes | `kittest_ui.rs`: `etichette_non_selezionabili` |
| Restarting or stopping the supervisor from Settings on the UI thread (window frozen for up to ~75 s) | dedicated thread, "restart in progress" state, actions disabled in the meantime | existing headless tests; smoke |
| With `--catalogo` Settings reported changes to apply and a restart lost the command-line configuration | the command-line configuration stays in use across restarts and is shown | — |
| Mouse wheel forwarded to the guest even with the pointer outside the console | forwarded only with the pointer over the image | `console.rs`: `rotella_solo_con_il_puntatore_sopra_la_console` |
| Theme not saved; first frame with the default style | theme in the settings file; style installed when the window is created | `kittest_ui.rs`: `tema_scuro_predefinito_e_passaggio_al_chiaro` |
| `--apri-vm` overwrote the cause of the supervisor failing to start | cause kept and written to stderr | — |
| Dropdowns and buttons without a useful accessible name | accessible names ("Margine di memoria", "Budget di storage", "Nessuna distribuzione QEMU", …) | `vm_settings_ui.rs`, `kittest_ui.rs` |
| Supervisor status different between sidebar, Dashboard and Settings; VM panel open after a restart; "le 1 VM accese" | a single function for text and tone; panel closed on stop; singular | — |

Final outcome: `cargo fmt --check` and `cargo clippy -D warnings` clean (workspace, UI glow only, UI wgpu only, service without `supervisore`); `cargo test --workspace --locked`: 365 passed, 0 failed, 16 ignored; smoke `--renderer glow` and `--renderer warp` with `esito=ok`.


## Update 2026-10-10 (night): English codebase, start page, no simulated console by default

Section appended (rule 8).

- The whole repository except the frozen `docs/ROADMAP.md` is now in English: code identifiers, comments, UI text, messages, CLI help, scripts, file names and documentation. Verification files keep captured output, commands and names as recorded at the time.
- Desktop CLI flags renamed (`--catalogo` → `--catalog`, `--senza-supervisor` → `--no-supervisor`, `--apri-vm` → `--open-vm`, `--crea` → `--create`, `--disco` → `--disk`, `--memoria-margine` → `--memory-margin`, `--budget-disco` → `--disk-budget`, `--impostazioni` → `--settings`, `--servizio-exe` → `--service-exe`, `--salta-controllo-librerie` → `--skip-library-check`); SMOKE keys in English (`esito` → `outcome`, …). Settings file `ui-settings.json`, with the legacy `impostazioni-ui.json` still read when the new one is missing; default catalog folder `catalog`, with the legacy `catalogo` still used when it exists.
- The app opens on the Dashboard. Without a live console the Console page shows an empty state instead of the animated simulated framebuffer (ROADMAP F2 exit criterion: no screen with simulated data); the simulated framebuffer is opt-in with `--simulated-console` and implied by `--smoke` without a live console, so the earlier idle measurements keep their meaning.
- Snapshots regenerated with English text and names (11, including the new `console_empty`).

Result: `cargo fmt --check` and `cargo clippy -D warnings` clean (workspace, UI glow-only, UI wgpu-only, service without `supervisor`); `cargo test --workspace --locked`: 370 passed, 0 failed, 16 ignored.
