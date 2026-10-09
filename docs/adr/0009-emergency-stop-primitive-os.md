# ADR-0009 — Emergency Stop con primitive OS, indipendente da QMP

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Un QMP bloccato, un guest che non risponde o un client che continua a inviare richieste non devono impedire l'arresto. Nessun comando può dipendere dal consenso dell'LLM (roadmap, sezione 7.3).

## Decisione

- Tre azioni distinte: **Pausa agente** (blocca nuove mutazioni, la VM continua), **Revoca** (invalida sessione, scarta la coda non inoltrata, chiude i canali), **Emergency Stop** (revoca più terminazione dell'albero QEMU con primitive OS).
- L'Emergency Stop non attende QMP né lo shutdown guest; segue un percorso prioritario nel supervisor, indipendente dalla coda operazioni della VM.
- Su Windows la terminazione usa il Job Object del supervisor (ADR-0002); su Linux il meccanismo scelto in ADR-0015.
- Il completamento è dichiarato solo dopo aver osservato la fine dei processi.
- L'Emergency Stop può perdere lo stato dell'overlay ma non deve alterare la baseline.
- È previsto un comando locale di emergenza utilizzabile se la UI è bloccata.
- Il report distingue coda scartata, richieste già inoltrate ed effetti osservati; non si promette l'annullamento retroattivo.

## Alternative considerate

- Stop tramite QMP `quit` o `system_powerdown`: ammessi per l'arresto ordinario, non come percorso di emergenza.
- Kill per PID: escluso come unico identificatore (sezione 6.1).

## Conseguenze

- Il supervisor mantiene handle di processo e `run_id`.
- Obiettivo di misura: p95 ≤ 2 s per la fine del processo QEMU (sezione 13), da misurare, non garantito.

## Prove richieste

- Terminazione dell'albero QEMU senza QMP in F0: `docs/verification/F0-06-supervisore-processi.md` §2c (verificato su Windows: `TerminateJobObject` con codice dedicato, completamento dichiarato dopo l'attesa sull'handle di QEMU e `ActiveProcesses = 0`; con guest a desktop p50 394,3 ms, p95 640,2 ms, max 652,0 ms su 30 campioni; durante il boot p50 142,7 ms, p95 273,3 ms, max 354,5 ms su 100 campioni; 0 errori, 0 timeout, 0 residui). Timeout di attesa proposto: 10 s con errore visibile. Arresto ordinario con scalata `system_powerdown` → `quit` → terminazione: `F0-06` §2d (verificato come procedura; il live non conclude l'arresto ACPI).
- Linux: `cgroup.kill` in ambiente non accelerato, `docs/verification/F0-07-linux-kvm.md` §4 (**[processi]**); con QEMU reale in R2a.
- Test negativo "Revoca" (flood di input durante Emergency Stop) e misure di latenza di stop: F4.

## Riferimenti

Roadmap, sezioni 6.1, 7.3, 7.4, 10 (F0, F4), 13.
