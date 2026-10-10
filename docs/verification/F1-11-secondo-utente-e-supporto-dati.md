# F1-11 — Secondo utente sull'endpoint IPC (P-5, parte Linux in VM) e supporto dati in sola lettura

- Data di esecuzione (UTC): 2026-10-10, dalle 11:15 alle 11:25 circa
- Esecutore: agente di sviluppo (sessione Claude Code), su indicazione del responsabile del progetto ("U-3 lo fai facendo una VM")
- Passo della roadmap: sezione 7.4 (righe "Sessioni" e "IPC/console": un altro utente locale non raggiunge l'endpoint), 4 e 6.3 (supporto dati in sola lettura); pendenti F0 **P-5** (parte Linux) e **P-7** (prova di scrittura dal guest)
- ADR collegati: ADR-0002, ADR-0008, ADR-0016 (non modificati nella decisione)
- Esito complessivo: **parziale** — parte Linux di P-5 **verificata in una VM Ubuntu su Windows** (validità **[processi]**: VM annidata, ADR-0016); parte Windows (named pipe con ACL e un secondo account Windows) **non eseguita**; scrittura del supporto dati dal guest **verificata impossibile**

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). Log grezzi in `lab/logs/` (fuori dal repository): `p5-linux-vm.txt`, `p5-linux-vm-2.txt`, `f1-03/s1791631100127-p5-*.png`, `f1-03/s1791631282989-p5-*.png`.

## Scopo

1. P-5 (Linux): con il trasporto IPC del servizio (`lightvm-service::transport`, socket Unix), un secondo utente locale non deve raggiungere l'endpoint: né con i permessi impostati dal servizio, né con permessi allentati apposta (controllo `SO_PEERCRED`).
2. ADR-0008: il guest non può scrivere il supporto dati montato come CD-ROM in sola lettura.

Non dimostra: la parte Windows di P-5 (pipe con descrittore di sicurezza, SID diverso), il servizio completo su Linux (catalogo, QEMU, cgroup: R2a), un host Linux fisico.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | Windows 11 Pro 10.0.26200, i9-14900K, QEMU 11.1.0 con WHPX (come `F1-03`) |
| VM | Ubuntu 26.04.1 desktop live (ISO SHA-256 `601e30fb…1bda1f`), `-cpu Skylake-Client-v4`, 2 vCPU, 4096 MiB, ISO dati sul secondo CD-ROM IDE (`ide.1`, `read-only=on`) |
| Strumento | `lightvm-peer-check` (`crates/lightvm-service/src/bin/lightvm-peer-check.rs`), stesso modulo `transport` del servizio, compilato da Windows per `x86_64-unknown-linux-musl` (statico, collegato con `rust-lld`; `rustup target add` in questa sessione) |
| ISO dati | `lab/media/p5-dati.iso` generata con `pycdlib` 1.14.0 (ISO 9660 livello 3 + Rock Ridge): `prova.sh` (`crates/lightvm-service/scripts/p5-prova-guest.sh`) e `lvmpeer`; firma `CD001`; SHA-256 `0c5ee3904d591a22fa989545598ef742e64ec184014555f0dc9ea5c64dcab6f3` (seconda esecuzione) |
| Utenti nel guest | `ubuntu` (uid 1000, sudo senza password della sessione live), `altro` (uid 1002, creato dallo script) |

## Procedura

```text
set CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cargo build --release -p lightvm-service --no-default-features --bin lightvm-peer-check --target x86_64-unknown-linux-musl
python -I make_p5_iso.py lab\media\p5-dati.iso        (script nella scratchpad della sessione)
set LIGHTVM_QEMU_DIR=<workspace>/lab/qemu/qemu-11.1.0
cargo test -p qemu-backend --test real_qemu -- --ignored --nocapture reale_p5
```

Il test `reale_p5_secondo_utente_linux_in_vm` avvia la VM, attende il desktop (130 s), apre un terminale con Ctrl+Alt+T e digita via QMP (`input-send-event`, layout US) `sudo mount -o ro /dev/sr1 /mnt && bash /mnt/prova.sh`; dopo 20 s salva lo screenshot dell'esito, letto per registrare il risultato. Al termine: Emergency Stop e verifica di nessun processo orfano.

## Risultati

| Voce | Esito | Evidenza (screenshot `…-p5-esito.png`) |
| --- | --- | --- |
| Permessi impostati dal servizio | verificato | `drwx------ ubuntu /tmp/lvm-ipc`, `srw------- ubuntu …/svc-<casuale>.sock` |
| Stesso utente | verificato | `CLIENT CONNESSO`; server `ACCETTATO identita=uid=1000` |
| **Altro utente con i permessi del servizio** | verificato | `CLIENT ERRORE Permission denied (os error 13)` |
| **Altro utente con permessi allentati apposta** (`chmod 777` su directory e socket) | verificato | `CLIENT CHIUSO DAL SERVER`; server `RIFIUTATO UID del peer 1002 diverso da 1000` |
| ISO dati montata e usata nel guest (formato ISO 9660 + Rock Ridge, permessi di esecuzione conservati) | verificato | lo script e lo strumento sono stati eseguiti da `/mnt` |
| **Scrittura sul supporto dati dal guest** (ADR-0008) | verificato impossibile | `touch: cannot touch '/mnt/scrittura': Read-only file system`; remount in scrittura fallito; `dd: failed to open '/dev/sr1': Read-only file system`; SHA-256 dell'ISO sull'host identico prima e dopo |
| Nessun processo orfano dopo la prova | verificato | `assert_no_orphan` superato, 2 esecuzioni |
| Parte Windows di P-5 (secondo account Windows sulla named pipe) | non eseguito | nessun secondo account e nessuna immagine Windows per una VM; il controllo del SID è nel codice (`transport/windows.rs`) e il rifiuto a integrità bassa è verificato in `F1-04` |

## Limiti

- VM annidata su Windows: valida per i meccanismi di processo e di permesso del sistema operativo **[processi]**, non per la cella Ubuntu della matrice (ADR-0016, sezione 10).
- Provato il trasporto del servizio, non il servizio completo su Linux (il contenimento dei processi su Linux non è ancora implementato).
- Un campione per caso (due esecuzioni complete).

## Azioni richieste

1. Parte Windows di P-5: un secondo account Windows non amministratore sull'host (creazione a cura del responsabile: un agente non crea account) oppure una VM Windows con licenza; in alternativa registrarla come limite.
2. Ripetere la parte Linux sul server Linux fisico in R2a con il servizio completo.

## Decisioni proposte

Nessuna nuova decisione: i risultati confermano ADR-0002 (IPC locale autenticato solo per l'utente) e i dettagli del supporto dati di ADR-0008.
