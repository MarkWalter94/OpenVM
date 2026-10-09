# ADR-0024 — Crate `rfb-client` separato usato dalla UI; `qemu-backend` resta nel supervisor

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

La sezione 6.2 della roadmap elenca `crates/qemu-backend/` con la responsabilità "processi, QMP, probe, console". La sezione 6.4 esegue il client RFB **nel processo della UI**, in sola lettura, mentre la sezione 6.1 assegna processi, QMP e file VM al solo supervisor e stabilisce che la UI riceve lo stretto necessario e non è un livello fidato. La sezione 6.2 è un'organizzazione **iniziale**: ammette crate separati quando hanno responsabilità e dipendenze stabili, ma la modifica va registrata in un ADR (`F0-04`, Decisioni proposte).

Lo spike di `F0-04` ha prodotto `crates/rfb-client`: libreria `#![forbid(unsafe_code)]` con handshake RFB 3.8, VeNCrypt X.509 via rustls, decodifica ZRLE/Hextile/CopyRect/Raw/DesktopSize, sessione su thread con framebuffer condiviso; usata da `apps/desktop`. Il test `sola_lettura.rs` verifica che il client invii solo i messaggi di tipo 0, 2 e 3.

## Decisione

1. Il client RFB vive nel crate di libreria **`crates/rfb-client`**, usato dalla UI (`apps/desktop`). Non dipende da `qemu-backend`, da `vm-storage`, da `lightvm-service` né da alcun codice QMP o di gestione processi.
2. **`crates/qemu-backend` resta nel supervisor**: avvio di QEMU con `-vnc unix:` nella directory privata per esecuzione, creazione della directory e della sua ACL, emissione della credenziale del display (ADR-0013), client QMP unico (ADR-0012), input e `screendump` del bridge autorizzato, ripiego screendump. La parola "console" della sezione 6.2 si intende per questo lato server della console.
3. La UI riceve dal supervisor, tramite IPC tipizzato, soltanto l'endpoint e la credenziale del display; non riceve mai l'endpoint QMP. L'input dell'utente dalla UI va al supervisor tramite IPC e da lì a QMP (sezione 6.4).
4. La proprietà di sola lettura è verificata nel crate `rfb-client` in isolamento: insieme chiuso dei messaggi client (tipi 0, 2, 3) con `match` esaustivo, test sui byte scritti, nessun simbolo di input, appunti o power control.
5. Le dipendenze del crate (rustls 0.23.45 con ring 0.17.14, socket2 0.6.5, uds_windows 1.2.1, flate2 1.1.10) sono inventariate separatamente (ADR-0023).

Coerenza con la roadmap: la sezione 6.4 è rispettata (client nel processo UI, sola lettura per costruzione, input via QMP); la sezione 6.1 è rispettata meglio che con il client dentro `qemu-backend`, perché la UI non collega codice capace di avviare processi o di parlare QMP; la sezione 6.2 resta valida come direzione delle dipendenze (UI → protocollo; supervisor → backend), con un crate in più.

## Alternative considerate

- Client RFB dentro `qemu-backend` come da sezione 6.2 letterale: la UI dipenderebbe dal crate del supervisor e collegherebbe codice di processo e QMP, contro la sezione 6.1; la sola lettura sarebbe più difficile da verificare in isolamento.
- Modulo dentro `apps/desktop`: possibile, ma impedisce di testare e inventariare il client indipendentemente dalla UI e di riusarlo in strumenti di test.
- Client VNC di terze parti: ammesso solo se manutenuto e inventariato (sezione 6.4); il client interno (circa 2 000 righe con i test) copre il sottoinsieme richiesto ed è già verificato sul guest reale.

## Conseguenze

- `docs/ARCHITECTURE.md` riporta `crates/rfb-client` nella struttura dei crate e nella direzione delle dipendenze (`apps/desktop → rfb-client`, `apps/desktop → lightvm-proto`).
- Ogni modifica a `rfb-client` deve mantenere verde `tests/sola_lettura.rs` (test negativo "Console", sezione 7.4).
- Requisiti registrati per il client: arresto della sessione su Windows tramite risposta provocata dal server, rifiuto degli encoding non annunciati, scarto di ServerCutText, `None` accettato solo su socket Unix privato.
- Il codice dello spike è un punto di partenza: il passaggio a prodotto avviene in F1/F2 con la credenziale del display emessa dal supervisor.

## Prove richieste

- `docs/verification/F0-04-console-rfb-input.md` Codice prodotto, Risultati §1–§3, §6 (verificato).
- Credenziale del display consegnata via IPC e revocata: entro F2 (ADR-0013).

## Riferimenti

Roadmap, sezioni 6.1, 6.2, 6.4, 7.4, 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
