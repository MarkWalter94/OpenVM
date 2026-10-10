# F1-09 — UI che avvia il supervisor come processo separato e crash della UI con VM reale (P-2)

- Data di esecuzione (UTC): 2026-10-10, dalle 09:05 alle 09:20 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 6.1 (supervisor distinto dalla UI), 7.3 (perdita del genitore), 10 F1 ("skeleton del supervisor"); backlog `docs/RELEASE_PLAN.md` F1-11; pendente F0 **P-2** (`docs/STATUS.md`)
- ADR collegati: ADR-0002, ADR-0015 (parte B in sospeso), ADR-0024 (non modificati)
- Esito complessivo: **parziale** — UI → supervisor figlio → IPC verificato su Windows con backend non configurato e con QEMU reale; crash della UI con VM reale **verificato senza processi orfani**, ma l'arresto ordinato della VM **non si conclude** entro il timeout di arresto del supervisor (vedi Risultati e Decisioni proposte); Linux non eseguito

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `p2-crash-ui.txt`.

## Scopo

1. La UI (`lightvm-desktop`) avvia `lightvm-service` come processo figlio, legge la riga `ready` da stdout (pipe anonima privata), si autentica via IPC con la credenziale ricevuta e mostra le VM del catalogo del servizio; avvio, arresto ed Emergency Stop passano dal core del servizio. Nessun accesso a QMP o a file VM dalla UI in questo percorso.
2. La UI non linka `qemu-backend`, `vm-core` né `vm-storage` (ADR-0024).
3. P-2: con una VM reale in esecuzione, la terminazione brusca della UI (`TerminateProcess`) porta il supervisor a rilevare la perdita del genitore, applicare la politica predefinita (`shutdown`) e uscire, senza processi QEMU orfani.

Non dimostra: console della VM attraverso il supervisor (la console dello spike F0-04 apre ancora il socket QMP da riga di comando; il trasporto del display per esecuzione è F2, ADR-0013); crash della UI durante un avvio o un arresto in corso; politica `keep-manual-vms`; Linux; secondo utente.

## Ambiente

Stesso host, toolchain e QEMU di `F1-03-qemu-backend-reale.md` (Windows 11 Pro 10.0.26200, i9-14900K, Rust 1.99.0 MSVC, QEMU 11.1.0 con WHPX). Catalogo, directory runtime e file di audit in directory temporanee per prova (`%TEMP%\lightvm-desktop-*`), passati con `--catalog`, `--runtime-dir`, `--audit-log`.

## Modifiche provate

- `crates/lightvm-service`: feature `supervisore` (predefinita) che racchiude composition root, server e adattatori; senza di essa restano `client`, `credential`, `frame`, `transport`. I binari e i test d'integrazione del servizio richiedono la feature.
- `apps/desktop/src/servizio.rs`: avvio del figlio con `--parent-watch stdin`, lettura della riga `ready` con timeout (30 s), due connessioni IPC (richieste ed eventi: la lettura di un frame non è interrompibile in sicurezza), comandi `Aggiorna`, `Crea` (non esposto nei widget), `Avvia`, `Arresta`, `EmergencyStop`; arresto controllato con la riga `shutdown` e terminazione forzata dopo 15 s.
- `apps/desktop/src/lib.rs`: pannello "Supervisor" nella Dashboard (`servizio_ui`, funzione pura provabile senza processi).
- `apps/desktop/src/main.rs`: opzioni `--catalogo`, `--servizio-exe`, `--qemu-dir`, `--runtime-dir`, `--audit-log`; il supervisor parte solo a finestra creata (i tentativi di rendering falliti non lasciano servizi avviati); nello smoke, righe `SMOKE servizio …` e `SMOKE servizio_arresto …`.

## Procedura

```text
cargo build -p lightvm-service --bins
cargo test -p lightvm-desktop --lib --test servizio_ui --test servizio_figlio
cargo tree -p lightvm-desktop -e normal --prefix none   (filtrato su qemu-backend, vm-core, vm-storage, rusqlite)

set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p lightvm-desktop --test servizio_figlio -- --ignored --nocapture crash_ui_con_vm_reale
```

La prova P-2 (`crash_ui_con_vm_reale`) rilancia il proprio binario di test come processo "UI": questo avvia il supervisor con QEMU, crea una VM (1 vCPU, 1024 MiB, disco 8 GiB, nessuna ISO), la avvia, attende lo stato `running` e stampa il PID del servizio. Il test individua il figlio `qemu-system-x86_64.exe` del servizio (`Win32_Process.ParentProcessId`), termina la "UI" con `Child::kill` (`TerminateProcess`) e controlla con `tasklist` ogni 50 ms, per al massimo 120 s, la fine di QEMU e del servizio.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| La UI non linka `qemu-backend`, `vm-core`, `vm-storage`, `rusqlite` | verificato | `cargo tree -p lightvm-desktop -e normal`: tra questi compare solo `lightvm-service` (senza feature `supervisore`) |
| Lato client del servizio compilabile da solo | verificato | `cargo clippy -p lightvm-service --no-default-features --all-targets -- -D warnings`: nessun avviso |
| Riga `ready` valida e non valida | verificato | `servizio::tests::riga_ready_valida`, `riga_ready_non_valida` (JSON errato, evento diverso, pid, endpoint o credenziale mancanti o malformati) |
| Pannello: stato come testo, pulsanti abilitati solo se l'azione è ammessa, comando restituito al clic | verificato (headless) | `tests/servizio_ui.rs`, 4 test `egui_kittest`: `Avvia` solo su VM `stopped`, `Arresta` solo su `running`, tutto disabilitato con servizio terminato o non connesso |
| UI → supervisor figlio → handshake, iscrizione, capacità, elenco VM, Emergency Stop, arresto controllato | verificato | `ui_avvia_il_supervisor_e_lo_arresta`: stato `connesso`, catalogo vuoto, `Emergency Stop: 0 processi …`, uscita con codice 0 senza terminazione forzata |
| Seconda istanza sullo stesso catalogo | verificato | rifiutata con codice di uscita 3 (`UscitoPrimaDelPronto(Some(3))`); stderr del servizio: `un'altra istanza … sta già usando questo catalogo` |
| Eseguibile del servizio assente | verificato | `ErroreAvvio::Spawn` |
| Crea e avvia una VM reale dalla UI attraverso il servizio | verificato | nella prova P-2: VM `running` con un figlio `qemu-system-x86_64.exe` del servizio (`UI pid 25224 → servizio pid 936 → QEMU [38500]`) |
| **P-2:** perdita del genitore rilevata | verificato | stderr del servizio: `processo genitore terminato` |
| **P-2:** nessun processo orfano | verificato | QEMU terminato dopo 30,20 s, servizio dopo 30,08 s dal kill della UI; nessun processo residuo |
| **P-2:** arresto ordinato della VM | **fallito** | stderr: `timeout nell'arresto del supervisor`; audit: `vm_stop` `accepted` (seq 7) e `forwarded` (seq 9), **nessun** `succeeded`. la fine di QEMU 0,12 s dopo quella del servizio è compatibile con la chiusura del Job Object all'uscita del processo (deduzione dai tempi, non osservata direttamente), non con l'arresto ordinato |
| Linux (pipe di vita, pidfd) per questo percorso | non eseguito | — |
| Prova manuale dell'app con finestra (`lightvm-desktop --catalogo …`) | non eseguito | coperta solo dai test headless e dal modulo pilotato dai test |

Causa del fallimento dell'arresto ordinato: alla perdita del genitore il servizio arresta le VM con la politica ordinaria del backend (`ShutdownPolicy::default()`: `system_powerdown` con attesa 60 s, poi `quit` 5 s, poi kill 10 s). La VM nel firmware non risponde all'ACPI. Il conduttore dell'arresto del server attende il supervisor per `shutdown_timeout` = 30 s, poi rilascia il lock e il processo esce. Il budget dell'arresto del servizio (30 s) è inferiore a quello di una sola VM (fino a 75 s).

## Limiti

- Un solo host Windows; un solo campione P-2 (più i due tentativi precedenti descritti sotto).
- Prima esecuzione valida preceduta da due tentativi con difetti **del test**, non del prodotto: (1) la riga `PRONTO` del figlio era preceduta dal prefisso di libtest e non veniva riconosciuta; il test è terminato per timeout lasciando vivi UI di prova, servizio e QEMU, terminati poi a mano (processi spariti dopo il kill della UI di prova; tempi non registrati per un errore nello script di misura); (2) finestra di attesa di 30 s, inferiore al tempo effettivo (≈30,2 s). Il test ora termina la UI di prova anche in caso di timeout.
- Prima dell'aggiunta di `--audit-log`, un test ha creato un file di audit vuoto in `%LOCALAPPDATA%\LightVM`; il file e la directory sono stati rimossi.
- La release (`cargo build --release -p lightvm-desktop`) non produce `lightvm-service`: l'installer dovrà distribuire entrambi (la UI lo cerca accanto al proprio eseguibile o con `--servizio-exe`).

## Azioni richieste

1. Decidere il budget dell'arresto alla perdita della UI (ADR-0015 parte B): vedi Decisioni proposte.
2. Ripetere P-2 dopo la decisione, con almeno 5 campioni e con un guest che risponde all'ACPI (F1-17).
3. Eseguire il test `ui_avvia_il_supervisor_e_lo_arresta` su Linux (CI `ubuntu-latest` lo esegue con `cargo test --workspace`; mai eseguito su GitHub).
4. Portare la console sull'IPC del supervisor con la credenziale del display per esecuzione (F2, ADR-0013).

## Decisioni proposte

- **ADR-0015 parte B / budget di arresto:** alla perdita della UI usare per le VM una politica dedicata con attesa ACPI breve (per esempio pochi secondi, poi `quit` e kill) e rendere `shutdown_timeout` del servizio non inferiore alla somma dei passi della politica per il numero di VM arrestate in parallelo; in alternativa dichiarare che alla perdita della UI le VM sono terminate con Emergency Stop. La scelta spetta al responsabile del progetto: oggi il risultato finale (nessun orfano) dipende, secondo i tempi osservati, dalla chiusura del Job Object all'uscita del servizio.
