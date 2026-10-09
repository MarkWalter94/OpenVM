# Strategia di test

Ultimo aggiornamento: 2026-10-09 (riesame di F0; completamento entro F1, sezione 10)

Traduzione operativa della sezione 13 di `docs/ROADMAP.md`. Nessun test del prodotto è stato eseguito; le prove di fattibilità F0 (spike) sono registrate in `docs/verification/F0-*` e riassunte in `F0-09-riesame-prove.md`. Le procedure e gli esiti vanno registrati in `docs/verification/` secondo il template di `docs/verification/README.md`.

## 1. Pipeline di verifica

| Livello | Quando | Contenuto | Dove gira | Evidenza |
| --- | --- | --- | --- | --- |
| Per modifica | A ogni modifica | Unit test significativi, validazione degli schemi JSON generati, lint, build dei target configurati | CI senza segreti (Windows, Linux) | Log CI |
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
| `crates/*/` (test di unità) | Logica pura, stati, policy, validazione |
| `tests/integration/` | Supervisor headless, IPC, backend con QEMU reale o simulato (dichiarato) |
| `tests/security/` | Test negativi della sezione 7.4 (checklist in `docs/SECURITY.md`) |
| `tests/e2e/` | Scenari completi con VM reale e, da F5, client MCP |

La fixture grafica deterministica è definita in ADR-0019 e realizzata entro F3.

## 4. Framework di test UI

**egui_kittest 0.36.2**, che pilota l'interfaccia attraverso l'albero AccessKit (ADR-0022, chiusura entro F1). Provato in F0, senza GPU e senza finestra nativa:

| Prova | Host | Esito | Evidenza |
| --- | --- | --- | --- |
| Navigazione e cambio tema tramite ruolo ed etichetta AccessKit (3 test) | Windows 11 | verificato | `F0-01` §7 |
| Assenza di campi di testo nelle sezioni; console reale configurata; percorso UI → tabella tasti → QMP → guest con QEMU reale (test `console_reale`, ignorato per default) | Windows 11 | verificato | `F0-04` Risultati §4, §6 |
| `cargo test --workspace` inclusi 4 test `kittest_ui` | Linux in WSL2 (**[build]**) | verificato | `F0-07` §2 |
| Istantanee grafiche (feature `snapshot`) | — | non eseguito | backlog F1-19 |

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
| Memoria manager — glow | ≤ 250 MiB | F0: solo UI con console attiva, 60 s; private bytes (working set a parte) | 1 serie per condizione | Windows 11; glow | — | — | private 97,6 MiB; working set 94,5 MiB | — | solo UI entro l'obiettivo; supervisor non misurato | `F0-04` §7 |
| Memoria manager — wgpu DX12 | ≤ 250 MiB | come sopra | 1 serie per condizione | Windows 11; wgpu DX12 | — | — | private 372,0 MiB; working set 198,3 MiB | — | deviazione D-12 (private bytes oltre l'obiettivo con la sola UI) | `F0-04` §7 |
| Memoria manager — wgpu WARP | ≤ 250 MiB | spike senza console, lettura a 3 s | 1 | Windows 11; WARP | — | — | private 376,3 MiB; working set 124,8 MiB | — | deviazione, modalità degradata | `F0-01` §5 |
| Avvio UI (freddo) | p95 ≤ 3 s | Su SSD | 20 | — | — | — | — | — | non eseguito | — |
| Avvio UI (caldo) | p95 ≤ 3 s | F0: primo frame dall'ingresso in `main`; processo completo con `--smoke 0.5` riportato a parte | 20 (glow, wgpu Vulkan); 10 (wgpu DX12) | Windows 11 | primo frame: glow 294, Vulkan 705, DX12 501 ms | glow 326, Vulkan 919, DX12 538 ms (processo completo: glow 653, Vulkan 1333 ms) | glow 347, Vulkan 1005, DX12 538 ms | 0 | entro l'obiettivo sul campione a caldo | `F0-01` §5 |
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
- Proposta di job CI per F1 (lint, build e test Windows/Linux, processi, rendering software, acceleratore assente, licenze, laboratorio fisico): `F0-07`, sezione "Proposta: CI minima per F1"; backlog F1-02 in `docs/RELEASE_PLAN.md`.
- Build macOS su runner adeguato entro F2, senza dichiarare supporto operativo; se il runner manca, rischio registrato in `docs/STATUS.md` (sezione 10 F1).
