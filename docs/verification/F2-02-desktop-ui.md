# F2-02 — Desktop UI: Dashboard, wizard, console on the display ticket, dialogs, Settings

- Execution date (UTC): 2026-10-10
- Executed by: agent "desktop" (F2 work split)
- Roadmap step: section 10, F2 (Dashboard, wizard, console, dialogs, Settings, accessibility); section 6.4 (console); section 7.4 (negative test on text fields)
- Related ADRs: ADR-0013 (protected console transport), ADR-0008 (media registry), ADR-0015 (UI loss), ADR-0017 (VM limits), ADR-0022 (UI tests)
- Overall outcome: partial — UI verified headless with a stand-in supervisor and with the real supervisor without VMs; end-to-end with real QEMU **failed for a host condition** (free disk below the supervisor's storage reserve), console on a real guest **not executed**

## Purpose

Demonstrates, for `apps/desktop`:

1. Dashboard per F2: system, state, configured vCPU/RAM, virtual disk size and space used (`disk_used_bytes`), actions Start / Stop (orderly) / Force stop / Reset / Console / Edit / Delete enabled by state; force stop, reset and delete in a secondary "More" menu with confirmation.
2. "New VM" wizard: name (the only text field of the UI, validated live with `VmName::parse`), system, installation media (approved list or "Import ISO…" with the native picker and `ImportMedia`), optional data media, CPU/RAM/disk bounded by `Capabilities.host`, network (only Offline selectable), summary and `CreateVm`.
3. Console of the product path: `OpenConsole` → `rfb_client::Transport::Ticket`, framebuffer as texture with aspect ratio, focus, keyboard (declared layouts US/IT) and mouse as `SubmitVm Input(InputBatch)` with run and current geometry, release of keys on focus loss/disconnection/new run, connection state, reconnection on a new run, full screen, screenshot PNG via save dialog. The direct QMP path and the F0 options `--vnc-socket/--qmp-socket/--vnc-tls/--tls-dir/--capture-dir/--tmp-dir/--layout/--max-fps/--dump-fb` were **removed** from the UI (no developer mode kept: it would have needed a second input path).
4. Dialogs: deletion (names the VM, only when stopped, `DeleteVm` with revision, waits for the outcome), reset and force stop confirmations, close of the app with running VMs (close cancelled, "N VMs are running: Stop them and quit / Cancel", orderly stop with progress, supervisor shutdown, then close).
5. Settings: QEMU paths (existing), host capacity from `Capabilities.host` and resulting per-VM maxima, guest keyboard layout, read-only Security card (offline only, no clipboard, no shared folders, MCP not available, agent sessions disabled).
6. Coordinator additions: "Media" group in the VM Edit panel (remove or replace installation/data media, only when stopped) and, on the Dashboard, a hint on VMs stopped after at least one run with installation media attached ("Installation finished? Remove the installation media…") that removes it after confirmation (`UpdateVm` with `install_media: None`). Release builds use the Windows GUI subsystem.

It does NOT demonstrate: rendering on real hardware of the new views (snapshots are WARP headless), screen readers (AccessKit tree only), multi-monitor and DPI scaling on real displays, a real guest behind the console, Linux.

## Environment

| Item | Value |
| --- | --- |
| Host | Windows 11 Pro 10.0.26300, NVIDIA GPU (glow GL 3.3 in the smoke run) |
| Toolchain | Rust workspace toolchain via `cargo-msvc.cmd` (MSVC 2022), egui/eframe/egui_kittest 0.36.2 |
| QEMU | `lab/qemu/qemu-11.1.0` (approved build; reported by the supervisor as `qemu 11.1.0 (v11.1.0-12130-ge470268ff4)`) |
| Media | `lab/media/ubuntu-26.04.1-desktop-amd64.iso` (6 482 409 472 bytes), `lab/media/p5-dati.iso` |
| Supervisor | `lightvm-service` from the working tree of the parallel "service" agent (F2 operations implemented at the time of the test) |
| Graphics session | headless (egui_kittest; snapshots on wgpu WARP) and one local `--smoke` run |
| Other conditions | **system volume C: with 9.1 GB free** (931 GB disk) |

## Procedure

```powershell
$env:CARGO_TARGET_DIR="C:\Woarkareas\OpenVM\target\desktop"
cargo clippy -p lightvm-desktop --all-targets -- -D warnings
cargo clippy -p lightvm-desktop --no-default-features --features glow --all-targets -- -D warnings
cargo clippy -p lightvm-desktop --no-default-features --features wgpu --all-targets -- -D warnings
cargo build -p lightvm-service            # for service_child.rs and the E2E test
cargo test -p lightvm-desktop
$env:UPDATE_SNAPSHOTS=1; cargo test -p lightvm-desktop --test visual_snapshots   # then *.old.png/*.diff.png deleted
# Real supervisor + QEMU (ignored test):
$env:LIGHTVM_QEMU_DIR="C:\Woarkareas\OpenVM\lab\qemu\qemu-11.1.0"
$env:LIGHTVM_ISO="C:\Woarkareas\OpenVM\lab\media\ubuntu-26.04.1-desktop-amd64.iso"   # then p5-dati.iso
cargo test -p lightvm-desktop --test real_console -- --ignored --nocapture
# UI + real supervisor, no VM:
target\desktop\debug\lightvm-desktop.exe --smoke 4 --catalog %TEMP%\lightvm-smoke-f2\catalog --qemu-dir lab\qemu\qemu-11.1.0 --settings %TEMP%\lightvm-smoke-f2\ui.json
# GUI subsystem (release):
cargo build --release -p lightvm-desktop
```

Headless tests use a stand-in supervisor (`tests/common/mod.rs`, implementing `service::Supervisor`): the state is built by the test, the commands sent by the UI are recorded. They are **mock** tests in the sense of rule 4.

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| clippy `-D warnings`, default, glow-only, wgpu-only, all targets | verified | no warnings |
| Unit and headless tests | verified | 102 passed, 3 ignored (real QEMU): lib 37, clipboard_ui 5, console_ui 6, dialogs_ui 6, kittest_ui 9, service_child 3 (real supervisor, no QEMU), service_ui 6, visual_snapshots 14, vm_settings_ui 6, wizard_ui 4 |
| Only one text field, only in the wizard, accessible name "VM name" | verified (mock) | `wizard_ui::only_one_text_field_and_only_in_the_wizard`: 0 fields in Dashboard, Console, Settings, Edit panel and wizard steps 2–7; 1 `TextInput` labelled "VM name" on step 1, focused on open |
| Live name validation, duplicate names, Enter → next step | verified (mock) | `wizard_ui::full_flow_creates_the_vm` |
| Media from the registry, Import ISO with the picker, progress, auto-selection, `unsupported` explained | verified (mock) | `full_flow_creates_the_vm`, `unsupported_media_registry_is_explained` |
| Resources bounded by `Capabilities.host` (MVP limits ∩ host) | verified (mock) | label "Bounded by this host: up to 8 vCPU, 12 GiB … 180 GiB", `wizard::tests::bounds_follow_the_host` |
| Network: only Offline selectable | verified (mock) | NAT/bridged radio nodes disabled |
| `CreateVm` with the chosen config; failure shown, retry; wizard closes on success | verified (mock) | `full_flow_creates_the_vm` |
| Dashboard actions enabled by state (Stopped/Running/Starting/Failed matrix), per-VM accessible names | verified (mock) | `service_ui::actions_enabled_by_state`, `no_action_with_service_terminated_or_not_connected` |
| Delete dialog: names the VM, focus on Cancel, Esc cancels, `DeleteVm{revision}`, waits, `unsupported` explained, closes on `VmDeleted` | verified (mock) | `dialogs_ui::delete_names_the_vm_and_waits_for_the_outcome` |
| Reset / force stop confirmations, reset carries the run | verified (mock) | `reset_and_force_stop_ask_and_carry_the_run`, `stale_confirmation_is_refused` |
| Close with running VMs: `CancelClose`, dialog, Cancel, Stop them and quit → `Stop` per VM, progress, supervisor stopped, `Close` | verified (mock) | `close_with_running_vms_asks_then_stops_them`, `close_without_running_vms_is_immediate` |
| Install media hint and removal (`UpdateVm`, `install_media: None`); Edit panel Media group | verified (mock) | `service_ui::install_media_hint_after_first_run`, `dialogs_ui::installation_media_removal_after_confirmation`, `vm_settings_ui::media_removed_or_replaced`, `media_not_editable_while_running` |
| Console coordinates after resize (1100×760, 900×600, 1600×700, 950×1000): aspect 1.6 kept, corners → 0/32767, centre within 2 px, nothing outside the image, batches with run and geometry 1280×800 | verified (mock, synthetic frame) | `console_ui::coordinates_are_correct_after_resize` |
| Keys only with focus, Tab to the guest, release on focus loss | verified (mock) | `keyboard_goes_as_batches_only_with_focus_and_is_released_on_focus_loss` |
| Full screen `Ctrl+Alt+Enter`, leave `Ctrl+Alt+Esc`, panels hidden, chord keys not forwarded, buttons | verified (mock) | `chords_toggle_full_screen_and_leave_without_reaching_the_guest` |
| Screenshot PNG via save dialog | verified (mock) | `screenshot_is_saved_as_png_through_the_save_dialog` (PNG signature) |
| Console empty states, ticket request, no retry after `unsupported`, VM stopped | verified (mock) | `empty_states_and_ticket_request` |
| New run → release for the old run, new ticket | verified (mock) | `new_run_reconnects` |
| Clipboard: Paste/Copy/Cut never reach the host clipboard or the name field | verified (mock) | `clipboard_ui` (5 tests incl. `copy_does_not_write_the_host_clipboard`), `clipboard::tests::copy_and_cut_become_keys` |
| Snapshots regenerated and checked visually | verified | 19 PNGs in `apps/desktop/tests/snapshots/` (new: dashboard, dashboard_900x600, dashboard_light, dialog_close, dialog_delete, wizard_name, wizard_name_invalid, wizard_install_media, wizard_resources, wizard_network, wizard_summary; removed: vm_list, vm_list_empty, dashboard_vm_list) |
| UI + real supervisor (QEMU 11.1.0, WHPX), no VM | verified | `SMOKE service pid=30032 connected=true backend=qemu 11.1.0 (…) accelerator=whpx vms=0`, `service_stop code=Some(0) forced=false` |
| E2E with real QEMU: Ubuntu ISO import through the wizard | failed (host) | 1st run: import completed in 120.8 s (6.4 GB copy + SHA-256); later runs: `media import: I/O error … Spazio su disco insufficiente (os error 112)` after earlier catalogs filled the disk; catalogs removed |
| E2E with real QEMU: create, start, console, installer screen | not executed (blocked) | with the 8 MB `p5-dati.iso` the import succeeded in 0.2 s, then the wizard stopped at Resources: host free space 9 GiB < 16 GiB supervisor reserve, so no VM can be created on this host (`wizard blocked: … step: Resources … disk_gib: 8`). Correct product behaviour; console on a real guest not reached |
| Release `windows_subsystem = "windows"` | verified with caveat | no console window; output captured with `| Out-File`, `cmd /c … > file` and `Start-Process -Wait -RedirectStandardOutput` (11 SMOKE lines each); PowerShell `& exe --smoke 2 > out.txt` does **not** wait and writes 0 bytes; `--help` in an interactive console prints nothing (prints with `Start-Process -NoNewWindow -Wait` or redirection). Debug builds keep the console |

## Limits

- All UI behaviour above, except `service_child.rs` and the smoke run, is verified with a stand-in supervisor and a synthetic frame: the display socket, the ticket handshake and the core's acceptance of the input batches were not exercised from the UI.
- The end-to-end test (`tests/real_console.rs`, ignored, `LIGHTVM_QEMU_DIR` + `LIGHTVM_ISO`) is ready but could not complete on this host because of free disk space; the test now removes its catalog at the end (`LIGHTVM_E2E_KEEP` keeps it).
- The F0 script `apps/desktop/scripts/keyboard-test-u4.ps1` was rewritten for the product path (wizard + console); not executed.
- Keyboard layouts: qcodes are positional, the IT/US choice only declares the guest layout (as in F0-04); AltGr and keypad limits of egui-winit unchanged.
- Focus ring: egui's "active" style with a 2 px accent stroke, plus a ring on the navigation items during keyboard navigation; the Tab order of right-aligned header buttons follows creation order (Emergency Stop first).
- No screen reader run; no real DPI/multi-monitor run.

## Required actions

- Free at least ~25 GB on the test host (ISO copy + 16 GiB reserve + disk growth) and rerun `real_console.rs` with the Ubuntu ISO; then the U-4 script.
- Packaging/user guide: document how to capture `--smoke`/`--help` output of the GUI-subsystem release build.

## Proposed decisions

- ADR-0013: the UI keeps no developer path to QEMU sockets; the console is reachable only through `open_console` tickets.


## Update 2026-10-10 (night, coordinator): end-to-end with real QEMU after integration

Section appended (rule 8). After integrating the service work (asynchronous graceful stop, per-VM UEFI variables, user input while stopping) and freeing disk space, `real_console` was run on the integrated tree (debug build, real `lightvm-service`, QEMU 11.1.0 WHPX, `ubuntu-26.04.1-desktop-amd64.iso`):

```
E2E ISO imported and selected: 77.8 s
E2E wizard: Some((Summary, 2, 4096, 31))
E2E VM created: 0.1 s
E2E VM running: 63.5 s
E2E framebuffer received: 0.1 s
E2E console geometry=(1280, 800) link="Connected" stats=ConsoleStats { rfb_updates: 250, texture_uploads: 725, uploaded_pixels: 4852224, input_events: 0, resizes: 1 }
E2E input_error=None
E2E screenshot=…\e2e-console.png bytes=36764
E2E service_stop=Some((Some(0), false))
test result: ok. 1 passed; 0 failed
```

Notes: "VM running 63.5 s" includes the re-verification of the 6.4 GB install ISO hash at every start (ADR-0008); the stats line is printed before the two arrow keys are sent, so it does not prove input delivery (input is verified in `F2-01`). Default CPU model changed to `Skylake-Client-v4` in this integration (`lightvm-service` adapter), the model used by this run. Workspace after integration: fmt and clippy `-D warnings` clean (four variants), `cargo test --workspace --locked`: 424 passed, 0 failed, 18 ignored.
