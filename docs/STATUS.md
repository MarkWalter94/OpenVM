# Stato del progetto

Ultimo aggiornamento: 2026-10-10

Documento operativo previsto dalla sezione 17 di `docs/ROADMAP.md`: fase corrente, impedimenti e avanzamento. **Questo file non modifica la Definition of Done (sezione 11), il perimetro né i criteri di uscita.** Uno stato qui riportato non vale come verifica: le prove stanno in `docs/verification/`.

## Fase corrente

**F0 — Fattibilità e decisioni: in riesame** (roadmap, sezioni 10 e 15, passo 6). Il riesame `docs/verification/F0-09-riesame-prove.md` conclude che il criterio di uscita è soddisfatto con deviazioni pubblicate sul primo host Windows, ma F0 **non è ancora chiusa**: restano i pendenti elencati sotto. Le prove F0 riguardano spike di fattibilità. Il lavoro F1 avviato in parallelo è descritto nella sezione "Lavoro F1 avviato in parallelo"; nessuna capacità di prodotto è `verificato su hardware`.

Classificazione usata per ogni capacità (sezione 16, punto 6): `implementato`, `verificato in simulazione/test`, `verificato su hardware`, `non verificato`. Nessuna capacità di prodotto è `verificato su hardware`; alcune sono `verificato in simulazione/test` (file F1); le prove di fattibilità su hardware sono elencate nei file F0.

Regola della sezione 10: F1 parte dopo il criterio di uscita di F0. Il lavoro F1 avviato in parallelo non può essere usato per dichiarare un criterio di uscita F1 prima della chiusura di F0.

## Passi di F0 (sezione 15)

| Passo | Lavoro | Stato | Esito ed evidenze |
| --- | --- | --- | --- |
| 1 | Congelare scenario, minacce, target e dipendenze; verificare toolchain | completato con limiti | `F0-01` (build Windows verificata con ambiente MSVC esplicito; WARP verificato; glow senza accelerazione fallito; RDP non eseguito); `F0-03` (Ubuntu 26.04.1 LTS, checksum e firma verificati); ADR iniziali; matrice in `COMPATIBILITY.md` |
| 2 | QEMU reale su Windows, probe WHPX, QMP e stop | completato con deviazioni | `F0-02`: QEMU 11.1.0 con provenienza verificata (firma Authenticode scaduta), WHPX attivo, QMP su socket Unix, nessuna NIC con `-nodefaults -nic none`, arresto forzato senza orfani; boot fino al desktop live solo con `-cpu qemu64` (`-cpu host` fallito); lock immagini fallito |
| 3 | Console nativa con client RFB, input e screenshot sul canale protetto | completato con pendente | `F0-04`: client RFB interno su socket Unix privato verificato, input QMP e screenshot verificati (p95 19,2 ms); VeNCrypt verificato con arresto fallito; tastiera fisica attraverso winit non eseguita (P-4); secondo utente non eseguito (P-5) |
| 4 | Baseline/overlay/firmware e supervisione processi | completato con pendenti | `F0-05`: 5 cicli di ripristino con base e vars invariati, esclusione della base tramite share mode (ADR-0025), crash storage; `F0-06`: Job Object, morte del supervisor senza orfani, Emergency Stop p95 640,2 ms (30 campioni a desktop) e 273,3 ms (100 campioni durante il boot); crash della UI non eseguito (P-2) |
| 5 | Smoke test Linux/KVM e controlli risorse | completato con esito "laboratorio assente" | `F0-07`: nessun host Linux fisico; build e meccanismi di processo in WSL2 con validità limitata; `F0-08`: limiti CPU e memoria del Job Object verificati, nessuna quota storage rigida senza amministratore |
| 6 | Riesame delle prove | completato (proposta in attesa di conferma) | `F0-09`: decisione di prosecuzione proposta; backlog F1 in `docs/RELEASE_PLAN.md` |

Vincolo della sezione 15: nessuna costruzione estesa della dashboard prima dell'esito di QEMU, console e isolamento sul primo host. L'esito sul primo host è ora registrato in `F0-09`.

## Decisione di prosecuzione di F0 (proposta)

**Windows 11 x86_64 resta primo target e unico target del MVP, con esito "verificato solo Windows"** (sezione 10 F0, paragrafo "Laboratorio Linux assente"; ADR-0016). Nessun controllo è fallito senza un'alternativa ammessa; nessun risultato usa TCG. Dettaglio di deviazioni (D-1…D-18) e pendenti in `docs/verification/F0-09-riesame-prove.md` §2–§3.

La decisione diventa efficace con la conferma dell'utente (U-5) e con la chiusura dei pendenti P-2 e P-4…P-7.

### Pendenti per la chiusura di F0

| ID | Pendente | Chi |
| --- | --- | --- |
| P-1 | Conferma che in F0 non è disponibile un host Linux fisico con KVM | **chiuso il 2026-10-10**: confermato (U-5); server Linux fisico reperito dopo, per R2a; ADR-0016 accettata |
| P-2 | Crash della UI su Windows (UI che avvia il supervisor come processo separato); decisione ADR-0015 parte B | **chiuso il 2026-10-10**: ADR-0015 B accettata (impostazione, predefinito terminare le VM); prova `F1-09` verificata con entrambe le politiche |
| P-4 | Tastiera fisica → winit → egui → QMP, inclusi AltGr, Ctrl+C/V, tastierino | utente con sviluppo: il responsabile lo esegue a fine sviluppo F1 |
| P-5 | Test con secondo utente locale sui socket privati, oppure decisione di registrarlo come limite | **parte Linux verificata il 2026-10-10** in una VM Ubuntu (`F1-11`, validità [processi]): altro utente → `Permission denied`; con permessi allentati → rifiutato per UID. **Parte Windows non eseguita**: serve un secondo account Windows (creato dal responsabile) o registrarla come limite |
| P-6 | Disponibilità di firma del codice e revisione indipendente (ADR-0020) | **chiuso il 2026-10-10**: non disponibili (U-7); pubblicazione F6 bloccata finché manca, ADR-0020 accettata |
| P-7 | Dettagli del supporto dati approvato (ADR-0008) | **chiuso il 2026-10-10**: formato ISO 9660/UDF verificato, limiti 16/8 GiB, CD-ROM IDE in sola lettura, hash riverificato a ogni avvio (ADR-0008); scrittura dal guest verificata impossibile (`F1-11`) |

## Lavoro F1 avviato in parallelo

Avviato prima della chiusura di F0 e quindi **non utilizzabile per il criterio di uscita F1** (sezione 10). Avanzamento per voce di backlog in `docs/RELEASE_PLAN.md`, sezione "Avanzamento F1".

| Flusso | Crate | Evidenza | Esito dichiarato |
| --- | --- | --- | --- |
| Tipi condivisi, modello VM, stati, risorse, sessioni, audit | `lightvm-proto`, `vm-core` | `F1-01` | verificato in simulazione/test |
| Catalogo SQLite, migrazioni, journal, registro media, guardie dei dischi | `vm-storage` | `F1-02` | verificato in simulazione/test (Windows) |
| Supervisor, IPC autenticato, composition root | `lightvm-service` | `F1-04` | verificato in simulazione/test (Windows, WSL2); end-to-end con QEMU reale su WHPX, 2 esecuzioni, VM ferma nel firmware |
| SDK MCP su stdio | `lightvm-mcp` | `F1-06` | verificato in simulazione/test; nessun client MCP reale |
| Licenze e `cargo deny` | workspace | `F1-07` | verificato |
| CI e delega cgroup | `.github/workflows/ci.yml` | `F1-08` | validata in locale; **mai eseguita su GitHub** |
| Probe, processi, QMP, input, screenshot, profilo offline, 20 cicli reali, disco del servizio | `qemu-backend`, `lightvm-service` | `F1-03` | verificato su hardware (Windows, WHPX; VM nel firmware) |
| UI che avvia il supervisor e usa l'IPC; crash della UI con VM reale | `apps/desktop`, `lightvm-service` | `F1-09` | verificato su hardware; politica alla perdita della UI come impostazione (ADR-0015 B) |
| Limiti per VM (CPU, memoria, budget storage), impostazioni della VM, `--apri-vm` | `lightvm-proto`, `vm-core`, `qemu-backend`, `lightvm-service`, `apps/desktop` | `F1-10` | verificato su hardware (Windows); storage completo in F3 |
| UI: appunti, backend di rendering, istantanee, misure | `apps/desktop` | `F1-05` | verificato in simulazione/test (appunti, istantanee) e su hardware (ripiego di rendering, misure); lettura degli appunti in egui-winit non bloccata; memoria con wgpu oltre l'obiettivo (D-12) |

Punti critici emersi dal riesame del 2026-10-10:

- **Criterio di uscita F1 (F1-12) eseguito ma non valido**: le prove reali di `F1-03` (anche 20 cicli attraverso il servizio) passano sul primo host, ma per la sezione 10 non contano finché F0 non è chiusa.
- **Arresto alla perdita della UI** risolto il 2026-10-10: impostazione con predefinito "termina subito le VM" (ADR-0015 B), prove in `F1-09`.
- **La console dello spike apre ancora direttamente il socket QMP**: il percorso di prodotto della console passa dal supervisor in F2 (ADR-0012, ADR-0013).

## Esito del laboratorio Linux

**Proposto: "verificato solo Windows"**, in attesa della conferma dell'utente (ADR-0016). Con questo esito Windows è l'unico target del MVP, la cella Ubuntu di `docs/COMPATIBILITY.md` resta "non verificato" e la qualificazione Linux è rinviata a R2a, che non si chiude senza la prova su host fisico. WSL2 è stato usato solo per build e test indipendenti dall'acceleratore (`F0-07`).

## Documenti operativi di F0

| Documento | Stato |
| --- | --- |
| `docs/ARCHITECTURE.md` | aggiornato con gli esiti F0 (console, canali, modello di processo) |
| `docs/SECURITY.md` | controlli "pianificato", con fattibilità provata dove indicato; rischi residui F0 aggiunti |
| `docs/COMPATIBILITY.md` | riga Windows 11 → Linux "sperimentale" con i dati F0; riga Ubuntu "non verificato"; completato nel commit `aa04ccd` |
| `docs/TESTING.md` | misure F0 registrate; egui_kittest provato; completato nel commit `aa04ccd`; misure F1-18 assenti |
| `docs/RELEASE_PLAN.md` | backlog F0 con esiti; backlog F1 con avanzamento per voce |
| `docs/adr/` | 21 ADR Accettata, di cui 0016 condizionata alla conferma dell'utente e 0021, 0023 accettate in parte; ADR Proposta o aperte in parte il 2026-10-10: 0015 parte A (Linux, R2a), 0017 (limiti per VM accettati; soglie host e storage completo in F3); accettate il 2026-10-10: 0015 parte B, 0016, 0020, 0022, 0026 |

## Impedimenti e punti aperti

| # | Impedimento | Stato | Effetto | Riferimento |
| --- | --- | --- | --- | --- |
| I-1 | QEMU assente all'avvio | risolto | QEMU 11.1.0 installato in modo portabile con provenienza verificata | `F0-02` §2; ADR-0010 |
| I-2 | Stato di WHPX non verificato | risolto con deviazione | WHPX funzionante; feature `HypervisorPlatform` riportata *Disabled* via CIM (D-6); `-cpu host` non avvia OVMF (D-1) | `F0-02` §1, §9 |
| I-3 | Host Linux fisico con KVM non disponibile | chiuso per F0 | esito "verificato solo Windows" (ADR-0016); server fisico per R2a reperito in seguito | `F0-07`; ADR-0016 |
| I-4 | Avvisi di canonicalizzazione del percorso | non riprodotti | nessuna azione finché non ricompaiono | `F0-01` §8 |
| I-5 | Firma del codice e revisione indipendente non disponibili | dichiarato (2026-10-10) | non blocca F1–F5; **blocca la pubblicazione in F6** | ADR-0020 |
| I-6 | Comportamento di eframe senza accelerazione grafica (RDP) | aperto in parte | WARP verificato come modalità degradata; RDP non eseguito, limite dichiarato | `F0-01` §6; ADR-0014 |
| I-7 | Build MSVC richiede l'ambiente di Build Tools 2022 caricato esplicitamente | aperto | sviluppo e CI devono impostare l'ambiente; correzione locale a scelta dell'utente | `F0-01` §4.1 |
| I-8 | Nessun secondo account locale per i test negativi multiutente | aperto | P-5 | `F0-04` §6 |
| I-9 | Host condiviso con altri agenti durante le misure | dichiarato | le latenze F0 non sono misure di release | `F0-06`, `F0-08` |
| I-10 | Host di sviluppo attuale senza toolchain Rust 1.99.0 e senza QEMU 11.1.0; Visual Studio 2022 Community senza librerie MSVC x64 desktop | risolto | Rust 1.99.0 installato (rustup, senza modifica del PATH), componente MSVC x64 aggiunto (UAC), QEMU 11.1.0 e ISO scaricati con hash uguali a F0 | `F1-03` §Ambiente |

Il silenzio su un impedimento non equivale alla sua risoluzione (sezione 14, ultima riga).

## Azioni richieste all'utente

Elenco completo con origine in `docs/verification/F0-09-riesame-prove.md` §8.

| ID | Azione | Necessaria per |
| --- | --- | --- |
| U-1 | Da amministratore: verificare ed eventualmente abilitare la feature `HypervisorPlatform`, poi ripetere il probe e `-cpu host` | facoltativa (D-1, D-6) |
| U-2 | Fornire un host Linux fisico Ubuntu LTS con KVM, se disponibile | alternativa a U-5 |
| U-3 | Creare un secondo account locale non amministratore per i test sui socket privati, o decidere di registrare il limite | P-5 (risposta: "in una VM", vedi P-5) |
| U-4 | Prova con tastiera fisica a sessione sbloccata | P-4 — pronta: `powershell -ExecutionPolicy Bypass -File apps\desktop\scripts\prova-tastiera-u4.ps1` (istruzioni nello script; prova a vuoto del 2026-10-10: console con desktop Ubuntu dopo circa 100 s) |
| U-5 | Confermare l'esito "verificato solo Windows" | P-1 — **fatto il 2026-10-10** |
| U-6 | In WSL: `sudo apt install build-essential pkg-config` (ed eventualmente `libxkbcommon-x11-0`) | build Linux standard (non bloccante) |
| U-7 | Dichiarare responsabili di firma del codice e revisione indipendente, o la loro indisponibilità | P-6 — **fatto il 2026-10-10**: nessuno |
| U-8 | Facoltativa: smoke di `lightvm-desktop` in sessione RDP verso l'host | limite L-1 |
| U-9 | Decidere la correzione dell'ambiente MSVC locale | I-7 — sull'host attuale aggiunto il componente MSVC x64 a VS 2022 (2026-10-10) |
| U-10 | Facoltativa, da amministratore: interrogare le quote NTFS | ADR-0017 |
| U-11 | Facoltativa e non valida per la matrice: probe KVM annidato in WSL | solo integrazione |
