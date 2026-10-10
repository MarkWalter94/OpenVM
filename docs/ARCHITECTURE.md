# Architettura

Ultimo aggiornamento: 2026-10-10 (riesame di F0, avanzamento F1)

Documento richiesto dalla sezione 10 (F0) di `docs/ROADMAP.md`, derivato dalla sezione 6. Descrive componenti, confini e regole del prodotto; il codice esistente è costituito da spike di fattibilità F0 e da crate in costruzione. I punti marcati **[verificato in F0: …]** rimandano a prove di fattibilità sul primo host (Windows 11, `docs/verification/`); quelli marcati **[pendente]** dipendono da prove non ancora eseguite. Le decisioni sono negli ADR indicati.

## 1. Componenti e confini

| Componente | Processo | Responsabilità | Non fa | ADR |
| --- | --- | --- | --- | --- |
| UI desktop (`apps/desktop`, egui/eframe) | Processo UI | Schermate, wizard, console come texture, lettura input con winit, selettore file nativo | Non lancia QEMU, non apre dischi, non esegue comandi host generici, non è un livello fidato, non conosce l'endpoint QMP | 0001, 0014 |
| Client RFB (`rfb-client`) | Libreria nel processo UI | Handshake RFB 3.8, VeNCrypt X.509 con rustls, decodifica ZRLE/Hextile/CopyRect/Raw/DesktopSize, framebuffer condiviso | Non invia input, appunti né power control (solo messaggi client 0, 2, 3); non dipende da QMP né da codice di processo | 0003, 0024 |
| Supervisor (`lightvm-service`) | Processo supervisor, distinto dalla UI | Unica autorità su processi, sessioni, policy e file VM; IPC autenticato; coda per VM; Emergency Stop | Non espone QMP né percorsi host ai client | 0002, 0009 |
| Adapter MCP (`mcp-server`, binario `lightvm-mcp`, da F5) | Processo avviato dal client MCP via stdio | Traduzione MCP ↔ IPC; pairing | Non apre dischi, non esegue QEMU, non decide la policy | 0006 |
| Backend QEMU (`qemu-backend`) | Libreria nel supervisor | Probe, costruzione argomenti, spawn nel Job Object, client QMP unico, lato server della console (directory privata, `-vnc unix:`, credenziale del display), screendump, input | Non accetta opzioni QEMU arbitrarie; non contiene il client RFB | 0007, 0010, 0012, 0013, 0024 |
| Storage (`vm-storage`) | Libreria nel supervisor | SQLite, manifest JSON versionati, registro media, baseline, overlay, journal, esclusione dei dischi tramite share mode | Non modifica immagini in uso; non si affida al lock di QEMU | 0005, 0008, 0025 |
| Core (`vm-core`) | Libreria | Modelli, stati, casi d'uso, policy, sessioni | Nessuna dipendenza da UI o da I/O di processo | — |
| Protocollo (`lightvm-proto`) | Libreria | Tipi IPC/MCP e JSON Schema generati da fonte unica; errori pubblici stabili | — | — |
| QEMU | Processo figlio del supervisor, uno per VM in esecuzione | Esecuzione del guest con acceleratore | — | 0007 |

## 2. Diagramma

```text
 Processo UI (eframe)                      Processo client MCP (esterno)
 ┌─────────────────────────┐                ┌──────────────────────────┐
 │ schermate, wizard        │                │ agente                   │
 │ client RFB → texture     │                └────────────┬─────────────┘
 │ input winit (scancode)   │                             │ stdio (MCP 2025-11-25)
 └──────┬───────────▲───────┘                ┌────────────▼─────────────┐
        │ IPC       │ framebuffer            │ adapter lightvm-mcp      │
        │ tipizzato │ (canale display        └────────────┬─────────────┘
        │ autentic. │  protetto o IPC                     │ IPC autenticato
        │           │  nel ripiego)                       │
 ┌──────▼───────────┴─────────────────────────────────────▼─────────────┐
 │ Processo supervisor                                                   │
 │  autenticazione IPC → policy (intersezione) → sessioni/grant         │
 │  → coda operazioni per VM ─┐      percorso prioritario Emergency Stop │
 │  backend QEMU · storage · audit                                       │
 └────────┬───────────────────┴──────────────────────────────────────────┘
          │ spawn con argomenti strutturati, Job Object / meccanismo Linux
 ┌────────▼────────────────────┐
 │ processo QEMU (per VM)       │◄── QMP su canale privato (solo supervisor)
 │  display RFB protetto        │──► framebuffer verso la UI
 └────────┬────────────────────┘
          │
        guest (offline, nessuna NIC)
```

## 3. Modello di processo

- La UI avvia e sorveglia il supervisor; il supervisor avvia i processi QEMU (ADR-0002).
- Istanza singola del supervisor per catalogo; processi identificati con handle, tempo di creazione e `run_id`, mai solo con il PID. **[verificato in F0: `F0-06` §3, PID riassegnato riconosciuto dal tempo di creazione]**
- Windows: QEMU creato con `CreateProcessW` e argomenti strutturati, nel Job Object del supervisor **dalla creazione** (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), job con `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` senza breakaway, eredità limitata ai tre handle standard (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), `DETACHED_PROCESS`, ambiente ridotto a `SystemRoot`. **[verificato in F0: `F0-06` §1]**
- Crash del supervisor (Windows): il sistema operativo chiude il job e termina QEMU (p50 118,9 / 110,4 ms, max 143,9 ms, 10 cicli, 0 residui). **[verificato in F0: `F0-06` §2a]**
- Morte esterna di QEMU: rilevata dall'attesa sull'handle e classificata dal codice di uscita e dalle notifiche del job. **[verificato in F0: `F0-06` §2b]**
- Emergency Stop: `TerminateJobObject` con codice dedicato, completamento dopo l'uscita osservata e `ActiveProcesses = 0` (p95 640,2 ms su 30 campioni a desktop, 273,3 ms su 100 campioni durante il boot). **[verificato in F0: `F0-06` §2c]**
- Arresto ordinario: `system_powerdown` con timeout configurabile, poi `quit`, poi `TerminateJobObject`. **[verificato in F0 come procedura: `F0-06` §2d]**
- Limiti: hard cap CPU e limite di memoria del job applicati prima dello spawn, impostabili per VM (ADR-0017, accettata in parte). **[verificato in F0: `F0-08`; per VM in F1: `F1-10`]**
- Linux: combinazione proposta `PR_SET_PDEATHSIG` + pidfd + cgroup v2 delegato con `cgroup.kill` (ADR-0015, Proposta). **[verificato in F0 solo per il ciclo di vita dei processi in WSL2: `F0-07` §4; QEMU su host fisico in R2a]**
- Crash della UI: le sessioni agente sono revocate; la sorte delle VM manuali è un'impostazione (predefinito: terminarle subito; ADR-0015 parte B). **[verificato su Windows in F1: `F1-09` (P-2)]**
- Il recupero di UI o applicazione non riattiva automaticamente sessioni agente.
- Il supervisor non è un servizio sempre attivo nell'MVP.

## 4. Macchina a stati della VM

Stato osservato derivato da processo e QMP, non dal solo database (sezione 6.3).

```text
            start                 QMP pronto + processo vivo
 Stopped ─────────► Starting ───────────────────────────► Running
   ▲                   │                                    │
   │                   │ errore / timeout                   │ shutdown, force-stop, Emergency Stop
   │                   ▼                                    ▼
   │                Failed ◄──── uscita inattesa ───── Stopping
   │                   │                                    │
   │                   ▼                                    │ processo terminato e file rilasciati
   │              Recovering ── riconciliazione ──┐         │
   └───────────────────────────────────────────────┴────────┘
```

| Stato | Ingresso | Operazioni ammesse | Note |
| --- | --- | --- | --- |
| Stopped | Processo terminato e file rilasciati | start, configurazione, media, baseline, ripristino | Unico stato per operazioni su dischi, NVRAM e media |
| Starting | Spawn avviato | force-stop | Nuovo `run_id` assegnato |
| Running | Processo vivo e QMP pronto | shutdown, force-stop, reset, screenshot, input, observe | Richieste con `run_id` precedente rifiutate |
| Stopping | Arresto richiesto | force-stop | Stop dichiarato completo solo a fine processo osservata |
| Failed | Errore di avvio o uscita inattesa | recover, diagnostica | Nessuna promozione automatica a baseline |
| Recovering | Riconciliazione dopo crash | nessuna mutazione esterna | Applica journal, verifica file e processi |

## 5. Coda operazioni per VM

- Una coda di mutazioni per VM; operazioni incompatibili bloccate (`vm_busy`, `state_conflict`).
- Ogni operazione ha `operation_id`; ogni esecuzione ha `run_id`; controllo delle revisioni su configurazione e policy.
- Le richieste riferite a un `run_id` precedente sono invalidate dopo riavvio o ripristino.
- Retry automatici solo per operazioni idempotenti; input GUI mai ripetuti dopo un timeout.
- Comandi lunghi restituiscono uno stato tracciabile per `operation_id`.
- Il percorso di Emergency Stop non passa dalla coda (ADR-0009).
- Doppio controllo dell'autorizzazione: al ricevimento e immediatamente prima dell'inoltro al backend (sezione 7.3).

## 6. Policy come intersezione

```text
policy_effettiva = limiti_globali ∩ policy_VM ∩ grant_sessione ∩ capacità_host_disponibili
```

- Nessun livello amplia i precedenti; un'assenza di capacità host restringe, non viene ignorata.
- La stessa policy vale per UI e MCP; la UI non è un livello fidato aggiuntivo.
- La revisione della policy applicata è registrata in ogni report di sessione (sezione 9).
- Errori pubblici: `unauthenticated`, `permission_denied`, `session_expired`, `vm_busy`, `unsupported`, `resource_limit`, `state_conflict` (semantica dei codici di autenticazione in ADR-0026).

## 7. Console

- Client RFB interno nel processo UI, crate `rfb-client` separato (ADR-0024), sottoinsieme ristretto, sola lettura per costruzione; framebuffer disegnato come texture egui aggiornata per rettangoli (ADR-0003). **[verificato in F0: `F0-04` §1–§3, framebuffer dell'app identico a `screendump`]**
- Canale display primario: **VNC su socket Unix** nella directory privata per esecuzione creata dal supervisor, ACL solo-utente; alternativa documentata VeNCrypt X.509 con certificato client (rustls), non predefinita perché l'arresto della sessione TLS non interrompe la lettura (ADR-0013). **[verificato in F0: `F0-04` §1, §5, §6; secondo utente pendente]**
- Credenziale del display emessa dal supervisor per esecuzione e VM, distinta da QMP e IPC, consegnata alla UI e revocata con la sessione. **[pendente: da realizzare e provare entro F2, ADR-0013]**
- Ripiego: framebuffer da QMP `screendump` a frequenza limitata, file temporaneo in directory privata scelto dal backend, consegna alla UI via IPC; latenza dichiarata in `docs/COMPATIBILITY.md`. **[fattibilità verificata in F0: cattura p95 19,2 ms, `F0-04` §7; non necessario sul primo host]**
- Input: eventi winit con scancode fisici → qcode QEMU per layout dichiarati (US, IT) → IPC → bridge autorizzato → QMP. Tasti rilasciati su perdita di focus, disconnessione o cambio proprietario. **[verificato in F0 da `egui::Event` in poi: `F0-04` §4; tastiera fisica attraverso winit pendente]**
- Nessuna lettura degli appunti host: egui-winit è usato in una copia patchata che non li legge (`vendor/egui-winit`, F1-13); il filtro della UI resta come seconda difesa; le etichette non sono selezionabili, quindi nemmeno la copia verso gli appunti.
- Screenshot prodotti dal backend anche con console chiusa, con `run_id` e geometria del display.

### 7.1 Canali di controllo

| Canale | Trasporto | Proprietario | Stato |
| --- | --- | --- | --- |
| QMP | socket Unix `qmp.sock` nella directory privata per esecuzione; un solo client, il supervisor, collegato subito dopo lo spawn (QEMU serve un client alla volta) | supervisor | verificato in F0 (`F0-02` §4.3, `F0-04` §6, `F0-06`); named pipe creata da QEMU scartata (ADR-0012) |
| Display | socket Unix `vnc.sock` nella directory privata per esecuzione | UI (client RFB), con endpoint ricevuto dal supervisor | verificato in F0 (`F0-04`); credenziale per esecuzione pendente (ADR-0013) |
| IPC UI/MCP ↔ supervisor | named pipe con ACL (Windows), socket Unix con permessi restrittivi (Linux) | supervisor | da F1 |

## 8. Struttura dei crate (sezione 6.2)

```text
apps/desktop/              UI egui/eframe
crates/vm-core/            modelli, stati, casi d'uso, policy e sessioni
crates/qemu-backend/       processi, QMP, probe, lato server della console
crates/rfb-client/         client RFB in sola lettura, usato dalla UI (ADR-0024)
crates/vm-storage/         SQLite, manifest, baseline, journal
crates/lightvm-service/    supervisor e IPC locale
crates/lightvm-proto/      tipi IPC/MCP e JSON Schema generati
crates/mcp-server/         aggiunto in F5
tests/{integration,security,e2e}/
docs/{adr,verification}/
```

Direzione delle dipendenze (non fissata dalla roadmap; registrata qui e in ADR-0024):

```text
apps/desktop ──► lightvm-proto, rfb-client, lightvm-service (solo client IPC, senza `supervisore`),
                 f0-console (spike, console diretta da sostituire in F2)
rfb-client ──► (nessuna dipendenza da QMP, processi, storage o supervisor)
lightvm-service ──► vm-core, qemu-backend, vm-storage, lightvm-proto
qemu-backend ──► vm-core
vm-storage ──► vm-core
mcp-server ──► lightvm-proto
vm-core ──► (nessuna dipendenza da UI, processi o I/O)
```

La UI dipende dal protocollo e dal client RFB, mai dal backend né dallo storage; l'adapter MCP dipende solo dal protocollo. La sezione 6.2 colloca la console in `qemu-backend`, mentre la sezione 6.4 esegue il client RFB nel processo UI: ADR-0024 separa il client in `crates/rfb-client` e lascia in `qemu-backend` il lato server della console. Nuovi crate (`vm-network`, `ai-control-bridge`, `vm-security`, `agent-runtime`, `test-runner`) solo quando hanno responsabilità e dipendenze stabili; nessun framework di plugin o secondo backend nel MVP. Gli spike F0 (`spikes/`) non sono codice di prodotto; l'unica eccezione temporanea è `f0-console`, usata dalla console della UI fino al trasporto del display attraverso il supervisor (F2, ADR-0013).

## 9. Checklist di review (sezione 6.3)

Da applicare a ogni modifica che tocca processi, QEMU, file, IPC o MCP.

- [ ] Spawn con argomenti strutturati, mai stringhe per una shell; percorso QEMU approvato e verificato.
- [ ] Nessuna concatenazione di percorsi in opzioni QEMU interpretabili (`-drive` e simili); formato disco esplicito; protocolli in allowlist.
- [ ] Media solo dal registro approvato, con identità/hash verificati all'uso; apertura protetta contro symlink, junction e sostituzioni tra controllo e uso.
- [ ] Nessun montaggio di filesystem guest sull'host.
- [ ] Ambiente, handle e descrittori dei figli ripuliti; nessuna credenziale provider o MCP trasmessa a QEMU.
- [ ] QMP solo interno, su canale privato; nessun endpoint TCP non autenticato; nessun `qmp.execute`.
- [ ] Backend con API ridotta (probe, create, start, shutdown, force-stop, status, observe, input, baseline); capacità non disponibili producono errori espliciti.
- [ ] Stato VM derivato da processo e QMP, non dal solo database.
- [ ] Mutazioni tramite la coda per VM, con `operation_id`, `run_id` e controllo delle revisioni.
- [ ] Policy effettiva calcolata come intersezione; nessun livello che amplia.
- [ ] Retry solo per operazioni idempotenti; nessun input GUI ripetuto dopo timeout.
- [ ] Operazioni DB + filesystem con journal e riconciliazione.
- [ ] Tipi e JSON Schema generati dalla fonte unica; errori pubblici stabili.
- [ ] Dipendenze con versione fissata e lockfile aggiornato; versioni QEMU/firmware testate registrate.
- [ ] Testo dal guest, log e report resi come testo; nessun link aperto automaticamente.
- [ ] Nessun segreto in argomenti di processo, log, report o esempi.

## 10. Esito dei punti da validare in F0

Riepilogo del riesame `docs/verification/F0-09-riesame-prove.md`.

| Punto | ADR | Esito F0 | Evidenza |
| --- | --- | --- | --- |
| Build eframe Windows/Linux e backend di rendering, anche in RDP | 0014 | verificato con limiti (glow predefinito, wgpu selezionabile, WARP degradato; Linux solo WSL2; RDP non eseguito) | `F0-01`, `F0-04`, `F0-07` |
| Build QEMU, acceleratore WHPX, QMP privato, stop | 0010, 0012, 0007 | verificato con deviazioni (`-cpu qemu64`, firma scaduta) | `F0-02` |
| Guest e firmware | 0011 | verificato (boot live in F0; installazione su disco e boot dal disco verificati poi in F1-17, `F1-03`) | `F0-03`, `F0-02` §9 |
| Console RFB sul canale protetto, input, screenshot | 0003, 0013, 0024 | verificato con pendenti (tastiera fisica, secondo utente, credenziale del display) | `F0-04` |
| Baseline/overlay/NVRAM ed esclusione dei dischi | 0005, 0025 | verificato (5 cicli; overlay in uso non protetto da terzi dello stesso utente) | `F0-05` |
| Modello di processo e crash | 0002, 0009, 0015 | verificato con pendente (crash della UI) | `F0-06`, `F0-07` |
| Linux/KVM | 0016 | non eseguito (laboratorio assente); proposto "verificato solo Windows" | `F0-07` |
| Limiti risorse e prime misure | 0017, 0018 | identificati (soglie e prove sotto pressione pendenti) | `F0-08` |
