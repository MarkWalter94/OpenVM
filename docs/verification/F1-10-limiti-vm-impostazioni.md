# F1-10 — Limiti per VM, impostazioni della VM e apertura da riga di comando

- Data di esecuzione (UTC): 2026-10-10, dalle 10:30 alle 11:10 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 7.2 (limiti di risorse osservati sull'albero QEMU), 8 (budget storage), 10 F1 ("validazione risorse"); backlog `docs/RELEASE_PLAN.md` F1-04 (validazione risorse), F1-05 (Job Object); indicazione del responsabile su ADR-0017 del 2026-10-10
- ADR collegati: ADR-0017 (aggiornata), ADR-0009, ADR-0015, ADR-0026
- Esito complessivo: **parziale** — limiti CPU e memoria applicati al Job Object di QEMU e verificati su hardware (Windows, WHPX); modifica delle VM spente via IPC, UI e riga di comando verificata; budget di storage verificato solo come controllo all'avvio; misura dell'effetto del tetto CPU sotto carico, prove sotto pressione dello storage, MCP e Linux **non eseguiti**

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `f1-17-limiti-job.txt`, `f1-17-impostazioni-vm.txt`, `f1-03/*-limiti-job.log`.

## Scopo

Indicazione del responsabile del progetto: i limiti si impostano **nelle impostazioni della VM**, anche **da riga di comando all'apertura di una VM** e, più avanti, **da MCP**. Questa prova dimostra:

1. il modello dei limiti per VM e la loro validazione nel core;
2. l'applicazione dei limiti da parte del supervisor al processo QEMU (Job Object), con rilettura;
3. la modifica di una VM spenta via IPC (revisione attesa, disco solo in crescita, audit), dalla UI (pannello senza campi di testo) e da riga di comando (`lightvm-desktop --apri-vm`);
4. il ritorno a `stopped` delle VM rimaste in `recovering` dopo un riavvio del supervisor e la chiusura normale della UI senza Emergency Stop (difetti trovati durante la prova).

Non dimostra: l'effetto del tetto CPU misurato sotto carico del guest (provato con lo spike in `F0-08`, non ripetuto qui), la contabilità completa dello storage (overlay, screenshot e log per esecuzione: F3), prove sotto pressione (riempimento del volume), limiti impostati da MCP (F5), Linux (cgroup v2, R2a).

## Modello

`VmConfig.limits` (`lightvm-proto`, schema `vm_config.json`), con valori predefiniti per i cataloghi esistenti:

| Campo | Intervallo | Predefinito | Applicazione |
| --- | --- | --- | --- |
| `cpu_max_percent` | 5–100 | 100 (nessun tetto) | hard cap del Job Object: `vcpus × percento × 100 / processori logici` centesimi di punto percentuale della macchina, 1–10000 |
| `memory_margin_mib` | 256–16384 | 1024 | memoria del job = RAM guest + margine (`JOB_OBJECT_LIMIT_JOB_MEMORY`) |
| `storage_budget_gib` | 1–16384 o assente | assente | a ogni avvio: byte sotto `vms/<vm_id>/` escluso il disco principale; oltre il budget → `resource_limit`, nessun processo |

Modifica: richiesta IPC `update_vm` (solo UI; sessioni agente → `permission_denied`; F5 dovrà restare entro i limiti dell'utente), VM spenta e senza operazioni, revisione attesa, disco solo in crescita (`qemu-img resize` al primo avvio successivo), azione di audit `vm_configure`.

## Ambiente

Come `F1-03-qemu-backend-reale.md` (Windows 11 Pro 10.0.26200, i9-14900K con 32 processori logici, Rust 1.99.0 MSVC, QEMU 11.1.0 con WHPX). Catalogo della prova da riga di comando: `C:\lvmtest\catalogo` (percorso corto).

## Procedura

```text
cargo test --workspace --locked
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_limiti_job
set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture e2e_real_qemu_impostazioni
lightvm-desktop --impostazioni C:\lvmtest\imp.json --apri-vm prova-cli --crea --cpu-max 25 --memoria-margine 768 --ram 1024 --smoke 8 --static
lightvm-desktop --impostazioni C:\lvmtest\imp.json --apri-vm prova-cli --smoke 8 --static
```

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Validazione dei limiti nel core (intervalli, budget ≤ disco massimo) | verificato (sintetico) | `scenarios::update_vm_config_rejections_have_no_effect` |
| Modifica di una VM spenta: revisione +1, audit `vm_configure succeeded` | verificato (sintetico) | `scenarios::update_vm_config_changes_resources_and_limits_of_a_stopped_vm` |
| Rifiuti senza effetto: revisione superata, disco ridotto, limiti fuori intervallo, risorse oltre i limiti, sessione agente, VM accesa | verificato (sintetico) | stesso test; audit `vm_configure denied` |
| Via IPC con SQLite: persistenza dopo riavvio, conflitto di revisione, pairing → `permission_denied` | verificato | `core_path::update_vm_via_ipc_is_persisted_and_audited` |
| Calcolo dei limiti del job | verificato (unitario) | `adapters::backend::tests::limiti_del_job_dai_limiti_della_vm` (2 vCPU al 50 % su 32 processori → 312; minimi e massimi) |
| Limiti applicati e riletti dal Job Object | verificato su hardware | `reale_limiti_job_cpu_e_memoria`: `JobLimitReport { cpu_rate: Some(300), job_memory_bytes: Some(3221225472), peak_job_memory_bytes: 2268483584 }`, VM in esecuzione; test unitario `job_con_limiti_cpu_e_memoria` |
| Memoria del job inferiore alla RAM del guest | verificato su hardware | 512 MiB per un guest da 2048 MiB: avvio rifiutato dopo 148 ms, `QEMU è terminato durante l'avvio (codice 0x1): cannot set up guest memory 'pc.ram'` |
| Attraverso il servizio: disco 8 → 10 GiB, tetto CPU 25 %, margine 768 MiB | verificato su hardware | `e2e_real_qemu_impostazioni_vm`: `disco della VM ingrandito a 10 GiB`, `qemu-img info` 10737418240 byte, VM `running` con un processo QEMU |
| Budget di storage superato | verificato su hardware | budget 1 GiB, file di 2 GiB in `vms/<id>/overlays/`: avvio `failed` con `resource_limit`, nessun processo QEMU |
| Pannello Impostazioni VM: nessun campo di testo, salvataggio solo con modifiche a VM spenta | verificato (headless) | `impostazioni_vm_ui.rs` (3 test) |
| Comandi della UI su un supervisor reale senza QEMU: modifica, `--apri-vm` con modifiche, VM inesistente, `--crea` | verificato | `servizio_figlio::impostazioni_vm_e_apri_vm_senza_qemu` |
| `lightvm-desktop --apri-vm prova-cli --crea --cpu-max 25 …` con QEMU reale | verificato su hardware | `SMOKE servizio … vm=1 vm_accese=1 ultimo_esito="avvio: riuscito"` |
| Chiusura normale della UI con una VM accesa | **fallito, poi verificato dopo correzione** | prima: `processo genitore terminato` e Emergency Stop (l'EOF che segue la riga `shutdown` era trattato come crash); dopo la prima correzione: arresto ordinato ma 37 s e timeout del supervisor (ACPI 60 s contro 30 s); dopo la seconda: arresto ordinato limitato (ACPI 15 s → `quit` 5 s → kill 5 s), `arresto completato` senza timeout |
| VM rimasta in `recovering` dopo un supervisor terminato al timeout | **fallito, poi verificato dopo correzione** | prima: `stati=prova-cli:recovering`, nessun avvio possibile; dopo: `1 VM riportate a stopped dopo il riavvio`, `stati=prova-cli:running`; test `observations::reconcile_after_restart_returns_recovering_vms_to_stopped` |
| Workspace | verificato | `cargo test --workspace --locked`: 353 passati, 0 falliti, 12 ignorati; clippy `-D warnings` pulito (workspace, varianti UI, servizio senza `supervisore`) |
| Effetto del tetto CPU sotto carico del guest | non eseguito | guest nel firmware; spike `F0-08` |
| Limiti da MCP | non eseguito | F5 |
| Linux | non eseguito | cgroup v2, R2a |

## Limiti

- Un solo host; un campione per ciascuna prova reale.
- Il budget di storage conta i file sotto `vms/<vm_id>/` (overlay, baseline, firmware); log e screenshot per esecuzione (`runs/`) non sono ancora attribuiti alla VM: contabilità completa in F3.
- L'uscita di QEMU al limite di memoria a esecuzione avviata è riconosciuta dal picco del job e registrata solo come avviso del supervisor; non diventa ancora un codice distinto nell'osservazione del core.
- La riconciliazione al riavvio presuppone che nessun processo di un supervisor precedente sia vivo: vero su Windows (Job Object con `KILL_ON_JOB_CLOSE`), da verificare su Linux (R2a).
- La riga `runs` dell'esecuzione interrotta resta aperta nel catalogo dopo la riconciliazione al riavvio.
- `--crea` crea una VM senza supporto di installazione: l'importazione delle ISO nel registro media arriva con la creazione guidata (F2).

## Azioni richieste

1. Misurare l'effetto del tetto CPU con un guest che carica la CPU (dopo l'installazione su disco, F1-17).
2. F3: contabilità completa dello storage per VM e prove sotto pressione.
3. F5: strumento MCP per i limiti, entro i limiti impostati dall'utente.
4. Chiudere la riga `runs` interrotta durante la riconciliazione al riavvio.

## Decisioni proposte

Registrate in ADR-0017 (sezione del 2026-10-10).
