# ADR-0015 — Terminazione dei figli su Linux e sorte delle VM manuali senza UI

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F0

## Contesto

ADR-0002 fissa il modello di processo e il Job Object su Windows. Su Linux il meccanismo equivalente di terminazione dei figli alla morte del supervisor è scelto in F0. Va inoltre decisa la sorte delle VM manuali rimaste senza interfaccia dopo un crash della UI, tra arresto controllato e ricollegamento della UI; la decisione e la prova dei due scenari di crash (UI e supervisor) sono registrate in un ADR (roadmap, sezione 6.1).

## Decisione

Da prendere, in due parti.

**A. Meccanismo Linux.** Criteri:

1. Alla morte del supervisor, per qualunque causa incluso `SIGKILL`, l'intero albero QEMU termina; nessun processo orfano osservato.
2. I processi sono identificati con handle stabile (non solo PID riutilizzabile).
3. Il meccanismo funziona senza privilegi elevati.
4. È compatibile con i limiti di risorse scelti in ADR-0017.

**B. VM manuali dopo crash della UI.** Criteri:

1. Le sessioni agente vengono comunque revocate (sezione 7.3); la scelta riguarda solo le VM senza sessione AI.
2. Nessuna riattivazione automatica di sessioni agente.
3. Coerenza con il salvataggio del lavoro dell'utente (sezione 7.3, ultima frase).
4. Se si sceglie il ricollegamento: il supervisor deve sopravvivere alla UI per un tempo limitato e verificabile, e la nuova UI si autentica sull'IPC come la precedente.

### Stato delle prove al riesame di F0

**A. Meccanismo Linux — proposta pronta, prove [processi] in WSL2 (`F0-07` §4).** Nessun meccanismo da solo soddisfa i quattro criteri; la combinazione proposta li soddisfa con figlio di prova `sleep` in WSL2, ambiente ammesso da questo ADR per il solo ciclo di vita dei processi:

1. `PR_SET_PDEATHSIG(SIGKILL)` in `pre_exec` con controllo `getppid()` su ogni QEMU, creato da un thread che vive quanto il supervisor (il segnale è legato al thread creatore): SIGKILL al supervisor → fine del figlio diretto, p95 1,142 ms, 30 campioni, 0 orfani.
2. Identità con pidfd (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`): 30/30 `ESRCH` dopo la raccolta; kernel ≥ 5.4.
3. Un cgroup v2 per VM dentro uno scope utente systemd con `Delegate=yes`, con `memory.max`, `cpu.max`, `pids.max` e `cgroup.kill` (kernel ≥ 5.14): unico meccanismo provato che termina anche i discendenti usciti con `setsid` (p95 0,420 ms, 30 campioni, 0 orfani); è lo stesso strumento dei limiti di ADR-0017.
4. Gruppo di processi come ripiego dove la delega cgroup manca, con il limite dichiarato (non contiene discendenti che cambiano gruppo); in quel caso i limiti CPU/memoria sull'albero mancano e AI Mode resta disabilitato su quell'host (sezione 7.2).

Da decidere ancora: creazione dello scope delegato (`systemd-run --user --scope -p Delegate=yes` all'avvio, oppure `StartTransientUnit` via D-Bus dal supervisor) e scope per VM o sotto-cgroup dello scope del supervisor (provato il secondo). Prova successiva in WSL2 (`docs/verification/F1-08-ci.md` §3, valida solo per i processi): lo scope delegato si crea con `StartTransientUnit` via D-Bus senza `systemd-run`; se lo scope contiene solo la VM, systemd lo rimuove alla sua uscita, quindi supervisor e foglie VM devono stare nello stesso scope; senza sessione utente systemd il bus non è raggiungibile, si ripiega sul gruppo di processi e la modalità agente resta disabilitata. La parte A resta Proposta fino alla scelta esplicita. La conferma con QEMU reale e KVM su host fisico è rinviata a R2a (ADR-0016).

**B. VM manuali dopo crash della UI — aperta.**

- Windows: la morte del supervisor termina sempre QEMU tramite il Job Object (p50 118,9 ms con `abort()`, 110,4 ms con `TerminateProcess`, max 143,9 ms, 10 cicli, `F0-06` §2a): il ricollegamento è possibile solo se il supervisor sopravvive alla UI; se il supervisor muore la VM manuale è persa e il suo overlay non è uno stato pulito. Il **crash della UI non è stato eseguito** su Windows perché la UI non avvia ancora il supervisor come processo separato (pendente P-2 in `F0-09`).
- Linux, solo **[processi]**: il supervisor rileva la morte della UI con un pidfd e arresta la VM (SIGTERM, 2 s, SIGKILL): p95 1,769 ms fino alla fine di VM e supervisor, 30 campioni (`F0-07` §4).
- Proposta da confermare con la prova Windows: arresto controllato come comportamento iniziale (`system_powerdown` con timeout, poi `quit`, poi terminazione con primitive OS), perché verificabile senza IPC; il ricollegamento resta un'alternativa da provare quando esiste l'IPC autenticato.

L'ADR resta Proposta finché la parte B non ha la prova su Windows.

## Alternative considerate

A. Linux (candidati, da verificare):
- gruppo di processi più `PR_SET_PDEATHSIG` sui figli diretti;
- cgroup v2 dedicato (ad esempio scope utente di systemd) con terminazione dell'intero gruppo;
- combinazione dei due, con pidfd per l'identità dei processi.

B. VM manuali:
- arresto controllato alla perdita della UI (gentile con timeout, poi forzato);
- ricollegamento di una nuova istanza della UI al supervisor ancora attivo.

## Conseguenze

- A influenza ADR-0009 (Emergency Stop su Linux) e ADR-0017 (limiti).
- B determina il comportamento descritto all'utente e il test "Lifecycle" della sezione 7.4.

## Prove richieste

- Crash del supervisor su Windows: `docs/verification/F0-06-supervisore-processi.md` §2a (verificato). Crash della UI su Windows: non eseguito (pendente P-2 in `docs/verification/F0-09-riesame-prove.md`).
- Scenari su Linux in ambiente non accelerato, solo ciclo di vita dei processi: `docs/verification/F0-07-linux-kvm.md` §4 (verificato **[processi]**).
- Gli stessi scenari con QEMU reale su host Linux fisico: R2a (ADR-0016).

## Riferimenti

Roadmap, sezioni 6.1, 7.3, 7.4, 10 (F0), 13, 17.
