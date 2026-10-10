# Strategia di test

Ultimo aggiornamento: 2026-10-09 (riesame di F0; lavoro F1 in corso: CI, SDK MCP, licenze)

Traduzione operativa della sezione 13 di `docs/ROADMAP.md`. Le prove di fattibilità F0 (spike) sono registrate in `docs/verification/F0-*` e riassunte in `F0-09-riesame-prove.md`; i primi test dei crate F1 (con backend, catalogo o QMP simulati, dichiarati) sono in `docs/verification/F1-*`. Nessuna capacità di prodotto è verificata su hardware. Le procedure e gli esiti vanno registrati in `docs/verification/` secondo il template di `docs/verification/README.md`. Questo documento non riporta conteggi di test: i numeri stanno nei file di verifica che li hanno misurati.

## 0. Livelli di test

| Livello | Che cosa contiene | Dove si trova | Che cosa non dimostra | Esempi attuali |
| --- | --- | --- | --- | --- |
| Unitari | logica pura, stati, policy, validazione, parser con fixture | moduli `#[cfg(test)]` dei crate | integrazione tra processi, OS, QEMU | `vm-core`, `vm-storage`, `lightvm-proto`, `qemu-backend` (parser `probe`, `cmdline`, `validate`) |
| Integrazione headless | più componenti o processi reali senza UI né acceleratore: IPC del servizio, server MCP su stdio come processo figlio, SQLite su file temporanei | `crates/*/tests/` | QEMU reale, rendering, accelerazione | `lightvm-service/tests/ipc.rs` (`F1-04`), `lightvm-mcp/tests/stdio.rs` (`F1-06`) |
| Test UI strumentati | interfaccia egui pilotata via AccessKit con egui_kittest, supervisor simulato | `apps/desktop/tests/` | IPC reale, rendering nativo, accelerazione | `kittest_ui.rs`; istantanee in corso (§4) |
| Mock dichiarati | QMP, QEMU, backend o catalogo simulati, fixture di stderr/help registrate da QEMU reale | `crates/qemu-backend/tests/qmp_mock.rs`, `tests/fixtures/`; backend e catalogo finti di `lightvm-service` | avvio reale, dispositivi, latenze (§2) | classificazione dell'errore senza acceleratore da fixture (job `acceleratore-assente`) |
| Processi senza acceleratore | Job Object su Windows; pidfd, gruppi, cgroup delegato su Linux, con figli di prova | test dei crate e spike; job `processi-windows`, `processi-linux` | QEMU con KVM/WHPX, limiti sul guest | `F0-06`, `F0-07` §4, `F1-08` §3 (**[processi]** in WSL2) |
| Prove reali | QEMU con provenienza verificata e acceleratore su host fisico; test `#[ignore]` lanciati a mano o da runner fisici | `apps/desktop/tests/console_reale.rs`, `crates/qemu-backend/tests/real_qemu.rs` (ignorati per default), script in `lab/` | combinazioni diverse da quella provata | prove F0 su Windows (`F0-02`…`F0-06`) |

I file di verifica classificano ogni esito come mock/simulazione o hardware (sezione 16, punto 6).

## 1. Pipeline di verifica

| Livello | Quando | Contenuto | Dove gira | Evidenza |
| --- | --- | --- | --- | --- |
| Per modifica | A ogni modifica | Unit test significativi, validazione degli schemi JSON generati, lint, build dei target configurati, licenze e advisory | CI senza segreti, `.github/workflows/ci.yml`: `lint`, `build-test-windows`, `build-test-linux`, `processi-windows`, `processi-linux`, `smoke-render-software`, `acceleratore-assente`, `schema`, `cargo-deny`, `macos-check` (`F1-08`; **non ancora eseguita**: repository non pubblicato) | Log CI |
| Proprietà e input avversari | A ogni modifica sulle aree interessate | Test di proprietà sulle transizioni VM e sul registro storage; input avversari e fuzzing mirato dei parser esposti (IPC, MCP, RFB, manifest) | CI | Log CI, corpus di fuzzing |
| Runner fisici controllati | Su modifiche a backend, console, storage, supervisor; prima di ogni milestone | Probe, boot accelerato, QMP, console, ripristino, test negativi che richiedono una VM reale | Host fisici di laboratorio con acceleratore | File in `docs/verification/` |
| Pre-release | Prima di ogni release | Installazione pulita, upgrade, regressione E2E, test negativi sull'artefatto finale | Host puliti del target | File in `docs/verification/` |
| Per host | Prima di abilitare un host | Suite reale completa su quell'host, non solo cross-compilazione | Host fisico del target | Riga aggiornata in `docs/COMPATIBILITY.md` |

Evitare test che ripetono soltanto l'implementazione (sezione 13).

## 2. Test con mock e test con boot reale

| Tipo | Ammesso per | Non dimostra |
| --- | --- | --- |
| Mock di QEMU/QMP | Errori, timeout, protocollo, riconciliazione, transizioni di stato | Avvio reale, acceleratore, dispositivi, latenze |
| Supervisor simulato nei test UI | Layout, flussi, accessibilità tramite AccessKit | IPC reale, rendering nativo, accelerazione |
| Boot reale su runner fisico | Criteri di uscita delle fasi, test negativi su VM, misure | Combinazioni diverse da quella provata |

Regole:

- Mock e fixture solo nei test; il boot reale resta requisito di accettazione (sezione 16, punto 5; criterio di uscita F1).
- Ogni file di verifica dichiara se l'esito viene da mock, simulazione o hardware (classificazione della sezione 16, punto 6).
- Nessuna schermata con dati simulati nel percorso di prodotto (criterio di uscita F2).

## 3. Organizzazione dei test

| Directory | Contenuto |
| --- | --- |
| `crates/*/src` (moduli `#[cfg(test)]`) | Logica pura, stati, policy, validazione |
| `crates/*/tests/`, `apps/desktop/tests/` | Integrazione per crate: IPC del servizio, server MCP su stdio, storage, QMP simulato, UI con egui_kittest; test con QEMU reale marcati `#[ignore]` |
| `tests/integration/` | Integrazione tra più crate (supervisor headless con backend reale o simulato, dichiarato); oggi solo `README.md` |
| `tests/security/` | Test negativi della sezione 7.4 (checklist in `docs/SECURITY.md`); oggi solo `README.md` |
| `tests/e2e/` | Scenari completi con VM reale e, da F5, client MCP; oggi solo `README.md` |

Convenzioni di nome usate dalla CI: `acceleratore_assente` per i test del rilevamento capacità senza acceleratore (F1-03, job `acceleratore-assente`; già presenti in `qemu-backend`) e `cgroup_delegato` per i test `#[ignore]` della delega cgroup che il supervisor aggiungerà (F1-15, job `processi-linux`). Il job segnala con un avviso quando il filtro non trova test.

La fixture grafica deterministica è definita in ADR-0019 e realizzata entro F3.

## 4. Framework di test UI

**egui_kittest 0.36.2** è il framework scelto (ADR-0022; versione allineata a egui/eframe 0.36.2 di ADR-0014): pilota l'interfaccia attraverso l'albero AccessKit e, con la feature `snapshot`, confronta istantanee grafiche. L'ADR resta da chiudere con la prova delle istantanee (criterio 4) e dell'esclusione dei ganci dall'artefatto pubblico (criterio 5). Provato in F0, senza GPU e senza finestra nativa:

| Prova | Host | Esito | Evidenza |
| --- | --- | --- | --- |
| Navigazione e cambio tema tramite ruolo ed etichetta AccessKit (3 test) | Windows 11 | verificato | `F0-01` §7 |
| Assenza di campi di testo nelle sezioni; console reale configurata; percorso UI → tabella tasti → QMP → guest con QEMU reale (test `console_reale`, ignorato per default) | Windows 11 | verificato | `F0-04` Risultati §4, §6 |
| `cargo test --workspace` inclusi 4 test `kittest_ui` | Linux in WSL2 (**[build]**) | verificato | `F0-07` §2 |
| Istantanee grafiche (feature `snapshot`) | — | in corso in un altro flusso (`apps/desktop/tests/istantanee.rs`); nessun esito registrato in `docs/verification/` al momento di questo aggiornamento | backlog F1-19 |
| Esecuzione in CI (Windows e Ubuntu, runner senza GPU) | runner GitHub | non eseguito | `F1-08` |

- Usare ruolo più etichetta per individuare i nodi (`get_by_role_and_label`): l'etichetta da sola può corrispondere a più nodi (`F0-01` §7).
- I test UI sono affiancati dai test headless del core e, quando usano un supervisor simulato, sono etichettati come tali: non verificano IPC, rendering nativo o accelerazione.
- Ganci di automazione solo negli artefatti di test; l'artefatto pubblico non include driver di automazione, endpoint di test o impostazioni di debug (sezione 10 F6).
- Test UI strumentati e prove sull'installer reale hanno evidenze distinte.

## 5. Misure di prestazione

Obiettivi preliminari della sezione 13: da misurare, non risultati ottenuti. Uno scostamento si pubblica con causa e intervento pianificato, senza cambiare gli obiettivi. Bloccanti: assenza delle misure, processi orfani, crescita non contabilizzata, violazione dei limiti funzionali o autorizzativi.

Condizioni di riferimento: host fisico ≥ 4 core/8 thread, 16 GiB RAM, SSD; guest Linux 2 vCPU/4 GiB; display 1280×720; ≥ 100 GiB liberi (sezione 13; host di misura proposto in ADR-0018).

Prima riga di ogni misura: valori F0 su spike (non misure di release), host di ADR-0018 condiviso con altri agenti. Metrica di memoria proposta per l'obiettivo: private bytes su Windows, `RssAnon + VmSwap` su Linux; working set e `VmRSS` pubblicati a parte (`F0-09` §5).

| Misura | Obiettivo (sez. 13) | Metodo | Campioni | Host / backend | p50 | p95 | Massimo | Timeout / errori | Esito | Evidenza |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| CPU manager in idle | media < 1% di un core logico | F0: finestre da 10 s (`--static`) e 60 s (console a guest fermo), solo UI | 1 per configurazione | Windows 11; wgpu / glow | — | — | — | — | non eseguito nella forma richiesta (5 minuti, UI + supervisor); indicazioni: wgpu 0,05–0,16%, glow 0,18–0,47% | `F0-01` §5; `F0-04` §7 |
| CPU manager in idle | media < 1% di un core logico | F1: UI + supervisor (senza VM), `--static`, finestra 311 s (seconda serie; prima serie 271 s) | 2 serie per backend | Windows 11 (i9-14900K); glow / wgpu DX12 | — | — | glow 0,141 % (serie 1: 0,006 %); wgpu 0,025 % (serie 1: 0,069 %); supervisor 0 % | 0 | entro l'obiettivo | `F1-05` |
| Memoria manager — glow | ≤ 250 MiB | F0: solo UI con console attiva, 60 s; private bytes (working set a parte) | 1 serie per condizione | Windows 11; glow | — | — | private 97,6 MiB; working set 94,5 MiB | — | solo UI entro l'obiettivo; supervisor non misurato | `F0-04` §7 |
| Memoria manager — wgpu DX12 | ≤ 250 MiB | come sopra | 1 serie per condizione | Windows 11; wgpu DX12 | — | — | private 372,0 MiB; working set 198,3 MiB | — | deviazione D-12 (private bytes oltre l'obiettivo con la sola UI) | `F0-04` §7 |
| Memoria manager — glow (F1) | ≤ 250 MiB | F1: UI + supervisor senza VM, idle 5 min, campioni ogni 5 s; private bytes (working set a parte) | 62 campioni | Windows 11; glow | — | — | private UI 103,0 + supervisor 2,4 MiB (totale max 105,5); working set 73,3 + 11,0 MiB | 0 | entro l'obiettivo | `F1-05` |
| Memoria manager — wgpu DX12 (F1) | ≤ 250 MiB | come sopra | 62 campioni | Windows 11; wgpu DX12 | — | — | private UI 488,0 + supervisor 2,4 MiB (totale max 491,8); working set 185,3 + 11,0 MiB | 0 | deviazione D-12 confermata con il supervisor | `F1-05` |
| Memoria manager — wgpu DX12 con `MemoryHints::MemoryUsage` | ≤ 250 MiB | UI sola, idle 30 s, release | 6 campioni × 2 esecuzioni | Windows 11; wgpu DX12 / WARP | — | — | DX12 189,4 MiB; WARP 188,2 MiB (prima: 488,6 MiB) | 0 | **entro l'obiettivo**: D-12 risolta | `F1-05` (sezione D-12) |
| Memoria manager — wgpu WARP | ≤ 250 MiB | spike senza console, lettura a 3 s | 1 | Windows 11; WARP | — | — | private 376,3 MiB; working set 124,8 MiB | — | deviazione, modalità degradata | `F0-01` §5 |
| Avvio UI (freddo) | p95 ≤ 3 s | Su SSD | 20 | — | — | — | — | — | non eseguito (richiede riavvio o svuotamento della cache con privilegi) | `F1-05` |
| Avvio UI (caldo) | p95 ≤ 3 s | F0: primo frame dall'ingresso in `main`; processo completo con `--smoke 0.5` riportato a parte | 20 (glow, wgpu Vulkan); 10 (wgpu DX12) | Windows 11 | primo frame: glow 294, Vulkan 705, DX12 501 ms | glow 326, Vulkan 919, DX12 538 ms (processo completo: glow 653, Vulkan 1333 ms) | glow 347, Vulkan 1005, DX12 538 ms | 0 | entro l'obiettivo sul campione a caldo | `F0-01` §5 |
| Avvio UI (caldo) | p95 ≤ 3 s | F1: binario release, primo frame dall'ingresso in `main`; processo completo con `--smoke 0.5` | 20 per backend | Windows 11 (i9-14900K) | primo frame: glow 117, wgpu DX12 239 ms (processo: 633, 700 ms) | glow 139, wgpu DX12 248 ms (processo: 642, 732 ms) | glow 140, wgpu DX12 414 ms (processo: 645, 739 ms) | 0 | entro l'obiettivo | `F1-05` |
| Screenshot guest | p95 ≤ 500 ms a 1280×720 | F0: `screendump` → PPM → PNG in memoria, guest locale, 1280×800 | 120 (fermo); 100 (animazione) | Windows 11, QEMU 11.1.0/WHPX | 17,6 / 16,4 ms | 19,2 / 19,3 ms | 20,5 / 24,3 ms | 0 / 0 | entro l'obiettivo | `F0-04` §7 |
| Revoca | p95 ≤ 250 ms | Dall'azione locale al blocco dei nuovi inoltri nel supervisor | ≥ 100 | — | — | — | — | — | non eseguito (F4) | — |
| Emergency Stop — guest a desktop | p95 ≤ 2 s per fine processo QEMU | F0: `TerminateJobObject` fino all'uscita osservata e job vuoto, attesa 125–140 s dall'avvio | 30 | Windows 11, QEMU 11.1.0/WHPX | 394,3 ms | 640,2 ms | 652,0 ms | 0 / 0 | entro l'obiettivo; campioni inferiori a 100 (D-18) | `F0-06` §2c |
| Emergency Stop — guest in boot | p95 ≤ 2 s per fine processo QEMU | come sopra, attesa 5–60 s dall'avvio | 100 | Windows 11, QEMU 11.1.0/WHPX | 142,7 ms | 273,3 ms | 354,5 ms | 0 / 0 | entro l'obiettivo | `F0-06` §2c |
| Processi orfani | zero | F0: kill forzati, morte del supervisor, kill esterni di QEMU, Emergency Stop, arresti con scalata, cicli baseline | 5 + 10 + 6 + 130 + 10 + 5 cicli | Windows 11 | — | — | — | — | zero sui cicli eseguiti; crash della UI non eseguito | `F0-02` §7; `F0-05`; `F0-06` |
| Spazio temporaneo | nessuna crescita non contabilizzata | F0: contabilità per sessione (overlay, vars, log, screendump, socket) | — | Windows 11 | — | — | — | — | prima contabilità; serie di sessioni e pulizia dopo riavvio non eseguite | `F0-08`; `F0-05` §9 |
| Latenza console nel ripiego (se usato) | nessun obiettivo numerico; misurata e dichiarata (sez. 6.4) | F0: solo cattura `screendump` (consegna via IPC non esistente) | 120 | Windows 11 | 17,6 ms | 19,2 ms | 20,5 ms | 0 / 0 | ripiego non necessario sul primo host | `F0-04` §7 |

Regole di misura (sezione 13):

- Per le latenze riportare p50, p95, massimo, timeout ed errori; nessuna esclusione dei fallimenti.
- Le misure di memoria distinguono working set, private bytes o metriche equivalenti per OS; non confrontare metriche diverse sotto la stessa etichetta.
- Backend di rendering diversi per OS richiedono confronti per OS, senza nascondere processi figli.
- Dimensione dell'installer riportata separando componenti inclusi, download necessari e spazio VM.
- Registrare le condizioni dell'host (alimentazione, profilo energetico, processi in background); con CPU ibride indicare la normalizzazione usata (ADR-0018).

## 6. Regole per i runner con guest non fidati

- Nessuna credenziale di produzione, chiave di firma o token di pubblicazione sui runner che eseguono guest e immagini non fidati (sezione 13).
- Le PR non fidate non accedono automaticamente al laboratorio con segreti.
- Media e immagini usati sui runner provengono dal registro con hash verificato; nessun download non verificato durante il test.
- Profilo offline anche sui runner: il guest di test non ha NIC.
- Log e artefatti dei runner ripuliti da segreti prima dell'archiviazione in `docs/verification/`; i log grezzi restano fuori dal repository (`lab/`).
- WSL2, VM annidate o runner senza accelerazione verificabile servono solo per build e test indipendenti dall'acceleratore (sezione 10 F0); le prove di questo tipo sono etichettate **[build]** o **[processi]** come in `F0-07`.
- CI F1 (F1-02): workflow in `.github/workflows/ci.yml`, descritto e validato in locale in `F1-08` (proposta di partenza in `F0-07`, "Proposta: CI minima per F1"). Nessun runner della CI ha un acceleratore verificabile né il QEMU approvato: i job di laboratorio fisico (`lab-fisico-windows`, `lab-fisico-linux`) restano fuori dalla CI pubblica e non sono attivati da PR non fidate.
- Licenze e advisory a ogni modifica con `cargo-deny` (`deny.toml`, ADR-0023, `F1-07`).
- SDK MCP provato con un client di prova su stdio (`F1-06`); i test con client MCP reali sono di F5 e appartengono a `tests/e2e/`.
- Build macOS su runner adeguato entro F2, senza dichiarare supporto operativo: job `macos-check` non bloccante; se il runner manca, rischio registrato in `docs/STATUS.md` (sezione 10 F1).
