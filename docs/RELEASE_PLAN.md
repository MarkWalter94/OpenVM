# Piano di rilascio e backlog

Documento operativo previsto dalla sezione 17 di `docs/ROADMAP.md`: backlog, responsabili e ordine di esecuzione. **Non amplia né riduce il perimetro congelato.** Nessuna data e nessuna durata: l'avanzamento dipende dai criteri di uscita, non dal calendario.

Fasi sequenziali (sezione 10): F1 parte solo dopo il criterio di uscita di F0. Milestone v0.1 al termine di F2, MVP v0.2 al termine di F6 (sezioni 4 e 10).

## Backlog F0

Legenda tipo: **B** = bloccante per l'uscita da F0; **NB** = avviata in F0, completabile entro F1 senza violare i criteri di uscita (sezione 10, F0).

| ID | Attività | Tipo | Passo (sez. 15) | Responsabile | Evidenza / output | ADR | Esito |
| --- | --- | --- | --- | --- | --- | --- | --- |
| F0-B01 | Verifica toolchain Rust; build minima reale di eframe su Windows | B | 1 | da assegnare | `F0-01-toolchain-eframe.md` | 0001, 0014 | completato con limiti: `F0-01` (ambiente MSVC esplicito) |
| F0-B02 | Build minima di eframe su Linux; backend di rendering; comportamento senza accelerazione grafica (RDP) | B | 1 | da assegnare | `F0-01-toolchain-eframe.md` o file successivo | 0014 | in parte: build Linux in WSL2 (`F0-07`), WARP verificato (`F0-01`); RDP non eseguito (limite L-1) |
| F0-B03 | Congelare scenario, minacce, target, dipendenze: ADR iniziali, matrice iniziale | B | 1 | da assegnare | `docs/adr/`, `docs/COMPATIBILITY.md` | tutti | completato |
| F0-B04 | Procurare e verificare la ISO Ubuntu LTS (checksum, firma); fissare firmware | B | 1–2 | da assegnare | `F0-03-guest-media.md` | 0011 | completato: `F0-03`, firmware in ADR-0011 |
| F0-B05 | Installare QEMU con provenienza verificata; fissare binario e versione | B | 2 | da assegnare | `F0-02-qemu-whpx-qmp.md` | 0010 | completato con deviazione D-4 (firma scaduta): `F0-02` |
| F0-B06 | Avvio del guest su Windows/WHPX con configurazione documentata; probe senza credenziali utente nel guest | B | 2 | da assegnare | `F0-02-qemu-whpx-qmp.md` | 0007, 0010 | completato con deviazione D-1 (`-cpu qemu64`): `F0-02` §9 |
| F0-B07 | QMP su canale privato; arresto ordinario e forzato | B | 2 | da assegnare | `F0-02-qemu-whpx-qmp.md` | 0012 | completato: `F0-02` §4, §7; arresto ACPI non concluso dal live (D-7) |
| F0-B08 | Console nativa con client RFB sul canale protetto, oppure ripiego screendump; input; screenshot | B | 3 | da assegnare | `F0-04-console-rfb-input.md` | 0003, 0013 | completato con pendenti P-4, P-5: `F0-04` |
| F0-B09 | Ciclo baseline immutabile / overlay / ripristino con firmware persistente; prove di interruzione | B | 4 | da assegnare | `F0-05-baseline-overlay-firmware.md` | 0005, 0011 | completato: `F0-05` |
| F0-B10 | Modello di processo del supervisor: QEMU figlio, terminazione OS dei figli, crash UI, crash supervisor | B | 4 | da assegnare | `F0-06-supervisore-processi.md` | 0002, 0009, 0015 | in parte: `F0-06` completo; crash della UI non eseguito (P-2) |
| F0-B11 | Avvio su Linux/KVM se il laboratorio è disponibile, altrimenti registrazione "non eseguito" | B | 5 | da assegnare | `F0-07-linux-kvm.md` | 0016 | laboratorio assente: `F0-07`; esito proposto "verificato solo Windows" (P-1) |
| F0-B12 | Limiti OS per CPU, memoria, storage; prima misura dei consumi | B | 5 | da assegnare | `F0-08-limiti-risorse-misure.md` | 0017, 0018 | completato con limiti: `F0-08` |
| F0-B13 | Threat model, restrizioni MVP, matrice di supporto iniziale, definizione della fixture | B | 1, 6 | da assegnare | `docs/SECURITY.md`, `docs/COMPATIBILITY.md`, ADR-0019 | 0004, 0019 | completato: fixture in ADR-0019 |
| F0-B14 | Prima versione di `ARCHITECTURE.md` e `SECURITY.md`; ADR delle decisioni critiche | B | 1, 6 | da assegnare | documenti in `docs/` | tutti | completato |
| F0-B15 | Verifica della disponibilità di firma e revisione indipendente | B (decisione da chiudere in F0, sez. 17) | 1 | da assegnare | `docs/STATUS.md` | 0020 | aperto: P-6 |
| F0-B16 | Riesame delle prove; decisione di prosecuzione; backlog F1 | B | 6 | da assegnare | `F0-09-riesame-prove.md`, `docs/STATUS.md`, questo file | — | completato: `F0-09` (proposta in attesa di conferma) |
| F0-N01 | Selezione e prova dell'SDK MCP e dei client di riferimento | NB | — | da assegnare | file di verifica dedicato | 0021 | in parte: SDK scelto e provato (`F1-06`); client MCP reali rinviati a F5 |
| F0-N02 | Inventario packaging e licenze delle dipendenze | NB | — | da assegnare | file di verifica dedicato | 0023 | completato per inventario e `cargo deny` (`F1-07`); SBOM aperto fino a F6 |
| F0-N03 | `COMPATIBILITY.md` e `TESTING.md` completi; scelta del framework di test UI | NB | — | da assegnare | documenti in `docs/`; file di verifica | 0022 | in parte: egui_kittest provato (`F0-01`, `F0-07`); `COMPATIBILITY.md` e `TESTING.md` completati nel commit `aa04ccd`; istantanee senza evidenza |
| F0-N04 | Realizzazione della fixture nel guest (entro F3, sezione 9) | NB | — | da assegnare | file di verifica in F1–F3 | 0019 | aperto (entro F3) |

## Ordine di esecuzione

1. F0-B03, F0-B13 (bozza), F0-B14 (bozza), F0-B15: congelamento documentale del passo 1.
2. F0-B01, F0-B04, F0-B05 in parallelo: non dipendono l'uno dall'altro.
3. F0-B06, poi F0-B07: richiedono B04 e B05.
4. F0-B08: richiede B07 (QMP per input e screendump) e B01 (UI minima).
5. F0-B09 e F0-B10: richiedono B06 e B07; B10 fornisce il percorso di terminazione usato dalle prove di interruzione di B09.
6. F0-B02, F0-B11, F0-B12: B11 dipende dalla disponibilità dell'host Linux; B12 richiede B10 (albero processi sotto supervisione).
7. F0-B16: richiede tutte le precedenti attività bloccanti con esito registrato (verificato, fallito o non eseguito con motivo).

Le attività NB possono procedere in qualunque momento senza bloccare il passo 6, purché non anticipino una costruzione estesa della dashboard (sezione 15).

## Criteri di uscita richiamati

- F0: criterio di uscita, decisione di arresto o cambio target, regola "laboratorio Linux assente" (sezione 10, F0).
- F1–F6: criteri di uscita della sezione 10 per ciascuna fase.
- MVP pubblico: Definition of Done (sezione 11).

Una dichiarazione di completamento senza un criterio di uscita obbligatorio non è accettata (sezione 16, punto 8).

## Chiusura di F0

F0 si chiude quando i pendenti P-1, P-2 e P-4…P-7 di `docs/verification/F0-09-riesame-prove.md` §3.1 hanno un esito registrato. F1 parte dopo il criterio di uscita di F0 (sezione 10); le attività F1 già avviate non valgono per il criterio di uscita F1 prima di quella chiusura.

## Backlog F1

Derivato dalla sezione 10 F1 e dalle azioni richieste nei file F0 (riesame in `F0-09`). Tutti i responsabili sono da assegnare. Le attività non ampliano il perimetro: ciascuna traduce un'attività F1 della roadmap o un punto aperto emerso dalle prove.

| ID | Attività | Origine | Evidenza attesa | ADR |
| --- | --- | --- | --- | --- |
| F1-01 | Workspace, build riproducibile con `--locked`, lint, migrazioni SQLite, tipi IPC/MCP e JSON Schema generati da fonte unica con errori pubblici stabili | sez. 10 F1; sez. 6.3 | test e build in CI | 0002 |
| F1-02 | CI Windows/Linux senza segreti: lint, build e test per glow e wgpu, test headless egui_kittest, processi Windows (Job Object) e Linux (pidfd, gruppi, cgroup dove delegabile), rendering software (WARP, llvmpipe con Wayland e X11), acceleratore assente; ambiente MSVC esplicito sul runner Windows; build macOS entro F2 con rischio in `STATUS.md` se il runner manca | sez. 10 F1; `F0-07` "Proposta: CI minima per F1"; `F0-01` §4.1 | log CI; `docs/TESTING.md` | 0014, 0022 |
| F1-03 | Rilevamento capacità e diagnostica con errori utilizzabili: probe WHPX con `query-accelerators` (non con lo stato CIM della feature `HypervisorPlatform`), errore chiaro senza acceleratore e senza ripiego TCG, verifica degli hash dei binari QEMU approvati | sez. 10 F1; D-6; ADR-0007, 0010 | file F1 di verifica; job `acceleratore-assente` | 0007, 0010 |
| F1-04 | Modello VM, macchina a stati, validazione risorse, registro media approvato con copia gestita in sola lettura, hash e apertura protetta contro reparse point (`CREATE_NEW`) | sez. 10 F1; sez. 6.3 | test di proprietà; test negativi Host e File | 0005, 0008 |
| F1-05 | Gestione processi Windows in `qemu-backend`/`lightvm-service`: `CreateProcessW` con `JOB_LIST` e `HANDLE_LIST`, `DETACHED_PROCESS`, ambiente ridotto a `SystemRoot`, identità handle + tempo di creazione + `run_id`, classificazione dei codici di uscita (incluse le notifiche di limite del job), scalata `system_powerdown` → `quit` → `TerminateJobObject` con timeout configurabile | `F0-06` Azioni richieste e Decisioni proposte | 20 cicli reali (F1-12) | 0002, 0009 |
| F1-06 | **Unico proprietario QMP**: client QMP asincrono, uno per esecuzione, collegato subito dopo lo spawn, su socket Unix in directory privata per esecuzione con ACL solo-utente; multiplexing di comandi, input, screenshot ed eventi; timeout; rimozione del socket orfano; riconciliazione dello stato da processo e QMP | sez. 10 F1; D-9; `F0-04` Azioni 3 | test con QMP simulato (dichiarato) e con QEMU reale | 0012 |
| F1-07 | API interne di input e cattura: `screendump` in directory privata con file deciso dal backend, validazione PPM e limiti di dimensione; input tipizzato con tabella scancode → qcode per i layout dichiarati (US, IT) e rilascio dei tasti | sez. 10 F1; `F0-04` §4 | test unitari; prova reale | 0003 |
| F1-08 | Profilo offline e comandi QEMU da opzioni controllate: `-nodefaults -nic none` con verifica post-avvio via `info network` e `query-pci` (non `query-rx-filter`), `-blockdev` con formato esplicito, protocolli in allowlist (esclusi i driver remoti della build: `http`, `https`, `ftp`, `ftps`, `nfs`, `ssh`, `nbd`), rifiuto di percorsi con virgole o non assoluti | sez. 10 F1; `F0-02` §3, §5; `F0-05` Procedura | criterio "nessuna NIC inattesa" | 0004, 0010 |
| F1-09 | **Share mode sui dischi**: apertura della baseline e del suo vars con `FILE_SHARE_READ`, rilevatore di scrittori, handle dell'overlay senza `FILE_SHARE_DELETE`, verifica dell'hash della baseline prima di ogni avvio; prova di directory per esecuzione con ACL esplicita e di identità di processo distinta per QEMU per l'overlay in uso | `F0-05` §7, Azioni 1; D-2, D-3 | file F1 di verifica | 0025 |
| F1-10 | Audit minimo di creazione, avvio, arresto e dinieghi | sez. 10 F1 | log di audit nei test | — |
| F1-11 | Skeleton del supervisor e test headless senza UI; IPC locale autenticato con named pipe con ACL; UI che avvia il supervisor come processo separato (presupposto di P-2) | sez. 10 F1; ADR-0002 | test di integrazione headless | 0002 |
| F1-12 | Criterio di uscita F1: creazione disco, almeno 20 cicli start/stop reali senza processi orfani, errore chiaro con acceleratore assente, nessuna NIC inattesa, rifiuto di percorsi e risorse non validi | sez. 10 F1 | file F1 di verifica su runner fisico | 0002, 0004, 0007 |
| F1-13 | **Blocco della lettura degli appunti host**: egui-winit legge gli appunti su Ctrl+V anche se la UI scarta il testo; configurare o sostituire quel percorso prima di F2 | D-13; `F0-04` Azioni 5 | test UI e prova manuale | 0003 |
| F1-14 | **Packaging Linux e panico X11**: dichiarare le dipendenze caricate con `dlopen` (almeno `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`, `libEGL`/`libGL` o `libvulkan1`); trasformare il panico di `xkbcommon-dl` in un errore utilizzabile | D-17; `F0-07` Azioni 5 | job CI `smoke-render-software` | 0014 |
| F1-15 | **Delega cgroup**: creazione dello scope delegato senza `systemd-run` esterno (D-Bus `StartTransientUnit`) e comportamento senza sessione systemd utente, in ambiente non accelerato; preparazione delle prove R2a | `F0-07` Azioni 7; ADR-0015 parte A | job CI `processi-linux` | 0015, 0017 |
| F1-16 | Selezione automatica del backend (glow → wgpu DX12 → WARP) con indicazione visibile; prova in sessione RDP | ADR-0014; limite L-1 | file F1/F2 di verifica | 0014 |
| F1-17 | Modello CPU con più estensioni (`Skylake-Client-v4` o `Haswell-v4`) fino al desktop; installazione di Ubuntu su disco e boot dal disco | D-1; ADR-0011 criterio 2 | file F1 di verifica | 0011 |
| F1-18 | Prime misure con processi di prodotto: memoria UI + supervisor in private bytes (working set a parte), CPU manager in idle su 5 minuti, 20 avvii UI a freddo e 20 a caldo | sez. 13; limite L-3; `F0-09` §5 | riga in `docs/TESTING.md` | 0014, 0018 |
| F1-19 | Chiusura delle attività F0 non bloccanti: SDK e client MCP (ADR-0021), inventario packaging e licenze incluse le dipendenze di `rfb-client` (ADR-0023), istantanee egui_kittest (ADR-0022), `COMPATIBILITY.md` e `TESTING.md` completi | sez. 10 F0 e F1 | file di verifica dedicati | 0021, 0022, 0023 |

### Avanzamento F1

Aggiornato il 2026-10-10 dalla lettura dei file di verifica e del codice al commit `8c1f26e`. "Codice presente" non vale come verifica (sezione 16, punto 8): senza un file in `docs/verification/` la voce resta `non eseguito`. Nessuna voce vale per il criterio di uscita F1 finché F0 non è chiusa (sezione 10).

| ID | Stato | Evidenza o motivo |
| --- | --- | --- |
| F1-01 | in parte | tipi, schemi, errori e migrazioni verificati con test sintetici (`F1-01`, `F1-02`); build e test in CI su GitHub non eseguiti |
| F1-02 | in parte | workflow completo validato in locale (`F1-08`); mai eseguito su GitHub |
| F1-03 | codice presente, evidenza assente | `qemu-backend/src/probe.rs`, test `real_qemu::reale_probe_hash_e_acceleratore_assente` (`#[ignore]`); file `F1-03` non scritto |
| F1-04 | in parte | modello e registro verificati con test sintetici (`F1-01`, `F1-02`); identità OS del file e apertura protetta contro reparse point non collegate nel servizio (`TODO(F1)` in `lightvm-service/src/adapters/catalog.rs`) |
| F1-05 | codice presente, evidenza assente | `qemu-backend/src/process/`, `sys/windows.rs`, `run.rs`; end-to-end reale con 2 esecuzioni in `F1-04` (sezione in coda); perdita del genitore con QEMU reale non eseguita |
| F1-06 | in parte | client QMP in `qemu-backend/src/qmp/` con test simulati (`tests/qmp_mock.rs`), evidenza non registrata; **la UI apre ancora direttamente il socket QMP** (`apps/desktop/src/console.rs`, `qmp_worker`): va portata sull'IPC del supervisor per rispettare l'unico proprietario QMP (ADR-0012) |
| F1-07 | codice presente, evidenza assente | `qemu-backend/src/input.rs`, `screenshot.rs` |
| F1-08 | codice presente, evidenza assente | `qemu-backend/src/cmdline.rs`, `run::check_no_nic`, test `real_qemu::reale_rilevatore_nic` (`#[ignore]`) |
| F1-09 | in parte | parte storage verificata con test sintetici (`F1-02`); `qemu-backend/src/storage_guard.rs` senza evidenza; overlay in uso scrivibile da un altro processo dello stesso utente (decisione aperta) |
| F1-10 | verificato con test | audit di creazione, avvio, arresto, Emergency Stop e dinieghi (`F1-01`; `F1-04`, `core_path`) |
| F1-11 | in parte | supervisor headless e IPC con named pipe e ACL verificati (`F1-04`); **la UI non avvia il supervisor e non usa l'IPC**: presupposto di P-2 non realizzato |
| F1-12 | non eseguito | test `real_qemu::reale_cicli_start_stop` scritto (`LIGHTVM_F1_CYCLES`, predefinito 20) ma senza esecuzione registrata; creazione e registrazione del disco principale mancanti nel servizio (`TODO(F1-12)` in `lightvm-service/src/adapters/backend.rs`) |
| F1-13 | codice presente, evidenza assente | `apps/desktop/src/appunti.rs`, `tests/appunti_ui.rs`; prova manuale non eseguita |
| F1-14 | in parte | dipendenze Linux elencate (`F1-07`); job `smoke-render-software` non eseguito su GitHub; verifica a runtime non eseguita |
| F1-15 | in parte | probe D-Bus `StartTransientUnit` verificato in WSL2 (`F1-08`); non implementato in `qemu-backend` (`TODO(F1/R2a)` in `process/mod.rs`) |
| F1-16 | codice presente, evidenza assente | `apps/desktop/src/render.rs`; prova in RDP non eseguita |
| F1-17 | non avviato | `Skylake-Client-v4` accettato dalla validazione, nessuna prova di boot al desktop né installazione su disco |
| F1-18 | non eseguito | nessuna misura con processi di prodotto |
| F1-19 | in parte | SDK MCP (`F1-06`, ADR-0021 accettata per l'SDK), licenze (`F1-07`, ADR-0023 accettata in parte); istantanee egui_kittest presenti in `apps/desktop/tests/snapshots/` senza evidenza (ADR-0022 Proposta) |

Stato del workspace: secondo il commit `aa04ccd`, `cargo check --workspace --all-targets` passa; i test completi del workspace non sono stati rieseguiti dopo le ultime modifiche. Sull'host di sviluppo attuale (2026-10-10) Rust e QEMU non sono installati: nessuna build né test eseguiti.

### Punti aperti da F0 per le fasi successive

| Fase | Attività | Origine |
| --- | --- | --- |
| F2 | Credenziale del display per esecuzione e VM, distinta da QMP e IPC, consegnata via IPC e revocata | ADR-0013 |
| F2 | VeNCrypt: prova con `connections=` e timeout del supervisor; arresto della sessione TLS | D-5 |
| F2 | DesktopSize, Hextile, CopyRect e Raw inviati da un QEMU reale; riduzione dei frame doppi per aggiornamento; preferenza `HighPerformance`/`LowPower` | `F0-04` Azioni 6; ADR-0014 |
| F2 | AltGr con layout IT e screen reader reale con AccessKit | `F0-04` Limiti; ADR-0001 |
| F3 | Journal e riconciliazione (`F0-05` §9), file `.partial` con rinomina atomica e confronto, 30 cicli con installazione reale | `F0-05` Azioni 2; ADR-0005, 0025 |
| F3 | Soglie storage, riserva, limiti di screenshot e log, prove sotto pressione; misura dell'effetto del tasso di I/O | ADR-0017; `F0-08` |
| F3 | Realizzazione della fixture e calibrazione | ADR-0019 |
| F4 | Revoca ed Emergency Stop con flood di input concorrente, 100 campioni anche con guest a desktop | `F0-06` Limiti; sez. 13; D-18 |
| F6 | Verifica di provenienza aggiuntiva della build QEMU (firma scaduta) | ADR-0010 |
| R2a | Tutte le prove Linux su host fisico elencate in ADR-0016 | ADR-0016 |

## Decisioni aperte

ADR in stato Proposta (dettagli in `docs/adr/`):

| ADR | Decisione | Da chiudere entro | Motivo per cui resta aperta |
| --- | --- | --- | --- |
| 0015 | Terminazione dei figli su Linux e sorte delle VM manuali senza UI | F0 | parte B in attesa del crash della UI su Windows (P-2) |
| 0017 | Limiti OS e budget storage | F0 | meccanismi identificati; soglie, riserva e prove sotto pressione mancanti; Linux su host fisico in R2a |
| 0020 | Responsabilità di firma e revisione indipendente | F0 | dichiarazione dell'utente (P-6) |
| 0022 | Framework di test UI | F1 | istantanee scritte, evidenza non registrata |
| 0026 | Codici pubblici per autenticazione e autorizzazione | F1 | `unauthenticated` adottato nel codice (`F1-01`, `F1-04`), decisione non ancora accettata |

Accettate in parte: ADR-0021 (SDK `rmcp` 3.5.1 accettato; client MCP aperti fino a F5) e ADR-0023 (politica e controlli accettati; SBOM e font di egui aperti fino a F6).

ADR-0016 è Accettata ma diventa efficace solo con la conferma dell'utente (P-1). Dettagli operativi del supporto dati approvato (sezione 17): da registrare in ADR-0008 (P-7).
