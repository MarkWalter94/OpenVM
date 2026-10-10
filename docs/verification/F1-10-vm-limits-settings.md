# F1-10 — Per-VM limits, VM settings and opening from the command line

- Execution date (UTC): 2026-10-10, from about 10:30 to 11:10
- Executed by: development agent (Claude Code session), at the request of the project owner
- Roadmap step: section 7.2 (resource limits observed on the QEMU tree), 8 (storage budget), 10 F1 ("resource validation"); backlog `docs/RELEASE_PLAN.md` F1-04 (resource validation), F1-05 (Job Object); owner's direction on ADR-0017 of 2026-10-10
- Related ADRs: ADR-0017 (updated), ADR-0009, ADR-0015, ADR-0026
- Overall outcome: **partial** — CPU and memory limits applied to QEMU's Job Object and verified on hardware (Windows, WHPX); editing of stopped VMs via IPC, UI and command line verified; storage budget verified only as a check at startup; measurement of the CPU ceiling's effect under load, storage pressure tests, MCP and Linux **not executed**

Allowed outcomes: **verified**, **failed**, **not executed** (section 17). Raw logs in `lab/logs/` (outside the repository): `f1-17-limiti-job.txt`, `f1-17-impostazioni-vm.txt`, `f1-03/*-limiti-job.log`.

## Purpose

Direction from the project owner: limits are set **in the VM settings**, also **from the command line when opening a VM** and, later, **from MCP**. This test demonstrates:

1. the per-VM limit model and its validation in the core;
2. the application of the limits by the supervisor to the QEMU process (Job Object), with read-back;
3. editing a stopped VM via IPC (expected revision, disk only growing, audit), from the UI (panel without text fields) and from the command line (`lightvm-desktop --apri-vm`);
4. the return to `stopped` of VMs left in `recovering` after a supervisor restart and the normal closing of the UI without an Emergency Stop (defects found during the test).

It does not demonstrate: the effect of the CPU ceiling measured under guest load (tested with the spike in `F0-08`, not repeated here), complete storage accounting (overlays, screenshots and logs per run: F3), pressure tests (filling the volume), limits set from MCP (F5), Linux (cgroup v2, R2a).

## Model

`VmConfig.limits` (`lightvm-proto`, schema `vm_config.json`), with default values for existing catalogs:

| Field | Range | Default | Application |
| --- | --- | --- | --- |
| `cpu_max_percent` | 5–100 | 100 (no ceiling) | Job Object hard cap: `vcpus × percent × 100 / logical processors` hundredths of a percentage point of the machine, 1–10000 |
| `memory_margin_mib` | 256–16384 | 1024 | job memory = guest RAM + margin (`JOB_OBJECT_LIMIT_JOB_MEMORY`) |
| `storage_budget_gib` | 1–16384 or absent | absent | at every start: bytes under `vms/<vm_id>/` excluding the primary disk; above the budget → `resource_limit`, no process |

Editing: IPC request `update_vm` (UI only; agent sessions → `permission_denied`; F5 will have to stay within the user's limits), VM stopped and without operations, expected revision, disk only growing (`qemu-img resize` at the next start), audit action `vm_configure`.

## Environment

As in `F1-03-real-qemu-backend.md` (Windows 11 Pro 10.0.26200, i9-14900K with 32 logical processors, Rust 1.99.0 MSVC, QEMU 11.1.0 with WHPX). Catalog for the command-line test: `C:\lvmtest\catalogo` (short path).

## Procedure

```text
cargo test --workspace --locked
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_limiti_job
set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture e2e_real_qemu_impostazioni
lightvm-desktop --impostazioni C:\lvmtest\imp.json --apri-vm prova-cli --crea --cpu-max 25 --memoria-margine 768 --ram 1024 --smoke 8 --static
lightvm-desktop --impostazioni C:\lvmtest\imp.json --apri-vm prova-cli --smoke 8 --static
```

## Results

| Item | Outcome | Evidence |
| --- | --- | --- |
| Validation of the limits in the core (ranges, budget ≤ maximum disk) | verified (synthetic) | `scenarios::update_vm_config_rejections_have_no_effect` |
| Editing a stopped VM: revision +1, audit `vm_configure succeeded` | verified (synthetic) | `scenarios::update_vm_config_changes_resources_and_limits_of_a_stopped_vm` |
| Rejections without effect: superseded revision, shrunk disk, limits out of range, resources beyond the limits, agent session, running VM | verified (synthetic) | same test; audit `vm_configure denied` |
| Via IPC with SQLite: persistence after restart, revision conflict, pairing → `permission_denied` | verified | `core_path::update_vm_via_ipc_is_persisted_and_audited` |
| Computation of the job limits | verified (unit) | `adapters::backend::tests::limiti_del_job_dai_limiti_della_vm` (2 vCPUs at 50 % on 32 processors → 312; minimums and maximums) |
| Limits applied and read back from the Job Object | verified on hardware | `reale_limiti_job_cpu_e_memoria`: `JobLimitReport { cpu_rate: Some(300), job_memory_bytes: Some(3221225472), peak_job_memory_bytes: 2268483584 }`, VM running; unit test `job_con_limiti_cpu_e_memoria` |
| Job memory lower than the guest RAM | verified on hardware | 512 MiB for a 2048 MiB guest: start rejected after 148 ms, `QEMU è terminato durante l'avvio (codice 0x1): cannot set up guest memory 'pc.ram'` |
| Through the service: disk 8 → 10 GiB, CPU ceiling 25 %, margin 768 MiB | verified on hardware | `e2e_real_qemu_impostazioni_vm`: `disco della VM ingrandito a 10 GiB`, `qemu-img info` 10737418240 bytes, VM `running` with one QEMU process |
| Storage budget exceeded | verified on hardware | budget 1 GiB, 2 GiB file in `vms/<id>/overlays/`: start `failed` with `resource_limit`, no QEMU process |
| VM Settings panel: no text fields, save only with changes and the VM stopped | verified (headless) | `vm_settings_ui.rs` (3 tests) |
| UI commands on a real supervisor without QEMU: edit, `--apri-vm` with changes, nonexistent VM, `--crea` | verified | `servizio_figlio::impostazioni_vm_e_apri_vm_senza_qemu` |
| `lightvm-desktop --apri-vm prova-cli --crea --cpu-max 25 …` with real QEMU | verified on hardware | `SMOKE servizio … vm=1 vm_accese=1 ultimo_esito="avvio: riuscito"` |
| Normal closing of the UI with a VM running | **failed, then verified after a fix** | before: `processo genitore terminato` and Emergency Stop (the EOF following the `shutdown` line was treated as a crash); after the first fix: orderly shutdown but 37 s and supervisor timeout (ACPI 60 s versus 30 s); after the second: bounded orderly shutdown (ACPI 15 s → `quit` 5 s → kill 5 s), `arresto completato` without timeout |
| VM left in `recovering` after a supervisor terminated at the timeout | **failed, then verified after a fix** | before: `stati=prova-cli:recovering`, no start possible; after: `1 VM riportate a stopped dopo il riavvio`, `stati=prova-cli:running`; test `observations::reconcile_after_restart_returns_recovering_vms_to_stopped` |
| Workspace | verified | `cargo test --workspace --locked`: 353 passed, 0 failed, 12 ignored; clippy `-D warnings` clean (workspace, UI variants, service without `supervisore`) |
| Effect of the CPU ceiling under guest load | verified (see the 2026-10-10 section at the end) | Ubuntu boot from disk: 0.22 cores without a ceiling, 0.10 cores with a ceiling of 0.1 cores |
| Limits from MCP | not executed | F5 |
| Linux | not executed | cgroup v2, R2a |

## Limits

- A single host; one sample for each real test.
- The storage budget counts the files under `vms/<vm_id>/` (overlays, baselines, firmware); per-run logs and screenshots (`runs/`) are not yet attributed to the VM: complete accounting in F3.
- QEMU exiting at the memory limit once the run has started is recognized from the job's peak and recorded only as a supervisor warning; it does not yet become a distinct code in the core's observation.
- Reconciliation at restart assumes that no process of a previous supervisor is alive: true on Windows (Job Object with `KILL_ON_JOB_CLOSE`), to be verified on Linux (R2a).
- ~~The `runs` row of the interrupted run stays open in the catalog after reconciliation at restart.~~ Resolved the same day: reconciliation closes open runs with cause `recovered` (`core_path::restart_reconciles_running_vm_and_closes_open_run`).
- `--crea` creates a VM without installation media: importing ISOs into the media registry comes with the guided creation (F2).

## Required actions

1. Measure the effect of the CPU ceiling with a guest that loads the CPU (after installation to disk, F1-17).
2. F3: complete per-VM storage accounting and pressure tests.
3. F5: MCP tool for the limits, within the limits set by the user.
4. ~~Close the interrupted `runs` row during reconciliation at restart.~~ Done (see Limits).

## Proposed decisions

Recorded in ADR-0017 (section of 2026-10-10).

## Update of 2026-10-10 (about 11:45 UTC): effect of the CPU ceiling with a real guest

Section appended at the end (rule 8). Guest: Ubuntu installed to disk in `F1-03` (section F1-17), 2 vCPUs, 4096 MiB, `Skylake-Client-v4`; CPU time of the QEMU process read from the system (`TotalProcessorTime`) in the first 60 s of boot. Test: `real_qemu::reale_effetto_tetto_cpu_durante_il_boot`.

| Item | Outcome | Evidence |
| --- | --- | --- |
| First run: ceiling at 25 % of the vCPUs (0.5 cores) | inconclusive | without a ceiling 13.0 s of CPU (0.22 cores on average), with the ceiling 12.4 s: the boot does not reach 0.5 cores, the ceiling does not kick in (a defect of the test, not of the limit) |
| **Ceiling at 5 % of the vCPUs (0.1 cores, `cpu_rate` 31 on 32 processors)** | verified | without a ceiling 13.0 s (0.22 cores), with the ceiling **5.8 s (0.10 cores)**: consumption equal to the granted share, less than half the unconstrained consumption; no orphans |

The ceiling still has to be measured with a sustained load in the guest (session with login), useful for higher ceilings.
