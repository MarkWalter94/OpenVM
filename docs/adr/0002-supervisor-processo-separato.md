# ADR-0002 — Supervisor come processo separato dalla UI; QEMU figlio del supervisor

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Processi QEMU, sessioni, policy e file VM richiedono un'unica autorità. Se la UI possedesse direttamente QEMU, un crash dell'interfaccia lascerebbe processi orfani o sessioni attive, e la UI diventerebbe un livello fidato (roadmap, sezione 6.1).

## Decisione

- Il supervisor è un processo distinto dal processo eframe, avviato e sorvegliato dalla UI, raggiungibile solo tramite IPC locale autenticato, lo stesso usato dall'adapter MCP.
- Il supervisor è l'unica autorità su processi, sessioni, policy e file VM; la logica risiede in librerie Rust indipendenti dalla UI.
- I processi QEMU sono figli del supervisor, non della UI.
- Su Windows i processi QEMU appartengono a un Job Object del supervisor con terminazione automatica alla chiusura dell'handle.
- Su Linux si usa un meccanismo equivalente, scelto in F0 (ADR-0015).
- Il supervisor parte con LightVM e termina dopo la chiusura controllata delle VM; nell'MVP non è un servizio installato sempre attivo.
- Istanza singola del supervisor per catalogo; processi riconosciuti tramite handle/identità e `run_id`, mai solo tramite PID.
- La perdita del supervisor interrompe le sessioni; il recupero di UI o applicazione non riattiva automaticamente una sessione agente.
- Un crash della UI revoca le sessioni agente (sezione 7.3). La sorte delle VM manuali rimaste senza UI è decisa in ADR-0015.

## Alternative considerate

- QEMU figlio della UI: escluso (sezione 6.1).
- Supervisor come servizio di sistema sempre attivo: escluso per l'MVP (sezione 6.1).
- Logica nella UI con supervisor sottile: esclusa; la UI non è un livello fidato (sezione 6.1).

## Conseguenze

- Serve un IPC tipizzato e autenticato fin da F1, con named pipe con ACL su Windows o socket locale con permessi restrittivi su Unix (sezione 9).
- Tre scenari di crash da provare: UI, supervisor, QEMU.
- La UI riceve solo i dati necessari alle schermate.

## Prove richieste

- QEMU figlio del supervisor in un Job Object, terminazione dei figli alla morte del supervisor, rilevazione della morte esterna di QEMU, identità handle + tempo di creazione, nessun processo orfano: `docs/verification/F0-06-supervisore-processi.md` §1, §2a, §2b, §3 (verificato su Windows). Crash della UI con revoca delle sessioni: non eseguito (la UI non avvia ancora il supervisor; pendente P-2 in `docs/verification/F0-09-riesame-prove.md`).
- Meccanismi Linux in ambiente non accelerato: `docs/verification/F0-07-linux-kvm.md` §4 (**[processi]**; ADR-0015).

## Dettagli di implementazione provati in F0 (Windows)

Da riportare in `qemu-backend` e `lightvm-service` (`F0-06`, Decisioni proposte):

- QEMU creato con `CreateProcessW`, percorso assoluto, argomenti strutturati con quoting MSVC, nessuna shell.
- `PROC_THREAD_ATTRIBUTE_JOB_LIST`: QEMU nel job dalla creazione, senza la finestra di un'assegnazione successiva; job anonimo con `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, senza `BREAKAWAY_OK` né `SILENT_BREAKAWAY_OK`, con porta di completamento per le notifiche.
- `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` limitata a stdin (`NUL`), stdout e stderr su file.
- `DETACHED_PROCESS` (nessun `conhost.exe`, albero di un solo processo); entrambe le varianti verificate nel job.
- Ambiente di QEMU ridotto a `SystemRoot`, unica variabile risultata necessaria nella configurazione provata.
- Identità: handle conservato per tutta l'esecuzione, PID, tempo di creazione e `run_id`; lo stato "vivo" si legge dall'attesa sull'handle, non dall'esistenza del PID.
- IPC rifiuta processi senza credenziale (sezione 7.4, riga "IPC/console"): da F1.

## Riferimenti

Roadmap, sezioni 6.1, 6.2, 7.3, 7.4, 9, 10 (F0), 13.
