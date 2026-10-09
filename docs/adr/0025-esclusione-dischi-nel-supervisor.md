# ADR-0025 — Esclusione dei dischi garantita dal supervisor, non dal lock di QEMU

- Stato: Accettata (Windows); verifica Linux rinviata a R2a con ADR-0016
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

`qemu-img` non deve modificare immagini in uso (roadmap, sezioni 3 e 8) e la baseline è immutabile (ADR-0005). In `F0-02` §8.3, con QEMU 11.1.0 su Windows, `qemu-io` e `qemu-img` hanno scritto e ridimensionato sia l'overlay sia la base mentre QEMU li aveva aperti: il lock delle immagini non protegge. `F0-05` §7 ha trovato la causa (QEMU apre base e overlay con `FILE_SHARE_WRITE`) e ha provato un'esclusione indipendente da QEMU.

## Decisione

1. **Il lock delle immagini di QEMU non è usato come garanzia.** Nessun controllo del prodotto dipende dal rifiuto di `qemu-img` o di `qemu-io`.
2. **Baseline:** il supervisor apre `base.qcow2` e il vars della baseline con accesso in sola lettura e `FILE_SHARE_READ` (senza `FILE_SHARE_WRITE` né `FILE_SHARE_DELETE`) per ogni esecuzione e ogni operazione storage della VM. Con l'handle aperto QEMU legge la base; scrittura, `resize`, `snapshot`, `commit` verso la base, rinomina e cancellazione da parte di altri processi falliscono (errore 32). Vale aprendo prima o dopo QEMU.
3. **Rilevatore di scrittori:** se l'apertura esclusiva della baseline fallisce, qualcuno la tiene aperta in scrittura: l'avvio o l'operazione sono rifiutati (`vm_busy` o `state_conflict`).
4. **Overlay e vars dell'esecuzione:** il supervisor tiene un handle senza `FILE_SHARE_DELETE` (impedisce rinomina e cancellazione) e li colloca in una directory per esecuzione con ACL ristretta. **Limite dichiarato:** un altro processo dello stesso utente può scrivere l'overlay in uso, perché QEMU deve scriverlo e lo apre con `FILE_SHARE_WRITE`; la share mode non distingue i processi. Il limite è coerente con la sezione 7.1 (separazione assoluta da processi malevoli dello stesso utente fuori perimetro) ed è registrato in `docs/SECURITY.md`. **Resta aperta** la scelta della protezione aggiuntiva dell'overlay in uso tra ACL per esecuzione concessa solo all'identità di QEMU, identità di processo distinta per QEMU (token ristretto o AppContainer) oppure limite dichiarato in via definitiva; la scelta si chiude in F1 con prova (backlog F1-09). Fino ad allora vale il limite dichiarato.
5. **Tra un'esecuzione e l'altra:** attributo di sola lettura o ACL sulla baseline e verifica dell'hash del manifest prima di ogni uso, perché la protezione dell'handle cessa alla chiusura del supervisor.
6. **Operazioni che producono immagini** (creazione e materializzazione di baseline) scrivono su file `.partial` creati con `CREATE_NEW` e promuovono con rinomina atomica solo dopo `qemu-img check` **e** confronto (`qemu-img compare`) o hash: un `convert` interrotto produce un file che supera `check` (`F0-05` §8.2).
7. Tutte le mutazioni dei dischi passano dalla coda per VM e dal journal (ADR-0005; schema proposto in `F0-05` §9).

## Alternative considerate

- Affidarsi al lock di QEMU: escluso, fallito su Windows.
- Share mode che riservi l'overlay al solo QEMU: non realizzabile (`F0-05` §7.3: share READ prima di QEMU impedisce l'avvio; dopo QEMU è rifiutata; READ|WRITE lascia scrivere chiunque).
- Solo attributo di sola lettura sulla base (`F0-02` §8.4): utile contro errori, rimovibile dallo stesso utente e senza effetto sull'overlay; adottato come misura complementare.
- Identità di processo distinta per QEMU (token ristretto, AppContainer) con ACL dell'overlay concessa solo a quell'identità: non provata; candidata in F1 per ridurre il limite del punto 4.

## Conseguenze

- `vm-storage` e il supervisor implementano l'apertura esclusiva della baseline, il rilevatore di scrittori, i nomi `.partial` con rinomina atomica e la verifica dell'hash prima di ogni avvio (backlog F1/F3).
- Il test negativo "Storage" della sezione 7.4 (ripristino concorrente al boot, eliminazione di base referenziata) usa questi meccanismi.
- La protezione vale finché il supervisor è vivo; alla sua morte il Job Object termina anche QEMU (`F0-06`), quindi non resta un QEMU attivo su una base non protetta.
- Non protegge da un amministratore, da accessi a livello di volume o da mappature in memoria (non provati).

## Prove richieste

- Fallimento del lock di QEMU: `docs/verification/F0-02-qemu-whpx-qmp.md` §8.3 (fallito, come atteso dalla decisione).
- Share mode di QEMU, protezione della base, rilevatore di scrittori, limite dell'overlay, vars della baseline: `docs/verification/F0-05-baseline-overlay-firmware.md` §7 (verificato, due esecuzioni con esiti identici).
- File parziale non riconoscibile da `check`: `F0-05` §8.2 (verificato).
- Prova di ACL per esecuzione e identità distinta per QEMU: F1.
- Linux: R2a.

## Riferimenti

Roadmap, sezioni 3, 6.3, 7.1, 7.4, 8, 10 (F0, F3). Riesame: `docs/verification/F0-09-riesame-prove.md`.
