# ADR-0008 — Registro media approvato e supporto dati in sola lettura

- Stato: Accettata (i dettagli operativi del supporto dati sono da chiudere in F0, sezione 17)
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

Un percorso selezionato dall'utente può essere sostituito dopo l'approvazione (symlink, junction, scambio di file). Il guest ha bisogno di un canale per ricevere il software da testare senza cartelle condivise (roadmap, sezioni 4 e 6.3).

## Decisione

- Il registro media approvato conserva una copia gestita in sola lettura di ogni ISO, con identità e hash; QEMU riceve solo media del registro.
- Creazione e apertura protette contro symlink, junction e sostituzioni tra controllo e uso, con primitive OS e directory controllate.
- Oltre al supporto di installazione, una VM può montare **a VM spenta** un secondo supporto ISO dati in sola lettura, proveniente dal registro. È l'unico ingresso di file nel guest nell'MVP: non scrivibile dal guest, non gestibile dall'agente.
- Nessun tool MCP monta, sostituisce o rimuove supporti (sezione 9).
- Nessun montaggio automatico di filesystem guest sull'host; nessun catalogo automatico di immagini; ISO introdotte dall'utente tramite selettore nativo.
- Il client MCP fornisce ID, non percorsi (sezione 7.2, punto 5).

## Dettagli da chiudere in F0

La sezione 17 elenca il "supporto dati approvato" tra le decisioni di F0. Restano da fissare: strumento e formato con cui l'utente produce l'ISO dati, limiti di dimensione, tipo di dispositivo emulato (CD-ROM in sola lettura) e verifica che il guest non possa scriverlo. Questi dettagli vanno registrati in questo ADR (sezione aggiuntiva) o in un ADR successivo collegato, senza cambiare la decisione sopra.

## Alternative considerate

- Cartella condivisa (virtio-fs, 9p, SMB): esclusa (sezione 4).
- Disco dati scrivibile: escluso; il supporto è in sola lettura.
- Gestione media da parte dell'agente: esclusa (sezione 9).

## Conseguenze

- La fixture di test (ADR-0019) viene distribuita tramite il supporto dati entro F3.
- Il registro media fa parte del budget storage (ADR-0017).

## Prove richieste

- Test negativo "File" della sezione 7.4 (ISO sostituita dopo l'approvazione) e "Host" (path traversal, UNC, device path, symlink/junction): da F1.
- Identità e checksum della ISO guest: `docs/verification/F0-03-guest-media.md` (verificato).
- Dettagli del supporto dati approvato: non ancora registrati (pendente P-7 in `docs/verification/F0-09-riesame-prove.md`); la fixture di ADR-0019 ne dipende per il percorso di montaggio.

## Riferimenti

Roadmap, sezioni 4, 6.3, 7.2, 7.4, 9, 17.
