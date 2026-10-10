# F1-03 — `qemu-backend` con QEMU reale: probe, rifiuti, NIC, 20 cicli start/stop e disco del servizio

- Data di esecuzione (UTC): 2026-10-10, dalle 09:00 alle 09:15 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 10 F1 (criterio di uscita: "creazione disco, almeno 20 cicli start/stop reali senza processi orfani, errore chiaro con acceleratore assente, nessuna NIC inattesa, rifiuto di percorsi e risorse non validi"); backlog `docs/RELEASE_PLAN.md` F1-03, F1-05, F1-06, F1-07, F1-08, F1-12
- ADR collegati: ADR-0002, ADR-0004, ADR-0007, ADR-0009, ADR-0010, ADR-0011, ADR-0012 (non modificati)
- Esito complessivo: **parziale** — le quattro prove reali di `qemu-backend` e la prova end-to-end del servizio con disco principale sono **verificate su hardware** su un solo host Windows; le VM restano nel firmware UEFI (nessun sistema operativo avviato); **non valgono come criterio di uscita F1 finché F0 non è chiusa** (sezione 10)

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `f1-03-real-all.txt`, `f1-03-probe-run.txt`, `f1-04-e2e-disco.txt`, `f1-03/s1791623264863-cicli.log`, `f1-03/s1791623264863-cicli.csv`, `f1-03/s1791623264863-screendump.png`, `f1-03/s1791623326125-probe.log`, `f1-03/s1791623327921-rifiuti.log`, `f1-03/s1791623329287-nic.log`, `qemu-extract.txt`.

## Scopo

Dimostrare con QEMU reale e WHPX, attraverso le API di `qemu-backend` usate dal supervisor:

1. probe dei binari approvati (hash), rifiuto di un hash non approvato, errore chiaro con acceleratore assente senza ripiego TCG;
2. rifiuto di percorsi, media, risorse, modelli CPU, macchine e protocolli non validi prima di creare qualunque processo;
3. rilevamento di una NIC inattesa con `info network` e `query-pci`;
4. almeno 20 cicli start/stop reali con disco creato da `qemu-img`, overlay per esecuzione, Job Object, QMP privato, nessun processo orfano, nessuna NIC, stato riconciliato; screendump e input reali sul primo ciclo; Emergency Stop sull'ultimo;
5. creazione del disco principale da parte del servizio (`lightvm-service`) al primo avvio di una VM, chiusura del `TODO(F1-12)`.

Non dimostra: boot di un sistema operativo (la VM resta nel firmware o nel boot loader della ISO), arresto ACPI riuscito, installazione su disco (F1-17), Linux, prestazioni di release, comportamento sotto carico, secondo utente.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | Intel Core i9-14900K (32 processori logici), 64 GB RAM (65 303 MiB rilevati), SSD di sistema con circa 62 GiB liberi |
| OS e build | Microsoft Windows 11 Pro 10.0.26200 |
| Acceleratore e stato | WHPX: `probe_accelerator whpx: Available { machine: "pc-q35-11.1", cpu: "qemu64" }` |
| Toolchain | Rust 1.99.0 `x86_64-pc-windows-msvc` (rustup 1.29.1, installato in questa sessione; `rustup-init.exe` con SHA-256 `6f4bef66…bdb7e` uguale al file `.sha256` pubblicato); MSVC 14.44.35207 di Visual Studio Community 2022 con il componente `Microsoft.VisualStudio.Component.VC.Tools.x86.x64` aggiunto in questa sessione (vedi Limiti) |
| Binario QEMU | 11.1.0 (`v11.1.0-12130-ge470268ff4`) estratto in modo portabile con 7-Zip da `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe`: SHA-512 dell'installer uguale al file `.sha512` pubblicato e a F0-02 (`5bcf9eed…16bf543`), SHA-256 `f98a8aeb…e2a59f` uguale a F0-02; `qemu-system-x86_64.exe` SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd`, `qemu-img.exe` SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` (uguali a F0-02); 3388 file, 1 256 078 336 byte |
| Firmware | `share/edk2-x86_64-code.fd` SHA-256 `33090cc0…6dbb2a`, `share/edk2-i386-vars.fd` SHA-256 `5d2ac383…be5d1e` (uguali a F0-02) |
| Guest / media | `ubuntu-26.04.1-desktop-amd64.iso` scaricata da `https://releases.ubuntu.com/26.04.1/`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` (`sha256sum -c`: OK; firma GPG del file dei checksum non riverificata in questa sessione, vedi `F0-03`) |
| Sessione grafica | locale (le prove non aprono finestre) |
| Codice | ramo `f1-consolidamento` a partire da `3958b2e`, con le modifiche di questa sessione al servizio (disco principale, spazio libero misurato) |
| Altre condizioni | host non dedicato: i tempi non sono misure di release (sezione 13) |

## Procedura

Ambiente MSVC caricato con `vcvars64.bat` di Visual Studio 2022 Community in un `cmd` (wrapper nella scratchpad della sessione, non nel repository).

```text
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture --test-threads=1

set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture
```

`LIGHTVM_F1_CYCLES` non impostata: 20 cicli (valore predefinito). La configurazione di ogni ciclo è quella del test `reale_cicli_start_stop` (`crates/qemu-backend/tests/real_qemu.rs`): `pc-q35-11.1`, WHPX, `qemu64`, 2 vCPU, 2048 MiB, overlay qcow2 per esecuzione su una base da 1024 MiB creata con `prepare::create_disk`, ISO come supporto di installazione, copia per esecuzione delle variabili UEFI.

## Risultati

### Prove di `qemu-backend` (4 test, 64,6 s, `test result: ok. 4 passed`)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Creazione del disco con `prepare::create_disk` e rifiuto di sovrascrivere un disco esistente | verificato | `disco qcow2 da 1024 MiB creato … in 34.0905ms (197120 byte)`; seconda creazione → errore (asserzione del test) |
| Probe dei binari approvati e dell'acceleratore | verificato | `QEMU 11.1.0 … sha256 47d57a60…; acceleratori ["tcg", "whpx"]; macchina consigliata pc-q35-11.1`; `probe_accelerator whpx: Available` in 150,7 ms |
| Hash non approvato rifiutato | verificato | `unsupported / il binario qemu-system-x86_64 non corrisponde a una build QEMU approvata … avvio rifiutato` |
| Acceleratore assente: errore chiaro, nessun ripiego TCG | verificato | `-accel kvm` sul binario Windows → `unsupported / acceleratore hardware non disponibile: KVM non è incluso nel binario QEMU. Avvio rifiutato: nessun ripiego sull'emulazione TCG …`; acceleratore diverso dal probe → stesso codice con indicazioni su HypervisorPlatform |
| Rifiuto di percorsi e risorse non validi | verificato | 18 rifiuti, `nessun processo QEMU creato`: virgola, relativo, UNC, dispositivo, `..`, protocollo, media con virgola, media non registrato → `invalid_argument`; 0 e 65 vCPU, 64 MiB e RAM eccessiva → `resource_limit`; `-cpu host` e modello assente → `unsupported`; CPU con flag e alias `q35` → `invalid_argument`; `nbd`, `https` → `invalid_argument` (profilo offline) |
| Rilevatore di NIC inattese | verificato | VM avviata apposta con `virtio-net-pci`: `info network` e `query-pci` (classe rete) la riportano, `check_no_nic` → `infrastructure_error` |
| 20 cicli start/stop reali | verificato | vedi sotto |

### Cicli start/stop (CSV `s1791623264863-cicli.csv`, SHA-256 `cc7f88b8…885fb8`)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Avvio con WHPX, un solo processo nel Job Object, stato `running` riconciliato | verificato (20/20) | `run.accelerator().enabled == "whpx"`, `active_processes() == 1`, `reconcile` → `Running` |
| Nessuna NIC | verificato (20/20) | `verify_no_nic` riuscito a ogni ciclo (colonna `nic_ok = true`) |
| Socket QMP orfano rimosso dal backend | verificato | file `qmp.sock` finto creato prima del ciclo 1, avvio riuscito |
| Screendump reale in directory privata, PNG, nessun `.ppm` residuo | verificato | `screendump dopo 1.09s: 1280x800, PNG 40681 byte, sha256 1f9aced2…2598, non nero 1.79%`: OVMF (TianoCore) con `BdsDxe: starting Boot0001 "UEFI QEMU DVD-ROM"` |
| Input reale e rilascio dei tasti; rifiuto di geometria obsoleta, `run_id` superato, tasto `power`; limite di dimensione dello screenshot | verificato | `input: movimento + shift premuto, rilasciati 1 tasti`; asserzioni negative del test; `screenshot_limited(16)` → `resource_limit` |
| Arresto con solo `quit` (cicli dispari 1–19) | verificato (10/10) | fase `Quit`, codice 0, classe `Clean`, causa `QmpQuit`; durata min 91,6 ms, mediana 102,1 ms, max 322,2 ms |
| Arresto con scalata `system_powerdown` (2 s) → `quit` (cicli pari 2–18) | verificato (9/9) | il firmware non risponde all'ACPI (nota `nessuna uscita dopo system_powerdown`, come D-7 in F0), poi `quit` riuscito; durata min 2098,8 ms, mediana 2106,3 ms, max 2110,8 ms |
| Emergency Stop (ciclo 20) | verificato | fase `Kill`, codice 58853, classe `EmergencyStop`, durata 41,3 ms |
| Dopo ogni arresto: 0 processi nel job, Job Object chiuso, handle rilasciato, socket QMP rimosso, stato `stopped` | verificato (20/20) | colonne `attivi_dopo = 0`, `job_chiuso = true`, `handle_rilasciato = true`, `stato_riconciliato = Stopped`; `qmp_socket_left = false` (asserzione) |
| Nessun processo orfano | verificato | `identita_dopo = NotFound` per tutti i 20 PID (identità PID + tempo di creazione); verifica finale su 20 identità |

Tempi (20 campioni, host non dedicato): overlay min 86,0 / mediana 96,1 / p95 104,6 / max 111,1 ms; avvio fino a QMP pronto min 153,2 / mediana 157,2 / p95 162,9 / max 164,3 ms; chiusura dell'esecuzione mediana 0,2 ms. Nessun errore, nessun timeout, nessun tentativo ripetuto.

### Servizio: disco principale e spazio libero (`core_path::e2e_real_qemu_create_start_status_stop`, 6,65 s)

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Spazio libero del catalogo misurato (non più valore fisso) | verificato | `HostCapacity { … free_storage_gib: 62, reserved_storage_gib: 16, accelerator_available: true … }` |
| Disco principale creato dal servizio al primo avvio (`vms/<vm_id>/primary.qcow2`, disposizione di `vm-storage`) | verificato | `disco principale: …\vms\01a12514-…\primary.qcow2 (197120 byte)` dopo il primo arresto |
| Secondo avvio con lo stesso disco, Emergency Stop | verificato | avvio riuscito; `EmergencyStopReport { processes_terminated: 1, completed: true }` in 22,8 ms |
| IPC → core → SQLite → backend reale → audit | verificato | avvio 180,3 ms, arresto 3,13 s (powerdown 3 s non concluso, poi `quit`), 20 righe di audit con `vm_start`, `vm_stop`, `emergency_stop` `succeeded` |

### Build e test del workspace sullo stesso host

| Voce | Esito | Evidenza |
| --- | --- | --- |
| `cargo build --workspace --locked` | verificato | dopo l'aggiunta del componente MSVC x64 (prima: `LNK1104 msvcrt.lib`, come F0-01) |
| `cargo fmt --all --check` | verificato | nessuna differenza |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`; `lightvm-desktop` con solo `glow` e solo `wgpu`; `lightvm-service --no-default-features` | verificato | nessun avviso |
| `cargo test --workspace --locked --no-fail-fast` | verificato | 328 passati, 0 falliti, 7 ignorati (prove con QEMU reale) in 55 binari di test |

## Limiti

- Un solo host, una sola build QEMU, un solo guest; nessuna estensione ad altri host (sezione 5).
- Le VM non avviano un sistema operativo: restano in OVMF o nel boot loader della ISO. L'arresto ACPI non è mai concluso (scalata a `quit`); un guest reale può comportarsi diversamente.
- Le prove di `qemu-backend` usano direttamente le API del backend; attraverso il servizio è stata eseguita solo la prova end-to-end (2 avvii), non i 20 cicli.
- Il disco principale del servizio è creato fuori dal journal del catalogo: un file preesistente (anche parziale dopo un crash) è usato così com'è. Journal e rinomina atomica sono in F3 (`RELEASE_PLAN.md`, punti aperti F3).
- Il componente MSVC x64 è stato aggiunto all'installazione di Visual Studio 2022 Community dell'utente (con conferma UAC): l'installazione precedente conteneva solo le librerie `onecore`, senza header e senza `vcvarsall.bat`. Rust è installato con `--no-modify-path`: `cargo` non è nel PATH di sistema.
- Firma GPG del file dei checksum Ubuntu non riverificata in questa sessione (verificata in `F0-03` per la stessa ISO e lo stesso hash).
- **Validità per il criterio di uscita:** F0 non è chiusa (P-1, P-4…P-7 in `docs/STATUS.md`); per la sezione 10 queste prove non chiudono F1.

## Azioni richieste

1. Chiudere F0 (azioni utente U-3, U-5, U-7 in `docs/STATUS.md`), poi ripetere questa prova come evidenza del criterio di uscita F1.
2. Ripetere i 20 cicli attraverso il servizio (IPC → core → backend), non solo sulle API del backend.
3. F1-17: modello CPU più ricco e boot fino al desktop e da disco.
4. Portare la creazione del disco principale nel journal del catalogo (F3).

## Decisioni proposte

Nessuna nuova decisione: i risultati confermano ADR-0007 (nessun ripiego TCG), ADR-0009 (Emergency Stop con primitive OS), ADR-0012 (QMP privato) sul nuovo host.

## Aggiornamento del 2026-10-10 (dalle 09:25 alle 09:35 circa UTC): cicli attraverso il servizio e boot con modello CPU nominale

Sezione aggiunta in coda (regola 8 di `docs/verification/README.md`): le sezioni precedenti restano come registrate. Stesso host, toolchain, QEMU, firmware e ISO.

### Procedura

```text
set LIGHTVM_E2E_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-service --test core_path -- --ignored --nocapture e2e_real_qemu_cicli

set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_boot_live
```

- `core_path::e2e_real_qemu_cicli_attraverso_il_servizio` (nuovo): IPC → core → SQLite → `qemu-backend` → QEMU, una VM (1 vCPU, 1024 MiB, disco 8 GiB creato al primo avvio, nessuna ISO) avviata e arrestata 20 volte; cicli dispari `force_stop`, pari `stop` (politica del servizio in prova: `system_powerdown` 1 s → `quit` 10 s → kill 10 s), ciclo 20 Emergency Stop; dopo ogni arresto, processi `qemu-system-x86_64.exe` figli del processo di prova contati con `Win32_Process`. Log: `lab/logs/f1-12-cicli-servizio.txt`.
- `real_qemu::reale_boot_live_cpu_nominale` (nuovo): `-cpu Skylake-Client-v4`, 2 vCPU, 4096 MiB, ISO live, overlay su base da 1024 MiB, uno screendump ogni 10 s per 240 s, poi Emergency Stop. Log: `lab/logs/f1-17-boot-skylake.txt`, `lab/logs/f1-03/s1791624568749-boot-cpu.log` e 24 PNG.

### Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| 20 cicli start/stop **attraverso il servizio**, un processo QEMU per avvio, nessun residuo dopo ogni arresto e dopo l'arresto del servizio | verificato | `ciclo i: … residui 0` per i = 1…20; `test result: ok. 1 passed` in 47,7 s |
| Stato riconciliato e audit | verificato | `running` dopo ogni avvio e `stopped` dopo ogni arresto (via `vm_status`); audit `succeeded`: `vm_start` 20, `vm_stop` 19 (include `force_stop`), `emergency_stop` 1 |
| Disco principale riusato fra i cicli | verificato | `vms/<vm_id>/primary.qcow2` presente a fine prova; creato una sola volta |
| Tempi | registrati | avvio (IPC → `running`) min 139,3 / mediana 154,1 / p95 1181,1 / max 1820,9 ms; `force_stop` (10) mediana 55,9, max 1149,3 ms; `stop` (9) mediana 1122,1, max 3101,7 ms; Emergency Stop 49,6 ms. I cicli 7–11 sono più lenti (fino a 1,8 s di avvio): causa non indagata, host non dedicato; nessun errore né timeout |
| F1-17: boot con `Skylake-Client-v4` fino al desktop | verificato | processo vivo per 245 s; firmware e boot loader fino a t≈80 s, a t=101 s desktop GNOME live con la finestra "Welcome to Ubuntu" (screendump SHA-256 `a1c329f9…0b96`, 99,93 % non nero); deviazione D-1 superata con un modello nominale più ricco di `qemu64` (`-cpu host` resta rifiutato) |
| F1-17: installazione di Ubuntu su disco e boot dal disco | non eseguito | richiede di pilotare l'installer (o un `autoinstall` dal boot loader); non tentato in questa sessione |

### Limiti

- Un solo campione di boot con `Skylake-Client-v4`; `Haswell-v4` non provato.
- I 20 cicli attraverso il servizio usano una politica di arresto ridotta (`system_powerdown` 1 s) impostata dal test; la politica predefinita del servizio (60 s) non è stata usata nei cicli.
- Validità per il criterio di uscita: invariata rispetto alle sezioni precedenti (F0 aperta).
